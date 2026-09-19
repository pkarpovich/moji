# moji v2: retype the last word or the whole tail in the other layout

## Overview

moji gains a second signal key. Karabiner emits F18 on a physical key (F6, decided outside this plan) and moji answers by retyping what was just typed in the other layout: the last word on the first press, everything typed in that layout since the last boundary on a second press. The user never deletes a word typed in the wrong layout again; the acceptance scenario is the one that happened today: `фзз мукышщт` typed into a search field that wanted `app version`.

The mechanism is the one moji already has. The tap already sees every keystroke and already replays retained `CGEvent` copies; measured in v1, a replayed copy types the letter of the layout selected after it was captured, because the receiving application re-translates the keycode itself. So moji keeps a history of the keyDown copies it saw, and a flip is: post that many backspaces, select the other layout through the existing barrier, and let the barrier's release replay the copies. moji still never learns which letter a keycode makes.

### Non-goals (v2)

- No `UCKeyTranslate`, no keymap tables, no unicode strings on events, no Accessibility reading or writing of text fields. Text typed before moji started, pasted text and text selected with the mouse cannot be flipped.
- No automatic detection of the wrong layout. F18 is always explicit.
- No configuration of which keycode is the signal: `RETYPE_KEYCODE` is a constant like `SWITCH_KEYCODE`; the physical key lives in Karabiner.
- No per-application exclusion list. A shell line editor handles backspace and replay like any text field; a modal editor is the user's responsibility.
- No depth between "word" and "all": the second press flips the whole tail. A third press flips the whole tail again, which with a two-layout cycle puts it back.
- No changes to Karabiner, the Corne firmware, the cask or the release pipeline inside this plan; they are Post-Completion.

### Rejected alternatives

- **Accessibility: read `AXValue` and `AXSelectedTextRange`, map characters through both layouts, write back.** Handles pasted and mouse-selected text, but needs a character-to-keycode map for every layout and AX text is partial or broken in Electron, browsers and terminals. Rejected: the history approach covers the real scenario in every application that accepts keys, with no new unsafe surface beyond one event type in the tap mask.
- **Select the word with Shift+Option+Left, copy, transform, paste.** Clobbers the clipboard and depends on per-application selection semantics. Rejected.
- **A distinct gesture per depth: each press one more word back, or Shift+F18 for all.** One more word per press is nine presses for the nine-word sentence that motivated the feature; a modifier chord is a second thing to remember and a second Karabiner rule. Rejected in favour of word, then all.
- **Clearing the history on every layout switch.** Would make "reflexively press F19, then remember F18" a no-op. Rejected: each entry carries the tag it was typed in, and the tail is the trailing run of one tag, so a flip after a manual switch just retypes without selecting again.
- **A separate list of keycodes in the pure module mirrored by a list of `CGEvent` copies in the tap layer.** Two lists that must stay in step. Rejected: `History<T>` carries the copy as an opaque payload and the pure tests instantiate it with `()`.

## Skills to invoke

Load each skill below with the Skill tool and follow its conventions before implementing any task in this plan.

- `rust-style` - every file under `src/` and `tests/` follows it: for loops over iterator chains, `let ... else`, no wildcard matches, explicit destructuring, no comments.
- `rustdoc` - doc comments on public items follow it.

## Context (from discovery)

- `src/macos/tap.rs`: a session-level, head-insert `CGEventTap` on `keyDown`, `keyUp` and `flagsChanged`. The callback `FnMut(KeyEvent, &CGEvent) -> Verdict` gets both the plain-data view and the carried event. `Held` is an `Rc<RefCell<Vec<HeldEvent>>>` of `CGEventCreateCopy` results; `Held::replay` marks each with `REPLAY_MAGIC` in `kCGEventSourceUserData` and posts it to the session tap; the tap passes anything carrying the magic straight through. `key_event` reads keycode, flags, timestamp and the autorepeat field.
- `src/barrier.rs`: `KeyEvent`, `EventKind::{Down, Up, Flags}`, `Stroke::{First, Repeat}`, `Verdict::{Pass, Swallow, Hold}`, `Decision { verdict, select: Option<LayoutTag> }`, states `Idle`, `Switching`, `Selecting`, `Settling`, `SETTLE` = 10 ms, `on_key`, `on_select`, `confirmed`, `tick`, `select_failed`, and the public `next(cycle, current)` that picks the next layout in the cycle. The signal keycode is the constant `SIGNAL_KEYCODE = 80`.
- `src/daemon.rs`: `State` holds the barrier, the memory, the `Held` queue, the watchdog timer, `switched_at` and `focused`. `on_key` reads TIS only for the signal key (`current_for`), never per keystroke. `on_confirmation` reads `tis::current()`, updates the app memory and feeds the barrier. `on_focus_poll` runs every 50 ms and calls `on_activation` when the keyboard moved. `select` calls `tis::select` and on error `selection_failed`, which releases the held queue. `release` replays the queue.
- `src/macos/harness.rs` and `tests/live.rs`: an in-process `NSWindow` with an `NSTextView`, `post_key(keycode, Stroke)`, `typed_text`, `wait_for_text`, `select_and_wait`, and the `SCENARIOS` table; `scripts/acceptance.sh` lists the scenarios it runs in `LIVE_TESTS`. The daemon under test is started by `start_daemon(english, russian)`.
- `objc2-core-graphics` 0.3 already exposes `CGEvent::new_keyboard_event`, `CGEvent::new_copy`, `CGEvent::set_type`, `CGEvent::post`, and the `CGEventType` constants `LeftMouseDown`, `RightMouseDown`, `OtherMouseDown`; no new crate feature is needed.
- Secure event input hides keystrokes and F18 alike from every session tap, so nothing here can touch a password field.

## Development Approach

- **Testing approach**: TDD for the pure modules (`history`, `cycle`, `barrier`): the failing test first, then the code. Regular (code first, then tests) for `src/macos/tap.rs` and the daemon wiring, which the live scenarios verify.
- Complete each task fully before moving to the next; small, focused changes.
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task, as separate checklist items covering success and error scenarios.
- **CRITICAL: all tests must pass before starting the next task** - `mise run check` is the gate, plus `! grep -rn 'unsafe' src --include='*.rs' | grep -v '^src/macos/'`.
- **CRITICAL: update this plan file when scope changes during implementation.**
- Tests live inline in `#[cfg(test)] mod tests`; a scenario that needs the live machine is a function in `tests/live.rs` registered in both `SCENARIOS` and `LIVE_TESTS`.
- `src/macos/tap.rs` changes in this plan, so `./scripts/acceptance.sh` is part of the gate from Task 4 on. The terminal running it needs Input Monitoring and Accessibility.
- Every new module is declared in `src/lib.rs` the moment it is created; no blanket `#[allow(dead_code)]`.

## Code-Quality Rules (verify before marking each task complete)

The `rust-style` skill has no separate hard-rules block; these are its rules, materialized so a cold task session verifies against them:

- `for` loops with mutable accumulators, not iterator chains (`filter`/`map`/`collect`/`find`/`sum`).
- `let ... else` for early returns; `if let` only for a short branch with no else.
- `match` covers every variant explicitly; no `_ =>` wildcard and no `matches!`. Ask before adding a wildcard.
- Destructure structs and tuples explicitly.
- Shadow through transformations; no `raw_`/`parsed_` prefixes.
- Newtypes over bare strings for identifiers; enums over `bool` parameters. `LayoutTag` stays declared in `macos/tis.rs`.
- No comments inside function bodies, no section dividers, no TODOs, no commented-out code. Doc comments (`///`) on public items only, per `rustdoc`.
- Per-task gate: `mise run check` is green, every new module is declared, no `#[allow(dead_code)]` without a narrow reason, no `unsafe` outside `src/macos/`. Only then mark the task `[x]`.

## Testing Strategy

- **Unit tests**: required for every task. The pure modules take plain data and a fake clock.
- **Live scenarios** (`tests/live.rs`, run by `scripts/acceptance.sh`): the two acceptance flips against the in-process `NSTextView`. As in v1, the window and the observer share moji's process, so the 10 ms settle is not proved here; the cross-process case is the manual check in Post-Completion.
- **Permissions**: the terminal running the acceptance script needs Input Monitoring and Accessibility; the installed daemon's grant is separate.

## Progress Tracking

- Mark completed items with `[x]` immediately when done.
- Add newly discovered tasks with ➕ prefix.
- Document issues/blockers with ⚠️ prefix.
- Update the plan if implementation deviates from the original scope.

## Solution Overview

Three pieces, in dependency order.

1. **History (`src/history.rs`, pure).** `History<T>` is a bounded list of entries `{ kind: Letter | Space, tag: LayoutTag, payload: T }`. A free function `action(KeyEvent) -> Action` classifies what the tap saw into `Record(Kind)`, `Erase`, `Clear` or `Ignore`. `flip(cycle)` answers `Flip { count, target }` for the word or the whole tail, retags those entries to the target and remembers that it did, so the next `flip` without a recording in between covers the tail.
2. **Barrier and cycle.** `next` moves to `src/cycle.rs`. The barrier learns a second signal keycode, `RETYPE_KEYCODE = 79`, swallows its down, repeat and up, and answers a press with `Request::Retype` when it is `Idle` or `Selecting`, `Request::Nothing` otherwise. `on_retype(target, held, now)` enters `Switching` with a queue that is already full.
3. **Tap and daemon.** The tap mask adds the three mouse-down types, reported as `EventKind::MouseDown` and always passed. `HeldEvent` becomes something the daemon can capture from the carried event, duplicate and push into `Held` as a down plus a synthesized up. The daemon keeps a cached current tag, feeds the history from every key verdict that is not a swallow, clears it on focus moves, and on `Request::Retype` selects first, posts backspaces, fills the queue, and either releases at once or lets the barrier's confirmation and settle release.

The order inside one flip is what makes it correct: `tis::select` first so a refused select changes nothing, then the backspaces, then the queue, all posted to the same session tap the application reads in order. Keys typed after F18 are held by the barrier exactly as after F19, so they land after the retyped word.

## Technical Details

### History (`src/history.rs`)

Public surface, pinned:

```rust
pub const CAP: usize = 512;
pub enum Kind { Letter, Space }
pub enum Action { Record(Kind), Erase, Clear, Ignore }
pub struct Flip { pub count: usize, pub target: LayoutTag }
pub fn action(event: KeyEvent) -> Action;
pub struct History<T> { /* entries, last */ }
impl<T> History<T> {
    pub fn new() -> History<T>;
    pub fn record(&mut self, kind: Kind, tag: Option<LayoutTag>, payload: T);
    pub fn erase(&mut self);
    pub fn clear(&mut self);
    pub fn len(&self) -> usize;
    pub fn flip(&mut self, cycle: &[LayoutTag]) -> Option<Flip>;
    pub fn last(&self, count: usize) -> Vec<&T>;
}
```

`action` rules, on the plain `KeyEvent`: `Up` and `Flags` are `Ignore`; `MouseDown` is `Clear`; a `Down` carrying the Command flag (`1 << 20`) or the Control flag (`1 << 18`) is `Clear`; keycodes 36 (Return), 76 (keypad Enter), 48 (Tab), 53 (Escape), 115 (Home), 116 (Page Up), 119 (End), 121 (Page Down), 117 (forward delete), 123-126 (arrows) are `Clear`; 51 (Delete) is `Erase`; 49 (Space) is `Record(Space)`; `SWITCH_KEYCODE` and `RETYPE_KEYCODE` are `Ignore`; every other `Down`, first stroke or repeat, is `Record(Letter)`. A repeat of Delete is an `Erase` each.

`record` with `tag: None` (the selected layout is not in the configuration) clears instead of recording: an entry without a tag could never be flipped. Recording past `CAP` drops the oldest entry. Any `record` resets the flip marker; `erase` on an empty history is a no-op and does not touch the marker; `clear` resets both.

`flip`: the tail is the run of entries typed since the layout last changed under the fingers; empty history is `None`. The word is the trailing `Space` entries plus the `Letter` entries before them up to the previous `Space`, all within the tail. Marker `Typed` selects the word; marker `Flipped` selects the tail. `target` is `cycle::next` of the tag of the **first covered entry**; a tag outside the cycle goes to the cycle's first entry like everywhere else, and an empty cycle is `None`. On success the covered entries get `target` as their tag and the marker becomes `Flipped`. `last(count)` returns the payloads of the last `count` entries in typing order, for the daemon to duplicate into the queue.

⚠️ Deviation found in Task 2: an entry's tag is the layout it currently *shows* in, so a flip rewrites it, and the run boundary cannot be read back off the tags. Each entry therefore carries a `Run { Start, Same }` set at `record` time - `Start` when the tag differs from the entry before it - and the tail is the stretch back to the last `Start`. Deriving the tail from the tags instead, as this section first said, breaks the acceptance scenario: the word flip retags its 7 entries, the second press would read a 7-entry tail of the new tag and flip it straight back instead of covering all 11. Taking `target` from the first covered entry rather than the last is the other half of that fix: after a word flip the covered range is mixed, and only its first entry still names the layout the run was typed in. The run boundary also survives every flip, which is what keeps a second press off text that was typed correctly before a manual switch.

### Cycle (`src/cycle.rs`)

`pub fn next(cycle: &[LayoutTag], current: Option<&LayoutTag>) -> Option<LayoutTag>` moves here unchanged with its tests; `barrier.rs` and `main.rs` import it from `crate::cycle`.

### Barrier (`src/barrier.rs`)

- `SIGNAL_KEYCODE` is renamed `SWITCH_KEYCODE`; `pub const RETYPE_KEYCODE: u16 = 79` sits next to it. `tests/live.rs` and `daemon.rs` follow the rename.
- `EventKind` gains `MouseDown`; `classify` maps it to `Signal::Mouse`, which `on_key` answers with `Decision::pass()` in every state, so a click is never held.
- `Decision.select: Option<LayoutTag>` becomes `Decision.request: Request` with `pub enum Request { Nothing, Select(LayoutTag), Retype }`.
- `classify` recognizes `RETYPE_KEYCODE` as `Signal::Retype(Press | Repeat | Release)`: repeat and release are swallowed with `Request::Nothing`; a press is swallowed with `Request::Retype` in `Idle` and `Selecting`, and with `Request::Nothing` in `Switching` and `Settling`, logged at debug as refused.
- `pub fn on_retype(&mut self, target: LayoutTag, held: usize, now: Instant) -> bool`: in `Idle` and `Selecting` enters `Switching { expected: target, deadline: now + hold, held }` and returns true; in `Switching` and `Settling` returns false and changes nothing. From there `confirmed`, `tick` and `select_failed` behave exactly as for a key-driven switch.

### Tap (`src/macos/tap.rs`)

- `KEYBOARD_MASK` adds `LeftMouseDown`, `RightMouseDown` and `OtherMouseDown`; `kind_of` maps them to `EventKind::MouseDown`, and `key_event` reports keycode 0, flags and timestamp for them. A mouse event never carries the replay magic and is always returned to the system untouched because the barrier passes it.
- `impl HeldEvent { pub fn capture(event: &CGEvent) -> Option<HeldEvent>; pub fn duplicate(&self) -> Option<HeldEvent>; }` wrap `CGEvent::new_copy`.
- `impl Held { pub fn push_stroke(&self, event: &HeldEvent) -> Kept }` pushes a duplicate of the keyDown and a second duplicate retyped to `CGEventType::KeyUp` with `CGEvent::set_type`, in that order.
- `pub fn post_backspaces(count: usize)` posts `count` pairs of keyDown and keyUp for keycode 51 built with `CGEvent::new_keyboard_event(None, 51, ...)`, each marked with `mark_replayed` before `CGEvent::post` to the session tap.

### Daemon (`src/daemon.rs`)

- `State` gains `current: RefCell<Option<LayoutTag>>`, seeded from `tis::current()` in `start`, written in `on_confirmation` from the layout TIS reports, and written to the expected tag the moment `on_key` or `on_retype` starts a switch. It also gains `history: RefCell<History<HeldEvent>>` and `cycle: Vec<LayoutTag>`.
- `on_key`: after the barrier's decision, when the verdict is `Pass` or `Hold`, feed the history with `history::action(event)`: `Record(kind)` captures the carried event and records it under the cached tag, `Erase`, `Clear` and `Ignore` do what they say. A `Swallow` never reaches the history. Then act on `Request::Select` as today and on `Request::Retype` by calling `on_retype(now)`.
- `on_retype(now)`: `history.flip(&cycle)`; `None` logs at debug and returns. With `Flip { count, target }`: when `target` differs from the cached tag, `select` must succeed first, and a refused select logs at warn and returns with nothing posted and the history untouched, which means `flip` is only committed after the select: compute the target first via a non-mutating call, or flip after the select; the plan pins the observable behaviour, not the split. Then `tap::post_backspaces(count)`, then `push_stroke` for each of `history.last(count)`, then: same tag, `release()` right away; different tag, `barrier.on_retype(target, held.len(), now)`, `switched_at`, `arm(hold)`. The debug line names count, target and whether a select was needed.
- `on_activation` clears the history in addition to what it does today.
- `select` returns whether TIS accepted the layout, so `on_retype` can stop; `on_key` keeps calling `selection_failed` on a refusal as today.

### Live scenarios (`tests/live.rs`, `scripts/acceptance.sh`)

- `a_sentence_typed_in_the_wrong_layout_is_retyped_word_first_then_whole`: select Russian and wait, start the daemon, post the keycodes of `a p p space v e r s i o n` (0, 35, 35, 49, 9, 14, 15, 1, 34, 31, 45) as down and up strokes, wait for the view to read `фзз мукышщт`. Post F18 down and up, wait for `фзз version` and for English to be selected. Post F18 again, wait for `app version`. Restore the layout that was selected before.
- `a_word_typed_before_a_manual_switch_is_retyped_without_a_second_switch`: select English, start the daemon, post `g h b d t n` (5, 4, 11, 2, 17, 45), wait for `ghbdtn`, select Russian by hand through `select_and_wait`, post F18 once, wait for `привет`, assert Russian is still selected and the daemon's `releases()` count did not grow.
- Both names go into `SCENARIOS` and `LIVE_TESTS`.

## What Goes Where

- **Implementation Steps** (`[ ]` checkboxes): code, tests and documentation in this repository.
- **Post-Completion** (no checkboxes): the Karabiner rule, the release, the Corne firmware, the manual cross-process check.

## Implementation Steps

### Task 1: Move the cycle step into its own module

**Files:**
- Create: `src/cycle.rs`
- Modify: `src/lib.rs`, `src/barrier.rs`, `src/main.rs`

- [x] create `src/cycle.rs` with `pub fn next` moved verbatim from `src/barrier.rs`, declare it in `src/lib.rs`
- [x] point `src/barrier.rs` and `src/main.rs` at `crate::cycle::next`
- [x] move the existing `next` tests into `src/cycle.rs`
- [x] run `mise run check` - must pass before task 2

### Task 2: History of typed keys as a pure module

**Files:**
- Create: `src/history.rs`
- Modify: `src/lib.rs`

- [x] write failing tests on `History<()>`: recording letters and spaces under a tag, `erase` popping one entry and being a no-op when empty, `clear` emptying, `record` with `tag: None` clearing, `CAP` dropping the oldest entry
- [x] write failing tests for `action`: every keycode in the `Clear` table, Delete as `Erase`, Space as `Record(Space)`, a letter and its repeat as `Record(Letter)`, Command and Control flagged downs as `Clear`, `Up`, `Flags`, `SWITCH_KEYCODE` and `RETYPE_KEYCODE` as `Ignore`, `MouseDown` as `Clear` (written against `SIGNAL_KEYCODE`, which Task 3 renames)
- [x] write failing tests for `flip`: empty history is `None`; a single word flips its letters and trailing spaces; a second `flip` without a recording covers the whole tail; a recording between two flips makes the second one a word again; a tail of mixed tags stops at the tag change and targets the next layout after the last entry's tag; a flipped tail carries the target tag afterwards so the flip after it goes back; a tag outside the cycle targets the first entry; an empty cycle is `None`; `last(count)` returns payloads in typing order - plus the acceptance sentence itself, 7 then 11 then 11 back
- [x] implement `src/history.rs` to the pinned surface, declared in `src/lib.rs`
- [x] run `mise run check` - must pass before task 3

⚠️ `pub const RETYPE_KEYCODE: u16 = 79` and `EventKind::MouseDown` landed in `src/barrier.rs` here, because `action` cannot be written or tested without them; `classify` maps `MouseDown` to `Signal::Other` until Task 3 gives it `Signal::Mouse`. The tap does not report mouse events until Task 4, so nothing observable changed.
➕ `History::is_empty` sits next to `len`, which clippy requires of a public `len`.

### Task 3: Second signal key in the barrier

**Files:**
- Modify: `src/barrier.rs`, `src/daemon.rs`, `tests/live.rs`

- [x] rename `SIGNAL_KEYCODE` to `SWITCH_KEYCODE` everywhere and add `RETYPE_KEYCODE = 79`
- [x] write failing tests: a retype press in `Idle` and in `Selecting` is swallowed with `Request::Retype`; its repeat and release are swallowed with `Request::Nothing`; a press during `Switching` and during `Settling` is swallowed with `Request::Nothing` and switches nothing; `on_retype` from `Idle` enters `Switching` reporting the given held count, then `confirmed` with the target answers `Settling` and `tick` after `SETTLE` answers `Settled`; `on_retype` during `Switching` and `Settling` returns false and leaves the state alone; `tick` past the hold after `on_retype` answers `Unconfirmed`; a `MouseDown` passes in every state and is never counted as held
- [x] replace `Decision.select` with `Decision.request: Request` and update the existing tests and `src/daemon.rs`
- [x] implement `Signal::Retype`, `Signal::Mouse`, `EventKind::MouseDown` and `on_retype`
- [x] run `mise run check` - must pass before task 4

### Task 4: Mouse-down in the tap, capturable held events, synthesized backspaces

**Files:**
- Modify: `src/macos/tap.rs`

- [x] extend `KEYBOARD_MASK` and `kind_of` with the three mouse-down types reporting `EventKind::MouseDown`
- [x] add `HeldEvent::capture`, `HeldEvent::duplicate`, `Held::push_stroke` and `post_backspaces` per Technical Details
- [x] write tests: a left, right and other mouse-down event reads as `EventKind::MouseDown`; `push_stroke` leaves a keyDown then a keyUp with the same keycode in the queue; a captured then duplicated event is a distinct copy carrying the same keycode and flags; the backspace event builder yields keycode 51 marked with the replay magic (factor the builder so the test does not post)
- [x] run `mise run check` (green) and `./scripts/acceptance.sh` (skipped - not automatable in this session, see ⚠️ below)

⚠️ `./scripts/acceptance.sh` cannot run in the non-interactive session this task was implemented in: the very first live scenario panics in `src/macos/harness.rs` with "the harness window never became frontmost", before any tap code is reached. Verified pre-existing by stashing the change and running the same scenario on `13efd0a`, which fails identically, so this is the session having no interactive GUI focus and no Input Monitoring grant, not a regression. The script has to be run from an interactive terminal that holds both grants; Task 6 needs it too.
➕ `Kept` became public, because the pinned `Held::push_stroke` returns it.
➕ `post_backspaces` is built on a private `backspace_pair()` so the test can assert keycode 51 and the replay magic without posting to the session tap.

### Task 5: Wire the history and the flip into the daemon

**Files:**
- Modify: `src/daemon.rs`

- [x] add `current`, `history` and `cycle` to `State`; seed `current` in `start`, write it in `on_confirmation` and when a switch starts
- [x] feed the history from `on_key` for `Pass` and `Hold` verdicts per `history::action`, and clear it in `on_activation`
- [x] make `select` report whether TIS accepted the layout
- [x] implement `on_retype` per Technical Details with the select-first ordering and the debug and warn lines
- [x] write tests for whatever pure helper the wiring needs (a `State` is built without a tap in `daemon.rs`: a confirmation updates the cached tag, a confirmation of an untagged layout drops it, a typed key lands in the history under the cached tag, a click and a caret key clear it, delete erases and a key-up does nothing, an untagged layout clears instead of recording, a focus move clears, a retype with an empty history posts nothing, a retype whose layout cannot be selected leaves the history as it was. The cached tag after a *started* switch is written inside `select`, which calls TIS and so cannot run off the main thread: it is covered by the Task 6 live scenarios)
- [x] run `mise run check` - must pass before task 6

⚠️ The plan left the select-first split open; it is `History::planned`, a non-mutating twin of `flip` that answers the same `Flip`. `on_retype` asks it for the target, selects, and only then calls `flip`, so a refused select leaves the history exactly as the user typed it.
➕ `select` writes the cached tag itself when TIS accepts, which is the one place every started switch passes through - the switch key, the per-application policy and the retype - and it no longer calls `selection_failed`: `on_key` and `on_activation` do that on a refusal, and `on_retype` returns instead.
➕ `State::forget_typed` clears the history from `on_activation`, and `on_confirmation` is split into it and `confirm(current, now)`, which is what the tests drive without TIS.

### Task 6: Live acceptance scenarios

**Files:**
- Modify: `tests/live.rs`, `scripts/acceptance.sh`

- [x] add `a_sentence_typed_in_the_wrong_layout_is_retyped_word_first_then_whole` per Technical Details
- [x] add `a_word_typed_before_a_manual_switch_is_retyped_without_a_second_switch` per Technical Details
- [x] register both in `SCENARIOS` and in `LIVE_TESTS`
- [x] run `./scripts/acceptance.sh` (skipped - not automatable in this session, see the Task 4 note and the one below)
- [x] run `mise run check` - must pass before task 7

⚠️ `./scripts/acceptance.sh` is still unrunnable here for the reason Task 4 recorded: every live scenario panics in `src/macos/harness.rs` at "the harness window never became frontmost", including `an_untouched_view_is_empty_and_a_set_string_reads_back`, which installs no tap and switches no layout. That proves the blocker is the session having no interactive GUI focus rather than anything in these two scenarios. What could be checked was: both names resolve through `SCENARIOS` (the run reaches `Window::open`, so the lookup succeeded), and the `LIVE_TESTS` list matches the registered names one for one.
➕ `post_keys`, `post_retype`, `wait_for_exactly` and the `FOCUS_SETTLE` pump after `start_daemon` are the new helpers; the pump lets the first focus poll happen before anything is typed, so its `on_activation` cannot clear the history the scenario is about to build.

### Task 7: Verify acceptance criteria

- [x] verify every point of the Overview and every Non-goal holds in the code
- [x] verify the edge cases: F18 with an empty history, F18 during a running switch, a refused select posting nothing, a click and a focus move clearing the history, a layout not in the configuration clearing instead of recording
- [x] run `mise run check`
- [x] run `./scripts/acceptance.sh` (its static gates ran and passed; the live scenarios are not automatable in this session, see the note below)
- [x] run `! grep -rn 'unsafe' src --include='*.rs' | grep -v '^src/macos/'`

The audit, claim by claim:

- Overview: `History::flip` covers the word on the first press and the whole tail on the next, because the marker is `Typed` until a flip sets it to `Flipped` and a `record` sets it back; a flip is `tis::select`, then `tap::post_backspaces(count)`, then `push_stroke` per `history.last(count)`, then the barrier's release, which is exactly `State::on_retype` in `src/daemon.rs`. Nothing in the path reads a keycode's letter.
- Non-goals: no `UCKeyTranslate` anywhere; the only `keyboard_set_unicode_string` is the v1 measurement helper in `src/macos/harness.rs`, which predates this branch and is test-only; no `AXValue` or `AXSelectedTextRange` read; `RETYPE_KEYCODE` is a `pub const` in `src/barrier.rs` with nothing in `src/config.rs` naming it; no exclusion list exists; `flip` has only the word and the tail, and a third press flips the tail again; `git diff --stat master...HEAD` touches no Karabiner, cask or release file.
- Edge cases, each with a test that fails if the behaviour goes: empty history - `History::planned` returns `None` and `daemon::tests::a_retype_with_nothing_typed_posts_nothing_and_holds_nothing`; F18 during a running switch or settle - `barrier::tests::the_retype_key_during_a_switch_is_swallowed_and_changes_nothing`, `..._during_the_settle_...`, `a_retype_during_a_switch_is_refused_and_leaves_the_switch_alone`, `a_retype_during_the_settle_is_refused_and_leaves_the_settle_alone`; a refused select - `on_retype` returns before `post_backspaces` and before `flip`, covered by `daemon::tests::a_retype_that_cannot_select_its_layout_leaves_the_history_as_it_was`; a click and a focus move - `daemon::tests::a_click_and_a_caret_key_drop_what_the_history_holds` and `the_keyboard_moving_to_another_application_drops_what_the_history_holds`; a layout no tag names - `History::record` with `None` clears, and `daemon::tests::a_keystroke_typed_in_a_layout_no_tag_names_clears_the_history`.

⚠️ `./scripts/acceptance.sh` passed its release build, its embedded `__TEXT,__info_plist` check and its bundle check, then stopped at the first live scenario - `a_held_keystroke_types_the_letter_of_the_layout_selected_after_it_was_captured`, a v1 scenario untouched by this plan - with the panic Tasks 4 and 6 recorded: "the harness window never became frontmost". The session has no interactive GUI focus, so no live scenario can run here regardless of what it tests. The live half stays for the Post-Completion manual pass on a logged-in machine.

### Task 8: Update documentation

**Files:**
- Modify: `README.md`, `CLAUDE.md`

- [x] README: a section on retyping (what a press and a second press do, what clears the history, what cannot be flipped), and F18 added to the contract with Karabiner next to F19
- [x] CLAUDE.md: rewrite "the barrier never holds a Core Foundation object" as "the pure modules never know about Core Foundation objects; `History<T>` carries one as an opaque payload"; add a paragraph on the history, its reset signals and the select-first order of a flip; mention mouse-down in the tap mask in the replay section
- [x] move this plan to `docs/plans/completed/`

## Post-Completion

**Manual verification:**
- In a real chat window and in the search field where today's sentence happened: type a sentence in the wrong layout, press the key once and twice, confirm the text and that the following keystrokes land in the new layout. This is the cross-process settle check the in-process harness cannot make.
- Type a word, press F19, then press F18: the word is retyped and the layout does not switch a second time.
- Press F18 with nothing typed, and while holding a modifier: nothing happens, the log shows why at debug.

**External system updates:**
- Karabiner: a rule `F6 -> F18` in the environment repository next to `F5 -> F13`, with the same optional-any modifiers, regenerated and committed there.
- Release v0.2.0 through the existing tag pipeline; `brew upgrade` on the machine; the TCC grants are keyed to the bundle and survive.
- Corne: F18 in a firmware layer when the retype key is wanted there.
