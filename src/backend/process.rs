//! Subprocess plumbing shared by the CLI-based backends (kdotool, X11), and
//! the call deadline every Linux backend (these and Window Calls) uses.
//! Only "is the tool there", "did it exit cleanly" and "did it answer in
//! time" live here; how each tool reports "no match" is interpreted by its
//! own adapter.

use super::BackendError;
use std::future::Future;
use std::process::Output;
use std::time::Duration;
use tokio::process::Command;

/// How long one backend call (a kdotool, wmctrl or xdotool run, or a Window
/// Calls method) may take. openaction handles events one at a time, so a
/// call that never returns would freeze every key until the plugin
/// restarts; past this it fails and the key alerts instead. Calls normally
/// take a few milliseconds.
pub(super) const CALL_TIMEOUT: Duration = Duration::from_secs(3);

/// Awaits `fut`, or fails with `CommandFailed("{what} timed out after N s")`
/// once `limit` passes. Dropping `fut` on timeout is what cancels the call.
pub(super) async fn with_deadline<T>(
    what: &str,
    limit: Duration,
    fut: impl Future<Output = Result<T, BackendError>>,
) -> Result<T, BackendError> {
    tokio::time::timeout(limit, fut).await.unwrap_or_else(|_| {
        Err(BackendError::CommandFailed(format!(
            "{what} timed out after {} s",
            limit.as_secs_f64()
        )))
    })
}

/// Runs `cmd args...` (no shell) and returns its output, bounded by
/// `CALL_TIMEOUT`. A missing binary is `Unavailable`, any other spawn
/// failure or a timeout `CommandFailed`.
pub(super) async fn run(cmd: &str, args: &[&str]) -> Result<Output, BackendError> {
    run_with_timeout(cmd, args, CALL_TIMEOUT).await
}

/// `run` with an explicit bound. The child is killed (and reaped by tokio)
/// when the timeout drops the `output()` future.
async fn run_with_timeout(
    cmd: &str,
    args: &[&str],
    limit: Duration,
) -> Result<Output, BackendError> {
    // Name the call by its subcommand only: later arguments can be long
    // search patterns.
    let what = match args.first() {
        Some(sub) => format!("{cmd} {sub}"),
        None => cmd.to_string(),
    };
    let output = Command::new(cmd).args(args).kill_on_drop(true).output();
    with_deadline(&what, limit, async {
        output.await.map_err(|e| spawn_error(cmd, &e))
    })
    .await
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
    use std::time::{Duration, Instant};

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

    #[tokio::test]
    async fn a_hung_command_times_out_and_is_killed() {
        let started = Instant::now();
        let result = run_with_timeout("sleep", &["30"], Duration::from_millis(100)).await;
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "took {:?}",
            started.elapsed()
        );
        match result {
            Err(BackendError::CommandFailed(msg)) => {
                assert!(msg.contains("sleep"), "message was: {msg}");
                assert!(msg.contains("timed out"), "message was: {msg}");
            }
            other => panic!("expected CommandFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_command_within_its_bound_returns_its_output() {
        let output = run_with_timeout("sh", &["-c", "echo hi"], Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(output.stdout, b"hi\n");
    }

    #[tokio::test]
    async fn a_missing_binary_is_unavailable_even_with_a_bound() {
        let result = run_with_timeout(
            "opendeck-focus-launcher-no-such-tool",
            &[],
            Duration::from_secs(5),
        )
        .await;
        assert!(
            matches!(result, Err(BackendError::Unavailable(_))),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn with_deadline_turns_a_pending_future_into_a_timeout_error() {
        let started = Instant::now();
        let result = with_deadline(
            "Window Calls List",
            Duration::from_millis(100),
            std::future::pending::<Result<(), BackendError>>(),
        )
        .await;
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(
            result,
            Err(BackendError::CommandFailed(
                "Window Calls List timed out after 0.1 s".to_string()
            ))
        );
    }

    #[tokio::test]
    async fn with_deadline_passes_a_prompt_result_through() {
        let result = with_deadline("x", Duration::from_secs(5), async {
            Err::<(), _>(BackendError::Unavailable("gone".into()))
        })
        .await;
        assert_eq!(result, Err(BackendError::Unavailable("gone".into())));
    }
}
