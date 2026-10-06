//! The one window-class matching rule every backend must follow.
//!
//! A `WindowClass` is the validated class a key looks for. It is the only
//! type `WindowBackend::list_windows` accepts, so an empty, whitespace-only or
//! otherwise unsafe class can never reach a backend. That matters twice over:
//! close-all-on-hold closes *every* match, and the KDE backend embeds the class
//! in a script that kdotool runs inside `KWin`.

use thiserror::Error;

/// Longest class accepted. Real `WM_CLASS` / Wayland `app_id` values are far
/// shorter; the cap only keeps garbage from being shipped to a backend.
pub const MAX_CLASS_LEN: usize = 255;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum InvalidClass {
    #[error("window class is empty")]
    Empty,
    #[error("window class is longer than {MAX_CLASS_LEN} characters")]
    TooLong,
    #[error(
        "window class {0:?} may only contain letters, digits, spaces and the characters . _ - +"
    )]
    BadCharacters(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowClass(String);

/// Characters allowed in a class. Covers every `StartupWMClass` and desktop
/// id found on the development machine (reverse-DNS ids, `crx_<id>`, snap
/// `name_app`, Steam shortcuts with spaces), and excludes everything that is
/// meaningful to a regex engine or a JavaScript template literal except `.`
/// and `+`, which `anchored_regex` escapes.
fn is_allowed(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | ' ')
}

impl WindowClass {
    /// Validates a class from settings or a `.desktop` file. Surrounding
    /// whitespace is ignored.
    pub fn parse(raw: &str) -> Result<Self, InvalidClass> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(InvalidClass::Empty);
        }
        if trimmed.len() > MAX_CLASS_LEN {
            return Err(InvalidClass::TooLong);
        }
        if !trimmed.chars().all(is_allowed) {
            return Err(InvalidClass::BadCharacters(trimmed.to_string()));
        }
        Ok(Self(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The matching contract: a window matches when its class (`WM_CLASS`
    /// class half / Wayland `app_id`) or its instance name (`WM_CLASS` instance
    /// half / `KWin` `resourceName`) equals this class, ignoring ASCII case.
    /// No substring, prefix or pattern matching: `firefox` does not match
    /// `firefox-esr`, and `st` does not match `steam`.
    pub fn matches(&self, candidate: &str) -> bool {
        candidate.eq_ignore_ascii_case(&self.0)
    }

    /// The same rule expressed as an anchored regex, for tools that only take
    /// a pattern (kdotool, which matches case-insensitively by default).
    /// Every regex metacharacter is escaped. The allowlist in `parse` already
    /// rules out backticks, `$`, `{`, quotes and backslashes, so the result
    /// is also inert inside kdotool's generated `String.raw` template.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub fn anchored_regex(&self) -> String {
        let mut pattern = String::with_capacity(self.0.len() + 8);
        pattern.push('^');
        for c in self.0.chars() {
            if "\\^$.|?*+()[]{}".contains(c) {
                pattern.push('\\');
            }
            pattern.push(c);
        }
        pattern.push('$');
        pattern
    }
}

impl std::fmt::Display for WindowClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(s: &str) -> WindowClass {
        WindowClass::parse(s).unwrap()
    }

    #[test]
    fn rejects_empty_and_whitespace_only_classes() {
        assert_eq!(WindowClass::parse(""), Err(InvalidClass::Empty));
        assert_eq!(WindowClass::parse("   "), Err(InvalidClass::Empty));
        assert_eq!(WindowClass::parse("\t\n"), Err(InvalidClass::Empty));
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(class("  firefox ").as_str(), "firefox");
    }

    #[test]
    fn accepts_real_world_class_shapes() {
        for ok in [
            "firefox",
            "org.kde.kate",
            "com.anthropic.Claude",
            "crx_abcdefghijklmnop",
            "chrome-abc-Default",
            "plex-desktop_plex-desktop",
            "ELDEN RING",
            "gnome-c++-ide",
        ] {
            assert!(WindowClass::parse(ok).is_ok(), "{ok} should be accepted");
        }
    }

    #[test]
    fn rejects_script_and_regex_injection() {
        for bad in [
            "x`${callDBus()}`",
            "a\"); evil(); (\"",
            "a$",
            "a|b",
            ".*",
            "a\\b",
            "a{1}",
            "a/b",
        ] {
            assert!(
                matches!(WindowClass::parse(bad), Err(InvalidClass::BadCharacters(_))),
                "{bad} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_overlong_classes() {
        let long = "a".repeat(MAX_CLASS_LEN + 1);
        assert_eq!(WindowClass::parse(&long), Err(InvalidClass::TooLong));
        assert!(WindowClass::parse(&"a".repeat(MAX_CLASS_LEN)).is_ok());
    }

    #[test]
    fn matching_is_exact_and_ascii_case_insensitive() {
        let firefox = class("firefox");
        assert!(firefox.matches("firefox"));
        assert!(firefox.matches("Firefox"));
        assert!(!firefox.matches("firefox-esr"));
        assert!(!firefox.matches("firefoxdeveloperedition"));
        assert!(!firefox.matches("fire"));
        assert!(!firefox.matches(""));
        assert!(!class("st").matches("steam"));
        assert!(!class("e").matches("kate"));
    }

    #[test]
    fn anchored_regex_escapes_metacharacters() {
        assert_eq!(class("org.kde.kate").anchored_regex(), r"^org\.kde\.kate$");
        assert_eq!(class("gnome-c++").anchored_regex(), r"^gnome-c\+\+$");
        assert_eq!(class("ELDEN RING").anchored_regex(), "^ELDEN RING$");
    }
}
