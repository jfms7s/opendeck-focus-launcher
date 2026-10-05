//! Key icons: resolve an `Icon=` value (theme name or absolute path) to a
//! file, read it with limits, and encode it for `OpenDeck`'s `setImage`.
//!
//! Lookups go through `freedesktop-icons` (an Icon Theme spec lookup that
//! probes the theme's directories instead of walking every icon file), run
//! on the blocking pool, and are cached per icon for the life of the
//! process, so a profile load resolves each distinct icon once.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use thiserror::Error;

/// Largest icon file read. Real app icons are well under this; the cap
/// stops a bad `icon_override` (`/dev/zero`, a huge file) from exhausting
/// memory or stalling the plugin.
pub const MAX_ICON_BYTES: u64 = 2 * 1024 * 1024;

/// Pixel size asked of the icon theme (Stream Deck keys render at up to
/// 144 px on the XL/+ models); the theme picks the closest it has.
const ICON_SIZE: u16 = 144;

#[derive(Debug, Error)]
pub enum IconError {
    #[error("no icon named {0:?} in the icon theme")]
    NotFound(String),
    #[error("{0} is not a regular file")]
    NotAFile(PathBuf),
    #[error("{0} is not a supported image type (png, svg, jpg, bmp, gif, webp, ico)")]
    UnsupportedType(PathBuf),
    #[error("{0} is larger than {MAX_ICON_BYTES} bytes")]
    TooLarge(PathBuf),
    #[error("could not read {0}: {1}")]
    Read(PathBuf, std::io::Error),
}

/// MIME type for the image formats `OpenDeck` can render, by extension.
fn mime_for(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "jpg" | "jpeg" => "image/jpeg",
        "bmp" => "image/bmp",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        _ => return None,
    })
}

/// Builds the `image` string `OpenDeck`'s `setImage` expects: always a base64
/// data URI. `OpenDeck` only treats `image` as inline data when it starts with
/// `data:`; anything else is taken as a file name inside the plugin bundle,
/// so SVG must be encoded too. Only regular files of a known image type up
/// to `MAX_ICON_BYTES` are read.
pub fn build_image_payload(path: &Path) -> Result<String, IconError> {
    let mime = mime_for(path).ok_or_else(|| IconError::UnsupportedType(path.to_path_buf()))?;
    let metadata = std::fs::metadata(path).map_err(|e| IconError::Read(path.to_path_buf(), e))?;
    if !metadata.is_file() {
        return Err(IconError::NotAFile(path.to_path_buf()));
    }
    if metadata.len() > MAX_ICON_BYTES {
        return Err(IconError::TooLarge(path.to_path_buf()));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    std::fs::File::open(path)
        .and_then(|f| f.take(MAX_ICON_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|e| IconError::Read(path.to_path_buf(), e))?;
    if bytes.len() as u64 > MAX_ICON_BYTES {
        return Err(IconError::TooLarge(path.to_path_buf()));
    }
    Ok(format!("data:{mime};base64,{}", STANDARD.encode(&bytes)))
}

/// Reads `[Icons] Theme=` from a kdeglobals file's contents.
fn kde_icon_theme(kdeglobals: &str) -> Option<String> {
    let mut in_icons = false;
    for line in kdeglobals.lines().map(str::trim) {
        if line.starts_with('[') {
            in_icons = line == "[Icons]";
        } else if in_icons && let Some(theme) = line.strip_prefix("Theme=") {
            let theme = theme.trim();
            if !theme.is_empty() {
                return Some(theme.to_string());
            }
        }
    }
    None
}

fn gsettings_icon_theme() -> Option<String> {
    let output = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "icon-theme"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let theme = String::from_utf8(output.stdout).ok()?;
    let theme = theme.trim().trim_matches('\'');
    (!theme.is_empty()).then(|| theme.to_string())
}

/// The icon theme directory name to look icons up in: KDE's own setting,
/// then the GNOME/GTK one (Plasma mirrors its theme there too), then
/// `breeze` on Plasma, then `hicolor`. Never panics when a tool is missing.
fn detect_icon_theme() -> String {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    if let Some(theme) = config_home
        .and_then(|dir| std::fs::read_to_string(dir.join("kdeglobals")).ok())
        .and_then(|contents| kde_icon_theme(&contents))
    {
        return theme;
    }
    if let Some(theme) = gsettings_icon_theme() {
        return theme;
    }
    let on_kde = std::env::var("XDG_CURRENT_DESKTOP")
        .is_ok_and(|d| d.split(':').any(|p| p.eq_ignore_ascii_case("kde")));
    if on_kde { "breeze" } else { "hicolor" }.to_string()
}

fn icon_theme() -> &'static str {
    static THEME: OnceLock<String> = OnceLock::new();
    THEME.get_or_init(detect_icon_theme)
}

/// Resolves an `Icon=` value to a file: an absolute path is used as-is, a
/// name is looked up in the icon theme (with `hicolor` and pixmaps as
/// fallbacks, per the spec).
fn resolve_icon_path(icon: &str) -> Result<PathBuf, IconError> {
    let as_path = Path::new(icon);
    if as_path.is_absolute() {
        return Ok(as_path.to_path_buf());
    }
    freedesktop_icons::lookup(icon)
        .with_size(ICON_SIZE)
        .with_theme(icon_theme())
        .with_cache()
        .find()
        .ok_or_else(|| IconError::NotFound(icon.to_string()))
}

fn load_icon(icon: &str) -> Result<String, IconError> {
    build_image_payload(&resolve_icon_path(icon)?)
}

/// Per-process cache of encoded icons, keyed by the `Icon=` value. Failures
/// are cached too (and logged once), so a missing icon isn't looked up
/// again on every key appearance.
pub struct IconCache {
    entries: Mutex<HashMap<String, Option<Arc<str>>>>,
    loader: fn(&str) -> Result<String, IconError>,
}

impl IconCache {
    pub fn new() -> Self {
        Self::with_loader(load_icon)
    }

    fn with_loader(loader: fn(&str) -> Result<String, IconError>) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            loader,
        }
    }

    /// The data URI for `icon`, resolving and reading it on the blocking
    /// pool the first time.
    pub async fn data_uri(&self, icon: &str) -> Option<Arc<str>> {
        if let Some(cached) = self.lock().get(icon) {
            return cached.clone();
        }
        let loader = self.loader;
        let name = icon.to_string();
        let result = tokio::task::spawn_blocking(move || loader(&name))
            .await
            .unwrap_or_else(|e| Err(IconError::NotFound(format!("icon lookup task failed: {e}"))));
        let value = match result {
            Ok(uri) => Some(Arc::<str>::from(uri)),
            Err(e) => {
                log::warn!("no icon for {icon:?}: {e}");
                None
            }
        };
        self.lock().insert(icon.to_string(), value.clone());
        value
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Option<Arc<str>>>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Default for IconCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // A minimal valid 1x1 transparent PNG (67 bytes).
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn decode(payload: &str, prefix: &str) -> Vec<u8> {
        assert!(payload.starts_with(prefix), "got: {payload}");
        STANDARD.decode(&payload[prefix.len()..]).unwrap()
    }

    #[test]
    fn encodes_a_raster_icon_as_a_base64_data_uri() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("icon.png");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(TINY_PNG)
            .unwrap();
        let payload = build_image_payload(&path).unwrap();
        assert_eq!(decode(&payload, "data:image/png;base64,"), TINY_PNG);
    }

    #[test]
    fn encodes_an_svg_icon_as_a_base64_data_uri() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("icon.svg");
        let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";
        std::fs::write(&path, svg).unwrap();
        let payload = build_image_payload(&path).unwrap();
        assert_eq!(
            decode(&payload, "data:image/svg+xml;base64,"),
            svg.as_bytes()
        );
    }

    #[test]
    fn labels_each_supported_format_correctly() {
        for (name, mime) in [
            ("a.jpg", "image/jpeg"),
            ("a.JPEG", "image/jpeg"),
            ("a.bmp", "image/bmp"),
            ("a.gif", "image/gif"),
            ("a.webp", "image/webp"),
            ("a.ico", "image/x-icon"),
        ] {
            assert_eq!(mime_for(Path::new(name)), Some(mime), "{name}");
        }
        assert_eq!(mime_for(Path::new("a.xpm")), None);
        assert_eq!(mime_for(Path::new("noext")), None);
    }

    #[test]
    fn refuses_unsupported_types_instead_of_mislabelling_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("icon.xpm");
        std::fs::write(&path, "/* XPM */").unwrap();
        assert!(matches!(
            build_image_payload(&path),
            Err(IconError::UnsupportedType(_))
        ));
    }

    #[test]
    fn refuses_non_regular_files() {
        // A directory named like an image stands in for /dev/zero or a FIFO.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("looks-like.png");
        std::fs::create_dir(&path).unwrap();
        assert!(matches!(
            build_image_payload(&path),
            Err(IconError::NotAFile(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_character_device() {
        // /dev/zero would never finish reading without the file-type check.
        let link_dir = tempfile::tempdir().unwrap();
        let link = link_dir.path().join("zero.png");
        std::os::unix::fs::symlink("/dev/zero", &link).unwrap();
        assert!(matches!(
            build_image_payload(&link),
            Err(IconError::NotAFile(_))
        ));
    }

    #[test]
    fn refuses_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.png");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_ICON_BYTES + 1).unwrap();
        assert!(matches!(
            build_image_payload(&path),
            Err(IconError::TooLarge(_))
        ));
    }

    #[test]
    fn reports_a_missing_file() {
        let result = build_image_payload(Path::new("/nonexistent/icon.png"));
        assert!(matches!(result, Err(IconError::Read(_, _))));
    }

    #[test]
    fn reads_the_kde_icon_theme_setting() {
        let kdeglobals = "[General]\nTheme=ignored\n\n[Icons]\nTheme=breeze-dark\n";
        assert_eq!(kde_icon_theme(kdeglobals).as_deref(), Some("breeze-dark"));
        assert_eq!(kde_icon_theme("[General]\nTheme=x\n"), None);
    }

    #[test]
    fn absolute_icon_paths_skip_the_theme_lookup() {
        assert_eq!(
            resolve_icon_path("/opt/app/icon.png").unwrap(),
            PathBuf::from("/opt/app/icon.png")
        );
    }

    static LOADS: AtomicUsize = AtomicUsize::new(0);

    fn counting_loader(icon: &str) -> Result<String, IconError> {
        LOADS.fetch_add(1, Ordering::SeqCst);
        if icon == "missing" {
            Err(IconError::NotFound(icon.to_string()))
        } else {
            Ok(format!("data:image/png;base64,{icon}"))
        }
    }

    #[tokio::test]
    async fn cache_resolves_each_icon_once_including_misses() {
        let cache = IconCache::with_loader(counting_loader);
        let before = LOADS.load(Ordering::SeqCst);
        assert_eq!(
            cache.data_uri("firefox").await.as_deref(),
            Some("data:image/png;base64,firefox")
        );
        assert_eq!(
            cache.data_uri("firefox").await.as_deref(),
            Some("data:image/png;base64,firefox")
        );
        assert_eq!(cache.data_uri("missing").await, None);
        assert_eq!(cache.data_uri("missing").await, None);
        assert_eq!(LOADS.load(Ordering::SeqCst) - before, 2);
    }
}

/// Run by hand with `cargo test -- --ignored --nocapture`: resolves every
/// installed app's icon against the real icon theme and reports timings.
#[cfg(test)]
mod live_tests {
    use super::*;

    #[test]
    #[ignore = "reads the real icon theme and installed apps"]
    fn resolves_installed_app_icons() {
        let started = std::time::Instant::now();
        let apps = crate::apps::list_installed_apps();
        let scanned = started.elapsed();
        let started = std::time::Instant::now();
        let mut resolved = 0;
        let mut failed = Vec::new();
        for app in &apps {
            let Some(icon) = app.icon.as_deref() else {
                continue;
            };
            match load_icon(icon) {
                Ok(_) => resolved += 1,
                Err(e) => failed.push(format!("{}: {e}", app.id)),
            }
        }
        println!(
            "theme {:?}: {} apps scanned in {scanned:?}; {resolved} icons resolved and encoded in {:?}; {} failed",
            icon_theme(),
            apps.len(),
            started.elapsed(),
            failed.len()
        );
        for f in &failed {
            println!("  {f}");
        }
        assert!(resolved > 0);
    }
}
