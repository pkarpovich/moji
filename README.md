# moji

moji (文字) is a macOS LaunchAgent that becomes the only thing on the machine that switches keyboard layouts.

Switching a layout and typing the next letter normally travel on two unrelated paths, and nothing orders them, so letters typed right after a fast switch land in the old layout. moji fixes the ordering: Karabiner emits one signal key, F19, moji swallows it, selects the next layout, and holds every following keystroke until the change is confirmed - then replays them in order. The confirmation is a notification moji observes in its own process, and every other process learns of the switch a few milliseconds later, so the replay waits 10 ms past the confirmation. A watchdog releases the keystrokes after 50 ms regardless, so the keyboard can never hang.

It also pins a layout per application and remembers the layout every other application last used.

The full design, and the reasoning behind every decision, is in `docs/plans/completed/20260918-moji-layout-daemon.md`; the coding conventions are in `CLAUDE.md`.

## Install

```
brew install --cask pkarpovich/apps/moji
```

Write `~/.config/moji/config.toml` **before** installing the service, then `moji --check-config`, `moji install`, and grant Input Monitoring and Accessibility to Moji when it asks. Upgrades need nothing: the running daemon notices the new bundle and restarts itself under launchd.

## Configuration

`$HOME/.config/moji/config.toml`, or wherever `MOJI_CONFIG` points. moji ships no default and writes none: `run`, `set` and `toggle` exit non-zero while the file is missing, and an installed agent without one is restarted by launchd until it exists. `status` and `list` answer without it.

```toml
cycle = ["en", "ru"]

[layouts]
en = "English - Universal"
ru = "Russian - Universal"

[apps]
"com.brnbw.Tuna" = "en"
```

- `[layouts]` maps a short tag to the **localized name** of an enabled keyboard layout, the one `moji list` prints in its first column. Names are matched, never input source IDs: the same layout reports two different IDs depending on which process asks.
- `cycle` is the order the switch key and `moji toggle` walk, wrapping at the end. A layout outside the cycle, or one no tag names at all, goes to the first entry. The cycle needs at least two entries, and no tag may stand in it twice: selecting the layout that is already selected is confirmed by no notification, so the barrier would wait for the watchdog on every tap.
- `[apps]` pins a layout to a bundle id. Every application not listed gets the layout it last used, which moji records when the application loses focus and whenever the layout changes. An application whose decided layout is already the selected one is left alone, for the same no-notification reason. Which application the keyboard goes to is asked of Accessibility every 50 ms rather than of the workspace: a launcher such as Tuna shows its panel without activating, so the workspace keeps naming the window behind it while the panel takes the keys, and Accessibility's focused application follows the keys. `moji status` prints that bundle id, which is how to find the one to pin.
- An unknown field, a tag `[layouts]` does not carry, or a name no enabled layout answers to is an error that names the file, the field and what was expected. `moji --check-config` parses, resolves every tag against the enabled layouts, prints what it made of the file, and exits.

## Retyping the last word

The second signal key, **F18**, retypes what was just typed in the other layout. moji keeps the keystrokes it saw - up to 512 of them - and a press posts that many backspaces, selects the next layout in the cycle, and replays the very events it captured. A replayed keystroke types the letter of the layout selected after it was captured, because the receiving application translates the keycode itself, so moji never learns which letter a keycode makes and never touches the text field.

- The first press covers the **last word**: the letters back to the space before them, plus the spaces that follow.
- The next press, with nothing typed in between, covers the **whole tail**: everything typed since the layout last changed under the fingers. Typing again makes the press after it a word again, and a third press flips the same tail once more, which with a two-layout cycle puts it back.
- The layout to retype in is the one after the layout the covered keystrokes were typed in, so a word typed before a manual switch is retyped without switching again.

What moji did not see, it cannot flip. The history holds only keystrokes that passed the tap since it started, so text typed before moji ran, pasted text, and text selected with the mouse are out of reach. These forget the history outright: a click, Return, Tab, Escape, an arrow or any other caret key, a chord carrying Command or Control, the keyboard moving to another application, and a keystroke typed in a layout no tag in the configuration names. Delete drops the last keystroke, as it did downstream.

A press with nothing in reach does nothing, and so does one while a switch is still running. When the layout cannot be selected, nothing is deleted and nothing is replayed: the text stays as it was typed.

## The contract with Karabiner

Karabiner stops switching layouts. It keeps its tap-vs-hold logic per device and emits **F19** (virtual keycode 80) instead of `select_input_source`: globe on the built-in keyboard, left Control on external keyboards, left Shift on the Corne. moji swallows both the down and the up of that key and owns everything that follows.

The retype key is **F18** (virtual keycode 79), emitted on a key of its own, and moji swallows its down, its repeat and its up the same way. Neither keycode is configurable: which physical key produces it is Karabiner's half of the contract.

While moji is not running, F19 and F18 are unbound keys and nothing switches. That is the accepted failure mode of having a single owner; launchd's `KeepAlive` on the program path and the upgrade self-restart are what keep the window small.

## Permissions

Two TCC grants, both to the application bundle rather than to a path that changes with every version - run `scripts/bundle.sh`, move `Moji.app` to its final location, start it once from there, and grant:

- **Input Monitoring** - the event tap has to see the keystrokes to know there are any to hold.
- **Accessibility** - holding a keystroke means swallowing it and posting it again later.

`moji run` preflights both. When either is missing it asks for it once so the system prompt appears, logs the System Settings pane by name, and exits non-zero; launchd starts it again once the grant lands.

## Commands

```
moji run          own the switch key and the layout for as long as the process lives
moji set <tag>    select the layout a tag names
moji toggle       select the layout after the current one in the cycle
moji status       print the current layout, its id, language, tag, and the application the keyboard goes to
moji list         print every enabled keyboard layout as name, id and language
moji install      run this binary as a launchd agent
moji uninstall    unload the agent and remove what install wrote
moji --check-config
                  resolve every configured tag against the enabled layouts, print what moji made
                  of the file, and exit
moji --version, -V
                  print the version
```

Everything but `run` is one-shot and talks to Text Input Sources directly: there is no daemon socket, and `moji status` answers on a machine where no daemon is running.

## Logging

`moji run` logs at `info` by default, and a switch that confirmed in time logs nothing: silence is the normal path. Two things are always reported: the watchdog releasing held keys because no confirmation arrived within 50 ms (a `warn` naming how many keys it let go and how many times that happened since start), and a layout that could not be selected. `MOJI_LOG=debug` adds one line per step of every switch: what asked for it (the signal key or the application the keyboard moved to), when the confirmation arrived and how many keys were waiting, and when they went through. `MOJI_LOG` takes any `tracing` filter, so `MOJI_LOG=moji::daemon=debug` narrows it to the daemon.

`moji install` writes `~/Library/LaunchAgents/dev.pkarpovich.moji.plist` naming the running binary with its symlinks resolved, and captures the daemon's output in `~/Library/Logs/moji/moji.log` and `moji.err.log` - that is where an installed daemon's log lines go, since launchd gives it no terminal. `moji uninstall` unloads the agent and removes the plist; the logs stay.

## Development

```
mise run check    fmt, clippy with -D warnings, and the reproducible tests
mise run build
./scripts/bundle.sh target/release/moji target [identity]
                  assemble Moji.app, signed with the hardened runtime when a codesigning
                  identity is given and unsigned when it is not
./scripts/acceptance.sh                           the live suite, needs permissions granted
```
