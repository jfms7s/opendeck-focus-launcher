//! Per-key settings (the property inspector contract) and how they resolve
//! against the selected app.

use crate::apps::AppEntry;
use crate::decision::HoldAction;
use serde::{Deserialize, Serialize};

/// What the property inspector stores for one key. The JSON field names are
/// the PI contract: `assets/propertyInspector/index.html` declares the same
/// keys in its `FIELDS` table, and `pi_contract_tests` below checks both
/// sides against `tests/fixtures/pi-settings.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusOrLaunchSettings {
    /// The selected app's desktop id.
    pub app: Option<String>,
    /// Window class to match instead of the app's own.
    pub class_override: Option<String>,
    #[serde(default = "default_true")]
    pub cycle_windows: bool,
    #[serde(default = "default_true")]
    pub minimize_when_focused: bool,
    /// The key's title instead of the app's `Name=`.
    pub name_override: Option<String>,
    /// An icon theme name (or absolute image path) instead of the app's own
    /// `Icon=`.
    pub icon_override: Option<String>,
    /// The launch command instead of the app's `Exec=`. Split with
    /// shell-style quoting and run directly, never through a shell.
    pub exec_override: Option<String>,
    /// Extra launch arguments, split the same way and appended.
    pub custom_args: Option<String>,
    /// The pre-0.7 hold setting: holding the key closes every window of the
    /// app. Read only when `hold_action` is unset (keys saved by an older PI);
    /// the PI keeps writing it (`hold_action == "close_all"`) so a downgrade
    /// keeps close-all keys working. See `hold_action()`.
    #[serde(default)]
    pub close_all_windows_on_hold: bool,
    /// What holding the key (past `HOLD_THRESHOLD`) does: `"same_as_tap"`,
    /// `"new_window"` or `"close_all"`. Read through `hold_action()`.
    #[serde(default)]
    pub hold_action: Option<String>,
    /// Only the app's windows whose title contains this text (ignoring case)
    /// count as the key's windows. Read through `title_filter()`.
    pub title_filter: Option<String>,
    /// Only the app's windows on the current virtual desktop (or on every
    /// desktop) count as the key's windows, so a window elsewhere doesn't
    /// pull the desktop over: the key launches here instead. KDE only; other
    /// backends ignore it with a log line.
    #[serde(default)]
    pub current_desktop_only: bool,
    /// The key shows whether its app is open (dimmed / dot / ring over the
    /// icon, kept current by `watch`). Linux only: macOS is frozen.
    #[serde(default = "default_true")]
    pub show_running_state: bool,
}

fn default_true() -> bool {
    true
}

// `openaction` falls back to `Default::default()` whenever the settings JSON
// fails to deserialize at all, not just when fields are missing. A derived
// `Default` would turn both "default on" options off, so this matches the
// field-level `serde(default = "default_true")` instead.
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
            hold_action: None,
            title_filter: None,
            current_desktop_only: false,
            show_running_state: default_true(),
        }
    }
}

impl FocusOrLaunchSettings {
    /// The stored `hold_action`, or, for a key saved before it existed, the
    /// old `close_all_windows_on_hold` boolean. An unknown value (e.g. from a
    /// newer PI) falls back to the safe `SameAsTap`.
    pub fn hold_action(&self) -> HoldAction {
        match non_empty(self.hold_action.as_deref()) {
            Some("same_as_tap") => HoldAction::SameAsTap,
            Some("new_window") => HoldAction::NewWindow,
            Some("close_all") => HoldAction::CloseAll,
            Some(other) => {
                log::warn!("unknown hold action {other:?}, treating it as same as tap");
                HoldAction::SameAsTap
            }
            None if self.close_all_windows_on_hold => HoldAction::CloseAll,
            None => HoldAction::SameAsTap,
        }
    }

    /// The window title filter, trimmed; `None` when unset or blank.
    pub fn title_filter(&self) -> Option<&str> {
        non_empty(self.title_filter.as_deref())
    }

    /// The selected app id, if one is set.
    pub fn app_id(&self) -> Option<&str> {
        non_empty(self.app.as_deref())
    }
}

/// An optional text setting with surrounding whitespace removed, `None` when
/// unset or blank. A cleared PI text field sends `null`, but a field holding
/// only spaces would otherwise count as "set": as a class override that used
/// to mean "match every window".
pub fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// Finds the selected app in the installed-apps list.
pub fn find_app<'a>(
    settings: &FocusOrLaunchSettings,
    apps: &'a [AppEntry],
) -> Option<&'a AppEntry> {
    let id = settings.app_id()?;
    apps.iter().find(|a| a.id == id)
}

/// The key's title: `name_override` if set, else the app's own `Name=`.
pub fn resolve_display_name<'a>(
    settings: &'a FocusOrLaunchSettings,
    entry: &'a AppEntry,
) -> &'a str {
    non_empty(settings.name_override.as_deref()).unwrap_or(&entry.name)
}

/// The `Icon=`-style value to show: `icon_override` if set, else the app's
/// own `Icon=`.
pub fn resolve_icon<'a>(
    settings: &'a FocusOrLaunchSettings,
    entry: &'a AppEntry,
) -> Option<&'a str> {
    non_empty(settings.icon_override.as_deref()).or(entry.icon.as_deref())
}

/// The class to look for: `class_override` if set, else the app's own.
pub fn resolve_class<'a>(settings: &'a FocusOrLaunchSettings, entry: &'a AppEntry) -> &'a str {
    non_empty(settings.class_override.as_deref()).unwrap_or(&entry.window_class)
}

/// The class to retry with when the app's own class matches no windows: the
/// desktop entry's id, but only when no `class_override` is set (a
/// user-typed override that finds nothing is respected as-is) and the id
/// differs from the class. Some apps' running window class differs from
/// what their `.desktop` file declares, e.g. Chrome PWAs report the desktop
/// id (`chrome-<ext-id>-Default`) as their Wayland `app_id` rather than the
/// X11-era `StartupWMClass=crx_<ext-id>` they ship. Apps without a
/// `StartupWMClass` (e.g. the Plex snap) already search by id, so the
/// fallback has nothing to add for them; they need a `class_override`.
pub fn id_fallback_class<'a>(
    settings: &FocusOrLaunchSettings,
    entry: &'a AppEntry,
) -> Option<&'a str> {
    let overridden = non_empty(settings.class_override.as_deref()).is_some();
    (!overridden && !entry.window_class.eq_ignore_ascii_case(&entry.id))
        .then_some(entry.id.as_str())
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub fn app(id: &str, class: &str) -> AppEntry {
        AppEntry {
            id: id.to_string(),
            name: id.to_string(),
            window_class: class.to_string(),
            exec: format!("{id}-binary"),
            icon: Some(format!("{id}-icon")),
            path: std::path::PathBuf::from(format!("/usr/share/applications/{id}.desktop")),
        }
    }

    pub fn settings(app: &str, cycle: bool, minimize: bool) -> FocusOrLaunchSettings {
        FocusOrLaunchSettings {
            app: Some(app.to_string()),
            cycle_windows: cycle,
            minimize_when_focused: minimize,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{app, settings};
    use super::*;

    #[test]
    fn name_override_wins_when_set() {
        let mut s = settings("org.mozilla.firefox", true, true);
        s.name_override = Some("Browser".to_string());
        let entry = app("org.mozilla.firefox", "firefox");
        assert_eq!(resolve_display_name(&s, &entry), "Browser");
    }

    #[test]
    fn name_falls_back_to_the_apps_own_name_when_no_override() {
        let s = settings("org.mozilla.firefox", true, true);
        let entry = app("org.mozilla.firefox", "firefox");
        assert_eq!(resolve_display_name(&s, &entry), entry.name);
    }

    #[test]
    fn icon_override_wins_when_set() {
        let mut s = settings("org.mozilla.firefox", true, true);
        s.icon_override = Some("firefox-nightly".to_string());
        let entry = app("org.mozilla.firefox", "firefox");
        assert_eq!(resolve_icon(&s, &entry), Some("firefox-nightly"));
    }

    #[test]
    fn icon_falls_back_to_the_apps_own_icon_when_no_override() {
        let s = settings("org.mozilla.firefox", true, true);
        let entry = app("org.mozilla.firefox", "firefox");
        assert_eq!(resolve_icon(&s, &entry), entry.icon.as_deref());
    }

    #[test]
    fn class_override_wins_when_set() {
        let mut s = settings("org.mozilla.firefox", true, true);
        s.class_override = Some("Navigator".to_string());
        let entry = app("org.mozilla.firefox", "firefox");
        assert_eq!(resolve_class(&s, &entry), "Navigator");
    }

    #[test]
    fn empty_and_whitespace_overrides_are_treated_as_unset() {
        for blank in ["", "  ", "\t"] {
            let mut s = settings("org.mozilla.firefox", true, true);
            s.class_override = Some(blank.to_string());
            s.name_override = Some(blank.to_string());
            s.icon_override = Some(blank.to_string());
            s.exec_override = Some(blank.to_string());
            s.custom_args = Some(blank.to_string());
            s.title_filter = Some(blank.to_string());
            let entry = app("org.mozilla.firefox", "firefox");

            assert_eq!(resolve_class(&s, &entry), "firefox", "{blank:?}");
            assert_eq!(resolve_display_name(&s, &entry), entry.name);
            assert_eq!(resolve_icon(&s, &entry), entry.icon.as_deref());
            assert_eq!(non_empty(s.exec_override.as_deref()), None);
            assert_eq!(non_empty(s.custom_args.as_deref()), None);
            assert_eq!(s.title_filter(), None);
            assert_eq!(id_fallback_class(&s, &entry), Some("org.mozilla.firefox"));
        }
    }

    #[test]
    fn overrides_are_trimmed() {
        let mut s = settings("org.mozilla.firefox", true, true);
        s.class_override = Some("  Navigator ".to_string());
        assert_eq!(
            resolve_class(&s, &app("org.mozilla.firefox", "firefox")),
            "Navigator"
        );
    }

    #[test]
    fn the_title_filter_is_trimmed() {
        let mut s = settings("org.kde.konsole", true, true);
        s.title_filter = Some("  homelab ".to_string());
        assert_eq!(s.title_filter(), Some("homelab"));
    }

    #[test]
    fn finds_the_selected_app_or_nothing() {
        let apps = vec![app("org.mozilla.firefox", "firefox")];
        assert!(find_app(&settings("org.mozilla.firefox", true, true), &apps).is_some());
        assert!(find_app(&settings("uninstalled.app", true, true), &apps).is_none());
        assert!(find_app(&FocusOrLaunchSettings::default(), &apps).is_none());
    }

    #[test]
    fn id_fallback_offered_when_default_class_differs_from_entry_id() {
        let entry = app("chrome-abc-Default", "crx_abc");
        let s = settings("chrome-abc-Default", true, true);
        assert_eq!(id_fallback_class(&s, &entry), Some("chrome-abc-Default"));
    }

    #[test]
    fn id_fallback_not_offered_when_class_and_id_already_match() {
        // No StartupWMClass (e.g. the Plex snap): the class already is the id.
        let entry = app("plex-desktop_plex-desktop", "plex-desktop_plex-desktop");
        let s = settings("plex-desktop_plex-desktop", true, true);
        assert_eq!(id_fallback_class(&s, &entry), None);
    }

    #[test]
    fn id_fallback_not_offered_when_class_was_overridden() {
        let entry = app("chrome-abc-Default", "crx_abc");
        let mut s = settings("chrome-abc-Default", true, true);
        s.class_override = Some("Navigator".to_string());
        assert_eq!(id_fallback_class(&s, &entry), None);
    }

    #[test]
    fn hold_action_follows_the_stored_boolean_when_unset() {
        let mut s = FocusOrLaunchSettings::default();
        assert_eq!(s.hold_action(), HoldAction::SameAsTap);
        s.close_all_windows_on_hold = true;
        assert_eq!(s.hold_action(), HoldAction::CloseAll);
        s.hold_action = Some("  ".to_string());
        assert_eq!(s.hold_action(), HoldAction::CloseAll);
    }

    #[test]
    fn the_stored_hold_action_wins_over_the_boolean() {
        for (stored, expected) in [
            ("same_as_tap", HoldAction::SameAsTap),
            ("new_window", HoldAction::NewWindow),
            ("close_all", HoldAction::CloseAll),
        ] {
            for legacy in [false, true] {
                let s = FocusOrLaunchSettings {
                    hold_action: Some(stored.to_string()),
                    close_all_windows_on_hold: legacy,
                    ..Default::default()
                };
                assert_eq!(s.hold_action(), expected, "{stored} with {legacy}");
            }
        }
    }

    #[test]
    fn an_unknown_hold_action_is_same_as_tap() {
        let s = FocusOrLaunchSettings {
            hold_action: Some("bring_here".to_string()),
            close_all_windows_on_hold: true,
            ..Default::default()
        };
        assert_eq!(s.hold_action(), HoldAction::SameAsTap);
    }

    #[test]
    fn default_matches_missing_key_deserialization() {
        // `openaction` falls back to `Default::default()` when settings JSON
        // fails to deserialize at all; that must equal deserializing `{}`.
        let from_missing_keys: FocusOrLaunchSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(from_missing_keys, FocusOrLaunchSettings::default());
        assert!(from_missing_keys.cycle_windows);
        assert!(from_missing_keys.minimize_when_focused);
        assert!(from_missing_keys.show_running_state);
    }
}

/// Checks the Rust settings struct, the PI's `FIELDS` table and a
/// hand-written `setSettings` payload (in the shape `sendSettings()` sends)
/// all agree on the same keys, and that settings saved by an older PI still
/// load.
#[cfg(test)]
mod pi_contract_tests {
    use super::*;
    use std::collections::BTreeSet;

    /// The shape `sendSettings()` in the property inspector sends: every
    /// field present, `null` for blank text fields.
    const PI_PAYLOAD: &str = include_str!("../tests/fixtures/pi-settings.json");
    const PI_HTML: &str = include_str!("../assets/propertyInspector/index.html");

    fn rust_keys() -> BTreeSet<String> {
        let value = serde_json::to_value(FocusOrLaunchSettings::default()).unwrap();
        value.as_object().unwrap().keys().cloned().collect()
    }

    /// The `key: "..."` entries of the PI's `FIELDS` table.
    fn pi_field_keys() -> BTreeSet<String> {
        let start = PI_HTML
            .find("const FIELDS = [")
            .expect("FIELDS table in index.html");
        let end = start + PI_HTML[start..].find("];").expect("end of FIELDS table");
        PI_HTML[start..end]
            .split("key: \"")
            .skip(1)
            .map(|rest| rest.split('"').next().unwrap().to_string())
            .collect()
    }

    #[test]
    fn the_pi_field_table_declares_exactly_the_rust_fields() {
        assert_eq!(pi_field_keys(), rust_keys());
    }

    /// Each `FIELDS` entry's `key` and its `def:` value, parsed as JSON.
    fn pi_field_defaults() -> Vec<(String, serde_json::Value)> {
        let start = PI_HTML
            .find("const FIELDS = [")
            .expect("FIELDS table in index.html");
        let end = start + PI_HTML[start..].find("];").expect("end of FIELDS table");
        PI_HTML[start..end]
            .split("key: \"")
            .skip(1)
            .map(|rest| {
                let key = rest.split('"').next().unwrap().to_string();
                let def = rest
                    .split("def: ")
                    .nth(1)
                    .unwrap_or_else(|| panic!("{key} has no def"));
                let def = def.split([',', '}']).next().unwrap().trim();
                let def = serde_json::from_str(def)
                    .unwrap_or_else(|e| panic!("{key}: def {def:?} is not JSON: {e}"));
                (key, def)
            })
            .collect()
    }

    /// The PI's `def` for each field is the Rust default. `hold_action` is
    /// stored as `null` by Rust and `"same_as_tap"` by the PI: both must
    /// resolve to the same hold action.
    #[test]
    fn the_pi_defaults_are_the_rust_defaults() {
        let rust = FocusOrLaunchSettings::default();
        let rust_value = serde_json::to_value(&rust).unwrap();
        let defaults = pi_field_defaults();
        assert_eq!(defaults.len(), rust_keys().len());
        for (key, def) in defaults {
            if key == "hold_action" {
                let pi = FocusOrLaunchSettings {
                    hold_action: def.as_str().map(str::to_string),
                    ..FocusOrLaunchSettings::default()
                };
                assert_eq!(pi.hold_action(), rust.hold_action(), "{key}");
            } else {
                assert_eq!(def, rust_value[&key], "{key}");
            }
        }
    }

    #[test]
    fn a_pi_payload_has_exactly_the_rust_fields_and_round_trips() {
        let value: serde_json::Value = serde_json::from_str(PI_PAYLOAD).unwrap();
        let payload_keys: BTreeSet<String> = value.as_object().unwrap().keys().cloned().collect();
        assert_eq!(payload_keys, rust_keys());

        let settings: FocusOrLaunchSettings = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(settings.app.as_deref(), Some("org.mozilla.firefox"));
        assert_eq!(settings.class_override, None);
        assert!(!settings.cycle_windows);
        assert!(settings.minimize_when_focused);
        assert!(settings.close_all_windows_on_hold);
        assert_eq!(settings.hold_action(), HoldAction::CloseAll);
        assert_eq!(settings.custom_args.as_deref(), Some("--new-window"));
        assert_eq!(settings.title_filter(), Some("grafana"));
        assert!(settings.current_desktop_only);
        assert!(!settings.show_running_state);
        assert_eq!(serde_json::to_value(&settings).unwrap(), value);
    }

    /// Hand-written in the eight-key shape an older PI saved (before
    /// `close_all_windows_on_hold` and `title_filter` existed).
    const LEGACY_PAYLOAD: &str = include_str!("../tests/fixtures/pi-settings-legacy.json");

    #[test]
    fn settings_saved_by_an_older_pi_still_load() {
        let settings: FocusOrLaunchSettings = serde_json::from_str(LEGACY_PAYLOAD).unwrap();
        assert_eq!(settings.app.as_deref(), Some("plex-desktop_plex-desktop"));
        assert_eq!(settings.class_override.as_deref(), Some("plex"));
        assert!(settings.cycle_windows);
        assert!(settings.minimize_when_focused);
        assert!(!settings.close_all_windows_on_hold);
        assert_eq!(settings.hold_action(), HoldAction::SameAsTap);
        assert_eq!(settings.title_filter(), None);
        assert!(!settings.current_desktop_only);
        assert!(settings.show_running_state);
    }

    /// Hand-written in the nine-key shape v0.6 saved for a close-all key
    /// (before `hold_action` existed).
    #[test]
    fn a_close_all_key_saved_by_v0_6_still_closes_all() {
        let settings: FocusOrLaunchSettings = serde_json::from_str(
            r#"{"app": "org.mozilla.firefox", "class_override": null, "cycle_windows": true,
                "minimize_when_focused": true, "name_override": null, "icon_override": null,
                "exec_override": null, "custom_args": null, "close_all_windows_on_hold": true,
                "title_filter": null}"#,
        )
        .unwrap();
        assert_eq!(settings.hold_action, None);
        assert_eq!(settings.hold_action(), HoldAction::CloseAll);
    }
}
