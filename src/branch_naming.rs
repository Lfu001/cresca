use crate::git::{run_git_command, ReviewMetadata};
use serde::Deserialize;
use std::fmt;
use std::path::Path;
use std::process::Command;

#[derive(Debug)]
pub struct BranchNamingError {
    message: String,
}

impl BranchNamingError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for BranchNamingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for BranchNamingError {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CrescaConfig {
    review_branch: Option<ReviewBranchConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewBranchConfig {
    naming_hook: Option<NamingHook>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NamingHook {
    program: String,
    #[serde(default)]
    args: Vec<String>,
}

fn parse_config(bytes: &[u8]) -> Result<CrescaConfig, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| format!("configuration is not valid UTF-8: {error}"))?;
    let config: CrescaConfig = toml::from_str(text)
        .map_err(|error| format!("configuration is not valid TOML: {error}"))?;
    if config
        .review_branch
        .as_ref()
        .and_then(|review_branch| review_branch.naming_hook.as_ref())
        .is_some_and(|hook| hook.program.trim().is_empty())
    {
        return Err("review branch naming hook program must not be empty".to_string());
    }
    Ok(config)
}

fn parse_hook_stdout(bytes: &[u8]) -> Result<String, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| format!("naming hook output is not valid UTF-8: {error}"))?;
    let name = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text);
    if name.is_empty() {
        return Err("naming hook returned an empty branch name".to_string());
    }
    if name.contains(['\r', '\n']) {
        return Err("naming hook must return exactly one line".to_string());
    }
    Ok(name.to_string())
}

fn load_naming_hook(path: &Path) -> Result<Option<NamingHook>, BranchNamingError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(BranchNamingError::new(format!(
                "Failed to read Cresca configuration `{}`: {error}",
                path.display()
            )))
        }
    };
    let config = parse_config(&bytes).map_err(|error| {
        BranchNamingError::new(format!(
            "Invalid Cresca configuration `{}`: {error}",
            path.display()
        ))
    })?;
    Ok(config
        .review_branch
        .and_then(|review_branch| review_branch.naming_hook))
}

fn validate_branch_name(name: &str, verbose: bool) -> Result<(), BranchNamingError> {
    let output = run_git_command(
        "validate review branch name",
        &["check-ref-format", "--branch", name],
        &[1],
        verbose,
    )
    .map_err(|error| {
        BranchNamingError::new(format!(
            "Failed to validate review branch name with Git: {}",
            String::from_utf8_lossy(&error.stderr).trim()
        ))
    })?;
    let normalized = std::str::from_utf8(&output.stdout)
        .ok()
        .map(str::trim_end)
        .unwrap_or_default();
    if !output.status.success() || normalized != name {
        return Err(BranchNamingError::new(format!(
            "Naming hook returned `{name}`, which is not a valid Git branch name."
        )));
    }
    Ok(())
}

fn resolve_new_review_branch_name_at(
    metadata: &ReviewMetadata,
    verbose: bool,
    config_path: &Path,
) -> Result<String, BranchNamingError> {
    let Some(hook) = load_naming_hook(config_path)? else {
        return Ok(format!("review-{}-{}", metadata.target, metadata.source).replace('/', "_"));
    };
    if verbose {
        println!("[review branch naming hook: {}]", hook.program);
    }
    let output = Command::new(&hook.program)
        .args(&hook.args)
        .arg(&metadata.source)
        .arg(&metadata.target)
        .output()
        .map_err(|error| {
            BranchNamingError::new(format!(
                "Failed to run review branch naming hook `{}`: {error}",
                hook.program
            ))
        })?;
    if !output.status.success() {
        let status = output
            .status
            .code()
            .map_or_else(|| "unavailable".to_string(), |code| code.to_string());
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BranchNamingError::new(format!(
            "Review branch naming hook `{}` failed with exit status {status}.\nHook stderr:\n{}",
            hook.program,
            stderr.trim_end()
        )));
    }
    if verbose && !output.stderr.is_empty() {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
    }
    let name = parse_hook_stdout(&output.stdout).map_err(BranchNamingError::new)?;
    validate_branch_name(&name, verbose)?;
    Ok(name)
}

pub fn resolve_new_review_branch_name(
    metadata: &ReviewMetadata,
    verbose: bool,
) -> Result<String, BranchNamingError> {
    let home = dirs::home_dir()
        .ok_or_else(|| BranchNamingError::new("Cannot locate the user home directory."))?;
    resolve_new_review_branch_name_at(metadata, verbose, &home.join(".cresca/config.toml"))
}

#[cfg(test)]
mod tests {
    use super::{parse_config, parse_hook_stdout, resolve_new_review_branch_name_at};
    use crate::git::ReviewMetadata;
    use tempfile::TempDir;

    #[test]
    fn parses_missing_naming_hook() {
        let config = parse_config(b"").expect("empty configuration should be valid");

        assert!(config.review_branch.is_none());
    }

    #[test]
    fn parses_program_and_fixed_arguments() {
        let config = parse_config(
            br#"[review_branch.naming_hook]
program = "pwsh"
args = ["-NoProfile", "-File", "name.ps1"]
"#,
        )
        .expect("valid naming hook should parse");

        let hook = config
            .review_branch
            .expect("review branch configuration should exist")
            .naming_hook
            .expect("naming hook should exist");
        assert_eq!(hook.program, "pwsh");
        assert_eq!(hook.args, ["-NoProfile", "-File", "name.ps1"]);
    }

    #[test]
    fn rejects_malformed_toml() {
        assert!(parse_config(b"[review_branch").is_err());
    }

    #[test]
    fn rejects_empty_hook_program() {
        assert!(parse_config(
            br#"[review_branch.naming_hook]
program = "  "
"#,
        )
        .is_err());
    }

    #[test]
    fn parses_single_line_hook_output() {
        assert_eq!(parse_hook_stdout(b"chosen-name\n").unwrap(), "chosen-name");
    }

    #[test]
    fn trims_crlf_from_hook_output() {
        assert_eq!(
            parse_hook_stdout(b"chosen-name\r\n").unwrap(),
            "chosen-name"
        );
    }

    #[test]
    fn rejects_empty_hook_output() {
        assert!(parse_hook_stdout(b"").is_err());
        assert!(parse_hook_stdout(b"\n").is_err());
    }

    #[test]
    fn rejects_multiline_hook_output() {
        assert!(parse_hook_stdout(b"chosen-name\nextra\n").is_err());
    }

    #[test]
    fn rejects_non_utf8_hook_output() {
        assert!(parse_hook_stdout(&[0xff, b'\n']).is_err());
    }

    #[test]
    fn missing_configuration_uses_default_name() {
        let home = TempDir::new().unwrap();
        let metadata = ReviewMetadata {
            target: "main".to_string(),
            source: "feature/login".to_string(),
        };

        let name =
            resolve_new_review_branch_name_at(&metadata, false, &home.path().join("missing.toml"))
                .unwrap();

        assert_eq!(name, "review-main-feature_login");
    }

    #[cfg(unix)]
    #[test]
    fn hook_receives_fixed_arguments_then_source_and_target() {
        let home = TempDir::new().unwrap();
        let config_path = home.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"[review_branch.naming_hook]
program = "sh"
args = ["-c", "printf '%s-%s-%s\\n' \"$1\" \"$2\" \"$3\"", "hook", "fixed"]
"#,
        )
        .unwrap();
        let metadata = ReviewMetadata {
            target: "main".to_string(),
            source: "feature/login".to_string(),
        };

        let name = resolve_new_review_branch_name_at(&metadata, false, &config_path).unwrap();

        assert_eq!(name, "fixed-feature/login-main");
    }

    #[cfg(unix)]
    #[test]
    fn hook_failure_includes_stderr() {
        let home = TempDir::new().unwrap();
        let config_path = home.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"[review_branch.naming_hook]
program = "sh"
args = ["-c", "printf 'naming failed\\n' >&2; exit 23"]
"#,
        )
        .unwrap();
        let metadata = ReviewMetadata {
            target: "main".to_string(),
            source: "develop".to_string(),
        };

        let error = resolve_new_review_branch_name_at(&metadata, false, &config_path)
            .expect_err("non-zero hook must fail");

        assert!(error.to_string().contains("naming failed"));
        assert!(error.to_string().contains("23"));
    }

    #[cfg(unix)]
    #[test]
    fn hook_result_must_be_a_literal_valid_branch_name() {
        let home = TempDir::new().unwrap();
        let config_path = home.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"[review_branch.naming_hook]
program = "sh"
args = ["-c", "printf '@{-1}\\n'"]
"#,
        )
        .unwrap();
        let metadata = ReviewMetadata {
            target: "main".to_string(),
            source: "develop".to_string(),
        };

        let error = resolve_new_review_branch_name_at(&metadata, false, &config_path)
            .expect_err("checkout shorthand must not be accepted as a branch name");

        assert!(error.to_string().contains("valid Git branch name"));
    }
}
