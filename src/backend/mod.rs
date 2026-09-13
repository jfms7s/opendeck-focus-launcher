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
