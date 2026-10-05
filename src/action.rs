//! The `OpenDeck` adapter: maps key/dial events to a gesture and intent, runs
//! `orchestrate::run`, and shows the result on the key. All decisions live
//! in `decision`, `orchestrate` and `settings`; this file only wires events.

use crate::apps::{AppEntry, Launcher, SystemLauncher};
use crate::backend::WindowBackend;
use crate::catalog::AppCatalog;
use crate::decision::{Gesture, HOLD_THRESHOLD, intent_for, is_hold};
use crate::icon::IconCache;
use crate::orchestrate::{self, Context, LaunchGuard, RunOutcome, Severity, report_for};
use crate::settings::{FocusOrLaunchSettings, find_app, resolve_display_name, resolve_icon};
use async_trait::async_trait;
use openaction::{Action, Instance, OpenActionResult};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
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

pub struct FocusOrLaunchAction {
    /// `None` when no backend fits this desktop session (see
    /// `select_backend`); every press then alerts instead of guessing.
    backend: Option<Box<dyn WindowBackend>>,
    launcher: Box<dyn Launcher>,
    catalog: AppCatalog,
    icons: IconCache,
    launches: LaunchGuard,
    /// When each instance's key or dial was last pressed, so the release can
    /// tell a hold from a tap. One action object serves every key.
    pressed_at: Mutex<HashMap<String, Instant>>,
}

impl FocusOrLaunchAction {
    pub fn new(backend: Option<Box<dyn WindowBackend>>) -> Self {
        Self {
            backend,
            launcher: Box::new(SystemLauncher),
            catalog: AppCatalog::system(),
            icons: IconCache::new(),
            launches: LaunchGuard::default(),
            pressed_at: Mutex::new(HashMap::new()),
        }
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
    /// A missing icon is logged (once, by the cache) and skipped: a key with
    /// no icon is still usable.
    async fn apply_visuals(
        &self,
        instance: &Instance,
        settings: &FocusOrLaunchSettings,
        apps: &[AppEntry],
    ) -> OpenActionResult<()> {
        let Some(entry) = find_app(settings, apps) else {
            if let Some(id) = settings.app_id() {
                log::warn!("the app selected for a key ({id}) is not installed");
            }
            return Ok(());
        };
        instance
            .set_title(Some(resolve_display_name(settings, entry)), None)
            .await?;
        if let Some(icon) = resolve_icon(settings, entry) {
            if let Some(uri) = self.icons.data_uri(icon).await {
                instance.set_image(Some(uri.as_ref()), None).await?;
            }
        } else {
            log::warn!("{} has no icon", entry.id);
        }
        Ok(())
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
    async fn press_released(
        &self,
        instance: &Instance,
        settings: &FocusOrLaunchSettings,
    ) -> OpenActionResult<()> {
        let pressed_at = self.pressed_at().remove(&instance.instance_id);
        let gesture = if pressed_at.is_some_and(|at| is_hold(at.elapsed(), HOLD_THRESHOLD)) {
            Gesture::Hold
        } else {
            Gesture::Tap
        };
        let intent = intent_for(gesture, settings.hold_action());

        let Some(backend) = self.backend.as_deref() else {
            log::error!("no supported window backend for this desktop session; taking no action");
            if let Err(e) = instance.show_alert().await {
                log::warn!("could not show the alert on the key: {e}");
            }
            return Ok(());
        };
        let apps = self.catalog.containing(settings.app_id()).await;
        let ctx = Context {
            backend,
            launcher: self.launcher.as_ref(),
            launches: &self.launches,
        };
        let outcome = orchestrate::run(&ctx, intent, settings, &apps).await;
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
        self.send_apps_to_pi(instance, &apps).await?;
        self.apply_visuals(instance, settings, &apps).await
    }

    async fn did_receive_settings(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        let apps = self.catalog.containing(settings.app_id()).await;
        self.apply_visuals(instance, settings, &apps).await
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
        self.press_released(instance, settings).await
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
        self.press_released(instance, settings).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::test_support::app;

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
