# moji conventions

`README.md` carries what moji is, its configuration and the contract with Karabiner; this file
carries only the rules that govern how code is written here. The plan that built the daemon, with
the verified API behaviour behind every decision, is
`docs/plans/completed/20260918-moji-layout-daemon.md`.

## Everything runs natively on macOS

The crate links Apple frameworks, so it does not build on any other target. A gate that cannot link
is a gate that proves nothing.

## Unsafe containment

Every `unsafe` block lives in `src/macos/` and nowhere else. The rest of the daemon never sees a raw
pointer, and the module's public API exposes only safe types.

Core Foundation memory discipline is the live bug class: a value from a `Copy` or `Create` function
is owned and must be released on **every** path including error returns; a value from a `Get`
function is not.

## Text Input Sources runs on the main thread

TIS is not thread-safe and Apple documents it as main-thread-only. The daemon has no second thread
and no async runtime: the event tap callback, both notification observers, the barrier watchdog and
every TIS call are sources on the main thread's `CFRunLoop`.

That shapes anything periodic or asynchronous. A distributed notification is delivered on the main
run loop and nowhere else. A `CFRunLoopTimer` that does not repeat is invalidated by its own fire,
so a one-shot like the watchdog is a timer whose interval is a day: arming it pushes its next fire
date, it is never recreated. A signal handler may not stop a run loop, so SIGTERM sets a flag and a
repeating timer polls it - the same shape the upgrade watch in `executable.rs` uses.

## The ordering logic is pure

`src/barrier.rs` and `src/memory.rs` take plain data and the clock, and return decisions the
`src/macos/` layer executes. The barrier never holds a Core Foundation object and the tap layer owns
the queue of held events. That split is what makes the ordering testable with a fake clock.

## A replayed event is the captured one, unchanged

Measured on this machine, and the reason the tap layer carries no keycode translation: re-posting
the very `CGEvent` a tap captured before the switch types the letter of the layout selected **after**
the capture. A captured event carrying the unicode string `"a"`, posted once Russian was selected,
typed `ф`. The receiving application re-translates the keycode against the current source and
ignores the unicode string the event carries.

So `HeldEvent` is a `CGEventCreateCopy` posted as it is, plus a magic value written into
`kCGEventSourceUserData`. That magic is what keeps the tap from re-entering the barrier on its own
replays, and it is the only thing a replayed event carries that the captured one did not. Do not
add `CGEventKeyboardSetUnicodeString`, and do not reach for `UCKeyTranslate`: nothing needs moji to
know which letter a keycode makes.

## Tests live inline, except the ones that need the main thread

Tests go in a `#[cfg(test)] mod tests` block in the file they cover. A sibling `foo_test.rs` is not
compiled unless something declares it, so it would sit unbuilt while the gate reported success.

A test that needs the live machine - a real window, a real tap, a real layout switch - lives in
`tests/live.rs`, which is declared in `Cargo.toml` with `harness = false` and owns `main`. Cargo's
own test harness runs every test on a worker thread even with `--test-threads=1`, and AppKit aborts
the process when an `NSWindow` is created or the event queue is pumped anywhere but the main thread,
so a live test cannot be an `#[ignore]`d `#[test]`. Each scenario is a function named in the
`SCENARIOS` table there, and `main` runs only the scenario named on the command line;
`scripts/acceptance.sh` names them one at a time and greps `live: <name> passed`. A plain
`cargo test` runs the target with no name, so it prints one line and does nothing: `cargo test` must
stay reproducible on a machine where nothing in particular is open.

A new scenario is registered twice: in `SCENARIOS` in `tests/live.rs`, and in `LIVE_TESTS` in
`scripts/acceptance.sh`. Only the second list makes the gate run it, and a scenario missing from it
leaves the gate green while never executing.

That split is why the crate has both a lib (`src/lib.rs`) and a bin (`src/main.rs`): an integration
target cannot reach a bin crate, nor a `#[cfg(test)]` item. Anything a live test drives is a plain
`pub` module of the lib.

TCC attributes an event tap and a posted event to the responsible process: the terminal running
`scripts/acceptance.sh`. That terminal needs Input Monitoring and Accessibility once. The installed
daemon's own grant goes to `Moji.app` and is separate.

## Declare every module

Every new module file is declared the moment it is created - `mod x;` in its parent `mod.rs` or in
`lib.rs`. An undeclared module is not compiled, and neither are its inline tests.

No module carries a blanket `#[allow(dead_code)]`: it is what the compiler uses to report a helper
that was written and never wired up. An item that exists only for tests is `#[cfg(test)]`, and a
field held for ownership rather than reading carries its own narrow allow.

Every `objc2-*` dependency is `default-features = false` with the features it needs listed by name,
so reaching for a new Apple type is a new feature in `Cargo.toml` before it is an import: an
unresolved `objc2_app_kit::…` is almost always a missing feature rather than a missing crate.

## Per-task gate

```
mise run check
! grep -rn 'unsafe' src --include='*.rs' | grep -v '^src/macos/'
```

`mise run check` is `cargo fmt --all -- --check`, then `cargo clippy --all-targets -- -D warnings`,
then `cargo test`. Both commands must pass before the next task starts.

A change to `Info.plist.template`, `build.rs`, `scripts/bundle.sh`, `src/macos/tap.rs` or
`src/macos/tis.rs` additionally requires `./scripts/acceptance.sh`. It is the only thing that
asserts the binary still carries a readable `__TEXT,__info_plist` with
`CFBundleIdentifier = dev.pkarpovich.moji`, which both TCC grants are keyed to; changing that
identifier loses them silently.

## Style

- No comments inside function bodies; clear names instead. Doc comments (`///`) on public items
  only.
- `for` loops with mutable accumulators, not iterator chains.
- `let ... else` for early returns; the main path stays flat.
- `match` covers every variant explicitly; no `_ =>` wildcard and no `matches!`.
- Destructure structs and tuples explicitly.
- Newtypes over bare strings for identifiers; enums over `bool` parameters. A newtype is declared in
  the module that produces one - `LayoutTag` in `macos/tis.rs`, `BundleId` in `macos/workspace.rs` -
  and the pure modules import it from there rather than declaring their own.
- The daemon must never panic: a layout that no tag names is ordinary input, never an unwrap.
