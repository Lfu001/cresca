use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fmt;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug)]
pub struct HookError(String);

impl HookError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for HookError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for HookError {}

#[derive(Debug, Default, Serialize)]
pub struct HookRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BranchInputs {
    pub target: String,
    pub source: String,
}

/// Invocation-only context; never part of persisted review identity or approval state.
#[derive(Debug, Serialize)]
pub struct ReviewHookData {
    pub request: HookRequest,
    pub branches: BranchInputs,
    pub metadata: Map<String, Value>,
}

impl ReviewHookData {
    pub fn positional(target: &str, source: &str) -> Self {
        Self {
            request: HookRequest::default(),
            branches: BranchInputs {
                target: target.to_owned(),
                source: source.to_owned(),
            },
            metadata: Map::new(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct CrescaConfig {
    review_branch: Option<ReviewBranchConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewBranchConfig {
    pub naming_hook: Option<HookProgram>,
    pub pr_hook: Option<HookProgram>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookProgram {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
}

fn parse_config(bytes: &[u8]) -> Result<ReviewBranchConfig, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| format!("configuration is not valid UTF-8: {error}"))?;
    let config: CrescaConfig = toml::from_str(text)
        .map_err(|error| format!("configuration is not valid TOML: {error}"))?;
    let config = config.review_branch.unwrap_or_default();
    for (role, hook) in [
        ("naming", &config.naming_hook),
        ("PR resolution", &config.pr_hook),
    ] {
        if hook
            .as_ref()
            .is_some_and(|hook| hook.program.trim().is_empty())
        {
            return Err(format!(
                "review branch {role} hook program must not be empty"
            ));
        }
    }
    Ok(config)
}

pub fn load_config_at(path: &Path) -> Result<ReviewBranchConfig, HookError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ReviewBranchConfig::default())
        }
        Err(error) => {
            return Err(HookError::new(format!(
                "Failed to read Cresca configuration `{}`: {error}",
                path.display()
            )))
        }
    };
    parse_config(&bytes).map_err(|error| {
        HookError::new(format!(
            "Invalid Cresca configuration `{}`: {error}",
            path.display()
        ))
    })
}

pub fn load_config() -> Result<ReviewBranchConfig, HookError> {
    let home =
        dirs::home_dir().ok_or_else(|| HookError::new("Cannot locate the user home directory."))?;
    load_config_at(&home.join(".cresca/config.toml"))
}

/// Runs a user-selected program with fixed arguments and a closed JSON stdin.
/// Callers establish the repository-root cwd before invoking either hook.
pub fn run_hook(
    hook: &HookProgram,
    role: &str,
    input: &impl Serialize,
    verbose: bool,
) -> Result<Vec<u8>, HookError> {
    let bytes = serde_json::to_vec(input)
        .map_err(|error| HookError::new(format!("Failed to encode {role} hook input: {error}")))?;
    if verbose {
        println!("[review branch {role} hook: {}]", hook.program);
    }
    let mut child = Command::new(&hook.program)
        .args(&hook.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            HookError::new(format!(
                "Failed to run review branch {role} hook `{}`: {error}",
                hook.program
            ))
        })?;
    let mut stdin = child.stdin.take().expect("hook stdin is piped");
    // Drain output concurrently with input writing: metadata can exceed pipe capacity.
    let writer = std::thread::spawn(move || stdin.write_all(&bytes));
    let output = child.wait_with_output();
    let written = writer.join().expect("hook input writer must not panic");
    let output = output.map_err(|error| {
        HookError::new(format!(
            "Failed to wait for review branch {role} hook `{}`: {error}",
            hook.program
        ))
    })?;
    if !output.status.success() {
        let status = output
            .status
            .code()
            .map_or_else(|| "unavailable".to_owned(), |code| code.to_string());
        return Err(HookError::new(format!(
            "Review branch {role} hook `{}` failed with exit status {status}.\nHook stderr:\n{}",
            hook.program,
            String::from_utf8_lossy(&output.stderr).trim_end()
        )));
    }
    if let Err(error) = written {
        // Hooks that exit without reading input can still return a valid result.
        if error.kind() != std::io::ErrorKind::BrokenPipe {
            return Err(HookError::new(format!(
                "Failed to write to review branch {role} hook `{}`: {error}",
                hook.program
            )));
        }
    }
    if verbose && !output.stderr.is_empty() {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_hooks_and_defaults() {
        assert!(parse_config(b"").unwrap().naming_hook.is_none());
        let config = parse_config(
            br#"[review_branch.naming_hook]
program = "name"
args = ["fixed"]
[review_branch.pr_hook]
program = "resolve"
"#,
        )
        .unwrap();
        assert_eq!(config.naming_hook.unwrap().args, ["fixed"]);
        assert!(config.pr_hook.unwrap().args.is_empty());
    }

    #[test]
    fn rejects_invalid_configuration() {
        for text in [
            "[review_branch",
            "unknown = 1",
            "[review_branch.other]",
            "[review_branch.pr_hook]\nprogram = '  '",
            "[review_branch.naming_hook]\nprogram = ''",
            "[review_branch.pr_hook]\nprogram = 'x'\nargs = 'x'",
            "[review_branch.pr_hook]\nprogram = 'x'\nextra = 1",
        ] {
            assert!(parse_config(text.as_bytes()).is_err(), "{text}");
        }
        assert!(parse_config(&[0xff]).is_err());
    }
}
