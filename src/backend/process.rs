//! Subprocess plumbing shared by the CLI-based backends (kdotool, X11).
//! Only "is the tool there" and "did it exit cleanly" live here; how each
//! tool reports "no match" is interpreted by its own adapter.

use super::BackendError;
use std::process::Output;
use tokio::process::Command;

/// Runs `cmd args...` (no shell) and returns its output. A missing binary is
/// `Unavailable`, any other spawn failure `CommandFailed`.
pub(super) async fn run(cmd: &str, args: &[&str]) -> Result<Output, BackendError> {
    Command::new(cmd)
        .args(args)
        .output()
        .await
        .map_err(|e| spawn_error(cmd, &e))
}

fn spawn_error(cmd: &str, e: &std::io::Error) -> BackendError {
    if e.kind() == std::io::ErrorKind::NotFound {
        BackendError::Unavailable(format!("{cmd} is not installed"))
    } else {
        BackendError::CommandFailed(format!("could not run {cmd}: {e}"))
    }
}

/// Turns a non-zero exit into `CommandFailed`, including stderr when there
/// is any.
pub(super) fn check_status(output: &Output, what: &str) -> Result<(), BackendError> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    Err(BackendError::CommandFailed(if stderr.is_empty() {
        format!("{what} exited with {}", output.status)
    } else {
        format!("{what} exited with {}: {stderr}", output.status)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_status_ok_on_success() {
        let output = std::process::Command::new("true").output().unwrap();
        assert!(check_status(&output, "true").is_ok());
    }

    #[test]
    fn check_status_reports_command_failed_with_stderr_on_non_zero_exit() {
        let output = std::process::Command::new("sh")
            .args(["-c", "echo 'window not found' 1>&2; exit 1"])
            .output()
            .unwrap();
        match check_status(&output, "kdotool windowactivate") {
            Err(BackendError::CommandFailed(msg)) => {
                assert!(msg.contains("window not found"), "message was: {msg}");
                assert!(msg.contains("kdotool windowactivate"), "message was: {msg}");
            }
            other => panic!("expected CommandFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_missing_binary_is_unavailable() {
        let result = run("opendeck-focus-launcher-no-such-tool", &[]).await;
        assert!(
            matches!(result, Err(BackendError::Unavailable(_))),
            "{result:?}"
        );
    }
}
