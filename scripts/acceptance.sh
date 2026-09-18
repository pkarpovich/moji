#!/usr/bin/env bash
set -euo pipefail

IDENTIFIER="dev.pkarpovich.moji"

# Every live test below installs an event tap or switches the layout, and TCC attributes both to the
# process responsible for this script - the terminal running it. That terminal needs Input
# Monitoring and Accessibility once, or these tests fail for a reason that has nothing to do with
# the code. They live in the `live` target rather than in cargo's test harness because AppKit
# refuses a window on anything but the main thread, which that harness never gives a test.
LIVE_TESTS=(
	a_held_keystroke_types_the_letter_of_the_layout_selected_after_it_was_captured
	an_untouched_view_is_empty_and_a_set_string_reads_back
	letters_typed_immediately_after_the_switch_land_in_the_new_layout
	a_switch_that_is_never_confirmed_still_releases_the_keys
)

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

echo "acceptance: the release binary"
cargo build --release
binary="target/release/moji"

echo "acceptance: the embedded Info.plist"
plist="$scratch/Info.plist"
otool -P "$binary" | tail -n +3 >"$plist"

if ! plutil -lint "$plist" >/dev/null 2>&1; then
	echo "acceptance: $binary carries no readable __TEXT,__info_plist section" >&2
	exit 1
fi

identifier="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$plist" 2>/dev/null || echo missing)"
if [ "$identifier" != "$IDENTIFIER" ]; then
	echo "acceptance: CFBundleIdentifier is $identifier, so every TCC grant made against $IDENTIFIER is lost" >&2
	exit 1
fi

echo "acceptance: the application bundle"
app="$(./scripts/bundle.sh "$binary" "$scratch" | tail -n1)"
bundled="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app/Contents/Info.plist")"
if [ "$bundled" != "$IDENTIFIER" ]; then
	echo "acceptance: $app declares $bundled, so the Input Monitoring and Accessibility grants do not follow it" >&2
	exit 1
fi

if [ ${#LIVE_TESTS[@]} -eq 0 ]; then
	echo "acceptance: no live test exists yet, so nothing here touched a tap or a layout"
else
	for name in "${LIVE_TESTS[@]}"; do
		echo "acceptance: $name"
		output="$(cargo test --test live -- "$name" 2>&1 || true)"
		echo "$output"
		if ! echo "$output" | grep -q "^live: $name passed$"; then
			echo "acceptance: $name neither ran nor passed" >&2
			exit 1
		fi
	done
fi

echo "acceptance: every check passed, and $binary is the binary they ran against"
