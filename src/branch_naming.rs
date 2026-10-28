use crate::git::run_git_command;
use crate::hooks::{load_config, run_hook, HookError, ReviewHookData};

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

fn validate_branch_name(name: &str, verbose: bool) -> Result<(), HookError> {
    let output = run_git_command(
        "validate review branch name",
        &["check-ref-format", "--branch", name],
        &[1, 128],
        verbose,
    )
    .map_err(|error| {
        HookError::new(format!(
            "Failed to validate review branch name with Git: {}",
            String::from_utf8_lossy(&error.stderr).trim()
        ))
    })?;
    let normalized = std::str::from_utf8(&output.stdout)
        .ok()
        .map(str::trim_end)
        .unwrap_or_default();
    if !output.status.success() || normalized != name {
        return Err(HookError::new(format!(
            "Naming hook returned `{name}`, which is not a valid Git branch name."
        )));
    }
    Ok(())
}

pub fn resolve_new_review_branch_name(
    input: &ReviewHookData,
    verbose: bool,
) -> Result<String, HookError> {
    let Some(hook) = load_config()?.naming_hook else {
        return Ok(
            format!("review-{}-{}", input.branches.target, input.branches.source).replace('/', "_"),
        );
    };
    let stdout = run_hook(&hook, "naming", input, verbose)?;
    let name = parse_hook_stdout(&stdout).map_err(HookError::new)?;
    validate_branch_name(&name, verbose)?;
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_exactly_one_utf8_line() {
        for bytes in [
            b"chosen-name".as_slice(),
            b"chosen-name\n",
            b"chosen-name\r\n",
        ] {
            assert_eq!(parse_hook_stdout(bytes).unwrap(), "chosen-name");
        }
        for bytes in [
            b"".as_slice(),
            b"\n",
            b"name\nextra\n",
            b"name\n\n",
            &[0xff, b'\n'],
        ] {
            assert!(parse_hook_stdout(bytes).is_err());
        }
    }
}
