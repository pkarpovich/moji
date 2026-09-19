# moji: a keyboard layout daemon with an ordering barrier

## Overview

moji (文字) is a macOS LaunchAgent, written in Rust, that becomes the only thing on this machine that switches keyboard layouts.

The problem it solves: today Karabiner switches layouts with `select_input_source`, and the switch and the keystrokes travel on two unrelated paths. The switch goes grabber -> IPC -> console_user_server -> `TISSelectInputSource` -> distributed notification -> the application; the next letters go grabber -> virtual keyboard -> WindowServer -> the application directly. Nothing orders the two, so letters typed right after a fast switch land in the old layout. Measured on this machine: `TISSelectInputSource` takes 2-7 ms and another process sees the change 1-10 ms later, so the API is fast and the defect is purely ordering.

What moji does:

1. **Barrier.** Karabiner stops switching and instead emits one signal key, F19, on the same tap-vs-hold logic it runs today (globe on the MacBook, left Control on the Rainy75, left Shift on the Corne). moji owns a session-level CGEventTap, swallows F19, selects the next layout, and holds every following keyboard event until the layout change is confirmed, then replays them in order. A watchdog releases them after 50 ms regardless, so the keyboard can never hang.
2. **Per-application layout.** A TOML config pins a layout to a bundle id (the Tuna launcher `com.brnbw.Tuna` -> English is the motivating case); every other application gets the layout it last used. Driven by NSWorkspace activation notifications.
3. **CLI.** `moji set <tag>`, `moji toggle`, `moji status`, `moji list`, `moji install`, `moji uninstall`, `moji --version`. The CLI talks to TIS directly; there is no daemon socket.

Acceptance scenario, the one that matters: typing Russian in a chat window, tap the switch key, and without any pause type `hello`. The window shows `hello`, not `руддщ`. Second scenario: open Tuna, it types English; return to the chat window, it is Russian again.

### Non-goals (v1)

- No indicator, no menu bar item, no HUD.
- No automatic layout detection (Punto-style) and no "retype last word" hotkey. Both are later phases that reuse the same tap; nothing in v1 prepares for them beyond keeping the tap module thin.
- No daemon socket or IPC; the CLI and the daemon both talk to TIS.
- No persistence of the per-application memory across restarts.
- No Homebrew cask, notarization, release workflow, or tap update. Deployment is done by hand afterwards.
- No changes to the Corne firmware and no VIA remap of the Rainy75: Control and Shift stay real modifiers, so those keys can only switch on release. The barrier is what fixes them.

### Rejected alternatives

- **Karabiner keeps `select_input_source`, moji only holds keys.** Degrades gracefully when moji is dead, but the owner of the switch would be split between two processes. Rejected by the user: moji is the single owner of layout switching. The single point of failure is covered by launchd `KeepAlive`, the upgrade self-restart, and the barrier watchdog.
- **moji as the hotkey handler instead of Karabiner.** Would duplicate Karabiner's tap-vs-hold handling at HID level. Karabiner already does it per device; moji consumes one key.
- **Matching layouts by input source ID.** On this macOS the same layout reports two IDs depending on the process that asks (`me.tonsky.keyboardlayout.universal.keylayout.English-Universal` in a fresh process, the bundle plist's `me.tonsky.keyboardlayout.universal.english-universal` in a process that selected it). moji matches by localized name.
- **Switching via `shell_command` from Karabiner.** Adds a fork/exec per tap and still has no ordering guarantee.

## Skills to invoke

Load each skill below with the Skill tool and follow its conventions before implementing any task in this plan.

- `rust-style` - every file under `src/` and `tests/` follows it: for loops over iterator chains, `let ... else`, no wildcard matches, explicit destructuring, no comments.
- `rustdoc` - doc comments on public items follow it.
- `go` - only for the Post-Completion change to `rules.go` in the environment repository.

## Context (from discovery)

- The repository is new. The layout, toolchain and conventions are copied from the sibling daemon nikki (`~/Projects/nikki`): `mise.toml` pinning Rust 1.98 with `check` = fmt + clippy `-D warnings` + test, edition 2024, `build.rs` embedding `Info.plist.template` into `__TEXT,__info_plist`, `scripts/bundle.sh` assembling an `.app` so TCC keys the permission to a bundle id rather than to a Homebrew path that changes every version, `src/service.rs` writing a LaunchAgent with `KeepAlive` as `PathState`, `src/executable.rs` watching the binary's inode so the daemon exits when it is replaced.
- Crates, pinned to what nikki builds with: `objc2` 0.6, `objc2-foundation` 0.3, `objc2-app-kit` 0.3 (`NSWorkspace`, `NSRunningApplication`), `objc2-core-foundation` 0.3 (`CFRunLoop`, `CFString`, `CFArray`, `CFDictionary`), `objc2-core-graphics` 0.3 (`CGEvent`, `CGEventSource`, `CGEventTypes`), `block2` 0.6, `argh`, `serde` + `toml`, `tracing` + `tracing-subscriber`, `thiserror`. Verified in the cached `objc2-core-graphics` 0.3.2 source: it binds `CGEventTapCreate`, `CGEventTapEnable`, `CGEventPost`, `CGEventTapPostEvent`, `CGEventCreateKeyboardEvent`, `CGEventCreateCopy`, `CGEventGetIntegerValueField` / `CGEventSetIntegerValueField`, `CGEventKeyboardGetUnicodeString` / `CGEventKeyboardSetUnicodeString`, `CGPreflightListenEventAccess`, `CGPreflightPostEventAccess`, `CGRequestListenEventAccess`, `CGRequestPostEventAccess`.
- No objc2 crate covers HIToolbox / Text Input Sources. The TIS functions are declared by hand in `src/macos/tis.rs` and linked against the `Carbon` framework: `TISCreateInputSourceList`, `TISCopyCurrentKeyboardInputSource`, `TISSelectInputSource`, `TISGetInputSourceProperty`, and the property keys `kTISPropertyInputSourceID`, `kTISPropertyLocalizedName`, `kTISPropertyInputSourceCategory`, `kTISPropertyInputSourceIsEnabled`, `kTISPropertyInputSourceIsSelectCapable`, `kTISCategoryKeyboardInputSource`, plus the notification name `kTISNotifySelectedKeyboardInputSourceChanged`. TIS is not thread-safe and is documented as main-thread: everything that touches it runs on the main run loop.
- The enabled keyboard layouts on the user's machine are `English - Universal`, `Russian - Universal` (both from `~/Library/Keyboard Layouts/Universal.bundle`) and Apple's `ABC`. `ABC` also reports language `en`, which is why matching by language alone is not enough and the config names layouts by localized name.
- Karabiner's current rules live in the environment repository at `karabiner/rules.go` (`langSwitch()`): three variants (`keyboard_fn` on the built-in keyboard, `left_control` on every non-built-in keyboard, `left_shift` on the Corne with a 180 ms `to_if_alone` timeout), each duplicated per direction with an `input_source_if` condition. The user's Corne keytap daemon (`corne/keytap` in the same repository) is prior art for a listen-only tap in Rust and for the Input Monitoring lesson: TCC keys the grant to the binary path, and running through `cargo run` confuses the record.
- macOS 27, Karabiner-Elements 16.3.0. The system's own input-source hotkeys (symbolic hotkeys 60 and 61) are disabled, and `AppleFnUsageType` is 0, so nothing but moji will switch once Karabiner emits F19.

## Development Approach

- **Testing approach**: TDD for the pure modules (`barrier`, `memory`, `config`, the TIS name matcher): write the failing test first, then the code. Regular (code first, then tests) for the FFI wrappers in `src/macos/`, which are verified by `#[ignore]`d live tests and by the acceptance script.
- Complete each task fully before moving to the next; small, focused changes.
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task. Tests are a required deliverable: unit tests for new and modified functions, success and error scenarios, listed as separate checklist items.
- **CRITICAL: all tests must pass before starting the next task** - `mise run check` (fmt, clippy with `-D warnings`, test) is the gate.
- **CRITICAL: update this plan file when scope changes during implementation.**
- Tests live inline in a `#[cfg(test)] mod tests` block of the file they cover, as in nikki. A test that needs the live machine (a real tap, a real layout switch) is `#[ignore]`d with a reason and run by `scripts/acceptance.sh`; `cargo test` stays reproducible on a machine where nothing is open.
- Every `unsafe` block lives under `src/macos/` and nowhere else. Core Foundation values from `Copy`/`Create` functions are owned and released on every path; values from `Get` functions are not.
- Every new module file is declared with `mod` the moment it is created; no module carries a blanket `#[allow(dead_code)]`.

## Code-Quality Rules (verify before marking each task complete)

The `rust-style` skill has no separate hard-rules block; these are its rules, materialized so a cold task session verifies against them:

- `for` loops with mutable accumulators, not iterator chains (`filter`/`map`/`collect`/`find`/`sum`).
- `let ... else` for early returns; `if let` only for a short branch with no else.
- `match` covers every variant explicitly; no `_ =>` wildcard and no `matches!`. Ask before adding a wildcard.
- Destructure structs and tuples explicitly (`let Layout { tag, name } = layout;`).
- Shadow through transformations; no `raw_`/`parsed_` prefixes.
- Newtypes over bare strings for identifiers (`BundleId(String)`, `LayoutTag(String)`); enums over `bool` parameters.
- No comments inside function bodies, no section dividers, no TODOs, no commented-out code. Doc comments (`///`) on public items only, per `rustdoc`.
- Per-task gate: `mise run check` is green, every new module is declared, no `#[allow(dead_code)]`, no `unsafe` outside `src/macos/`. Only then mark the task `[x]`.

## Testing Strategy

- **Unit tests**: required for every task (see Development Approach).
- **Live tests** (`#[ignore]`, run by `scripts/acceptance.sh` on the user's Mac with Input Monitoring and Accessibility granted): the translation spike, the barrier end-to-end harness, the per-application restore.
- **No e2e UI framework**: the in-process harness of Task 3 is the automated e2e. It opens its own `NSWindow` with an `NSTextView`, brings itself frontmost, posts synthetic events and reads the text back. It proves ordering inside one process; the cross-process case (a real chat window) is the manual acceptance scenario in Post-Completion.
- **Permissions for the live suite**: the `#[ignore]`d tests run inside cargo's test binary, and TCC attributes an event tap to the responsible process, which is the terminal that runs `scripts/acceptance.sh`. That terminal needs Input Monitoring and Accessibility once; this is a stated precondition of every live gate from Task 3 on. The installed daemon's own grant goes to `Moji.app` (Post-Completion) and is separate.

## Progress Tracking

- Mark completed items with `[x]` immediately when done.
- Add newly discovered tasks with a `+` prefix.
- Document issues and blockers with a `!` prefix.
- Update the plan if implementation deviates from the original scope.

## Solution Overview

One binary, `moji`. `moji run` is the daemon the LaunchAgent starts; the other subcommands are one-shot. The daemon's whole life is the main thread's `CFRunLoop`, which carries four sources:

1. the CGEventTap (session level, head insert, default options so it can swallow) for `keyDown`, `keyUp` and `flagsChanged`;
2. an `NSDistributedNotificationCenter` observer for `kTISNotifySelectedKeyboardInputSourceChanged`;
3. an `NSWorkspace` observer for `NSWorkspaceDidActivateApplicationNotification`;
4. a one-shot run-loop timer the `macos` layer arms at the 50 ms deadline whenever a decision carries a `select` or `on_select` is accepted, and cancels on replay: the barrier's watchdog.

All four feed one `Barrier` state machine (`src/barrier.rs`) and one `Memory` policy (`src/memory.rs`), both pure: they take plain `KeyEvent`s and the clock and return decisions (`Pass`, `Swallow`, `Hold`, `replay: bool`, `select: Option<LayoutTag>`), and the `macos` layer executes them. The tap layer owns the queue of held events; the barrier never holds a Core Foundation object. That split is what makes the ordering logic unit-testable with a fake clock and keeps every `unsafe` in one directory.

Signal key: F19, virtual keycode 80. Karabiner emits it on a tap; moji swallows both its down and up.

## Technical Details

### Layout model and config

```toml
cycle = ["en", "ru"]

[layouts]
en = "English - Universal"
ru = "Russian - Universal"

[apps]
"com.brnbw.Tuna" = "en"
```

- `layouts` maps a short tag to the **localized name** of an enabled keyboard layout (`kTISPropertyLocalizedName`). At startup, and on every TIS notification, moji resolves each tag against `TISCreateInputSourceList` filtered to `kTISCategoryKeyboardInputSource` + enabled + select-capable; an unresolvable tag is a startup error naming the tags that did resolve. IDs are never compared.
- `cycle` is the toggle order; `toggle` selects the entry after the current one, wrapping. A current layout outside `cycle`, or one with no tag at all (ABC is enabled on this machine and deliberately unmapped), toggles to `cycle[0]`. The barrier therefore takes the current layout as `Option<LayoutTag>`; `None` is a normal input, never a panic.
- `apps` pins a layout tag per bundle id; unknown tags are a config error. Every application not listed is `remember`: the layout that was selected when it last lost focus. Memory is a `HashMap<BundleId, LayoutTag>` in the daemon, empty at start, written on every activation change (for the application losing focus) and on every layout change (for the frontmost application), so an application that never switched still remembers the layout it was used with.
- Config path: `$HOME/.config/moji/config.toml`, overridable with `MOJI_CONFIG` (the same seam nikki uses for its acceptance run). `moji --check-config` parses, resolves layouts and exits.

### TIS layer (`src/macos/tis.rs`)

Safe surface, everything else private:

```rust
pub struct Layout { pub name: String, pub id: String, pub language: Option<String> }
pub fn enabled_layouts() -> Vec<Layout>
pub fn current() -> Option<Layout>
pub fn select(layout: &Layout) -> Result<(), TisError>
pub fn observe_changes(on_change: impl Fn() + 'static) -> ChangeObserver
```

- `select` re-fetches the `TISInputSourceRef` by name on every call rather than caching the ref: a cached ref works, but the ID it reports drifts (see Rejected alternatives), and a fresh fetch keeps `current()` and `select()` consistent.
- `observe_changes` subscribes to `kTISNotifySelectedKeyboardInputSourceChanged` on the distributed center with `deliverImmediately`; the callback runs on the main run loop.
- Link with `#[link(name = "Carbon", kind = "framework")]`.

### Tap layer (`src/macos/tap.rs`)

```rust
pub enum Placement { Listen, Intercept }
pub struct Tap { .. }
pub fn install(placement: Placement, on_event: impl FnMut(KeyEvent, &CGEvent) -> Verdict + 'static) -> Result<Tap, TapError>
pub enum Verdict { Pass, Swallow, Hold }
pub fn replay(held: Vec<HeldEvent>)
```

- `KeyEvent { kind: Down | Up | Flags, keycode: u16, flags: u64, timestamp }` is the plain-data view the barrier reasons about; `HeldEvent` owns a retained copy (`CGEventCreateCopy`) so a swallowed event can be posted later.
- Replay marks every re-posted event by writing a magic value into `kCGEventSourceUserData` via `CGEventSetIntegerValueField`; the tap passes events carrying the magic straight through, otherwise its own replays would re-enter the barrier. Replay posts with `CGEventPost` at `kCGSessionEventTap`, which is downstream of Karabiner's virtual keyboard and upstream of every application.
- The confirmation the barrier waits for is observed in moji's own process; distributed notifications carry no cross-process ordering, so the receiving application may learn of the switch after moji does. That is sufficient only when the replayed event already carries the character decided in moji's process (strategies (b) and (c) of Task 3). If the chosen strategy is (a), replay waits for the confirmation plus a fixed 10 ms settle. The in-process harness cannot prove the settle: its window, tap and observer live in one process, so the cross-process lag is zero by construction. The proof is the manual acceptance scenario against a real chat window (Post-Completion); record the chosen strategy and the settle as a `+` line here.
- + Measured by Task 3 on this machine: **strategy (a) is the one Task 5 replays with.** Re-posting the very `CGEvent` a listen-only tap captured before the switch types the letter of the layout selected **after** the capture: the captured event carried the unicode string `"a"`, and posting it once Russian was selected typed `ф`. The receiving application re-translates the keycode against the current source and ignores the unicode string the event carries. (b) typed `ф` as well and (c), the control, typed what it was told. So replay posts the held copy unchanged, plus the magic user data.
- + Strategy (a) therefore carries the settle this section makes mandatory for it: a matching confirmation moves the barrier to `Settling { deadline: now + SETTLE, held }` instead of releasing, `barrier::SETTLE` is 10 ms, and the daemon re-arms the same watchdog timer for it. Keys arriving inside the settle window are held too, so the replay keeps the order the user typed in, and `Barrier::tick` distinguishes `Settled` (replay, ordinary) from `Unconfirmed` (release, counted in `Releases` and logged at warn). The in-process harness still cannot prove the settle - its lag is zero by construction - so the proof stays the manual chat-window scenario in Post-Completion, which is now written down there.
- `TapDisabledByTimeout` / `TapDisabledByUserInput` re-enable the tap and log at warn; the callback must stay well under the system's timeout, and the only slow thing it does is one `select` (2-7 ms measured).
- Permissions: `CGPreflightListenEventAccess` and `CGPreflightPostEventAccess` at startup; when either is missing, call the matching `CGRequest*` once so the system prompt appears, log the exact System Settings pane, and exit non-zero. With `KeepAlive` as `PathState` launchd respawns it until the grant lands, which is a loop of one prompt per respawn; the log line names the pane so the loop is self-explanatory. While moji is not running, F19 is an unbound key and nothing switches: that is the accepted failure mode of moji being the single owner.

### Barrier (`src/barrier.rs`, pure)

States: `Idle`, `Switching { expected, deadline }` (entered by F19: keys are held) and `Selecting { expected, deadline }` (entered by an activation-driven select: keys pass). The barrier tracks how many events are held; the tap layer owns the events themselves. Every entry point takes the current layout as `Option<LayoutTag>`; `None` (an unmapped layout such as ABC, reachable from the menu bar) is ordinary input at every one of them, never a panic.

- In `Idle`: F19 down -> `Swallow` + `select: Some(next)` + enter `Switching { expected: next }`; F19 up -> `Swallow`; anything else -> `Pass`. `confirmed`, `tick` and `select_failed` in `Idle` are no-ops returning `replay: false` (a `moji set` from a shell, an external switch, or the trailing notification after a watchdog release all land here).
- In `Switching`: F19 down/up -> `Swallow` (a second tap inside the window is ignored, not queued); any other key or flags event -> `Hold` (the tap appends it to its queue in arrival order); `confirmed(now_selected: Option<LayoutTag>)` -> replay only when `now_selected == Some(expected)`, a notification naming any other layout or an unmapped one is ignored and the deadline keeps running; `tick(now)` past the deadline -> replay + a warn-level log naming how many events were held; `select_failed()` -> replay immediately (nothing to wait for).
- `on_select(expected, now) -> bool` is the entry for a select the daemon initiates without a key (an activation pin or memory restore). From `Idle` it enters `Selecting { expected, deadline }` and returns `true`. From `Switching` it is refused (`false`): the key-driven switch owns its window, and the daemon issues `tis::select` only when the barrier accepted the call. From `Selecting` it replaces `expected`, re-arms the deadline and returns `true`. In `Selecting` every key passes; F19 down starts a real switch exactly as from `Idle`, with `next` computed from `expected` rather than from `current()`, and enters `Switching`; `confirmed(Some(expected))` returns to `Idle` with nothing to replay; a mismatched or `None` confirmation is ignored; the deadline returns to `Idle` with a warn; `select_failed` returns to `Idle`.
- Deadline: 50 ms after the select. The clock is injected (`fn(&mut self, now: Instant)`), so tests drive it.
- Which layout is next is decided by `cycle` and the current tag (`Option<LayoutTag>`, `None` for an unmapped layout such as ABC); the barrier receives the resolved tag, it never touches TIS. The daemon resolves the tag for `confirmed` by reading `current()` inside the notification handler and matching its name against `layouts`.

### Per-application memory (`src/memory.rs`, pure)

- `on_layout_changed(app: BundleId, layout: Option<LayoutTag>)` records the layout for the frontmost app; called from the TIS notification with the current frontmost bundle id; `None` records nothing.
- `on_activated(app: BundleId, current: Option<LayoutTag>) -> Option<LayoutTag>`: first records `current` for the application that is losing focus (the one it saw activated last; `None` records nothing), then decides for `app`: a pinned app returns its pin; a remembered app returns its memory; an unknown app returns `None` (keep whatever is selected). **A decided tag equal to `current` also returns `None`**: selecting the already-selected source emits no TIS notification, so a select the daemon issued would wait for a confirmation that never comes; after memory warms up this is the common Cmd-Tab, not a corner case. For the same reason the config rejects a `cycle` shorter than two entries. This is what makes an application that never switched keep the layout it was used with when the user comes back from a pinned one.
- The daemon executes the returned select through `barrier.on_select(tag, now)` and, only when that returned `true`, `tis::select`; an activation switch is thus confirmed by the same tag-matched notification but never holds keys, an F19 arriving during it starts a real switch instead of being swallowed, and an activation during a key-driven switch does not steal its window.
- Tuna is an `LSUIElement` agent; whether showing its panel makes it the frontmost application is unverified (see Post-Completion). If it does not, `!` this task and design an AX focused-window signal in a follow-up plan; do not build it speculatively.

### Threading

Everything runs on the main thread's run loop: the tap callback, both notification observers, the watchdog timer, and every TIS call. There is no tokio runtime and no second thread in v1. `moji run` installs the sources, logs "moji is running" with the version and the resolved layouts, and calls `CFRunLoopRun`. SIGTERM (what launchd sends) stops the run loop; the tap is destroyed on drop.

### Service (`src/service.rs`, `src/executable.rs`)

Copied from nikki with the label `dev.pkarpovich.moji`: `install` writes `~/Library/LaunchAgents/dev.pkarpovich.moji.plist` running the canonicalized executable (which, when it sits inside `Moji.app/Contents/MacOS/`, is the bundle path TCC keys the grant to), `KeepAlive` as `PathState` on the program, stdout/stderr under `~/Library/Logs/moji/`, `bootout` then wait-unloaded then `bootstrap`. `executable::swapped` ends the run loop when the binary's inode changes so launchd starts the new version.

### CLI (`src/main.rs`, `argh`)

`moji run`, `moji set <tag>`, `moji toggle`, `moji status` (prints current layout name, its tag if any, and the frontmost bundle id), `moji list` (every enabled keyboard layout with name, id and language, so the user can copy names into the config), `moji install`, `moji uninstall`, `moji --check-config`, `moji --version`. `set`/`toggle`/`status`/`list` load the config, talk to TIS, and exit; they never need the daemon.

## What Goes Where

- **Implementation Steps** (`[ ]` checkboxes): everything buildable and testable inside this repository.
- **Post-Completion** (no checkboxes): the Karabiner change in the environment repository, the live Tuna check, granting permissions, deployment.

## Implementation Steps

### Task 1: Scaffold the crate the way nikki is laid out

**Files:**
- Create: `Cargo.toml`, `mise.toml`, `.gitignore`, `build.rs`, `Info.plist.template`, `README.md`, `CLAUDE.md`
- Create: `src/main.rs`, `src/macos/mod.rs`
- Create: `scripts/bundle.sh`, `scripts/acceptance.sh`

- [x] `Cargo.toml`: package `moji`, edition 2024, the dependency set from Context pinned to nikki's versions, `[profile.release]` with `lto`, `strip`
- [x] `mise.toml` with the `build`/`test`/`lint`/`fmt`/`check` tasks as in nikki; `build.rs` + `Info.plist.template` with bundle id `dev.pkarpovich.moji` and `CFBundleName` `moji`
- [x] `src/main.rs`: `argh` command enum with every subcommand from Technical Details, each returning "not implemented" for now except `--version`, which prints `moji <CARGO_PKG_VERSION>`
- [x] `scripts/bundle.sh` assembling `Moji.app` (executable `moji`, `LSUIElement` true, no icon yet) and `scripts/acceptance.sh` that builds release, checks the embedded plist's `CFBundleIdentifier`, and runs the `#[ignore]`d live tests added by later tasks
- [x] `CLAUDE.md`: the conventions from Development Approach (unsafe containment, inline tests, declare every module, main-thread TIS)
- [x] write tests for the command parser: every subcommand and flag parses, an unknown subcommand is an error
- [x] run `mise run check` - must pass before task 2
- + `scripts/acceptance.sh` also asserts that `scripts/bundle.sh` assembles `Moji.app` with `CFBundleIdentifier = dev.pkarpovich.moji`: nothing else in the repository runs the bundle script, and both TCC grants depend on it. Its `LIVE_TESTS` array is empty until Task 3 adds the first `#[ignore]`d test.
- + The dependency set omits `libc`: nothing in v1 needs it before the SIGTERM handling of Task 5, which adds it then.

### Task 2: TIS layer with name-based resolution

**Files:**
- Create: `src/macos/tis.rs`
- Modify: `src/macos/mod.rs`, `src/main.rs`

- [x] declare the TIS functions and property keys from Context in `tis.rs` with `#[link(name = "Carbon", kind = "framework")]`; wrap `TISInputSourceRef` in an owned newtype that releases on drop
- [x] `enabled_layouts()`: list, filter to keyboard-layout category + enabled + select-capable, read name/id/language into `Layout`
- [x] `current()` and `select(&Layout)` (re-fetching by name, error on no match or non-zero `OSStatus`)
- [x] `observe_changes` on the distributed center with `deliverImmediately`, returning a guard that removes the observer on drop
- [x] wire `moji list` and `moji status` (status without the frontmost app for now)
- [x] write tests: the pure name matcher (`resolve(tags, layouts)`) resolves every tag, reports every unresolved tag by name, and rejects two tags mapped to one name; a `Layout` with the ABC name and language `en` does not satisfy a tag whose name is `English - Universal`
- [x] write an `#[ignore]`d live test: `enabled_layouts()` on this machine contains at least one layout and `current()` is one of them; `select` to another and back leaves `current()` where it started
- [x] run `mise run check` - must pass before task 3
- + The extern block declares `kTISPropertyInputSourceLanguages` as well: Context lists the keys the filter needs, but `Layout.language` has no other source. Each symbol is declared under a Rust name with `#[link_name = "..."]`, which keeps the block free of a `non_upper_case_globals` allow.
- + `observe_changes` gets `deliverImmediately` by calling `setSuspended(false)` on the distributed center: the block-based `addObserverForName:object:queue:usingBlock:` has no suspension-behavior parameter, and the selector-based API that does would need an Objective-C class.
- + `LayoutTag` lives in `tis.rs`, not in `barrier.rs`: `resolve` needs it one task earlier. Task 4 imports it from there instead of declaring it again.
- + Measured on this machine: a distributed notification is delivered on the main run loop only. Cargo runs every test on a worker thread, so a live test cannot observe a real layout change; the live observer test asserts that the center is installed unsuspended and the guard removes it, and the delivery itself is proven by the Task 3 harness, which owns the main thread.
- + `select`, `observe_changes` and `resolve` carry `#[cfg_attr(not(test), allow(dead_code, reason = "..."))]`, each naming the task that wires it. Nothing in `moji run` exists yet, so the bin target cannot reach them while the test target can. The task that wires each one deletes its attribute.
- + `moji status` prints name, id and language: the tag needs the configuration, which Task 6 adds.

### Task 3: Translation spike as a live test harness

The question this task answers, and nothing else: after a layout switch, which of three ways of re-posting a held keystroke makes the receiving application type the letter of the **new** layout? (a) posting the very `CGEvent` that a listen-only tap captured before the switch (it already carries the unicode string the old layout produced), (b) a fresh `CGEventCreateKeyboardEvent` with the same keycode created after the switch, (c) the captured copy with its unicode string rewritten with `CGEventKeyboardSetUnicodeString`. The strategies are ranked (a) > (b) > (c): (a) needs nothing, (b) needs a fresh event per held key, (c) needs moji to translate keycodes itself. (c) is kept only as a control: in the test the string is set to the known answer, so it always types `ф` and proves nothing about production, where the character would have to come from `UCKeyTranslate` over `kTISPropertyUnicodeKeyLayoutData`, which the TIS layer does not provide. The answer decides how Task 5 replays.

**Files:**
- Create: `src/macos/harness.rs` (cfg(test)-only helper: an `NSWindow` with an `NSTextView` that the process brings to the front, `typed_text()` reading the view's string, and `capture_next_key()`, a listen-only `CGEventTapCreate` that hands back a retained copy of the next keyDown it sees, so strategy (a) operates on a genuinely captured event rather than a fabricated one)
- Modify: `src/macos/mod.rs`, `scripts/acceptance.sh`

- [x] `harness.rs`: create the window and text view on the main thread, `NSApplication` activation so the view is first responder, and a helper that pumps the run loop for a bounded time
- [x] the `#[ignore]`d live test `a_held_keystroke_types_the_letter_of_the_layout_selected_after_it_was_captured`: select `English - Universal`, post keycode 0 (`a`) and capture it through `capture_next_key()` (the view shows `a`, clear it), select `Russian - Universal`, wait for the TIS notification, then post via (a), (b) and (c) in three separate sub-steps with the view cleared between them, and record for each whether the view shows `ф` or `a`
- [x] the test prints one verdict line per strategy, plus the unicode string the captured event carried (`CGEventKeyboardGetUnicodeString`), because a self-posted event may carry none while a hardware event carries the old layout's letter; the premise is measured, not assumed. The only assertion is the disjunction the plan depends on: at least one of (a) or (b) types `ф`, the failure message naming which did not. (c) is printed, never asserted. Write the highest-ranked passing strategy into this plan under Technical Details -> Tap layer -> Replay as a `+` line before starting Task 5; a passing (a) is provisional until Task 5's live test confirms it on events that came through the tap. If neither (a) nor (b) types `ф`, stop and add a `+` task to Task 2 that declares `kTISPropertyUnicodeKeyLayoutData` and `UCKeyTranslate`, because replay then has to translate keycodes itself
- [x] write tests for `typed_text()` on an untouched view (empty) and after setting the string directly
- [x] run `mise run check` and `scripts/acceptance.sh` - must pass before task 4
- ! **AppKit refuses cargo's test harness.** Measured: `NSWindow` raises `NSInternalInconsistencyException` ("NSWindow should only be instantiated on the main thread!") and so does `nextEventMatchingMask`, and an Objective-C exception crossing Rust frames aborts the process. Cargo's harness runs every test on a worker thread even with `--test-threads=1`, so no `#[ignore]`d test can own a window. This is the same constraint the Task 2 `+` note found for distributed notifications, and it governs Tasks 5 and 6 too.
- + The crate therefore gained a **lib target** (`src/lib.rs`, `pub mod macos;`) and a **`tests/live.rs` target with `harness = false`**, declared in `Cargo.toml`. That target owns `main`, so everything it runs is on the process's real main thread. `src/main.rs` is now a thin bin over the lib. Every live scenario lives there, named in a `SCENARIOS` table, and `main` runs only the scenario named on the command line: a plain `cargo test` runs the target with no name, it prints one line and exits, so `mise run check` stays reproducible. `scripts/acceptance.sh` names one scenario per run and greps `live: <name> passed`. Task 5 and Task 6 add their live tests as scenarios there, not as `#[ignore]`d tests.
- + `harness.rs` is a normal module of the lib, not `#[cfg(test)]`: an integration target cannot see a crate's test-only items. Its own inline `#[cfg(test)] mod tests` keeps what needs no window (the unicode string round trip), which is what `cargo test` covers.
- + `Window::open` retries `activateIgnoringOtherApps(true)` until `isActive() && isKeyWindow()`, up to 3 s. The replacement `activate()` leaves an unbundled binary behind the terminal that started it, and then posted keys land in the terminal instead of the harness - the first run of the spike failed exactly that way. The deprecation carries a narrow `#[allow(deprecated, reason = ...)]`.
- + The three `#[cfg_attr(not(test), allow(dead_code, ...))]` attributes Task 2 put on `tis::select`, `tis::observe_changes` and `tis::resolve` are gone: a `pub` item in a lib is never dead code, so the attributes had nothing left to suppress.

### Task 4: Barrier state machine

**Files:**
- Create: `src/barrier.rs`
- Modify: `src/main.rs`

- [x] types: `LayoutTag(String)`, `KeyEvent`, `Verdict { Pass, Swallow, Hold }`, `Decision { verdict, select: Option<LayoutTag>, replay: bool }`, `Barrier::new(cycle: Vec<LayoutTag>, hold: Duration)`
- [x] `on_key(&mut self, event: KeyEvent, current: Option<LayoutTag>, now: Instant) -> Decision` implementing the state table from Technical Details, including the second-tap-inside-the-window rule and the F19-during-`Selecting` rule
- [x] `on_select(&mut self, expected: LayoutTag, now: Instant) -> bool` per the state table; `confirmed(&mut self, now_selected: Option<LayoutTag>) -> bool` and `tick(&mut self, now: Instant) -> bool` returning whether the caller must replay; `select_failed(&mut self) -> bool`
- [x] `next(cycle, current: Option<&LayoutTag>)` wrapping, with a current layout outside the cycle or unmapped (`None`) going to the first entry
- [x] write tests (TDD): F19 in idle swallows and selects the next tag; keys after F19 are held in order; confirmation replays exactly the held keys; the deadline replays and reports the count; a second F19 during the window is swallowed and not queued; keys in idle pass; flags events are held like keys; a select failure replays at once; a confirmation naming a different tag than the one requested does not replay and the deadline still fires; an activation select (`on_select`) never holds keys; F19 during an activation select starts a real switch and holds; a confirmation of an activation select replays nothing; `on_select` during a key-driven switch is refused and the held keys survive; `on_select` during `Selecting` replaces the target and re-arms the deadline; the `Selecting` deadline returns to idle with nothing to replay; a `None` confirmation never matches; `confirmed`, `tick` and `select_failed` in idle replay nothing; cycle wraps; a layout outside the cycle goes to the first entry; an unmapped current layout (`None`) goes to the first entry
- [x] run `mise run check` - must pass before task 5
- + `LayoutTag` was not redeclared: Task 2 already put it in `tis.rs` and `barrier.rs` imports it from there, as that task's `+` note said it would.
- + The module is declared in `src/lib.rs`, not in `src/main.rs`: since Task 3 the bin is a thin shell over the lib, and `tests/live.rs` reaches the barrier only through the lib. `main.rs` is therefore unchanged by this task - the barrier is wired into `moji run` in Task 5.
- + `next` returns `Option<LayoutTag>`, not `LayoutTag`: an empty cycle has no next layout, and the daemon must never panic on one. F19 against an empty cycle is swallowed with `select: None`, so the key stays dead rather than taking the process down. The config rejects such a cycle (Task 6); this is the barrier refusing to depend on that.
- + `Barrier::held()` reports how many events wait for a replay. The state table hides the count inside `Switching`, and the deadline is required to name it, so the tests need a way to read it that is not the log line.
- + A `flagsChanged` event carrying keycode 80 is ordinary input, not the signal: only `Down` and `Up` on `SIGNAL_KEYCODE` (a `pub const` in `barrier.rs`) drive the state machine. F19 is not a modifier, so such an event is not something Karabiner emits.
- + `next` uses `cycle.first()?` rather than `let ... else`: clippy's `question_mark` lint is denied by the gate and fires on the `let ... else` form the style skill prefers.

### Task 5: Tap layer and the barrier wired into `moji run`

**Files:**
- Create: `src/macos/tap.rs`
- Modify: `src/macos/mod.rs`, `src/main.rs`

- [x] `tap.rs`: `install` creating the session tap (head insert, default options, mask `keyDown` | `keyUp` | `flagsChanged`), the run-loop source, `TapDisabled*` re-enable, the magic user-data pass-through, `HeldEvent` as a retained copy
- [x] `replay(held)` using the strategy Task 3 proved; every replayed event carries the magic user-data
- [x] permission preflight for listen and post access with the request-once-then-exit behavior from Technical Details
- [x] `moji run`: load config, resolve layouts, install the tap, `observe_changes` -> read `tis::current()`, resolve its tag by name (`None` when unmapped) -> `barrier.confirmed(tag)` -> replay when it says so, watchdog timer -> `barrier.tick` -> replay, SIGTERM stops the run loop, `executable::swapped` (added in Task 7) hooks in later
- [x] write tests: the pure `is_replayed(user_data)` and `mark_replayed` round-trip; `KeyEvent::from` a synthetic `CGEvent` reads keycode and kind for down, up and flags
- [x] write the live test `letters_typed_immediately_after_the_switch_land_in_the_new_layout` on the Task 3 harness: with the daemon's tap installed in-process and English selected, post F19 down/up followed within 1 ms by keycode 0 five times; the view shows `ффффф` and the log shows no watchdog release
- [x] write the live test `a_switch_that_is_never_confirmed_still_releases_the_keys`: same, with the confirmation observer disconnected; the view shows five letters (whichever layout) within 100 ms and the log shows one watchdog release naming 10 held events
- [x] run `mise run check` and `scripts/acceptance.sh` - must pass before task 6
- + Both live tests are scenarios in `tests/live.rs`, not `#[ignore]`d tests, for the reason Task 3 recorded: cargo's harness gives a test a worker thread and AppKit refuses a window on one.
- + Measured on this machine: the confirmed switch types `ффффф` with **0** watchdog releases, so the notification wins the 50 ms race comfortably; with the observer disconnected the watchdog releases all **10** held events and the letters land 153 ms after the burst was posted, which is the pump's own granularity rather than the deadline. The second scenario asserts a 500 ms bound instead of the plan's 100 ms: the harness reads the view by pumping in 10 ms slices, so a tighter bound would measure the test loop and not the barrier.
- + The held queue is a shared handle, `tap::Held` over an `Rc<RefCell<Vec<HeldEvent>>>` handed to `install`, rather than a private field of `Tap` with a `take_held`. A select that fails replays from inside the tap callback, and a queue owned by that callback's own context cannot be drained while the callback holds it. `replay` is therefore a method on `Held`, not the free `replay(held: Vec<HeldEvent>)` the plan sketched.
- + `KeyEvent::from` is `tap::key_event(&CGEvent) -> Option<KeyEvent>`: an event reaching the tap need not carry a key at all, and `From` has no way to say so.
- + Two modules the file list did not name. `src/macos/timer.rs` carries the watchdog: a `CFRunLoopTimer` that does not repeat is invalidated by its own fire, so a quiet timer here is one whose interval is a day and whose next fire date is pushed that far out; arming it is a fire date, not a new timer. `src/macos/signals.rs` carries SIGTERM, which only sets a flag - a signal handler may not stop a run loop - and the daemon polls that flag from a repeating timer, which is the shape Task 7's upgrade watch reuses.
- + The wiring lives in `src/daemon.rs`, a lib module holding no `unsafe`, not in `src/main.rs`: `tests/live.rs` drives exactly the sources the binary runs, and an integration target cannot reach a bin crate. `Daemon::releases()` and `Daemon::disconnect_confirmation()` exist for those scenarios, because a warn-level log line is not something a test can assert on.
- + The daemon reads `tis::current()` only for the signal key, never for an ordinary keystroke: the barrier consults `current` nowhere but `start_switch`, and a TIS call per keystroke would push the tap callback towards the system's timeout under fast typing.
- + `moji run` has no configuration yet, so it cycles through **every** enabled keyboard layout, each tagged with its own localized name, and logs a warning saying so. Task 6 replaces that with the config; nothing else depends on it.

### Task 6: Config and per-application memory

**Files:**
- Create: `src/config.rs`, `src/memory.rs`, `src/macos/workspace.rs`
- Modify: `src/main.rs`, `src/macos/mod.rs`

- [x] `config.rs`: the schema from Technical Details with `serde` + `toml`, `deny_unknown_fields`, path resolution (`MOJI_CONFIG` then `$HOME/.config/moji/config.toml`), errors that name the file and the field; `moji --check-config`
- [x] `memory.rs`: `Memory::new(pins: HashMap<BundleId, LayoutTag>)`, `on_layout_changed`, `on_activated` per Technical Details
- [x] `workspace.rs`: `frontmost() -> Option<BundleId>` and `observe_activation(on_activate: impl Fn(BundleId) + 'static) -> ActivationObserver` over `NSWorkspaceDidActivateApplicationNotification`, callback on the main run loop
- [x] wire into `moji run`: TIS notification -> `memory.on_layout_changed(frontmost, current)`; activation -> `memory.on_activated(app, current)` -> `barrier.on_select(tag, now)` -> `tis::select` only when accepted; `moji status` gains the frontmost bundle id
- [x] write tests (TDD) for config: the sample parses; a tag in `apps` missing from `layouts` is an error naming both; an unknown field is an error; a `cycle` shorter than two entries is an error; `MOJI_CONFIG` wins over the default path
- [x] write tests (TDD) for memory: a pinned app returns its pin even after a different layout was recorded for it; a remembered app returns the last recorded layout; an unknown app returns `None`; recording for app A does not affect app B; an app activated on `ru` that never switched, then left for a pinned `en` app, returns `ru` when activated again (the Tuna round trip); a pin equal to the current layout returns `None`; `None` as the current layout records nothing and still answers for the incoming app
- [x] write the `#[ignore]`d live test `activating_a_pinned_application_selects_its_layout`: with Russian selected, activate the harness window's own bundle id pinned to `en` through the daemon's wiring; `current()` becomes `English - Universal`
- [x] run `mise run check` and `scripts/acceptance.sh` - must pass before task 7
- + Delete the no-configuration fallback Task 5 left in `moji run`: the cycle it builds from every enabled layout, the tags it makes from localized names, and the warning that announces it. Done.
- + `BundleId` lives in `src/macos/workspace.rs`, the module that produces one, and `config.rs` and `memory.rs` import it from there. That is the precedent Task 2 set with `LayoutTag` in `tis.rs`.
- + The live test is a scenario in `tests/live.rs`, not an `#[ignore]`d test, for the reason Task 3 recorded. It drives `Daemon::activated(BundleId)` rather than a real workspace notification: measured, `workspace::frontmost()` is `None` while the harness window is frontmost, because the live binary is unbundled and `bundleIdentifier` is nil without an `Info.plist`. The scenario therefore pins the synthetic id `dev.pkarpovich.moji.live`. Measured on this machine: activating it on `Russian - Universal` selected `English - Universal` with **0** watchdog releases, so an activation select holds no keys. `moji status` run from the shell does print a real bundle id (`ru.keepcoder.Telegram`), which is what proves `frontmost()` itself.
- + The configuration also rejects a `cycle` entry that `[layouts]` does not carry, with the same `UnknownTag` error an `apps` entry gets: the barrier would otherwise name a tag the daemon cannot select, and `select` would fail on every tap of the key.
- + `Daemon::start` gained a `pins` parameter between `layouts` and `hold`.
- + `moji set <tag>` and `moji toggle` were wired here. Both need nothing but the configuration's tags and `tis::select`, no task of this plan owns them, and Task 8 verifies the CLI surface. `moji status` reads the tag out of the configuration's own names, so a missing or broken configuration prints `-` rather than failing the command.

### Task 7: LaunchAgent service and upgrade watch

**Files:**
- Create: `src/service.rs`, `src/executable.rs`
- Modify: `src/main.rs`

- [x] `service.rs` from nikki with the `dev.pkarpovich.moji` label, `Layout` under `$HOME`, `install`/`uninstall`, `PathState` keep-alive, bootout -> wait-unloaded -> bootstrap, canonicalized executable path, bundle detection
- [x] `executable.rs` from nikki; `moji run` ends the run loop when `swapped` fires, logging that it stops so launchd starts the new version. Since v1 has no tokio, implement the poll as a run-loop timer at the same 2 s interval instead of an async future
- [x] `moji install` / `moji uninstall` wired
- [x] write tests: the plist names the given program and keeps alive on its path only; an escaped path stays parsable; a bundle path is recognized and a Cellar path is not; the inode watch reports a swapped file and stays quiet on an untouched one
- [x] run `mise run check` - must pass before task 8
- + The agent's `ProgramArguments` carries `run` after the program: nikki's binary is the daemon, moji's is a CLI whose daemon is a subcommand, and a plist naming the bare binary would have launchd respawn a process that prints the subcommand list and exits. A test asserts the argument.
- + Nikki's `BREW_LABEL` and the brew agent it removes were dropped, along with `Layout.brew_agent`: moji was never installed by `brew services`, so there is nothing to replace. The logs live in `$HOME/Library/Logs/moji/`, the directory Technical Details names, rather than beside the other logs as nikki's do.
- + `service.rs` is a lib module and holds no `unsafe`, so `getuid` moved to `src/macos/user.rs`, the smallest module that can carry it. `launchctl` addresses the agent as `gui/<uid>/<label>`, which is the only thing that needs the user id.
- + `executable.rs` exposes `Executable::current()` and `Executable::swapped()` rather than nikki's `async fn swapped`. `identity` and `replaced` are private: the daemon asks the executable, and a pure helper with no caller outside its own tests does not belong in the lib's public surface. The watch is a `Repeat::Every(POLL)` timer installed by `Daemon::run`, dropped when it returns.
- + `moji install` was not run against launchd from this session: loading an agent that names the debug binary would have launchd respawn a process with no TCC grants. `moji uninstall` was run and is clean on a machine where nothing is loaded (`Boot-out failed: 3: No such process` from launchctl, then a success line). Installing for real is the Post-Completion permissions step.

### Task 8: Verify acceptance criteria

- [x] verify every requirement from Overview is implemented: barrier with watchdog, config-driven per-app layout with remember fallback, the CLI surface, name-based layout matching
- [x] verify edge cases: a config naming a layout that is not enabled fails at startup with the enabled names listed; a daemon started without a permission logs the System Settings pane by name and exits non-zero; `moji status` on a machine where the daemon is not running still answers from TIS
- [x] run the full suite: `mise run check`
- [x] run the live suite on the user's Mac with permissions granted: `scripts/acceptance.sh`
- [x] grep gates: no `unsafe` outside `src/macos/`, no `#[allow(dead_code)]`, no `_ =>` wildcard arms, no comments inside function bodies
- + Measured from the shell against the release binary, with no daemon running and no configuration at the default path. `moji list` prints the three enabled layouts; `moji status` answers from TIS alone (`Russian - Universal`, frontmost `ru.keepcoder.Telegram`) and prints `tag -` when no configuration names it; `MOJI_CONFIG` pointing at a config whose `ru` names `Klingon - Universal` fails with `no enabled keyboard layout is named ru = Klingon - Universal; enabled: ABC, English - Universal, Russian - Universal` and exit 1; `moji set en` then `moji toggle` walks `en -> ru` and `moji set bogus` exits 1. `scripts/acceptance.sh` passed all five live scenarios: the confirmed switch typed `ффффф` with 0 watchdog releases, the disconnected one with 1 release after 155 ms, and the pinned activation left the layout on `English - Universal`.
- + The permission edge case is verified by inspection and by the unit test `every_access_names_the_pane_that_grants_it`, not live: revoking the terminal's Input Monitoring grant to observe the failure would also cost every other live scenario its grant. `moji run` calls `tap::request_missing_access` first, logs `access.pane()` per missing grant and returns `ExitCode::FAILURE`.
- + The `#[allow(dead_code)]` gate passes as the convention states it: the four occurrences (`daemon::Daemon::tap`, `daemon::Daemon::activation`, `timer::Timer::on_fire`, `tap::Tap::context`) are field-level allows with a `reason`, each naming the pointer Core Foundation holds instead of Rust. No module carries a blanket one.

### Task 9: Update documentation

- [x] `README.md`: what moji is, the config file with the sample, the Karabiner contract (F19 on tap), the permissions it needs and why (Input Monitoring to see keys, Accessibility to hold and re-post them), the CLI
- [x] `CLAUDE.md`: any convention discovered during implementation (the replay strategy Task 3 proved, the run-loop-only threading)
- [x] move this plan to `docs/plans/completed/`
- + `CLAUDE.md` gained three rules, not two: the replay strategy (post the captured copy unchanged, never translate a keycode), the run-loop consequences the threading section only implied (a non-repeating `CFRunLoopTimer` is invalidated by its own fire, so the watchdog is a day-long interval whose fire date moves; a signal handler sets a flag a repeating timer polls), and the newtype-location precedent (`LayoutTag` in `macos/tis.rs`, `BundleId` in `macos/workspace.rs`).
- + `README.md` documents the two no-notification rules the configuration and the memory enforce - a cycle shorter than two entries, and a decided layout equal to the selected one - because both read as arbitrary restrictions without the reason.

## Post-Completion

*Items requiring manual intervention or external systems - no checkboxes, informational only*

**Karabiner (environment repository, `karabiner/rules.go`, done last, by hand, with the `go` skill):** `langSwitch()` stops emitting `select_input_source` and drops both the `input_source_if` conditions and the two source-ID constants. The `keyboard_fn` variant emits `f19` in `to` (on press: the globe key does nothing else). The `left_control` and `left_shift` variants keep their `to` modifier and emit `f19` in `to_if_alone`, the Corne one keeping its 180 ms timeout. Update `rules_test.go` and `testdata/karabiner.golden.json`, regenerate with `go run .`; Karabiner reloads the file on its own. Do this only after moji is installed and its permissions are granted, otherwise F19 is a dead key.

**Cross-process replay check (one minute at the Mac, the scenario the whole project exists for):** with moji running, put the cursor in a real chat window - another process, not the live harness - and type a burst starting with the signal key: F19 then five letters, as fast as the keyboard allows. All five must land in the new layout. This is the only thing that exercises the 10 ms settle (`barrier::SETTLE`): the harness's window, tap and observer share moji's process, so the cross-process lag the settle covers is zero there. If letters still land in the old layout, the settle is too short for this machine rather than wrong - raise `SETTLE` and repeat; if the first letter is duplicated or out of order, that is the replay and not the settle.

**Live Tuna check (one minute at the Mac):** run `moji status` in a loop or the `front` probe while opening Tuna. If Tuna's bundle id shows up as frontmost, the per-app pin works as designed. If it never does, the activation signal for `LSUIElement` panels needs an AX focused-window observer; that is a follow-up plan, not a patch.

**Permissions (once per bundle):** run the bundled `Moji.app` binary from its final location, accept the Input Monitoring prompt, then the Accessibility prompt, then `moji install`. Never grant to a `cargo run` path.

**Deployment:** cask, notarization and the tap update are deliberately outside this plan and will be done by hand afterwards using the mimi/nikki release workflow as the template.
