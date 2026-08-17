//! Pure logic for `touchpad-toggle-daemon`, kept separate from `main.rs` so
//! it can be unit-tested without real udev devices, an X server, or an
//! `xinput` binary on `PATH`.

/// Tracks how many external mice are currently connected and reports
/// whether each add/remove event crosses the 0/1 boundary that should
/// toggle the touchpad, so the daemon behaves correctly with multiple mice
/// plugged in at once (e.g. unplugging one of two leaves the touchpad
/// disabled).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MouseCounter(usize);

impl MouseCounter {
    /// Creates a counter seeded with `initial` already-connected mice, for
    /// startup enumeration.
    #[inline]
    #[must_use]
    pub fn new(initial: usize) -> Self {
        Self(initial)
    }

    /// The number of mice currently tracked as connected.
    #[inline]
    #[must_use]
    pub fn count(self) -> usize {
        self.0
    }

    /// Registers a connected external mouse. Returns `true` exactly when
    /// this is the first currently-connected mouse, i.e. when the touchpad
    /// should now be disabled.
    #[inline]
    pub fn connect(&mut self) -> bool {
        self.0 += 1;
        self.0 == 1
    }

    /// Registers a disconnected external mouse. Returns `true` exactly when
    /// no mice remain, i.e. when the touchpad should now be re-enabled. A
    /// disconnect with no mice currently tracked is treated as spurious and
    /// ignored, rather than underflowing or re-triggering the transition.
    #[inline]
    pub fn disconnect(&mut self) -> bool {
        if self.0 == 0 {
            return false;
        }
        self.0 -= 1;
        self.0 == 0
    }
}

/// Whether a udev "input" subsystem device counts as an external mouse.
///
/// `has_devnode` filters out the parent "input" class device, which shares
/// the same udev database properties as its child "eventN" nodes and would
/// otherwise cause a single physical mouse to be counted more than once.
/// `id_input_mouse` is the device's `ID_INPUT_MOUSE` udev property, used
/// instead of matching on device name, vendor ID, or product ID so any
/// mouse is detected generically.
#[inline]
#[must_use]
pub fn is_mouse_event_device(has_devnode: bool, id_input_mouse: Option<&str>) -> bool {
    has_devnode && id_input_mouse == Some("1")
}

/// The `xinput` subcommand for toggling a device's enabled state.
#[inline]
#[must_use]
pub fn xinput_action(enabled: bool) -> &'static str {
    if enabled {
        "enable"
    } else {
        "disable"
    }
}

/// Parses `xinput list --name-only` output and returns the first device
/// name containing "touchpad" (case-insensitive), trimmed of surrounding
/// whitespace.
#[must_use]
pub fn find_touchpad_name(xinput_list_output: &str) -> Option<String> {
    xinput_list_output
        .lines()
        .map(str::trim)
        .find(|line| contains_ignore_ascii_case(line, "touchpad"))
        .map(str::to_owned)
}

/// ASCII case-insensitive substring search that avoids the heap allocation
/// `str::to_lowercase` would require for every candidate line.
#[inline]
fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    let (haystack, needle) = (haystack.as_bytes(), needle.as_bytes());
    needle.is_empty()
        || haystack
            .windows(needle.len())
            .any(|w| w.eq_ignore_ascii_case(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    mod mouse_counter {
        use super::*;

        #[test]
        fn starts_at_zero_by_default() {
            assert_eq!(MouseCounter::default().count(), 0);
        }

        #[test]
        fn new_seeds_initial_count() {
            assert_eq!(MouseCounter::new(3).count(), 3);
        }

        #[test]
        fn first_connect_triggers_disable() {
            let mut mice = MouseCounter::new(0);
            assert!(mice.connect());
            assert_eq!(mice.count(), 1);
        }

        #[test]
        fn second_connect_does_not_retrigger_disable() {
            let mut mice = MouseCounter::new(0);
            assert!(mice.connect());
            assert!(!mice.connect());
            assert_eq!(mice.count(), 2);
        }

        #[test]
        fn disconnect_down_to_nonzero_does_not_trigger_enable() {
            let mut mice = MouseCounter::new(2);
            assert!(!mice.disconnect());
            assert_eq!(mice.count(), 1);
        }

        #[test]
        fn last_disconnect_triggers_enable() {
            let mut mice = MouseCounter::new(1);
            assert!(mice.disconnect());
            assert_eq!(mice.count(), 0);
        }

        #[test]
        fn disconnect_on_empty_counter_is_spurious_and_ignored() {
            let mut mice = MouseCounter::new(0);
            assert!(!mice.disconnect());
            assert_eq!(mice.count(), 0);
        }

        #[test]
        fn two_mice_one_unplugged_leaves_touchpad_disabled() {
            // Regression test for the multi-mouse requirement: plugging in
            // a second mouse then unplugging one must not re-enable the
            // touchpad while the other is still connected.
            let mut mice = MouseCounter::new(0);
            assert!(mice.connect()); // mouse A: disable
            assert!(!mice.connect()); // mouse B: no-op, still disabled
            assert!(!mice.disconnect()); // unplug A: still one left, no-op
            assert_eq!(mice.count(), 1);
            assert!(mice.disconnect()); // unplug B: now re-enable
            assert_eq!(mice.count(), 0);
        }

        #[test]
        fn full_lifecycle_sequence_matches_expected_transitions() {
            let mut mice = MouseCounter::new(0);
            let events = [true, true, true, false, false, false];
            let expected_transitions = [true, false, false, false, false, true];
            for (connect, expect_transition) in events.into_iter().zip(expected_transitions) {
                let transitioned = if connect {
                    mice.connect()
                } else {
                    mice.disconnect()
                };
                assert_eq!(transitioned, expect_transition);
            }
            assert_eq!(mice.count(), 0);
        }
    }

    mod mouse_detection {
        use super::*;

        #[test]
        fn devnode_with_mouse_property_is_a_mouse() {
            assert!(is_mouse_event_device(true, Some("1")));
        }

        #[test]
        fn devnode_without_mouse_property_is_not_a_mouse() {
            assert!(!is_mouse_event_device(true, None));
        }

        #[test]
        fn devnode_with_mouse_property_set_to_zero_is_not_a_mouse() {
            assert!(!is_mouse_event_device(true, Some("0")));
        }

        #[test]
        fn no_devnode_is_never_a_mouse_even_with_property_set() {
            // Filters out the parent "input" class device so a single
            // physical mouse isn't double-counted via its eventN child.
            assert!(!is_mouse_event_device(false, Some("1")));
        }

        #[test]
        fn no_devnode_and_no_property_is_not_a_mouse() {
            assert!(!is_mouse_event_device(false, None));
        }

        #[test]
        fn malformed_property_value_is_not_a_mouse() {
            assert!(!is_mouse_event_device(true, Some("true")));
        }
    }

    mod xinput_action_fn {
        use super::*;

        #[test]
        fn enabled_maps_to_enable() {
            assert_eq!(xinput_action(true), "enable");
        }

        #[test]
        fn disabled_maps_to_disable() {
            assert_eq!(xinput_action(false), "disable");
        }
    }

    mod ascii_case_insensitive_search {
        use super::*;

        #[test]
        fn finds_exact_match() {
            assert!(contains_ignore_ascii_case("touchpad", "touchpad"));
        }

        #[test]
        fn finds_uppercase_needle_in_lowercase_haystack() {
            assert!(contains_ignore_ascii_case("touchpad", "TOUCHPAD"));
        }

        #[test]
        fn finds_mixed_case_substring() {
            assert!(contains_ignore_ascii_case(
                "SynPS/2 Synaptics TouchPad",
                "touchpad"
            ));
        }

        #[test]
        fn no_match_returns_false() {
            assert!(!contains_ignore_ascii_case(
                "Logitech USB Mouse",
                "touchpad"
            ));
        }

        #[test]
        fn empty_needle_always_matches() {
            assert!(contains_ignore_ascii_case("anything", ""));
        }

        #[test]
        fn needle_longer_than_haystack_does_not_panic_and_returns_false() {
            assert!(!contains_ignore_ascii_case("pad", "touchpad"));
        }

        #[test]
        fn empty_haystack_with_nonempty_needle_returns_false() {
            assert!(!contains_ignore_ascii_case("", "touchpad"));
        }
    }

    mod touchpad_name_parsing {
        use super::*;

        const SAMPLE_XINPUT_OUTPUT: &str = "\
Virtual core pointer
Virtual core XTEST pointer
SynPS/2 Synaptics TouchPad
Logitech USB Mouse
Virtual core keyboard
Virtual core XTEST keyboard
Power Button
";

        #[test]
        fn finds_touchpad_in_realistic_xinput_output() {
            assert_eq!(
                find_touchpad_name(SAMPLE_XINPUT_OUTPUT),
                Some("SynPS/2 Synaptics TouchPad".to_string())
            );
        }

        #[test]
        fn matches_case_insensitively() {
            assert_eq!(
                find_touchpad_name("Virtual core pointer\nALPS touchpad\n"),
                Some("ALPS touchpad".to_string())
            );
            assert_eq!(
                find_touchpad_name("ELAN1200:00 04F3:3067 TOUCHPAD\n"),
                Some("ELAN1200:00 04F3:3067 TOUCHPAD".to_string())
            );
        }

        #[test]
        fn trims_surrounding_whitespace() {
            assert_eq!(
                find_touchpad_name("  Foo Touchpad  \n"),
                Some("Foo Touchpad".to_string())
            );
        }

        #[test]
        fn returns_first_match_when_multiple_present() {
            let output = "First Touchpad\nSecond Touchpad\n";
            assert_eq!(
                find_touchpad_name(output),
                Some("First Touchpad".to_string())
            );
        }

        #[test]
        fn no_touchpad_present_returns_none() {
            let output = "Virtual core pointer\nLogitech USB Mouse\nVirtual core keyboard\n";
            assert_eq!(find_touchpad_name(output), None);
        }

        #[test]
        fn empty_input_returns_none() {
            assert_eq!(find_touchpad_name(""), None);
        }

        #[test]
        fn blank_lines_are_skipped_without_matching() {
            assert_eq!(find_touchpad_name("\n\n\n"), None);
        }
    }
}
