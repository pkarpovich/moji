//! The tests that need the live machine: a real window, a real event tap, a real layout switch.
//!
//! Cargo's test harness runs every test on a worker thread, and AppKit refuses to instantiate an
//! `NSWindow` or pump the event queue anywhere but the main thread, so this target declares
//! `harness = false` and owns `main`. `scripts/acceptance.sh` names one scenario per run; a plain
//! `cargo test` runs this binary with no name and it does nothing, which is what keeps `cargo test`
//! reproducible on a machine where nothing in particular is open.
//!
//! The terminal that runs these needs Input Monitoring and Accessibility, because TCC attributes
//! the tap and the posted events to the process responsible for this one.

use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use moji::barrier::{RETYPE_KEYCODE, SWITCH_KEYCODE};
use moji::daemon::{Daemon, Releases};
use moji::macos::harness::{self, KEYCODE_A, Stroke, Window};
use moji::macos::tis::{self, Layout, LayoutTag};
use moji::macos::workspace::{self, BundleId};

const ENGLISH: &str = "English - Universal";
const RUSSIAN: &str = "Russian - Universal";
const RUSSIAN_A: &str = "ф";
const SETTLE: Duration = Duration::from_millis(500);
const HOLD: Duration = Duration::from_millis(50);
const BURST: usize = 5;
const FOCUS_SETTLE: Duration = Duration::from_millis(150);
const SENTENCE: &[u16] = &[0, 35, 35, 49, 9, 14, 15, 1, 34, 31, 45];
const SENTENCE_IN_RUSSIAN: &str = "фзз мукышщт";
const SENTENCE_WORD_RETYPED: &str = "фзз version";
const SENTENCE_RETYPED: &str = "app version";
const GREETING: &[u16] = &[5, 4, 11, 2, 17, 45];
const GREETING_IN_ENGLISH: &str = "ghbdtn";
const GREETING_RETYPED: &str = "привет";

const SCENARIOS: &[(&str, fn())] = &[
    (
        "a_held_keystroke_types_the_letter_of_the_layout_selected_after_it_was_captured",
        a_held_keystroke_types_the_letter_of_the_layout_selected_after_it_was_captured,
    ),
    (
        "an_untouched_view_is_empty_and_a_set_string_reads_back",
        an_untouched_view_is_empty_and_a_set_string_reads_back,
    ),
    (
        "letters_typed_immediately_after_the_switch_land_in_the_new_layout",
        letters_typed_immediately_after_the_switch_land_in_the_new_layout,
    ),
    (
        "a_switch_that_is_never_confirmed_still_releases_the_keys",
        a_switch_that_is_never_confirmed_still_releases_the_keys,
    ),
    (
        "activating_a_pinned_application_selects_its_layout",
        activating_a_pinned_application_selects_its_layout,
    ),
    (
        "a_sentence_typed_in_the_wrong_layout_is_retyped_word_first_then_whole",
        a_sentence_typed_in_the_wrong_layout_is_retyped_word_first_then_whole,
    ),
    (
        "a_word_typed_before_a_manual_switch_is_retyped_without_a_second_switch",
        a_word_typed_before_a_manual_switch_is_retyped_without_a_second_switch,
    ),
];

fn main() -> ExitCode {
    let mut requested = Vec::new();
    for argument in std::env::args().skip(1) {
        requested.push(argument);
    }

    if requested.is_empty() {
        println!("live: no scenario named, so nothing ran; scripts/acceptance.sh names them");
        return ExitCode::SUCCESS;
    }

    let mut failed = 0;
    for name in requested {
        let Some(scenario) = scenario_named(&name) else {
            println!(
                "live: {name} is not a scenario, and these are: {}",
                every_name()
            );
            return ExitCode::FAILURE;
        };

        let started_on = tis::current();
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(scenario));
        restore(started_on);
        match outcome {
            Ok(()) => println!("live: {name} passed"),
            Err(_) => {
                println!("live: {name} failed");
                failed += 1;
            }
        }
    }

    if failed > 0 {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn scenario_named(name: &str) -> Option<fn()> {
    for (candidate, scenario) in SCENARIOS {
        if *candidate == name {
            return Some(*scenario);
        }
    }
    None
}

fn every_name() -> String {
    let mut names = String::new();
    for (name, _) in SCENARIOS {
        if !names.is_empty() {
            names.push_str(", ");
        }
        names.push_str(name);
    }
    names
}

fn restore(layout: Option<Layout>) {
    let Some(layout) = layout else {
        return;
    };
    let Err(error) = tis::select(&layout) else {
        return;
    };
    println!("live: the layout could not be restored: {error}");
}

fn layout_named(name: &str) -> Layout {
    let layouts = tis::enabled_layouts();
    let mut found = None;
    for layout in &layouts {
        let Layout {
            name: candidate,
            id: _,
            language: _,
        } = layout;
        if candidate == name {
            found = Some(layout.clone());
            break;
        }
    }
    let Some(found) = found else {
        panic!("the layout {name} is not enabled, so this scenario cannot run");
    };
    found
}

fn select_and_wait(window: &Window, layout: &Layout) {
    let Layout {
        name,
        id: _,
        language: _,
    } = layout;
    let Ok(()) = tis::select(layout) else {
        panic!("selecting {name} failed");
    };

    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let Some(current) = tis::current() else {
            panic!("no keyboard layout is selected at all");
        };
        let Layout {
            name: selected,
            id: _,
            language: _,
        } = &current;
        if selected == name {
            window.pump(Duration::from_millis(50));
            return;
        }
        if Instant::now() >= deadline {
            panic!("{name} was not selected within a second, it stayed on {selected}");
        }
        window.pump(Duration::from_millis(10));
    }
}

fn typed_after(window: &Window, post_stroke: impl FnOnce()) -> String {
    window.clear();
    post_stroke();
    harness::post_key(KEYCODE_A, Stroke::Up);
    window.wait_for_text(SETTLE)
}

fn a_held_keystroke_types_the_letter_of_the_layout_selected_after_it_was_captured() {
    let english = layout_named(ENGLISH);
    let russian = layout_named(RUSSIAN);

    let window = Window::open();
    select_and_wait(&window, &english);

    let capture = harness::capture_next_key();
    let typed = typed_after(&window, || harness::post_key(KEYCODE_A, Stroke::Down));
    let Some(captured) = capture.take() else {
        panic!("the listen-only tap saw no keyDown: is Input Monitoring granted?");
    };
    drop(capture);
    assert_eq!(
        typed, "a",
        "posting keycode {KEYCODE_A} on {ENGLISH} did not type a"
    );

    let carried = harness::unicode_string(&captured);
    select_and_wait(&window, &russian);

    let replayed = typed_after(&window, || harness::post(&captured));
    let fresh = typed_after(&window, || harness::post_key(KEYCODE_A, Stroke::Down));
    let rewritten = typed_after(&window, || {
        let Some(event) = harness::copy(&captured) else {
            panic!("copying the captured event failed");
        };
        harness::set_unicode_string(&event, RUSSIAN_A);
        harness::post(&event);
    });

    println!("live: the captured event carried {carried:?}");
    println!("live: (a) the captured event replayed typed {replayed:?}");
    println!("live: (b) a fresh keyboard event typed {fresh:?}");
    println!("live: (c) the captured event rewritten typed {rewritten:?}");

    assert!(
        replayed == RUSSIAN_A || fresh == RUSSIAN_A,
        "neither (a) nor (b) typed {RUSSIAN_A} after the switch: (a) typed {replayed:?}, (b) typed {fresh:?}"
    );
}

fn start_daemon(english: &Layout, russian: &Layout) -> Daemon {
    start_daemon_pinning(english, russian, BTreeMap::new())
}

fn start_daemon_pinning(
    english: &Layout,
    russian: &Layout,
    pins: BTreeMap<BundleId, LayoutTag>,
) -> Daemon {
    let english_tag = LayoutTag("en".to_string());
    let russian_tag = LayoutTag("ru".to_string());

    let mut layouts = BTreeMap::new();
    layouts.insert(english_tag.clone(), english.clone());
    layouts.insert(russian_tag.clone(), russian.clone());
    let cycle = vec![english_tag, russian_tag];

    let Ok(daemon) = Daemon::start(cycle, layouts, pins, HOLD) else {
        panic!(
            "the daemon could not install its tap: is Input Monitoring granted to this terminal?"
        );
    };
    daemon
}

fn post_switch_and_burst() {
    harness::post_key(SWITCH_KEYCODE, Stroke::Down);
    harness::post_key(SWITCH_KEYCODE, Stroke::Up);
    for _ in 0..BURST {
        harness::post_key(KEYCODE_A, Stroke::Down);
        harness::post_key(KEYCODE_A, Stroke::Up);
    }
}

fn wait_for_length(window: &Window, length: usize, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let typed = window.typed_text();
        if typed.chars().count() >= length {
            return typed;
        }
        if Instant::now() >= deadline {
            return typed;
        }
        window.pump(Duration::from_millis(10));
    }
}

fn letters_typed_immediately_after_the_switch_land_in_the_new_layout() {
    let english = layout_named(ENGLISH);
    let russian = layout_named(RUSSIAN);

    let window = Window::open();
    select_and_wait(&window, &english);

    let daemon = start_daemon(&english, &russian);
    window.clear();
    post_switch_and_burst();

    let typed = wait_for_length(&window, BURST, SETTLE * 4);
    let Releases { count, last } = daemon.releases();
    drop(daemon);

    println!("live: the window shows {typed:?} after {count} watchdog releases");
    assert_eq!(
        typed,
        RUSSIAN_A.repeat(BURST),
        "the letters typed right after the switch did not land in {RUSSIAN}"
    );
    assert_eq!(
        count, 0,
        "the watchdog released {last} events, so the confirmation lost the race it should win"
    );
}

fn a_switch_that_is_never_confirmed_still_releases_the_keys() {
    let english = layout_named(ENGLISH);
    let russian = layout_named(RUSSIAN);

    let window = Window::open();
    select_and_wait(&window, &english);

    let mut daemon = start_daemon(&english, &russian);
    daemon.disconnect_confirmation();
    window.clear();

    let started = Instant::now();
    post_switch_and_burst();
    let typed = wait_for_length(&window, BURST, SETTLE * 4);
    let waited = started.elapsed();
    let Releases { count, last } = daemon.releases();
    drop(daemon);

    println!(
        "live: the window shows {typed:?} after {waited:?}, released by the watchdog {count} time(s)"
    );
    assert_eq!(
        typed.chars().count(),
        BURST,
        "the keys held by a switch nobody confirmed never came out: the window shows {typed:?}"
    );
    assert_eq!(
        count, 1,
        "the watchdog released the held keys {count} times"
    );
    assert_eq!(
        last,
        BURST * 2,
        "the watchdog released {last} events, not the {} it was holding",
        BURST * 2
    );
    assert!(
        waited < Duration::from_millis(500),
        "the keyboard was stuck for {waited:?}, which is far past the {HOLD:?} deadline"
    );
}

fn wait_for_layout(window: &Window, name: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let Some(current) = tis::current() else {
            panic!("no keyboard layout is selected at all");
        };
        let Layout {
            name: selected,
            id: _,
            language: _,
        } = current;
        if selected == name {
            return selected;
        }
        if Instant::now() >= deadline {
            return selected;
        }
        window.pump(Duration::from_millis(10));
    }
}

fn activating_a_pinned_application_selects_its_layout() {
    let english = layout_named(ENGLISH);
    let russian = layout_named(RUSSIAN);

    let window = Window::open();
    select_and_wait(&window, &russian);

    let app = match workspace::frontmost() {
        Some(app) => app,
        None => BundleId("dev.pkarpovich.moji.live".to_string()),
    };
    println!("live: the pinned application is {app}");

    let mut pins = BTreeMap::new();
    pins.insert(app.clone(), LayoutTag("en".to_string()));
    let daemon = start_daemon_pinning(&english, &russian, pins);

    daemon.activated(app);
    let selected = wait_for_layout(&window, ENGLISH, SETTLE);
    let Releases { count, last: _ } = daemon.releases();
    drop(daemon);

    println!("live: activating the pinned application left the layout on {selected:?}");
    assert_eq!(
        selected, ENGLISH,
        "the application pinned to en was activated on {RUSSIAN} and the layout stayed there"
    );
    assert_eq!(
        count, 0,
        "an activation select held keys, which it must never do"
    );
}

fn an_untouched_view_is_empty_and_a_set_string_reads_back() {
    let window = Window::open();

    assert_eq!(window.typed_text(), "");

    window.set_text("moji");
    assert_eq!(window.typed_text(), "moji");

    window.clear();
    assert_eq!(window.typed_text(), "");
}

fn post_keys(keycodes: &[u16]) {
    for keycode in keycodes {
        harness::post_key(*keycode, Stroke::Down);
        harness::post_key(*keycode, Stroke::Up);
    }
}

fn post_retype() {
    harness::post_key(RETYPE_KEYCODE, Stroke::Down);
    harness::post_key(RETYPE_KEYCODE, Stroke::Up);
}

fn wait_for_exactly(window: &Window, wanted: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let typed = window.typed_text();
        if typed == wanted {
            return typed;
        }
        if Instant::now() >= deadline {
            return typed;
        }
        window.pump(Duration::from_millis(10));
    }
}

fn a_sentence_typed_in_the_wrong_layout_is_retyped_word_first_then_whole() {
    let english = layout_named(ENGLISH);
    let russian = layout_named(RUSSIAN);

    let window = Window::open();
    select_and_wait(&window, &russian);

    let daemon = start_daemon(&english, &russian);
    window.clear();
    window.pump(FOCUS_SETTLE);

    post_keys(SENTENCE);
    let typed = wait_for_exactly(&window, SENTENCE_IN_RUSSIAN, SETTLE * 4);
    println!("live: the sentence landed as {typed:?}");
    assert_eq!(
        typed, SENTENCE_IN_RUSSIAN,
        "the keycodes of {SENTENCE_RETYPED:?} did not type {SENTENCE_IN_RUSSIAN:?} on {RUSSIAN}"
    );

    post_retype();
    let word = wait_for_exactly(&window, SENTENCE_WORD_RETYPED, SETTLE * 4);
    let selected = wait_for_layout(&window, ENGLISH, SETTLE);
    println!("live: the first press left {word:?} on {selected:?}");
    assert_eq!(
        word, SENTENCE_WORD_RETYPED,
        "the first press of the retype key did not retype the last word alone"
    );
    assert_eq!(
        selected, ENGLISH,
        "the retype did not leave {ENGLISH} selected, so the following keys land in {RUSSIAN}"
    );

    post_retype();
    let whole = wait_for_exactly(&window, SENTENCE_RETYPED, SETTLE * 4);
    let Releases { count, last } = daemon.releases();
    drop(daemon);

    println!("live: the second press left {whole:?} after {count} watchdog releases");
    assert_eq!(
        whole, SENTENCE_RETYPED,
        "the second press of the retype key did not cover the whole tail"
    );
    assert_eq!(
        count, 0,
        "the watchdog released {last} events, so a confirmation lost the race it should win"
    );
}

fn a_word_typed_before_a_manual_switch_is_retyped_without_a_second_switch() {
    let english = layout_named(ENGLISH);
    let russian = layout_named(RUSSIAN);

    let window = Window::open();
    select_and_wait(&window, &english);

    let daemon = start_daemon(&english, &russian);
    window.clear();
    window.pump(FOCUS_SETTLE);

    post_keys(GREETING);
    let typed = wait_for_exactly(&window, GREETING_IN_ENGLISH, SETTLE * 4);
    println!("live: the word landed as {typed:?}");
    assert_eq!(
        typed, GREETING_IN_ENGLISH,
        "the keycodes of {GREETING_IN_ENGLISH:?} did not type it on {ENGLISH}"
    );

    select_and_wait(&window, &russian);
    let Releases {
        count: before,
        last: _,
    } = daemon.releases();

    post_retype();
    let retyped = wait_for_exactly(&window, GREETING_RETYPED, SETTLE * 4);
    let selected = wait_for_layout(&window, RUSSIAN, SETTLE);
    let Releases { count, last } = daemon.releases();
    drop(daemon);

    println!("live: the press after the manual switch left {retyped:?} on {selected:?}");
    assert_eq!(
        retyped, GREETING_RETYPED,
        "the retype after a manual switch did not retype the word in {RUSSIAN}"
    );
    assert_eq!(
        selected, RUSSIAN,
        "the retype switched the layout a second time, away from the one the user chose"
    );
    assert_eq!(
        count, before,
        "the retype held its keys behind a switch nobody confirmed, releasing {last} events"
    );
}
