//! KDE Plasma backend: shells out to `kdotool`, which renders a `KWin` script
//! from a template and runs it inside the compositor.

use super::process::{check_status, run};
use super::{BackendError, WindowBackend, WindowClass, WindowId, WindowSnapshot};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Whether a window belongs to a class it doesn't match by `resourceClass`
/// (so only, if at all, by `resourceName`), keyed by window id and then by
/// the class in lower case. See `snapshot_many`.
type NameMatches = HashMap<WindowId, HashMap<String, bool>>;

pub struct KdotoolBackend {
    name_matches: Mutex<NameMatches>,
}

impl KdotoolBackend {
    pub fn new() -> Self {
        Self {
            name_matches: Mutex::new(HashMap::new()),
        }
    }

    fn name_matches(&self) -> MutexGuard<'_, NameMatches> {
        self.name_matches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for KdotoolBackend {
    fn default() -> Self {
        Self::new()
    }
}

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

/// Arguments for the live state poller's one kdotool run per tick: the
/// focused window's id, then every window matching any of `classes`
/// (`--class` plus `--classname`, as `search_args`), then each of those
/// windows' `resourceClass`. The pattern is the classes' anchored regexes
/// joined with `|`, so it is built only from validated classes. kdotool
/// can't run two `search`es in one script (v0.2.3 fails with "No such
/// object path '/Scripting/Script0'"), hence one alternation.
fn classed_search_args(classes: &[WindowClass]) -> Vec<String> {
    let pattern: Vec<String> = classes.iter().map(WindowClass::anchored_regex).collect();
    let mut args: Vec<String> = [
        "getactivewindow",
        "getwindowid",
        "%1",
        "search",
        "--class",
        "--classname",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    args.push(pattern.join("|"));
    args.extend(
        ["getwindowid", "%@", "getwindowclassname", "%@"]
            .into_iter()
            .map(str::to_string),
    );
    args
}

/// The parsed `classed_search_args` output.
#[derive(Debug, PartialEq, Eq)]
struct ClassedSearch {
    active: Option<WindowId>,
    /// Each matching window and its `resourceClass`, in `windowList()` order.
    windows: Vec<(WindowId, String)>,
}

fn is_window_id(line: &str) -> bool {
    line.len() > 2 && line.starts_with('{') && line.ends_with('}')
}

/// Parses `classed_search_args` output (captured with kdotool v0.2.3 on
/// Plasma 6): an optional focused-window id (absent when nothing is
/// focused), N window ids, then N classes, one per line. A class can be
/// empty, so blank lines are kept; the line count's parity says whether the
/// focused id is there. Anything malformed is an error, never a guess.
fn parse_classed_search(stdout: &str) -> Result<ClassedSearch, BackendError> {
    let bad = |what: String| BackendError::CommandFailed(format!("kdotool state query: {what}"));
    let lines: Vec<&str> = stdout
        .lines()
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    let (active, rest) = if lines.len() % 2 == 1 {
        let first = lines[0].trim();
        if !is_window_id(first) {
            return Err(bad(format!("{first:?} is not a window id")));
        }
        (Some(WindowId::new(first)), &lines[1..])
    } else {
        (None, &lines[..])
    };
    let (ids, classes) = rest.split_at(rest.len() / 2);
    let mut windows = Vec::with_capacity(ids.len());
    for (id, class) in ids.iter().zip(classes) {
        let id = id.trim();
        if !is_window_id(id) {
            return Err(bad(format!("{id:?} is not a window id")));
        }
        windows.push((WindowId::new(id), class.trim().to_string()));
    }
    Ok(ClassedSearch { active, windows })
}

fn class_key(class: &WindowClass) -> String {
    class.as_str().to_ascii_lowercase()
}

/// The classes (by index) for which some listed window neither matches by
/// `resourceClass` nor has a cached answer: each needs one `search`.
fn unresolved_classes(
    classes: &[WindowClass],
    windows: &[(WindowId, String)],
    cache: &NameMatches,
) -> Vec<usize> {
    (0..classes.len())
        .filter(|&i| {
            let key = class_key(&classes[i]);
            windows.iter().any(|(id, resource_class)| {
                !classes[i].matches(resource_class)
                    && !cache.get(id).is_some_and(|m| m.contains_key(&key))
            })
        })
        .collect()
}

/// Records one per-class `search` result (`listed`) for every window that
/// doesn't match `class` by `resourceClass`.
fn record_search(
    cache: &mut NameMatches,
    class: &WindowClass,
    windows: &[(WindowId, String)],
    listed: &[WindowId],
) {
    let key = class_key(class);
    for (id, resource_class) in windows {
        if !class.matches(resource_class) {
            cache
                .entry(id.clone())
                .or_default()
                .insert(key.clone(), listed.contains(id));
        }
    }
}

/// Each class's windows: those whose `resourceClass` matches, plus those the
/// cache says its `search` lists. A pair with no answer counts as "no".
fn assign(
    classes: &[WindowClass],
    search: &ClassedSearch,
    cache: &NameMatches,
) -> Vec<WindowSnapshot> {
    classes
        .iter()
        .map(|class| {
            let key = class_key(class);
            let windows = search
                .windows
                .iter()
                .filter(|(id, resource_class)| {
                    class.matches(resource_class)
                        || cache
                            .get(id)
                            .and_then(|m| m.get(&key))
                            .copied()
                            .unwrap_or(false)
                })
                .map(|(id, _)| id.clone())
                .collect();
            WindowSnapshot {
                windows,
                active: search.active.clone(),
            }
        })
        .collect()
}

/// Arguments for one kdotool run that prints the current desktop number,
/// then the class's window ids, then each of those windows' desktop. Only
/// fixed words and the anchored class pattern (see `search_args`) are
/// passed: the desktop number is read from the output, never sent back.
fn current_desktop_args(class: &WindowClass) -> Vec<String> {
    let mut args = vec!["get_desktop".to_string()];
    args.extend(search_args(class));
    args.extend(
        ["getwindowid", "%@", "get_desktop_for_window", "%@"]
            .into_iter()
            .map(str::to_string),
    );
    args
}

/// Parses `current_desktop_args`' output (captured with kdotool v0.2.3 on
/// Plasma 6): the current desktop, N window ids, then N desktops, where
/// `null` is a window on every desktop (`KWin` 6 lists no desktops for it).
/// Returns the ids on the current desktop or on all desktops, in order. A
/// window on several (not all) desktops is judged by its first one, which
/// is all `get_desktop_for_window` prints. Anything malformed is an error:
/// guessing would launch duplicates on every press.
fn parse_current_desktop_windows(stdout: &str) -> Result<Vec<WindowId>, BackendError> {
    let bad = |what: String| BackendError::CommandFailed(format!("kdotool desktop query: {what}"));
    let mut lines = stdout.lines().map(str::trim).filter(|l| !l.is_empty());
    let current: u32 = lines
        .next()
        .ok_or_else(|| bad("no current desktop".to_string()))?
        .parse()
        .map_err(|e| bad(format!("current desktop is not a number: {e}")))?;
    let rest: Vec<&str> = lines.collect();
    if !rest.len().is_multiple_of(2) {
        return Err(bad(format!("{} lines for ids and desktops", rest.len())));
    }
    let (ids, desktops) = rest.split_at(rest.len() / 2);
    let mut here = Vec::new();
    for (id, desktop) in ids.iter().zip(desktops) {
        if !(id.starts_with('{') && id.ends_with('}')) {
            return Err(bad(format!("{id:?} is not a window id")));
        }
        let on_current = match *desktop {
            "null" => true,
            n => {
                n.parse::<u32>()
                    .map_err(|e| bad(format!("desktop {n:?} is not a number: {e}")))?
                    == current
            }
        };
        if on_current {
            here.push(WindowId::new(*id));
        }
    }
    Ok(here)
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

/// Parses `kdotool getwindowname <id>`: the title followed by one newline.
/// An id whose window has gone prints nothing (exit 0), read as `""`.
/// Only the final newline is removed; the title's own spaces are kept.
fn parse_window_name(stdout: &str) -> String {
    let title = stdout.strip_suffix('\n').unwrap_or(stdout);
    title.strip_suffix('\r').unwrap_or(title).to_string()
}

/// Every window's class: `kdotool search --class . getwindowclassname %@`
/// (the README's manual command). The pattern is a fixed constant, never
/// user text, so nothing reaches kdotool's generated `KWin` script.
const CLASSES_ARGS: &[&str] = &["search", "--class", ".", "getwindowclassname", "%@"];

/// Parses the `CLASSES_ARGS` output: one class per line, one line per
/// window (captured with kdotool v0.2.3 on Plasma 6 Wayland).
fn parse_classes(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
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

    /// One kdotool run: current desktop, ids, and each window's desktop.
    async fn current_desktop_windows(
        &self,
        class: &WindowClass,
    ) -> Result<Option<Vec<WindowId>>, BackendError> {
        let args = current_desktop_args(class);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = run("kdotool", &args).await?;
        check_status(&output, "kdotool get_desktop")?;
        parse_current_desktop_windows(&String::from_utf8_lossy(&output.stdout)).map(Some)
    }

    /// One kdotool run per tick in the steady state (`classed_search_args`).
    /// `getwindowclassname` prints only `resourceClass`, so a window that
    /// matched a class by `resourceName` alone (VS Code's `code`) can't be
    /// placed from that output: for each (window, class) pair with no
    /// `resourceClass` match, one per-class `search` decides, and the answer
    /// is cached by window id. Ids are never reused and a window's names
    /// don't change, so the cache stays exact; ids no longer listed are
    /// dropped. A new matching window costs at most one extra `search` per
    /// class, once. The lock is never held across a kdotool run.
    async fn snapshot_many(
        &self,
        classes: &[WindowClass],
    ) -> Result<Vec<WindowSnapshot>, BackendError> {
        if classes.is_empty() {
            return Ok(Vec::new());
        }
        let args = classed_search_args(classes);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = run("kdotool", &args).await?;
        check_status(&output, "kdotool search")?;
        let search = parse_classed_search(&String::from_utf8_lossy(&output.stdout))?;
        let unresolved = {
            let mut cache = self.name_matches();
            cache.retain(|id, _| search.windows.iter().any(|(w, _)| w == id));
            unresolved_classes(classes, &search.windows, &cache)
        };
        for i in unresolved {
            let listed = self.list_windows(&classes[i]).await?;
            record_search(
                &mut self.name_matches(),
                &classes[i],
                &search.windows,
                &listed,
            );
        }
        Ok(assign(classes, &search, &self.name_matches()))
    }

    async fn open_window_classes(&self) -> Result<Vec<String>, BackendError> {
        let output = run("kdotool", CLASSES_ARGS).await?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let classes = parse_classes(&stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if classes.is_empty() && !output.status.success() && !stderr.trim().is_empty() {
            return Err(BackendError::CommandFailed(format!(
                "kdotool search failed: {}",
                stderr.trim()
            )));
        }
        Ok(classes)
    }

    /// One `kdotool getwindowname <id>` per window. Not chained after
    /// `search`: separate runs need no line splitting, so a title with a
    /// newline in it can't shift titles onto other windows. Only the fixed
    /// subcommand and a kdotool-issued id are passed; user text (the title
    /// filter) never reaches kdotool.
    async fn window_titles(&self, ids: &[WindowId]) -> Result<Option<Vec<String>>, BackendError> {
        let mut titles = Vec::with_capacity(ids.len());
        for id in ids {
            let output = run("kdotool", &["getwindowname", id.as_str()]).await?;
            check_status(&output, "kdotool getwindowname")?;
            titles.push(parse_window_name(&String::from_utf8_lossy(&output.stdout)));
        }
        Ok(Some(titles))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured from kdotool v0.2.3 on KDE Plasma 6 (Wayland): `kdotool
    // getactivewindow getwindowid %1 search --class --classname
    // '^org\.kde\.konsole$|^code$|^zzz$' getwindowid %@ getwindowclassname
    // %@`, exit 0. The focused window (not a match), Konsole, then VS Code,
    // which matched `code` by its resourceName: its resourceClass is
    // `com.microsoft.VSCode`.
    const CLASSED: &str = include_str!("../../tests/fixtures/kdotool-search-classnames.stdout");

    fn classes(names: &[&str]) -> Vec<WindowClass> {
        names
            .iter()
            .map(|c| WindowClass::parse(c).unwrap())
            .collect()
    }

    #[test]
    fn the_state_query_passes_only_fixed_words_and_anchored_classes() {
        let wanted = classes(&["org.kde.konsole", "code"]);
        assert_eq!(
            classed_search_args(&wanted),
            [
                "getactivewindow",
                "getwindowid",
                "%1",
                "search",
                "--class",
                "--classname",
                r"^org\.kde\.konsole$|^code$",
                "getwindowid",
                "%@",
                "getwindowclassname",
                "%@",
            ]
        );
    }

    #[test]
    fn parses_the_real_state_query_output() {
        let search = parse_classed_search(CLASSED).unwrap();
        assert_eq!(
            search.active,
            Some(WindowId::new("{d9fb1f05-c21d-42ba-b854-60bb869231a0}"))
        );
        assert_eq!(
            search.windows,
            [
                (
                    WindowId::new("{6bd0e314-ef58-41f0-83aa-2cf821a91a35}"),
                    "org.kde.konsole".to_string()
                ),
                (
                    WindowId::new("{bb1fdfae-a29e-4a87-8fe1-ef2ed1201c55}"),
                    "com.microsoft.VSCode".to_string()
                ),
            ]
        );
    }

    #[test]
    fn the_state_query_without_a_focused_window_or_matches() {
        assert_eq!(
            parse_classed_search("").unwrap(),
            ClassedSearch {
                active: None,
                windows: vec![]
            }
        );
        assert_eq!(
            parse_classed_search("{a}\n").unwrap().active,
            Some(WindowId::new("{a}"))
        );
        let no_focus = parse_classed_search("{a}\n{b}\nk\n\n").unwrap();
        assert_eq!(no_focus.active, None);
        assert_eq!(
            no_focus.windows,
            [
                (WindowId::new("{a}"), "k".to_string()),
                (WindowId::new("{b}"), String::new())
            ]
        );
    }

    #[test]
    fn malformed_state_output_is_an_error_not_a_guess() {
        for out in [
            "konsole\n",
            "x\n{a}\nk\n",
            "{a}\nk\n{b}\n",
            "{a}\n{b}\nk\n{c}\n{d}\n",
        ] {
            assert!(
                matches!(
                    parse_classed_search(out),
                    Err(BackendError::CommandFailed(_))
                ),
                "{out:?}"
            );
        }
    }

    #[test]
    fn a_window_matched_by_its_name_only_is_placed_by_one_search_then_cached() {
        let wanted = classes(&["org.kde.konsole", "Code"]);
        let search = parse_classed_search(CLASSED).unwrap();
        let konsole = search.windows[0].0.clone();
        let vscode = search.windows[1].0.clone();
        let mut cache = NameMatches::new();
        // Konsole doesn't match `Code` by class and VS Code doesn't match
        // either class by class: both classes need a search.
        assert_eq!(unresolved_classes(&wanted, &search.windows, &cache), [0, 1]);
        record_search(
            &mut cache,
            &wanted[0],
            &search.windows,
            std::slice::from_ref(&konsole),
        );
        record_search(
            &mut cache,
            &wanted[1],
            &search.windows,
            std::slice::from_ref(&vscode),
        );
        assert!(unresolved_classes(&wanted, &search.windows, &cache).is_empty());
        let snapshots = assign(&wanted, &search, &cache);
        assert_eq!(snapshots[0].windows, [konsole]);
        assert_eq!(snapshots[1].windows, [vscode]);
        assert_eq!(snapshots[1].active, search.active);
        // A new class reopens only its own question.
        let more = classes(&["org.kde.konsole", "Code", "org.kde.dolphin"]);
        assert_eq!(unresolved_classes(&more, &search.windows, &cache), [2]);
    }

    #[test]
    fn a_pair_with_no_answer_counts_as_not_matching() {
        let wanted = classes(&["code"]);
        let search = parse_classed_search(CLASSED).unwrap();
        let snapshots = assign(&wanted, &search, &NameMatches::new());
        assert!(snapshots[0].windows.is_empty());
    }

    // Captured from kdotool v0.2.3 on KDE Plasma 6 (Wayland), one desktop:
    // `kdotool get_desktop search --class --classname
    // '^(plasmashell|org\.kde\.konsole)$' getwindowid %@
    // get_desktop_for_window %@`, exit 0. Konsole is on desktop 1; the four
    // plasmashell windows are on every desktop (`null`).
    const CURRENT_DESKTOP: &str =
        include_str!("../../tests/fixtures/kdotool-current-desktop.stdout");

    #[test]
    fn keeps_windows_on_the_current_desktop_and_sticky_ones_from_real_output() {
        let here = parse_current_desktop_windows(CURRENT_DESKTOP).unwrap();
        assert_eq!(here.len(), 5);
        assert_eq!(
            here[0],
            WindowId::new("{6bd0e314-ef58-41f0-83aa-2cf821a91a35}")
        );
    }

    #[test]
    fn drops_windows_on_another_desktop() {
        let out = "2\n{a}\n{b}\n{c}\n1\n2\nnull\n";
        assert_eq!(
            parse_current_desktop_windows(out).unwrap(),
            [WindowId::new("{b}"), WindowId::new("{c}")]
        );
    }

    #[test]
    fn no_matching_window_prints_only_the_desktop() {
        assert_eq!(parse_current_desktop_windows("3\n").unwrap(), []);
    }

    #[test]
    fn malformed_desktop_output_is_an_error_not_a_guess() {
        for out in [
            "",
            "one\n{a}\n1\n",
            "-1\n{a}\n1\n",
            "1\n{a}\n",
            "1\n{a}\nx\n",
            "1\nnot-an-id\n1\n",
            "1\n{a}\n{b}\n1\n",
        ] {
            assert!(
                matches!(
                    parse_current_desktop_windows(out),
                    Err(BackendError::CommandFailed(_))
                ),
                "{out:?}"
            );
        }
    }

    #[test]
    fn the_desktop_query_passes_only_fixed_words_and_the_anchored_class() {
        let class = WindowClass::parse("org.kde.konsole").unwrap();
        assert_eq!(
            current_desktop_args(&class),
            [
                "get_desktop",
                "search",
                "--class",
                "--classname",
                &class.anchored_regex(),
                "getwindowid",
                "%@",
                "get_desktop_for_window",
                "%@",
            ]
        );
    }

    // Captured from kdotool v0.2.3 on KDE Plasma 6 (Wayland):
    // `kdotool search --class --classname '^plasmashell$'`, exit 0.
    const SEARCH_PLASMASHELL: &str =
        include_str!("../../tests/fixtures/kdotool-search-plasmashell.stdout");
    /// `kdotool search --class --classname '^plasmashell$' getwindowclassname %@`
    /// on the development machine: the `CLASSES_ARGS` output format, captured
    /// with a filter matching only Plasma's own windows.
    const CLASSNAMES: &str =
        include_str!("../../tests/fixtures/kdotool-getwindowclassname-all.stdout");

    #[test]
    fn chained_class_names_are_one_per_window() {
        let classes = parse_classes(CLASSNAMES);
        assert_eq!(classes.len(), parse_search_output(SEARCH_PLASMASHELL).len());
        assert!(classes.iter().all(|c| c == "plasmashell"));
        assert!(parse_classes("").is_empty());
    }
    // `kdotool search --class --classname '^opendeck-focus-launcher-no-such-app$'`:
    // empty output, exit 0.
    const SEARCH_NO_MATCH: &str =
        include_str!("../../tests/fixtures/kdotool-search-no-match.stdout");
    // `kdotool getactivewindow`, exit 0.
    const GETACTIVEWINDOW: &str =
        include_str!("../../tests/fixtures/kdotool-getactivewindow.stdout");
    // Captured from kdotool v0.2.3 on KDE Plasma 6 (Wayland):
    // `kdotool getwindowname <id>` for a Konsole window, exit 0; the
    // directory name in the title is redacted to `myproject`.
    const GETWINDOWNAME: &str = include_str!("../../tests/fixtures/kdotool-getwindowname.stdout");
    // `kdotool getwindowname '{00000000-0000-0000-0000-000000000000}'` (no such
    // window): empty output, exit 0.
    const GETWINDOWNAME_MISSING: &str =
        include_str!("../../tests/fixtures/kdotool-getwindowname-missing.stdout");

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
    fn parses_a_real_window_name() {
        assert_eq!(parse_window_name(GETWINDOWNAME), "myproject : uv — Konsole");
    }

    #[test]
    fn a_vanished_window_has_an_empty_name() {
        assert_eq!(parse_window_name(GETWINDOWNAME_MISSING), "");
    }

    #[test]
    fn only_the_final_newline_is_removed_from_a_name() {
        assert_eq!(parse_window_name("  a  b \n"), "  a  b ");
        assert_eq!(
            parse_window_name("line one\nline two\r\n"),
            "line one\nline two"
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
