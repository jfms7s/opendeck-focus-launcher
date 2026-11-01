//! Generic X11 backend: `wmctrl` for listing/activating/closing, `xdotool`
//! for the focused window and minimizing.

use super::process::{check_status, run};
use super::{BackendError, WindowBackend, WindowClass, WindowId};
use async_trait::async_trait;
use std::collections::HashMap;

pub struct X11Backend;

/// The canonical X11 id form shared by both tools' output: `0x%08x`, which
/// is what `wmctrl -l` prints.
fn canonical_id(xid: u64) -> WindowId {
    WindowId::new(format!("0x{xid:08x}"))
}

/// The `WM_CLASS` halves to match against, from wmctrl's joined
/// `<instance>.<class>` column. Both halves may themselves contain dots
/// (`com.anthropic.claude.com.anthropic.Claude`), so the join is ambiguous.
/// Most apps set the two halves to the same string up to case, so a split
/// where they agree is taken as the real one. Otherwise every possible split
/// is offered; matching stays exact equality, so this can only widen the
/// match to a full dot-separated head or tail of the joined string, never a
/// substring.
fn wm_class_candidates(joined: &str) -> Vec<&str> {
    if joined == "N/A" {
        return Vec::new();
    }
    let splits: Vec<(&str, &str)> = joined
        .match_indices('.')
        .map(|(i, _)| (&joined[..i], &joined[i + 1..]))
        .collect();
    if splits.is_empty() {
        return vec![joined];
    }
    if let Some((instance, class)) = splits
        .iter()
        .find(|(instance, class)| instance.eq_ignore_ascii_case(class))
    {
        return vec![instance, class];
    }
    splits
        .into_iter()
        .flat_map(|(instance, class)| [instance, class])
        .collect()
}

/// Parses `wmctrl -l -x` output, keeping the windows that match `class`.
/// Each line is `<id> <desktop> <instance.class> <host> <title...>`. Lines
/// come in `_NET_CLIENT_LIST` (mapping) order, which is stable.
fn parse_wmctrl_list(stdout: &str, class: &WindowClass) -> Vec<WindowId> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let id = parse_hex_id(parts.next()?)?;
            let _desktop = parts.next()?;
            let wm_class = parts.next()?;
            wm_class_candidates(wm_class)
                .into_iter()
                .any(|candidate| class.matches(candidate))
                .then(|| canonical_id(id))
        })
        .collect()
}

/// Parses `wmctrl -l -x` output into each window's title: the rest of the
/// line after the host column, keyed by canonical id. A line with no title
/// gets `""`.
fn parse_wmctrl_titles(stdout: &str) -> HashMap<WindowId, String> {
    stdout
        .lines()
        .filter_map(|line| {
            let (id, _) = next_field(line)?;
            let id = parse_hex_id(id)?;
            let mut rest = line;
            for _ in 0..4 {
                rest = next_field(rest).map_or("", |(_, after)| after);
            }
            Some((canonical_id(id), rest.trim_start().to_string()))
        })
        .collect()
}

/// Splits off the first whitespace-separated field: `(field, rest)`.
fn next_field(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    let end = s.find(char::is_whitespace).unwrap_or(s.len());
    Some((&s[..end], &s[end..]))
}

fn parse_hex_id(raw: &str) -> Option<u64> {
    u64::from_str_radix(raw.strip_prefix("0x")?, 16).ok()
}

/// Parses `xdotool getactivewindow`, which prints the id in decimal, into
/// the canonical hex form so it compares equal to wmctrl's ids.
fn parse_xdotool_active_window(stdout: &str) -> Result<Option<WindowId>, BackendError> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let xid: u64 = trimmed.parse().map_err(|_| {
        BackendError::CommandFailed(format!(
            "unexpected xdotool getactivewindow output: {trimmed}"
        ))
    })?;
    Ok((xid != 0).then(|| canonical_id(xid)))
}

#[async_trait]
impl WindowBackend for X11Backend {
    async fn list_windows(&self, class: &WindowClass) -> Result<Vec<WindowId>, BackendError> {
        let output = run("wmctrl", &["-l", "-x"]).await?;
        check_status(&output, "wmctrl -l -x")?;
        Ok(parse_wmctrl_list(
            &String::from_utf8_lossy(&output.stdout),
            class,
        ))
    }

    async fn activate(&self, id: &WindowId) -> Result<(), BackendError> {
        let output = run("wmctrl", &["-i", "-a", id.as_str()]).await?;
        check_status(&output, "wmctrl -i -a")
    }

    async fn minimize(&self, id: &WindowId) -> Result<(), BackendError> {
        let output = run("xdotool", &["windowminimize", id.as_str()]).await?;
        check_status(&output, "xdotool windowminimize")
    }

    async fn close(&self, id: &WindowId) -> Result<(), BackendError> {
        let output = run("wmctrl", &["-i", "-c", id.as_str()]).await?;
        check_status(&output, "wmctrl -i -c")
    }

    async fn active_window(&self) -> Result<Option<WindowId>, BackendError> {
        let output = run("xdotool", &["getactivewindow"]).await?;
        check_status(&output, "xdotool getactivewindow")?;
        parse_xdotool_active_window(&String::from_utf8_lossy(&output.stdout))
    }

    /// One more `wmctrl -l -x`; a window that has gone since gets `""`.
    async fn window_titles(&self, ids: &[WindowId]) -> Result<Option<Vec<String>>, BackendError> {
        let output = run("wmctrl", &["-l", "-x"]).await?;
        check_status(&output, "wmctrl -l -x")?;
        let mut titles = parse_wmctrl_titles(&String::from_utf8_lossy(&output.stdout));
        Ok(Some(
            ids.iter()
                .map(|id| titles.remove(id).unwrap_or_default())
                .collect(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::NEAR_MISSES;

    // Captured from `wmctrl -l -x` on KDE Plasma 6 Wayland (XWayland
    // clients); hostname redacted to `myhost`. Both WM_CLASS halves of the
    // Claude window contain dots.
    const REAL_WMCTRL: &str = include_str!("../../tests/fixtures/wmctrl-l-x.stdout");

    fn class(s: &str) -> WindowClass {
        WindowClass::parse(s).unwrap()
    }

    fn ids(list: &[&str]) -> Vec<WindowId> {
        list.iter().map(|s| WindowId::new(*s)).collect()
    }

    #[test]
    fn matches_a_dotted_class_from_real_output() {
        assert_eq!(
            parse_wmctrl_list(REAL_WMCTRL, &class("com.anthropic.Claude")),
            ids(&["0x01800004"])
        );
        assert_eq!(
            parse_wmctrl_list(REAL_WMCTRL, &class("com.anthropic.claude")),
            ids(&["0x01800004"])
        );
    }

    #[test]
    fn matches_a_reverse_dns_desktop_id_used_as_the_class() {
        let stdout = "0x03e00007  0 org.gnome.Nautilus.Org.gnome.Nautilus  myhost Files\n";
        assert_eq!(
            parse_wmctrl_list(stdout, &class("org.gnome.Nautilus")),
            ids(&["0x03e00007"])
        );
    }

    #[test]
    fn matches_either_half_when_they_differ() {
        let stdout = "0x03e00007  0 Navigator.firefox  myhost Mozilla Firefox\n0x01c00003  0 kate.kate myhost Kate\n";
        assert_eq!(
            parse_wmctrl_list(stdout, &class("firefox")),
            ids(&["0x03e00007"])
        );
        assert_eq!(
            parse_wmctrl_list(stdout, &class("navigator")),
            ids(&["0x03e00007"])
        );
    }

    #[test]
    fn a_dotted_head_or_tail_is_not_a_substring_match() {
        assert!(parse_wmctrl_list(REAL_WMCTRL, &class("claude")).is_empty());
        assert!(parse_wmctrl_list(REAL_WMCTRL, &class("anthropic")).is_empty());
        assert!(parse_wmctrl_list(REAL_WMCTRL, &class("xwayland")).is_empty());
    }

    #[test]
    fn contract_near_misses_do_not_match() {
        for (window_class, wanted) in NEAR_MISSES {
            let stdout = format!("0x00000001  0 {window_class}.{window_class}  myhost t\n");
            assert!(
                parse_wmctrl_list(&stdout, &class(wanted)).is_empty(),
                "{wanted} must not match {window_class}"
            );
        }
    }

    #[test]
    fn windows_without_a_wm_class_never_match() {
        let stdout = "0x00000001  0 N/A  myhost untitled\n";
        assert!(parse_wmctrl_list(stdout, &class("N")).is_empty());
    }

    #[test]
    fn ids_are_canonicalized_from_wmctrl_hex() {
        let stdout = "0x3e00007  -1 kate.kate myhost Kate\n";
        assert_eq!(
            parse_wmctrl_list(stdout, &class("kate")),
            ids(&["0x03e00007"])
        );
    }

    #[test]
    fn parses_titles_with_spaces_from_real_output() {
        let titles = parse_wmctrl_titles(REAL_WMCTRL);
        assert_eq!(titles.len(), 2);
        assert_eq!(titles[&WindowId::new("0x01800004")], "Claude");
        assert_eq!(
            titles[&WindowId::new("0x01200008")],
            "Wayland to X Recording bridge — Xwayland Video Bridge"
        );
    }

    #[test]
    fn title_ids_are_canonical_and_a_missing_title_is_empty() {
        let stdout =
            "0x3e00007  -1 kate.kate myhost\n0x01c00003  0 kate.kate myhost   two  spaces\n";
        let titles = parse_wmctrl_titles(stdout);
        assert_eq!(titles[&WindowId::new("0x03e00007")], "");
        assert_eq!(titles[&WindowId::new("0x01c00003")], "two  spaces");
    }

    #[test]
    fn xdotool_decimal_id_normalizes_to_wmctrl_hex() {
        // 0x01800004 from the real wmctrl fixture is 25165828 in decimal.
        let active = parse_xdotool_active_window("25165828\n").unwrap();
        assert_eq!(active, Some(WindowId::new("0x01800004")));
        assert_eq!(
            parse_wmctrl_list(REAL_WMCTRL, &class("com.anthropic.Claude")).first(),
            active.as_ref()
        );
    }

    #[test]
    fn no_active_window_when_empty_or_zero() {
        assert_eq!(parse_xdotool_active_window("").unwrap(), None);
        assert_eq!(parse_xdotool_active_window("\n").unwrap(), None);
        assert_eq!(parse_xdotool_active_window("0\n").unwrap(), None);
    }

    #[test]
    fn garbage_active_window_output_is_an_error() {
        assert!(matches!(
            parse_xdotool_active_window("0x0\n"),
            Err(BackendError::CommandFailed(_))
        ));
    }
}
