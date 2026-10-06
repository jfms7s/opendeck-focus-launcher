//! macOS application discovery: `.app` bundles in the standard folders,
//! read from their `Contents/Info.plist`. Pure Rust and platform-neutral, so
//! the Linux test suite covers it; only macOS calls it for real.

use crate::apps::AppEntry;
use plist::{Dictionary, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// An Info.plist is a few KiB; anything this big isn't one worth parsing.
const MAX_PLIST_BYTES: u64 = 1024 * 1024;

/// Where macOS keeps applications, in search order (a bundle id found in an
/// earlier folder wins).
pub fn search_dirs(home: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = [
        "/Applications",
        "/Applications/Utilities",
        "/System/Applications",
        "/System/Applications/Utilities",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect();
    dirs.push(home.join("Applications"));
    dirs
}

/// The `.app` bundles directly in `dirs`, or one folder down (vendor
/// folders like `/Applications/Foo/Foo.app`), sorted by name. Never looks
/// inside a bundle, so helper apps aren't listed and symlink loops can't
/// recurse.
pub fn list_apps_in(dirs: &[PathBuf]) -> Vec<AppEntry> {
    let mut seen = HashSet::new();
    let mut apps: Vec<AppEntry> = dirs
        .iter()
        .flat_map(|d| bundles_in(d, 1))
        .filter_map(|b| read_bundle(&b))
        .filter(|a| seen.insert(a.id.clone()))
        .collect();
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

fn is_bundle(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "app")
}

fn bundles_in(dir: &Path, depth: u8) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    entries
        .into_iter()
        .filter(|p| p.is_dir())
        .flat_map(|p| {
            if is_bundle(&p) {
                vec![p]
            } else if depth > 0 {
                bundles_in(&p, depth - 1)
            } else {
                Vec::new()
            }
        })
        .collect()
}

fn string<'a>(dict: &'a Dictionary, key: &str) -> Option<&'a str> {
    dict.get(key)
        .and_then(Value::as_string)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Info.plist flags come as booleans, numbers or "1"/"YES"/"true" strings.
fn flag(dict: &Dictionary, key: &str) -> bool {
    match dict.get(key) {
        Some(Value::Boolean(b)) => *b,
        Some(Value::Integer(i)) => i.as_signed() == Some(1),
        Some(Value::String(s)) => {
            matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "yes" | "true")
        }
        _ => false,
    }
}

fn read_plist(path: &Path) -> Option<Dictionary> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_PLIST_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    Value::from_reader(std::io::Cursor::new(bytes))
        .ok()?
        .into_dictionary()
}

fn read_bundle(bundle: &Path) -> Option<AppEntry> {
    let dict = read_plist(&bundle.join("Contents/Info.plist"))?;
    // Menu-bar agents and background-only apps have no windows to focus
    // (the macOS counterpart of a .desktop file's NoDisplay).
    if flag(&dict, "LSUIElement") || flag(&dict, "LSBackgroundOnly") {
        return None;
    }
    let id = string(&dict, "CFBundleIdentifier")?.to_string();
    let stem = bundle.file_stem().and_then(|s| s.to_str()).unwrap_or(&id);
    let name = string(&dict, "CFBundleDisplayName")
        .or_else(|| string(&dict, "CFBundleName"))
        .unwrap_or(stem)
        .to_string();
    let icon = string(&dict, "CFBundleIconFile").and_then(|file| {
        let resources = bundle.join("Contents/Resources");
        let with_ext = if Path::new(file).extension().is_some() {
            resources.join(file)
        } else {
            resources.join(format!("{file}.icns"))
        };
        with_ext
            .is_file()
            .then(|| with_ext.to_string_lossy().into_owned())
    });
    Some(AppEntry {
        window_class: id.clone(),
        exec: format!("open -b {id}"),
        name,
        icon,
        path: bundle.to_path_buf(),
        id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes `<dir>/<name>.app` with an Info.plist made of `pairs`.
    fn app(dir: &Path, name: &str, pairs: &[(&str, Value)], binary: bool) -> PathBuf {
        let bundle = dir.join(format!("{name}.app"));
        std::fs::create_dir_all(bundle.join("Contents/Resources")).unwrap();
        let mut dict = Dictionary::new();
        for (k, v) in pairs {
            dict.insert(k.to_string(), v.clone());
        }
        let plist = bundle.join("Contents/Info.plist");
        if binary {
            Value::Dictionary(dict).to_file_binary(&plist).unwrap();
        } else {
            Value::Dictionary(dict).to_file_xml(&plist).unwrap();
        }
        bundle
    }

    fn s(v: &str) -> Value {
        Value::String(v.to_string())
    }

    #[test]
    fn reads_xml_and_binary_bundles() {
        let tmp = tempfile::tempdir().unwrap();
        let safari = app(
            tmp.path(),
            "Safari",
            &[
                ("CFBundleIdentifier", s("com.apple.Safari")),
                ("CFBundleName", s("Safari")),
                ("CFBundleIconFile", s("AppIcon")),
            ],
            false,
        );
        std::fs::write(safari.join("Contents/Resources/AppIcon.icns"), b"icns").unwrap();
        app(
            tmp.path(),
            "Calculator",
            &[
                ("CFBundleIdentifier", s("com.apple.calculator")),
                ("CFBundleDisplayName", s("Calculator")),
                ("CFBundleName", s("Calc")),
            ],
            true,
        );
        let apps = list_apps_in(&[tmp.path().to_path_buf()]);
        assert_eq!(apps.len(), 2);
        // Sorted by name, case-insensitively.
        assert_eq!(apps[0].id, "com.apple.calculator");
        assert_eq!(apps[0].name, "Calculator");
        assert_eq!(apps[0].icon, None);
        let s = &apps[1];
        assert_eq!(
            (s.id.as_str(), s.name.as_str(), s.window_class.as_str()),
            ("com.apple.Safari", "Safari", "com.apple.Safari")
        );
        assert_eq!(s.exec, "open -b com.apple.Safari");
        assert_eq!(s.path, safari);
        assert_eq!(
            s.icon.as_deref(),
            Some(
                safari
                    .join("Contents/Resources/AppIcon.icns")
                    .to_str()
                    .unwrap()
            )
        );
    }

    #[test]
    fn the_name_falls_back_to_the_bundle_file_name() {
        let tmp = tempfile::tempdir().unwrap();
        app(
            tmp.path(),
            "Some Tool",
            &[("CFBundleIdentifier", s("org.example.tool"))],
            false,
        );
        let apps = list_apps_in(&[tmp.path().to_path_buf()]);
        assert_eq!(apps[0].name, "Some Tool");
    }

    #[test]
    fn an_icon_file_name_with_its_extension_is_used_as_is() {
        let tmp = tempfile::tempdir().unwrap();
        let b = app(
            tmp.path(),
            "X",
            &[
                ("CFBundleIdentifier", s("x")),
                ("CFBundleIconFile", s("x.icns")),
            ],
            false,
        );
        std::fs::write(b.join("Contents/Resources/x.icns"), b"icns").unwrap();
        let apps = list_apps_in(&[tmp.path().to_path_buf()]);
        assert!(
            apps[0]
                .icon
                .as_deref()
                .unwrap()
                .ends_with("Resources/x.icns")
        );
    }

    #[test]
    fn agents_and_bundles_without_an_id_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        app(tmp.path(), "NoId", &[("CFBundleName", s("NoId"))], false);
        app(
            tmp.path(),
            "Menu",
            &[
                ("CFBundleIdentifier", s("menu")),
                ("LSUIElement", Value::Boolean(true)),
            ],
            false,
        );
        app(
            tmp.path(),
            "MenuStr",
            &[("CFBundleIdentifier", s("menu2")), ("LSUIElement", s("1"))],
            false,
        );
        app(
            tmp.path(),
            "Bg",
            &[
                ("CFBundleIdentifier", s("bg")),
                ("LSBackgroundOnly", Value::Boolean(true)),
            ],
            false,
        );
        app(
            tmp.path(),
            "Shown",
            &[
                ("CFBundleIdentifier", s("shown")),
                ("LSUIElement", Value::Boolean(false)),
            ],
            false,
        );
        let ids: Vec<_> = list_apps_in(&[tmp.path().to_path_buf()])
            .into_iter()
            .map(|a| a.id)
            .collect();
        assert_eq!(ids, ["shown"]);
    }

    #[test]
    fn duplicates_keep_the_first_search_folder() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let a = app(
            first.path(),
            "A",
            &[("CFBundleIdentifier", s("dup"))],
            false,
        );
        app(
            second.path(),
            "B",
            &[("CFBundleIdentifier", s("dup"))],
            false,
        );
        let apps = list_apps_in(&[first.path().to_path_buf(), second.path().to_path_buf()]);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].path, a);
    }

    #[test]
    fn looks_one_folder_deep_but_not_inside_bundles() {
        let tmp = tempfile::tempdir().unwrap();
        let vendor = tmp.path().join("Vendor");
        app(
            &vendor,
            "Nested",
            &[("CFBundleIdentifier", s("nested"))],
            false,
        );
        let outer = app(
            tmp.path(),
            "Outer",
            &[("CFBundleIdentifier", s("outer"))],
            false,
        );
        // A helper app inside another bundle is not listed.
        app(
            &outer.join("Contents/Helpers"),
            "Helper",
            &[("CFBundleIdentifier", s("helper"))],
            false,
        );
        // Two levels down is too deep.
        app(
            &tmp.path().join("a/b"),
            "Deep",
            &[("CFBundleIdentifier", s("deep"))],
            false,
        );
        let mut ids: Vec<_> = list_apps_in(&[tmp.path().to_path_buf()])
            .into_iter()
            .map(|a| a.id)
            .collect();
        ids.sort();
        assert_eq!(ids, ["nested", "outer"]);
    }

    #[test]
    fn oversized_or_broken_plists_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let big = app(
            tmp.path(),
            "Big",
            &[("CFBundleIdentifier", s("big"))],
            false,
        );
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(big.join("Contents/Info.plist"))
            .unwrap();
        f.set_len(MAX_PLIST_BYTES + 1).unwrap();
        let broken = tmp.path().join("Broken.app/Contents");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("Info.plist"), b"not a plist").unwrap();
        assert!(list_apps_in(&[tmp.path().to_path_buf()]).is_empty());
    }

    #[test]
    fn a_missing_search_folder_is_fine() {
        assert!(list_apps_in(&[PathBuf::from("/nonexistent/Applications")]).is_empty());
    }

    #[test]
    fn searches_the_standard_folders_in_order() {
        let dirs = search_dirs(Path::new("/Users/jf"));
        assert_eq!(
            dirs,
            [
                "/Applications",
                "/Applications/Utilities",
                "/System/Applications",
                "/System/Applications/Utilities",
                "/Users/jf/Applications",
            ]
            .map(PathBuf::from)
        );
    }
}
