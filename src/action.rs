use crate::apps::{AppEntry, launch_app, list_installed_apps};
use crate::backend::{BackendError, WindowBackend, WindowId, select_backend};
use crate::decision::{Decision, decide};
use async_trait::async_trait;
use openaction::{Action, Instance, OpenActionResult};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
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

// `openaction`'s settings-deserialization fallback (`ActionWrapper::deserialize_settings`)
// falls back to `Default::default()` whenever the settings JSON fails to
// deserialize at all - not just when fields are missing. A derived `Default`
// would give `cycle_windows: false, minimize_when_focused: false`, the
// opposite of the documented default (both on). Hand-write it so it matches
// the same `default_true()` the field-level `serde(default = ...)` uses.
impl Default for FocusOrLaunchSettings {
    fn default() -> Self {
        Self {
            app: None,
            class_override: None,
            cycle_windows: default_true(),
            minimize_when_focused: default_true(),
        }
    }
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

/// The outcome of one orchestration pass (list windows -> decide -> dispatch),
/// kept separate from any `Instance`/logging side effects so it can be tested
/// against a fake `WindowBackend` without a live OpenDeck connection.
#[derive(Debug, PartialEq, Eq)]
enum RunOutcome {
    NoAppSelected,
    BackendUnavailable(String),
    ListWindowsFailed(String),
    Launched,
    LaunchFailed(String),
    Activated(WindowId),
    ActivateFailed(WindowId, String),
    Minimized(WindowId),
    MinimizeFailed(WindowId, String),
    NoOp,
}

/// Runs one full orchestration pass against the given backend: resolve the
/// target app/class from settings, list its windows, decide what to do, and
/// dispatch. Takes the backend as a plain `&dyn WindowBackend` (rather than
/// `&self`) so tests can pass a fake backend without constructing a whole
/// `FocusOrLaunchAction`.
async fn orchestrate(
    backend: &dyn WindowBackend,
    settings: &FocusOrLaunchSettings,
    apps: &[AppEntry],
) -> RunOutcome {
    let Some((class, entry)) = resolve_target(settings, apps) else {
        return RunOutcome::NoAppSelected;
    };

    let windows = match backend.list_windows(&class).await {
        Ok(w) => w,
        Err(BackendError::Unavailable(msg)) => return RunOutcome::BackendUnavailable(msg),
        Err(BackendError::CommandFailed(msg)) => return RunOutcome::ListWindowsFailed(msg),
    };

    let active = if windows.is_empty() {
        None
    } else {
        backend.active_window().await.ok().flatten()
    };

    match decide(
        &windows,
        active.as_ref(),
        settings.cycle_windows,
        settings.minimize_when_focused,
    ) {
        Decision::Launch => match launch_app(&entry.exec, None).await {
            Ok(()) => RunOutcome::Launched,
            Err(e) => RunOutcome::LaunchFailed(e.to_string()),
        },
        Decision::Activate(id) => match backend.activate(&id).await {
            Ok(()) => RunOutcome::Activated(id),
            Err(e) => RunOutcome::ActivateFailed(id, e.to_string()),
        },
        Decision::Minimize(id) => match backend.minimize(&id).await {
            Ok(()) => RunOutcome::Minimized(id),
            Err(e) => RunOutcome::MinimizeFailed(id, e.to_string()),
        },
        Decision::NoOp => RunOutcome::NoOp,
    }
}

pub struct FocusOrLaunchAction {
    /// `None` when no supported window backend could be determined for this
    /// desktop session (see `select_backend`) - every call site must check
    /// for that and degrade gracefully (log + `show_alert`) rather than
    /// assume a backend is always present.
    backend: Option<Box<dyn WindowBackend>>,
}

impl FocusOrLaunchAction {
    pub fn new() -> Self {
        let backend = select_backend(
            std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref(),
            std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
            std::env::var("DISPLAY").ok().as_deref(),
        );
        if backend.is_none() {
            log::error!(
                "no supported window backend for this desktop session; \
                 Focus or Launch keys will do nothing until this is resolved"
            );
        }
        Self { backend }
    }

    /// Sends the installed-apps list to the property inspector. Shared by
    /// `will_appear` (fires on plugin/device lifecycle, typically before any
    /// PI is open) and `property_inspector_did_appear` (fires when a user
    /// actually opens a key's settings panel - the PI needs the list sent
    /// again at that point, since nobody was listening the first time).
    async fn send_apps_to_pi(&self, instance: &Instance) -> OpenActionResult<()> {
        let apps = list_installed_apps();
        let payload = serde_json::json!({ "apps": apps.iter().map(|a| serde_json::json!({"id": a.id, "name": a.name})).collect::<Vec<_>>() });
        instance.send_to_property_inspector(&payload).await?;
        Ok(())
    }

    async fn run_for_settings(
        &self,
        instance: &Instance,
        settings: &FocusOrLaunchSettings,
    ) -> OpenActionResult<()> {
        let Some(backend) = self.backend.as_deref() else {
            log::error!("no supported window backend for this desktop session; taking no action");
            let _ = instance.show_alert().await;
            return Ok(());
        };

        let apps = list_installed_apps();
        match orchestrate(backend, settings, &apps).await {
            RunOutcome::NoAppSelected => {
                log::warn!("no app selected for this key");
            }
            RunOutcome::BackendUnavailable(msg) => {
                log::error!("window backend unavailable: {msg}");
                let _ = instance.show_alert().await;
            }
            RunOutcome::ListWindowsFailed(msg) => {
                log::error!("window backend command failed: {msg}");
            }
            RunOutcome::Launched => {}
            RunOutcome::LaunchFailed(msg) => {
                log::error!("failed to launch app: {msg}");
                let _ = instance.show_alert().await;
            }
            RunOutcome::Activated(_) => {}
            RunOutcome::ActivateFailed(id, msg) => {
                log::error!("failed to activate window {id}: {msg}");
                let _ = instance.show_alert().await;
            }
            RunOutcome::Minimized(_) => {}
            RunOutcome::MinimizeFailed(id, msg) => {
                log::error!("failed to minimize window {id}: {msg}");
                let _ = instance.show_alert().await;
            }
            RunOutcome::NoOp => {}
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
        self.send_apps_to_pi(instance).await
    }

    async fn property_inspector_did_appear(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.send_apps_to_pi(instance).await
    }

    async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        self.run_for_settings(instance, settings).await
    }

    async fn dial_up(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.run_for_settings(instance, settings).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn app(id: &str, class: &str) -> AppEntry {
        AppEntry {
            id: id.to_string(),
            name: id.to_string(),
            window_class: class.to_string(),
            exec: format!("{id}-binary"),
        }
    }

    fn settings(app: &str, cycle: bool, minimize: bool) -> FocusOrLaunchSettings {
        FocusOrLaunchSettings {
            app: Some(app.to_string()),
            class_override: None,
            cycle_windows: cycle,
            minimize_when_focused: minimize,
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

    #[test]
    fn default_matches_missing_key_deserialization() {
        // `openaction` falls back to `Default::default()` when settings JSON
        // fails to deserialize at all, not just on missing fields. That must
        // land on the same values as deserializing `{}` (missing keys, which
        // go through `serde(default = "default_true")`).
        let from_missing_keys: FocusOrLaunchSettings = serde_json::from_str("{}").unwrap();
        let from_default = FocusOrLaunchSettings::default();
        assert_eq!(from_missing_keys.cycle_windows, from_default.cycle_windows);
        assert_eq!(
            from_missing_keys.minimize_when_focused,
            from_default.minimize_when_focused
        );
        assert!(from_default.cycle_windows);
        assert!(from_default.minimize_when_focused);
    }

    /// Records every call made to it and returns pre-configured canned
    /// responses, so orchestration tests can assert both the outcome and
    /// exactly what was dispatched to the backend.
    #[derive(Default)]
    struct RecordingFakeBackend {
        windows: Vec<WindowId>,
        active: Option<WindowId>,
        list_windows_err: Option<BackendError>,
        activate_calls: Mutex<Vec<WindowId>>,
        minimize_calls: Mutex<Vec<WindowId>>,
    }

    #[async_trait]
    impl WindowBackend for RecordingFakeBackend {
        async fn list_windows(&self, _class: &str) -> Result<Vec<WindowId>, BackendError> {
            if let Some(err) = &self.list_windows_err {
                return Err(match err {
                    BackendError::Unavailable(m) => BackendError::Unavailable(m.clone()),
                    BackendError::CommandFailed(m) => BackendError::CommandFailed(m.clone()),
                });
            }
            Ok(self.windows.clone())
        }

        async fn activate(&self, id: &WindowId) -> Result<(), BackendError> {
            self.activate_calls.lock().unwrap().push(id.clone());
            Ok(())
        }

        async fn minimize(&self, id: &WindowId) -> Result<(), BackendError> {
            self.minimize_calls.lock().unwrap().push(id.clone());
            Ok(())
        }

        async fn active_window(&self) -> Result<Option<WindowId>, BackendError> {
            Ok(self.active.clone())
        }
    }

    #[tokio::test]
    async fn orchestrate_launches_when_no_window_is_open() {
        let backend = RecordingFakeBackend::default();
        let apps = vec![app("org.mozilla.firefox", "firefox")];
        let settings = settings("org.mozilla.firefox", true, true);

        let outcome = orchestrate(&backend, &settings, &apps).await;

        assert_eq!(outcome, RunOutcome::Launched);
        assert!(backend.activate_calls.lock().unwrap().is_empty());
        assert!(backend.minimize_calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn orchestrate_activates_a_background_window() {
        let backend = RecordingFakeBackend {
            windows: vec!["w1".to_string()],
            active: None,
            ..Default::default()
        };
        let apps = vec![app("org.mozilla.firefox", "firefox")];
        let settings = settings("org.mozilla.firefox", true, true);

        let outcome = orchestrate(&backend, &settings, &apps).await;

        assert_eq!(outcome, RunOutcome::Activated("w1".to_string()));
        assert_eq!(
            *backend.activate_calls.lock().unwrap(),
            vec!["w1".to_string()]
        );
    }

    #[tokio::test]
    async fn orchestrate_cycles_to_next_window_when_focused_with_several_open() {
        let backend = RecordingFakeBackend {
            windows: vec!["w1".to_string(), "w2".to_string()],
            active: Some("w1".to_string()),
            ..Default::default()
        };
        let apps = vec![app("org.mozilla.firefox", "firefox")];
        let settings = settings("org.mozilla.firefox", true, true);

        let outcome = orchestrate(&backend, &settings, &apps).await;

        assert_eq!(outcome, RunOutcome::Activated("w2".to_string()));
        assert_eq!(
            *backend.activate_calls.lock().unwrap(),
            vec!["w2".to_string()]
        );
    }

    #[tokio::test]
    async fn orchestrate_minimizes_a_focused_lone_window() {
        let backend = RecordingFakeBackend {
            windows: vec!["w1".to_string()],
            active: Some("w1".to_string()),
            ..Default::default()
        };
        let apps = vec![app("org.mozilla.firefox", "firefox")];
        let settings = settings("org.mozilla.firefox", true, true);

        let outcome = orchestrate(&backend, &settings, &apps).await;

        assert_eq!(outcome, RunOutcome::Minimized("w1".to_string()));
        assert_eq!(
            *backend.minimize_calls.lock().unwrap(),
            vec!["w1".to_string()]
        );
    }

    #[tokio::test]
    async fn orchestrate_reports_backend_unavailable_without_launching() {
        let backend = RecordingFakeBackend {
            list_windows_err: Some(BackendError::Unavailable("no wmctrl".to_string())),
            ..Default::default()
        };
        let apps = vec![app("org.mozilla.firefox", "firefox")];
        let settings = settings("org.mozilla.firefox", true, true);

        let outcome = orchestrate(&backend, &settings, &apps).await;

        assert_eq!(
            outcome,
            RunOutcome::BackendUnavailable("no wmctrl".to_string())
        );
        assert!(backend.activate_calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn orchestrate_reports_no_app_selected() {
        let backend = RecordingFakeBackend::default();
        let apps = vec![app("org.mozilla.firefox", "firefox")];
        let settings = FocusOrLaunchSettings::default();

        let outcome = orchestrate(&backend, &settings, &apps).await;

        assert_eq!(outcome, RunOutcome::NoAppSelected);
    }
}
