//! Draws a key's live state over its icon: a generated SVG that embeds the
//! icon's data URI and adds a dot (the app has a window) and a ring (one of
//! its windows is focused), or dims it (no window). `OpenDeck` renders SVG
//! with `resvg`, which decodes a nested PNG, JPEG, GIF or SVG data URI.

use crate::watch::KeyState;
use base64::{Engine, engine::general_purpose::STANDARD};

/// The canvas, in the 144 px a high-resolution key uses; SVG scales.
const SIZE: u32 = 144;
/// `OpenDeck`'s own highlight colour.
const RING_COLOUR: &str = "#F7821B";
/// Opacity of the icon of an app with no window.
const DIMMED_OPACITY: &str = "0.35";

/// The icon formats a nested `<image>` can carry: what `resvg` decodes.
/// Others (BMP, ICO, WebP) are never composed; the key keeps its plain icon.
const COMPOSABLE: &[&str] = &[
    "data:image/png;base64,",
    "data:image/svg+xml;base64,",
    "data:image/jpeg;base64,",
    "data:image/gif;base64,",
];

/// Whether `icon_uri` is a base64 data URI of a format `compose` can nest,
/// with nothing in it that could close the `href` attribute.
pub fn composable(icon_uri: &str) -> bool {
    COMPOSABLE.iter().any(|prefix| {
        icon_uri.strip_prefix(prefix).is_some_and(|data| {
            data.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
        })
    })
}

/// The key image for `state`: `icon_uri` (a data URI `composable` accepts)
/// with the state drawn over it, as a base64 SVG data URI for `set_image`.
pub fn compose(icon_uri: &str, state: KeyState) -> String {
    debug_assert!(composable(icon_uri));
    let opacity = match state {
        KeyState::Closed => format!(r#" opacity="{DIMMED_OPACITY}""#),
        KeyState::Running | KeyState::Focused => String::new(),
    };
    let ring = match state {
        KeyState::Focused => format!(
            r#"<rect x="3" y="3" width="138" height="138" rx="18" fill="none" stroke="{RING_COLOUR}" stroke-width="6"/>"#
        ),
        KeyState::Closed | KeyState::Running => String::new(),
    };
    // Top right, clear of a title at the top or bottom.
    let dot = match state {
        KeyState::Running | KeyState::Focused => {
            r##"<circle cx="124" cy="20" r="9" fill="#FFFFFF" stroke="#000000" stroke-opacity="0.6" stroke-width="2.5"/>"##
        }
        KeyState::Closed => "",
    };
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{SIZE}" height="{SIZE}" viewBox="0 0 {SIZE} {SIZE}"><image x="0" y="0" width="{SIZE}" height="{SIZE}" preserveAspectRatio="xMidYMid meet"{opacity} href="{icon_uri}"/>{ring}{dot}</svg>"#
    );
    format!("data:image/svg+xml;base64,{}", STANDARD.encode(svg))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &str = "data:image/png;base64,iVBORw0KGgo=";

    fn decoded(uri: &str) -> String {
        let data = uri
            .strip_prefix("data:image/svg+xml;base64,")
            .expect("an SVG data URI");
        String::from_utf8(STANDARD.decode(data).unwrap()).unwrap()
    }

    #[test]
    fn a_closed_app_is_dimmed_with_no_dot_or_ring() {
        let svg = decoded(&compose(PNG, KeyState::Closed));
        assert!(svg.contains(&format!(r#"href="{PNG}""#)), "{svg}");
        assert!(svg.contains(r#"opacity="0.35""#), "{svg}");
        assert!(!svg.contains("<circle"), "{svg}");
        assert!(!svg.contains("<rect"), "{svg}");
    }

    #[test]
    fn a_running_app_gets_a_dot_at_full_opacity() {
        let svg = decoded(&compose(PNG, KeyState::Running));
        assert!(svg.contains(&format!(r#"href="{PNG}""#)), "{svg}");
        assert!(!svg.contains("opacity=\"0.35\""), "{svg}");
        assert!(svg.contains("<circle"), "{svg}");
        assert!(!svg.contains("<rect"), "{svg}");
    }

    #[test]
    fn a_focused_app_gets_the_ring_and_the_dot() {
        let svg = decoded(&compose(PNG, KeyState::Focused));
        assert!(svg.contains(&format!(r#"stroke="{RING_COLOUR}""#)), "{svg}");
        assert!(svg.contains("<circle"), "{svg}");
        assert!(!svg.contains("opacity=\"0.35\""), "{svg}");
    }

    #[test]
    fn the_three_states_differ() {
        let images = [KeyState::Closed, KeyState::Running, KeyState::Focused]
            .map(|state| compose(PNG, state));
        assert_ne!(images[0], images[1]);
        assert_ne!(images[1], images[2]);
        assert_ne!(images[0], images[2]);
    }

    #[test]
    fn only_base64_data_uris_resvg_decodes_are_composable() {
        assert!(composable(PNG));
        assert!(composable("data:image/svg+xml;base64,PHN2Zz4="));
        assert!(composable("data:image/jpeg;base64,/9j/"));
        assert!(composable("data:image/gif;base64,R0lG"));
        assert!(!composable("data:image/x-icon;base64,AAAB"));
        assert!(!composable("data:image/bmp;base64,Qk0="));
        assert!(!composable("data:image/webp;base64,UklG"));
        assert!(!composable("icons/actionDefaultImage"));
        assert!(!composable(r#"data:image/png;base64,AA"/><script/>"#));
        assert!(!composable("data:image/png;base64,AA&amp;"));
    }
}
