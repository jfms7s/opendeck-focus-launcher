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

#[async_trait]
impl WindowBackend for X11Backend {
    async fn list_windows(&self, class: &str) -> Result<Vec<WindowId>, BackendError> {
        let output = run("wmctrl", &["-l", "-x"]).await?;
        Ok(parse_wmctrl_list(
            &String::from_utf8_lossy(&output.stdout),
            class,
        ))
    }

    async fn activate(&self, id: &WindowId) -> Result<(), BackendError> {
        run("wmctrl", &["-i", "-a", id]).await?;
        Ok(())
    }

    async fn minimize(&self, id: &WindowId) -> Result<(), BackendError> {
        run("xdotool", &["windowminimize", id]).await?;
        Ok(())
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
}
