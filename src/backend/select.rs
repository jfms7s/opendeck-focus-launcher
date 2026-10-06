//! Picks the window backend for the current desktop session.

use super::WindowBackend;
#[cfg(not(target_os = "macos"))]
use super::{gnome, kdotool, x11};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendKind {
    #[cfg(not(target_os = "macos"))]
    Kdotool,
    #[cfg(not(target_os = "macos"))]
    GnomeWindowCalls,
    #[cfg(not(target_os = "macos"))]
    X11,
    #[cfg(target_os = "macos")]
    MacAccessibility,
}

impl BackendKind {
    pub fn name(self) -> &'static str {
        match self {
            #[cfg(not(target_os = "macos"))]
            Self::Kdotool => "kdotool (KDE Plasma)",
            #[cfg(not(target_os = "macos"))]
            Self::GnomeWindowCalls => "Window Calls (GNOME Shell)",
            #[cfg(not(target_os = "macos"))]
            Self::X11 => "wmctrl/xdotool (X11)",
            #[cfg(target_os = "macos")]
            Self::MacAccessibility => "Accessibility (macOS)",
        }
    }

    pub fn build(self) -> Box<dyn WindowBackend> {
        match self {
            #[cfg(not(target_os = "macos"))]
            Self::Kdotool => Box::new(kdotool::KdotoolBackend),
            #[cfg(not(target_os = "macos"))]
            Self::GnomeWindowCalls => Box::new(gnome::GnomeWindowCallsBackend::new()),
            #[cfg(not(target_os = "macos"))]
            Self::X11 => Box::new(x11::X11Backend),
            #[cfg(target_os = "macos")]
            Self::MacAccessibility => Box::new(std::sync::Arc::new(
                super::macos::MacAccessibilityBackend::default(),
            )),
        }
    }
}

/// Chooses a backend from `XDG_CURRENT_DESKTOP`, `XDG_SESSION_TYPE` and
/// `DISPLAY`.
///
/// KDE and GNOME have dedicated backends that work in both X11 and Wayland
/// sessions. Anything else falls back to wmctrl/xdotool only when the session
/// is not Wayland: on a Wayland compositor (sway, Hyprland, niri, ...)
/// `DISPLAY` only points at `XWayland`, which can't see native Wayland windows,
/// so the X11 backend would report "no windows" and every press would launch
/// a duplicate. In that case no backend is chosen and each press alerts.
/// A missing or non-`wayland` session type with `DISPLAY` set (for example
/// `tty` under `startx`) is treated as X11.
#[cfg(not(target_os = "macos"))]
pub fn select_backend(
    current_desktop: Option<&str>,
    session_type: Option<&str>,
    display: Option<&str>,
) -> Option<BackendKind> {
    let desktop = current_desktop.unwrap_or_default().to_ascii_lowercase();
    if desktop.split(':').any(|d| d == "kde") {
        return Some(BackendKind::Kdotool);
    }
    if desktop.split(':').any(|d| d == "gnome") {
        return Some(BackendKind::GnomeWindowCalls);
    }
    let is_wayland = session_type.is_some_and(|s| s.eq_ignore_ascii_case("wayland"));
    if display.is_some_and(|d| !d.is_empty()) && !is_wayland {
        return Some(BackendKind::X11);
    }
    None
}

/// macOS has one window system: always the Accessibility backend.
#[cfg(target_os = "macos")]
pub fn select_backend(
    _current_desktop: Option<&str>,
    _session_type: Option<&str>,
    _display: Option<&str>,
) -> Option<BackendKind> {
    Some(BackendKind::MacAccessibility)
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn picks_kdotool_for_kde_in_either_session_type() {
        assert_eq!(
            select_backend(Some("KDE"), Some("wayland"), None),
            Some(BackendKind::Kdotool)
        );
        assert_eq!(
            select_backend(Some("KDE"), Some("x11"), Some(":0")),
            Some(BackendKind::Kdotool)
        );
    }

    #[test]
    fn picks_gnome_for_gnome_shell_including_ubuntu_style_lists() {
        assert_eq!(
            select_backend(Some("GNOME"), Some("wayland"), None),
            Some(BackendKind::GnomeWindowCalls)
        );
        assert_eq!(
            select_backend(Some("ubuntu:GNOME"), Some("wayland"), Some(":0")),
            Some(BackendKind::GnomeWindowCalls)
        );
    }

    #[test]
    fn picks_x11_fallback_on_an_x11_session() {
        assert_eq!(
            select_backend(Some("XFCE"), Some("x11"), Some(":0")),
            Some(BackendKind::X11)
        );
    }

    #[test]
    fn picks_x11_fallback_under_startx_where_the_session_type_is_tty() {
        assert_eq!(
            select_backend(None, Some("tty"), Some(":0")),
            Some(BackendKind::X11)
        );
    }

    #[test]
    fn refuses_x11_fallback_on_an_unknown_wayland_compositor() {
        assert_eq!(
            select_backend(Some("sway"), Some("wayland"), Some(":0")),
            None
        );
        assert_eq!(
            select_backend(Some("Hyprland"), Some("Wayland"), Some(":1")),
            None
        );
    }

    #[test]
    fn refuses_to_guess_with_no_recognizable_signal() {
        assert_eq!(select_backend(None, None, None), None);
        assert_eq!(select_backend(Some("XFCE"), Some("x11"), Some("")), None);
    }

    #[test]
    fn does_not_mistake_a_desktop_that_merely_contains_kde_or_gnome() {
        assert_eq!(
            select_backend(Some("NotKDEish"), Some("wayland"), None),
            None
        );
        assert_eq!(
            select_backend(Some("gnome-flashback-ish"), Some("wayland"), None),
            None
        );
    }
}
