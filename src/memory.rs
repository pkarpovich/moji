//! Which layout each application gets, as a pure policy over plain data.
//!
//! A pinned application always gets its pin; every other one gets the layout it was last used
//! with, which is recorded when it loses focus and whenever the layout changes while it is in
//! front. Nothing here selects anything: the daemon takes the decision to the barrier first, and
//! only a barrier that accepts it makes the daemon call Text Input Sources.

use std::collections::BTreeMap;

use crate::macos::tis::LayoutTag;
use crate::macos::workspace::BundleId;

/// The layout each application was last used with, plus the pins that override it.
pub struct Memory {
    pins: BTreeMap<BundleId, LayoutTag>,
    remembered: BTreeMap<BundleId, LayoutTag>,
    frontmost: Option<BundleId>,
}

impl Memory {
    /// Creates a memory in which `pins` fixes the layout of a bundle id for good.
    pub fn new(pins: BTreeMap<BundleId, LayoutTag>) -> Memory {
        Memory {
            pins,
            remembered: BTreeMap::new(),
            frontmost: None,
        }
    }

    /// Records the layout `app` is using now; an unmapped layout records nothing.
    pub fn on_layout_changed(&mut self, app: BundleId, layout: Option<LayoutTag>) {
        let Some(layout) = layout else {
            return;
        };
        self.remembered.insert(app, layout);
    }

    /// Records what the application losing focus was using and returns what `app` needs.
    ///
    /// [`None`] means the daemon must not select anything: either the application is unknown, or
    /// what it wants is already selected, and selecting the selected layout is confirmed by no
    /// notification at all.
    pub fn on_activated(&mut self, app: BundleId, current: Option<LayoutTag>) -> Option<LayoutTag> {
        let losing = self.frontmost.replace(app.clone());
        if let Some(losing) = losing {
            self.on_layout_changed(losing, current.clone());
        }

        let decided = self.decide(&app)?;
        if Some(&decided) == current.as_ref() {
            return None;
        }
        Some(decided)
    }

    fn decide(&self, app: &BundleId) -> Option<LayoutTag> {
        let pinned = self.pins.get(app);
        if let Some(pinned) = pinned {
            return Some(pinned.clone());
        }
        let remembered = self.remembered.get(app)?;
        Some(remembered.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(tag: &str) -> LayoutTag {
        LayoutTag(tag.to_string())
    }

    fn bundle(bundle: &str) -> BundleId {
        BundleId(bundle.to_string())
    }

    fn pinned(pairs: &[(&str, &str)]) -> Memory {
        let mut pins = BTreeMap::new();
        for (app, layout) in pairs {
            pins.insert(bundle(app), tag(layout));
        }
        Memory::new(pins)
    }

    #[test]
    fn a_pinned_app_gets_its_pin_however_it_was_last_used() {
        let mut memory = pinned(&[("com.brnbw.Tuna", "en")]);
        memory.on_layout_changed(bundle("com.brnbw.Tuna"), Some(tag("ru")));

        assert_eq!(
            memory.on_activated(bundle("com.brnbw.Tuna"), Some(tag("ru"))),
            Some(tag("en"))
        );
    }

    #[test]
    fn a_remembered_app_gets_the_layout_that_was_recorded_for_it() {
        let mut memory = pinned(&[]);
        memory.on_layout_changed(bundle("com.tinyspeck.slackmacgap"), Some(tag("ru")));

        assert_eq!(
            memory.on_activated(bundle("com.tinyspeck.slackmacgap"), Some(tag("en"))),
            Some(tag("ru"))
        );
    }

    #[test]
    fn an_app_nothing_is_known_about_keeps_whatever_is_selected() {
        let mut memory = pinned(&[("com.brnbw.Tuna", "en")]);

        assert_eq!(
            memory.on_activated(bundle("com.apple.Safari"), Some(tag("ru"))),
            None
        );
    }

    #[test]
    fn recording_for_one_app_leaves_another_alone() {
        let mut memory = pinned(&[]);
        memory.on_layout_changed(bundle("com.apple.Safari"), Some(tag("ru")));

        assert_eq!(
            memory.on_activated(bundle("com.apple.Terminal"), Some(tag("en"))),
            None
        );
        assert_eq!(
            memory.on_activated(bundle("com.apple.Safari"), Some(tag("en"))),
            Some(tag("ru"))
        );
    }

    #[test]
    fn an_app_that_never_switched_still_comes_back_to_its_layout() {
        let mut memory = pinned(&[("com.brnbw.Tuna", "en")]);

        assert_eq!(
            memory.on_activated(bundle("com.tinyspeck.slackmacgap"), Some(tag("ru"))),
            None
        );
        assert_eq!(
            memory.on_activated(bundle("com.brnbw.Tuna"), Some(tag("ru"))),
            Some(tag("en"))
        );
        assert_eq!(
            memory.on_activated(bundle("com.tinyspeck.slackmacgap"), Some(tag("en"))),
            Some(tag("ru"))
        );
    }

    #[test]
    fn a_pin_that_is_already_selected_selects_nothing() {
        let mut memory = pinned(&[("com.brnbw.Tuna", "en")]);

        assert_eq!(
            memory.on_activated(bundle("com.brnbw.Tuna"), Some(tag("en"))),
            None
        );
    }

    #[test]
    fn a_memory_that_is_already_selected_selects_nothing() {
        let mut memory = pinned(&[]);
        memory.on_layout_changed(bundle("com.apple.Safari"), Some(tag("ru")));

        assert_eq!(
            memory.on_activated(bundle("com.apple.Safari"), Some(tag("ru"))),
            None
        );
    }

    #[test]
    fn an_unmapped_current_layout_records_nothing_and_still_answers() {
        let mut memory = pinned(&[("com.brnbw.Tuna", "en")]);
        memory.on_layout_changed(bundle("com.apple.Safari"), None);

        assert_eq!(memory.on_activated(bundle("com.apple.Safari"), None), None);
        assert_eq!(
            memory.on_activated(bundle("com.brnbw.Tuna"), None),
            Some(tag("en"))
        );
        assert_eq!(memory.on_activated(bundle("com.apple.Safari"), None), None);
    }

    #[test]
    fn the_first_activation_has_no_predecessor_to_record_for() {
        let mut memory = pinned(&[]);

        assert_eq!(
            memory.on_activated(bundle("com.apple.Safari"), Some(tag("ru"))),
            None
        );
    }

    #[test]
    fn a_layout_change_records_for_the_app_it_names() {
        let mut memory = pinned(&[]);
        memory.on_layout_changed(bundle("com.apple.Safari"), Some(tag("ru")));
        memory.on_layout_changed(bundle("com.apple.Safari"), Some(tag("en")));

        assert_eq!(
            memory.on_activated(bundle("com.apple.Safari"), Some(tag("ru"))),
            Some(tag("en"))
        );
    }
}
