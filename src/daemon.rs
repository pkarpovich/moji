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

use crate::barrier::{
    Barrier, Confirmed, Decision, Elapsed, KeyEvent, SETTLE, SIGNAL_KEYCODE, Verdict,
};
use crate::executable::{self, Executable};
use crate::macos::focus;
use crate::macos::signals;
use crate::macos::tap::{self, Held, Placement, Tap, TapError};
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
            barrier: RefCell::new(Barrier::new(cycle, hold)),
            memory: RefCell::new(Memory::new(pins)),
            layouts,
            held: Held::empty(),
            releases: Cell::new(Releases::default()),
            switched_at: Cell::new(None),
            focused: RefCell::new(None),
            watchdog: OnceCell::new(),
            hold,
        });

        state.seed_frontmost();

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
            move |event, _carried| tapping.on_key(event, Instant::now()),
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
}

impl State {
    fn on_key(&self, event: KeyEvent, now: Instant) -> Verdict {
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
        let Decision { verdict, select } = decision;

        let Some(tag) = select else {
            return verdict;
        };
        tracing::debug!(%tag, "the signal key asks for a switch");
        self.switched_at.set(Some(now));
        self.arm(self.hold);
        self.select(&tag);
        verdict
    }

    fn on_confirmation(&self) {
        let current = self.current_tag();
        self.remember(current.clone());

        let confirmed = {
            let Ok(mut barrier) = self.barrier.try_borrow_mut() else {
                return;
            };
            barrier.confirmed(current.clone(), Instant::now())
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
        self.select(&tag);
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

    fn select(&self, tag: &LayoutTag) {
        let Some(layout) = self.layouts.get(tag) else {
            tracing::warn!(%tag, "the barrier named a layout the configuration does not carry");
            self.selection_failed();
            return;
        };
        let Err(error) = tis::select(layout) else {
            return;
        };
        tracing::warn!(%tag, %error, "selecting the layout failed");
        self.selection_failed();
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
        if keycode != SIGNAL_KEYCODE {
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
    use super::*;

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
