//! NSWorkspace: the frontmost application, and the bundle id an application is known by.
//!
//! The frontmost application is only the fallback for [`super::focus`], which follows the keyboard
//! rather than the activation: a launcher panel takes the keyboard without ever becoming frontmost.

use std::fmt;

use objc2_app_kit::{NSRunningApplication, NSWorkspace};

/// The bundle identifier of an application, which is what the configuration pins a layout to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BundleId(pub String);

impl fmt::Display for BundleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let BundleId(bundle) = self;
        formatter.write_str(bundle)
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

pub(super) fn bundle_id(application: &NSRunningApplication) -> Option<BundleId> {
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
