//! The ordering barrier, as a state machine over plain data.
//!
//! Nothing here touches Core Foundation or Text Input Sources: the barrier takes keyboard events
//! as plain data plus the clock, and returns what the `macos` layer must do. The tap layer owns
//! the held events themselves; the barrier only counts them. That split is what makes the
//! ordering testable with a fake clock.

use std::time::{Duration, Instant};

use crate::macos::tis::LayoutTag;

/// The virtual keycode of F19, the key Karabiner emits on a tap and moji swallows.
pub const SIGNAL_KEYCODE: u16 = 80;

/// Which kind of keyboard event the tap saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// A key going down.
    Down,
    /// A key coming up.
    Up,
    /// A modifier changing state.
    Flags,
}

/// The plain-data view of a keyboard event that the barrier reasons about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    /// Whether the key went down, came up, or is a modifier change.
    pub kind: EventKind,
    /// The virtual keycode the event carries.
    pub keycode: u16,
    /// The modifier flags the event carries.
    pub flags: u64,
    /// The event's own timestamp, as the tap reported it.
    pub timestamp: u64,
}

/// What the tap must do with the event it just handed over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Let the event through untouched.
    Pass,
    /// Drop the event; nothing downstream ever sees it.
    Swallow,
    /// Keep a copy in arrival order and drop the original until the replay.
    Hold,
}

/// Everything the barrier decided about one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// What to do with the event itself.
    pub verdict: Verdict,
    /// The layout to select, when the event started a switch.
    pub select: Option<LayoutTag>,
}

impl Decision {
    fn pass() -> Decision {
        Decision {
            verdict: Verdict::Pass,
            select: None,
        }
    }

    fn swallow() -> Decision {
        Decision {
            verdict: Verdict::Swallow,
            select: None,
        }
    }

    fn hold() -> Decision {
        Decision {
            verdict: Verdict::Hold,
            select: None,
        }
    }
}

enum Signal {
    Press,
    Release,
    Other,
}

enum State {
    Idle,
    Switching {
        expected: LayoutTag,
        deadline: Instant,
        held: usize,
    },
    Selecting {
        expected: LayoutTag,
        deadline: Instant,
    },
}

/// The state machine that decides whether a keyboard event passes, is swallowed, or waits.
pub struct Barrier {
    cycle: Vec<LayoutTag>,
    hold: Duration,
    state: State,
}

impl Barrier {
    /// Creates a barrier that toggles along `cycle` and waits `hold` for a confirmation.
    pub fn new(cycle: Vec<LayoutTag>, hold: Duration) -> Barrier {
        Barrier {
            cycle,
            hold,
            state: State::Idle,
        }
    }

    /// Returns how many events are waiting for a replay.
    pub fn held(&self) -> usize {
        match &self.state {
            State::Idle => 0,
            State::Switching {
                expected: _,
                deadline: _,
                held,
            } => *held,
            State::Selecting {
                expected: _,
                deadline: _,
            } => 0,
        }
    }

    /// Decides what happens to one keyboard event, given the layout selected right now.
    pub fn on_key(
        &mut self,
        event: KeyEvent,
        current: Option<LayoutTag>,
        now: Instant,
    ) -> Decision {
        match classify(event) {
            Signal::Press => self.start_switch(current, now),
            Signal::Release => Decision::swallow(),
            Signal::Other => self.hold_or_pass(),
        }
    }

    /// Announces a select the daemon makes without a key, and returns whether it may proceed.
    ///
    /// A key-driven switch owns its window, so this is refused while one is running.
    pub fn on_select(&mut self, expected: LayoutTag, now: Instant) -> bool {
        match &self.state {
            State::Idle => {}
            State::Switching {
                expected: _,
                deadline: _,
                held: _,
            } => return false,
            State::Selecting {
                expected: _,
                deadline: _,
            } => {}
        }
        self.state = State::Selecting {
            expected,
            deadline: now + self.hold,
        };
        true
    }

    /// Reports the layout that is selected now, and returns whether the held events must replay.
    pub fn confirmed(&mut self, now_selected: Option<LayoutTag>) -> bool {
        let Some(now_selected) = now_selected else {
            return false;
        };
        match &self.state {
            State::Idle => false,
            State::Switching {
                expected,
                deadline: _,
                held: _,
            } => {
                if *expected != now_selected {
                    return false;
                }
                self.state = State::Idle;
                true
            }
            State::Selecting {
                expected,
                deadline: _,
            } => {
                if *expected != now_selected {
                    return false;
                }
                self.state = State::Idle;
                false
            }
        }
    }

    /// Releases the held events when the deadline has passed, and returns whether it did.
    pub fn tick(&mut self, now: Instant) -> bool {
        match &self.state {
            State::Idle => false,
            State::Switching {
                expected,
                deadline,
                held,
            } => {
                if now < *deadline {
                    return false;
                }
                tracing::warn!(
                    expected = %expected,
                    held = *held,
                    "layout change was not confirmed in time, releasing the held events"
                );
                self.state = State::Idle;
                true
            }
            State::Selecting { expected, deadline } => {
                if now < *deadline {
                    return false;
                }
                tracing::warn!(expected = %expected, "layout change was not confirmed in time");
                self.state = State::Idle;
                false
            }
        }
    }

    /// Reports that the selection itself failed, and returns whether the held events must replay.
    pub fn select_failed(&mut self) -> bool {
        match &self.state {
            State::Idle => false,
            State::Switching {
                expected: _,
                deadline: _,
                held: _,
            } => {
                self.state = State::Idle;
                true
            }
            State::Selecting {
                expected: _,
                deadline: _,
            } => {
                self.state = State::Idle;
                false
            }
        }
    }

    fn start_switch(&mut self, current: Option<LayoutTag>, now: Instant) -> Decision {
        let from = match &self.state {
            State::Idle => current,
            State::Switching {
                expected: _,
                deadline: _,
                held: _,
            } => return Decision::swallow(),
            State::Selecting {
                expected,
                deadline: _,
            } => Some(expected.clone()),
        };
        let Some(next) = next(&self.cycle, from.as_ref()) else {
            return Decision::swallow();
        };
        self.state = State::Switching {
            expected: next.clone(),
            deadline: now + self.hold,
            held: 0,
        };
        Decision {
            verdict: Verdict::Swallow,
            select: Some(next),
        }
    }

    fn hold_or_pass(&mut self) -> Decision {
        match &mut self.state {
            State::Idle => Decision::pass(),
            State::Switching {
                expected: _,
                deadline: _,
                held,
            } => {
                *held += 1;
                Decision::hold()
            }
            State::Selecting {
                expected: _,
                deadline: _,
            } => Decision::pass(),
        }
    }
}

/// Returns the layout after `current` in the cycle, wrapping.
///
/// A layout outside the cycle, and an unmapped one, both go to the cycle's first entry. An empty
/// cycle has no next layout at all.
pub fn next(cycle: &[LayoutTag], current: Option<&LayoutTag>) -> Option<LayoutTag> {
    let first = cycle.first()?;
    let Some(current) = current else {
        return Some(first.clone());
    };

    let mut found = None;
    for (index, candidate) in cycle.iter().enumerate() {
        if candidate == current {
            found = Some(index);
            break;
        }
    }

    let Some(index) = found else {
        return Some(first.clone());
    };
    let index = (index + 1) % cycle.len();
    Some(cycle[index].clone())
}

fn classify(event: KeyEvent) -> Signal {
    let KeyEvent {
        kind,
        keycode,
        flags: _,
        timestamp: _,
    } = event;
    if keycode != SIGNAL_KEYCODE {
        return Signal::Other;
    }
    match kind {
        EventKind::Down => Signal::Press,
        EventKind::Up => Signal::Release,
        EventKind::Flags => Signal::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOLD: Duration = Duration::from_millis(50);

    fn tag(tag: &str) -> LayoutTag {
        LayoutTag(tag.to_string())
    }

    fn barrier() -> Barrier {
        Barrier::new(vec![tag("en"), tag("ru")], HOLD)
    }

    fn key(kind: EventKind, keycode: u16) -> KeyEvent {
        KeyEvent {
            kind,
            keycode,
            flags: 0,
            timestamp: 0,
        }
    }

    fn signal_down() -> KeyEvent {
        key(EventKind::Down, SIGNAL_KEYCODE)
    }

    fn signal_up() -> KeyEvent {
        key(EventKind::Up, SIGNAL_KEYCODE)
    }

    #[test]
    fn the_signal_key_in_idle_swallows_and_selects_the_next_layout() {
        let mut barrier = barrier();
        let now = Instant::now();

        let Decision { verdict, select } = barrier.on_key(signal_down(), Some(tag("en")), now);

        assert_eq!(verdict, Verdict::Swallow);
        assert_eq!(select, Some(tag("ru")));
    }

    #[test]
    fn the_signal_key_coming_up_is_swallowed_and_selects_nothing() {
        let mut barrier = barrier();
        let now = Instant::now();

        let Decision { verdict, select } = barrier.on_key(signal_up(), Some(tag("en")), now);

        assert_eq!(verdict, Verdict::Swallow);
        assert_eq!(select, None);
    }

    #[test]
    fn keys_in_idle_pass() {
        let mut barrier = barrier();
        let now = Instant::now();

        let Decision { verdict, select } =
            barrier.on_key(key(EventKind::Down, 0), Some(tag("en")), now);

        assert_eq!(verdict, Verdict::Pass);
        assert_eq!(select, None);
        assert_eq!(barrier.held(), 0);
    }

    #[test]
    fn keys_after_the_signal_are_held_and_the_confirmation_replays_them() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(signal_down(), Some(tag("en")), now);

        for keycode in [0u16, 1, 2] {
            let Decision { verdict, select } =
                barrier.on_key(key(EventKind::Down, keycode), Some(tag("en")), now);
            assert_eq!(verdict, Verdict::Hold);
            assert_eq!(select, None);
        }
        assert_eq!(barrier.held(), 3);

        assert!(barrier.confirmed(Some(tag("ru"))));
        assert_eq!(barrier.held(), 0);
    }

    #[test]
    fn flags_events_are_held_like_keys() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(signal_down(), Some(tag("en")), now);

        let Decision { verdict, select: _ } =
            barrier.on_key(key(EventKind::Flags, 56), Some(tag("en")), now);

        assert_eq!(verdict, Verdict::Hold);
        assert_eq!(barrier.held(), 1);
    }

    #[test]
    fn a_second_signal_inside_the_window_is_swallowed_and_not_queued() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(signal_down(), Some(tag("en")), now);
        barrier.on_key(key(EventKind::Down, 0), Some(tag("en")), now);

        let Decision { verdict, select } = barrier.on_key(signal_down(), Some(tag("en")), now);

        assert_eq!(verdict, Verdict::Swallow);
        assert_eq!(select, None);
        assert_eq!(barrier.held(), 1);
    }

    #[test]
    fn the_deadline_replays_what_was_held() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(signal_down(), Some(tag("en")), now);
        barrier.on_key(key(EventKind::Down, 0), Some(tag("en")), now);
        barrier.on_key(key(EventKind::Down, 1), Some(tag("en")), now);

        assert!(!barrier.tick(now + Duration::from_millis(49)));
        assert_eq!(barrier.held(), 2);

        assert!(barrier.tick(now + HOLD));
        assert_eq!(barrier.held(), 0);
    }

    #[test]
    fn a_select_failure_replays_at_once() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(signal_down(), Some(tag("en")), now);
        barrier.on_key(key(EventKind::Down, 0), Some(tag("en")), now);

        assert!(barrier.select_failed());
        assert_eq!(barrier.held(), 0);
    }

    #[test]
    fn a_confirmation_naming_another_layout_does_not_replay_and_the_deadline_still_fires() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(signal_down(), Some(tag("en")), now);
        barrier.on_key(key(EventKind::Down, 0), Some(tag("en")), now);

        assert!(!barrier.confirmed(Some(tag("en"))));
        assert_eq!(barrier.held(), 1);

        assert!(barrier.tick(now + HOLD));
        assert_eq!(barrier.held(), 0);
    }

    #[test]
    fn a_confirmation_of_an_unmapped_layout_never_matches() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(signal_down(), Some(tag("en")), now);
        barrier.on_key(key(EventKind::Down, 0), Some(tag("en")), now);

        assert!(!barrier.confirmed(None));
        assert_eq!(barrier.held(), 1);
    }

    #[test]
    fn confirmed_tick_and_select_failed_in_idle_replay_nothing() {
        let mut barrier = barrier();
        let now = Instant::now();

        assert!(!barrier.confirmed(Some(tag("ru"))));
        assert!(!barrier.tick(now + HOLD));
        assert!(!barrier.select_failed());
    }

    #[test]
    fn an_activation_select_never_holds_keys() {
        let mut barrier = barrier();
        let now = Instant::now();

        assert!(barrier.on_select(tag("en"), now));

        let Decision { verdict, select } =
            barrier.on_key(key(EventKind::Down, 0), Some(tag("ru")), now);
        assert_eq!(verdict, Verdict::Pass);
        assert_eq!(select, None);
        assert_eq!(barrier.held(), 0);
    }

    #[test]
    fn the_signal_key_during_an_activation_select_starts_a_real_switch() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_select(tag("en"), now);

        let Decision { verdict, select } = barrier.on_key(signal_down(), Some(tag("ru")), now);
        assert_eq!(verdict, Verdict::Swallow);
        assert_eq!(select, Some(tag("ru")));

        let Decision { verdict, select: _ } =
            barrier.on_key(key(EventKind::Down, 0), Some(tag("ru")), now);
        assert_eq!(verdict, Verdict::Hold);
        assert_eq!(barrier.held(), 1);
    }

    #[test]
    fn a_confirmation_of_an_activation_select_replays_nothing_and_returns_to_idle() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_select(tag("ru"), now);

        assert!(!barrier.confirmed(Some(tag("ru"))));

        let Decision { verdict: _, select } = barrier.on_key(signal_down(), None, now);
        assert_eq!(select, Some(tag("en")));
    }

    #[test]
    fn an_activation_select_during_a_key_driven_switch_is_refused() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(signal_down(), Some(tag("en")), now);
        barrier.on_key(key(EventKind::Down, 0), Some(tag("en")), now);

        assert!(!barrier.on_select(tag("en"), now));
        assert_eq!(barrier.held(), 1);
        assert!(barrier.confirmed(Some(tag("ru"))));
    }

    #[test]
    fn an_activation_select_during_another_one_replaces_the_target_and_rearms_the_deadline() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_select(tag("en"), now);

        assert!(barrier.on_select(tag("ru"), now + Duration::from_millis(40)));
        assert!(!barrier.tick(now + Duration::from_millis(60)));

        let Decision { verdict: _, select } = barrier.on_key(
            signal_down(),
            Some(tag("en")),
            now + Duration::from_millis(60),
        );
        assert_eq!(select, Some(tag("en")));
    }

    #[test]
    fn the_activation_deadline_returns_to_idle_with_nothing_to_replay() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_select(tag("en"), now);

        assert!(!barrier.tick(now + HOLD));

        let Decision { verdict: _, select } = barrier.on_key(signal_down(), None, now + HOLD);
        assert_eq!(select, Some(tag("en")));
    }

    #[test]
    fn an_activation_select_failure_returns_to_idle() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_select(tag("ru"), now);

        assert!(!barrier.select_failed());

        let Decision { verdict: _, select } = barrier.on_key(signal_down(), None, now);
        assert_eq!(select, Some(tag("en")));
    }

    #[test]
    fn the_cycle_wraps() {
        let cycle = vec![tag("en"), tag("ru")];

        assert_eq!(next(&cycle, Some(&tag("en"))), Some(tag("ru")));
        assert_eq!(next(&cycle, Some(&tag("ru"))), Some(tag("en")));
    }

    #[test]
    fn a_layout_outside_the_cycle_goes_to_its_first_entry() {
        let cycle = vec![tag("en"), tag("ru")];

        assert_eq!(next(&cycle, Some(&tag("de"))), Some(tag("en")));
    }

    #[test]
    fn an_unmapped_layout_goes_to_the_first_entry_of_the_cycle() {
        let cycle = vec![tag("en"), tag("ru")];

        assert_eq!(next(&cycle, None), Some(tag("en")));
    }

    #[test]
    fn an_empty_cycle_selects_nothing_and_never_panics() {
        assert_eq!(next(&[], None), None);
        assert_eq!(next(&[], Some(&tag("en"))), None);

        let mut barrier = Barrier::new(Vec::new(), HOLD);
        let now = Instant::now();

        let Decision { verdict, select } = barrier.on_key(signal_down(), Some(tag("en")), now);
        assert_eq!(verdict, Verdict::Swallow);
        assert_eq!(select, None);

        let Decision { verdict, select: _ } = barrier.on_key(key(EventKind::Down, 0), None, now);
        assert_eq!(verdict, Verdict::Pass);
    }
}
