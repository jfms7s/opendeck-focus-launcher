use super::{BackendError, WindowBackend, WindowId};
use async_trait::async_trait;
use serde::Deserialize;

pub struct GnomeWindowCallsBackend;

#[derive(Debug, Deserialize)]
struct GnomeWindow {
    id: u32,
    wm_class: String,
    #[serde(default)]
    focus: bool,
}

/// Parses the JSON array returned by Window Calls' `List` D-Bus method,
/// keeping only windows whose class matches (case-insensitive substring,
/// matching how StartupWMClass/resource-class strings are compared elsewhere
/// in this project's shell-script prototype).
fn parse_window_list(json: &str, class: &str) -> Result<Vec<GnomeWindow>, BackendError> {
    let all: Vec<GnomeWindow> = serde_json::from_str(json)
        .map_err(|e| BackendError::CommandFailed(format!("bad Window Calls JSON: {e}")))?;
    let class_lower = class.to_lowercase();
    Ok(all
        .into_iter()
        .filter(|w| w.wm_class.to_lowercase().contains(&class_lower))
        .collect())
}

/// Calls a method on the GNOME Shell "Window Calls" extension's D-Bus
/// interface and returns the raw reply `Message`, letting each caller
/// deserialize the body it actually expects (a JSON `String` for `List`,
/// nothing for `Activate`/`Minimize`).
///
/// `body` is passed to zbus as-is (not JSON-encoded): `()` produces the
/// empty D-Bus signature (no arguments), and a bare `u32` produces the
/// single-argument signature `"u"`. Wrapping a lone argument in a 1-tuple
/// (`(id,)`) would instead produce the STRUCT signature `"(u)"` - wrong for
/// a plain single-argument D-Bus method call - so callers must pass bare
/// scalars, not tuples, for single arguments.
async fn call_gnome_shell<B>(method: &'static str, body: B) -> Result<zbus::Message, BackendError>
where
    B: serde::Serialize + zbus::zvariant::Type + Send + Sync + 'static,
{
    tokio::task::spawn_blocking(move || -> Result<zbus::Message, BackendError> {
        let connection = zbus::blocking::Connection::session()
            .map_err(|e| BackendError::Unavailable(format!("no session D-Bus: {e}")))?;
        connection
            .call_method(
                Some("org.gnome.Shell"),
                "/org/gnome/Shell/Extensions/Windows",
                Some("org.gnome.Shell.Extensions.Windows"),
                method,
                &body,
            )
            .map_err(|e| {
                BackendError::Unavailable(format!(
                    "Window Calls GNOME Shell extension not available: {e}"
                ))
            })
    })
    .await
    .map_err(|e| BackendError::CommandFailed(e.to_string()))?
}

/// Calls `List` and returns its JSON-string reply body, unparsed.
async fn list_json() -> Result<String, BackendError> {
    let reply = call_gnome_shell("List", ()).await?;
    reply
        .body()
        .deserialize::<String>()
        .map_err(|e| BackendError::CommandFailed(e.to_string()))
}

#[async_trait]
impl WindowBackend for GnomeWindowCallsBackend {
    async fn list_windows(&self, class: &str) -> Result<Vec<WindowId>, BackendError> {
        let json = list_json().await?;
        let windows = parse_window_list(&json, class)?;
        Ok(windows.into_iter().map(|w| w.id.to_string()).collect())
    }

    async fn activate(&self, id: &WindowId) -> Result<(), BackendError> {
        let id: u32 = id
            .parse()
            .map_err(|_| BackendError::CommandFailed(format!("not a Window Calls id: {id}")))?;
        call_gnome_shell("Activate", id).await?;
        Ok(())
    }

    async fn minimize(&self, id: &WindowId) -> Result<(), BackendError> {
        let id: u32 = id
            .parse()
            .map_err(|_| BackendError::CommandFailed(format!("not a Window Calls id: {id}")))?;
        call_gnome_shell("Minimize", id).await?;
        Ok(())
    }

    async fn active_window(&self) -> Result<Option<WindowId>, BackendError> {
        let json = list_json().await?;
        let all: Vec<GnomeWindow> = serde_json::from_str(&json)
            .map_err(|e| BackendError::CommandFailed(format!("bad Window Calls JSON: {e}")))?;
        Ok(all.into_iter().find(|w| w.focus).map(|w| w.id.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"[
        {"id": 1, "wm_class": "firefox", "focus": false},
        {"id": 2, "wm_class": "org.kde.kate", "focus": true},
        {"id": 3, "wm_class": "Firefox", "focus": false}
    ]"#;

    #[test]
    fn filters_by_class_case_insensitively() {
        let matched = parse_window_list(SAMPLE, "firefox").unwrap();
        assert_eq!(matched.len(), 2);
        assert_eq!(matched[0].id, 1);
        assert_eq!(matched[1].id, 3);
    }

    #[test]
    fn no_match_returns_empty() {
        let matched = parse_window_list(SAMPLE, "does-not-exist").unwrap();
        assert!(matched.is_empty());
    }

    #[test]
    fn bad_json_is_a_command_failed_error() {
        let result = parse_window_list("not json", "firefox");
        assert!(matches!(result, Err(BackendError::CommandFailed(_))));
    }
}
