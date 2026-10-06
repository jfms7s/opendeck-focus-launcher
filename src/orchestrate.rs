//! One press, end to end: resolve the app and class, snapshot its windows,
//! decide, dispatch. Returns a `RunOutcome` value and never touches the
//! `OpenDeck` `Instance`, so it is tested against a fake backend and launcher.

use crate::apps::{AppEntry, Launcher, build_launch_argv};
use crate::backend::{BackendError, WindowBackend, WindowClass, WindowId, WindowSnapshot};
use crate::decision::{Decision, Intent, decide, decide_close_all};
use crate::settings::{
    FocusOrLaunchSettings, find_app, id_fallback_class, non_empty, resolve_class,
};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq, Eq)]
pub enum RunOutcome {
    NoAppSelected,
    AppNotFound(String),
    InvalidClass(String),
    BackendUnavailable(String),
    ListWindowsFailed(String),
    /// No window yet, but this app was launched moments ago: a repeat press
    /// during a cold start, not a request for a second instance.
    LaunchPending,
    Launched(Vec<String>),
    LaunchFailed(String),
    Activated(WindowId),
    ActivateFailed(WindowId, String),
    Minimized(WindowId),
    MinimizeFailed(WindowId, String),
    NoOp,
    ClosedAll(Vec<WindowId>),
    CloseFailed(Vec<(WindowId, String)>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warn,
    Error,
}

/// How an outcome is surfaced: log level, whether the key shows the alert
/// triangle, and the log line. Every failure alerts; nothing fails silently.
#[derive(Debug, PartialEq, Eq)]
pub struct Report {
    pub severity: Severity,
    pub alert: bool,
    pub message: String,
}

pub fn report_for(outcome: &RunOutcome) -> Report {
    let (severity, alert, message) = match outcome {
        RunOutcome::NoAppSelected => (
            Severity::Warn,
            false,
            "no app selected for this key".to_string(),
        ),
        RunOutcome::AppNotFound(id) => (
            Severity::Error,
            true,
            format!("the app selected for this key ({id}) is not installed"),
        ),
        RunOutcome::InvalidClass(msg) => (
            Severity::Error,
            true,
            format!("invalid window class: {msg}"),
        ),
        RunOutcome::BackendUnavailable(msg) => (
            Severity::Error,
            true,
            format!("window backend unavailable: {msg}"),
        ),
        RunOutcome::ListWindowsFailed(msg) => (
            Severity::Error,
            true,
            format!("could not list windows: {msg}"),
        ),
        RunOutcome::LaunchPending => (
            Severity::Info,
            false,
            "app is still starting; not launching it again".to_string(),
        ),
        RunOutcome::Launched(argv) => (Severity::Info, false, format!("launched {argv:?}")),
        RunOutcome::LaunchFailed(msg) => (
            Severity::Error,
            true,
            format!("failed to launch app: {msg}"),
        ),
        RunOutcome::Activated(id) => (Severity::Info, false, format!("activated window {id}")),
        RunOutcome::ActivateFailed(id, msg) => (
            Severity::Error,
            true,
            format!("failed to activate window {id}: {msg}"),
        ),
        RunOutcome::Minimized(id) => (Severity::Info, false, format!("minimized window {id}")),
        RunOutcome::MinimizeFailed(id, msg) => (
            Severity::Error,
            true,
            format!("failed to minimize window {id}: {msg}"),
        ),
        RunOutcome::NoOp => (Severity::Info, false, "nothing to do".to_string()),
        RunOutcome::ClosedAll(ids) => (
            Severity::Info,
            false,
            format!("closed {} window(s) {ids:?}", ids.len()),
        ),
        RunOutcome::CloseFailed(failures) => (
            Severity::Error,
            true,
            format!(
                "failed to close {} window(s): {}",
                failures.len(),
                failures
                    .iter()
                    .map(|(id, msg)| format!("{id}: {msg}"))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        ),
    };
    Report {
        severity,
        alert,
        message,
    }
}

/// How long after a launch an app with no window counts as "still
/// starting". Covers typical Electron/browser cold starts.
pub const LAUNCH_GRACE: Duration = Duration::from_secs(4);

/// Remembers recent launches per app id (shared by every key), so a second
/// press, or another key bound to the same app, during a cold start doesn't
/// launch a duplicate. Cleared as soon as the app's window shows up.
pub struct LaunchGuard {
    grace: Duration,
    launched_at: Mutex<HashMap<String, Instant>>,
}

impl LaunchGuard {
    pub fn new(grace: Duration) -> Self {
        Self {
            grace,
            launched_at: Mutex::new(HashMap::new()),
        }
    }

    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, Instant>> {
        self.launched_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn is_pending(&self, app_id: &str, now: Instant) -> bool {
        self.map()
            .get(app_id)
            .is_some_and(|at| now.saturating_duration_since(*at) < self.grace)
    }

    fn record(&self, app_id: &str, now: Instant) {
        self.map().insert(app_id.to_string(), now);
    }

    fn clear(&self, app_id: &str) {
        self.map().remove(app_id);
    }
}

impl Default for LaunchGuard {
    fn default() -> Self {
        Self::new(LAUNCH_GRACE)
    }
}

/// Everything a press needs besides the settings and app list.
pub struct Context<'a> {
    pub backend: &'a dyn WindowBackend,
    pub launcher: &'a dyn Launcher,
    pub launches: &'a LaunchGuard,
}

fn backend_failure(e: BackendError) -> RunOutcome {
    match e {
        BackendError::Unavailable(msg) => RunOutcome::BackendUnavailable(msg),
        BackendError::CommandFailed(msg) => RunOutcome::ListWindowsFailed(msg),
    }
}

/// Resolves the target app and class, then snapshots its windows, retrying
/// with the entry id (see `id_fallback_class`) when the app's own class
/// matches nothing. A failed retry is reported, not taken as "no windows":
/// launching on an unknown state could open a duplicate.
async fn target_snapshot<'a>(
    backend: &dyn WindowBackend,
    settings: &FocusOrLaunchSettings,
    apps: &'a [AppEntry],
) -> Result<(&'a AppEntry, WindowSnapshot), RunOutcome> {
    let Some(app_id) = settings.app_id() else {
        return Err(RunOutcome::NoAppSelected);
    };
    let entry =
        find_app(settings, apps).ok_or_else(|| RunOutcome::AppNotFound(app_id.to_string()))?;
    let class = WindowClass::parse(resolve_class(settings, entry))
        .map_err(|e| RunOutcome::InvalidClass(format!("{}: {e}", entry.id)))?;

    let mut snapshot = backend.snapshot(&class).await.map_err(backend_failure)?;
    let mut matched_class = class;
    if snapshot.windows.is_empty()
        && let Some(fallback) = id_fallback_class(settings, entry)
        && let Ok(fallback) = WindowClass::parse(fallback)
    {
        let retry = backend.snapshot(&fallback).await.map_err(|e| {
            log::warn!(
                "{}: retry with the desktop id {fallback} failed: {e}",
                entry.id
            );
            backend_failure(e)
        })?;
        if !retry.windows.is_empty() {
            snapshot = retry;
            matched_class = fallback;
        }
    }
    log::info!(
        "{}: class {matched_class} matched {} window(s) {:?}, focused {:?}",
        entry.id,
        snapshot.windows.len(),
        snapshot.windows,
        snapshot.active
    );
    Ok((entry, snapshot))
}

/// Runs one press for `intent` and reports what happened.
pub async fn run(
    ctx: &Context<'_>,
    intent: Intent,
    settings: &FocusOrLaunchSettings,
    apps: &[AppEntry],
) -> RunOutcome {
    let (entry, snapshot) = match target_snapshot(ctx.backend, settings, apps).await {
        Ok(v) => v,
        Err(outcome) => return outcome,
    };
    if !snapshot.windows.is_empty() {
        ctx.launches.clear(&entry.id);
    }

    let decision = match intent {
        Intent::FocusOrLaunch => decide(
            &snapshot.windows,
            snapshot.active.as_ref(),
            settings.cycle_windows,
            settings.minimize_when_focused,
        ),
        Intent::CloseAll => decide_close_all(&snapshot.windows),
    };

    match decision {
        Decision::Launch => launch(ctx, settings, entry),
        Decision::Activate(id) => match ctx.backend.activate(&id).await {
            Ok(()) => RunOutcome::Activated(id),
            Err(e) => RunOutcome::ActivateFailed(id, e.to_string()),
        },
        Decision::Minimize(id) => match ctx.backend.minimize(&id).await {
            Ok(()) => RunOutcome::Minimized(id),
            Err(e) => RunOutcome::MinimizeFailed(id, e.to_string()),
        },
        Decision::CloseAll(ids) => close_all(ctx.backend, ids).await,
        Decision::NoOp => RunOutcome::NoOp,
    }
}

fn launch(ctx: &Context<'_>, settings: &FocusOrLaunchSettings, entry: &AppEntry) -> RunOutcome {
    let now = Instant::now();
    if ctx.launches.is_pending(&entry.id, now) {
        return RunOutcome::LaunchPending;
    }
    let argv = match build_launch_argv(
        entry,
        non_empty(settings.exec_override.as_deref()),
        non_empty(settings.custom_args.as_deref()),
    ) {
        Ok(argv) => argv,
        Err(e) => return RunOutcome::LaunchFailed(format!("{}: {e}", entry.id)),
    };
    match ctx.launcher.launch(&argv) {
        Ok(()) => {
            ctx.launches.record(&entry.id, now);
            RunOutcome::Launched(argv)
        }
        Err(e) => RunOutcome::LaunchFailed(e.to_string()),
    }
}

/// Closes every window; a failure on one doesn't stop the rest.
async fn close_all(backend: &dyn WindowBackend, ids: Vec<WindowId>) -> RunOutcome {
    let mut failures = Vec::new();
    for id in &ids {
        if let Err(e) = backend.close(id).await {
            failures.push((id.clone(), e.to_string()));
        }
    }
    if failures.is_empty() {
        RunOutcome::ClosedAll(ids)
    } else {
        RunOutcome::CloseFailed(failures)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::test_support::{app, settings};
    use async_trait::async_trait;

    fn id(s: &str) -> WindowId {
        WindowId::new(s)
    }

    fn ids(list: &[&str]) -> Vec<WindowId> {
        list.iter().map(|s| id(s)).collect()
    }

    /// Records every call and returns canned responses. Windows are keyed by
    /// the queried class, so tests also prove which class reached the
    /// backend; `windows_for_any_class` answers every query instead.
    #[derive(Default)]
    struct RecordingFakeBackend {
        windows_by_class: HashMap<String, Vec<WindowId>>,
        windows_for_any_class: Option<Vec<WindowId>>,
        active: Option<WindowId>,
        list_err: Option<BackendError>,
        /// Fail every `list_windows` call after the first.
        fail_after_first_list: Option<BackendError>,
        active_err: Option<BackendError>,
        list_calls: Mutex<Vec<String>>,
        activate_calls: Mutex<Vec<WindowId>>,
        minimize_calls: Mutex<Vec<WindowId>>,
        close_calls: Mutex<Vec<WindowId>>,
        close_fails_for: Vec<WindowId>,
    }

    #[async_trait]
    impl WindowBackend for RecordingFakeBackend {
        async fn list_windows(&self, class: &WindowClass) -> Result<Vec<WindowId>, BackendError> {
            let mut calls = self.list_calls.lock().unwrap();
            calls.push(class.as_str().to_string());
            if let Some(err) = &self.list_err {
                return Err(err.clone());
            }
            if calls.len() > 1
                && let Some(err) = &self.fail_after_first_list
            {
                return Err(err.clone());
            }
            if let Some(all) = &self.windows_for_any_class {
                return Ok(all.clone());
            }
            Ok(self
                .windows_by_class
                .get(class.as_str())
                .cloned()
                .unwrap_or_default())
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
            match &self.active_err {
                Some(err) => Err(err.clone()),
                None => Ok(self.active.clone()),
            }
        }
    }

    fn with_windows(class: &str, windows: &[&str]) -> RecordingFakeBackend {
        RecordingFakeBackend {
            windows_by_class: HashMap::from([(class.to_string(), ids(windows))]),
            ..Default::default()
        }
    }

    /// Records launches instead of spawning anything.
    #[derive(Default)]
    struct RecordingLauncher {
        launches: Mutex<Vec<Vec<String>>>,
        fail: bool,
    }

    impl Launcher for RecordingLauncher {
        fn launch(&self, argv: &[String]) -> std::io::Result<()> {
            if self.fail {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "no such binary",
                ));
            }
            self.launches.lock().unwrap().push(argv.to_vec());
            Ok(())
        }
    }

    struct Harness {
        backend: RecordingFakeBackend,
        launcher: RecordingLauncher,
        launches: LaunchGuard,
    }

    impl Harness {
        fn new(backend: RecordingFakeBackend) -> Self {
            Self {
                backend,
                launcher: RecordingLauncher::default(),
                launches: LaunchGuard::default(),
            }
        }

        async fn press(
            &self,
            intent: Intent,
            settings: &FocusOrLaunchSettings,
            apps: &[AppEntry],
        ) -> RunOutcome {
            let ctx = Context {
                backend: &self.backend,
                launcher: &self.launcher,
                launches: &self.launches,
            };
            run(&ctx, intent, settings, apps).await
        }

        async fn tap(&self, settings: &FocusOrLaunchSettings, apps: &[AppEntry]) -> RunOutcome {
            self.press(Intent::FocusOrLaunch, settings, apps).await
        }

        fn launched(&self) -> Vec<Vec<String>> {
            self.launcher.launches.lock().unwrap().clone()
        }
    }

    fn firefox_apps() -> Vec<AppEntry> {
        vec![app("org.mozilla.firefox", "firefox")]
    }

    fn firefox() -> FocusOrLaunchSettings {
        settings("org.mozilla.firefox", true, true)
    }

    #[tokio::test]
    async fn launches_when_no_window_is_open_without_a_shell() {
        let h = Harness::new(RecordingFakeBackend::default());
        // The platform's own launch command: the entry's Exec= line, or
        // `open -b <bundle id>` on macOS.
        let argv: Vec<String> = if cfg!(target_os = "macos") {
            ["open", "-b", "org.mozilla.firefox"]
                .map(String::from)
                .to_vec()
        } else {
            vec!["org.mozilla.firefox-binary".to_string()]
        };
        let outcome = h.tap(&firefox(), &firefox_apps()).await;
        assert_eq!(outcome, RunOutcome::Launched(argv.clone()));
        assert_eq!(h.launched(), vec![argv]);
        assert!(h.backend.activate_calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn exec_override_and_custom_args_reach_the_launcher_as_argv() {
        let h = Harness::new(RecordingFakeBackend::default());
        let mut s = firefox();
        s.exec_override = Some("firefox --private-window".to_string());
        s.custom_args = Some("'https://example.com/a b' $(id)".to_string());
        h.tap(&s, &firefox_apps()).await;
        assert_eq!(
            h.launched(),
            vec![vec![
                "firefox",
                "--private-window",
                "https://example.com/a b",
                "$(id)"
            ]]
        );
    }

    #[tokio::test]
    async fn a_launch_that_cannot_spawn_is_reported_as_failed() {
        let mut h = Harness::new(RecordingFakeBackend::default());
        h.launcher.fail = true;
        let outcome = h.tap(&firefox(), &firefox_apps()).await;
        assert!(
            matches!(outcome, RunOutcome::LaunchFailed(_)),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn an_unparseable_exec_is_reported_as_failed_without_launching() {
        let h = Harness::new(RecordingFakeBackend::default());
        let mut s = firefox();
        s.exec_override = Some("firefox 'unterminated".to_string());
        let outcome = h.tap(&s, &firefox_apps()).await;
        assert!(
            matches!(outcome, RunOutcome::LaunchFailed(_)),
            "{outcome:?}"
        );
        assert!(h.launched().is_empty());
    }

    #[tokio::test]
    async fn a_second_press_during_a_cold_start_does_not_launch_a_duplicate() {
        let h = Harness::new(RecordingFakeBackend::default());
        let apps = firefox_apps();
        assert!(matches!(
            h.tap(&firefox(), &apps).await,
            RunOutcome::Launched(_)
        ));
        assert_eq!(h.tap(&firefox(), &apps).await, RunOutcome::LaunchPending);
        assert_eq!(h.launched().len(), 1);
    }

    #[tokio::test]
    async fn the_launch_guard_clears_once_the_window_appears() {
        // Launched, then the window showed up: a later press with no windows
        // (the user closed it) must be allowed to launch again.
        let h = Harness::new(with_windows("firefox", &["w1"]));
        h.launches.record("org.mozilla.firefox", Instant::now());
        h.tap(&firefox(), &firefox_apps()).await;
        assert!(!h.launches.is_pending("org.mozilla.firefox", Instant::now()));
    }

    #[test]
    fn the_launch_guard_expires_after_the_grace_period() {
        let guard = LaunchGuard::new(Duration::from_secs(4));
        let t0 = Instant::now();
        guard.record("a", t0);
        assert!(guard.is_pending("a", t0 + Duration::from_secs(3)));
        assert!(!guard.is_pending("a", t0 + Duration::from_secs(4)));
        assert!(!guard.is_pending("b", t0));
    }

    #[tokio::test]
    async fn activates_a_background_window() {
        let h = Harness::new(with_windows("firefox", &["w1"]));
        assert_eq!(
            h.tap(&firefox(), &firefox_apps()).await,
            RunOutcome::Activated(id("w1"))
        );
        assert_eq!(*h.backend.activate_calls.lock().unwrap(), ids(&["w1"]));
    }

    #[tokio::test]
    async fn cycles_to_next_window_when_focused_with_several_open() {
        let mut backend = with_windows("firefox", &["w1", "w2"]);
        backend.active = Some(id("w1"));
        let h = Harness::new(backend);
        assert_eq!(
            h.tap(&firefox(), &firefox_apps()).await,
            RunOutcome::Activated(id("w2"))
        );
    }

    #[tokio::test]
    async fn minimizes_a_focused_lone_window() {
        let mut backend = with_windows("firefox", &["w1"]);
        backend.active = Some(id("w1"));
        let h = Harness::new(backend);
        assert_eq!(
            h.tap(&firefox(), &firefox_apps()).await,
            RunOutcome::Minimized(id("w1"))
        );
        assert_eq!(*h.backend.minimize_calls.lock().unwrap(), ids(&["w1"]));
    }

    #[tokio::test]
    async fn a_failed_focused_window_query_still_brings_the_app_forward() {
        let mut backend = with_windows("firefox", &["w1"]);
        backend.active_err = Some(BackendError::CommandFailed("boom".into()));
        let h = Harness::new(backend);
        assert_eq!(
            h.tap(&firefox(), &firefox_apps()).await,
            RunOutcome::Activated(id("w1"))
        );
    }

    #[tokio::test]
    async fn reports_backend_unavailable_without_launching() {
        let h = Harness::new(RecordingFakeBackend {
            list_err: Some(BackendError::Unavailable("no wmctrl".to_string())),
            ..Default::default()
        });
        assert_eq!(
            h.tap(&firefox(), &firefox_apps()).await,
            RunOutcome::BackendUnavailable("no wmctrl".to_string())
        );
        assert!(h.launched().is_empty());
    }

    #[tokio::test]
    async fn falls_back_to_entry_id_when_startup_wm_class_matches_nothing() {
        let h = Harness::new(with_windows("chrome-abc-Default", &["w1"]));
        let apps = vec![app("chrome-abc-Default", "crx_abc")];
        let outcome = h
            .tap(&settings("chrome-abc-Default", true, true), &apps)
            .await;
        assert_eq!(outcome, RunOutcome::Activated(id("w1")));
        assert_eq!(
            *h.backend.list_calls.lock().unwrap(),
            vec!["crx_abc", "chrome-abc-Default"]
        );
    }

    #[tokio::test]
    async fn launches_when_neither_class_nor_id_fallback_matches() {
        let h = Harness::new(RecordingFakeBackend::default());
        let apps = vec![app("chrome-abc-Default", "crx_abc")];
        let outcome = h
            .tap(&settings("chrome-abc-Default", true, true), &apps)
            .await;
        assert!(matches!(outcome, RunOutcome::Launched(_)));
    }

    #[tokio::test]
    async fn a_failing_fallback_search_is_reported_and_does_not_launch() {
        let h = Harness::new(RecordingFakeBackend {
            fail_after_first_list: Some(BackendError::CommandFailed("boom".into())),
            ..Default::default()
        });
        let apps = vec![app("chrome-abc-Default", "crx_abc")];
        let outcome = h
            .tap(&settings("chrome-abc-Default", true, true), &apps)
            .await;
        assert_eq!(outcome, RunOutcome::ListWindowsFailed("boom".into()));
        assert!(h.launched().is_empty());
    }

    #[tokio::test]
    async fn does_not_fall_back_when_class_was_explicitly_overridden() {
        // The override matches nothing, but the entry id would: the fallback
        // must not silently ignore the user's override.
        let h = Harness::new(with_windows("chrome-abc-Default", &["w1"]));
        let apps = vec![app("chrome-abc-Default", "crx_abc")];
        let mut s = settings("chrome-abc-Default", true, true);
        s.class_override = Some("some-typo".to_string());
        assert!(matches!(h.tap(&s, &apps).await, RunOutcome::Launched(_)));
        assert_eq!(*h.backend.list_calls.lock().unwrap(), vec!["some-typo"]);
    }

    #[tokio::test]
    async fn reports_no_app_selected() {
        let h = Harness::new(RecordingFakeBackend::default());
        let outcome = h
            .tap(&FocusOrLaunchSettings::default(), &firefox_apps())
            .await;
        assert_eq!(outcome, RunOutcome::NoAppSelected);
    }

    #[tokio::test]
    async fn reports_an_uninstalled_app() {
        let h = Harness::new(RecordingFakeBackend::default());
        let outcome = h
            .tap(&settings("gone.app", true, true), &firefox_apps())
            .await;
        assert_eq!(outcome, RunOutcome::AppNotFound("gone.app".to_string()));
        assert!(h.backend.list_calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_invalid_class_override_never_reaches_the_backend() {
        let h = Harness::new(RecordingFakeBackend::default());
        let mut s = firefox();
        s.class_override = Some("x`${callDBus()}`".to_string());
        let outcome = h.tap(&s, &firefox_apps()).await;
        assert!(
            matches!(outcome, RunOutcome::InvalidClass(_)),
            "{outcome:?}"
        );
        assert!(h.backend.list_calls.lock().unwrap().is_empty());
        assert!(h.launched().is_empty());
    }

    #[tokio::test]
    async fn close_all_closes_every_matching_window() {
        let h = Harness::new(with_windows("firefox", &["w1", "w2"]));
        let outcome = h.press(Intent::CloseAll, &firefox(), &firefox_apps()).await;
        assert_eq!(outcome, RunOutcome::ClosedAll(ids(&["w1", "w2"])));
        assert_eq!(*h.backend.close_calls.lock().unwrap(), ids(&["w1", "w2"]));
    }

    #[tokio::test]
    async fn close_all_with_a_blank_class_override_uses_the_apps_own_class() {
        // A blank override used to reach the backends as "" or "  ", which
        // every backend matched against every window on the desktop.
        for blank in ["", "  ", "\t"] {
            let h = Harness::new(with_windows("firefox", &["w1"]));
            let mut s = firefox();
            s.class_override = Some(blank.to_string());
            let outcome = h.press(Intent::CloseAll, &s, &firefox_apps()).await;
            assert_eq!(outcome, RunOutcome::ClosedAll(ids(&["w1"])));
            assert_eq!(*h.backend.list_calls.lock().unwrap(), vec!["firefox"]);
        }
    }

    #[tokio::test]
    async fn close_all_refuses_an_app_whose_class_is_empty() {
        let h = Harness::new(RecordingFakeBackend {
            windows_for_any_class: Some(ids(&["every", "window"])),
            ..Default::default()
        });
        let apps = vec![app("weird.app", "   ")];
        let outcome = h
            .press(Intent::CloseAll, &settings("weird.app", true, true), &apps)
            .await;
        assert!(
            matches!(outcome, RunOutcome::InvalidClass(_)),
            "{outcome:?}"
        );
        assert!(h.backend.close_calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn close_all_is_a_no_op_when_nothing_is_open() {
        let h = Harness::new(RecordingFakeBackend::default());
        let outcome = h.press(Intent::CloseAll, &firefox(), &firefox_apps()).await;
        assert_eq!(outcome, RunOutcome::NoOp);
        assert!(h.backend.close_calls.lock().unwrap().is_empty());
        assert!(h.launched().is_empty());
    }

    #[tokio::test]
    async fn close_all_reports_windows_that_failed_to_close_and_tries_the_rest() {
        let mut backend = with_windows("firefox", &["w1", "w2"]);
        backend.close_fails_for = ids(&["w1"]);
        let h = Harness::new(backend);
        let outcome = h.press(Intent::CloseAll, &firefox(), &firefox_apps()).await;
        assert_eq!(
            outcome,
            RunOutcome::CloseFailed(vec![(
                id("w1"),
                "backend command failed: could not close w1".to_string()
            )])
        );
        assert_eq!(*h.backend.close_calls.lock().unwrap(), ids(&["w1", "w2"]));
    }

    #[tokio::test]
    async fn close_all_falls_back_to_entry_id() {
        let h = Harness::new(with_windows("chrome-abc-Default", &["w1"]));
        let apps = vec![app("chrome-abc-Default", "crx_abc")];
        let outcome = h
            .press(
                Intent::CloseAll,
                &settings("chrome-abc-Default", true, true),
                &apps,
            )
            .await;
        assert_eq!(outcome, RunOutcome::ClosedAll(ids(&["w1"])));
    }

    #[test]
    fn every_failure_alerts_and_no_success_does() {
        let failures = [
            RunOutcome::AppNotFound("a".into()),
            RunOutcome::InvalidClass("x".into()),
            RunOutcome::BackendUnavailable("x".into()),
            RunOutcome::ListWindowsFailed("x".into()),
            RunOutcome::LaunchFailed("x".into()),
            RunOutcome::ActivateFailed(id("w"), "x".into()),
            RunOutcome::MinimizeFailed(id("w"), "x".into()),
            RunOutcome::CloseFailed(vec![(id("w"), "x".into())]),
        ];
        for outcome in &failures {
            let report = report_for(outcome);
            assert!(report.alert, "{outcome:?} must alert");
            assert_eq!(report.severity, Severity::Error, "{outcome:?}");
        }
        let quiet = [
            RunOutcome::NoAppSelected,
            RunOutcome::LaunchPending,
            RunOutcome::Launched(vec!["a".into()]),
            RunOutcome::Activated(id("w")),
            RunOutcome::Minimized(id("w")),
            RunOutcome::NoOp,
            RunOutcome::ClosedAll(vec![id("w")]),
        ];
        for outcome in &quiet {
            assert!(!report_for(outcome).alert, "{outcome:?} must not alert");
        }
    }
}
