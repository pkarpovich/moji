# moji conventions

`README.md` carries what moji is, its configuration and the contract with Karabiner; this file carries only the rules that govern how code is written here. The plan that built the daemon, with the verified API behaviour behind every decision, is `docs/plans/completed/20260918-moji-layout-daemon.md`, and the one that added the retype key is `docs/plans/completed/20260919-moji-retype-last-word.md`.

## Everything runs natively on macOS

The crate links Apple frameworks, so it does not build on any other target. A gate that cannot link is a gate that proves nothing.

## Unsafe containment

Every `unsafe` block lives in `src/macos/` and nowhere else. The rest of the daemon never sees a raw pointer, and the module's public API exposes only safe types.

Core Foundation memory discipline is the live bug class: a value from a `Copy` or `Create` function is owned and must be released on **every** path including error returns; a value from a `Get` function is not.

## Text Input Sources runs on the main thread

TIS is not thread-safe and Apple documents it as main-thread-only. The daemon has no second thread and no async runtime: the event tap callback, both notification observers, the barrier watchdog and every TIS call are sources on the main thread's `CFRunLoop`.

That shapes anything periodic or asynchronous. A distributed notification is delivered on the main run loop and nowhere else. A `CFRunLoopTimer` that does not repeat is invalidated by its own fire, so a one-shot like the watchdog is a timer whose interval is a day: arming it pushes its next fire date, it is never recreated. A signal handler may not stop a run loop, so SIGTERM sets a flag and a repeating timer polls it - the same shape the upgrade watch in `executable.rs` uses.

## The keyboard's application comes from Accessibility, not from the workspace

Measured on this machine: Tuna's panel takes the keyboard without activating, so `NSWorkspaceDidActivateApplicationNotification` never fires for it and `frontmostApplication` keeps naming the window behind it, while Accessibility's system-wide `AXFocusedApplication` moves to the panel and back. There is no notification for that attribute, so `src/macos/focus.rs` polls it on a 50 ms run-loop timer; the workspace's frontmost application is only the fallback for a poll Accessibility does not answer. An unreadable answer is not a move: the last application stands until a new one is read.

## The ordering logic is pure

`src/barrier.rs`, `src/memory.rs`, `src/cycle.rs` and `src/history.rs` take plain data and the clock, and return decisions the `src/macos/` layer executes. The pure modules never know about Core Foundation objects; `History<T>` carries one as an opaque payload, never looking inside it, and the tap layer owns the queue of held events. That split is what makes the ordering and the flip testable with a fake clock and a `History<()>`.

The history is what the retype key reads. `history::action` turns one `KeyEvent` into `Record`, `Erase`, `Clear` or `Ignore`, and the daemon feeds it from the same `on_key` that drives the barrier: a letter or a space is recorded under the tag the daemon has cached for the selected layout, Delete erases the last entry, and a click, a caret key, a chord carrying `barrier::CHORD` (Command, Control or Option), a key that types no letter at all such as a function key, a layout no tag names and the keyboard moving to another application all clear it. The invariant that rule protects is one entry per character on screen: a keystroke moji cannot account for exactly is one it could never retype, so it clears rather than records - otherwise the backspaces a flip posts eat text the user did type. The two signal keys are `Ignore`: they are moji's, not the text's, and a retype press carrying a chord is refused before it reaches the history.

A flip is select first, then delete, then replay. `History::planned` answers what the next flip would cover without touching the history, `State::on_retype` selects that layout and returns on a refusal, and only a selection TIS accepted reaches `History::flip`, `tap::post_backspaces` and the strokes pushed onto the held queue for the barrier's release. That order is what leaves the text exactly as the user typed it when the layout cannot be selected.

## A replayed event is the captured one, unchanged

Measured on this machine, and the reason the tap layer carries no keycode translation: re-posting the very `CGEvent` a tap captured before the switch types the letter of the layout selected **after** the capture. A captured event carrying the unicode string `"a"`, posted once Russian was selected, typed `ф`. The receiving application re-translates the keycode against the current source and ignores the unicode string the event carries.

That is also why the tap's mask carries the three mouse-down types next to the keyboard ones: a click moves the caret somewhere the history cannot follow, so the tap reports it as `EventKind::MouseDown`, which passes through the barrier untouched and clears the history.

So `HeldEvent` is a `CGEventCreateCopy` posted as it is, plus a magic value written into `kCGEventSourceUserData`. That magic is what keeps the tap from re-entering the barrier on its own replays, and it is the only thing a replayed event carries that the captured one did not. Do not add `CGEventKeyboardSetUnicodeString`, and do not reach for `UCKeyTranslate`: nothing needs moji to know which letter a keycode makes.

Two of the events a retype posts are not captured ones. The history keeps key-downs only, so `Held::push_stroke` queues a duplicate of the captured down plus a second duplicate whose type `CGEvent::set_type` turns into the up; and the deletions are fresh keycode-51 pairs built by `tap::post_backspaces`, marked with the same magic. Changing a copy's *type* and synthesizing a backspace is allowed; changing which letter an event claims to type is not.

That is also why the confirmation alone is not the moment to replay. Since the letter is decided in the *receiving* process, and the distributed notification that carries the switch reaches that process after it reaches moji, the barrier answers a matching confirmation with `Settling` rather than a replay: the held events wait `barrier::SETTLE` (10 ms) longer, and keys arriving inside that window queue behind them so the order the user typed in survives. The in-process live harness cannot prove this - its window and its observer share moji's process, where the lag is zero by construction - so the settle is verified by typing into a real chat window, which is a Post-Completion item of the plan.

## Tests live inline, except the ones that need the main thread

Tests go in a `#[cfg(test)] mod tests` block in the file they cover. A sibling `foo_test.rs` is not compiled unless something declares it, so it would sit unbuilt while the gate reported success.

A test that needs the live machine - a real window, a real tap, a real layout switch - lives in `tests/live.rs`, which is declared in `Cargo.toml` with `harness = false` and owns `main`. Cargo's own test harness runs every test on a worker thread even with `--test-threads=1`, and AppKit aborts the process when an `NSWindow` is created or the event queue is pumped anywhere but the main thread, so a live test cannot be an `#[ignore]`d `#[test]`. Each scenario is a function named in the `SCENARIOS` table there, and `main` runs only the scenario named on the command line; `scripts/acceptance.sh` names them one at a time and greps `live: <name> passed`. A plain `cargo test` runs the target with no name, so it prints one line and does nothing: `cargo test` must stay reproducible on a machine where nothing in particular is open.

A new scenario is registered twice: in `SCENARIOS` in `tests/live.rs`, and in `LIVE_TESTS` in `scripts/acceptance.sh`. Only the second list makes the gate run it, and a scenario missing from it leaves the gate green while never executing.

That split is why the crate has both a lib (`src/lib.rs`) and a bin (`src/main.rs`): an integration target cannot reach a bin crate, nor a `#[cfg(test)]` item. Anything a live test drives is a plain `pub` module of the lib.

TCC attributes an event tap and a posted event to the responsible process: the terminal running `scripts/acceptance.sh`. That terminal needs Input Monitoring and Accessibility once. The installed daemon's own grant goes to `Moji.app` and is separate.

## Declare every module

Every new module file is declared the moment it is created - `mod x;` in its parent `mod.rs` or in `lib.rs`. An undeclared module is not compiled, and neither are its inline tests.

No module carries a blanket `#[allow(dead_code)]`: it is what the compiler uses to report a helper that was written and never wired up. An item that exists only for tests is `#[cfg(test)]`, and a field held for ownership rather than reading carries its own narrow allow.

Every `objc2-*` dependency is `default-features = false` with the features it needs listed by name, so reaching for a new Apple type is a new feature in `Cargo.toml` before it is an import: an unresolved `objc2_app_kit::…` is almost always a missing feature rather than a missing crate.

## Per-task gate

```
mise run check
! grep -rn 'unsafe' src --include='*.rs' | grep -v '^src/macos/'
```

`mise run check` is `cargo fmt --all -- --check`, then `cargo clippy --all-targets -- -D warnings`, then `cargo test`. Both commands must pass before the next task starts.

A change to `Info.plist.template`, `build.rs`, `scripts/bundle.sh`, `src/macos/tap.rs` or `src/macos/tis.rs` additionally requires `./scripts/acceptance.sh`. It is the only thing that asserts the binary still carries a readable `__TEXT,__info_plist` with `CFBundleIdentifier = dev.pkarpovich.moji`, which both TCC grants are keyed to; changing that identifier loses them silently.

## Style

- No comments inside function bodies; clear names instead. Doc comments (`///`) on public items only.
- `for` loops with mutable accumulators, not iterator chains.
- `let ... else` for early returns; the main path stays flat.
- `match` covers every variant explicitly; no `_ =>` wildcard and no `matches!`.
- Destructure structs and tuples explicitly.
- Newtypes over bare strings for identifiers; enums over `bool` parameters. A newtype is declared in the module that produces one - `LayoutTag` in `macos/tis.rs`, `BundleId` in `macos/workspace.rs` - and the pure modules import it from there rather than declaring their own.
- The daemon must never panic: a layout that no tag names is ordinary input, never an unwrap.
