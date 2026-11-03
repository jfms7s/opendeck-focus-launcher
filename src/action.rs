//! The `OpenDeck` adapter: maps key/dial events to a gesture and intent, runs
//! `orchestrate::run`, and shows the result on the key. All decisions live
//! in `decision`, `orchestrate` and `settings`; this file only wires events.

use crate::apps::{AppEntry, Launcher, SystemLauncher};
use crate::backend::{WindowBackend, WindowClass, WindowId};
use crate::badge;
use crate::catalog::AppCatalog;
use crate::decision::{Gesture, HOLD_THRESHOLD, Intent, intent_for, is_hold};
use crate::icon::IconCache;
use crate::orchestrate::{
    self, Context, DialPreview, LaunchGuard, RunOutcome, Severity, report_for,
};
use crate::settings::{
    FocusOrLaunchSettings, find_app, id_fallback_class, resolve_class, resolve_display_name,
    resolve_icon,
};
use crate::watch::Watcher;
use async_trait::async_trait;
use openaction::{Action, Instance, OpenActionResult};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// One entry of the apps list sent to the property inspector.
#[derive(Debug, Serialize)]
struct PiApp<'a> {
    id: &'a str,
    name: &'a str,
    path: std::borrow::Cow<'a, str>,
    exec: &'a str,
    window_class: &'a str,
}

impl<'a> From<&'a AppEntry> for PiApp<'a> {
    fn from(app: &'a AppEntry) -> Self {
        Self {
            id: &app.id,
            name: &app.name,
            path: app.path.to_string_lossy(),
            exec: &app.exec,
            window_class: &app.window_class,
        }
    }
}

#[derive(Debug, Serialize)]
struct PiPayload<'a> {
    apps: Vec<PiApp<'a>>,
}

/// The second message to the property inspector: how many windows a press
/// would see now. Sent after the apps list so a slow backend never holds up
/// the app dropdown. `open_windows: None` with a note means "unknown, and
/// why"; both `None` means there is nothing to show (no app selected).
#[derive(Debug, PartialEq, Eq, Serialize)]
struct PiWindowCount {
    open_windows: Option<usize>,
    windows_note: Option<String>,
    /// Only when the PI opens: the classes of every open window, for the
    /// "pick from open windows" list. Absent on a settings change.
    #[serde(skip_serializing_if = "Option::is_none")]
    window_classes: Option<Vec<PiClass>>,
}

/// One entry of the open-windows class list. `valid: false` marks a class
/// `WindowClass::parse` rejects: the PI shows it but can't pick it.
#[derive(Debug, PartialEq, Eq, Serialize)]
struct PiClass {
    class: String,
    valid: bool,
}

/// The picker list: each distinct class once (exact duplicates dropped),
/// sorted ignoring case, validated as a key's class would be.
fn pi_classes(mut classes: Vec<String>) -> Vec<PiClass> {
    classes.sort_by(|a, b| {
        a.to_lowercase()
            .cmp(&b.to_lowercase())
            .then_with(|| a.cmp(b))
    });
    classes.dedup();
    classes
        .into_iter()
        .map(|class| PiClass {
            valid: WindowClass::parse(&class).is_ok(),
            class,
        })
        .collect()
}

/// The count message for a press's would-be result: `None` when no window
/// backend fits this desktop.
fn pi_window_count(count: Option<Result<usize, RunOutcome>>) -> PiWindowCount {
    let (open_windows, windows_note) = match count {
        None => (None, Some(report_for(&RunOutcome::NoBackend).message)),
        Some(Ok(n)) => (Some(n), None),
        Some(Err(RunOutcome::NoAppSelected)) => (None, None),
        Some(Err(outcome)) => (None, Some(report_for(&outcome).message)),
    };
    PiWindowCount {
        open_windows,
        windows_note,
        window_classes: None,
    }
}

/// The intent a key or dial release asks for: a hold (pressed for at least
/// `HOLD_THRESHOLD`) uses the key's hold setting, anything else is a tap. A
/// release with no recorded press (e.g. the press predates the plugin) is a
/// tap. Takes the settings so `press_released` has no decision of its own.
fn release_intent(
    pressed_at: Option<Instant>,
    now: Instant,
    settings: &FocusOrLaunchSettings,
) -> Intent {
    let gesture = if pressed_at
        .is_some_and(|at| is_hold(now.saturating_duration_since(at), HOLD_THRESHOLD))
    {
        Gesture::Hold
    } else {
        Gesture::Tap
    };
    intent_for(gesture, settings.hold_action())
}

pub struct FocusOrLaunchAction {
    /// `None` when no backend fits this desktop session (see
    /// `select_backend`); every press then alerts instead of guessing.
    backend: Option<Arc<dyn WindowBackend>>,
    /// Keeps visible keys' running state current; `None` without a backend
    /// and on macOS (frozen: live state ships Linux-only).
    watcher: Option<Watcher>,
    launcher: Box<dyn Launcher>,
    catalog: AppCatalog,
    icons: IconCache,
    launches: LaunchGuard,
    /// When each instance's key or dial was last pressed, so the release can
    /// tell a hold from a tap. One action object serves every key.
    pressed_at: Mutex<HashMap<String, Instant>>,
    /// Instances whose property inspector is open, so a settings change
    /// re-sends the window count only to a PI that can show it.
    pi_open: Mutex<HashSet<String>>,
    /// Dials whose title shows a rotation preview, with the window shown
    /// (`None`: "no windows"). The next press acts on it and restores the
    /// key's face.
    dial_cursors: Mutex<HashMap<String, Option<WindowId>>>,
}

impl FocusOrLaunchAction {
    pub fn new(backend: Option<Box<dyn WindowBackend>>) -> Self {
        let backend: Option<Arc<dyn WindowBackend>> = backend.map(Arc::from);
        let watcher = if cfg!(target_os = "macos") {
            None
        } else {
            backend.as_ref().map(|backend| {
                Watcher::new(Arc::clone(backend), Arc::new(crate::watch::DeckPainter))
            })
        };
        Self {
            backend,
            watcher,
            launcher: Box::new(SystemLauncher),
            catalog: AppCatalog::system(),
            icons: IconCache::new(),
            launches: LaunchGuard::default(),
            pressed_at: Mutex::new(HashMap::new()),
            pi_open: Mutex::new(HashSet::new()),
            dial_cursors: Mutex::new(HashMap::new()),
        }
    }

    fn dial_cursors(&self) -> std::sync::MutexGuard<'_, HashMap<String, Option<WindowId>>> {
        self.dial_cursors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn pi_open(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.pi_open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Sends the property inspector how many windows a press would see now
    /// and, with `with_classes` (the PI just opened), every open window's
    /// class for the picker.
    async fn send_window_count_to_pi(
        &self,
        instance: &Instance,
        settings: &FocusOrLaunchSettings,
        apps: &[AppEntry],
        with_classes: bool,
    ) -> OpenActionResult<()> {
        let count = match self.backend.as_deref() {
            Some(backend) => {
                Some(orchestrate::matching_window_count(backend, settings, apps).await)
            }
            None => None,
        };
        let mut message = pi_window_count(count);
        if with_classes && let Some(backend) = self.backend.as_deref() {
            let classes = backend.open_window_classes().await.unwrap_or_else(|e| {
                log::warn!("could not list the open windows' classes: {e}");
                Vec::new()
            });
            message.window_classes = Some(pi_classes(classes));
        }
        instance.send_to_property_inspector(&message).await
    }

    fn pressed_at(&self) -> std::sync::MutexGuard<'_, HashMap<String, Instant>> {
        self.pressed_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Sends the installed-apps list to the property inspector. Only called
    /// when a PI is actually open.
    async fn send_apps_to_pi(
        &self,
        instance: &Instance,
        apps: &[AppEntry],
    ) -> OpenActionResult<()> {
        let payload = PiPayload {
            apps: apps.iter().map(PiApp::from).collect(),
        };
        instance.send_to_property_inspector(&payload).await
    }

    /// Sets the key's title and image from the selected app plus overrides.
    /// Whatever can't be shown falls back to OpenDeck's default (the
    /// manifest's image, no title), so a key never keeps the face of the app
    /// it showed before. A failed icon lookup is logged once, by the cache.
    async fn apply_visuals(
        &self,
        instance: &Instance,
        settings: &FocusOrLaunchSettings,
        apps: &[AppEntry],
    ) -> OpenActionResult<()> {
        let face = key_face(settings, apps);
        if face.title.is_none()
            && let Some(id) = settings.app_id()
        {
            log::warn!("the app selected for a key ({id:?}) is not installed");
        }
        instance.set_title(face.title, None).await?;
        let uri = match face.icon {
            Some(icon) => self.icons.data_uri(icon).await,
            None => {
                if let Some(entry) = find_app(settings, apps) {
                    log::debug!("{} has no icon", entry.id);
                }
                None
            }
        };
        // Held until the key is re-tracked or untracked, so no state paint
        // from a tick already under way lands on top of the plain icon.
        let _painting = match &self.watcher {
            Some(watcher) => Some(watcher.paint_guard().await),
            None => None,
        };
        match &uri {
            Some(uri) => instance.set_image(Some(uri.as_ref()), None).await?,
            None => instance.set_image(None::<String>, None).await?,
        }
        // After the plain icon is painted, so the poller's state paint
        // comes after it, never before.
        self.follow(&instance.instance_id, settings, apps, uri);
        Ok(())
    }

    /// Tracks the key for live state when it shows one (see `watched`),
    /// else stops tracking it.
    fn follow(
        &self,
        instance_id: &str,
        settings: &FocusOrLaunchSettings,
        apps: &[AppEntry],
        icon: Option<Arc<str>>,
    ) {
        let Some(watcher) = &self.watcher else {
            return;
        };
        match watched(settings, apps, icon) {
            Some((classes, icon)) => watcher.track(instance_id, classes, icon),
            None => watcher.untrack(instance_id),
        }
    }

    async fn report(&self, instance: &Instance, outcome: &RunOutcome) {
        let report = report_for(outcome);
        match report.severity {
            Severity::Info => log::info!("{}", report.message),
            Severity::Warn => log::warn!("{}", report.message),
            Severity::Error => log::error!("{}", report.message),
        }
        if report.alert
            && let Err(e) = instance.show_alert().await
        {
            log::warn!("could not show the alert on the key: {e}");
        }
    }

    fn press_started(&self, instance: &Instance) {
        self.pressed_at()
            .insert(instance.instance_id.clone(), Instant::now());
    }

    /// Shared by `key_up` and `dial_up`, so dials get the same hold
    /// behaviour as keys.
    /// `chosen` is the window a dial's rotation showed: a tap activates it
    /// (if it is still there) instead of the usual tap; a hold ignores it.
    async fn press_released(
        &self,
        instance: &Instance,
        settings: &FocusOrLaunchSettings,
        chosen: Option<WindowId>,
    ) -> OpenActionResult<()> {
        // Removed before the early return below, so the map keeps no stale
        // entry when there is no backend.
        let pressed_at = self.pressed_at().remove(&instance.instance_id);
        let intent = release_intent(pressed_at, Instant::now(), settings);

        let Some(backend) = self.backend.as_deref() else {
            self.report(instance, &RunOutcome::NoBackend).await;
            return Ok(());
        };
        let apps = self.catalog.containing(settings.app_id()).await;
        let ctx = Context {
            backend,
            launcher: self.launcher.as_ref(),
            launches: &self.launches,
        };
        let outcome = match chosen {
            Some(id) if intent == Intent::FocusOrLaunch => {
                orchestrate::activate_chosen(&ctx, settings, &apps, &id).await
            }
            _ => orchestrate::run(&ctx, intent, settings, &apps).await,
        };
        self.report(instance, &outcome).await;
        Ok(())
    }
}

#[async_trait]
impl Action for FocusOrLaunchAction {
    const UUID: &'static str = "com.jfms7s.focuslauncher.focusorlaunch";
    type Settings = FocusOrLaunchSettings;

    /// No apps list here: no property inspector is open yet, and sending
    /// ~14 KB per key at profile load reached nobody.
    async fn will_appear(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        let apps = self.catalog.containing(settings.app_id()).await;
        self.apply_visuals(instance, settings, &apps).await
    }

    async fn will_disappear(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.pressed_at().remove(&instance.instance_id);
        self.pi_open().remove(&instance.instance_id);
        self.dial_cursors().remove(&instance.instance_id);
        if let Some(watcher) = &self.watcher {
            watcher.untrack(&instance.instance_id);
        }
        Ok(())
    }

    /// The user opened this key's settings: rescan (they may just have
    /// installed an app) and send the list.
    async fn property_inspector_did_appear(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        let apps = self.catalog.refresh().await;
        // Opening the PI is the "rescan" moment: an icon that failed before
        // (a theme installed since, a path created since) gets another try.
        self.icons.forget_all();
        self.pi_open().insert(instance.instance_id.clone());
        self.send_apps_to_pi(instance, &apps).await?;
        self.apply_visuals(instance, settings, &apps).await?;
        self.send_window_count_to_pi(instance, settings, &apps, true)
            .await
    }

    async fn property_inspector_did_disappear(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.pi_open().remove(&instance.instance_id);
        Ok(())
    }

    /// The PI saved a change (an app picked, a class typed): redraw the key
    /// and, while its PI is open, update the window count.
    async fn did_receive_settings(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        let apps = self.catalog.containing(settings.app_id()).await;
        self.apply_visuals(instance, settings, &apps).await?;
        if self.pi_open().contains(&instance.instance_id) {
            self.send_window_count_to_pi(instance, settings, &apps, false)
                .await?;
        }
        Ok(())
    }

    async fn key_down(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.press_started(instance);
        Ok(())
    }

    async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        self.press_released(instance, settings, None).await
    }

    /// The dial window switcher: each rotate event steps through the app's
    /// windows (all its ticks at once, one snapshot) and shows the window's
    /// place and title as the dial's title. It never activates anything;
    /// the next press does.
    async fn dial_rotate(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
        ticks: i16,
        _pressed: bool,
    ) -> OpenActionResult<()> {
        let Some(backend) = self.backend.as_deref() else {
            self.report(instance, &RunOutcome::NoBackend).await;
            return Ok(());
        };
        let apps = self.catalog.containing(settings.app_id()).await;
        let cursor = self
            .dial_cursors()
            .get(&instance.instance_id)
            .cloned()
            .flatten();
        match orchestrate::dial_preview(backend, settings, &apps, cursor.as_ref(), ticks).await {
            Ok(preview) => {
                let title = dial_title(preview.as_ref());
                self.dial_cursors()
                    .insert(instance.instance_id.clone(), preview.map(|p| p.id));
                instance.set_title(Some(title), None).await
            }
            Err(outcome) => {
                self.report(instance, &outcome).await;
                Ok(())
            }
        }
    }

    async fn dial_down(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.press_started(instance);
        Ok(())
    }

    async fn dial_up(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        let shown = self.dial_cursors().remove(&instance.instance_id);
        self.press_released(instance, settings, shown.clone().flatten())
            .await?;
        if shown.is_some() {
            // The title showed the preview: back to the app's own face.
            let apps = self.catalog.containing(settings.app_id()).await;
            self.apply_visuals(instance, settings, &apps).await?;
        }
        Ok(())
    }
}

/// Longest window title shown on a dial, in characters.
const DIAL_TITLE_CHARS: usize = 18;

/// A dial's title while rotating: `"<n>/<count>"`, then the window title
/// (shortened) on a second line when the backend reads titles.
fn dial_title(preview: Option<&DialPreview>) -> String {
    let Some(preview) = preview else {
        return "no windows".to_string();
    };
    let place = format!("{}/{}", preview.position, preview.count);
    match preview.title.as_deref().map(str::trim) {
        Some(title) if !title.is_empty() => {
            let mut short: String = title.chars().take(DIAL_TITLE_CHARS).collect();
            if title.chars().count() > DIAL_TITLE_CHARS {
                short.pop();
                short.push('…');
            }
            format!("{place}\n{short}")
        }
        _ => place,
    }
}

/// What live state follows for a key, or `None` when it shows none: the
/// option is off, the app isn't installed, its class is invalid (a press
/// would alert), or there is no icon `badge::compose` can draw over. The
/// classes are the key's class and, if it has one, its id fallback, as a
/// press resolves them (the title filter and desktop scope aren't applied).
fn watched(
    settings: &FocusOrLaunchSettings,
    apps: &[AppEntry],
    icon: Option<Arc<str>>,
) -> Option<(Vec<WindowClass>, Arc<str>)> {
    if !settings.show_running_state {
        return None;
    }
    let icon = icon.filter(|icon| badge::composable(icon))?;
    let entry = find_app(settings, apps)?;
    let mut classes = vec![WindowClass::parse(resolve_class(settings, entry)).ok()?];
    if let Some(fallback) =
        id_fallback_class(settings, entry).and_then(|f| WindowClass::parse(f).ok())
    {
        classes.push(fallback);
    }
    Some((classes, icon))
}

/// What a key shows: `None` means OpenDeck's default for that part.
#[derive(Debug, PartialEq, Eq)]
struct KeyFace<'a> {
    title: Option<&'a str>,
    icon: Option<&'a str>,
}

/// The key's title and icon for `settings`: both `None` when the selected app
/// isn't installed (or none is selected), else the resolved name and the
/// resolved `Icon=` value, if any.
fn key_face<'a>(settings: &'a FocusOrLaunchSettings, apps: &'a [AppEntry]) -> KeyFace<'a> {
    match find_app(settings, apps) {
        None => KeyFace {
            title: None,
            icon: None,
        },
        Some(entry) => KeyFace {
            title: Some(resolve_display_name(settings, entry)),
            icon: resolve_icon(settings, entry),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::test_support::{app, settings};

    fn preview(position: usize, count: usize, title: Option<&str>) -> DialPreview {
        DialPreview {
            id: WindowId::new("w"),
            position,
            count,
            title: title.map(str::to_string),
        }
    }

    #[test]
    fn a_dial_title_shows_the_place_and_a_short_window_title() {
        assert_eq!(
            dial_title(Some(&preview(2, 3, Some("Konsole")))),
            "2/3\nKonsole"
        );
        assert_eq!(dial_title(Some(&preview(1, 1, None))), "1/1");
        assert_eq!(dial_title(Some(&preview(1, 2, Some("  ")))), "1/2");
        assert_eq!(dial_title(None), "no windows");
        let long = dial_title(Some(&preview(1, 2, Some("ansible-management — Konsole"))));
        assert_eq!(long, "1/2\nansible-managemen…");
        assert_eq!(
            long.lines().nth(1).unwrap().chars().count(),
            DIAL_TITLE_CHARS
        );
    }

    #[test]
    fn the_picker_list_is_sorted_deduplicated_and_validated() {
        let list = pi_classes(vec![
            "firefox".into(),
            "Code".into(),
            "firefox".into(),
            "bad\"class".into(),
            "org.kde.kate".into(),
        ]);
        let classes: Vec<(&str, bool)> = list.iter().map(|c| (c.class.as_str(), c.valid)).collect();
        assert_eq!(
            classes,
            [
                ("bad\"class", false),
                ("Code", true),
                ("firefox", true),
                ("org.kde.kate", true)
            ]
        );
    }

    #[test]
    fn the_class_list_is_only_in_the_message_when_sent() {
        let without = serde_json::to_value(pi_window_count(Some(Ok(1)))).unwrap();
        assert!(without.get("window_classes").is_none());
        let mut with = pi_window_count(Some(Ok(1)));
        with.window_classes = Some(pi_classes(vec!["firefox".into()]));
        assert_eq!(
            serde_json::to_value(with).unwrap()["window_classes"],
            serde_json::json!([{"class": "firefox", "valid": true}])
        );
    }

    #[test]
    fn the_window_count_message_says_the_count_or_why_not() {
        assert_eq!(
            serde_json::to_value(pi_window_count(Some(Ok(3)))).unwrap(),
            serde_json::json!({"open_windows": 3, "windows_note": null})
        );
        assert_eq!(
            pi_window_count(Some(Err(RunOutcome::NoAppSelected))),
            PiWindowCount {
                open_windows: None,
                windows_note: None,
                window_classes: None
            }
        );
        let unavailable = pi_window_count(Some(Err(RunOutcome::BackendUnavailable(
            "kdotool not found".into(),
        ))));
        assert_eq!(unavailable.open_windows, None);
        assert!(
            unavailable
                .windows_note
                .as_deref()
                .is_some_and(|n| n.contains("kdotool not found")),
            "{unavailable:?}"
        );
        let no_backend = pi_window_count(None);
        assert_eq!(no_backend.open_windows, None);
        assert!(no_backend.windows_note.is_some());
    }

    const PNG: &str = "data:image/png;base64,iVBORw0KGgo=";

    #[test]
    fn a_key_is_watched_by_its_class_and_id_fallback() {
        let s = settings("org.mozilla.firefox", true, true);
        let apps = [app("org.mozilla.firefox", "firefox")];
        let (classes, icon) = watched(&s, &apps, Some(PNG.into())).unwrap();
        assert_eq!(
            classes,
            [
                WindowClass::parse("firefox").unwrap(),
                WindowClass::parse("org.mozilla.firefox").unwrap()
            ]
        );
        assert_eq!(&*icon, PNG);
    }

    #[test]
    fn a_class_override_is_watched_without_a_fallback() {
        let mut s = settings("org.mozilla.firefox", true, true);
        s.class_override = Some("Navigator".to_string());
        let apps = [app("org.mozilla.firefox", "firefox")];
        let (classes, _) = watched(&s, &apps, Some(PNG.into())).unwrap();
        assert_eq!(classes, [WindowClass::parse("Navigator").unwrap()]);
    }

    #[test]
    fn some_keys_are_not_watched() {
        let apps = [app("org.mozilla.firefox", "firefox")];
        let on = settings("org.mozilla.firefox", true, true);
        let mut off = on.clone();
        off.show_running_state = false;
        assert!(watched(&off, &apps, Some(PNG.into())).is_none());
        // No icon, or one resvg can't nest.
        assert!(watched(&on, &apps, None).is_none());
        assert!(watched(&on, &apps, Some("data:image/x-icon;base64,AAAB".into())).is_none());
        // Not installed.
        assert!(watched(&on, &[], Some(PNG.into())).is_none());
        // A class a press would reject.
        let mut bad = on.clone();
        bad.class_override = Some("bad\"class".to_string());
        assert!(watched(&bad, &apps, Some(PNG.into())).is_none());
    }

    #[test]
    fn key_face_shows_the_installed_apps_name_and_icon() {
        let s = settings("org.mozilla.firefox", true, true);
        let apps = [app("org.mozilla.firefox", "firefox")];
        assert_eq!(
            key_face(&s, &apps),
            KeyFace {
                title: Some("org.mozilla.firefox"),
                icon: Some("org.mozilla.firefox-icon"),
            }
        );
    }

    #[test]
    fn key_face_has_no_icon_when_the_app_declares_none() {
        let s = settings("org.mozilla.firefox", true, true);
        let mut entry = app("org.mozilla.firefox", "firefox");
        entry.icon = None;
        let apps = [entry];
        assert_eq!(
            key_face(&s, &apps),
            KeyFace {
                title: Some("org.mozilla.firefox"),
                icon: None,
            }
        );
    }

    #[test]
    fn key_face_uses_the_icon_override() {
        let mut s = settings("org.mozilla.firefox", true, true);
        s.icon_override = Some("/opt/icons/browser.png".to_string());
        let apps = [app("org.mozilla.firefox", "firefox")];
        assert_eq!(key_face(&s, &apps).icon, Some("/opt/icons/browser.png"));
    }

    #[test]
    fn key_face_is_empty_when_the_app_is_not_installed() {
        let s = settings("org.mozilla.firefox", true, true);
        let apps = [app("org.kde.kate", "kate")];
        assert_eq!(
            key_face(&s, &apps),
            KeyFace {
                title: None,
                icon: None,
            }
        );
        assert_eq!(
            key_face(&FocusOrLaunchSettings::default(), &apps),
            KeyFace {
                title: None,
                icon: None,
            }
        );
    }
    use std::time::Duration;

    fn close_all_on_hold(enabled: bool) -> FocusOrLaunchSettings {
        let mut s = settings("org.mozilla.firefox", true, true);
        s.close_all_windows_on_hold = enabled;
        s
    }

    #[test]
    fn release_intent_tap_with_close_all_on_focuses_or_launches() {
        let t0 = Instant::now();
        assert_eq!(
            release_intent(
                Some(t0),
                t0 + Duration::from_millis(10),
                &close_all_on_hold(true)
            ),
            Intent::FocusOrLaunch
        );
    }

    #[test]
    fn release_intent_hold_with_close_all_on_closes_all() {
        let t0 = Instant::now();
        assert_eq!(
            release_intent(Some(t0), t0 + HOLD_THRESHOLD * 2, &close_all_on_hold(true)),
            Intent::CloseAll
        );
    }

    #[test]
    fn release_intent_tap_and_hold_with_close_all_off_focus_or_launch() {
        let t0 = Instant::now();
        let off = close_all_on_hold(false);
        assert_eq!(
            release_intent(Some(t0), t0 + Duration::from_millis(10), &off),
            Intent::FocusOrLaunch
        );
        assert_eq!(
            release_intent(Some(t0), t0 + HOLD_THRESHOLD * 2, &off),
            Intent::FocusOrLaunch
        );
    }

    #[test]
    fn release_intent_hold_with_new_window_launches_another_and_a_tap_does_not() {
        let t0 = Instant::now();
        let mut s = settings("org.mozilla.firefox", true, true);
        s.hold_action = Some("new_window".to_string());
        assert_eq!(
            release_intent(Some(t0), t0 + HOLD_THRESHOLD * 2, &s),
            Intent::LaunchAnother
        );
        assert_eq!(
            release_intent(Some(t0), t0 + Duration::from_millis(10), &s),
            Intent::FocusOrLaunch
        );
    }

    #[test]
    fn release_intent_without_a_recorded_press_is_a_tap() {
        assert_eq!(
            release_intent(None, Instant::now(), &close_all_on_hold(true)),
            Intent::FocusOrLaunch
        );
    }

    #[test]
    fn release_intent_exactly_at_the_threshold_is_a_hold() {
        let t0 = Instant::now();
        assert_eq!(
            release_intent(Some(t0), t0 + HOLD_THRESHOLD, &close_all_on_hold(true)),
            Intent::CloseAll
        );
    }

    #[test]
    fn release_intent_just_under_the_threshold_is_a_tap() {
        let t0 = Instant::now();
        let later = t0 + HOLD_THRESHOLD - Duration::from_millis(1);
        assert_eq!(
            release_intent(Some(t0), later, &close_all_on_hold(true)),
            Intent::FocusOrLaunch
        );
    }

    #[test]
    fn release_intent_with_a_clock_behind_the_press_is_a_tap() {
        let t0 = Instant::now() + Duration::from_secs(1);
        assert_eq!(
            release_intent(
                Some(t0),
                t0 - Duration::from_millis(1),
                &close_all_on_hold(true)
            ),
            Intent::FocusOrLaunch
        );
    }

    #[test]
    fn the_pi_apps_payload_includes_the_window_class() {
        let apps = [app("org.mozilla.firefox", "firefox")];
        let payload = PiPayload {
            apps: apps.iter().map(PiApp::from).collect(),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"apps": [{
                "id": "org.mozilla.firefox",
                "name": "org.mozilla.firefox",
                "path": "/usr/share/applications/org.mozilla.firefox.desktop",
                "exec": "org.mozilla.firefox-binary",
                "window_class": "firefox",
            }]})
        );
    }

    #[test]
    fn the_action_uuid_matches_the_manifest() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../assets/manifest.json")).unwrap();
        assert_eq!(
            manifest["Actions"][0]["UUID"].as_str(),
            Some(<FocusOrLaunchAction as Action>::UUID)
        );
    }
}
