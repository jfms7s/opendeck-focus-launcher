//! The platform-neutral half of the macOS backend: window ids, process →
//! bundle mapping and window order. Kept apart from the AX calls in
//! `macos.rs` so the Linux test suite covers it.

use super::WindowId;
use std::path::{Path, PathBuf};

/// A window as `<pid>/<CGWindowID>`: the pid finds its app again, the
/// window number tells its windows apart.
pub fn window_id(pid: i32, window: u32) -> WindowId {
    WindowId::new(format!("{pid}/{window}"))
}

/// The pid and window number in an id from `window_id`; `None` for anything
/// else, so a bad id is never acted on.
pub fn parse_window_id(id: &WindowId) -> Option<(i32, u32)> {
    let (pid, window) = id.as_str().split_once('/')?;
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(pid) || !all_digits(window) {
        return None;
    }
    let pid: i32 = pid.parse().ok()?;
    let window: u32 = window.parse().ok()?;
    (pid > 0 && window > 0).then_some((pid, window))
}

/// The `.app` bundle whose main executable `exe` is
/// (`<X>.app/Contents/MacOS/<exe>`); `None` for any other process.
pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let named = |p: &Path, n: &str| p.file_name().is_some_and(|f| f == n);
    (named(macos, "MacOS")
        && named(contents, "Contents")
        && bundle.extension().is_some_and(|e| e == "app"))
    .then(|| bundle.to_path_buf())
}

/// `(window number, pid)` pairs as ids, oldest first: window numbers grow
/// as windows are created, which gives the backend contract's stable order.
pub fn ordered(mut windows: Vec<(u32, i32)>) -> Vec<WindowId> {
    windows.sort_unstable();
    windows
        .into_iter()
        .map(|(w, pid)| window_id(pid, w))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_carry_the_pid_and_the_window_number() {
        let id = window_id(4242, 7);
        assert_eq!(id.as_str(), "4242/7");
        assert_eq!(parse_window_id(&id), Some((4242, 7)));
    }

    #[test]
    fn malformed_ids_never_parse() {
        for bad in [
            "", "4242", "4242/", "/7", "a/7", "4242/b", "0/7", "-1/7", "4242/0", "1/2/3", " 1/2",
        ] {
            assert_eq!(parse_window_id(&WindowId::new(bad)), None, "{bad:?}");
        }
    }

    #[test]
    fn an_apps_main_executable_maps_to_its_bundle() {
        assert_eq!(
            bundle_of(Path::new("/Applications/Safari.app/Contents/MacOS/Safari")),
            Some(PathBuf::from("/Applications/Safari.app"))
        );
        // A helper app inside another bundle maps to the helper, not its host.
        assert_eq!(
            bundle_of(Path::new(
                "/Applications/Chrome.app/Contents/Frameworks/X.framework/Helpers/Helper.app/Contents/MacOS/Helper"
            )),
            Some(PathBuf::from(
                "/Applications/Chrome.app/Contents/Frameworks/X.framework/Helpers/Helper.app"
            ))
        );
        for not_an_app in [
            "/usr/bin/zsh",
            "/Applications/Safari.app/Contents/Resources/tool",
            "/opt/Thing/Contents/MacOS/thing",
        ] {
            assert_eq!(bundle_of(Path::new(not_an_app)), None, "{not_an_app}");
        }
    }

    #[test]
    fn windows_are_ordered_oldest_first() {
        let ids = ordered(vec![(30, 2), (10, 1), (20, 1)]);
        let ids: Vec<_> = ids.iter().map(WindowId::as_str).collect();
        assert_eq!(ids, ["1/10", "1/20", "2/30"]);
    }
}
