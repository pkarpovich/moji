//! The ordering barrier, as a state machine over plain data.
//!
//! Nothing here touches Core Foundation or Text Input Sources: the barrier takes keyboard events
//! as plain data plus the clock, and returns what the `macos` layer must do. The tap layer owns
//! the held events themselves; the barrier only counts them. That split is what makes the
//! ordering testable with a fake clock.

use std::time::{Duration, Instant};

use crate::cycle::next;
use crate::macos::tis::LayoutTag;

/// The virtual keycode of F19, the key Karabiner emits on a tap and moji swallows.
pub const SWITCH_KEYCODE: u16 = 80;

/// The virtual keycode of F18, the key Karabiner emits to ask for a retype and moji swallows.
pub const RETYPE_KEYCODE: u16 = 79;

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
    /// A mouse button going down.
    MouseDown,
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

/// What the barrier asks the daemon to do besides executing the verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Nothing: the event started neither a switch nor a retype.
    Nothing,
    /// Select this layout: the event started a switch.
    Select(LayoutTag),
    /// Retype what was typed last, in the other layout.
    Retype,
}

/// Everything the barrier decided about one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// What to do with the event itself.
    pub verdict: Verdict,
    /// What the event asks the daemon for beyond its own verdict.
    pub request: Request,
}

impl Decision {
    fn pass() -> Decision {
        Decision {
            verdict: Verdict::Pass,
            request: Request::Nothing,
        }
    }

    fn swallow() -> Decision {
        Decision {
            verdict: Verdict::Swallow,
            request: Request::Nothing,
        }
    }

    fn hold() -> Decision {
        Decision {
            verdict: Verdict::Hold,
            request: Request::Nothing,
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

enum Phase {
    Press,
    Repeat,
    Release,
}

enum Signal {
    Switch(Phase),
    Retype(Phase),
    Mouse,
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
            Signal::Switch(phase) => match phase {
                Phase::Press => self.start_switch(current, now),
                Phase::Repeat => Decision::swallow(),
                Phase::Release => Decision::swallow(),
            },
            Signal::Retype(phase) => match phase {
                Phase::Press => self.ask_retype(),
                Phase::Repeat => Decision::swallow(),
                Phase::Release => Decision::swallow(),
            },
            Signal::Mouse => Decision::pass(),
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

    /// Announces a retype the daemon is about to post, and returns whether it may proceed.
    ///
    /// The queue the retype fills is already full when the switch starts, so `held` is the number
    /// of events waiting before a single key of the user's has arrived. A switch that is already
    /// running owns its window, so this is refused while one is running and while its held events
    /// are settling.
    pub fn on_retype(&mut self, target: LayoutTag, held: usize, now: Instant) -> bool {
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
        self.state = State::Switching {
            expected: target,
            deadline: now + self.hold,
            held,
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
            request: Request::Select(next),
        }
    }

    fn ask_retype(&mut self) -> Decision {
        match &self.state {
            State::Idle => {}
            State::Switching {
                expected,
                deadline: _,
                held: _,
            } => {
                tracing::debug!(%expected, "a switch is already running, so the retype is refused");
                return Decision::swallow();
            }
            State::Selecting {
                expected: _,
                deadline: _,
            } => {}
            State::Settling {
                deadline: _,
                held: _,
            } => {
                tracing::debug!("the held keys are settling, so the retype is refused");
                return Decision::swallow();
            }
        }
        Decision {
            verdict: Verdict::Swallow,
            request: Request::Retype,
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

fn classify(event: KeyEvent) -> Signal {
    let KeyEvent {
        kind,
        keycode,
        flags: _,
        timestamp: _,
        stroke,
    } = event;
    let phase = match kind {
        EventKind::Down => match stroke {
            Stroke::First => Phase::Press,
            Stroke::Repeat => Phase::Repeat,
        },
        EventKind::Up => Phase::Release,
        EventKind::Flags => return Signal::Other,
        EventKind::MouseDown => return Signal::Mouse,
    };
    if keycode == SWITCH_KEYCODE {
        return Signal::Switch(phase);
    }
    if keycode == RETYPE_KEYCODE {
        return Signal::Retype(phase);
    }
    Signal::Other
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

    fn switch_repeat() -> KeyEvent {
        KeyEvent {
            stroke: Stroke::Repeat,
            ..switch_down()
        }
    }

    fn switch_down() -> KeyEvent {
        key(EventKind::Down, SWITCH_KEYCODE)
    }

    fn switch_up() -> KeyEvent {
        key(EventKind::Up, SWITCH_KEYCODE)
    }

    fn retype_down() -> KeyEvent {
        key(EventKind::Down, RETYPE_KEYCODE)
    }

    fn retype_repeat() -> KeyEvent {
        KeyEvent {
            stroke: Stroke::Repeat,
            ..retype_down()
        }
    }

    fn retype_up() -> KeyEvent {
        key(EventKind::Up, RETYPE_KEYCODE)
    }

    fn mouse_down() -> KeyEvent {
        key(EventKind::MouseDown, 0)
    }

    #[test]
    fn a_held_switch_key_repeating_is_swallowed_and_switches_nothing() {
        let mut barrier = barrier();
        let now = Instant::now();

        let Decision { verdict, request } = barrier.on_key(switch_repeat(), Some(tag("en")), now);

        assert_eq!(verdict, Verdict::Swallow);
        assert_eq!(
            request,
            Request::Nothing,
            "a globe held for a moment would otherwise toggle the layout on every autorepeat"
        );
        assert_eq!(barrier.held(), 0);
    }

    #[test]
    fn the_switch_key_in_idle_swallows_and_selects_the_next_layout() {
        let mut barrier = barrier();
        let now = Instant::now();

        let Decision { verdict, request } = barrier.on_key(switch_down(), Some(tag("en")), now);

        assert_eq!(verdict, Verdict::Swallow);
        assert_eq!(request, Request::Select(tag("ru")));
    }

    #[test]
    fn the_switch_key_coming_up_is_swallowed_and_selects_nothing() {
        let mut barrier = barrier();
        let now = Instant::now();

        let Decision { verdict, request } = barrier.on_key(switch_up(), Some(tag("en")), now);

        assert_eq!(verdict, Verdict::Swallow);
        assert_eq!(request, Request::Nothing);
    }

    #[test]
    fn keys_in_idle_pass() {
        let mut barrier = barrier();
        let now = Instant::now();

        let Decision { verdict, request } =
            barrier.on_key(key(EventKind::Down, 0), Some(tag("en")), now);

        assert_eq!(verdict, Verdict::Pass);
        assert_eq!(request, Request::Nothing);
        assert_eq!(barrier.held(), 0);
    }

    #[test]
    fn keys_after_the_switch_are_held_and_the_settle_after_the_confirmation_replays_them() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(switch_down(), Some(tag("en")), now);

        for keycode in [0u16, 1, 2] {
            let Decision { verdict, request } =
                barrier.on_key(key(EventKind::Down, keycode), Some(tag("en")), now);
            assert_eq!(verdict, Verdict::Hold);
            assert_eq!(request, Request::Nothing);
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
        barrier.on_key(switch_down(), Some(tag("en")), now);
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
        barrier.on_key(switch_down(), Some(tag("en")), now);
        barrier.on_key(key(EventKind::Down, 0), Some(tag("en")), now);
        barrier.confirmed(Some(tag("ru")), now);

        let Decision { verdict, request } = barrier.on_key(
            key(EventKind::Down, 1),
            Some(tag("ru")),
            now + Duration::from_millis(5),
        );

        assert_eq!(verdict, Verdict::Hold);
        assert_eq!(request, Request::Nothing);
        assert_eq!(barrier.held(), 2);
        assert_eq!(barrier.tick(now + SETTLE), Elapsed::Settled);
    }

    #[test]
    fn a_second_confirmation_during_the_settle_changes_nothing() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(switch_down(), Some(tag("en")), now);
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
        barrier.on_key(switch_down(), Some(tag("en")), now);
        barrier.on_key(key(EventKind::Down, 0), Some(tag("en")), now);
        barrier.confirmed(Some(tag("ru")), now);

        assert!(!barrier.on_select(tag("en"), now));
        assert_eq!(barrier.held(), 1);
        assert_eq!(barrier.tick(now + SETTLE), Elapsed::Settled);
    }

    #[test]
    fn the_switch_key_during_the_settle_is_swallowed_and_not_queued() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(switch_down(), Some(tag("en")), now);
        barrier.on_key(key(EventKind::Down, 0), Some(tag("en")), now);
        barrier.confirmed(Some(tag("ru")), now);

        let Decision { verdict, request } = barrier.on_key(switch_down(), Some(tag("ru")), now);

        assert_eq!(verdict, Verdict::Swallow);
        assert_eq!(request, Request::Nothing);
        assert_eq!(barrier.held(), 1);
    }

    #[test]
    fn flags_events_are_held_like_keys() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(switch_down(), Some(tag("en")), now);

        let Decision {
            verdict,
            request: _,
        } = barrier.on_key(key(EventKind::Flags, 56), Some(tag("en")), now);

        assert_eq!(verdict, Verdict::Hold);
        assert_eq!(barrier.held(), 1);
    }

    #[test]
    fn a_second_switch_inside_the_window_is_swallowed_and_not_queued() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(switch_down(), Some(tag("en")), now);
        barrier.on_key(key(EventKind::Down, 0), Some(tag("en")), now);

        let Decision { verdict, request } = barrier.on_key(switch_down(), Some(tag("en")), now);

        assert_eq!(verdict, Verdict::Swallow);
        assert_eq!(request, Request::Nothing);
        assert_eq!(barrier.held(), 1);
    }

    #[test]
    fn the_deadline_replays_what_was_held() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(switch_down(), Some(tag("en")), now);
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
        barrier.on_key(switch_down(), Some(tag("en")), now);
        barrier.on_key(key(EventKind::Down, 0), Some(tag("en")), now);

        assert!(barrier.select_failed());
        assert_eq!(barrier.held(), 0);
    }

    #[test]
    fn a_confirmation_naming_another_layout_does_not_replay_and_the_deadline_still_fires() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(switch_down(), Some(tag("en")), now);
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
        barrier.on_key(switch_down(), Some(tag("en")), now);
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

        let Decision { verdict, request } =
            barrier.on_key(key(EventKind::Down, 0), Some(tag("ru")), now);
        assert_eq!(verdict, Verdict::Pass);
        assert_eq!(request, Request::Nothing);
        assert_eq!(barrier.held(), 0);
    }

    #[test]
    fn the_switch_key_during_an_activation_select_starts_a_real_switch() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_select(tag("en"), now);

        let Decision { verdict, request } = barrier.on_key(switch_down(), Some(tag("ru")), now);
        assert_eq!(verdict, Verdict::Swallow);
        assert_eq!(request, Request::Select(tag("ru")));

        let Decision {
            verdict,
            request: _,
        } = barrier.on_key(key(EventKind::Down, 0), Some(tag("ru")), now);
        assert_eq!(verdict, Verdict::Hold);
        assert_eq!(barrier.held(), 1);
    }

    #[test]
    fn a_confirmation_of_an_activation_select_replays_nothing_and_returns_to_idle() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_select(tag("ru"), now);

        assert_eq!(barrier.confirmed(Some(tag("ru")), now), Confirmed::Nothing);

        let Decision {
            verdict: _,
            request,
        } = barrier.on_key(switch_down(), None, now);
        assert_eq!(request, Request::Select(tag("en")));
    }

    #[test]
    fn an_activation_select_during_a_key_driven_switch_is_refused() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(switch_down(), Some(tag("en")), now);
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

        let Decision {
            verdict: _,
            request,
        } = barrier.on_key(
            switch_down(),
            Some(tag("en")),
            now + Duration::from_millis(60),
        );
        assert_eq!(request, Request::Select(tag("en")));
    }

    #[test]
    fn the_activation_deadline_returns_to_idle_with_nothing_to_replay() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_select(tag("en"), now);

        assert_eq!(barrier.tick(now + HOLD), Elapsed::Nothing);

        let Decision {
            verdict: _,
            request,
        } = barrier.on_key(switch_down(), None, now + HOLD);
        assert_eq!(request, Request::Select(tag("en")));
    }

    #[test]
    fn an_activation_select_failure_returns_to_idle() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_select(tag("ru"), now);

        assert!(!barrier.select_failed());

        let Decision {
            verdict: _,
            request,
        } = barrier.on_key(switch_down(), None, now);
        assert_eq!(request, Request::Select(tag("en")));
    }

    #[test]
    fn an_empty_cycle_selects_nothing_and_never_panics() {
        let mut barrier = Barrier::new(Vec::new(), HOLD);
        let now = Instant::now();

        let Decision { verdict, request } = barrier.on_key(switch_down(), Some(tag("en")), now);
        assert_eq!(verdict, Verdict::Swallow);
        assert_eq!(request, Request::Nothing);

        let Decision {
            verdict,
            request: _,
        } = barrier.on_key(key(EventKind::Down, 0), None, now);
        assert_eq!(verdict, Verdict::Pass);
    }

    #[test]
    fn the_retype_key_in_idle_is_swallowed_and_asks_for_a_retype() {
        let mut barrier = barrier();
        let now = Instant::now();

        let Decision { verdict, request } = barrier.on_key(retype_down(), Some(tag("en")), now);

        assert_eq!(verdict, Verdict::Swallow);
        assert_eq!(request, Request::Retype);
        assert_eq!(barrier.held(), 0);
    }

    #[test]
    fn the_retype_key_during_an_activation_select_asks_for_a_retype() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_select(tag("ru"), now);

        let Decision { verdict, request } = barrier.on_key(retype_down(), Some(tag("ru")), now);

        assert_eq!(verdict, Verdict::Swallow);
        assert_eq!(request, Request::Retype);
    }

    #[test]
    fn a_held_retype_key_repeating_and_coming_up_ask_for_nothing() {
        let mut barrier = barrier();
        let now = Instant::now();

        let Decision { verdict, request } = barrier.on_key(retype_repeat(), Some(tag("en")), now);
        assert_eq!(verdict, Verdict::Swallow);
        assert_eq!(
            request,
            Request::Nothing,
            "a key held for a moment would otherwise retype on every autorepeat"
        );

        let Decision { verdict, request } = barrier.on_key(retype_up(), Some(tag("en")), now);
        assert_eq!(verdict, Verdict::Swallow);
        assert_eq!(request, Request::Nothing);
        assert_eq!(barrier.held(), 0);
    }

    #[test]
    fn the_retype_key_during_a_switch_is_swallowed_and_changes_nothing() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(switch_down(), Some(tag("en")), now);
        barrier.on_key(key(EventKind::Down, 0), Some(tag("en")), now);

        let Decision { verdict, request } = barrier.on_key(retype_down(), Some(tag("en")), now);

        assert_eq!(verdict, Verdict::Swallow);
        assert_eq!(request, Request::Nothing);
        assert_eq!(barrier.held(), 1);
        assert_eq!(barrier.confirmed(Some(tag("ru")), now), Confirmed::Settling);
    }

    #[test]
    fn the_retype_key_during_the_settle_is_swallowed_and_changes_nothing() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(switch_down(), Some(tag("en")), now);
        barrier.on_key(key(EventKind::Down, 0), Some(tag("en")), now);
        barrier.confirmed(Some(tag("ru")), now);

        let Decision { verdict, request } = barrier.on_key(retype_down(), Some(tag("ru")), now);

        assert_eq!(verdict, Verdict::Swallow);
        assert_eq!(request, Request::Nothing);
        assert_eq!(barrier.held(), 1);
        assert_eq!(barrier.tick(now + SETTLE), Elapsed::Settled);
    }

    #[test]
    fn a_retype_switches_with_its_queue_already_full_and_the_settle_replays_it() {
        let mut barrier = barrier();
        let now = Instant::now();

        assert!(barrier.on_retype(tag("en"), 3, now));
        assert_eq!(barrier.held(), 3);

        let Decision { verdict, request } =
            barrier.on_key(key(EventKind::Down, 0), Some(tag("ru")), now);
        assert_eq!(verdict, Verdict::Hold);
        assert_eq!(request, Request::Nothing);
        assert_eq!(barrier.held(), 4);

        assert_eq!(barrier.confirmed(Some(tag("en")), now), Confirmed::Settling);
        assert_eq!(barrier.held(), 4);

        assert_eq!(barrier.tick(now + SETTLE), Elapsed::Settled);
        assert_eq!(barrier.held(), 0);
    }

    #[test]
    fn a_retype_during_an_activation_select_takes_the_window_over() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_select(tag("ru"), now);

        assert!(barrier.on_retype(tag("en"), 2, now));
        assert_eq!(barrier.held(), 2);
        assert_eq!(barrier.confirmed(Some(tag("en")), now), Confirmed::Settling);
    }

    #[test]
    fn a_retype_during_a_switch_is_refused_and_leaves_the_switch_alone() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(switch_down(), Some(tag("en")), now);
        barrier.on_key(key(EventKind::Down, 0), Some(tag("en")), now);

        assert!(!barrier.on_retype(tag("en"), 7, now));
        assert_eq!(barrier.held(), 1);
        assert_eq!(barrier.confirmed(Some(tag("ru")), now), Confirmed::Settling);
    }

    #[test]
    fn a_retype_during_the_settle_is_refused_and_leaves_the_settle_alone() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_key(switch_down(), Some(tag("en")), now);
        barrier.on_key(key(EventKind::Down, 0), Some(tag("en")), now);
        barrier.confirmed(Some(tag("ru")), now);

        assert!(!barrier.on_retype(tag("en"), 7, now));
        assert_eq!(barrier.held(), 1);
        assert_eq!(barrier.tick(now + SETTLE), Elapsed::Settled);
    }

    #[test]
    fn a_retype_that_is_never_confirmed_releases_what_it_queued() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_retype(tag("en"), 2, now);

        assert_eq!(
            barrier.tick(now + Duration::from_millis(49)),
            Elapsed::Waiting(Duration::from_millis(1))
        );
        assert_eq!(barrier.held(), 2);

        assert_eq!(barrier.tick(now + HOLD), Elapsed::Unconfirmed);
        assert_eq!(barrier.held(), 0);
    }

    #[test]
    fn a_retype_whose_select_failed_replays_at_once() {
        let mut barrier = barrier();
        let now = Instant::now();
        barrier.on_retype(tag("en"), 2, now);

        assert!(barrier.select_failed());
        assert_eq!(barrier.held(), 0);
    }

    #[test]
    fn a_mouse_down_passes_in_every_state_and_is_never_held() {
        let mut barrier = barrier();
        let now = Instant::now();

        let Decision { verdict, request } = barrier.on_key(mouse_down(), Some(tag("en")), now);
        assert_eq!(verdict, Verdict::Pass);
        assert_eq!(request, Request::Nothing);

        barrier.on_select(tag("ru"), now);
        let Decision {
            verdict,
            request: _,
        } = barrier.on_key(mouse_down(), Some(tag("en")), now);
        assert_eq!(verdict, Verdict::Pass);

        barrier.on_key(switch_down(), Some(tag("en")), now);
        let Decision {
            verdict,
            request: _,
        } = barrier.on_key(mouse_down(), Some(tag("en")), now);
        assert_eq!(verdict, Verdict::Pass);
        assert_eq!(barrier.held(), 0);

        barrier.confirmed(Some(tag("ru")), now);
        let Decision {
            verdict,
            request: _,
        } = barrier.on_key(mouse_down(), Some(tag("ru")), now);
        assert_eq!(verdict, Verdict::Pass);
        assert_eq!(barrier.held(), 0);
    }
}
