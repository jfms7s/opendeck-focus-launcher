use super::{BackendError, WindowBackend, WindowId};
use async_trait::async_trait;
use tokio::process::Command;

pub struct KdotoolBackend;

/// Parses `kdotool search --class <pattern>` output: one window id per line,
/// e.g. `{a1b2c3d4-...}`. Empty output (no match) is not an error - kdotool
/// exits non-zero when nothing matches, which the caller must distinguish
/// from "kdotool itself is missing" separately (see list_windows below).
fn parse_search_output(stdout: &str) -> Vec<WindowId> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

fn parse_single_id(stdout: &str) -> Option<WindowId> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

async fn run_kdotool(args: &[&str]) -> Result<std::process::Output, BackendError> {
    Command::new("kdotool")
        .args(args)
        .output()
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                BackendError::Unavailable("kdotool is not installed".to_string())
            } else {
                BackendError::CommandFailed(e.to_string())
            }
        })
}

/// Turns a non-success `kdotool` exit into `CommandFailed`, including stderr.
/// Safe to use for any subcommand where "no result" is only ever expressed
/// as empty stdout on exit 0, or where a non-zero exit is unambiguous (e.g.
/// `windowactivate`/`windowminimize`, which don't have a "no match" case the
/// way `search` does).
fn check_status(output: &std::process::Output, cmd: &str) -> Result<(), BackendError> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    Err(BackendError::CommandFailed(if stderr.is_empty() {
        format!("{cmd} exited with {}", output.status)
    } else {
        format!("{cmd} exited with {}: {stderr}", output.status)
    }))
}

/// Interprets a completed `kdotool search` invocation. `kdotool search`
/// exits non-zero BOTH when nothing matches and on a real error (e.g. no
/// KWin D-Bus scripting interface), so exit status alone can't distinguish
/// "no windows" from "broken session" - treating every non-zero exit as an
/// error would break the common, legitimate no-match case. An error message
/// on stderr with nothing on stdout is a much stronger signal of a real
/// failure than a genuine no-match, which normally prints nothing to stderr
/// either - so only escalate when both hold. This is a best-effort
/// heuristic, not a guarantee (see finding #3 in the final review).
fn interpret_search_output(
    stdout: &str,
    status_success: bool,
    stderr: &str,
) -> Result<Vec<WindowId>, BackendError> {
    let windows = parse_search_output(stdout);
    let stderr = stderr.trim();
    if windows.is_empty() && !status_success && !stderr.is_empty() {
        return Err(BackendError::CommandFailed(format!(
            "kdotool search failed: {stderr}"
        )));
    }
    Ok(windows)
}

#[async_trait]
impl WindowBackend for KdotoolBackend {
    async fn list_windows(&self, class: &str) -> Result<Vec<WindowId>, BackendError> {
        let output = run_kdotool(&["search", "--class", class]).await?;
        interpret_search_output(
            &String::from_utf8_lossy(&output.stdout),
            output.status.success(),
            &String::from_utf8_lossy(&output.stderr),
        )
    }

    async fn activate(&self, id: &WindowId) -> Result<(), BackendError> {
        let output = run_kdotool(&["windowactivate", id]).await?;
        check_status(&output, "kdotool windowactivate")
    }

    async fn minimize(&self, id: &WindowId) -> Result<(), BackendError> {
        let output = run_kdotool(&["windowminimize", id]).await?;
        check_status(&output, "kdotool windowminimize")
    }

    async fn close(&self, id: &WindowId) -> Result<(), BackendError> {
        let output = run_kdotool(&["windowclose", id]).await?;
        check_status(&output, "kdotool windowclose")
    }

    async fn active_window(&self) -> Result<Option<WindowId>, BackendError> {
        let output = run_kdotool(&["getactivewindow"]).await?;
        Ok(parse_single_id(&String::from_utf8_lossy(&output.stdout)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiple_window_ids() {
        let stdout =
            "{aaaaaaaa-0000-0000-0000-000000000001}\n{bbbbbbbb-0000-0000-0000-000000000002}\n";
        assert_eq!(
            parse_search_output(stdout),
            vec![
                "{aaaaaaaa-0000-0000-0000-000000000001}".to_string(),
                "{bbbbbbbb-0000-0000-0000-000000000002}".to_string(),
            ]
        );
    }

    #[test]
    fn parses_empty_output_as_no_windows() {
        assert_eq!(parse_search_output(""), Vec::<WindowId>::new());
        assert_eq!(parse_search_output("\n\n"), Vec::<WindowId>::new());
    }

    #[test]
    fn parses_single_active_window_id() {
        assert_eq!(
            parse_single_id("{aaaaaaaa-0000-0000-0000-000000000001}\n"),
            Some("{aaaaaaaa-0000-0000-0000-000000000001}".to_string())
        );
    }

    #[test]
    fn parses_no_active_window() {
        assert_eq!(parse_single_id(""), None);
        assert_eq!(parse_single_id("\n"), None);
    }

    #[test]
    fn search_success_with_no_matches_is_not_an_error() {
        let result = interpret_search_output("", true, "");
        assert_eq!(result.unwrap(), Vec::<WindowId>::new());
    }

    #[test]
    fn search_non_zero_exit_with_empty_stdout_and_stderr_is_treated_as_no_match() {
        // kdotool's documented no-match case: non-zero exit, nothing on
        // either stream. Must NOT be treated as an error, or a genuinely
        // empty desktop would look like a broken backend.
        let result = interpret_search_output("", false, "");
        assert_eq!(result.unwrap(), Vec::<WindowId>::new());
    }

    #[test]
    fn search_non_zero_exit_with_stderr_and_empty_stdout_is_a_command_failure() {
        let result = interpret_search_output("", false, "kdotool: no KWin scripting interface");
        assert!(matches!(result, Err(BackendError::CommandFailed(_))));
    }

    #[test]
    fn search_non_zero_exit_with_matches_on_stdout_still_returns_them() {
        // Defensive: if kdotool ever prints a partial match list alongside a
        // non-zero exit, prefer the data it did produce over discarding it.
        let result = interpret_search_output("{aaaa}\n", false, "some warning");
        assert_eq!(result.unwrap(), vec!["{aaaa}".to_string()]);
    }

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
            }
            other => panic!("expected CommandFailed, got {other:?}"),
        }
    }
}
