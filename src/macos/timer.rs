//! A run loop timer, so the barrier's deadline is a source on the main thread like every other.
//!
//! The daemon has no second thread and no async runtime, so the watchdog that releases held keys
//! after 50 ms is a `CFRunLoopTimer` the tap layer arms. A timer that never repeats becomes
//! invalid the moment it fires, so a quiet timer here is one whose interval is a day and whose
//! next fire date is pushed that far out; arming it is a fire date, not a new timer.

use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::time::Duration;

use objc2_core_foundation::{
    CFAbsoluteTimeGetCurrent, CFRetained, CFRunLoop, CFRunLoopTimer, CFRunLoopTimerContext,
    kCFRunLoopDefaultMode,
};

const QUIET: f64 = 86_400.0;

/// Whether a timer waits to be armed, or fires on its own for as long as it lives.
pub enum Repeat {
    /// Stays quiet until [`Timer::arm`] names a moment.
    Never,
    /// Fires every interval, the first time one interval from now.
    Every(Duration),
}

/// What can go wrong when installing a run loop timer.
#[derive(Debug, thiserror::Error)]
pub enum TimerError {
    /// Core Foundation refused to create the timer.
    #[error("creating the run loop timer failed")]
    NotCreated,
    /// This thread has no run loop, so the timer would never fire.
    #[error("this thread has no run loop, so a timer on it would never fire")]
    NoRunLoop,
}

/// A timer on the current run loop that calls the closure it was installed with.
pub struct Timer {
    timer: CFRetained<CFRunLoopTimer>,
    #[allow(
        dead_code,
        reason = "the timer callback reaches it through the pointer install handed Core Foundation"
    )]
    on_fire: Box<RefCell<Box<dyn FnMut()>>>,
}

impl Timer {
    /// Installs a timer on the current run loop.
    ///
    /// # Errors
    ///
    /// Returns [`TimerError::NotCreated`] when Core Foundation refuses the timer, and
    /// [`TimerError::NoRunLoop`] when the calling thread has none.
    pub fn install(repeat: Repeat, on_fire: impl FnMut() + 'static) -> Result<Timer, TimerError> {
        let on_fire: Box<RefCell<Box<dyn FnMut()>>> = Box::new(RefCell::new(Box::new(on_fire)));
        let info = (&*on_fire as *const RefCell<Box<dyn FnMut()>>).cast_mut();

        let interval = match repeat {
            Repeat::Never => QUIET,
            Repeat::Every(interval) => interval.as_secs_f64(),
        };
        let mut context = CFRunLoopTimerContext {
            version: 0,
            info: info.cast::<c_void>(),
            retain: None,
            release: None,
            copyDescription: None,
        };
        let timer = unsafe {
            CFRunLoopTimer::new(
                None,
                CFAbsoluteTimeGetCurrent() + interval,
                interval,
                0,
                0,
                Some(fire),
                &mut context,
            )
        };
        let Some(timer) = timer else {
            return Err(TimerError::NotCreated);
        };
        let Some(run_loop) = CFRunLoop::current() else {
            return Err(TimerError::NoRunLoop);
        };
        run_loop.add_timer(Some(&timer), unsafe { kCFRunLoopDefaultMode });

        Ok(Timer { timer, on_fire })
    }

    /// Makes the timer fire once, `after` from now, replacing whatever was pending.
    pub fn arm(&self, after: Duration) {
        let Timer { timer, on_fire: _ } = self;
        timer.set_next_fire_date(CFAbsoluteTimeGetCurrent() + after.as_secs_f64());
    }

    /// Pushes the next fire far enough out that the timer stays quiet until it is armed again.
    pub fn disarm(&self) {
        let Timer { timer, on_fire: _ } = self;
        timer.set_next_fire_date(CFAbsoluteTimeGetCurrent() + QUIET);
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        let Timer { timer, on_fire: _ } = self;
        timer.invalidate();
    }
}

unsafe extern "C-unwind" fn fire(_timer: *mut CFRunLoopTimer, info: *mut c_void) {
    let on_fire = info.cast::<RefCell<Box<dyn FnMut()>>>();
    let Some(on_fire) = NonNull::new(on_fire) else {
        return;
    };
    let on_fire = unsafe { on_fire.as_ref() };
    let Ok(mut on_fire) = on_fire.try_borrow_mut() else {
        return;
    };
    on_fire();
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use objc2_core_foundation::kCFRunLoopDefaultMode;

    use super::*;

    fn pump(duration: Duration) {
        CFRunLoop::run_in_mode(
            unsafe { kCFRunLoopDefaultMode },
            duration.as_secs_f64(),
            false,
        );
    }

    #[test]
    fn a_timer_that_was_never_armed_stays_quiet() {
        let fired = Rc::new(Cell::new(0usize));
        let seen = Rc::clone(&fired);
        let timer = Timer::install(Repeat::Never, move || seen.set(seen.get() + 1));
        let Ok(timer) = timer else {
            panic!("installing a run loop timer failed");
        };

        pump(Duration::from_millis(50));

        assert_eq!(fired.get(), 0);
        drop(timer);
    }

    #[test]
    fn an_armed_timer_fires_once() {
        let fired = Rc::new(Cell::new(0usize));
        let seen = Rc::clone(&fired);
        let timer = Timer::install(Repeat::Never, move || seen.set(seen.get() + 1));
        let Ok(timer) = timer else {
            panic!("installing a run loop timer failed");
        };

        timer.arm(Duration::from_millis(10));
        pump(Duration::from_millis(200));

        assert_eq!(fired.get(), 1, "an armed one-shot timer did not fire once");
        drop(timer);
    }

    #[test]
    fn disarming_before_the_deadline_stops_the_fire() {
        let fired = Rc::new(Cell::new(0usize));
        let seen = Rc::clone(&fired);
        let timer = Timer::install(Repeat::Never, move || seen.set(seen.get() + 1));
        let Ok(timer) = timer else {
            panic!("installing a run loop timer failed");
        };

        timer.arm(Duration::from_millis(100));
        timer.disarm();
        pump(Duration::from_millis(200));

        assert_eq!(fired.get(), 0, "a disarmed timer fired anyway");
        drop(timer);
    }

    #[test]
    fn a_repeating_timer_fires_more_than_once() {
        let fired = Rc::new(Cell::new(0usize));
        let seen = Rc::clone(&fired);
        let timer = Timer::install(Repeat::Every(Duration::from_millis(10)), move || {
            seen.set(seen.get() + 1)
        });
        let Ok(timer) = timer else {
            panic!("installing a repeating run loop timer failed");
        };

        pump(Duration::from_millis(200));

        assert!(
            fired.get() > 1,
            "a repeating timer fired {} times in 200 ms",
            fired.get()
        );
        drop(timer);
    }

    #[test]
    fn a_dropped_timer_never_fires_again() {
        let fired = Rc::new(Cell::new(0usize));
        let seen = Rc::clone(&fired);
        let timer = Timer::install(Repeat::Every(Duration::from_millis(10)), move || {
            seen.set(seen.get() + 1)
        });
        let Ok(timer) = timer else {
            panic!("installing a repeating run loop timer failed");
        };

        drop(timer);
        pump(Duration::from_millis(100));

        assert_eq!(fired.get(), 0, "a dropped timer kept firing");
    }
}
