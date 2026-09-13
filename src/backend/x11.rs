use super::{BackendError, WindowBackend, WindowId};
use async_trait::async_trait;
use tokio::process::Command;

pub struct X11Backend;

/// Parses `wmctrl -l -x` output. Each line: `<id> <desktop> <wm_class> <host> <title>`,
/// e.g. `0x03e00007  0 firefox.Firefox        myhost Mozilla Firefox`.
/// wm_class here is `<instance>.<class>`; match against either half.
fn parse_wmctrl_list(stdout: &str, class: &str) -> Vec<WindowId> {
    let class_lower = class.to_lowercase();
    stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let id = parts.next()?;
            let _desktop = parts.next()?;
            let wm_class = parts.next()?.to_lowercase();
            let matches = wm_class.split('.').any(|part| part.contains(&class_lower));
            matches.then(|| id.to_string())
        })
        .collect()
}

fn parse_active_window(stdout: &str) -> Option<WindowId> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() || trimmed == "0x0" {
        None
    } else {
        Some(trimmed.to_string())
    }
}

async fn run(cmd: &str, args: &[&str]) -> Result<std::process::Output, BackendError> {
    Command::new(cmd).args(args).output().await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            BackendError::Unavailable(format!("{cmd} is not installed"))
        } else {
            BackendError::CommandFailed(e.to_string())
        }
    })
}

/// Checks a completed command's exit status, turning a non-zero exit into a
/// `CommandFailed` (including stderr when present) rather than letting the
/// caller silently treat empty/partial stdout as "nothing found". `wmctrl`
/// (unlike `kdotool search`) exits 0 on success and non-zero only on a real
/// failure, so this check is reliable for every wmctrl/xdotool call here.
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

#[async_trait]
impl WindowBackend for X11Backend {
    async fn list_windows(&self, class: &str) -> Result<Vec<WindowId>, BackendError> {
        let output = run("wmctrl", &["-l", "-x"]).await?;
        check_status(&output, "wmctrl -l -x")?;
        Ok(parse_wmctrl_list(
            &String::from_utf8_lossy(&output.stdout),
            class,
        ))
    }

    async fn activate(&self, id: &WindowId) -> Result<(), BackendError> {
        let output = run("wmctrl", &["-i", "-a", id]).await?;
        check_status(&output, "wmctrl -i -a")
    }

    async fn minimize(&self, id: &WindowId) -> Result<(), BackendError> {
        let output = run("xdotool", &["windowminimize", id]).await?;
        check_status(&output, "xdotool windowminimize")
    }

    async fn active_window(&self) -> Result<Option<WindowId>, BackendError> {
        let output = run("xdotool", &["getactivewindow"]).await?;
        let raw = String::from_utf8_lossy(&output.stdout);
        // xdotool prints a decimal window id; wmctrl -l -x prints hex - normalize
        // to hex so ids from both tools compare equal.
        Ok(parse_active_window(&raw).map(|dec| {
            dec.parse::<u64>()
                .map(|n| format!("0x{n:08x}"))
                .unwrap_or(dec)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_matching_window_by_instance_or_class() {
        let stdout = "0x03e00007  0 firefox.Firefox        myhost Mozilla Firefox\n0x01c00003  0 kate.kate myhost Kate\n";
        assert_eq!(
            parse_wmctrl_list(stdout, "firefox"),
            vec!["0x03e00007".to_string()]
        );
    }

    #[test]
    fn no_match_returns_empty() {
        let stdout = "0x01c00003  0 kate.kate myhost Kate\n";
        assert!(parse_wmctrl_list(stdout, "firefox").is_empty());
    }

    #[test]
    fn parses_active_window_normalizing_decimal_to_hex() {
        assert_eq!(parse_active_window("65011719\n").unwrap(), "65011719");
    }

    #[test]
    fn no_active_window_when_zero_or_empty() {
        assert_eq!(parse_active_window("0x0\n"), None);
        assert_eq!(parse_active_window(""), None);
        assert_eq!(parse_active_window("\n"), None);
    }

    #[test]
    fn check_status_ok_on_success() {
        let output = std::process::Command::new("true").output().unwrap();
        assert!(check_status(&output, "true").is_ok());
    }

    #[test]
    fn check_status_reports_command_failed_with_stderr_on_non_zero_exit() {
        let output = std::process::Command::new("sh")
            .args(["-c", "echo 'no such window' 1>&2; exit 1"])
            .output()
            .unwrap();
        match check_status(&output, "wmctrl -i -a") {
            Err(BackendError::CommandFailed(msg)) => {
                assert!(msg.contains("no such window"), "message was: {msg}");
            }
            other => panic!("expected CommandFailed, got {other:?}"),
        }
    }
}
