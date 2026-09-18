use std::collections::BTreeMap;
use std::process::ExitCode;

use argh::FromArgs;
use moji::barrier;
use moji::config::{self, Config};
use moji::daemon::{self, Daemon};
use moji::macos::tap;
use moji::macos::tis::{self, Layout, LayoutTag};
use moji::macos::workspace;

const SUBCOMMANDS: &str = "run, set, toggle, status, list, install, uninstall";

/// moji owns keyboard layout switching on this Mac.
#[derive(FromArgs, Debug, PartialEq, Eq)]
struct Args {
    /// print the version and exit
    #[argh(switch, short = 'V')]
    version: bool,
    /// load the configuration, resolve every layout against the enabled ones, then exit
    #[argh(switch)]
    check_config: bool,
    #[argh(subcommand)]
    command: Option<Command>,
}

#[derive(FromArgs, Debug, PartialEq, Eq)]
#[argh(subcommand)]
enum Command {
    Run(Run),
    Set(Set),
    Toggle(Toggle),
    Status(Status),
    List(List),
    Install(Install),
    Uninstall(Uninstall),
}

/// own the switch key and the layout for as long as this process lives
#[derive(FromArgs, Debug, PartialEq, Eq)]
#[argh(subcommand, name = "run")]
struct Run {}

/// select the layout a tag names in the configuration
#[derive(FromArgs, Debug, PartialEq, Eq)]
#[argh(subcommand, name = "set")]
struct Set {
    /// the tag of the layout to select, as named in the configuration
    #[argh(positional)]
    tag: String,
}

/// select the layout after the current one in the configured cycle
#[derive(FromArgs, Debug, PartialEq, Eq)]
#[argh(subcommand, name = "toggle")]
struct Toggle {}

/// print the current layout, its tag if it has one, and the frontmost application
#[derive(FromArgs, Debug, PartialEq, Eq)]
#[argh(subcommand, name = "status")]
struct Status {}

/// print every enabled keyboard layout, so its name can be copied into the configuration
#[derive(FromArgs, Debug, PartialEq, Eq)]
#[argh(subcommand, name = "list")]
struct List {}

/// run this binary as a launchd agent, so macOS permissions survive an upgrade
#[derive(FromArgs, Debug, PartialEq, Eq)]
#[argh(subcommand, name = "install")]
struct Install {}

/// unload the launchd agent and remove what install wrote
#[derive(FromArgs, Debug, PartialEq, Eq)]
#[argh(subcommand, name = "uninstall")]
struct Uninstall {}

fn main() -> ExitCode {
    tracing_subscriber::fmt().with_target(false).init();

    let Args {
        version,
        check_config,
        command,
    } = argh::from_env();

    if version {
        println!("moji {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    if check_config {
        return check();
    }

    let Some(command) = command else {
        tracing::error!("moji needs one of these subcommands: {SUBCOMMANDS}");
        return ExitCode::FAILURE;
    };

    match command {
        Command::Run(Run {}) => run(),
        Command::Set(Set { tag }) => set(&LayoutTag(tag)),
        Command::Toggle(Toggle {}) => toggle(),
        Command::Status(Status {}) => status(),
        Command::List(List {}) => list(),
        Command::Install(Install {}) => not_implemented("install"),
        Command::Uninstall(Uninstall {}) => not_implemented("uninstall"),
    }
}

fn run() -> ExitCode {
    let missing = tap::request_missing_access();
    if !missing.is_empty() {
        for access in missing {
            tracing::error!(
                pane = access.pane(),
                "moji cannot run without this grant; launchd will start it again once it is given"
            );
        }
        return ExitCode::FAILURE;
    }

    let Some(config) = loaded() else {
        return ExitCode::FAILURE;
    };
    let Some(resolved) = resolved(&config) else {
        return ExitCode::FAILURE;
    };
    let Config {
        cycle,
        layouts,
        apps,
    } = config;

    let daemon = match Daemon::start(cycle, resolved, apps, daemon::HOLD) {
        Ok(daemon) => daemon,
        Err(error) => {
            tracing::error!(%error, "moji could not install its run loop sources");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        layouts = described(&layouts),
        "moji is running"
    );

    match daemon.run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "moji stopped because its run loop could not be watched");
            ExitCode::FAILURE
        }
    }
}

fn list() -> ExitCode {
    let layouts = tis::enabled_layouts();
    if layouts.is_empty() {
        tracing::error!("Text Input Sources reports no enabled keyboard layout");
        return ExitCode::FAILURE;
    }

    for layout in layouts {
        let Layout { name, id, language } = layout;
        let language = language.unwrap_or_else(|| "-".to_string());
        println!("{name}\t{id}\t{language}");
    }
    ExitCode::SUCCESS
}

fn status() -> ExitCode {
    let Some(layout) = tis::current() else {
        tracing::error!("Text Input Sources reports no selected keyboard layout");
        return ExitCode::FAILURE;
    };

    let Layout { name, id, language } = layout;
    let language = language.unwrap_or_else(|| "-".to_string());
    let tag = match configured_tag(&name) {
        Some(tag) => tag.to_string(),
        None => "-".to_string(),
    };
    let frontmost = match workspace::frontmost() {
        Some(frontmost) => frontmost.to_string(),
        None => "-".to_string(),
    };

    println!("layout\t{name}");
    println!("id\t{id}");
    println!("language\t{language}");
    println!("tag\t{tag}");
    println!("frontmost\t{frontmost}");
    ExitCode::SUCCESS
}

fn set(tag: &LayoutTag) -> ExitCode {
    let Some(config) = loaded() else {
        return ExitCode::FAILURE;
    };
    let Some(resolved) = resolved(&config) else {
        return ExitCode::FAILURE;
    };

    let Some(layout) = resolved.get(tag) else {
        tracing::error!(%tag, "the configuration carries no layout under this tag");
        return ExitCode::FAILURE;
    };
    select(layout)
}

fn toggle() -> ExitCode {
    let Some(config) = loaded() else {
        return ExitCode::FAILURE;
    };
    let Some(resolved) = resolved(&config) else {
        return ExitCode::FAILURE;
    };
    let Config {
        cycle,
        layouts: _,
        apps: _,
    } = &config;

    let current = current_tag(&resolved);
    let Some(next) = barrier::next(cycle, current.as_ref()) else {
        tracing::error!("the configured cycle is empty, so there is nothing to toggle to");
        return ExitCode::FAILURE;
    };
    let Some(layout) = resolved.get(&next) else {
        tracing::error!(tag = %next, "the configuration carries no layout under this tag");
        return ExitCode::FAILURE;
    };
    select(layout)
}

fn check() -> ExitCode {
    let Some(config) = loaded() else {
        return ExitCode::FAILURE;
    };
    let Some(resolved) = resolved(&config) else {
        return ExitCode::FAILURE;
    };
    let Config {
        cycle,
        layouts: _,
        apps,
    } = &config;

    let mut order = String::new();
    for tag in cycle {
        if !order.is_empty() {
            order.push_str(" -> ");
        }
        order.push_str(&tag.to_string());
    }
    println!("cycle\t{order}");

    for (tag, layout) in &resolved {
        let Layout {
            name,
            id: _,
            language: _,
        } = layout;
        println!("{tag}\t{name}");
    }
    for (bundle, tag) in apps {
        println!("{bundle}\t{tag}");
    }
    ExitCode::SUCCESS
}

fn not_implemented(command: &str) -> ExitCode {
    tracing::error!(command, "not implemented yet");
    ExitCode::FAILURE
}

fn select(layout: &Layout) -> ExitCode {
    let Err(error) = tis::select(layout) else {
        return ExitCode::SUCCESS;
    };
    tracing::error!(%error, "the layout could not be selected");
    ExitCode::FAILURE
}

fn loaded() -> Option<Config> {
    match config::load() {
        Ok(config) => Some(config),
        Err(error) => {
            tracing::error!(%error, "the configuration could not be loaded");
            None
        }
    }
}

fn resolved(config: &Config) -> Option<BTreeMap<LayoutTag, Layout>> {
    let Config {
        cycle: _,
        layouts,
        apps: _,
    } = config;
    match tis::resolve(layouts, &tis::enabled_layouts()) {
        Ok(resolved) => Some(resolved),
        Err(error) => {
            tracing::error!(%error, "the configured layouts are not all enabled");
            None
        }
    }
}

fn current_tag(layouts: &BTreeMap<LayoutTag, Layout>) -> Option<LayoutTag> {
    let Layout {
        name,
        id: _,
        language: _,
    } = tis::current()?;

    for (tag, layout) in layouts {
        let Layout {
            name: candidate,
            id: _,
            language: _,
        } = layout;
        if *candidate == name {
            return Some(tag.clone());
        }
    }
    None
}

fn configured_tag(name: &str) -> Option<LayoutTag> {
    let Ok(config) = config::load() else {
        return None;
    };
    let Config {
        cycle: _,
        layouts,
        apps: _,
    } = config;

    for (tag, candidate) in layouts {
        if candidate == name {
            return Some(tag);
        }
    }
    None
}

fn described(layouts: &BTreeMap<LayoutTag, String>) -> String {
    let mut described = String::new();
    for (tag, name) in layouts {
        if !described.is_empty() {
            described.push_str(", ");
        }
        described.push_str(&format!("{tag} = {name}"));
    }
    described
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, argh::EarlyExit> {
        Args::from_args(&["moji"], args)
    }

    fn parsed(args: &[&str]) -> Args {
        let Ok(args) = parse(args) else {
            panic!("{args:?} does not parse");
        };
        args
    }

    #[test]
    fn every_subcommand_parses() {
        assert_eq!(
            parsed(&["run"]),
            Args {
                version: false,
                check_config: false,
                command: Some(Command::Run(Run {})),
            }
        );
        assert_eq!(
            parsed(&["toggle"]).command,
            Some(Command::Toggle(Toggle {}))
        );
        assert_eq!(
            parsed(&["status"]).command,
            Some(Command::Status(Status {}))
        );
        assert_eq!(parsed(&["list"]).command, Some(Command::List(List {})));
        assert_eq!(
            parsed(&["install"]).command,
            Some(Command::Install(Install {}))
        );
        assert_eq!(
            parsed(&["uninstall"]).command,
            Some(Command::Uninstall(Uninstall {}))
        );
    }

    #[test]
    fn set_carries_the_tag() {
        assert_eq!(
            parsed(&["set", "ru"]).command,
            Some(Command::Set(Set {
                tag: "ru".to_string()
            }))
        );
    }

    #[test]
    fn set_without_a_tag_is_an_error() {
        assert!(parse(&["set"]).is_err());
    }

    #[test]
    fn the_version_switch_parses_long_and_short() {
        assert!(parsed(&["--version"]).version);
        assert!(parsed(&["-V"]).version);
    }

    #[test]
    fn the_check_config_switch_parses() {
        let Args {
            version,
            check_config,
            command,
        } = parsed(&["--check-config"]);
        assert!(!version);
        assert!(check_config);
        assert_eq!(command, None);
    }

    #[test]
    fn no_argument_at_all_parses_to_no_command() {
        assert_eq!(parsed(&[]).command, None);
    }

    #[test]
    fn an_unknown_subcommand_is_an_error() {
        assert!(parse(&["switcheroo"]).is_err());
    }

    #[test]
    fn an_unknown_switch_is_an_error() {
        assert!(parse(&["--reticulate"]).is_err());
    }
}
