# moji conventions

`README.md` carries what moji is, its configuration and the contract with Karabiner; this file
carries only the rules that govern how code is written here. The plan that builds the daemon, with
the verified API behaviour behind every decision, is
`docs/plans/20260918-moji-layout-daemon.md`.

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

## The ordering logic is pure

`src/barrier.rs` and `src/memory.rs` take plain data and the clock, and return decisions the
`src/macos/` layer executes. The barrier never holds a Core Foundation object and the tap layer owns
the queue of held events. That split is what makes the ordering testable with a fake clock.

## Tests live inline

Tests go in a `#[cfg(test)] mod tests` block in the file they cover. A sibling `foo_test.rs` is not
compiled unless something declares it, so it would sit unbuilt while the gate reported success.

A test that needs the live machine - a real tap, a real layout switch - is `#[ignore]`d with a
reason, and `scripts/acceptance.sh` runs it with `-- --ignored`. `cargo test` must stay reproducible
on a machine where nothing in particular is open.

The live tests run inside cargo's test binary, and TCC attributes an event tap to the responsible
process: the terminal running `scripts/acceptance.sh`. That terminal needs Input Monitoring and
Accessibility once. The installed daemon's own grant goes to `Moji.app` and is separate.

## Declare every module

Every new module file is declared the moment it is created - `mod x;` in its parent `mod.rs` or in
`main.rs`. An undeclared module is not compiled, and neither are its inline tests.

No module carries a blanket `#[allow(dead_code)]`: it is what the compiler uses to report a helper
that was written and never wired up. An item that exists only for tests is `#[cfg(test)]`, and a
field held for ownership rather than reading carries its own narrow allow.

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
- Newtypes over bare strings for identifiers; enums over `bool` parameters.
- The daemon must never panic: a layout that no tag names is ordinary input, never an unwrap.
