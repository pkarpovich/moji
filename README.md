# moji

moji (文字) is a macOS LaunchAgent that becomes the only thing on the machine that switches keyboard
layouts.

Switching a layout and typing the next letter normally travel on two unrelated paths, and nothing
orders them, so letters typed right after a fast switch land in the old layout. moji fixes the
ordering: Karabiner emits one signal key, F19, moji swallows it, selects the next layout, and holds
every following keystroke until the change is confirmed - then replays them in order. A watchdog
releases them after 50 ms regardless, so the keyboard can never hang.

It also pins a layout per application and remembers the layout every other application last used.

Status: under construction. The full design, and the reasoning behind every decision, is in
`docs/plans/20260918-moji-layout-daemon.md`; the coding conventions are in `CLAUDE.md`.

## Commands

```
moji run          own the switch key and the layout for as long as the process lives
moji set <tag>    select the layout a tag names
moji toggle       select the layout after the current one in the cycle
moji status       print the current layout, its tag, and the frontmost application
moji list         print every enabled keyboard layout
moji install      run this binary as a launchd agent
moji uninstall    unload the agent and remove what install wrote
moji --check-config
moji --version
```

## Development

```
mise run check    fmt, clippy with -D warnings, and the reproducible tests
mise run build
./scripts/bundle.sh target/release/moji target    assemble Moji.app
./scripts/acceptance.sh                           the live suite, needs permissions granted
```
