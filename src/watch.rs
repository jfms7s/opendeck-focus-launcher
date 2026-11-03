//! Live key state: one background poller keeps every visible key's image
//! showing whether its app has a window (dimmed / dot / ring, see `badge`).
//!
//! Keys are tracked from `apply_visuals` and untracked on `will_disappear`.
//! While nothing is tracked the poller waits on a `Notify` with no timer and
//! makes no backend call. Otherwise it ticks every `INTERVAL`: one
//! `snapshot_many` over the distinct classes (bounded by `TICK_TIMEOUT`, so
//! a hung tool costs one skipped tick), then a repaint of only the keys
//! whose state changed. Linux only: on macOS (frozen) nothing is tracked.

use crate::backend::{WindowBackend, WindowClass, WindowSnapshot};
use crate::badge;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Once, PoisonError};
use std::time::Duration;
use tokio::sync::Notify;

/// How often a visible key's state is refreshed.
pub const INTERVAL: Duration = Duration::from_secs(2);
/// The most one tick's backend call may take; past it the tick is dropped.
/// Each tool run also has its own `CALL_TIMEOUT` (3 s).
pub const TICK_TIMEOUT: Duration = Duration::from_secs(5);

/// What a key shows about its app.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyState {
    /// No window: the icon is dimmed.
    Closed,
    /// At least one window: a dot.
    Running,
    /// One of its windows is focused: a ring and the dot.
    Focused,
}

/// A key's state from its class's snapshot and, when the key has one, its
/// id fallback's (used only when the class has no window, as a press does).
pub fn key_state(primary: &WindowSnapshot, fallback: Option<&WindowSnapshot>) -> KeyState {
    let snapshot = match fallback {
        Some(fallback) if primary.windows.is_empty() => fallback,
        _ => primary,
    };
    if snapshot.windows.is_empty() {
        KeyState::Closed
    } else if snapshot
        .active
        .as_ref()
        .is_some_and(|active| snapshot.windows.contains(active))
    {
        KeyState::Focused
    } else {
        KeyState::Running
    }
}

/// Sets a key's image; the production one goes through `openaction`.
#[async_trait]
pub trait Painter: Send + Sync {
    async fn paint(&self, instance_id: &str, image: &str);
}

/// Paints through `openaction`; an instance that has gone is skipped.
pub struct DeckPainter;

#[async_trait]
impl Painter for DeckPainter {
    async fn paint(&self, instance_id: &str, image: &str) {
        let Some(instance) = openaction::get_instance(instance_id.to_string()).await else {
            return;
        };
        if let Err(e) = instance.set_image(Some(image), None).await {
            log::warn!("could not update a key's running state: {e}");
        }
    }
}

/// What the poller needs to know about one visible key.
struct Entry {
    /// The key's class, then its id fallback if it has one.
    classes: Vec<WindowClass>,
    /// The key's plain icon, a data URI `badge::composable` accepts.
    icon: Arc<str>,
    /// The state last painted; `None` until the first tick paints it.
    shown: Option<KeyState>,
    /// Composed images for `icon`, by state.
    composed: HashMap<KeyState, Arc<str>>,
}

struct Shared {
    backend: Arc<dyn WindowBackend>,
    painter: Arc<dyn Painter>,
    registry: Mutex<HashMap<String, Entry>>,
    wake: Notify,
    interval: Duration,
    tick_timeout: Duration,
    /// Set while ticks fail, so a failure is logged once, not every tick.
    failing: AtomicBool,
}

impl Shared {
    fn registry(&self) -> MutexGuard<'_, HashMap<String, Entry>> {
        self.registry.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Every tracked key's classes, each once, in a stable order.
    fn distinct_classes(&self) -> Vec<WindowClass> {
        let mut classes: Vec<WindowClass> = Vec::new();
        for entry in self.registry().values() {
            for class in &entry.classes {
                if !classes.contains(class) {
                    classes.push(class.clone());
                }
            }
        }
        classes.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        classes
    }

    fn failed(&self, why: &str) {
        if !self.failing.swap(true, Ordering::Relaxed) {
            log::warn!("could not refresh the keys' running state (retrying every tick): {why}");
        }
    }

    /// One refresh: one backend call for every class, then a repaint of
    /// the keys whose state changed.
    async fn tick(&self, classes: &[WindowClass]) {
        let snapshots = match tokio::time::timeout(
            self.tick_timeout,
            self.backend.snapshot_many(classes),
        )
        .await
        {
            Err(_) => {
                return self.failed(&format!(
                    "the window backend took longer than {} s",
                    self.tick_timeout.as_secs_f64()
                ));
            }
            Ok(Err(e)) => return self.failed(&e.to_string()),
            Ok(Ok(snapshots)) if snapshots.len() != classes.len() => {
                return self.failed(&format!(
                    "asked for {} classes, got {} snapshots",
                    classes.len(),
                    snapshots.len()
                ));
            }
            Ok(Ok(snapshots)) => snapshots,
        };
        if self.failing.swap(false, Ordering::Relaxed) {
            log::info!("the keys' running state is refreshing again");
        }
        let snapshot_of = |class: &WindowClass| {
            classes
                .iter()
                .position(|c| c == class)
                .map(|i| &snapshots[i])
        };
        let mut paints = Vec::new();
        for (id, entry) in self.registry().iter_mut() {
            // A key tracked after `classes` was taken waits for the next tick.
            let Some(primary) = entry.classes.first().and_then(snapshot_of) else {
                continue;
            };
            let fallback = match entry.classes.get(1) {
                Some(class) => match snapshot_of(class) {
                    Some(snapshot) => Some(snapshot),
                    None => continue,
                },
                None => None,
            };
            let state = key_state(primary, fallback);
            if entry.shown == Some(state) {
                continue;
            }
            entry.shown = Some(state);
            let icon = Arc::clone(&entry.icon);
            let image = entry
                .composed
                .entry(state)
                .or_insert_with(|| badge::compose(&icon, state).into());
            paints.push((id.clone(), Arc::clone(image)));
        }
        // Painted outside the lock: `apply_visuals` may paint and re-track a
        // key meanwhile, and re-tracking forgets `shown`, so a stale paint
        // here is replaced on the next tick.
        for (id, image) in paints {
            self.painter.paint(&id, &image).await;
        }
    }
}

async fn poll(shared: Arc<Shared>) {
    loop {
        let classes = shared.distinct_classes();
        if classes.is_empty() {
            // Idle: no timer, no backend call, until a key is tracked.
            shared.wake.notified().await;
            continue;
        }
        shared.tick(&classes).await;
        tokio::select! {
            () = tokio::time::sleep(shared.interval) => {}
            () = shared.wake.notified() => {}
        }
    }
}

/// The registry of visible keys plus the one poller task, started with the
/// first tracked key.
pub struct Watcher {
    shared: Arc<Shared>,
    started: Once,
}

impl Watcher {
    pub fn new(backend: Arc<dyn WindowBackend>, painter: Arc<dyn Painter>) -> Self {
        Self::with_timing(backend, painter, INTERVAL, TICK_TIMEOUT)
    }

    fn with_timing(
        backend: Arc<dyn WindowBackend>,
        painter: Arc<dyn Painter>,
        interval: Duration,
        tick_timeout: Duration,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                backend,
                painter,
                registry: Mutex::new(HashMap::new()),
                wake: Notify::new(),
                interval,
                tick_timeout,
                failing: AtomicBool::new(false),
            }),
            started: Once::new(),
        }
    }

    /// Starts following `instance_id`: its class (and id fallback) and the
    /// plain icon it shows now. Call after painting that icon: the state is
    /// unknown until the next tick, which runs at once.
    pub fn track(&self, instance_id: &str, classes: Vec<WindowClass>, icon: Arc<str>) {
        debug_assert!(!classes.is_empty() && badge::composable(&icon));
        {
            let mut registry = self.shared.registry();
            let composed = match registry.remove(instance_id) {
                Some(old) if old.icon == icon => old.composed,
                _ => HashMap::new(),
            };
            registry.insert(
                instance_id.to_string(),
                Entry {
                    classes,
                    icon,
                    shown: None,
                    composed,
                },
            );
        }
        // A new poller ticks at once by itself; waking it as well would
        // leave a stored permit that makes it tick twice in a row.
        let mut spawned = false;
        self.started.call_once(|| {
            tokio::spawn(poll(Arc::clone(&self.shared)));
            spawned = true;
        });
        if !spawned {
            self.shared.wake.notify_one();
        }
    }

    /// Stops following `instance_id` (it disappeared, or its key no longer
    /// shows state). The last one leaving sends the poller idle at once.
    pub fn untrack(&self, instance_id: &str) {
        let now_empty = {
            let mut registry = self.shared.registry();
            registry.remove(instance_id).is_some() && registry.is_empty()
        };
        if now_empty {
            self.shared.wake.notify_one();
        }
    }

    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.shared.registry().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendError, WindowId};

    fn class(name: &str) -> WindowClass {
        WindowClass::parse(name).unwrap()
    }

    fn snap(windows: &[&str], active: Option<&str>) -> WindowSnapshot {
        WindowSnapshot {
            windows: windows.iter().map(|w| WindowId::new(*w)).collect(),
            active: active.map(WindowId::new),
        }
    }

    #[test]
    fn no_window_is_closed() {
        assert_eq!(key_state(&snap(&[], Some("x")), None), KeyState::Closed);
    }

    #[test]
    fn a_window_is_running() {
        assert_eq!(key_state(&snap(&["a"], None), None), KeyState::Running);
        assert_eq!(
            key_state(&snap(&["a", "b"], Some("x")), None),
            KeyState::Running
        );
    }

    #[test]
    fn a_focused_window_is_focused() {
        assert_eq!(
            key_state(&snap(&["a", "b"], Some("b")), None),
            KeyState::Focused
        );
    }

    #[test]
    fn the_fallback_counts_only_when_the_class_has_no_window() {
        let fallback = snap(&["f"], Some("f"));
        assert_eq!(
            key_state(&snap(&[], Some("f")), Some(&fallback)),
            KeyState::Focused
        );
        assert_eq!(
            key_state(&snap(&["a"], Some("f")), Some(&fallback)),
            KeyState::Running
        );
        assert_eq!(
            key_state(&snap(&[], None), Some(&snap(&[], None))),
            KeyState::Closed
        );
    }

    /// What one `snapshot_many` call does.
    #[derive(Clone)]
    enum Reply {
        Windows(HashMap<String, Vec<WindowId>>, Option<WindowId>),
        Hang,
        Fail,
    }

    /// Records every `snapshot_many` call; answers from `replies` in turn,
    /// repeating the last one.
    struct FakeBackend {
        calls: Mutex<Vec<Vec<String>>>,
        replies: Mutex<Vec<Reply>>,
    }

    impl FakeBackend {
        fn new(replies: Vec<Reply>) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                replies: Mutex::new(replies),
            })
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }
    }

    fn windows(pairs: &[(&str, &[&str])], active: Option<&str>) -> Reply {
        Reply::Windows(
            pairs
                .iter()
                .map(|(class, ids)| {
                    (
                        (*class).to_string(),
                        ids.iter().map(|id| WindowId::new(*id)).collect(),
                    )
                })
                .collect(),
            active.map(WindowId::new),
        )
    }

    #[async_trait]
    impl WindowBackend for FakeBackend {
        async fn list_windows(&self, _: &WindowClass) -> Result<Vec<WindowId>, BackendError> {
            unreachable!("the poller only calls snapshot_many")
        }
        async fn activate(&self, _: &WindowId) -> Result<(), BackendError> {
            unreachable!()
        }
        async fn minimize(&self, _: &WindowId) -> Result<(), BackendError> {
            unreachable!()
        }
        async fn close(&self, _: &WindowId) -> Result<(), BackendError> {
            unreachable!()
        }
        async fn active_window(&self) -> Result<Option<WindowId>, BackendError> {
            unreachable!("the poller only calls snapshot_many")
        }
        async fn snapshot_many(
            &self,
            classes: &[WindowClass],
        ) -> Result<Vec<WindowSnapshot>, BackendError> {
            self.calls
                .lock()
                .unwrap()
                .push(classes.iter().map(|c| c.as_str().to_string()).collect());
            let reply = {
                let mut replies = self.replies.lock().unwrap();
                if replies.len() > 1 {
                    replies.remove(0)
                } else {
                    replies[0].clone()
                }
            };
            match reply {
                Reply::Hang => std::future::pending().await,
                Reply::Fail => Err(BackendError::CommandFailed("boom".into())),
                Reply::Windows(by_class, active) => Ok(classes
                    .iter()
                    .map(|c| WindowSnapshot {
                        windows: by_class.get(c.as_str()).cloned().unwrap_or_default(),
                        active: active.clone(),
                    })
                    .collect()),
            }
        }
    }

    #[derive(Default)]
    struct RecordingPainter {
        paints: Mutex<Vec<(String, String)>>,
    }

    impl RecordingPainter {
        fn paints(&self) -> Vec<(String, String)> {
            self.paints.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Painter for RecordingPainter {
        async fn paint(&self, instance_id: &str, image: &str) {
            self.paints
                .lock()
                .unwrap()
                .push((instance_id.to_string(), image.to_string()));
        }
    }

    const ICON: &str = "data:image/png;base64,iVBORw0KGgo=";

    fn icon() -> Arc<str> {
        ICON.into()
    }

    fn watcher(backend: &Arc<FakeBackend>, painter: &Arc<RecordingPainter>) -> Watcher {
        Watcher::new(
            Arc::clone(backend) as Arc<dyn WindowBackend>,
            Arc::clone(painter) as Arc<dyn Painter>,
        )
    }

    /// Lets the poller run for `d` of (paused) tokio time.
    async fn run_for(d: Duration) {
        tokio::time::sleep(d).await;
    }

    #[tokio::test(start_paused = true)]
    async fn nothing_is_called_while_no_key_is_tracked() {
        let backend = FakeBackend::new(vec![windows(&[], None)]);
        let painter = Arc::new(RecordingPainter::default());
        let w = watcher(&backend, &painter);
        run_for(INTERVAL * 10).await;
        assert!(backend.calls().is_empty());
        // Tracking then untracking before the poller runs leaves it idle too.
        w.track("k1", vec![class("konsole")], icon());
        w.untrack("k1");
        run_for(INTERVAL * 10).await;
        assert!(backend.calls().len() <= 1, "{:?}", backend.calls());
        let settled = backend.calls().len();
        run_for(INTERVAL * 10).await;
        assert_eq!(backend.calls().len(), settled);
    }

    #[tokio::test(start_paused = true)]
    async fn one_call_per_tick_whatever_the_number_of_keys() {
        let backend = FakeBackend::new(vec![windows(&[("konsole", &["w1"])], None)]);
        let painter = Arc::new(RecordingPainter::default());
        let w = watcher(&backend, &painter);
        w.track("k1", vec![class("konsole")], icon());
        w.track("k2", vec![class("konsole")], icon());
        w.track(
            "k3",
            vec![class("code"), class("com.visualstudio.code")],
            icon(),
        );
        run_for(INTERVAL * 3 + Duration::from_millis(100)).await;
        let calls = backend.calls();
        // An immediate tick, then one every interval.
        assert!((3..=5).contains(&calls.len()), "{calls:?}");
        for call in &calls {
            assert_eq!(call, &["code", "com.visualstudio.code", "konsole"]);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_key_is_painted_once_and_again_only_when_its_state_changes() {
        let backend = FakeBackend::new(vec![
            windows(&[("konsole", &["w1"])], None),
            windows(&[("konsole", &["w1"])], None),
            windows(&[("konsole", &["w1"])], Some("w1")),
        ]);
        let painter = Arc::new(RecordingPainter::default());
        let w = watcher(&backend, &painter);
        w.track("k1", vec![class("konsole")], icon());
        run_for(INTERVAL * 4).await;
        let paints = painter.paints();
        assert_eq!(
            paints,
            [
                ("k1".to_string(), badge::compose(ICON, KeyState::Running)),
                ("k1".to_string(), badge::compose(ICON, KeyState::Focused)),
            ]
        );
        assert!(backend.calls().len() >= 4);
    }

    #[tokio::test(start_paused = true)]
    async fn re_tracking_a_key_repaints_it_on_the_next_tick() {
        let backend = FakeBackend::new(vec![windows(&[], None)]);
        let painter = Arc::new(RecordingPainter::default());
        let w = watcher(&backend, &painter);
        w.track("k1", vec![class("konsole")], icon());
        run_for(INTERVAL * 2).await;
        assert_eq!(painter.paints().len(), 1);
        // `apply_visuals` painted the plain icon again.
        w.track("k1", vec![class("konsole")], icon());
        run_for(Duration::from_millis(10)).await;
        assert_eq!(painter.paints().len(), 2);
        assert_eq!(
            painter.paints()[1].1,
            badge::compose(ICON, KeyState::Closed)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_tick_is_abandoned_and_the_next_one_runs() {
        let backend = FakeBackend::new(vec![Reply::Hang, windows(&[("konsole", &["w1"])], None)]);
        let painter = Arc::new(RecordingPainter::default());
        let w = watcher(&backend, &painter);
        w.track("k1", vec![class("konsole")], icon());
        run_for(TICK_TIMEOUT - Duration::from_millis(10)).await;
        assert_eq!(backend.calls().len(), 1);
        assert!(painter.paints().is_empty());
        run_for(Duration::from_millis(10) + INTERVAL + Duration::from_millis(10)).await;
        assert_eq!(backend.calls().len(), 2);
        assert_eq!(
            painter.paints(),
            [("k1".to_string(), badge::compose(ICON, KeyState::Running))]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_tick_paints_nothing_and_the_poller_keeps_going() {
        let backend = FakeBackend::new(vec![Reply::Fail, windows(&[], None)]);
        let painter = Arc::new(RecordingPainter::default());
        let w = watcher(&backend, &painter);
        w.track("k1", vec![class("konsole")], icon());
        run_for(Duration::from_millis(10)).await;
        assert!(painter.paints().is_empty());
        run_for(INTERVAL).await;
        assert_eq!(
            painter.paints(),
            [("k1".to_string(), badge::compose(ICON, KeyState::Closed))]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_poller_goes_idle_when_the_last_key_disappears() {
        let backend = FakeBackend::new(vec![windows(&[], None)]);
        let painter = Arc::new(RecordingPainter::default());
        let w = watcher(&backend, &painter);
        w.track("k1", vec![class("konsole")], icon());
        w.track("k2", vec![class("code")], icon());
        run_for(INTERVAL * 2).await;
        w.untrack("k1");
        run_for(INTERVAL + Duration::from_millis(10)).await;
        assert_eq!(backend.calls().last().unwrap(), &["code"]);
        w.untrack("k2");
        assert_eq!(w.tracked(), 0);
        run_for(Duration::from_millis(10)).await;
        let idle_at = backend.calls().len();
        run_for(INTERVAL * 20).await;
        assert_eq!(backend.calls().len(), idle_at);
        // And wakes again for a new key.
        w.track("k3", vec![class("konsole")], icon());
        run_for(Duration::from_millis(10)).await;
        assert_eq!(backend.calls().len(), idle_at + 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_key_with_an_id_fallback_follows_it_when_its_class_has_no_window() {
        let backend = FakeBackend::new(vec![windows(&[("chrome-x-Default", &["w9"])], Some("w9"))]);
        let painter = Arc::new(RecordingPainter::default());
        let w = watcher(&backend, &painter);
        w.track(
            "k1",
            vec![class("crx_x"), class("chrome-x-Default")],
            icon(),
        );
        run_for(Duration::from_millis(10)).await;
        assert_eq!(
            painter.paints(),
            [("k1".to_string(), badge::compose(ICON, KeyState::Focused))]
        );
    }
}
