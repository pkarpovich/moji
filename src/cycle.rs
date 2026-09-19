//! The layout cycle, as a step from one tag to the next.
//!
//! Nothing here touches Core Foundation or Text Input Sources: the step takes the configured
//! cycle plus the tag selected right now, and returns the tag that comes after it.

use crate::macos::tis::LayoutTag;

/// Returns the layout after `current` in the cycle, wrapping.
///
/// A layout outside the cycle, and an unmapped one, both go to the cycle's first entry. An empty
/// cycle has no next layout at all.
pub fn next(cycle: &[LayoutTag], current: Option<&LayoutTag>) -> Option<LayoutTag> {
    let first = cycle.first()?;
    let Some(current) = current else {
        return Some(first.clone());
    };

    let mut found = None;
    for (index, candidate) in cycle.iter().enumerate() {
        if candidate == current {
            found = Some(index);
            break;
        }
    }

    let Some(index) = found else {
        return Some(first.clone());
    };
    let index = (index + 1) % cycle.len();
    Some(cycle[index].clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(tag: &str) -> LayoutTag {
        LayoutTag(tag.to_string())
    }

    #[test]
    fn the_cycle_wraps() {
        let cycle = vec![tag("en"), tag("ru")];

        assert_eq!(next(&cycle, Some(&tag("en"))), Some(tag("ru")));
        assert_eq!(next(&cycle, Some(&tag("ru"))), Some(tag("en")));
    }

    #[test]
    fn a_layout_outside_the_cycle_goes_to_its_first_entry() {
        let cycle = vec![tag("en"), tag("ru")];

        assert_eq!(next(&cycle, Some(&tag("de"))), Some(tag("en")));
    }

    #[test]
    fn an_unmapped_layout_goes_to_the_first_entry_of_the_cycle() {
        let cycle = vec![tag("en"), tag("ru")];

        assert_eq!(next(&cycle, None), Some(tag("en")));
    }

    #[test]
    fn an_empty_cycle_has_no_next_layout() {
        assert_eq!(next(&[], None), None);
        assert_eq!(next(&[], Some(&tag("en"))), None);
    }
}
