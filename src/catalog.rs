//! A cached list of installed apps, shared by every key.
//!
//! Scanning every `.desktop` file takes 7-58 ms; it used to run twice per key
//! appearance and once per press. The catalog scans once, on the blocking
//! pool, and reuses the result. Once the scan is older than `ttl`, a request
//! still gets the cached list at once and one rescan starts in the
//! background (stale-while-revalidate), so a press never waits for it. A
//! request waits for a scan only when there is none yet, when the property
//! inspector opens (the user may just have installed an app), or when a key
//! asks for an app id the cached list doesn't have.

use crate::apps::{AppEntry, list_installed_apps};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Serve a scan as fresh for this long; after that, serve it while a
/// background rescan runs.
pub const CATALOG_TTL: Duration = Duration::from_secs(60);
/// Minimum age before a missing app id triggers a rescan, so many keys
/// bound to an uninstalled app don't each rescan on a profile load.
const MISS_RESCAN_AFTER: Duration = Duration::from_secs(5);

type Scanner = Arc<dyn Fn() -> Vec<AppEntry> + Send + Sync>;
type Cached = Arc<Mutex<Option<(Instant, Arc<Vec<AppEntry>>)>>>;

pub struct AppCatalog {
    scanner: Scanner,
    ttl: Duration,
    miss_rescan_after: Duration,
    cached: Cached,
    /// Set while a background rescan runs, so stale reads start only one.
    revalidating: Arc<AtomicBool>,
}

/// Runs `scanner` on the blocking pool; `None` (logged) if the task failed.
async fn scan_with(scanner: Scanner) -> Option<Arc<Vec<AppEntry>>> {
    match tokio::task::spawn_blocking(move || scanner()).await {
        Ok(apps) => Some(Arc::new(apps)),
        Err(e) => {
            log::error!("installed-apps scan failed: {e}");
            None
        }
    }
}

/// Stores a scan that started at `started` unless the cache already holds
/// one that started later (scans can finish out of order). Entries are
/// stamped with their scan's start, so their age is never understated.
async fn store_if_newer(cached: &Cached, started: Instant, apps: &Arc<Vec<AppEntry>>) {
    let mut cached = cached.lock().await;
    if cached.as_ref().is_none_or(|(at, _)| *at < started) {
        *cached = Some((started, Arc::clone(apps)));
    }
}

impl AppCatalog {
    pub fn system() -> Self {
        Self::with_scanner(Arc::new(list_installed_apps), CATALOG_TTL)
    }

    fn with_scanner(scanner: Scanner, ttl: Duration) -> Self {
        Self {
            scanner,
            ttl,
            miss_rescan_after: MISS_RESCAN_AFTER,
            cached: Arc::new(Mutex::new(None)),
            revalidating: Arc::new(AtomicBool::new(false)),
        }
    }

    /// A scan for a caller that waits for it: a failed task gives an empty
    /// list (the keys then report the app as not installed).
    async fn scan(&self) -> Arc<Vec<AppEntry>> {
        scan_with(Arc::clone(&self.scanner))
            .await
            .unwrap_or_default()
    }

    /// The installed apps. Waits for a scan only when there is none yet
    /// (concurrent callers then wait for one scan rather than each starting
    /// one); a stale scan is returned as is while one background rescan
    /// replaces it.
    pub async fn all(&self) -> Arc<Vec<AppEntry>> {
        let mut cached = self.cached.lock().await;
        match cached.as_ref() {
            Some((at, apps)) if at.elapsed() < self.ttl => Arc::clone(apps),
            Some((_, apps)) => {
                let apps = Arc::clone(apps);
                drop(cached);
                self.revalidate_in_background();
                apps
            }
            None => {
                let started = Instant::now();
                let apps = self.scan().await;
                *cached = Some((started, Arc::clone(&apps)));
                apps
            }
        }
    }

    /// Starts one background rescan unless one is already running. The lock
    /// is taken only to store the result, never across the scan; a result
    /// older than what a forced `refresh` stored meanwhile is dropped, and a
    /// failed scan keeps the cached list.
    fn revalidate_in_background(&self) {
        if self.revalidating.swap(true, Ordering::AcqRel) {
            return;
        }
        let scanner = Arc::clone(&self.scanner);
        let cached = Arc::clone(&self.cached);
        let revalidating = Arc::clone(&self.revalidating);
        tokio::spawn(async move {
            let started = Instant::now();
            if let Some(apps) = scan_with(scanner).await {
                store_if_newer(&cached, started, &apps).await;
            }
            revalidating.store(false, Ordering::Release);
        });
    }

    /// Forces a rescan and waits for it. The cache stays readable (stale)
    /// meanwhile: the lock is taken only to store the result.
    pub async fn refresh(&self) -> Arc<Vec<AppEntry>> {
        let started = Instant::now();
        let apps = self.scan().await;
        store_if_newer(&self.cached, started, &apps).await;
        apps
    }

    /// The installed apps, rescanning once, synchronously, if `app_id` is
    /// missing from a scan older than a few seconds (e.g. the app was
    /// installed after the last scan).
    pub async fn containing(&self, app_id: Option<&str>) -> Arc<Vec<AppEntry>> {
        let apps = self.all().await;
        let Some(id) = app_id else {
            return apps;
        };
        if apps.iter().any(|a| a.id == id) {
            return apps;
        }
        let stale = self
            .cached
            .lock()
            .await
            .as_ref()
            .is_none_or(|(at, _)| at.elapsed() >= self.miss_rescan_after);
        if stale { self.refresh().await } else { apps }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::test_support::app;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;

    fn counting(apps: Vec<AppEntry>) -> (Arc<AtomicUsize>, Scanner) {
        let count = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&count);
        let scanner: Scanner = Arc::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
            apps.clone()
        });
        (count, scanner)
    }

    #[tokio::test]
    async fn scans_once_for_repeated_requests() {
        let (count, scanner) = counting(vec![app("a", "a")]);
        let catalog = AppCatalog::with_scanner(scanner, Duration::from_secs(60));
        for _ in 0..12 {
            assert_eq!(catalog.all().await.len(), 1);
        }
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    /// Polls until `done` holds (or fails after ~2 s), for background work.
    async fn eventually(done: impl Fn() -> bool) {
        for _ in 0..400 {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("the background rescan never finished");
    }

    /// A scanner whose first call returns at once and whose later calls each
    /// wait for one `()` on the returned sender, returning `n` apps on the
    /// n-th call.
    fn gated() -> (Arc<AtomicUsize>, mpsc::Sender<()>, Scanner) {
        let count = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = mpsc::channel::<()>();
        let rx = std::sync::Mutex::new(rx);
        let c = Arc::clone(&count);
        let scanner: Scanner = Arc::new(move || {
            let n = c.fetch_add(1, Ordering::SeqCst) + 1;
            if n > 1 {
                rx.lock().unwrap().recv().unwrap();
            }
            (0..n).map(|i| app(&format!("app{i}"), "c")).collect()
        });
        (count, tx, scanner)
    }

    #[tokio::test]
    async fn a_stale_scan_is_served_at_once_and_replaced_in_the_background() {
        let (count, scanner) = counting(vec![]);
        let catalog = AppCatalog::with_scanner(scanner, Duration::ZERO);
        catalog.all().await;
        catalog.all().await;
        // The second call didn't wait for its rescan; it ran in the background.
        eventually(|| count.load(Ordering::SeqCst) == 2).await;
    }

    #[tokio::test]
    async fn a_stale_read_does_not_wait_for_the_rescan() {
        let (count, release, scanner) = gated();
        let catalog = AppCatalog::with_scanner(scanner, Duration::ZERO);
        assert_eq!(catalog.all().await.len(), 1);
        // The rescan is blocked until `release`, yet the stale list comes back.
        assert_eq!(catalog.all().await.len(), 1);
        release.send(()).unwrap();
        eventually(|| count.load(Ordering::SeqCst) == 2).await;
        eventually(|| !catalog.revalidating.load(Ordering::Acquire)).await;
        assert_eq!(catalog.cached.lock().await.as_ref().unwrap().1.len(), 2);
    }

    #[tokio::test]
    async fn concurrent_stale_reads_start_one_background_rescan() {
        let (count, release, scanner) = gated();
        let catalog = AppCatalog::with_scanner(scanner, Duration::ZERO);
        catalog.all().await;
        for _ in 0..8 {
            assert_eq!(catalog.all().await.len(), 1);
        }
        release.send(()).unwrap();
        eventually(|| !catalog.revalidating.load(Ordering::Acquire)).await;
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_missing_app_in_an_old_enough_scan_rescans_before_returning() {
        let (count, scanner) = counting(vec![app("a", "a")]);
        let mut catalog = AppCatalog::with_scanner(scanner, Duration::from_secs(60));
        catalog.miss_rescan_after = Duration::ZERO;
        catalog.containing(Some("just-installed")).await;
        // Initial scan plus the synchronous rescan, both done on return.
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_background_rescan_does_not_overwrite_a_newer_refresh() {
        let (count, release, scanner) = gated();
        let catalog = AppCatalog::with_scanner(scanner, Duration::ZERO);
        catalog.all().await;
        catalog.all().await; // starts background scan #2, blocked
        eventually(|| count.load(Ordering::SeqCst) == 2).await;
        let refreshed = tokio::spawn({
            let catalog = Arc::new(catalog);
            let c = Arc::clone(&catalog);
            async move { (c.refresh().await.len(), catalog) }
        });
        // Scan #3 (the refresh) waits behind #2 for the next `()`. Release
        // #2 first, then #3: #3 started later, so it must end up cached.
        release.send(()).unwrap();
        release.send(()).unwrap();
        let (len, catalog) = refreshed.await.unwrap();
        eventually(|| !catalog.revalidating.load(Ordering::Acquire)).await;
        assert_eq!(len, 3);
        assert_eq!(catalog.cached.lock().await.as_ref().unwrap().1.len(), 3);
    }

    #[tokio::test]
    async fn refresh_always_rescans() {
        let (count, scanner) = counting(vec![]);
        let catalog = AppCatalog::with_scanner(scanner, Duration::from_secs(60));
        catalog.all().await;
        catalog.refresh().await;
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_known_app_never_triggers_a_rescan() {
        let (count, scanner) = counting(vec![app("a", "a")]);
        let catalog = AppCatalog::with_scanner(scanner, Duration::from_secs(60));
        catalog.containing(Some("a")).await;
        catalog.containing(Some("a")).await;
        catalog.containing(None).await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_missing_app_does_not_rescan_a_fresh_scan() {
        let (count, scanner) = counting(vec![app("a", "a")]);
        let catalog = AppCatalog::with_scanner(scanner, Duration::from_secs(60));
        for _ in 0..12 {
            catalog.containing(Some("uninstalled")).await;
        }
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_failed_background_rescan_keeps_the_cached_list() {
        let count = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&count);
        let scanner: Scanner = Arc::new(move || {
            assert!(c.fetch_add(1, Ordering::SeqCst) == 0, "scanner failed");
            vec![app("a", "a")]
        });
        let catalog = AppCatalog::with_scanner(scanner, Duration::ZERO);
        assert_eq!(catalog.all().await.len(), 1);
        catalog.all().await; // background rescan panics
        eventually(|| !catalog.revalidating.load(Ordering::Acquire)).await;
        assert_eq!(count.load(Ordering::SeqCst), 2);
        assert_eq!(catalog.all().await.len(), 1);
    }

    #[tokio::test]
    async fn a_stale_read_does_not_wait_for_a_refresh() {
        let (_count, release, scanner) = gated();
        let catalog = Arc::new(AppCatalog::with_scanner(scanner, Duration::from_secs(60)));
        catalog.all().await;
        let refreshing = tokio::spawn({
            let c = Arc::clone(&catalog);
            async move { c.refresh().await.len() }
        });
        tokio::task::yield_now().await;
        // The refresh's scan is blocked, yet a press still gets the list.
        assert_eq!(catalog.all().await.len(), 1);
        release.send(()).unwrap();
        assert_eq!(refreshing.await.unwrap(), 2);
        assert_eq!(catalog.all().await.len(), 2);
    }
}
