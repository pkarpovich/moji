//! The session event tap: the only thing between a keystroke and the application that types it.
//!
//! The tap sees `keyDown`, `keyUp`, `flagsChanged` and the three mouse-down types at the session
//! level with head insert, so it is downstream of Karabiner's virtual keyboard and upstream of
//! every application. A click is reported so the history knows the caret moved, and is always
//! passed on. It hands each
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

use crate::barrier::{EventKind, KeyEvent, Stroke, Verdict};

const KEYBOARD_MASK: CGEventMask = (1 << CGEventType::KeyDown.0)
    | (1 << CGEventType::KeyUp.0)
    | (1 << CGEventType::FlagsChanged.0)
    | (1 << CGEventType::LeftMouseDown.0)
    | (1 << CGEventType::RightMouseDown.0)
    | (1 << CGEventType::OtherMouseDown.0);

const BACKSPACE_KEYCODE: u16 = 51;

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

impl HeldEvent {
    /// Returns a retained copy of the event a tap carried, or nothing when the copy fails.
    pub fn capture(event: &CGEvent) -> Option<HeldEvent> {
        let event = CGEvent::new_copy(Some(event))?;
        Some(HeldEvent(event))
    }

    /// Returns another retained copy of this event, or nothing when the copy fails.
    pub fn duplicate(&self) -> Option<HeldEvent> {
        let HeldEvent(event) = self;
        HeldEvent::capture(event)
    }
}

/// Whether an event reached the queue that holds it back.
pub enum Kept {
    /// The queue carries it now.
    Yes,
    /// Copying the event or reaching the queue failed, so the queue is unchanged.
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

    /// Pushes the keyDown of every captured stroke and its matching keyUp, in that order.
    ///
    /// Every copy is made before the queue is touched, so a copy that fails leaves the queue
    /// exactly as it was rather than holding half of what was asked for.
    pub fn push_strokes(&self, events: &[&HeldEvent]) -> Kept {
        let mut strokes = Vec::new();
        for event in events {
            let Some(down) = event.duplicate() else {
                return Kept::No;
            };
            let Some(up) = event.duplicate() else {
                return Kept::No;
            };
            let HeldEvent(raised) = &up;
            CGEvent::set_type(Some(raised), CGEventType::KeyUp);
            strokes.push(down);
            strokes.push(up);
        }

        let Held(queue) = self;
        let Ok(mut queue) = queue.try_borrow_mut() else {
            return Kept::No;
        };
        for stroke in strokes {
            queue.push(stroke);
        }
        Kept::Yes
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
    let autorepeat =
        CGEvent::integer_value_field(Some(event), CGEventField::KeyboardEventAutorepeat);
    let stroke = match autorepeat {
        0 => Stroke::First,
        _ => Stroke::Repeat,
    };
    Some(KeyEvent {
        kind,
        keycode: keycode as u16,
        flags: CGEvent::flags(Some(event)).bits(),
        timestamp: CGEvent::timestamp(Some(event)),
        stroke,
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
    if kind == CGEventType::LeftMouseDown
        || kind == CGEventType::RightMouseDown
        || kind == CGEventType::OtherMouseDown
    {
        return Some(EventKind::MouseDown);
    }
    None
}

fn backspace_pair() -> Option<(CFRetained<CGEvent>, CFRetained<CGEvent>)> {
    let down = CGEvent::new_keyboard_event(None, BACKSPACE_KEYCODE, true)?;
    let up = CGEvent::new_keyboard_event(None, BACKSPACE_KEYCODE, false)?;
    mark_replayed(&down);
    mark_replayed(&up);
    Some((down, up))
}

/// The backspace strokes a retype deletes with, all built before any of them goes out.
pub struct Deletions(Vec<HeldEvent>);

impl Deletions {
    /// Returns the strokes that delete `count` characters, or nothing when building one fails.
    ///
    /// Every stroke is built up front, so a failure posts nothing at all instead of leaving the
    /// text with some of the characters deleted and no replacement coming.
    pub fn built(count: usize) -> Option<Deletions> {
        let mut strokes = Vec::new();
        for _ in 0..count {
            let (down, up) = backspace_pair()?;
            strokes.push(HeldEvent(down));
            strokes.push(HeldEvent(up));
        }
        Some(Deletions(strokes))
    }

    /// Posts every stroke to the session tap, marked so moji's own tap passes it on.
    pub fn post(self) {
        let Deletions(strokes) = self;
        for HeldEvent(event) in &strokes {
            CGEvent::post(CGEventTapLocation::SessionEventTap, Some(event));
        }
    }
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
    use objc2_core_graphics::CGEventFlags;

    use super::*;

    const KEYCODE_A: u16 = 0;

    fn keycode_of(event: &CGEvent) -> i64 {
        CGEvent::integer_value_field(Some(event), CGEventField::KeyboardEventKeycode)
    }

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
            stroke: _,
        }) = key_event(&event)
        else {
            panic!("a keyDown event is not a key event");
        };

        assert_eq!(kind, EventKind::Down);
        assert_eq!(keycode, KEYCODE_A);
    }

    #[test]
    fn a_repeated_key_down_reads_as_a_repeat_stroke() {
        let event = keyboard_event(KEYCODE_A, true);
        CGEvent::set_integer_value_field(Some(&event), CGEventField::KeyboardEventAutorepeat, 1);

        let Some(KeyEvent {
            kind,
            keycode: _,
            flags: _,
            timestamp: _,
            stroke,
        }) = key_event(&event)
        else {
            panic!("a repeated keyDown event is not a key event");
        };

        assert_eq!(kind, EventKind::Down);
        assert_eq!(stroke, Stroke::Repeat);
    }

    #[test]
    fn a_key_up_reads_its_kind_and_keycode() {
        let event = keyboard_event(7, false);

        let Some(KeyEvent {
            kind,
            keycode,
            flags: _,
            timestamp: _,
            stroke: _,
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
            stroke: _,
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
    fn every_mouse_down_reads_as_a_mouse_down_event() {
        for kind in [
            CGEventType::LeftMouseDown,
            CGEventType::RightMouseDown,
            CGEventType::OtherMouseDown,
        ] {
            let event = keyboard_event(KEYCODE_A, true);
            CGEvent::set_type(Some(&event), kind);

            let Some(KeyEvent {
                kind,
                keycode: _,
                flags: _,
                timestamp: _,
                stroke: _,
            }) = key_event(&event)
            else {
                panic!("a mouse-down event is not a key event");
            };

            assert_eq!(kind, EventKind::MouseDown);
        }
    }

    #[test]
    fn a_captured_event_is_a_copy_carrying_the_same_keycode_and_flags() {
        let event = keyboard_event(7, true);
        CGEvent::set_flags(Some(&event), CGEventFlags::MaskShift);

        let Some(captured) = HeldEvent::capture(&event) else {
            panic!("capturing a keyboard event failed");
        };
        let Some(duplicate) = captured.duplicate() else {
            panic!("duplicating a captured event failed");
        };

        let HeldEvent(captured) = &captured;
        let HeldEvent(duplicate) = &duplicate;
        assert_eq!(keycode_of(captured), 7);
        assert_eq!(keycode_of(duplicate), 7);
        assert_eq!(
            CGEvent::flags(Some(duplicate)).bits(),
            CGEventFlags::MaskShift.bits()
        );
        assert!(!std::ptr::eq(&raw const *captured, &raw const *duplicate));
    }

    #[test]
    fn a_captured_event_survives_its_source_being_dropped() {
        let captured = {
            let event = keyboard_event(7, true);
            let Some(captured) = HeldEvent::capture(&event) else {
                panic!("capturing a keyboard event failed");
            };
            captured
        };

        let HeldEvent(event) = &captured;
        assert_eq!(keycode_of(event), 7);
    }

    #[test]
    fn a_pushed_stroke_is_a_key_down_then_a_key_up_of_the_same_key() {
        let held = Held::empty();
        let Some(captured) = HeldEvent::capture(&keyboard_event(7, true)) else {
            panic!("capturing a keyboard event failed");
        };

        let Kept::Yes = held.push_strokes(&[&captured]) else {
            panic!("pushing a captured stroke into the queue failed");
        };

        let drained = held.drain();
        let mut strokes = Vec::new();
        for HeldEvent(event) in &drained {
            strokes.push((CGEvent::r#type(Some(event)), keycode_of(event)));
        }

        assert_eq!(
            strokes,
            vec![(CGEventType::KeyDown, 7), (CGEventType::KeyUp, 7)]
        );
    }

    #[test]
    fn a_pushed_stroke_leaves_the_captured_event_a_key_down() {
        let held = Held::empty();
        let Some(captured) = HeldEvent::capture(&keyboard_event(7, true)) else {
            panic!("capturing a keyboard event failed");
        };

        let Kept::Yes = held.push_strokes(&[&captured, &captured]) else {
            panic!("pushing a captured stroke into the queue failed");
        };

        let HeldEvent(event) = &captured;
        assert_eq!(CGEvent::r#type(Some(event)), CGEventType::KeyDown);
        assert_eq!(held.len(), 4);
    }

    #[test]
    fn every_deletion_is_built_before_any_of_them_is_posted() {
        let Some(Deletions(strokes)) = Deletions::built(3) else {
            panic!("building the deletions failed");
        };

        let mut built = Vec::new();
        for HeldEvent(event) in &strokes {
            built.push((CGEvent::r#type(Some(event)), keycode_of(event)));
            assert!(is_replayed(user_data(event)));
        }

        let keycode = i64::from(BACKSPACE_KEYCODE);
        assert_eq!(
            built,
            vec![
                (CGEventType::KeyDown, keycode),
                (CGEventType::KeyUp, keycode),
                (CGEventType::KeyDown, keycode),
                (CGEventType::KeyUp, keycode),
                (CGEventType::KeyDown, keycode),
                (CGEventType::KeyUp, keycode),
            ]
        );
    }

    #[test]
    fn nothing_to_delete_builds_no_stroke_at_all() {
        let Some(Deletions(strokes)) = Deletions::built(0) else {
            panic!("building no deletion at all failed");
        };

        assert!(strokes.is_empty());
    }

    #[test]
    fn a_backspace_pair_is_keycode_fifty_one_marked_as_a_replay() {
        let Some((down, up)) = backspace_pair() else {
            panic!("creating a backspace pair failed");
        };

        assert_eq!(keycode_of(&down), i64::from(BACKSPACE_KEYCODE));
        assert_eq!(keycode_of(&up), i64::from(BACKSPACE_KEYCODE));
        assert_eq!(CGEvent::r#type(Some(&down)), CGEventType::KeyDown);
        assert_eq!(CGEvent::r#type(Some(&up)), CGEventType::KeyUp);
        assert!(is_replayed(user_data(&down)));
        assert!(is_replayed(user_data(&up)));
    }

    #[test]
    fn every_access_names_the_pane_that_grants_it() {
        assert!(Access::Listen.pane().contains("Input Monitoring"));
        assert!(Access::Post.pane().contains("Accessibility"));
    }
}
