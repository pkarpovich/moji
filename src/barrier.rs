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

/// How long the held events wait after the confirmation before they are replayed.
///
/// The confirmation is observed in moji's own process, and the distributed notification that
/// carries it reaches another process 1-10 ms later. A replayed event is the captured one, so the
/// receiving application translates its keycode against whatever source it believes is current:
/// replaying the instant moji learns of the switch types the old layout in an application that has
/// not learned of it yet.
pub const SETTLE: Duration = Duration::from_millis(10);

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

/// Whether a key-down is the press itself or the system repeating a held key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stroke {
    /// The key went down under a finger.
    First,
    /// The system repeated a key that is still held.
    Repeat,
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
    /// Whether a key-down is the press or an autorepeat of it.
    pub stroke: Stroke,
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

/// What the daemon must do with the layout change it just reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirmed {
    /// Nothing: no switch was waiting for this layout.
    Nothing,
    /// The switch arrived; the held events wait [`SETTLE`] longer before they replay.
    Settling,
}

/// What the deadline that just passed asks the daemon for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elapsed {
    /// Nothing is waiting, so nothing is due.
    Nothing,
    /// A deadline is still ahead: arm the watchdog again for this long.
    Waiting(Duration),
    /// The settle passed: replay the held events into the layout every process now agrees on.
    Settled,
    /// No confirmation came in time: release the held events anyway.
    Unconfirmed,
}

enum Signal {
    Press,
    Repeat,
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
    Settling {
        deadline: Instant,
        held: usize,
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
            State::Settling { deadline: _, held } => *held,
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
            Signal::Repeat => Decision::swallow(),
            Signal::Release => Decision::swallow(),
            Signal::Other => self.hold_or_pass(),
        }
    }

    /// Announces a select the daemon makes without a key, and returns whether it may proceed.
    ///
    /// A key-driven switch owns its window, so this is refused while one is running and while its
    /// held events are settling.
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
            State::Settling {
                deadline: _,
                held: _,
            } => return false,
        }
        self.state = State::Selecting {
            expected,
            deadline: now + self.hold,
        };
        true
    }

    /// Reports the layout that is selected now, and returns what the daemon must do about it.
    ///
    /// A key-driven switch does not replay on its confirmation: the applications downstream learn
    /// of the change after moji does, so the held events wait [`SETTLE`] longer.
    pub fn confirmed(&mut self, now_selected: Option<LayoutTag>, now: Instant) -> Confirmed {
        let Some(now_selected) = now_selected else {
            return Confirmed::Nothing;
        };
        match &self.state {
            State::Idle => Confirmed::Nothing,
            State::Switching {
                expected,
                deadline: _,
                held,
            } => {
                if *expected != now_selected {
                    return Confirmed::Nothing;
                }
                self.state = State::Settling {
                    deadline: now + SETTLE,
                    held: *held,
                };
                Confirmed::Settling
            }
            State::Selecting {
                expected,
                deadline: _,
            } => {
                if *expected != now_selected {
                    return Confirmed::Nothing;
                }
                self.state = State::Idle;
                Confirmed::Nothing
            }
            State::Settling {
                deadline: _,
                held: _,
            } => Confirmed::Nothing,
        }
    }

    /// Reports the clock, and returns what the deadline that passed asks for.
    pub fn tick(&mut self, now: Instant) -> Elapsed {
        match &self.state {
            State::Idle => Elapsed::Nothing,
            State::Switching {
                expected,
                deadline,
                held,
            } => {
                if now < *deadline {
                    return Elapsed::Waiting(deadline.saturating_duration_since(now));
                }
                tracing::warn!(
                    expected = %expected,
                    held = *held,
                    "layout change was not confirmed in time, releasing the held events"
                );
                self.state = State::Idle;
                Elapsed::Unconfirmed
            }
            State::Selecting { expected, deadline } => {
                if now < *deadline {
                    return Elapsed::Waiting(deadline.saturating_duration_since(now));
                }
                tracing::warn!(expected = %expected, "layout change was not confirmed in time");
                self.state = State::Idle;
                Elapsed::Nothing
            }
            State::Settling { deadline, held: _ } => {
                if now < *deadline {
                    return Elapsed::Waiting(deadline.saturating_duration_since(now));
                }
                self.state = State::Idle;
                Elapsed::Settled
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
            State::Settling {
                deadline: _,
                held: _,
            } => false,
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
            State::Settling {
                deadline: _,
                held: _,
            } => return Decision::swallow(),
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
            State::Settling { deadline: _, held } => {
                *held += 1;
                Decision::hold()
            }
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
        stroke,
    } = event;
    if keycode != SIGNAL_KEYCODE {
        return Signal::Other;
    }
    match kind {
        EventKind::Down => match stroke {
            Stroke::First => Signal::Press,
            Stroke::Repeat => Signal::Repeat,
        },
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
            stroke: Stroke::First,
        }
    }

    fn signal_repeat() -> KeyEvent {
        KeyEvent {
            stroke: Stroke::Repeat,
            ..signal_down()
        }
    }

    fn signal_down() -> KeyEvent {
        key(EventKind::Down, SIGNAL_KEYCODE)
    }

    fn signal_up() -> KeyEvent {
        key(EventKind::Up, SIGNAL_KEYCODE)
    }

    #[test]
    fn a_held_signal_key_repeating_is_swallowed_and_switches_nothing() {
        let mut barrier = barrier();
        let now = Instant::now();

        let Decision { verdict, select } = barrier.on_key(signal_repeat(), Some(tag("en")), now);

        assert_eq!(verdict, Verdict::Swallow);
        assert_eq!(
            select, None,
            "a globe held for a moment would otherwise toggle the layout on every autorepeat"
        );
        assert_eq!(barrier.held(), 0);
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
    fn keys_after_the_signal_are_held_and_the_settle_after_the_confirmation_replays_them() {
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

        assert_eq!(barrier.confirmed(Some(tag("ru")), now), Confirmed::Settling);
        assert_eq!(barrier.held(), 3);

        assert_eq!(barrier.tick(now + SETTLE), Elapsed::Settled);
        assert_eq!(barrier.held(), 0);
    }

    #[test]
    fn the_confirmation_does_not_replay_before_the_settle_has_passed() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(signal_down(), Some(tag("en")), now);
        barrier.on_key(key(EventKind::Down, 0), Some(tag("en")), now);

        barrier.confirmed(Some(tag("ru")), now);

        assert_eq!(
            barrier.tick(now + Duration::from_millis(9)),
            Elapsed::Waiting(Duration::from_millis(1))
        );
        assert_eq!(barrier.held(), 1);
    }

    #[test]
    fn a_key_arriving_during_the_settle_is_held_and_replays_with_the_rest() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(signal_down(), Some(tag("en")), now);
        barrier.on_key(key(EventKind::Down, 0), Some(tag("en")), now);
        barrier.confirmed(Some(tag("ru")), now);

        let Decision { verdict, select } = barrier.on_key(
            key(EventKind::Down, 1),
            Some(tag("ru")),
            now + Duration::from_millis(5),
        );

        assert_eq!(verdict, Verdict::Hold);
        assert_eq!(select, None);
        assert_eq!(barrier.held(), 2);
        assert_eq!(barrier.tick(now + SETTLE), Elapsed::Settled);
    }

    #[test]
    fn a_second_confirmation_during_the_settle_changes_nothing() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(signal_down(), Some(tag("en")), now);
        barrier.on_key(key(EventKind::Down, 0), Some(tag("en")), now);
        barrier.confirmed(Some(tag("ru")), now);

        assert_eq!(barrier.confirmed(Some(tag("en")), now), Confirmed::Nothing);
        assert_eq!(barrier.held(), 1);
        assert_eq!(barrier.tick(now + SETTLE), Elapsed::Settled);
    }

    #[test]
    fn an_activation_select_during_the_settle_is_refused() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(signal_down(), Some(tag("en")), now);
        barrier.on_key(key(EventKind::Down, 0), Some(tag("en")), now);
        barrier.confirmed(Some(tag("ru")), now);

        assert!(!barrier.on_select(tag("en"), now));
        assert_eq!(barrier.held(), 1);
        assert_eq!(barrier.tick(now + SETTLE), Elapsed::Settled);
    }

    #[test]
    fn the_signal_key_during_the_settle_is_swallowed_and_not_queued() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(signal_down(), Some(tag("en")), now);
        barrier.on_key(key(EventKind::Down, 0), Some(tag("en")), now);
        barrier.confirmed(Some(tag("ru")), now);

        let Decision { verdict, select } = barrier.on_key(signal_down(), Some(tag("ru")), now);

        assert_eq!(verdict, Verdict::Swallow);
        assert_eq!(select, None);
        assert_eq!(barrier.held(), 1);
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

        assert_eq!(
            barrier.tick(now + Duration::from_millis(49)),
            Elapsed::Waiting(Duration::from_millis(1))
        );
        assert_eq!(barrier.held(), 2);

        assert_eq!(barrier.tick(now + HOLD), Elapsed::Unconfirmed);
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

        assert_eq!(barrier.confirmed(Some(tag("en")), now), Confirmed::Nothing);
        assert_eq!(barrier.held(), 1);

        assert_eq!(barrier.tick(now + HOLD), Elapsed::Unconfirmed);
        assert_eq!(barrier.held(), 0);
    }

    #[test]
    fn a_confirmation_of_an_unmapped_layout_never_matches() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(signal_down(), Some(tag("en")), now);
        barrier.on_key(key(EventKind::Down, 0), Some(tag("en")), now);

        assert_eq!(barrier.confirmed(None, now), Confirmed::Nothing);
        assert_eq!(barrier.held(), 1);
    }

    #[test]
    fn confirmed_tick_and_select_failed_in_idle_replay_nothing() {
        let mut barrier = barrier();
        let now = Instant::now();

        assert_eq!(barrier.confirmed(Some(tag("ru")), now), Confirmed::Nothing);
        assert_eq!(barrier.tick(now + HOLD), Elapsed::Nothing);
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

        assert_eq!(barrier.confirmed(Some(tag("ru")), now), Confirmed::Nothing);

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
        assert_eq!(barrier.confirmed(Some(tag("ru")), now), Confirmed::Settling);
    }

    #[test]
    fn an_activation_select_during_another_one_replaces_the_target_and_rearms_the_deadline() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_select(tag("en"), now);

        assert!(barrier.on_select(tag("ru"), now + Duration::from_millis(40)));
        assert_eq!(
            barrier.tick(now + Duration::from_millis(60)),
            Elapsed::Waiting(Duration::from_millis(30))
        );

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

        assert_eq!(barrier.tick(now + HOLD), Elapsed::Nothing);

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
