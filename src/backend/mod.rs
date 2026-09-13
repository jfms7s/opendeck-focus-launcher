use async_trait::async_trait;
use thiserror::Error;

pub type WindowId = String;

#[derive(Debug, Error)]
pub enum BackendError {
    #[error("the window backend for this desktop is not available: {0}")]
    Unavailable(String),
    #[error("backend command failed: {0}")]
    CommandFailed(String),
}

#[async_trait]
pub trait WindowBackend: Send + Sync {
    async fn list_windows(&self, class: &str) -> Result<Vec<WindowId>, BackendError>;
    async fn activate(&self, id: &WindowId) -> Result<(), BackendError>;
    async fn minimize(&self, id: &WindowId) -> Result<(), BackendError>;
    async fn active_window(&self) -> Result<Option<WindowId>, BackendError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AlwaysUnavailable;

    #[async_trait]
    impl WindowBackend for AlwaysUnavailable {
        async fn list_windows(&self, _class: &str) -> Result<Vec<WindowId>, BackendError> {
            Err(BackendError::Unavailable("test backend".into()))
        }
        async fn activate(&self, _id: &WindowId) -> Result<(), BackendError> {
            unreachable!()
        }
        async fn minimize(&self, _id: &WindowId) -> Result<(), BackendError> {
            unreachable!()
        }
        async fn active_window(&self) -> Result<Option<WindowId>, BackendError> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn trait_object_is_usable_through_a_dyn_reference() {
        let backend: Box<dyn WindowBackend> = Box::new(AlwaysUnavailable);
        let result = backend.list_windows("firefox").await;
        assert!(matches!(result, Err(BackendError::Unavailable(_))));
    }
}

pub mod kdotool;
pub mod gnome;
pub mod x11;

pub fn select_backend(
    current_desktop: Option<&str>,
    session_type: Option<&str>,
    display: Option<&str>,
) -> Option<Box<dyn WindowBackend>> {
    let desktop = current_desktop.unwrap_or_default().to_lowercase();
    if desktop.contains("kde") {
        return Some(Box::new(kdotool::KdotoolBackend));
    }
    if desktop.contains("gnome") {
        return Some(Box::new(gnome::GnomeWindowCallsBackend));
    }
    let _ = session_type;
    if display.is_some() {
        return Some(Box::new(x11::X11Backend));
    }
    None
}

#[cfg(test)]
mod selection_tests {
    use super::*;

    #[test]
    fn picks_kdotool_for_kde() {
        let backend = select_backend(Some("KDE"), Some("wayland"), None);
        assert!(backend.is_some());
    }

    #[test]
    fn picks_gnome_for_gnome_shell() {
        let backend = select_backend(Some("GNOME"), Some("wayland"), None);
        assert!(backend.is_some());
    }

    #[test]
    fn picks_x11_fallback_when_display_is_set() {
        let backend = select_backend(Some("XFCE"), Some("x11"), Some(":0"));
        assert!(backend.is_some());
    }

    #[test]
    fn refuses_to_guess_with_no_recognizable_signal() {
        let backend = select_backend(None, None, None);
        assert!(backend.is_none());
    }
}
