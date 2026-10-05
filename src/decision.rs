//! Pure decision logic: which behaviour a gesture asks for, and what to do
//! with the app's windows. No I/O, so every branch is unit-tested.

use crate::backend::WindowId;

/// How a key or dial press ended. The protocol has no long-press event, so
/// the adapter times `key_down`→`key_up` itself (see `is_hold`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    Tap,
    Hold,
}

/// What holding the key does, per key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoldAction {
    SameAsTap,
    CloseAll,
}

/// The behaviour a press asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    FocusOrLaunch,
    CloseAll,
}

pub fn intent_for(gesture: Gesture, hold_action: HoldAction) -> Intent {
    match (gesture, hold_action) {
        (Gesture::Hold, HoldAction::CloseAll) => Intent::CloseAll,
        _ => Intent::FocusOrLaunch,
    }
}

/// How long a press must last to count as a hold.
pub const HOLD_THRESHOLD: std::time::Duration = std::time::Duration::from_millis(500);

pub fn is_hold(elapsed: std::time::Duration, threshold: std::time::Duration) -> bool {
    elapsed >= threshold
}

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Launch,
    Activate(WindowId),
    Minimize(WindowId),
    CloseAll(Vec<WindowId>),
    NoOp,
}

/// The close-all behaviour: close every matching window, or nothing.
pub fn decide_close_all(windows: &[WindowId]) -> Decision {
    if windows.is_empty() {
        Decision::NoOp
    } else {
        Decision::CloseAll(windows.to_vec())
    }
}

pub fn decide(
    windows: &[WindowId],
    active: Option<&WindowId>,
    cycle_windows: bool,
    minimize_when_focused: bool,
) -> Decision {
    if windows.is_empty() {
        return Decision::Launch;
    }
    match active {
        Some(active_id) if windows.iter().any(|w| w == active_id) => {
            if windows.len() == 1 {
                if minimize_when_focused {
                    Decision::Minimize(active_id.clone())
                } else {
                    Decision::NoOp
                }
            } else if cycle_windows {
                let pos = windows.iter().position(|w| w == active_id).unwrap();
                let next = &windows[(pos + 1) % windows.len()];
                Decision::Activate(next.clone())
            } else {
                Decision::NoOp
            }
        }
        _ => Decision::Activate(windows[0].clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> WindowId {
        WindowId::new(s)
    }

    #[test]
    fn only_a_hold_with_close_all_configured_closes_windows() {
        assert_eq!(
            intent_for(Gesture::Tap, HoldAction::SameAsTap),
            Intent::FocusOrLaunch
        );
        assert_eq!(
            intent_for(Gesture::Tap, HoldAction::CloseAll),
            Intent::FocusOrLaunch
        );
        assert_eq!(
            intent_for(Gesture::Hold, HoldAction::SameAsTap),
            Intent::FocusOrLaunch
        );
        assert_eq!(
            intent_for(Gesture::Hold, HoldAction::CloseAll),
            Intent::CloseAll
        );
    }

    #[test]
    fn is_hold_at_and_beyond_the_threshold_only() {
        let ms = std::time::Duration::from_millis;
        assert!(is_hold(ms(500), ms(500)));
        assert!(is_hold(ms(800), ms(500)));
        assert!(!is_hold(ms(200), ms(500)));
    }

    #[test]
    fn close_all_closes_every_window_or_does_nothing() {
        assert_eq!(decide_close_all(&[]), Decision::NoOp);
        assert_eq!(
            decide_close_all(&[id("w1"), id("w2")]),
            Decision::CloseAll(vec![id("w1"), id("w2")])
        );
    }

    #[test]
    fn no_windows_means_launch() {
        assert_eq!(decide(&[], None, true, true), Decision::Launch);
    }

    #[test]
    fn background_window_gets_activated() {
        let windows = vec![id("w1")];
        assert_eq!(
            decide(&windows, None, true, true),
            Decision::Activate(id("w1"))
        );
    }

    #[test]
    fn focused_lone_window_gets_minimized_when_enabled() {
        let windows = vec![id("w1")];
        assert_eq!(
            decide(&windows, Some(&id("w1")), true, true),
            Decision::Minimize(id("w1"))
        );
    }

    #[test]
    fn focused_lone_window_is_noop_when_minimize_disabled() {
        let windows = vec![id("w1")];
        assert_eq!(
            decide(&windows, Some(&id("w1")), true, false),
            Decision::NoOp
        );
    }

    #[test]
    fn focused_window_cycles_to_next_when_enabled() {
        let windows = vec![id("w1"), id("w2"), id("w3")];
        assert_eq!(
            decide(&windows, Some(&id("w1")), true, true),
            Decision::Activate(id("w2"))
        );
        // wraps around
        assert_eq!(
            decide(&windows, Some(&id("w3")), true, true),
            Decision::Activate(id("w1"))
        );
    }

    #[test]
    fn focused_window_is_noop_when_cycle_disabled_and_multiple_exist() {
        let windows = vec![id("w1"), id("w2")];
        assert_eq!(
            decide(&windows, Some(&id("w1")), false, true),
            Decision::NoOp
        );
    }

    #[test]
    fn active_window_belonging_to_a_different_app_activates_first_match() {
        let windows = vec![id("w1"), id("w2")];
        assert_eq!(
            decide(&windows, Some(&id("other-app-window")), true, true),
            Decision::Activate(id("w1"))
        );
    }
}
