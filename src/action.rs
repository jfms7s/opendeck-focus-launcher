use crate::apps::{AppEntry, launch_app, list_installed_apps};
use crate::backend::{BackendError, WindowBackend, select_backend};
use crate::decision::{Decision, decide};
use async_trait::async_trait;
use openaction::{Action, Instance, OpenActionResult};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct FocusOrLaunchSettings {
    pub app: Option<String>,
    pub class_override: Option<String>,
    #[serde(default = "default_true")]
    pub cycle_windows: bool,
    #[serde(default = "default_true")]
    pub minimize_when_focused: bool,
}

fn default_true() -> bool {
    true
}

/// The class to search for and the AppEntry to launch when there's no window -
/// resolved once per key press from the settings + the current installed-apps
/// list, so `key_up` itself is a thin wrapper around `decide()`.
fn resolve_target(
    settings: &FocusOrLaunchSettings,
    apps: &[AppEntry],
) -> Option<(String, AppEntry)> {
    let app_id = settings.app.as_ref()?;
    let entry = apps.iter().find(|a| &a.id == app_id)?;
    let class = settings
        .class_override
        .clone()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| entry.window_class.clone());
    Some((class, entry.clone()))
}

pub struct FocusOrLaunchAction {
    backend: Box<dyn WindowBackend>,
}

impl FocusOrLaunchAction {
    pub fn new() -> Self {
        let backend = select_backend(
            std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref(),
            std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
            std::env::var("DISPLAY").ok().as_deref(),
        )
        .expect("no supported window backend for this desktop session");
        Self { backend }
    }

    async fn run_for_settings(&self, settings: &FocusOrLaunchSettings) -> OpenActionResult<()> {
        let apps = list_installed_apps();
        let Some((class, entry)) = resolve_target(settings, &apps) else {
            log::warn!("no app selected for this key");
            return Ok(());
        };

        let windows = match self.backend.list_windows(&class).await {
            Ok(w) => w,
            Err(BackendError::Unavailable(msg)) => {
                log::error!("window backend unavailable: {msg}");
                return Ok(());
            }
            Err(BackendError::CommandFailed(msg)) => {
                log::error!("window backend command failed: {msg}");
                return Ok(());
            }
        };

        let active = if windows.is_empty() {
            None
        } else {
            self.backend.active_window().await.ok().flatten()
        };

        match decide(
            &windows,
            active.as_ref(),
            settings.cycle_windows,
            settings.minimize_when_focused,
        ) {
            Decision::Launch => {
                if let Err(e) = launch_app(&entry.exec_hint(), None).await {
                    log::error!("failed to launch {}: {e}", entry.id);
                }
            }
            Decision::Activate(id) => {
                let _ = self.backend.activate(&id).await;
            }
            Decision::Minimize(id) => {
                let _ = self.backend.minimize(&id).await;
            }
            Decision::NoOp => {}
        }
        Ok(())
    }
}

#[async_trait]
impl Action for FocusOrLaunchAction {
    const UUID: &'static str = "com.jfms7s.focuslauncher.focusorlaunch";
    type Settings = FocusOrLaunchSettings;

    async fn will_appear(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        let apps = list_installed_apps();
        let payload = serde_json::json!({ "apps": apps.iter().map(|a| serde_json::json!({"id": a.id, "name": a.name})).collect::<Vec<_>>() });
        instance.send_to_property_inspector(&payload).await?;
        Ok(())
    }

    async fn key_up(
        &self,
        _instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.run_for_settings(settings).await
    }

    async fn dial_up(
        &self,
        _instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.run_for_settings(settings).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, class: &str) -> AppEntry {
        AppEntry {
            id: id.to_string(),
            name: id.to_string(),
            window_class: class.to_string(),
            icon: None,
            exec: format!("{id}-binary"),
        }
    }

    #[test]
    fn resolves_target_using_the_apps_own_window_class() {
        let settings = FocusOrLaunchSettings {
            app: Some("org.mozilla.firefox".to_string()),
            class_override: None,
            cycle_windows: true,
            minimize_when_focused: true,
        };
        let apps = vec![app("org.mozilla.firefox", "firefox")];
        let (class, entry) = resolve_target(&settings, &apps).unwrap();
        assert_eq!(class, "firefox");
        assert_eq!(entry.id, "org.mozilla.firefox");
    }

    #[test]
    fn class_override_wins_when_set() {
        let settings = FocusOrLaunchSettings {
            app: Some("org.mozilla.firefox".to_string()),
            class_override: Some("Navigator".to_string()),
            cycle_windows: true,
            minimize_when_focused: true,
        };
        let apps = vec![app("org.mozilla.firefox", "firefox")];
        let (class, _) = resolve_target(&settings, &apps).unwrap();
        assert_eq!(class, "Navigator");
    }

    #[test]
    fn no_target_when_no_app_selected() {
        let settings = FocusOrLaunchSettings::default();
        let apps = vec![app("org.mozilla.firefox", "firefox")];
        assert!(resolve_target(&settings, &apps).is_none());
    }

    #[test]
    fn no_target_when_selected_app_no_longer_installed() {
        let settings = FocusOrLaunchSettings {
            app: Some("uninstalled.app".to_string()),
            class_override: None,
            cycle_windows: true,
            minimize_when_focused: true,
        };
        let apps = vec![app("org.mozilla.firefox", "firefox")];
        assert!(resolve_target(&settings, &apps).is_none());
    }
}
