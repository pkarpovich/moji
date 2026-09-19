//! The history of what was typed, as plain data the flip reads.
//!
//! Nothing here touches Core Foundation or Text Input Sources: the history takes keyboard events
//! as plain data plus the tag they were typed in, and carries whatever the tap layer needs to
//! replay them as an opaque payload. That split is what makes the flip testable without a tap.

use crate::barrier::{EventKind, KeyEvent, RETYPE_KEYCODE, SWITCH_KEYCODE};
use crate::cycle::next;
use crate::macos::tis::LayoutTag;

/// How many keystrokes the history keeps before the oldest one is dropped.
pub const CAP: usize = 512;

const COMMAND: u64 = 1 << 20;
const CONTROL: u64 = 1 << 18;
const DELETE: u16 = 51;
const SPACE: u16 = 49;
const CLEARING: [u16; 13] = [36, 76, 48, 53, 115, 116, 119, 121, 117, 123, 124, 125, 126];

/// What a recorded keystroke contributes to a word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A key that types a letter of the word.
    Letter,
    /// The space that ends a word.
    Space,
}

/// What one keyboard event asks the history to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Append the keystroke under the layout selected right now.
    Record(Kind),
    /// Drop the last keystroke, as the delete key did downstream.
    Erase,
    /// Forget everything: the caret moved somewhere the history cannot follow.
    Clear,
    /// Leave the history alone.
    Ignore,
}

/// How much to retype, and the layout to retype it in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flip {
    /// How many keystrokes the flip covers, counted back from the last one.
    pub count: usize,
    /// The layout the covered keystrokes must be retyped in.
    pub target: LayoutTag,
}

/// Returns what `event` asks the history to do.
///
/// A key-down carrying Command or Control, and every key that moves the caret out of the history's
/// reach, clears it; the two signal keys leave it alone.
pub fn action(event: KeyEvent) -> Action {
    let KeyEvent {
        kind,
        keycode,
        flags,
        timestamp: _,
        stroke: _,
    } = event;
    match kind {
        EventKind::Up => return Action::Ignore,
        EventKind::Flags => return Action::Ignore,
        EventKind::MouseDown => return Action::Clear,
        EventKind::Down => {}
    }
    if flags & (COMMAND | CONTROL) != 0 {
        return Action::Clear;
    }
    for clearing in CLEARING {
        if keycode == clearing {
            return Action::Clear;
        }
    }
    if keycode == DELETE {
        return Action::Erase;
    }
    if keycode == SPACE {
        return Action::Record(Kind::Space);
    }
    if keycode == SWITCH_KEYCODE || keycode == RETYPE_KEYCODE {
        return Action::Ignore;
    }
    Action::Record(Kind::Letter)
}

enum Run {
    Start,
    Same,
}

struct Entry<T> {
    kind: Kind,
    run: Run,
    tag: LayoutTag,
    payload: T,
}

enum Marker {
    Typed,
    Flipped,
}

/// The bounded list of keystrokes moji saw, each under the layout it was typed in.
pub struct History<T> {
    entries: Vec<Entry<T>>,
    last: Marker,
}

impl<T> Default for History<T> {
    fn default() -> History<T> {
        History::new()
    }
}

impl<T> History<T> {
    /// Creates an empty history.
    pub fn new() -> History<T> {
        History {
            entries: Vec::new(),
            last: Marker::Typed,
        }
    }

    /// Appends one keystroke, dropping the oldest once [`CAP`] is reached.
    ///
    /// A keystroke typed in a layout the configuration does not name could never be retyped, so an
    /// absent `tag` clears the history instead of recording.
    pub fn record(&mut self, kind: Kind, tag: Option<LayoutTag>, payload: T) {
        let Some(tag) = tag else {
            self.clear();
            return;
        };
        let run = match self.entries.last() {
            None => Run::Start,
            Some(Entry {
                kind: _,
                run: _,
                tag: before,
                payload: _,
            }) => match *before == tag {
                true => Run::Same,
                false => Run::Start,
            },
        };
        if self.entries.len() == CAP {
            self.entries.remove(0);
        }
        self.entries.push(Entry {
            kind,
            run,
            tag,
            payload,
        });
        self.last = Marker::Typed;
    }

    /// Drops the last keystroke, and does nothing when there is none.
    pub fn erase(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        self.entries.truncate(self.entries.len() - 1);
        self.last = Marker::Typed;
    }

    /// Forgets every keystroke.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.last = Marker::Typed;
    }

    /// Returns how many keystrokes the history holds.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether the history holds no keystroke at all.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns what to retype, the last word first and the whole tail on the press that follows.
    ///
    /// The tail is the run of keystrokes typed since the layout last changed under the fingers, and
    /// the target is the layout after the one the first covered keystroke shows in. The covered
    /// keystrokes take the target as their tag, so the press after a flip of the whole tail carries
    /// it back, while the run itself outlives every flip.
    pub fn flip(&mut self, cycle: &[LayoutTag]) -> Option<Flip> {
        if self.entries.is_empty() {
            return None;
        }
        let tail = tail_start(&self.entries);
        let start = match self.last {
            Marker::Typed => word_start(&self.entries, tail),
            Marker::Flipped => tail,
        };
        let Entry {
            kind: _,
            run: _,
            tag,
            payload: _,
        } = &self.entries[start];
        let target = next(cycle, Some(tag))?;

        for Entry {
            kind: _,
            run: _,
            tag,
            payload: _,
        } in &mut self.entries[start..]
        {
            *tag = target.clone();
        }
        self.last = Marker::Flipped;
        Some(Flip {
            count: self.entries.len() - start,
            target,
        })
    }

    /// Returns the payloads of the last `count` keystrokes, in the order they were typed.
    pub fn last(&self, count: usize) -> Vec<&T> {
        let start = self.entries.len().saturating_sub(count);
        let mut payloads = Vec::new();
        for Entry {
            kind: _,
            run: _,
            tag: _,
            payload,
        } in &self.entries[start..]
        {
            payloads.push(payload);
        }
        payloads
    }
}

fn tail_start<T>(entries: &[Entry<T>]) -> usize {
    let mut start = entries.len();
    for Entry {
        kind: _,
        run,
        tag: _,
        payload: _,
    } in entries.iter().rev()
    {
        start -= 1;
        match run {
            Run::Start => break,
            Run::Same => {}
        }
    }
    start
}

fn word_start<T>(entries: &[Entry<T>], tail: usize) -> usize {
    let mut start = entries.len();
    while start > tail {
        let Entry {
            kind,
            run: _,
            tag: _,
            payload: _,
        } = &entries[start - 1];
        match kind {
            Kind::Space => start -= 1,
            Kind::Letter => break,
        }
    }
    while start > tail {
        let Entry {
            kind,
            run: _,
            tag: _,
            payload: _,
        } = &entries[start - 1];
        match kind {
            Kind::Letter => start -= 1,
            Kind::Space => break,
        }
    }
    start
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::barrier::Stroke;

    fn tag(tag: &str) -> LayoutTag {
        LayoutTag(tag.to_string())
    }

    fn cycle() -> Vec<LayoutTag> {
        vec![tag("en"), tag("ru")]
    }

    fn typed(text: &str, layout: &str) -> History<usize> {
        let mut history = History::new();
        type_into(&mut history, text, layout);
        history
    }

    fn type_into(history: &mut History<usize>, text: &str, layout: &str) {
        for character in text.chars() {
            let kind = match character {
                ' ' => Kind::Space,
                _ => Kind::Letter,
            };
            let payload = history.len();
            history.record(kind, Some(tag(layout)), payload);
        }
    }

    fn key(kind: EventKind, keycode: u16) -> KeyEvent {
        KeyEvent {
            kind,
            keycode,
            flags: 0,
            timestamp: 0,
            stroke: Stroke::First,
        }
    }

    #[test]
    fn letters_and_spaces_are_recorded_under_the_layout_they_were_typed_in() {
        let mut history: History<()> = History::new();

        history.record(Kind::Letter, Some(tag("en")), ());
        history.record(Kind::Space, Some(tag("en")), ());

        assert_eq!(history.len(), 2);
        assert!(!history.is_empty());
        assert_eq!(
            history.flip(&cycle()),
            Some(Flip {
                count: 2,
                target: tag("ru")
            })
        );
    }

    #[test]
    fn erase_drops_the_last_keystroke_and_does_nothing_on_an_empty_history() {
        let mut history: History<()> = History::new();

        history.erase();
        assert_eq!(history.len(), 0);

        history.record(Kind::Letter, Some(tag("en")), ());
        history.erase();

        assert_eq!(history.len(), 0);
        assert!(history.is_empty());
    }

    #[test]
    fn clear_forgets_everything() {
        let mut history: History<()> = History::new();
        history.record(Kind::Letter, Some(tag("en")), ());

        history.clear();

        assert_eq!(history.len(), 0);
        assert_eq!(history.flip(&cycle()), None);
    }

    #[test]
    fn a_keystroke_typed_in_an_unnamed_layout_clears_instead_of_recording() {
        let mut history: History<()> = History::new();
        history.record(Kind::Letter, Some(tag("en")), ());

        history.record(Kind::Letter, None, ());

        assert_eq!(history.len(), 0);
    }

    #[test]
    fn recording_past_the_cap_drops_the_oldest_keystroke() {
        let mut history = History::new();
        for payload in 0..CAP + 1 {
            history.record(Kind::Letter, Some(tag("en")), payload);
        }

        assert_eq!(history.len(), CAP);
        assert_eq!(history.last(CAP).first(), Some(&&1));
    }

    #[test]
    fn keys_that_move_the_caret_out_of_reach_clear_the_history() {
        for keycode in CLEARING {
            assert_eq!(
                action(key(EventKind::Down, keycode)),
                Action::Clear,
                "keycode {keycode} must clear the history"
            );
        }
    }

    #[test]
    fn a_click_clears_the_history() {
        assert_eq!(action(key(EventKind::MouseDown, 0)), Action::Clear);
    }

    #[test]
    fn a_command_or_control_chord_clears_the_history() {
        let command = KeyEvent {
            flags: COMMAND,
            ..key(EventKind::Down, 0)
        };
        let control = KeyEvent {
            flags: CONTROL,
            ..key(EventKind::Down, 0)
        };

        assert_eq!(action(command), Action::Clear);
        assert_eq!(action(control), Action::Clear);
    }

    #[test]
    fn delete_erases_once_per_stroke_and_space_and_letters_record() {
        let delete = key(EventKind::Down, DELETE);
        let repeated = KeyEvent {
            stroke: Stroke::Repeat,
            ..delete
        };

        assert_eq!(action(delete), Action::Erase);
        assert_eq!(action(repeated), Action::Erase);
        assert_eq!(
            action(key(EventKind::Down, SPACE)),
            Action::Record(Kind::Space)
        );
        assert_eq!(
            action(key(EventKind::Down, 0)),
            Action::Record(Kind::Letter)
        );
        assert_eq!(
            action(KeyEvent {
                stroke: Stroke::Repeat,
                ..key(EventKind::Down, 0)
            }),
            Action::Record(Kind::Letter)
        );
    }

    #[test]
    fn the_signal_keys_and_everything_that_is_not_a_key_down_leave_the_history_alone() {
        assert_eq!(action(key(EventKind::Up, 0)), Action::Ignore);
        assert_eq!(action(key(EventKind::Flags, 56)), Action::Ignore);
        assert_eq!(action(key(EventKind::Down, SWITCH_KEYCODE)), Action::Ignore);
        assert_eq!(action(key(EventKind::Down, RETYPE_KEYCODE)), Action::Ignore);
    }

    #[test]
    fn an_empty_history_flips_nothing() {
        let mut history: History<()> = History::new();

        assert_eq!(history.flip(&cycle()), None);
    }

    #[test]
    fn the_first_flip_covers_the_last_word_and_the_spaces_after_it() {
        let mut history = typed("ab cd  ", "ru");

        assert_eq!(
            history.flip(&cycle()),
            Some(Flip {
                count: 4,
                target: tag("en")
            })
        );
    }

    #[test]
    fn the_flip_after_it_covers_the_whole_tail_in_the_same_layout() {
        let mut history = typed("ab cd", "ru");
        history.flip(&cycle());

        assert_eq!(
            history.flip(&cycle()),
            Some(Flip {
                count: 5,
                target: tag("en")
            })
        );
    }

    #[test]
    fn a_keystroke_between_two_flips_makes_the_second_one_a_word_again() {
        let mut history = typed("ab cd", "ru");
        history.flip(&cycle());
        type_into(&mut history, "x", "en");

        assert_eq!(
            history.flip(&cycle()),
            Some(Flip {
                count: 3,
                target: tag("ru")
            })
        );
    }

    #[test]
    fn the_tail_stops_where_the_layout_changed() {
        let mut history = typed("ab", "en");
        type_into(&mut history, "cd", "ru");

        assert_eq!(
            history.flip(&cycle()),
            Some(Flip {
                count: 2,
                target: tag("en")
            })
        );
    }

    #[test]
    fn the_flip_after_a_tail_never_reaches_past_the_layout_it_was_typed_in() {
        let mut history = typed("ab", "en");
        type_into(&mut history, "cd", "ru");
        history.flip(&cycle());

        assert_eq!(
            history.flip(&cycle()),
            Some(Flip {
                count: 2,
                target: tag("ru")
            })
        );
    }

    #[test]
    fn a_flipped_tail_carries_the_target_tag_so_the_flip_after_it_goes_back() {
        let mut history = typed("ab cd", "ru");
        history.flip(&cycle());
        history.flip(&cycle());

        assert_eq!(
            history.flip(&cycle()),
            Some(Flip {
                count: 5,
                target: tag("ru")
            })
        );
    }

    #[test]
    fn a_sentence_typed_in_one_layout_flips_word_first_then_whole_then_back() {
        let mut history = typed("app version", "ru");

        assert_eq!(
            history.flip(&cycle()),
            Some(Flip {
                count: 7,
                target: tag("en")
            })
        );
        assert_eq!(
            history.flip(&cycle()),
            Some(Flip {
                count: 11,
                target: tag("en")
            })
        );
        assert_eq!(
            history.flip(&cycle()),
            Some(Flip {
                count: 11,
                target: tag("ru")
            })
        );
    }

    #[test]
    fn a_layout_outside_the_cycle_flips_to_its_first_entry() {
        let mut history = typed("ab", "de");

        assert_eq!(
            history.flip(&cycle()),
            Some(Flip {
                count: 2,
                target: tag("en")
            })
        );
    }

    #[test]
    fn an_empty_cycle_flips_nothing_and_leaves_the_history_alone() {
        let mut history = typed("ab cd", "ru");

        assert_eq!(history.flip(&[]), None);

        assert_eq!(
            history.flip(&cycle()),
            Some(Flip {
                count: 2,
                target: tag("en")
            })
        );
    }

    #[test]
    fn the_last_keystrokes_come_back_in_the_order_they_were_typed() {
        let history = typed("abc", "en");

        assert_eq!(history.last(2), vec![&1, &2]);
        assert_eq!(history.last(9).len(), 3);
    }
}
