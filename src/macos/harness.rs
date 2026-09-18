//! A window this process types into, so a live test can read back what a key actually produced.
//!
//! The harness exists for the translation spike and for the barrier's live tests: it owns an
//! `NSTextView` in a window of moji's own process, pumps the event queue by hand, and captures a
//! key through a listen-only tap so a replay can operate on a genuinely captured event. AppKit
//! refuses every one of those on anything but the main thread, so the only caller is
//! `tests/live.rs`, which owns `main`.

use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEventMask, NSTextView,
    NSWindow, NSWindowStyleMask,
};
use objc2_core_foundation::{
    CFMachPort, CFRetained, CFRunLoop, CFRunLoopSource, kCFRunLoopDefaultMode,
};
use objc2_core_graphics::{
    CGEvent, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSPoint, NSRect, NSSize, NSString};

const KEY_DOWN_MASK: CGEventMask = 1 << CGEventType::KeyDown.0;

/// The virtual keycode of the key that types `a` on a US layout and `ф` on a Russian one.
pub const KEYCODE_A: u16 = 0;

/// Whether a posted keyboard event is a press or a release.
pub enum Stroke {
    /// A key going down.
    Down,
    /// A key coming back up.
    Up,
}

/// A window of moji's own process with a text view the tests type into.
pub struct Window {
    app: Retained<NSApplication>,
    window: Retained<NSWindow>,
    view: Retained<NSTextView>,
}

impl Window {
    /// Opens the window, brings this process to the front and makes the text view first responder.
    ///
    /// # Panics
    ///
    /// Panics when it is not called on the main thread: AppKit refuses to instantiate an
    /// `NSWindow` anywhere else, and cargo's own test harness runs every test on a worker thread.
    pub fn open() -> Window {
        let Some(mtm) = MainThreadMarker::new() else {
            panic!("the harness window needs the main thread: run it from the live target");
        };
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        app.finishLaunching();

        let frame = NSRect::new(NSPoint::new(160.0, 160.0), NSSize::new(480.0, 200.0));
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Titled,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        window.setTitle(&NSString::from_str("moji harness"));

        let view = NSTextView::initWithFrame(
            NSTextView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), frame.size),
        );
        window.setContentView(Some(&view));
        window.makeFirstResponder(Some(&view));

        let window = Window { app, window, view };
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            window.come_to_the_front();
            window.pump(Duration::from_millis(50));
            if window.is_frontmost() {
                return window;
            }
            if Instant::now() >= deadline {
                panic!(
                    "the harness window never became frontmost, so a keystroke would go elsewhere"
                );
            }
        }
    }

    fn come_to_the_front(&self) {
        let Window {
            app,
            window,
            view: _,
        } = self;
        window.makeKeyAndOrderFront(None);
        #[allow(
            deprecated,
            reason = "activate() leaves an unbundled binary behind the terminal that started it"
        )]
        app.activateIgnoringOtherApps(true);
    }

    /// Returns everything the text view holds right now.
    pub fn typed_text(&self) -> String {
        let Window {
            app: _,
            window: _,
            view,
        } = self;
        view.string().to_string()
    }

    /// Puts `text` into the text view without going through the keyboard at all.
    pub fn set_text(&self, text: &str) {
        let Window {
            app: _,
            window: _,
            view,
        } = self;
        view.setString(&NSString::from_str(text));
    }

    fn is_frontmost(&self) -> bool {
        let Window {
            app,
            window,
            view: _,
        } = self;
        app.isActive() && window.isKeyWindow()
    }

    /// Empties the text view, so the next keystroke is the only thing it holds.
    pub fn clear(&self) {
        let Window {
            app: _,
            window: _,
            view,
        } = self;
        view.setString(&NSString::from_str(""));
    }

    /// Dispatches window server events and run loop sources for `duration`, then returns.
    pub fn pump(&self, duration: Duration) {
        let Window {
            app,
            window: _,
            view: _,
        } = self;
        let deadline = Instant::now() + duration;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            let slice = deadline.duration_since(now).min(Duration::from_millis(10));
            let expiration = NSDate::dateWithTimeIntervalSinceNow(slice.as_secs_f64());
            let event = app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&expiration),
                unsafe { NSDefaultRunLoopMode },
                true,
            );
            let Some(event) = event else {
                continue;
            };
            app.sendEvent(&event);
            app.updateWindows();
        }
    }

    /// Pumps until the text view holds something, or until `timeout` passes with it still empty.
    pub fn wait_for_text(&self, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        loop {
            let typed = self.typed_text();
            if !typed.is_empty() {
                return typed;
            }
            if Instant::now() >= deadline {
                return typed;
            }
            self.pump(Duration::from_millis(10));
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let Window {
            app: _,
            window,
            view: _,
        } = self;
        window.close();
    }
}

/// A listen-only tap that keeps the first keyDown it sees, as a retained copy.
pub struct KeyCapture {
    tap: CFRetained<CFMachPort>,
    source: CFRetained<CFRunLoopSource>,
    captured: Box<RefCell<Option<CFRetained<CGEvent>>>>,
}

impl KeyCapture {
    /// Returns the captured keyDown, leaving the capture empty for the next one.
    pub fn take(&self) -> Option<CFRetained<CGEvent>> {
        let KeyCapture {
            tap: _,
            source: _,
            captured,
        } = self;
        captured.borrow_mut().take()
    }
}

impl Drop for KeyCapture {
    fn drop(&mut self) {
        let KeyCapture {
            tap,
            source,
            captured: _,
        } = self;
        CGEvent::tap_enable(tap, false);
        let Some(run_loop) = CFRunLoop::current() else {
            return;
        };
        run_loop.remove_source(Some(source), unsafe { kCFRunLoopDefaultMode });
    }
}

unsafe extern "C-unwind" fn keep_first_key_down(
    _proxy: CGEventTapProxy,
    _kind: CGEventType,
    event: NonNull<CGEvent>,
    user_info: *mut c_void,
) -> *mut CGEvent {
    let captured = user_info.cast::<RefCell<Option<CFRetained<CGEvent>>>>();
    let Some(captured) = NonNull::new(captured) else {
        return event.as_ptr();
    };
    let captured = unsafe { captured.as_ref() };
    let mut captured = captured.borrow_mut();
    if captured.is_none() {
        *captured = CGEvent::new_copy(Some(unsafe { event.as_ref() }));
    }
    event.as_ptr()
}

/// Installs a listen-only tap on the current run loop that keeps the next keyDown it sees.
///
/// # Panics
///
/// Panics when the tap cannot be created, which on this machine means the process running the
/// tests has no Input Monitoring grant.
pub fn capture_next_key() -> KeyCapture {
    let captured: Box<RefCell<Option<CFRetained<CGEvent>>>> = Box::new(RefCell::new(None));
    let user_info = (&*captured as *const RefCell<Option<CFRetained<CGEvent>>>).cast_mut();
    let tap = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::SessionEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::ListenOnly,
            KEY_DOWN_MASK,
            Some(keep_first_key_down),
            user_info.cast::<c_void>(),
        )
    };
    let Some(tap) = tap else {
        panic!("creating a listen-only event tap failed: this process has no Input Monitoring");
    };
    let Some(source) = CFMachPort::new_run_loop_source(None, Some(&tap), 0) else {
        panic!("the event tap has no run loop source");
    };
    let Some(run_loop) = CFRunLoop::current() else {
        panic!("this thread has no run loop");
    };
    run_loop.add_source(Some(&source), unsafe { kCFRunLoopDefaultMode });
    CGEvent::tap_enable(&tap, true);

    KeyCapture {
        tap,
        source,
        captured,
    }
}

/// Posts a freshly created keyboard event for `keycode` at the session tap.
///
/// # Panics
///
/// Panics when Core Graphics refuses to create the event.
pub fn post_key(keycode: u16, stroke: Stroke) {
    let down = match stroke {
        Stroke::Down => true,
        Stroke::Up => false,
    };
    let Some(event) = CGEvent::new_keyboard_event(None, keycode, down) else {
        panic!("creating a keyboard event for keycode {keycode} failed");
    };
    post(&event);
}

/// Returns an owned copy of an event, which is what holding one for a replay needs.
pub fn copy(event: &CGEvent) -> Option<CFRetained<CGEvent>> {
    CGEvent::new_copy(Some(event))
}

/// Posts an already built event at the session tap, which is where a replay would post it.
pub fn post(event: &CGEvent) {
    CGEvent::post(CGEventTapLocation::SessionEventTap, Some(event));
}

/// Returns the unicode string an event carries, which is empty for most synthetic events.
pub fn unicode_string(event: &CGEvent) -> String {
    let mut length = 0;
    unsafe {
        CGEvent::keyboard_get_unicode_string(Some(event), 0, &mut length, std::ptr::null_mut());
    }
    if length == 0 {
        return String::new();
    }

    let mut units = vec![0u16; length as usize];
    let mut written = 0;
    unsafe {
        CGEvent::keyboard_get_unicode_string(Some(event), length, &mut written, units.as_mut_ptr());
    }
    units.truncate(written as usize);
    String::from_utf16_lossy(&units)
}

/// Rewrites the unicode string an event carries, which is how strategy (c) replays a held key.
pub fn set_unicode_string(event: &CGEvent, text: &str) {
    let mut units = Vec::new();
    for unit in text.encode_utf16() {
        units.push(unit);
    }
    unsafe {
        CGEvent::keyboard_set_unicode_string(Some(event), units.len() as u64, units.as_ptr());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_string_sets_no_unicode_units() {
        let Some(event) = CGEvent::new_keyboard_event(None, KEYCODE_A, true) else {
            panic!("creating a keyboard event failed");
        };

        set_unicode_string(&event, "");
        assert_eq!(unicode_string(&event), "");
    }

    #[test]
    fn a_rewritten_unicode_string_reads_back() {
        let Some(event) = CGEvent::new_keyboard_event(None, KEYCODE_A, true) else {
            panic!("creating a keyboard event failed");
        };

        set_unicode_string(&event, "\u{444}");
        assert_eq!(unicode_string(&event), "\u{444}");
    }

    #[test]
    fn a_synthetic_keyboard_event_carries_no_unicode_string_of_its_own() {
        let Some(event) = CGEvent::new_keyboard_event(None, KEYCODE_A, true) else {
            panic!("creating a keyboard event failed");
        };

        assert_eq!(unicode_string(&event), "");
    }
}
