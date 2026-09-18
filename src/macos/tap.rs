//! The session event tap: the only thing between a keystroke and the application that types it.
//!
//! The tap sees `keyDown`, `keyUp` and `flagsChanged` at the session level with head insert, so it
//! is downstream of Karabiner's virtual keyboard and upstream of every application. It hands each
//! event to the barrier as plain data, and executes the verdict that comes back: pass it on,
//! swallow it, or keep a retained copy until the layout change is confirmed. A replayed event
//! carries moji's magic in its source user data, so the tap lets its own replays straight through
//! instead of feeding them back into the barrier.

use std::cell::{OnceCell, RefCell};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::Rc;

use objc2_core_foundation::{
    CFMachPort, CFRetained, CFRunLoop, CFRunLoopSource, kCFRunLoopDefaultMode,
};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType, CGPreflightListenEventAccess, CGPreflightPostEventAccess,
    CGRequestListenEventAccess, CGRequestPostEventAccess,
};

use crate::barrier::{EventKind, KeyEvent, Verdict};

const KEYBOARD_MASK: CGEventMask = (1 << CGEventType::KeyDown.0)
    | (1 << CGEventType::KeyUp.0)
    | (1 << CGEventType::FlagsChanged.0);

/// The source user data moji writes into a replayed event so its own tap recognizes it.
pub const REPLAY_MAGIC: i64 = 0x6d6f_6a69;

/// Whether the tap may change what the applications downstream see.
pub enum Placement {
    /// Watches the events and never touches them.
    Listen,
    /// Can swallow an event and keep it back, which is what the barrier needs.
    Intercept,
}

/// A macOS privacy grant moji cannot work without.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Input Monitoring: without it the tap never sees a key.
    Listen,
    /// Accessibility: without it a held key can never be posted again.
    Post,
}

impl Access {
    /// Returns the System Settings pane that grants this access.
    pub fn pane(self) -> &'static str {
        match self {
            Access::Listen => "System Settings > Privacy & Security > Input Monitoring",
            Access::Post => "System Settings > Privacy & Security > Accessibility",
        }
    }
}

/// What can go wrong when installing the keyboard tap.
#[derive(Debug, thiserror::Error)]
pub enum TapError {
    /// Core Graphics refused to create the tap, which is what a missing grant looks like.
    #[error("creating the keyboard event tap failed: this process has no Input Monitoring grant")]
    NotCreated,
    /// The tap has no run loop source, so nothing would ever call back.
    #[error("the keyboard event tap has no run loop source")]
    NoSource,
    /// The calling thread has no run loop to carry the tap.
    #[error("this thread has no run loop, so the keyboard tap would never fire")]
    NoRunLoop,
}

/// A keyboard event moji kept back, retained so it can be posted once the layout is confirmed.
pub struct HeldEvent(CFRetained<CGEvent>);

enum Kept {
    Yes,
    No,
}

/// The queue of events a tap is holding, shared with whoever releases them.
///
/// The tap pushes into it from its own callback and the daemon drains it from the confirmation or
/// from the watchdog, so the queue is a handle both sides hold rather than a field of either.
#[derive(Clone)]
pub struct Held(Rc<RefCell<Vec<HeldEvent>>>);

impl Held {
    /// Returns a queue holding nothing, which is what a tap is installed with.
    pub fn empty() -> Held {
        Held(Rc::new(RefCell::new(Vec::new())))
    }

    /// Posts every held event in arrival order and returns how many went out.
    pub fn replay(&self) -> usize {
        let events = self.drain();

        let mut posted = 0;
        for HeldEvent(event) in &events {
            mark_replayed(event);
            CGEvent::post(CGEventTapLocation::SessionEventTap, Some(event));
            posted += 1;
        }
        posted
    }

    fn drain(&self) -> Vec<HeldEvent> {
        let Held(queue) = self;
        let Ok(mut queue) = queue.try_borrow_mut() else {
            return Vec::new();
        };
        std::mem::take(&mut *queue)
    }

    /// Returns how many events are waiting to be replayed.
    pub fn len(&self) -> usize {
        let Held(queue) = self;
        let Ok(queue) = queue.try_borrow() else {
            return 0;
        };
        queue.len()
    }

    /// Returns whether nothing is waiting to be replayed.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn keep(&self, event: &CGEvent) -> Kept {
        let Held(queue) = self;
        let Some(event) = CGEvent::new_copy(Some(event)) else {
            return Kept::No;
        };
        let Ok(mut queue) = queue.try_borrow_mut() else {
            return Kept::No;
        };
        queue.push(HeldEvent(event));
        Kept::Yes
    }
}

type OnEvent = Box<dyn FnMut(KeyEvent, &CGEvent) -> Verdict>;

struct Context {
    on_event: RefCell<OnEvent>,
    held: Held,
    port: OnceCell<CFRetained<CFMachPort>>,
}

/// The installed keyboard tap, which stops watching the keyboard when it is dropped.
pub struct Tap {
    port: CFRetained<CFMachPort>,
    source: CFRetained<CFRunLoopSource>,
    #[allow(
        dead_code,
        reason = "the tap callback reaches it through the pointer install handed to Core Graphics"
    )]
    context: Box<Context>,
}

impl Drop for Tap {
    fn drop(&mut self) {
        let Tap {
            port,
            source,
            context: _,
        } = self;
        CGEvent::tap_enable(port, false);
        let Some(run_loop) = CFRunLoop::current() else {
            return;
        };
        run_loop.remove_source(Some(source), unsafe { kCFRunLoopDefaultMode });
    }
}

/// Installs the keyboard tap on the current run loop, holding into `held` whatever it keeps back.
///
/// # Errors
///
/// Returns [`TapError::NotCreated`] when Core Graphics refuses the tap, which is what a missing
/// Input Monitoring grant looks like, and [`TapError::NoSource`] or [`TapError::NoRunLoop`] when
/// the tap cannot become a source on this thread's run loop.
pub fn install(
    placement: Placement,
    held: Held,
    on_event: impl FnMut(KeyEvent, &CGEvent) -> Verdict + 'static,
) -> Result<Tap, TapError> {
    let context = Box::new(Context {
        on_event: RefCell::new(Box::new(on_event)),
        held,
        port: OnceCell::new(),
    });
    let user_info = (&*context as *const Context).cast_mut();

    let options = match placement {
        Placement::Listen => CGEventTapOptions::ListenOnly,
        Placement::Intercept => CGEventTapOptions::Default,
    };
    let port = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::SessionEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            options,
            KEYBOARD_MASK,
            Some(dispatch),
            user_info.cast::<c_void>(),
        )
    };
    let Some(port) = port else {
        return Err(TapError::NotCreated);
    };
    let Some(source) = CFMachPort::new_run_loop_source(None, Some(&port), 0) else {
        return Err(TapError::NoSource);
    };
    let Some(run_loop) = CFRunLoop::current() else {
        return Err(TapError::NoRunLoop);
    };

    let _ = context.port.set(port.clone());
    run_loop.add_source(Some(&source), unsafe { kCFRunLoopDefaultMode });
    CGEvent::tap_enable(&port, true);

    Ok(Tap {
        port,
        source,
        context,
    })
}

/// Prompts once for every grant moji is missing, and returns the ones still missing after that.
pub fn request_missing_access() -> Vec<Access> {
    let mut missing = Vec::new();
    if !CGPreflightListenEventAccess() {
        CGRequestListenEventAccess();
        missing.push(Access::Listen);
    }
    if !CGPreflightPostEventAccess() {
        CGRequestPostEventAccess();
        missing.push(Access::Post);
    }
    missing
}

/// Writes moji's magic into an event's source user data, so its own tap passes the replay through.
pub fn mark_replayed(event: &CGEvent) {
    CGEvent::set_integer_value_field(Some(event), CGEventField::EventSourceUserData, REPLAY_MAGIC);
}

/// Returns whether source user data carries moji's replay magic.
pub fn is_replayed(user_data: i64) -> bool {
    user_data == REPLAY_MAGIC
}

/// Returns the plain-data view of a keyboard event, or nothing when the event carries no key.
pub fn key_event(event: &CGEvent) -> Option<KeyEvent> {
    let kind = kind_of(CGEvent::r#type(Some(event)))?;
    let keycode = CGEvent::integer_value_field(Some(event), CGEventField::KeyboardEventKeycode);
    Some(KeyEvent {
        kind,
        keycode: keycode as u16,
        flags: CGEvent::flags(Some(event)).bits(),
        timestamp: CGEvent::timestamp(Some(event)),
    })
}

fn kind_of(kind: CGEventType) -> Option<EventKind> {
    if kind == CGEventType::KeyDown {
        return Some(EventKind::Down);
    }
    if kind == CGEventType::KeyUp {
        return Some(EventKind::Up);
    }
    if kind == CGEventType::FlagsChanged {
        return Some(EventKind::Flags);
    }
    None
}

fn user_data(event: &CGEvent) -> i64 {
    CGEvent::integer_value_field(Some(event), CGEventField::EventSourceUserData)
}

fn reenable(context: &Context) {
    let Context {
        on_event: _,
        held: _,
        port,
    } = context;
    let Some(port) = port.get() else {
        return;
    };
    tracing::warn!("the system disabled the keyboard tap, re-enabling it");
    CGEvent::tap_enable(port, true);
}

unsafe extern "C-unwind" fn dispatch(
    _proxy: CGEventTapProxy,
    kind: CGEventType,
    event: NonNull<CGEvent>,
    user_info: *mut c_void,
) -> *mut CGEvent {
    let context = user_info.cast::<Context>();
    let Some(context) = NonNull::new(context) else {
        return event.as_ptr();
    };
    let context = unsafe { context.as_ref() };

    if kind == CGEventType::TapDisabledByTimeout || kind == CGEventType::TapDisabledByUserInput {
        reenable(context);
        return std::ptr::null_mut();
    }

    let carried = unsafe { event.as_ref() };
    if is_replayed(user_data(carried)) {
        return event.as_ptr();
    }
    let Some(key) = key_event(carried) else {
        return event.as_ptr();
    };

    let Context {
        on_event,
        held,
        port: _,
    } = context;
    let Ok(mut on_event) = on_event.try_borrow_mut() else {
        return event.as_ptr();
    };
    let verdict = on_event(key, carried);
    drop(on_event);

    match verdict {
        Verdict::Pass => event.as_ptr(),
        Verdict::Swallow => std::ptr::null_mut(),
        Verdict::Hold => match held.keep(carried) {
            Kept::Yes => std::ptr::null_mut(),
            Kept::No => event.as_ptr(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEYCODE_A: u16 = 0;

    fn keyboard_event(keycode: u16, down: bool) -> CFRetained<CGEvent> {
        let Some(event) = CGEvent::new_keyboard_event(None, keycode, down) else {
            panic!("creating a keyboard event failed");
        };
        event
    }

    #[test]
    fn a_marked_event_reads_back_as_replayed() {
        let event = keyboard_event(KEYCODE_A, true);

        mark_replayed(&event);

        assert_eq!(user_data(&event), REPLAY_MAGIC);
        assert!(is_replayed(user_data(&event)));
    }

    #[test]
    fn an_untouched_event_is_not_a_replay() {
        let event = keyboard_event(KEYCODE_A, true);

        assert!(!is_replayed(user_data(&event)));
    }

    #[test]
    fn user_data_that_is_not_the_magic_is_not_a_replay() {
        assert!(!is_replayed(0));
        assert!(!is_replayed(REPLAY_MAGIC + 1));
        assert!(!is_replayed(-1));
    }

    #[test]
    fn a_key_down_reads_its_kind_and_keycode() {
        let event = keyboard_event(KEYCODE_A, true);

        let Some(KeyEvent {
            kind,
            keycode,
            flags: _,
            timestamp: _,
        }) = key_event(&event)
        else {
            panic!("a keyDown event is not a key event");
        };

        assert_eq!(kind, EventKind::Down);
        assert_eq!(keycode, KEYCODE_A);
    }

    #[test]
    fn a_key_up_reads_its_kind_and_keycode() {
        let event = keyboard_event(7, false);

        let Some(KeyEvent {
            kind,
            keycode,
            flags: _,
            timestamp: _,
        }) = key_event(&event)
        else {
            panic!("a keyUp event is not a key event");
        };

        assert_eq!(kind, EventKind::Up);
        assert_eq!(keycode, 7);
    }

    #[test]
    fn a_flags_change_reads_as_a_flags_event() {
        let event = keyboard_event(56, true);
        CGEvent::set_type(Some(&event), CGEventType::FlagsChanged);

        let Some(KeyEvent {
            kind,
            keycode,
            flags: _,
            timestamp: _,
        }) = key_event(&event)
        else {
            panic!("a flagsChanged event is not a key event");
        };

        assert_eq!(kind, EventKind::Flags);
        assert_eq!(keycode, 56);
    }

    #[test]
    fn an_event_that_carries_no_key_is_not_a_key_event() {
        let event = keyboard_event(KEYCODE_A, true);
        CGEvent::set_type(Some(&event), CGEventType::ScrollWheel);

        assert!(key_event(&event).is_none());
    }

    #[test]
    fn an_empty_queue_drains_to_nothing() {
        let held = Held::empty();

        assert!(held.is_empty());
        assert!(held.drain().is_empty());
    }

    #[test]
    fn a_kept_event_waits_until_it_is_replayed() {
        let held = Held::empty();
        let event = keyboard_event(KEYCODE_A, true);

        let Kept::Yes = held.keep(&event) else {
            panic!("copying a keyboard event for the queue failed");
        };

        assert_eq!(held.len(), 1);
        assert!(!is_replayed(user_data(&event)));
    }

    #[test]
    fn draining_empties_the_queue_and_keeps_arrival_order() {
        let held = Held::empty();
        let Kept::Yes = held.keep(&keyboard_event(KEYCODE_A, true)) else {
            panic!("copying a keyboard event for the queue failed");
        };
        let Kept::Yes = held.keep(&keyboard_event(7, true)) else {
            panic!("copying a keyboard event for the queue failed");
        };

        let drained = held.drain();
        let mut keycodes = Vec::new();
        for HeldEvent(event) in &drained {
            keycodes.push(CGEvent::integer_value_field(
                Some(event),
                CGEventField::KeyboardEventKeycode,
            ));
        }

        assert_eq!(keycodes, vec![i64::from(KEYCODE_A), 7]);
        assert!(held.is_empty());
        assert!(held.drain().is_empty());
    }

    #[test]
    fn a_queue_shares_what_it_holds_with_every_clone_of_it() {
        let held = Held::empty();
        let other = held.clone();

        let Kept::Yes = held.keep(&keyboard_event(KEYCODE_A, true)) else {
            panic!("copying a keyboard event for the queue failed");
        };

        assert_eq!(other.len(), 1);
        assert_eq!(other.drain().len(), 1);
        assert!(held.is_empty());
    }

    #[test]
    fn every_access_names_the_pane_that_grants_it() {
        assert!(Access::Listen.pane().contains("Input Monitoring"));
        assert!(Access::Post.pane().contains("Accessibility"));
    }
}
