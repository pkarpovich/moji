//! Which application the keyboard goes to right now, asked of Accessibility rather than of the
//! workspace.
//!
//! NSWorkspace announces an application only when it activates, and a launcher such as Tuna shows
//! its panel without activating: the panel takes the keyboard while the workspace still names the
//! window behind it. Accessibility's system-wide focused application follows the keyboard instead,
//! and it moves to the panel and back. There is no notification for it, so the daemon polls.

use std::ptr::NonNull;
use std::time::Duration;

use objc2_app_kit::NSRunningApplication;
use objc2_application_services::{AXError, AXUIElement};
use objc2_core_foundation::{CFRetained, CFString, CFType};

use super::workspace::{self, BundleId};

/// How often the daemon asks where the keyboard goes.
pub const POLL: Duration = Duration::from_millis(50);

const MESSAGING_TIMEOUT_SECONDS: f32 = 0.2;
const ATTRIBUTE_FOCUSED_APPLICATION: &str = "AXFocusedApplication";

/// Returns the bundle id of the application the keyboard goes to, or of the frontmost one when
/// Accessibility does not answer.
pub fn focused() -> Option<BundleId> {
    let Some(pid) = focused_pid() else {
        return workspace::frontmost();
    };
    let application = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
    workspace::bundle_id(&application)
}

/// Returns the application the keyboard moved to since `last`, if it moved at all.
///
/// An unreadable focus is not a move: the last known application stands until a new one is read.
pub fn moved(last: Option<&BundleId>, now: Option<BundleId>) -> Option<BundleId> {
    let now = now?;
    match last {
        Some(last) if *last == now => None,
        Some(_) => Some(now),
        None => Some(now),
    }
}

fn focused_pid() -> Option<i32> {
    let system_wide = unsafe { AXUIElement::new_system_wide() };
    let status = unsafe { system_wide.set_messaging_timeout(MESSAGING_TIMEOUT_SECONDS) };
    if status != AXError::Success {
        tracing::debug!(status = status.0, "could not set the accessibility timeout");
    }
    let element = attribute_element(&system_wide, ATTRIBUTE_FOCUSED_APPLICATION)?;
    let mut pid: i32 = 0;
    let status = unsafe { element.pid(NonNull::from(&mut pid)) };
    if status != AXError::Success {
        return None;
    }
    if pid <= 0 {
        return None;
    }
    Some(pid)
}

fn attribute_element(element: &AXUIElement, attribute: &str) -> Option<CFRetained<AXUIElement>> {
    let name = CFString::from_str(attribute);
    let mut value: *const CFType = std::ptr::null();
    let status = unsafe { element.copy_attribute_value(&name, NonNull::from(&mut value)) };
    if status != AXError::Success {
        return None;
    }
    let value = NonNull::new(value.cast_mut())?;
    let value = unsafe { CFRetained::from_raw(value) };
    value.downcast::<AXUIElement>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(bundle: &str) -> BundleId {
        BundleId(bundle.to_string())
    }

    #[test]
    fn the_keyboard_moving_to_another_application_is_reported() {
        assert_eq!(
            moved(
                Some(&app("com.umputun.agterm")),
                Some(app("com.brnbw.Tuna"))
            ),
            Some(app("com.brnbw.Tuna"))
        );
    }

    #[test]
    fn the_same_application_again_is_not_a_move() {
        assert_eq!(
            moved(Some(&app("com.brnbw.Tuna")), Some(app("com.brnbw.Tuna"))),
            None
        );
    }

    #[test]
    fn the_first_reading_is_a_move() {
        assert_eq!(
            moved(None, Some(app("com.brnbw.Tuna"))),
            Some(app("com.brnbw.Tuna"))
        );
    }

    #[test]
    fn an_unreadable_focus_leaves_the_last_application_standing() {
        assert_eq!(moved(Some(&app("com.brnbw.Tuna")), None), None);
        assert_eq!(moved(None, None), None);
    }
}
