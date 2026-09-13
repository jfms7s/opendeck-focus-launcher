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

#[async_trait]
impl WindowBackend for KdotoolBackend {
    async fn list_windows(&self, class: &str) -> Result<Vec<WindowId>, BackendError> {
        let output = run_kdotool(&["search", "--class", class]).await?;
        // kdotool exits non-zero both when nothing matches and on a real error;
        // treat non-UTF8-free stdout as authoritative either way (an empty
        // stdout means "no windows", which is a normal, non-error outcome).
        Ok(parse_search_output(&String::from_utf8_lossy(&output.stdout)))
    }

    async fn activate(&self, id: &WindowId) -> Result<(), BackendError> {
        run_kdotool(&["windowactivate", id]).await?;
        Ok(())
    }

    async fn minimize(&self, id: &WindowId) -> Result<(), BackendError> {
        run_kdotool(&["windowminimize", id]).await?;
        Ok(())
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
        let stdout = "{aaaaaaaa-0000-0000-0000-000000000001}\n{bbbbbbbb-0000-0000-0000-000000000002}\n";
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
}
