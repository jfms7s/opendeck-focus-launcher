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

/// What holding the key does, per key (the PI's "Hold" select).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoldAction {
    SameAsTap,
    /// Launch the app again even when it has windows.
    NewWindow,
    CloseAll,
}

/// The behaviour a press asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    FocusOrLaunch,
    /// Run the app's launch command whatever windows it has.
    LaunchAnother,
    CloseAll,
}

pub fn intent_for(gesture: Gesture, hold_action: HoldAction) -> Intent {
    match (gesture, hold_action) {
        (Gesture::Hold, HoldAction::NewWindow) => Intent::LaunchAnother,
        (Gesture::Hold, HoldAction::CloseAll) => Intent::CloseAll,
        (Gesture::Hold, HoldAction::SameAsTap) | (Gesture::Tap, _) => Intent::FocusOrLaunch,
    }
}

/// How long a press must last to count as a hold.
pub const HOLD_THRESHOLD: std::time::Duration = std::time::Duration::from_millis(500);

pub fn is_hold(elapsed: std::time::Duration, threshold: std::time::Duration) -> bool {
    elapsed >= threshold
}

/// Where a dial's window cursor lands after `ticks` detents (positive
/// clockwise) over `len` windows, wrapping both ways. `cursor` is the current
/// window's position, `None` when there is none (or it has gone): then the
/// first clockwise detent lands on the first window and the first
/// anticlockwise one on the last. `None` when there are no windows.
pub fn step(len: usize, cursor: Option<usize>, ticks: i16) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let len = len as i64;
    let ticks = i64::from(ticks);
    let target = match cursor.map(|c| c as i64).filter(|&c| c < len) {
        Some(c) => c + ticks,
        None if ticks > 0 => ticks - 1,
        None if ticks < 0 => len + ticks,
        None => 0,
    };
    Some(target.rem_euclid(len) as usize)
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
    fn a_tap_always_focuses_or_launches_and_a_hold_follows_its_action() {
        for action in [
            HoldAction::SameAsTap,
            HoldAction::NewWindow,
            HoldAction::CloseAll,
        ] {
            assert_eq!(intent_for(Gesture::Tap, action), Intent::FocusOrLaunch);
        }
        assert_eq!(
            intent_for(Gesture::Hold, HoldAction::NewWindow),
            Intent::LaunchAnother
        );
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
    fn step_moves_both_ways_and_wraps() {
        assert_eq!(step(3, Some(0), 1), Some(1));
        assert_eq!(step(3, Some(2), 1), Some(0));
        assert_eq!(step(3, Some(0), -1), Some(2));
        assert_eq!(step(3, Some(1), 5), Some(0));
        assert_eq!(step(3, Some(1), -7), Some(0));
        assert_eq!(step(3, Some(1), 0), Some(1));
    }

    #[test]
    fn step_without_a_cursor_starts_at_the_matching_end() {
        assert_eq!(step(3, None, 1), Some(0));
        assert_eq!(step(3, None, 2), Some(1));
        assert_eq!(step(3, None, -1), Some(2));
        assert_eq!(step(3, None, 0), Some(0));
        // A cursor past a list that shrank counts as no cursor.
        assert_eq!(step(2, Some(5), 1), Some(0));
    }

    #[test]
    fn step_over_no_windows_has_no_cursor() {
        assert_eq!(step(0, None, 1), None);
        assert_eq!(step(0, Some(0), -1), None);
        assert_eq!(step(1, Some(0), i16::MIN), Some(0));
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
