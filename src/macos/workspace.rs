//! NSWorkspace, the only door moji has to which application the user is typing into.
//!
//! The activation notification is posted on the main thread, and the block is registered with no
//! operation queue, so it runs there too: the same run loop every other source of the daemon lives
//! on, which is what lets the memory policy and the barrier stay free of locks.

use std::fmt;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2_app_kit::{
    NSRunningApplication, NSWorkspace, NSWorkspaceApplicationKey,
    NSWorkspaceDidActivateApplicationNotification,
};
use objc2_foundation::NSNotification;

/// The bundle identifier of an application, which is what the configuration pins a layout to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BundleId(pub String);

impl fmt::Display for BundleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let BundleId(bundle) = self;
        formatter.write_str(bundle)
    }
}

/// Removes the activation observer from the workspace notification center when dropped.
pub struct ActivationObserver {
    token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

impl Drop for ActivationObserver {
    fn drop(&mut self) {
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        unsafe { center.removeObserver(self.token.as_ref()) };
    }
}

/// Returns the bundle id of the application that is frontmost right now.
///
/// An application without an `Info.plist`, which is what an unbundled binary is, carries no bundle
/// id at all and is reported as [`None`].
pub fn frontmost() -> Option<BundleId> {
    let application = NSWorkspace::sharedWorkspace().frontmostApplication()?;
    bundle_id(&application)
}

/// Calls `on_activate` on the main run loop whenever another application comes to the front.
pub fn observe_activation(on_activate: impl Fn(BundleId) + 'static) -> ActivationObserver {
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    let block = RcBlock::new(move |notification: NonNull<NSNotification>| {
        let notification = unsafe { notification.as_ref() };
        let Some(activated) = activated_application(notification) else {
            return;
        };
        on_activate(activated);
    });
    let token = unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(NSWorkspaceDidActivateApplicationNotification),
            None,
            None,
            &block,
        )
    };
    ActivationObserver { token }
}

fn activated_application(notification: &NSNotification) -> Option<BundleId> {
    let user_info = notification.userInfo()?;
    let key: &AnyObject = unsafe { NSWorkspaceApplicationKey }.as_ref();
    let application = user_info.objectForKey(key)?;
    let application = application.downcast_ref::<NSRunningApplication>()?;
    bundle_id(application)
}

fn bundle_id(application: &NSRunningApplication) -> Option<BundleId> {
    let bundle = application.bundleIdentifier()?;
    Some(BundleId(bundle.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bundle_id_prints_as_itself() {
        let bundle = BundleId("com.brnbw.Tuna".to_string());

        assert_eq!(bundle.to_string(), "com.brnbw.Tuna");
    }

    #[test]
    fn two_bundle_ids_are_equal_only_when_they_spell_the_same() {
        assert_eq!(
            BundleId("com.brnbw.Tuna".to_string()),
            BundleId("com.brnbw.Tuna".to_string())
        );
        assert_ne!(
            BundleId("com.brnbw.Tuna".to_string()),
            BundleId("com.brnbw.tuna".to_string())
        );
    }
}
