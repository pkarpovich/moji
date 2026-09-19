//! The daemon: the run loop sources, the barrier they feed, and nothing else.
//!
//! Everything here is safe code over the `macos` layer. The tap hands keyboard events to the
//! barrier and executes its verdict, the input source observer confirms the switch, the focus
//! poll notices the keyboard moving to another application, and the watchdog releases the held
//! keys when no confirmation arrives in time. All of them are sources on the main thread's run
//! loop, which is also the only thread Text Input Sources may be called from.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use objc2_core_foundation::CFRunLoop;
use objc2_core_graphics::CGEvent;

use crate::barrier::{
    Barrier, Confirmed, Decision, Elapsed, KeyEvent, Request, SETTLE, SWITCH_KEYCODE, Verdict,
};
use crate::executable::{self, Executable};
use crate::history::{self, Action, Flip, History};
use crate::macos::focus;
use crate::macos::signals;
use crate::macos::tap::{self, Deletions, Held, HeldEvent, Kept, Placement, Tap, TapError};
use crate::macos::timer::{Repeat, Timer, TimerError};
use crate::macos::tis::{self, ChangeObserver, Layout, LayoutTag};
use crate::macos::workspace::BundleId;
use crate::memory::Memory;

/// How long a switch may wait for its confirmation before the held keys go through anyway.
pub const HOLD: Duration = Duration::from_millis(50);

const TERMINATION_POLL: Duration = Duration::from_millis(500);

/// How often the watchdog had to release held events, and how many the last release let go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Releases {
    /// How many times a deadline passed with events still waiting.
    pub count: usize,
    /// How many events the last of those releases let go.
    pub last: usize,
}

/// What can go wrong when starting the daemon.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// The keyboard tap could not be installed.
    #[error(transparent)]
    Tap(#[from] TapError),
    /// A run loop timer could not be installed.
    #[error(transparent)]
    Timer(#[from] TimerError),
}

/// Everything moji runs on the main thread's run loop.
pub struct Daemon {
    state: Rc<State>,
    tap: Tap,
    observer: Option<ChangeObserver>,
    #[allow(
        dead_code,
        reason = "the focus poll is held so that dropping the daemon stops it"
    )]
    focus: Timer,
}

impl Daemon {
    /// Installs the tap, both notification observers and the watchdog on the current run loop.
    ///
    /// # Errors
    ///
    /// Returns [`StartError::Tap`] when the keyboard tap cannot be created, which is what a
    /// missing Input Monitoring grant looks like, and [`StartError::Timer`] when the watchdog
    /// cannot become a source on this thread's run loop.
    pub fn start(
        cycle: Vec<LayoutTag>,
        layouts: BTreeMap<LayoutTag, Layout>,
        pins: BTreeMap<BundleId, LayoutTag>,
        hold: Duration,
    ) -> Result<Daemon, StartError> {
        let state = Rc::new(State {
            barrier: RefCell::new(Barrier::new(cycle.clone(), hold)),
            memory: RefCell::new(Memory::new(pins)),
            layouts,
            held: Held::empty(),
            releases: Cell::new(Releases::default()),
            switched_at: Cell::new(None),
            focused: RefCell::new(None),
            current: RefCell::new(None),
            history: RefCell::new(History::new()),
            cycle,
            watchdog: OnceCell::new(),
            hold,
        });

        state.seed_frontmost();
        state.seed_current();

        let watching = Rc::downgrade(&state);
        let watchdog = Timer::install(Repeat::Never, move || {
            let Some(state) = watching.upgrade() else {
                return;
            };
            state.on_deadline(Instant::now());
        })?;
        let _ = state.watchdog.set(watchdog);

        let tapping = Rc::clone(&state);
        let tap = tap::install(
            Placement::Intercept,
            state.held.clone(),
            move |event, carried| tapping.on_key(event, carried, Instant::now()),
        )?;

        let observing = Rc::clone(&state);
        let observer = tis::observe_changes(move || observing.on_confirmation());

        let polling = Rc::downgrade(&state);
        let focus = Timer::install(Repeat::Every(focus::POLL), move || {
            let Some(state) = polling.upgrade() else {
                return;
            };
            state.on_focus_poll(Instant::now());
        })?;

        Ok(Daemon {
            state,
            tap,
            observer: Some(observer),
            focus,
        })
    }

    /// Returns how often the watchdog had to release held events since the daemon started.
    pub fn releases(&self) -> Releases {
        let Daemon {
            state,
            tap: _,
            observer: _,
            focus: _,
        } = self;
        state.releases.get()
    }

    /// Runs the per-application policy for `app` as if the workspace had announced it.
    ///
    /// The live suite calls this because an unbundled binary carries no bundle id of its own, so
    /// the notification the daemon subscribes to cannot name the harness window.
    pub fn activated(&self, app: BundleId) {
        let Daemon {
            state,
            tap: _,
            observer: _,
            focus: _,
        } = self;
        state.on_activation(app, Instant::now());
    }

    /// Stops listening for input source changes, so nothing confirms a switch any more.
    ///
    /// The live suite calls this to prove that the watchdog still releases the held keys when the
    /// confirmation never arrives.
    pub fn disconnect_confirmation(&mut self) {
        let Daemon {
            state: _,
            tap: _,
            observer,
            focus: _,
        } = self;
        observer.take();
    }

    /// Runs the main run loop until SIGTERM arrives or the binary is replaced under it.
    ///
    /// The tap goes down before the events it still holds are posted: a replay travels through
    /// every session tap, and moji's own answers from a run loop that has already stopped.
    ///
    /// # Errors
    ///
    /// Returns [`StartError::Timer`] when the timer that watches for SIGTERM, or the one that
    /// watches the running binary, cannot be installed.
    pub fn run(self) -> Result<(), StartError> {
        signals::watch_for_termination();
        let stopper = Timer::install(Repeat::Every(TERMINATION_POLL), stop_when_terminating)?;

        let executable = Executable::current();
        let upgrade = Timer::install(Repeat::Every(executable::POLL), move || {
            stop_when_swapped(executable.as_ref())
        })?;

        CFRunLoop::run();

        let Daemon {
            state,
            tap,
            observer: _,
            focus: _,
        } = self;
        drop(tap);
        state.release_on_stop();

        drop(upgrade);
        drop(stopper);
        Ok(())
    }
}

fn stop_when_terminating() {
    if !signals::termination_requested() {
        return;
    }
    let Some(run_loop) = CFRunLoop::current() else {
        return;
    };
    tracing::info!("SIGTERM arrived, so moji stops and leaves the restart to launchd");
    run_loop.stop();
}

fn stop_when_swapped(executable: Option<&Executable>) {
    let Some(executable) = executable else {
        return;
    };
    if !executable.swapped() {
        return;
    }
    let Some(run_loop) = CFRunLoop::current() else {
        return;
    };
    tracing::info!(
        path = %executable.path().display(),
        "the binary was replaced, so moji stops and launchd starts the new version"
    );
    run_loop.stop();
}

#[derive(Debug, Clone, Copy)]
enum Switch {
    Needed,
    Unnecessary,
}

struct State {
    barrier: RefCell<Barrier>,
    memory: RefCell<Memory>,
    layouts: BTreeMap<LayoutTag, Layout>,
    held: Held,
    releases: Cell<Releases>,
    watchdog: OnceCell<Timer>,
    hold: Duration,
    switched_at: Cell<Option<Instant>>,
    focused: RefCell<Option<BundleId>>,
    current: RefCell<Option<LayoutTag>>,
    history: RefCell<History<HeldEvent>>,
    cycle: Vec<LayoutTag>,
}

impl State {
    fn on_key(&self, event: KeyEvent, carried: &CGEvent, now: Instant) -> Verdict {
        let KeyEvent {
            kind: _,
            keycode,
            flags: _,
            timestamp: _,
            stroke: _,
        } = event;
        let current = self.current_for(keycode);

        let decision = {
            let Ok(mut barrier) = self.barrier.try_borrow_mut() else {
                return Verdict::Pass;
            };
            barrier.on_key(event, current, now)
        };
        let Decision { verdict, request } = decision;

        match verdict {
            Verdict::Pass => self.on_typed(event, carried),
            Verdict::Hold => self.on_typed(event, carried),
            Verdict::Swallow => {}
        }

        match request {
            Request::Nothing => {}
            Request::Retype => self.on_retype(now),
            Request::Select(tag) => {
                tracing::debug!(%tag, "the switch key asks for a switch");
                self.switched_at.set(Some(now));
                self.arm(self.hold);
                if !self.select(&tag) {
                    self.selection_failed();
                }
            }
        }
        verdict
    }

    fn on_typed(&self, event: KeyEvent, carried: &CGEvent) {
        let action = history::action(event);
        let tag = self.current.borrow().clone();
        let Ok(mut history) = self.history.try_borrow_mut() else {
            return;
        };
        match action {
            Action::Ignore => {}
            Action::Erase => history.erase(),
            Action::Clear => history.clear(),
            Action::Record(kind) => {
                let Some(captured) = HeldEvent::capture(carried) else {
                    tracing::warn!("a keystroke could not be captured, so the history is dropped");
                    history.clear();
                    return;
                };
                history.record(kind, tag, captured);
            }
        }
    }

    fn on_retype(&self, now: Instant) {
        let planned = {
            let Ok(history) = self.history.try_borrow() else {
                return;
            };
            history.planned(&self.cycle)
        };
        let Some(Flip { count, target }) = planned else {
            tracing::debug!(
                "nothing moji saw typed is within reach, so the retype key does nothing"
            );
            return;
        };

        let switch = match self.current.borrow().clone() {
            None => Switch::Needed,
            Some(current) => match current == target {
                true => Switch::Unnecessary,
                false => Switch::Needed,
            },
        };
        match switch {
            Switch::Unnecessary => {}
            Switch::Needed => {
                if !self.select(&target) {
                    tracing::warn!(%target, "the retype has no layout to select, so nothing is retyped");
                    return;
                }
            }
        }

        let Some(deletions) = Deletions::built(count) else {
            tracing::warn!(
                count,
                "a deletion could not be built, so nothing is retyped"
            );
            return;
        };

        let flipped = {
            let Ok(mut history) = self.history.try_borrow_mut() else {
                return;
            };
            history.flip(&self.cycle)
        };
        let Some(Flip { count, target }) = flipped else {
            return;
        };

        match self.push_strokes(count) {
            Kept::Yes => {}
            Kept::No => {
                tracing::warn!(
                    "a keystroke could not be copied for the retype, so the history is dropped"
                );
                self.forget_history();
                self.held.replay();
                return;
            }
        }
        deletions.post();

        match switch {
            Switch::Unnecessary => {
                tracing::debug!(
                    count,
                    %target,
                    "the retype replays the keystrokes in the layout that is already selected"
                );
                self.held.replay();
            }
            Switch::Needed => {
                let waiting = self.held.len();
                let accepted = {
                    let Ok(mut barrier) = self.barrier.try_borrow_mut() else {
                        return;
                    };
                    barrier.on_retype(target.clone(), waiting, now)
                };
                if !accepted {
                    tracing::debug!(
                        count,
                        %target,
                        "the barrier refused the retype, so the keystrokes go out as they are"
                    );
                    self.release();
                    return;
                }
                self.switched_at.set(Some(now));
                self.arm(self.hold);
                tracing::debug!(
                    count,
                    %target,
                    held = waiting,
                    "the retype selects another layout and replays the keystrokes once it is confirmed"
                );
            }
        }
    }

    fn push_strokes(&self, count: usize) -> Kept {
        let Ok(history) = self.history.try_borrow() else {
            return Kept::No;
        };
        self.held.push_strokes(&history.last(count))
    }

    fn forget_history(&self) {
        let Ok(mut history) = self.history.try_borrow_mut() else {
            return;
        };
        history.clear();
    }

    fn on_confirmation(&self) {
        self.confirm(self.current_tag(), Instant::now());
    }

    fn confirm(&self, current: Option<LayoutTag>, now: Instant) {
        self.current.replace(current.clone());
        self.remember(current.clone());

        let confirmed = {
            let Ok(mut barrier) = self.barrier.try_borrow_mut() else {
                return;
            };
            barrier.confirmed(current.clone(), now)
        };
        match confirmed {
            Confirmed::Nothing => {}
            Confirmed::Settling => {
                tracing::debug!(
                    layout = ?current,
                    waited_ms = self.since_switch_ms(),
                    held = self.held_count(),
                    "the switch is confirmed, settling before the held keys go through"
                );
                self.arm(SETTLE);
            }
        }
    }

    fn on_activation(&self, app: BundleId, now: Instant) {
        tracing::debug!(%app, "the keyboard moved to another application");
        self.forget_typed();
        let current = self.current_tag();
        let decided = {
            let Ok(mut memory) = self.memory.try_borrow_mut() else {
                return;
            };
            memory.on_activated(app, current)
        };
        let Some(tag) = decided else {
            return;
        };

        let accepted = {
            let Ok(mut barrier) = self.barrier.try_borrow_mut() else {
                return;
            };
            barrier.on_select(tag.clone(), now)
        };
        if !accepted {
            return;
        }
        tracing::debug!(%tag, "the application the keyboard moved to asks for a switch");
        self.switched_at.set(Some(now));
        self.arm(self.hold);
        if !self.select(&tag) {
            self.selection_failed();
        }
    }

    fn forget_typed(&self) {
        let Ok(mut history) = self.history.try_borrow_mut() else {
            return;
        };
        history.clear();
    }

    fn seed_current(&self) {
        self.current.replace(self.current_tag());
    }

    fn seed_frontmost(&self) {
        let Some(app) = focus::focused() else {
            return;
        };
        self.focused.replace(Some(app.clone()));
        let Ok(mut memory) = self.memory.try_borrow_mut() else {
            return;
        };
        memory.seed(app);
    }

    fn on_focus_poll(&self, now: Instant) {
        let moved = {
            let last = self.focused.borrow();
            focus::moved(last.as_ref(), focus::focused())
        };
        let Some(app) = moved else {
            return;
        };
        self.focused.replace(Some(app.clone()));
        self.on_activation(app, now);
    }

    fn remember(&self, current: Option<LayoutTag>) {
        let Some(app) = self.focused.borrow().clone() else {
            return;
        };
        let Ok(mut memory) = self.memory.try_borrow_mut() else {
            return;
        };
        memory.on_layout_changed(app, current);
    }

    fn on_deadline(&self, now: Instant) {
        let (waiting, elapsed) = {
            let Ok(mut barrier) = self.barrier.try_borrow_mut() else {
                return;
            };
            let waiting = barrier.held();
            (waiting, barrier.tick(now))
        };

        match elapsed {
            Elapsed::Nothing => {}
            Elapsed::Waiting(left) => self.arm(left),
            Elapsed::Settled => {
                tracing::debug!(
                    held = waiting,
                    total_ms = self.since_switch_ms(),
                    "the held keys go through in the new layout"
                );
                self.release();
            }
            Elapsed::Unconfirmed => {
                let Releases { count, last: _ } = self.releases.get();
                self.releases.set(Releases {
                    count: count + 1,
                    last: waiting,
                });
                tracing::warn!(
                    held = waiting,
                    waited_ms = self.hold.as_millis(),
                    releases = count + 1,
                    "no confirmation arrived in time, so the held keys go through as they are"
                );
                self.release();
            }
        }
    }

    fn select(&self, tag: &LayoutTag) -> bool {
        let Some(layout) = self.layouts.get(tag) else {
            tracing::warn!(%tag, "the barrier named a layout the configuration does not carry");
            return false;
        };
        let Err(error) = tis::select(layout) else {
            self.current.replace(Some(tag.clone()));
            return true;
        };
        tracing::warn!(%tag, %error, "selecting the layout failed");
        false
    }

    fn selection_failed(&self) {
        let released = {
            let Ok(mut barrier) = self.barrier.try_borrow_mut() else {
                return;
            };
            barrier.select_failed()
        };
        if !released {
            return;
        }
        self.release();
    }

    fn release(&self) {
        self.disarm();
        self.held.replay();
    }

    fn release_on_stop(&self) {
        if self.held.is_empty() {
            return;
        }
        tracing::info!(
            held = self.held.len(),
            "moji stops with events still held, so they go out before it does"
        );
        self.release();
    }

    fn since_switch_ms(&self) -> u128 {
        let Some(switched_at) = self.switched_at.get() else {
            return 0;
        };
        switched_at.elapsed().as_millis()
    }

    fn held_count(&self) -> usize {
        let Ok(barrier) = self.barrier.try_borrow() else {
            return 0;
        };
        barrier.held()
    }

    fn arm(&self, after: Duration) {
        let Some(watchdog) = self.watchdog.get() else {
            return;
        };
        watchdog.arm(after);
    }

    fn disarm(&self) {
        let Some(watchdog) = self.watchdog.get() else {
            return;
        };
        watchdog.disarm();
    }

    fn current_for(&self, keycode: u16) -> Option<LayoutTag> {
        if keycode != SWITCH_KEYCODE {
            return None;
        }
        self.current_tag()
    }

    fn current_tag(&self) -> Option<LayoutTag> {
        let Layout {
            name,
            id: _,
            language: _,
        } = tis::current()?;
        tag_named(&self.layouts, &name)
    }
}

/// Returns the tag under which `layouts` carries the layout with this localized name.
pub fn tag_named(layouts: &BTreeMap<LayoutTag, Layout>, name: &str) -> Option<LayoutTag> {
    for (tag, layout) in layouts {
        let Layout {
            name: candidate,
            id: _,
            language: _,
        } = layout;
        if candidate == name {
            return Some(tag.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CFRetained;

    use super::*;
    use crate::barrier::{EventKind, Stroke};

    fn tag(tag: &str) -> LayoutTag {
        LayoutTag(tag.to_string())
    }

    fn layout(name: &str, id: &str, language: &str) -> Layout {
        Layout {
            name: name.to_string(),
            id: id.to_string(),
            language: Some(language.to_string()),
        }
    }

    fn universal() -> BTreeMap<LayoutTag, Layout> {
        let mut layouts = BTreeMap::new();
        layouts.insert(
            tag("en"),
            layout(
                "English - Universal",
                "me.tonsky.keyboardlayout.universal.keylayout.English-Universal",
                "en",
            ),
        );
        layouts.insert(
            tag("ru"),
            layout(
                "Russian - Universal",
                "me.tonsky.keyboardlayout.universal.keylayout.Russian-Universal",
                "ru",
            ),
        );
        layouts
    }

    fn state() -> State {
        let cycle = vec![tag("en"), tag("ru")];
        State {
            barrier: RefCell::new(Barrier::new(cycle.clone(), HOLD)),
            memory: RefCell::new(Memory::new(BTreeMap::new())),
            layouts: universal(),
            held: Held::empty(),
            releases: Cell::new(Releases::default()),
            watchdog: OnceCell::new(),
            hold: HOLD,
            switched_at: Cell::new(None),
            focused: RefCell::new(None),
            current: RefCell::new(None),
            history: RefCell::new(History::new()),
            cycle,
        }
    }

    fn keyboard_event(keycode: u16) -> CFRetained<CGEvent> {
        let Some(event) = CGEvent::new_keyboard_event(None, keycode, true) else {
            panic!("creating a keyboard event failed");
        };
        event
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

    fn type_letter(state: &State, keycode: u16) {
        state.on_typed(key(EventKind::Down, keycode), &keyboard_event(keycode));
    }

    #[test]
    fn a_confirmation_updates_the_layout_the_daemon_believes_is_selected() {
        let state = state();

        state.confirm(Some(tag("ru")), Instant::now());

        assert_eq!(*state.current.borrow(), Some(tag("ru")));
    }

    #[test]
    fn a_confirmation_of_a_layout_no_tag_names_leaves_the_daemon_without_one() {
        let state = state();
        state.confirm(Some(tag("ru")), Instant::now());

        state.confirm(None, Instant::now());

        assert_eq!(*state.current.borrow(), None);
    }

    #[test]
    fn a_typed_key_lands_in_the_history_under_the_layout_the_daemon_believes_is_selected() {
        let state = state();
        state.confirm(Some(tag("ru")), Instant::now());

        type_letter(&state, 0);
        type_letter(&state, 35);

        let Ok(mut history) = state.history.try_borrow_mut() else {
            panic!("the history is borrowed somewhere else");
        };
        assert_eq!(history.len(), 2);
        assert_eq!(
            history.flip(&state.cycle),
            Some(Flip {
                count: 2,
                target: tag("en")
            })
        );
    }

    #[test]
    fn a_click_and_a_caret_key_drop_what_the_history_holds() {
        let state = state();
        state.confirm(Some(tag("ru")), Instant::now());

        type_letter(&state, 0);
        state.on_typed(key(EventKind::MouseDown, 0), &keyboard_event(0));
        assert_eq!(state.history.borrow().len(), 0);

        type_letter(&state, 0);
        state.on_typed(key(EventKind::Down, 123), &keyboard_event(123));

        assert_eq!(state.history.borrow().len(), 0);
    }

    #[test]
    fn delete_erases_one_keystroke_and_a_key_up_leaves_the_history_alone() {
        let state = state();
        state.confirm(Some(tag("ru")), Instant::now());

        type_letter(&state, 0);
        type_letter(&state, 35);
        state.on_typed(key(EventKind::Up, 35), &keyboard_event(35));
        assert_eq!(state.history.borrow().len(), 2);

        state.on_typed(key(EventKind::Down, 51), &keyboard_event(51));

        assert_eq!(state.history.borrow().len(), 1);
    }

    #[test]
    fn a_keystroke_typed_in_a_layout_no_tag_names_clears_the_history() {
        let state = state();
        state.confirm(Some(tag("ru")), Instant::now());
        type_letter(&state, 0);

        state.confirm(None, Instant::now());
        type_letter(&state, 35);

        assert_eq!(state.history.borrow().len(), 0);
    }

    #[test]
    fn the_keyboard_moving_to_another_application_drops_what_the_history_holds() {
        let state = state();
        state.confirm(Some(tag("ru")), Instant::now());
        type_letter(&state, 0);

        state.forget_typed();

        assert_eq!(state.history.borrow().len(), 0);
    }

    #[test]
    fn a_retype_with_nothing_typed_posts_nothing_and_holds_nothing() {
        let state = state();
        state.confirm(Some(tag("ru")), Instant::now());

        state.on_retype(Instant::now());

        assert!(state.held.is_empty());
        assert_eq!(state.history.borrow().len(), 0);
    }

    #[test]
    fn a_retype_that_cannot_select_its_layout_leaves_the_history_as_it_was() {
        let mut state = state();
        state.confirm(Some(tag("ru")), Instant::now());
        type_letter(&state, 0);
        state.layouts = BTreeMap::new();

        state.on_retype(Instant::now());

        assert!(state.held.is_empty());
        let Ok(mut history) = state.history.try_borrow_mut() else {
            panic!("the history is borrowed somewhere else");
        };
        assert_eq!(history.len(), 1);
        assert_eq!(
            history.flip(&state.cycle),
            Some(Flip {
                count: 1,
                target: tag("en")
            })
        );
    }

    #[test]
    fn a_configured_layout_answers_with_its_tag() {
        let layouts = universal();

        assert_eq!(tag_named(&layouts, "English - Universal"), Some(tag("en")));
        assert_eq!(tag_named(&layouts, "Russian - Universal"), Some(tag("ru")));
    }

    #[test]
    fn a_layout_no_tag_names_has_no_tag() {
        let layouts = universal();

        assert_eq!(tag_named(&layouts, "ABC"), None);
    }

    #[test]
    fn the_name_must_match_exactly() {
        let layouts = universal();

        assert_eq!(tag_named(&layouts, "English"), None);
        assert_eq!(tag_named(&layouts, "english - universal"), None);
    }

    #[test]
    fn nothing_is_configured_so_nothing_has_a_tag() {
        let layouts = BTreeMap::new();

        assert_eq!(tag_named(&layouts, "English - Universal"), None);
    }

    #[test]
    fn a_daemon_that_never_ran_released_nothing() {
        let Releases { count, last } = Releases::default();

        assert_eq!(count, 0);
        assert_eq!(last, 0);
    }
}
