use crate::apps::{AppEntry, launch_app, list_installed_apps};
use crate::backend::{BackendError, WindowBackend, WindowId, select_backend};
use crate::decision::{Decision, decide};
use crate::icon::build_image_payload;
use async_trait::async_trait;
use openaction::{Action, Instance, OpenActionResult};
use serde::{Deserialize, Serialize};
use tux_icons::icon_fetcher::IconFetcher;

#[derive(Debug, Serialize, Deserialize)]
pub struct FocusOrLaunchSettings {
    pub app: Option<String>,
    pub class_override: Option<String>,
    #[serde(default = "default_true")]
    pub cycle_windows: bool,
    #[serde(default = "default_true")]
    pub minimize_when_focused: bool,
    /// Overrides the key's title (`instance.set_title`) instead of the app's
    /// own `.desktop` `Name=`.
    pub name_override: Option<String>,
    /// An icon *name* to resolve instead of the app's own icon - looked up
    /// via `tux_icons::IconFetcher::get_icon_path` rather than
    /// `get_icon_path_from_desktop`.
    pub icon_override: Option<String>,
    /// Overrides the launch command instead of the app's own `.desktop`
    /// `Exec=`.
    pub exec_override: Option<String>,
    /// Extra arguments appended when launching - passed straight through as
    /// `apps::launch_app`'s `args` parameter.
    pub custom_args: Option<String>,
    /// When set, holding the key down (past `HOLD_THRESHOLD`) and releasing
    /// it closes every window matching the target app instead of running the
    /// usual focus/launch/cycle/minimize logic.
    #[serde(default)]
    pub close_all_windows_on_hold: bool,
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
            name_override: None,
            icon_override: None,
            exec_override: None,
            custom_args: None,
            close_all_windows_on_hold: false,
        }
    }
}

/// Resolves an optional override field against a fallback: an empty string
/// (as a cleared HTML text input naturally sends) is treated the same as
/// unset, matching `class_override`'s existing behavior.
fn resolve_override(override_value: &Option<String>, fallback: &str) -> String {
    override_value
        .clone()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

/// The command line to launch: `exec_override` if set, else the app's own
/// `Exec=`.
fn resolve_exec(settings: &FocusOrLaunchSettings, entry: &AppEntry) -> String {
    resolve_override(&settings.exec_override, &entry.exec)
}

/// The key's title: `name_override` if set, else the app's own `Name=`.
fn resolve_display_name(settings: &FocusOrLaunchSettings, entry: &AppEntry) -> String {
    resolve_override(&settings.name_override, &entry.name)
}

/// Where to resolve the key's icon from: an explicit icon *name*
/// (`icon_override`), or the selected app's own `.desktop` file.
#[derive(Debug, PartialEq, Eq)]
enum IconSource {
    Named(String),
    FromDesktopFile(std::path::PathBuf),
}

fn resolve_icon_source(settings: &FocusOrLaunchSettings, entry: &AppEntry) -> IconSource {
    match settings.icon_override.clone().filter(|v| !v.is_empty()) {
        Some(name) => IconSource::Named(name),
        None => IconSource::FromDesktopFile(entry.path.clone()),
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
    let class = resolve_override(&settings.class_override, &entry.window_class);
    Some((class, entry.clone()))
}

/// The class to retry the window search with when `class` (the app's own
/// resolved StartupWMClass/id default) matches no windows: the desktop
/// entry's own id, but only when `class` is that unmodified default (not an
/// explicit `class_override` - a user-typed override that finds nothing
/// should stay respected as-is, not silently second-guessed) and the id is
/// actually a different value worth trying. This exists because some apps'
/// real running window class diverges from what their own `.desktop` file
/// declares - e.g. Chrome PWAs report their window's app_id as the desktop
/// file's own id (`chrome-<ext-id>-Default`) under native Wayland, not the
/// `StartupWMClass=crx_<ext-id>` they ship, which is an X11-era convention.
fn id_fallback_class<'a>(class: &str, entry: &'a AppEntry) -> Option<&'a str> {
    if class == entry.window_class && entry.window_class != entry.id {
        Some(&entry.id)
    } else {
        None
    }
}

/// How long a key must be held down before release counts as a hold rather
/// than a regular press. The OpenAction/Stream Deck protocol has no native
/// "long press" event - only separate `key_down`/`key_up` calls - so this is
/// timed by the plugin itself between the two.
const HOLD_THRESHOLD: std::time::Duration = std::time::Duration::from_millis(500);

/// Whether a key_down-to-key_up gap counts as a hold.
fn is_hold(elapsed: std::time::Duration, threshold: std::time::Duration) -> bool {
    elapsed >= threshold
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
    ClosedAll(Vec<WindowId>),
    CloseFailed(Vec<(WindowId, String)>),
}

/// Resolves the target app/class from settings and lists its windows,
/// retrying with the entry id fallback (see `id_fallback_class`) when the
/// primary class search comes up empty. Shared by `orchestrate` and
/// `orchestrate_close_all` - listing the target's windows is identical
/// between "focus or launch" and "close all"; only what happens with the
/// resulting list differs.
async fn resolve_target_windows(
    backend: &dyn WindowBackend,
    settings: &FocusOrLaunchSettings,
    apps: &[AppEntry],
) -> Result<(AppEntry, Vec<WindowId>), RunOutcome> {
    let Some((class, entry)) = resolve_target(settings, apps) else {
        return Err(RunOutcome::NoAppSelected);
    };

    let mut windows = match backend.list_windows(&class).await {
        Ok(w) => w,
        Err(BackendError::Unavailable(msg)) => return Err(RunOutcome::BackendUnavailable(msg)),
        Err(BackendError::CommandFailed(msg)) => return Err(RunOutcome::ListWindowsFailed(msg)),
    };

    if windows.is_empty()
        && let Some(fallback_class) = id_fallback_class(&class, &entry)
        && let Ok(fallback_windows) = backend.list_windows(fallback_class).await
        && !fallback_windows.is_empty()
    {
        windows = fallback_windows;
    }

    Ok((entry, windows))
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
    let (entry, windows) = match resolve_target_windows(backend, settings, apps).await {
        Ok(v) => v,
        Err(outcome) => return outcome,
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
        Decision::Launch => {
            let exec = resolve_exec(settings, &entry);
            match launch_app(&exec, settings.custom_args.as_deref()).await {
                Ok(()) => RunOutcome::Launched,
                Err(e) => RunOutcome::LaunchFailed(e.to_string()),
            }
        }
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

/// Runs a "close all windows" pass for the target app/class from settings,
/// triggered by holding a key down instead of tapping it (see `is_hold`).
/// Closes every matching window rather than picking one to focus/minimize;
/// a failure on one window doesn't stop the rest from being attempted.
async fn orchestrate_close_all(
    backend: &dyn WindowBackend,
    settings: &FocusOrLaunchSettings,
    apps: &[AppEntry],
) -> RunOutcome {
    let (_entry, windows) = match resolve_target_windows(backend, settings, apps).await {
        Ok(v) => v,
        Err(outcome) => return outcome,
    };

    if windows.is_empty() {
        return RunOutcome::NoOp;
    }

    let mut failures = Vec::new();
    for id in &windows {
        if let Err(e) = backend.close(id).await {
            failures.push((id.clone(), e.to_string()));
        }
    }

    if failures.is_empty() {
        RunOutcome::ClosedAll(windows)
    } else {
        RunOutcome::CloseFailed(failures)
    }
}

pub struct FocusOrLaunchAction {
    /// `None` when no supported window backend could be determined for this
    /// desktop session (see `select_backend`) - every call site must check
    /// for that and degrade gracefully (log + `show_alert`) rather than
    /// assume a backend is always present.
    backend: Option<Box<dyn WindowBackend>>,
    /// When each instance's key was last pressed down, so `key_up` can tell
    /// a hold from a regular press (see `is_hold`). Keyed by instance id
    /// rather than held per-`Instance` since the same `FocusOrLaunchAction`
    /// handles every key bound to this action.
    key_down_at: std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
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
        Self {
            backend,
            key_down_at: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Sends the installed-apps list to the property inspector. Shared by
    /// `will_appear` (fires on plugin/device lifecycle, typically before any
    /// PI is open) and `property_inspector_did_appear` (fires when a user
    /// actually opens a key's settings panel - the PI needs the list sent
    /// again at that point, since nobody was listening the first time).
    async fn send_apps_to_pi(&self, instance: &Instance) -> OpenActionResult<()> {
        let apps = list_installed_apps();
        let payload = serde_json::json!({
            "apps": apps
                .iter()
                .map(|a| serde_json::json!({
                    "id": a.id,
                    "name": a.name,
                    "path": a.path.to_string_lossy(),
                    "exec": a.exec,
                }))
                .collect::<Vec<_>>()
        });
        instance.send_to_property_inspector(&payload).await?;
        Ok(())
    }

    /// Sets the key's title and image from the currently selected app plus
    /// any overrides. Called whenever a key might have new settings to show
    /// (appearing, its PI opening, or settings being saved) - if no app is
    /// selected yet, this is a no-op (nothing to show). Icon resolution and
    /// encoding failures are logged and skipped rather than failing the
    /// whole call: a key with the wrong icon is still usable, one that never
    /// gets its title set because of an unrelated icon problem is worse.
    async fn apply_visuals(
        &self,
        instance: &Instance,
        settings: &FocusOrLaunchSettings,
    ) -> OpenActionResult<()> {
        let apps = list_installed_apps();
        let Some(entry) = settings
            .app
            .as_ref()
            .and_then(|id| apps.iter().find(|a| &a.id == id))
        else {
            return Ok(());
        };

        instance
            .set_title(Some(resolve_display_name(settings, entry)), None)
            .await?;

        let icon_path = match resolve_icon_source(settings, entry) {
            IconSource::Named(name) => IconFetcher::new().get_icon_path(name),
            IconSource::FromDesktopFile(path) => {
                IconFetcher::new().get_icon_path_from_desktop(path)
            }
        };
        match icon_path {
            Some(path) => match build_image_payload(&path) {
                Ok(payload) => {
                    instance.set_image(Some(payload), None).await?;
                }
                Err(crate::icon::IconEncodeError::Read(io_err)) => {
                    log::warn!("failed to read icon at {}: {io_err}", path.display());
                }
            },
            None => {
                log::warn!("no icon resolved for {}", entry.id);
            }
        }
        Ok(())
    }

    /// Returns the window backend for this desktop session, or logs +
    /// `show_alert`s and returns `None` if there isn't one - shared by every
    /// call site that needs a backend before it can do anything.
    async fn require_backend(&self, instance: &Instance) -> Option<&dyn WindowBackend> {
        match self.backend.as_deref() {
            Some(backend) => Some(backend),
            None => {
                log::error!(
                    "no supported window backend for this desktop session; taking no action"
                );
                let _ = instance.show_alert().await;
                None
            }
        }
    }

    /// Logs and surfaces (via `show_alert`) the result of an orchestration
    /// pass. Shared by `run_for_settings` and `run_close_all_for_settings` -
    /// only what's dispatched differs between them, not how the outcome is
    /// reported.
    async fn report_outcome(
        &self,
        instance: &Instance,
        outcome: RunOutcome,
    ) -> OpenActionResult<()> {
        match outcome {
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
            RunOutcome::ClosedAll(ids) => {
                log::info!("closed {} window(s)", ids.len());
            }
            RunOutcome::CloseFailed(failures) => {
                for (id, msg) in &failures {
                    log::error!("failed to close window {id}: {msg}");
                }
                let _ = instance.show_alert().await;
            }
        }
        Ok(())
    }

    async fn run_for_settings(
        &self,
        instance: &Instance,
        settings: &FocusOrLaunchSettings,
    ) -> OpenActionResult<()> {
        let Some(backend) = self.require_backend(instance).await else {
            return Ok(());
        };
        let apps = list_installed_apps();
        let outcome = orchestrate(backend, settings, &apps).await;
        self.report_outcome(instance, outcome).await
    }

    /// Same as `run_for_settings`, but closes every window matching the
    /// target app instead of focusing/launching/minimizing - triggered by
    /// holding the key down (see `is_hold`) instead of tapping it.
    async fn run_close_all_for_settings(
        &self,
        instance: &Instance,
        settings: &FocusOrLaunchSettings,
    ) -> OpenActionResult<()> {
        let Some(backend) = self.require_backend(instance).await else {
            return Ok(());
        };
        let apps = list_installed_apps();
        let outcome = orchestrate_close_all(backend, settings, &apps).await;
        self.report_outcome(instance, outcome).await
    }
}

#[async_trait]
impl Action for FocusOrLaunchAction {
    const UUID: &'static str = "com.jfms7s.focuslauncher.focusorlaunch";
    type Settings = FocusOrLaunchSettings;

    async fn will_appear(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.send_apps_to_pi(instance).await?;
        self.apply_visuals(instance, settings).await
    }

    async fn property_inspector_did_appear(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.send_apps_to_pi(instance).await?;
        self.apply_visuals(instance, settings).await
    }

    async fn did_receive_settings(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.apply_visuals(instance, settings).await
    }

    async fn key_down(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.key_down_at
            .lock()
            .unwrap()
            .insert(instance.instance_id.clone(), std::time::Instant::now());
        Ok(())
    }

    async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        let pressed_at = self
            .key_down_at
            .lock()
            .unwrap()
            .remove(&instance.instance_id);
        let held = pressed_at.is_some_and(|at| is_hold(at.elapsed(), HOLD_THRESHOLD));

        if held && settings.close_all_windows_on_hold {
            self.run_close_all_for_settings(instance, settings).await
        } else {
            self.run_for_settings(instance, settings).await
        }
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
            path: std::path::PathBuf::from(format!("/usr/share/applications/{id}.desktop")),
        }
    }

    fn settings(app: &str, cycle: bool, minimize: bool) -> FocusOrLaunchSettings {
        FocusOrLaunchSettings {
            app: Some(app.to_string()),
            class_override: None,
            cycle_windows: cycle,
            minimize_when_focused: minimize,
            name_override: None,
            icon_override: None,
            exec_override: None,
            custom_args: None,
            close_all_windows_on_hold: false,
        }
    }

    #[test]
    fn exec_override_wins_when_set() {
        let mut settings = settings("org.mozilla.firefox", true, true);
        settings.exec_override = Some("firefox --private-window".to_string());
        let entry = app("org.mozilla.firefox", "firefox");

        assert_eq!(resolve_exec(&settings, &entry), "firefox --private-window");
    }

    #[test]
    fn exec_falls_back_to_the_apps_own_exec_when_no_override() {
        let settings = settings("org.mozilla.firefox", true, true);
        let entry = app("org.mozilla.firefox", "firefox");

        assert_eq!(resolve_exec(&settings, &entry), entry.exec);
    }

    #[test]
    fn name_override_wins_when_set() {
        let mut settings = settings("org.mozilla.firefox", true, true);
        settings.name_override = Some("Browser".to_string());
        let entry = app("org.mozilla.firefox", "firefox");

        assert_eq!(resolve_display_name(&settings, &entry), "Browser");
    }

    #[test]
    fn name_falls_back_to_the_apps_own_name_when_no_override() {
        let settings = settings("org.mozilla.firefox", true, true);
        let entry = app("org.mozilla.firefox", "firefox");

        assert_eq!(resolve_display_name(&settings, &entry), entry.name);
    }

    #[test]
    fn icon_override_resolves_by_name_not_the_desktop_file() {
        let mut settings = settings("org.mozilla.firefox", true, true);
        settings.icon_override = Some("firefox-nightly".to_string());
        let entry = app("org.mozilla.firefox", "firefox");

        assert_eq!(
            resolve_icon_source(&settings, &entry),
            IconSource::Named("firefox-nightly".to_string())
        );
    }

    #[test]
    fn icon_falls_back_to_the_apps_own_desktop_file_when_no_override() {
        let settings = settings("org.mozilla.firefox", true, true);
        let entry = app("org.mozilla.firefox", "firefox");

        assert_eq!(
            resolve_icon_source(&settings, &entry),
            IconSource::FromDesktopFile(entry.path.clone())
        );
    }

    #[test]
    fn empty_string_overrides_are_treated_as_unset() {
        let mut settings = settings("org.mozilla.firefox", true, true);
        settings.name_override = Some(String::new());
        settings.icon_override = Some(String::new());
        settings.exec_override = Some(String::new());
        let entry = app("org.mozilla.firefox", "firefox");

        assert_eq!(resolve_display_name(&settings, &entry), entry.name);
        assert_eq!(resolve_exec(&settings, &entry), entry.exec);
        assert_eq!(
            resolve_icon_source(&settings, &entry),
            IconSource::FromDesktopFile(entry.path.clone())
        );
    }

    #[test]
    fn resolves_target_using_the_apps_own_window_class() {
        let settings = settings("org.mozilla.firefox", true, true);
        let apps = vec![app("org.mozilla.firefox", "firefox")];
        let (class, entry) = resolve_target(&settings, &apps).unwrap();
        assert_eq!(class, "firefox");
        assert_eq!(entry.id, "org.mozilla.firefox");
    }

    #[test]
    fn class_override_wins_when_set() {
        let mut settings = settings("org.mozilla.firefox", true, true);
        settings.class_override = Some("Navigator".to_string());
        let apps = vec![app("org.mozilla.firefox", "firefox")];
        let (class, _) = resolve_target(&settings, &apps).unwrap();
        assert_eq!(class, "Navigator");
    }

    #[test]
    fn is_hold_true_when_elapsed_meets_threshold() {
        assert!(is_hold(
            std::time::Duration::from_millis(500),
            std::time::Duration::from_millis(500)
        ));
    }

    #[test]
    fn is_hold_true_when_elapsed_exceeds_threshold() {
        assert!(is_hold(
            std::time::Duration::from_millis(800),
            std::time::Duration::from_millis(500)
        ));
    }

    #[test]
    fn is_hold_false_when_elapsed_is_below_threshold() {
        assert!(!is_hold(
            std::time::Duration::from_millis(200),
            std::time::Duration::from_millis(500)
        ));
    }

    #[test]
    fn no_target_when_no_app_selected() {
        let settings = FocusOrLaunchSettings::default();
        let apps = vec![app("org.mozilla.firefox", "firefox")];
        assert!(resolve_target(&settings, &apps).is_none());
    }

    #[test]
    fn no_target_when_selected_app_no_longer_installed() {
        let settings = settings("uninstalled.app", true, true);
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
        /// When set, `list_windows` looks the queried class up here instead
        /// of returning `windows` unconditionally - lets a test give
        /// different classes different results, e.g. to simulate the id
        /// fallback finding windows that the primary class search didn't.
        windows_by_class: Option<std::collections::HashMap<String, Vec<WindowId>>>,
        active: Option<WindowId>,
        list_windows_err: Option<BackendError>,
        activate_calls: Mutex<Vec<WindowId>>,
        minimize_calls: Mutex<Vec<WindowId>>,
        close_calls: Mutex<Vec<WindowId>>,
        /// Window ids that `close` should fail for, e.g. to simulate one
        /// window in a close-all batch refusing to close.
        close_fails_for: Vec<WindowId>,
    }

    #[async_trait]
    impl WindowBackend for RecordingFakeBackend {
        async fn list_windows(&self, class: &str) -> Result<Vec<WindowId>, BackendError> {
            if let Some(err) = &self.list_windows_err {
                return Err(match err {
                    BackendError::Unavailable(m) => BackendError::Unavailable(m.clone()),
                    BackendError::CommandFailed(m) => BackendError::CommandFailed(m.clone()),
                });
            }
            match &self.windows_by_class {
                Some(map) => Ok(map.get(class).cloned().unwrap_or_default()),
                None => Ok(self.windows.clone()),
            }
        }

        async fn activate(&self, id: &WindowId) -> Result<(), BackendError> {
            self.activate_calls.lock().unwrap().push(id.clone());
            Ok(())
        }

        async fn minimize(&self, id: &WindowId) -> Result<(), BackendError> {
            self.minimize_calls.lock().unwrap().push(id.clone());
            Ok(())
        }

        async fn close(&self, id: &WindowId) -> Result<(), BackendError> {
            self.close_calls.lock().unwrap().push(id.clone());
            if self.close_fails_for.contains(id) {
                return Err(BackendError::CommandFailed(format!("could not close {id}")));
            }
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

    #[test]
    fn id_fallback_offered_when_default_class_differs_from_entry_id() {
        let entry = app("chrome-abc-Default", "crx_abc");
        assert_eq!(
            id_fallback_class("crx_abc", &entry),
            Some("chrome-abc-Default")
        );
    }

    #[test]
    fn id_fallback_not_offered_when_class_and_id_already_match() {
        // No StartupWMClass case (e.g. the Plex snap): window_class already
        // equals id, so there's no distinct second value worth trying.
        let entry = app("plex-desktop_plex-desktop", "plex-desktop_plex-desktop");
        assert_eq!(id_fallback_class("plex-desktop_plex-desktop", &entry), None);
    }

    #[test]
    fn id_fallback_not_offered_when_class_was_overridden() {
        // `class` here no longer equals `entry.window_class`, meaning a
        // `class_override` won out in `resolve_target` - an explicit user
        // override that finds nothing should stay respected, not
        // second-guessed with the entry id.
        let entry = app("chrome-abc-Default", "crx_abc");
        assert_eq!(id_fallback_class("Navigator", &entry), None);
    }

    #[tokio::test]
    async fn orchestrate_falls_back_to_entry_id_when_startup_wm_class_matches_nothing() {
        let mut windows_by_class = std::collections::HashMap::new();
        windows_by_class.insert("crx_abc".to_string(), vec![]);
        windows_by_class.insert("chrome-abc-Default".to_string(), vec!["w1".to_string()]);
        let backend = RecordingFakeBackend {
            windows_by_class: Some(windows_by_class),
            ..Default::default()
        };
        let apps = vec![app("chrome-abc-Default", "crx_abc")];
        let settings = settings("chrome-abc-Default", true, true);

        let outcome = orchestrate(&backend, &settings, &apps).await;

        assert_eq!(outcome, RunOutcome::Activated("w1".to_string()));
    }

    #[tokio::test]
    async fn orchestrate_launches_when_neither_class_nor_id_fallback_matches() {
        let backend = RecordingFakeBackend {
            windows_by_class: Some(std::collections::HashMap::new()),
            ..Default::default()
        };
        let apps = vec![app("chrome-abc-Default", "crx_abc")];
        let settings = settings("chrome-abc-Default", true, true);

        let outcome = orchestrate(&backend, &settings, &apps).await;

        assert_eq!(outcome, RunOutcome::Launched);
    }

    #[tokio::test]
    async fn orchestrate_does_not_fall_back_when_class_was_explicitly_overridden() {
        let mut windows_by_class = std::collections::HashMap::new();
        // The override matches nothing, but the entry id would - the
        // fallback must not kick in and silently ignore the user's override.
        windows_by_class.insert("chrome-abc-Default".to_string(), vec!["w1".to_string()]);
        let backend = RecordingFakeBackend {
            windows_by_class: Some(windows_by_class),
            ..Default::default()
        };
        let apps = vec![app("chrome-abc-Default", "crx_abc")];
        let mut settings = settings("chrome-abc-Default", true, true);
        settings.class_override = Some("some-typo".to_string());

        let outcome = orchestrate(&backend, &settings, &apps).await;

        assert_eq!(outcome, RunOutcome::Launched);
    }

    #[tokio::test]
    async fn orchestrate_reports_no_app_selected() {
        let backend = RecordingFakeBackend::default();
        let apps = vec![app("org.mozilla.firefox", "firefox")];
        let settings = FocusOrLaunchSettings::default();

        let outcome = orchestrate(&backend, &settings, &apps).await;

        assert_eq!(outcome, RunOutcome::NoAppSelected);
    }

    #[tokio::test]
    async fn orchestrate_close_all_closes_every_matching_window() {
        let backend = RecordingFakeBackend {
            windows: vec!["w1".to_string(), "w2".to_string()],
            ..Default::default()
        };
        let apps = vec![app("org.mozilla.firefox", "firefox")];
        let settings = settings("org.mozilla.firefox", true, true);

        let outcome = orchestrate_close_all(&backend, &settings, &apps).await;

        assert_eq!(
            outcome,
            RunOutcome::ClosedAll(vec!["w1".to_string(), "w2".to_string()])
        );
        assert_eq!(
            *backend.close_calls.lock().unwrap(),
            vec!["w1".to_string(), "w2".to_string()]
        );
    }

    #[tokio::test]
    async fn orchestrate_close_all_is_a_no_op_when_nothing_is_open() {
        let backend = RecordingFakeBackend::default();
        let apps = vec![app("org.mozilla.firefox", "firefox")];
        let settings = settings("org.mozilla.firefox", true, true);

        let outcome = orchestrate_close_all(&backend, &settings, &apps).await;

        assert_eq!(outcome, RunOutcome::NoOp);
        assert!(backend.close_calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn orchestrate_close_all_reports_no_app_selected() {
        let backend = RecordingFakeBackend::default();
        let apps = vec![app("org.mozilla.firefox", "firefox")];
        let settings = FocusOrLaunchSettings::default();

        let outcome = orchestrate_close_all(&backend, &settings, &apps).await;

        assert_eq!(outcome, RunOutcome::NoAppSelected);
    }

    #[tokio::test]
    async fn orchestrate_close_all_reports_windows_that_failed_to_close() {
        let backend = RecordingFakeBackend {
            windows: vec!["w1".to_string(), "w2".to_string()],
            close_fails_for: vec!["w2".to_string()],
            ..Default::default()
        };
        let apps = vec![app("org.mozilla.firefox", "firefox")];
        let settings = settings("org.mozilla.firefox", true, true);

        let outcome = orchestrate_close_all(&backend, &settings, &apps).await;

        assert_eq!(
            outcome,
            RunOutcome::CloseFailed(vec![(
                "w2".to_string(),
                "backend command failed: could not close w2".to_string()
            )])
        );
        // Both windows are still attempted, even after one failure.
        assert_eq!(
            *backend.close_calls.lock().unwrap(),
            vec!["w1".to_string(), "w2".to_string()]
        );
    }

    #[tokio::test]
    async fn orchestrate_close_all_reports_backend_unavailable() {
        let backend = RecordingFakeBackend {
            list_windows_err: Some(BackendError::Unavailable("no wmctrl".to_string())),
            ..Default::default()
        };
        let apps = vec![app("org.mozilla.firefox", "firefox")];
        let settings = settings("org.mozilla.firefox", true, true);

        let outcome = orchestrate_close_all(&backend, &settings, &apps).await;

        assert_eq!(
            outcome,
            RunOutcome::BackendUnavailable("no wmctrl".to_string())
        );
    }

    #[tokio::test]
    async fn orchestrate_close_all_falls_back_to_entry_id_when_startup_wm_class_matches_nothing() {
        let mut windows_by_class = std::collections::HashMap::new();
        windows_by_class.insert("crx_abc".to_string(), vec![]);
        windows_by_class.insert("chrome-abc-Default".to_string(), vec!["w1".to_string()]);
        let backend = RecordingFakeBackend {
            windows_by_class: Some(windows_by_class),
            ..Default::default()
        };
        let apps = vec![app("chrome-abc-Default", "crx_abc")];
        let settings = settings("chrome-abc-Default", true, true);

        let outcome = orchestrate_close_all(&backend, &settings, &apps).await;

        assert_eq!(outcome, RunOutcome::ClosedAll(vec!["w1".to_string()]));
    }
}
