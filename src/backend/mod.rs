//! Window backends: one adapter per desktop, all behind `WindowBackend`.

pub mod class;
#[cfg(not(target_os = "macos"))]
pub mod gnome;
#[cfg(not(target_os = "macos"))]
pub mod kdotool;
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
/// each adapter's parser tests do (see `contract_tests` below).
///
/// - **Matching:** `list_windows` returns exactly the windows for which
///   `WindowClass::matches` holds against the window's class or instance
///   name: exact, ASCII-case-insensitive equality. Never substring, prefix
///   or regex matching. A backend that can only take a pattern must use
///   `WindowClass::anchored_regex`.
/// - **Ordering:** a stable order that does not change when a window is
///   focused or raised (creation / mapping order: `KWin` `windowList()`,
///   `_NET_CLIENT_LIST`, Window Calls ids sorted ascending). Cycling with
///   `(pos + 1) % len` then visits every window, and a background press
///   brings up `windows[0]`, the oldest window.
/// - **Identity:** ids from `list_windows` and `active_window` use the same
///   canonical `WindowId` form, so they compare equal with `==`.
/// - **Errors:** a missing tool is `Unavailable`; "no windows match" is
///   `Ok(vec![])`, never an error.
#[async_trait]
pub trait WindowBackend: Send + Sync {
    async fn list_windows(&self, class: &WindowClass) -> Result<Vec<WindowId>, BackendError>;
    async fn activate(&self, id: &WindowId) -> Result<(), BackendError>;
    async fn minimize(&self, id: &WindowId) -> Result<(), BackendError>;
    async fn close(&self, id: &WindowId) -> Result<(), BackendError>;
    async fn active_window(&self) -> Result<Option<WindowId>, BackendError>;

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

#[cfg(test)]
mod tests {
    use super::*;

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
    async fn default_snapshot_skips_the_active_query_when_nothing_matches() {
        let backend = OnlyLists {
            windows: vec![],
            active: Err(BackendError::CommandFailed("must not be asked".into())),
        };
        let snap = backend.snapshot(&firefox()).await.unwrap();
        assert_eq!(snap, WindowSnapshot::default());
    }
}
