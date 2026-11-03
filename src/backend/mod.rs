//! Window backends: one adapter per desktop, all behind `WindowBackend`.

pub mod class;
#[cfg(not(target_os = "macos"))]
pub mod gnome;
#[cfg(not(target_os = "macos"))]
pub mod kdotool;
// The macOS backend's platform-neutral half; tested everywhere.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub mod mac_ids;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(not(target_os = "macos"))]
mod process;
pub mod select;
#[cfg(not(target_os = "macos"))]
pub mod x11;

use async_trait::async_trait;
use thiserror::Error;

pub use class::WindowClass;
pub use select::{BackendKind, select_backend};

/// A window id in its backend's single canonical form. Ids are only ever
/// compared with ids produced by the *same* backend (`list_windows` against
/// `active_window`), so each backend must build both from one constructor
/// (for example X11 always formats ids as `0x%08x`, whichever tool reported
/// them). Treat the contents as opaque.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WindowId(String);

impl WindowId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WindowId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum BackendError {
    /// The tool, extension or bus the backend needs is missing. Never
    /// treated as "no windows", so it can't cause a duplicate launch.
    #[error("the window backend for this desktop is not available: {0}")]
    Unavailable(String),
    /// The backend is present but a call failed.
    #[error("backend command failed: {0}")]
    CommandFailed(String),
}

/// The matching windows plus the focused window, taken together.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowSnapshot {
    pub windows: Vec<WindowId>,
    pub active: Option<WindowId>,
}

/// The contract every backend must meet. `decide()` and the orchestration
/// tests rely on it, and the fake backend in the tests cannot check it, so
/// each adapter's parser tests do (see `NEAR_MISSES` below). A new backend
/// gets an arm in `near_miss_probe`; its exhaustive match makes the compiler
/// ask for one.
///
/// - **Matching:** `list_windows` returns exactly the windows for which
///   `WindowClass::matches` holds against the window's class or instance
///   name: exact, ASCII-case-insensitive equality. Never substring, prefix
///   or regex matching. A backend that can only take a pattern must use
///   `WindowClass::anchored_regex`. On macOS the candidate is the bundle id
///   of the window's owning app (`mac_ids::matching_pids`).
/// - **Ordering:** a stable order that does not change when a window is
///   focused or raised (creation / mapping order: `KWin` `windowList()`,
///   `_NET_CLIENT_LIST`, Window Calls ids sorted ascending; CGWindowID
///   ascending on macOS, `mac_ids::ordered`). Cycling with
///   `(pos + 1) % len` then visits every window, and a background press
///   brings up `windows[0]`, the oldest window.
/// - **Identity:** ids from `list_windows` and `active_window` use the same
///   canonical `WindowId` form, so they compare equal with `==`.
/// - **Errors:** a missing tool is `Unavailable`; "no windows match" is
///   `Ok(vec![])`, never an error.
/// - **Desktops:** `current_desktop_windows` returns the class's windows on
///   the current desktop or on all desktops; `Ok(None)` means unsupported.
/// - **Titles:** `window_titles` returns one title per id, in the same
///   order, `""` for a window that has gone; `Ok(None)` means the backend
///   can't read titles at all. It is only called for keys with a title
///   filter, so keys without one make no extra call.
#[async_trait]
pub trait WindowBackend: Send + Sync {
    async fn list_windows(&self, class: &WindowClass) -> Result<Vec<WindowId>, BackendError>;
    async fn activate(&self, id: &WindowId) -> Result<(), BackendError>;
    async fn minimize(&self, id: &WindowId) -> Result<(), BackendError>;
    async fn close(&self, id: &WindowId) -> Result<(), BackendError>;
    async fn active_window(&self) -> Result<Option<WindowId>, BackendError>;

    /// The titles of `ids` (see "Titles" above). The default is for a
    /// backend that can't read them: the caller then ignores the filter.
    async fn window_titles(&self, _ids: &[WindowId]) -> Result<Option<Vec<String>>, BackendError> {
        Ok(None)
    }

    /// The windows of `class` on the current virtual desktop, plus those on
    /// every desktop ("sticky"), for keys with "Only windows on the current
    /// desktop" on; the caller keeps the snapshot's windows that are in it.
    /// Only called for those keys, so other keys make no extra call. The
    /// default, `Ok(None)`, is for a backend that can't tell desktops apart
    /// (GNOME, X11, macOS): the caller then ignores the option.
    async fn current_desktop_windows(
        &self,
        _class: &WindowClass,
    ) -> Result<Option<Vec<WindowId>>, BackendError> {
        Ok(None)
    }

    /// One class per open window (any order, duplicates allowed), for the
    /// property inspector's "pick from open windows" list: a class that,
    /// typed as the key's Window class, matches that window. Only called when
    /// a property inspector opens. The default, for a backend that can't
    /// list every window (macOS, frozen), is empty: the PI hides the picker.
    async fn open_window_classes(&self) -> Result<Vec<String>, BackendError> {
        Ok(Vec::new())
    }

    /// Matching windows and the focused window. The default makes two
    /// queries; a backend whose tool returns both at once (Window Calls)
    /// overrides it with one. Failing to read the focused window is not
    /// fatal: it is logged and treated as "none of this app's windows is
    /// focused", so the press still brings the app forward.
    async fn snapshot(&self, class: &WindowClass) -> Result<WindowSnapshot, BackendError> {
        let windows = self.list_windows(class).await?;
        let active = if windows.is_empty() {
            None
        } else {
            match self.active_window().await {
                Ok(active) => active,
                Err(e) => {
                    log::warn!("could not read the focused window, assuming none: {e}");
                    None
                }
            }
        };
        Ok(WindowSnapshot { windows, active })
    }
}

/// Near-miss cases every backend's parser must reject, shared so the
/// adapters are held to one rule. Each entry is (window class, wanted class).
#[cfg(test)]
pub(crate) const NEAR_MISSES: &[(&str, &str)] = &[
    ("firefox-esr", "firefox"),
    ("firefoxdeveloperedition", "firefox"),
    ("steam", "st"),
    ("kate", "e"),
    ("org.kde.kate", "kate"),
];

/// A backend's matching rule run in-process on synthetic tool output: does a
/// window whose class (and instance) is `window_class` match `wanted`?
#[cfg(test)]
pub(crate) type NearMissProbe = fn(window_class: &str, wanted: &WindowClass) -> bool;

/// The one near-miss check: `probe` rejects every `NEAR_MISSES` pair and
/// accepts the exact class.
#[cfg(test)]
pub(crate) fn assert_rejects_near_misses(backend: &str, probe: NearMissProbe) {
    for (window_class, wanted) in NEAR_MISSES {
        let wanted = WindowClass::parse(wanted).unwrap();
        assert!(
            !probe(window_class, &wanted),
            "{backend}: {wanted} must not match {window_class}"
        );
    }
    let exact = WindowClass::parse("firefox").unwrap();
    assert!(
        probe("firefox", &exact),
        "{backend}: firefox must match itself"
    );
}

/// Every Linux backend's near-miss probe. The match has no `_` arm, so a new
/// `BackendKind` does not compile until it gets an arm here (and is added to
/// `every_backend_rejects_the_near_misses`).
#[cfg(all(test, not(target_os = "macos")))]
fn near_miss_probe(kind: BackendKind) -> Option<NearMissProbe> {
    match kind {
        BackendKind::X11 => Some(x11::near_miss_probe),
        BackendKind::GnomeWindowCalls => Some(gnome::near_miss_probe),
        // KWin evaluates the regex, so there is nothing to run in-process:
        // `search_args_anchor_and_escape_the_class` pins the anchored,
        // escaped pattern instead.
        BackendKind::Kdotool => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn every_backend_rejects_the_near_misses() {
        for kind in [
            BackendKind::Kdotool,
            BackendKind::GnomeWindowCalls,
            BackendKind::X11,
        ] {
            if let Some(probe) = near_miss_probe(kind) {
                assert_rejects_near_misses(kind.name(), probe);
            }
        }
    }

    struct OnlyLists {
        windows: Vec<WindowId>,
        active: Result<Option<WindowId>, BackendError>,
    }

    #[async_trait]
    impl WindowBackend for OnlyLists {
        async fn list_windows(&self, _class: &WindowClass) -> Result<Vec<WindowId>, BackendError> {
            Ok(self.windows.clone())
        }
        async fn activate(&self, _id: &WindowId) -> Result<(), BackendError> {
            unreachable!()
        }
        async fn minimize(&self, _id: &WindowId) -> Result<(), BackendError> {
            unreachable!()
        }
        async fn close(&self, _id: &WindowId) -> Result<(), BackendError> {
            unreachable!()
        }
        async fn active_window(&self) -> Result<Option<WindowId>, BackendError> {
            self.active.clone()
        }
    }

    fn firefox() -> WindowClass {
        WindowClass::parse("firefox").unwrap()
    }

    #[tokio::test]
    async fn default_snapshot_combines_list_and_active() {
        let backend = OnlyLists {
            windows: vec![WindowId::new("w1")],
            active: Ok(Some(WindowId::new("w1"))),
        };
        let snap = backend.snapshot(&firefox()).await.unwrap();
        assert_eq!(snap.windows, vec![WindowId::new("w1")]);
        assert_eq!(snap.active, Some(WindowId::new("w1")));
    }

    #[tokio::test]
    async fn default_snapshot_treats_a_failed_active_query_as_none() {
        let backend = OnlyLists {
            windows: vec![WindowId::new("w1")],
            active: Err(BackendError::CommandFailed("boom".into())),
        };
        let snap = backend.snapshot(&firefox()).await.unwrap();
        assert_eq!(snap.windows, vec![WindowId::new("w1")]);
        assert_eq!(snap.active, None);
    }

    #[tokio::test]
    async fn the_default_backend_cannot_read_titles() {
        let backend = OnlyLists {
            windows: vec![],
            active: Ok(None),
        };
        assert_eq!(
            backend.window_titles(&[WindowId::new("w1")]).await,
            Ok(None)
        );
    }

    #[tokio::test]
    async fn default_snapshot_skips_the_active_query_when_nothing_matches() {
        let backend = OnlyLists {
            windows: vec![],
            active: Err(BackendError::CommandFailed("must not be asked".into())),
        };
        let snap = backend.snapshot(&firefox()).await.unwrap();
        assert_eq!(snap, WindowSnapshot::default());
    }
}
