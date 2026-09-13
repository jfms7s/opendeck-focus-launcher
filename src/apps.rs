use freedesktop_desktop_entry::{DesktopEntry, Iter, default_paths};
use std::collections::HashSet;

#[derive(Clone, serde::Serialize)]
pub struct AppEntry {
    pub id: String,
    pub name: String,
    pub window_class: String,
    pub exec: String,
    /// The `.desktop` file's own absolute path - shown read-only in the
    /// property inspector (matching what the reference Launch App plugin
    /// persists as its whole `settings.app`) and used to resolve the app's
    /// own icon via `tux_icons::IconFetcher::get_icon_path_from_desktop`.
    pub path: std::path::PathBuf,
}

/// Strips the standard Exec field placeholders (%f, %F, %u, %U, %d, %D, %n, %N,
/// %i, %c, %k, %v, %m) that a launcher is expected to fill in but this plugin
/// never receives real values for.
pub fn strip_exec_placeholders(exec: &str) -> String {
    let placeholders = [
        "%f", "%F", "%u", "%U", "%d", "%D", "%n", "%N", "%i", "%c", "%k", "%v", "%m",
    ];
    let mut result = exec.to_string();
    for p in placeholders {
        result = result.replace(p, "");
    }
    result.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The window class to search for: an explicit StartupWMClass if the entry
/// declares one, else the desktop file's own id (the common fallback for
/// entries that don't set StartupWMClass).
pub fn resolve_window_class(entry_id: &str, startup_wm_class: Option<&str>) -> String {
    startup_wm_class
        .filter(|c| !c.is_empty())
        .unwrap_or(entry_id)
        .to_string()
}

pub fn list_installed_apps() -> Vec<AppEntry> {
    list_installed_apps_from_paths(default_paths())
}

fn list_installed_apps_from_paths<I: IntoIterator<Item = std::path::PathBuf>>(
    paths: I,
) -> Vec<AppEntry> {
    let locales = freedesktop_desktop_entry::get_languages_from_env();
    // NOTE (deviation from the brief's exact code): `freedesktop-desktop-entry`
    // 0.8's `Iter::new` takes an `Iterator<Item = PathBuf>`, not an
    // `IntoIterator`, so we call `.into_iter()` on `paths` ourselves. And
    // `Iter::entries`/`DesktopEntry::name` take `Option<&[L]>` / `&[L]`
    // (a slice), not a reference to the `Vec` directly, so we pass
    // `locales.as_slice()` rather than `&locales` to avoid relying on
    // deref coercion through the `Option` wrapper.
    // XDG data dirs can legitimately list the same app id twice (e.g. a
    // Flatpak override in ~/.local/share/applications shadowing the system
    // copy in /usr/share/applications) - `default_paths()`/`Iter` walk user
    // dirs before system dirs, i.e. in XDG precedence order, so keeping the
    // first occurrence of each id and dropping later ones is correct.
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut apps: Vec<AppEntry> = Iter::new(paths.into_iter())
        .entries(Some(locales.as_slice()))
        .filter(|entry: &DesktopEntry| {
            // `Type=Link`/`Type=Directory` entries have no `Exec=`, so
            // surfacing them in the dropdown would silently no-op on select.
            !entry.no_display() && !entry.hidden() && entry.type_() == Some("Application")
        })
        .filter(|entry| seen_ids.insert(entry.id().to_string()))
        .map(|entry| AppEntry {
            id: entry.id().to_string(),
            name: entry
                .name(locales.as_slice())
                .map(|n| n.to_string())
                .unwrap_or_else(|| entry.id().to_string()),
            window_class: resolve_window_class(entry.id(), entry.startup_wm_class()),
            exec: entry.exec().unwrap_or_default().to_string(),
            path: entry.path.clone(),
        })
        .collect();
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

/// Detects Flatpak sandboxing the same way `oadesktopentry` does, so a
/// sandboxed plugin can still launch a host app.
pub async fn launch_app(exec: &str, args: Option<&str>) -> Result<(), std::io::Error> {
    let mut command_line = strip_exec_placeholders(exec);
    if let Some(extra) = args {
        command_line.push(' ');
        command_line.push_str(extra);
    }

    let mut cmd = if std::env::var("FLATPAK_ID").is_ok() {
        let mut c = tokio::process::Command::new("flatpak-spawn");
        c.arg("--host").arg("sh").arg("-c").arg(command_line);
        c
    } else {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(command_line);
        c
    };
    cmd.spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn strips_every_known_placeholder() {
        assert_eq!(
            strip_exec_placeholders("firefox %u --new-window %F"),
            "firefox --new-window"
        );
    }

    #[test]
    fn leaves_plain_exec_untouched() {
        assert_eq!(strip_exec_placeholders("kate"), "kate");
    }

    #[test]
    fn resolve_window_class_prefers_startup_wm_class() {
        assert_eq!(
            resolve_window_class("org.mozilla.firefox", Some("firefox")),
            "firefox"
        );
    }

    #[test]
    fn resolve_window_class_falls_back_to_entry_id() {
        assert_eq!(resolve_window_class("org.kde.kate", None), "org.kde.kate");
        assert_eq!(
            resolve_window_class("org.kde.kate", Some("")),
            "org.kde.kate"
        );
    }

    #[test]
    fn discovers_a_fixture_desktop_entry() {
        let dir = tempfile::tempdir().unwrap();
        let apps_dir = dir.path().join("applications");
        std::fs::create_dir_all(&apps_dir).unwrap();
        let mut file = std::fs::File::create(apps_dir.join("test-app.desktop")).unwrap();
        writeln!(
            file,
            "[Desktop Entry]\nType=Application\nName=Test App\nExec=test-app %u\nStartupWMClass=testapp\n"
        )
        .unwrap();

        let apps = list_installed_apps_from_paths(vec![dir.path().to_path_buf()]);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "Test App");
        assert_eq!(apps[0].window_class, "testapp");
        assert_eq!(apps[0].exec, "test-app %u");
        assert_eq!(apps[0].path, apps_dir.join("test-app.desktop"));
    }

    #[test]
    fn hides_nodisplay_entries() {
        let dir = tempfile::tempdir().unwrap();
        let apps_dir = dir.path().join("applications");
        std::fs::create_dir_all(&apps_dir).unwrap();
        let mut file = std::fs::File::create(apps_dir.join("hidden-app.desktop")).unwrap();
        writeln!(
            file,
            "[Desktop Entry]\nType=Application\nName=Hidden App\nExec=hidden-app\nNoDisplay=true\n"
        )
        .unwrap();

        let apps = list_installed_apps_from_paths(vec![dir.path().to_path_buf()]);
        assert!(apps.is_empty());
    }

    #[test]
    fn hides_non_application_entries() {
        let dir = tempfile::tempdir().unwrap();
        let apps_dir = dir.path().join("applications");
        std::fs::create_dir_all(&apps_dir).unwrap();
        let mut file = std::fs::File::create(apps_dir.join("a-link.desktop")).unwrap();
        writeln!(
            file,
            "[Desktop Entry]\nType=Link\nName=Some Link\nURL=https://example.com\n"
        )
        .unwrap();

        let apps = list_installed_apps_from_paths(vec![dir.path().to_path_buf()]);
        assert!(apps.is_empty());
    }

    #[test]
    fn dedups_an_id_present_in_two_search_paths_keeping_the_first() {
        let user_dir = tempfile::tempdir().unwrap();
        let user_apps = user_dir.path().join("applications");
        std::fs::create_dir_all(&user_apps).unwrap();
        let mut user_file = std::fs::File::create(user_apps.join("dup-app.desktop")).unwrap();
        writeln!(
            user_file,
            "[Desktop Entry]\nType=Application\nName=User Override\nExec=dup-app\n"
        )
        .unwrap();

        let system_dir = tempfile::tempdir().unwrap();
        let system_apps = system_dir.path().join("applications");
        std::fs::create_dir_all(&system_apps).unwrap();
        let mut system_file = std::fs::File::create(system_apps.join("dup-app.desktop")).unwrap();
        writeln!(
            system_file,
            "[Desktop Entry]\nType=Application\nName=System Copy\nExec=dup-app\n"
        )
        .unwrap();

        // Search the user dir first, matching real XDG precedence order.
        let apps = list_installed_apps_from_paths(vec![
            user_dir.path().to_path_buf(),
            system_dir.path().to_path_buf(),
        ]);

        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "User Override");
    }
}
