use crate::backend::WindowId;

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Launch,
    Activate(WindowId),
    Minimize(WindowId),
    NoOp,
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
        s.to_string()
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
        assert_eq!(decide(&windows, Some(&id("w1")), true, false), Decision::NoOp);
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
        assert_eq!(decide(&windows, Some(&id("w1")), false, true), Decision::NoOp);
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
