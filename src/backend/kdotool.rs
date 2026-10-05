//! KDE Plasma backend: shells out to `kdotool`, which renders a `KWin` script
//! from a template and runs it inside the compositor.

use super::process::{check_status, run};
use super::{BackendError, WindowBackend, WindowClass, WindowId};
use async_trait::async_trait;

pub struct KdotoolBackend;

/// Arguments for `kdotool search` implementing the matching contract.
///
/// kdotool pastes the pattern unescaped into the generated `KWin` JavaScript,
/// inside both a double-quoted debug string and a ``String.raw`...` ``
/// template, then uses it as an unanchored, case-insensitive regex. So the
/// pattern must be (1) built only from a validated `WindowClass`, whose
/// allowlist excludes quotes, backticks, `$`, braces and backslashes, and
/// (2) anchored and escaped, so it means "exactly this class". `--class`
/// plus `--classname` matches `KWin`'s `resourceClass` or `resourceName`,
/// i.e. either `WM_CLASS` half / the Wayland `app_id`.
fn search_args(class: &WindowClass) -> Vec<String> {
    vec![
        "search".to_string(),
        "--class".to_string(),
        "--classname".to_string(),
        class.anchored_regex(),
    ]
}

/// Parses `kdotool search` output: one `{uuid}` window id per line, in
/// `KWin`'s `workspace.windowList()` order (creation order, stable across
/// focus changes).
fn parse_search_output(stdout: &str) -> Vec<WindowId> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(WindowId::new)
        .collect()
}

fn parse_single_id(stdout: &str) -> Option<WindowId> {
    let trimmed = stdout.trim();
    (!trimmed.is_empty()).then(|| WindowId::new(trimmed))
}

/// Interprets a completed `kdotool search`. kdotool v0.2.3 (captured on the
/// development machine) exits 0 with empty output when nothing matches, and
/// that is the normal no-match case. A non-zero exit that printed an error
/// and no ids is a real failure (e.g. no `KWin` scripting interface). A
/// non-zero exit with nothing on either stream is still read as "no match",
/// to stay tolerant of kdotool versions that signal no-match by exit code.
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
    async fn list_windows(&self, class: &WindowClass) -> Result<Vec<WindowId>, BackendError> {
        let args = search_args(class);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = run("kdotool", &args).await?;
        interpret_search_output(
            &String::from_utf8_lossy(&output.stdout),
            output.status.success(),
            &String::from_utf8_lossy(&output.stderr),
        )
    }

    async fn activate(&self, id: &WindowId) -> Result<(), BackendError> {
        let output = run("kdotool", &["windowactivate", id.as_str()]).await?;
        check_status(&output, "kdotool windowactivate")
    }

    async fn minimize(&self, id: &WindowId) -> Result<(), BackendError> {
        let output = run("kdotool", &["windowminimize", id.as_str()]).await?;
        check_status(&output, "kdotool windowminimize")
    }

    async fn close(&self, id: &WindowId) -> Result<(), BackendError> {
        let output = run("kdotool", &["windowclose", id.as_str()]).await?;
        check_status(&output, "kdotool windowclose")
    }

    async fn active_window(&self) -> Result<Option<WindowId>, BackendError> {
        let output = run("kdotool", &["getactivewindow"]).await?;
        check_status(&output, "kdotool getactivewindow")?;
        Ok(parse_single_id(&String::from_utf8_lossy(&output.stdout)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured from kdotool v0.2.3 on KDE Plasma 6 (Wayland):
    // `kdotool search --class --classname '^plasmashell$'`, exit 0.
    const SEARCH_PLASMASHELL: &str =
        include_str!("../../tests/fixtures/kdotool-search-plasmashell.stdout");
    // `kdotool search --class --classname '^opendeck-focus-launcher-no-such-app$'`:
    // empty output, exit 0.
    const SEARCH_NO_MATCH: &str =
        include_str!("../../tests/fixtures/kdotool-search-no-match.stdout");
    // `kdotool getactivewindow`, exit 0.
    const GETACTIVEWINDOW: &str =
        include_str!("../../tests/fixtures/kdotool-getactivewindow.stdout");

    fn class(s: &str) -> WindowClass {
        WindowClass::parse(s).unwrap()
    }

    #[test]
    fn search_args_anchor_and_escape_the_class() {
        assert_eq!(
            search_args(&class("org.kde.kate")),
            vec!["search", "--class", "--classname", r"^org\.kde\.kate$"]
        );
    }

    #[test]
    fn search_args_never_carry_template_or_regex_syntax_through() {
        // Whatever reaches kdotool went through WindowClass::parse, which
        // rejects anything that could break out of kdotool's JS template.
        assert!(WindowClass::parse("x`${callDBus()}`").is_err());
        assert!(WindowClass::parse("a\"); evil(); (\"").is_err());
        assert_eq!(search_args(&class("crx_abc"))[3], "^crx_abc$");
    }

    #[test]
    fn parses_a_real_multi_window_search() {
        let ids = parse_search_output(SEARCH_PLASMASHELL);
        assert_eq!(ids.len(), 4);
        assert_eq!(
            ids[0],
            WindowId::new("{66f646df-02d5-4289-bc00-aba6b4d45d2a}")
        );
    }

    #[test]
    fn real_no_match_output_is_an_empty_list_not_an_error() {
        let result = interpret_search_output(SEARCH_NO_MATCH, true, "");
        assert_eq!(result.unwrap(), Vec::<WindowId>::new());
    }

    #[test]
    fn parses_a_real_active_window_id() {
        assert_eq!(
            parse_single_id(GETACTIVEWINDOW),
            Some(WindowId::new("{cdbe9b36-5750-4d4a-8338-1479a28592da}"))
        );
    }

    #[test]
    fn parses_no_active_window() {
        assert_eq!(parse_single_id(""), None);
        assert_eq!(parse_single_id("\n"), None);
    }

    #[test]
    fn non_zero_exit_with_nothing_on_either_stream_is_read_as_no_match() {
        let result = interpret_search_output("", false, "");
        assert_eq!(result.unwrap(), Vec::<WindowId>::new());
    }

    #[test]
    fn non_zero_exit_with_stderr_and_empty_stdout_is_a_command_failure() {
        let result = interpret_search_output("", false, "kdotool: no KWin scripting interface");
        assert!(matches!(result, Err(BackendError::CommandFailed(_))));
    }

    #[test]
    fn non_zero_exit_with_matches_on_stdout_still_returns_them() {
        // Defensive: if kdotool ever prints a partial match list alongside a
        // non-zero exit, prefer the data it did produce over discarding it.
        let result = interpret_search_output("{aaaa}\n", false, "some warning");
        assert_eq!(result.unwrap(), vec![WindowId::new("{aaaa}")]);
    }
}
