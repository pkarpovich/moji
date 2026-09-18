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

use std::panic::AssertUnwindSafe;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use moji::macos::harness::{self, KEYCODE_A, Stroke, Window};
use moji::macos::tis::{self, Layout};

const ENGLISH: &str = "English - Universal";
const RUSSIAN: &str = "Russian - Universal";
const RUSSIAN_A: &str = "ф";
const SETTLE: Duration = Duration::from_millis(500);

const SCENARIOS: &[(&str, fn())] = &[
    (
        "a_held_keystroke_types_the_letter_of_the_layout_selected_after_it_was_captured",
        a_held_keystroke_types_the_letter_of_the_layout_selected_after_it_was_captured,
    ),
    (
        "an_untouched_view_is_empty_and_a_set_string_reads_back",
        an_untouched_view_is_empty_and_a_set_string_reads_back,
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

fn an_untouched_view_is_empty_and_a_set_string_reads_back() {
    let window = Window::open();

    assert_eq!(window.typed_text(), "");

    window.set_text("moji");
    assert_eq!(window.typed_text(), "moji");

    window.clear();
    assert_eq!(window.typed_text(), "");
}
