//! A cached list of installed apps, shared by every key.
//!
//! Scanning every `.desktop` file takes 7-58 ms; it used to run twice per key
//! appearance and once per press. The catalog scans once, on the blocking
//! pool, and reuses the result until it is older than `ttl`, until the
//! property inspector opens (the user may just have installed an app), or
//! until a key asks for an app id the cached list doesn't have.

use crate::apps::{AppEntry, list_installed_apps};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Reuse a scan for this long before rescanning on the next request.
pub const CATALOG_TTL: Duration = Duration::from_secs(60);
/// Minimum age before a missing app id triggers a rescan, so many keys
/// bound to an uninstalled app don't each rescan on a profile load.
const MISS_RESCAN_AFTER: Duration = Duration::from_secs(5);

type Scanner = Arc<dyn Fn() -> Vec<AppEntry> + Send + Sync>;

pub struct AppCatalog {
    scanner: Scanner,
    ttl: Duration,
    cached: Mutex<Option<(Instant, Arc<Vec<AppEntry>>)>>,
}

impl AppCatalog {
    pub fn system() -> Self {
        Self::with_scanner(Arc::new(list_installed_apps), CATALOG_TTL)
    }

    fn with_scanner(scanner: Scanner, ttl: Duration) -> Self {
        Self {
            scanner,
            ttl,
            cached: Mutex::new(None),
        }
    }

    async fn scan(&self) -> Arc<Vec<AppEntry>> {
        let scanner = Arc::clone(&self.scanner);
        match tokio::task::spawn_blocking(move || scanner()).await {
            Ok(apps) => Arc::new(apps),
            Err(e) => {
                log::error!("installed-apps scan failed: {e}");
                Arc::new(Vec::new())
            }
        }
    }

    /// The installed apps, scanning only when there is no fresh scan.
    /// Concurrent callers wait for one scan rather than each starting one.
    pub async fn all(&self) -> Arc<Vec<AppEntry>> {
        let mut cached = self.cached.lock().await;
        if let Some((at, apps)) = cached.as_ref()
            && at.elapsed() < self.ttl
        {
            return Arc::clone(apps);
        }
        let apps = self.scan().await;
        *cached = Some((Instant::now(), Arc::clone(&apps)));
        apps
    }

    /// Forces a rescan.
    pub async fn refresh(&self) -> Arc<Vec<AppEntry>> {
        let mut cached = self.cached.lock().await;
        let apps = self.scan().await;
        *cached = Some((Instant::now(), Arc::clone(&apps)));
        apps
    }

    /// The installed apps, rescanning once if `app_id` is missing from a
    /// scan older than a few seconds (e.g. the app was installed after the
    /// last scan).
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
            .is_none_or(|(at, _)| at.elapsed() >= MISS_RESCAN_AFTER);
        if stale { self.refresh().await } else { apps }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::test_support::app;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    #[tokio::test]
    async fn rescans_once_the_scan_is_older_than_the_ttl() {
        let (count, scanner) = counting(vec![]);
        let catalog = AppCatalog::with_scanner(scanner, Duration::ZERO);
        catalog.all().await;
        catalog.all().await;
        assert_eq!(count.load(Ordering::SeqCst), 2);
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
}
