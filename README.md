# moji

moji (文字) is a macOS LaunchAgent that becomes the only thing on the machine that switches keyboard
layouts.

Switching a layout and typing the next letter normally travel on two unrelated paths, and nothing
orders them, so letters typed right after a fast switch land in the old layout. moji fixes the
ordering: Karabiner emits one signal key, F19, moji swallows it, selects the next layout, and holds
every following keystroke until the change is confirmed - then replays them in order. A watchdog
releases them after 50 ms regardless, so the keyboard can never hang.

It also pins a layout per application and remembers the layout every other application last used.

The full design, and the reasoning behind every decision, is in
`docs/plans/completed/20260918-moji-layout-daemon.md`; the coding conventions are in `CLAUDE.md`.

## Configuration

`$HOME/.config/moji/config.toml`, or wherever `MOJI_CONFIG` points.

```toml
cycle = ["en", "ru"]

[layouts]
en = "English - Universal"
ru = "Russian - Universal"

[apps]
"com.brnbw.Tuna" = "en"
```

- `[layouts]` maps a short tag to the **localized name** of an enabled keyboard layout, the one
  `moji list` prints in its first column. Names are matched, never input source IDs: the same layout
  reports two different IDs depending on which process asks.
- `cycle` is the order the switch key and `moji toggle` walk, wrapping at the end. A layout outside
  the cycle, or one no tag names at all, goes to the first entry. The cycle needs at least two
  entries: selecting the layout that is already selected is confirmed by no notification, so the
  barrier would wait for the watchdog on every tap.
- `[apps]` pins a layout to a bundle id. Every application not listed gets the layout it last used,
  which moji records when the application loses focus and whenever the layout changes. An
  application whose decided layout is already the selected one is left alone, for the same
  no-notification reason.
- An unknown field, a tag `[layouts]` does not carry, or a name no enabled layout answers to is an
  error that names the file, the field and what was expected. `moji --check-config` parses, resolves
  every tag against the enabled layouts, prints what it made of the file, and exits.

## The contract with Karabiner

Karabiner stops switching layouts. It keeps its tap-vs-hold logic per device and emits **F19**
(virtual keycode 80) instead of `select_input_source`: globe on the built-in keyboard, left Control
on external keyboards, left Shift on the Corne. moji swallows both the down and the up of that key
and owns everything that follows.

While moji is not running, F19 is an unbound key and nothing switches. That is the accepted failure
mode of having a single owner; launchd's `KeepAlive` on the program path and the upgrade
self-restart are what keep the window small.

## Permissions

Two TCC grants, both to the application bundle rather than to a path that changes with every
version - run `scripts/bundle.sh`, move `Moji.app` to its final location, start it once from there,
and grant:

- **Input Monitoring** - the event tap has to see the keystrokes to know there are any to hold.
- **Accessibility** - holding a keystroke means swallowing it and posting it again later.

`moji run` preflights both. When either is missing it asks for it once so the system prompt appears,
logs the System Settings pane by name, and exits non-zero; launchd starts it again once the grant
lands.

## Commands

```
moji run          own the switch key and the layout for as long as the process lives
moji set <tag>    select the layout a tag names
moji toggle       select the layout after the current one in the cycle
moji status       print the current layout, its id, language, tag, and the frontmost application
moji list         print every enabled keyboard layout as name, id and language
moji install      run this binary as a launchd agent
moji uninstall    unload the agent and remove what install wrote
moji --check-config
moji --version
```

Everything but `run` is one-shot and talks to Text Input Sources directly: there is no daemon socket,
and `moji status` answers on a machine where no daemon is running.

## Development

```
mise run check    fmt, clippy with -D warnings, and the reproducible tests
mise run build
./scripts/bundle.sh target/release/moji target    assemble Moji.app
./scripts/acceptance.sh                           the live suite, needs permissions granted
```
