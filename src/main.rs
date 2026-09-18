mod macos;

use std::process::ExitCode;

use argh::FromArgs;

use crate::macos::tis::{self, Layout};

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
        return not_implemented("--check-config");
    }

    let Some(command) = command else {
        tracing::error!("moji needs one of these subcommands: {SUBCOMMANDS}");
        return ExitCode::FAILURE;
    };

    match command {
        Command::Run(Run {}) => not_implemented("run"),
        Command::Set(Set { tag }) => not_implemented(&format!("set {tag}")),
        Command::Toggle(Toggle {}) => not_implemented("toggle"),
        Command::Status(Status {}) => status(),
        Command::List(List {}) => list(),
        Command::Install(Install {}) => not_implemented("install"),
        Command::Uninstall(Uninstall {}) => not_implemented("uninstall"),
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
    println!("layout\t{name}");
    println!("id\t{id}");
    println!("language\t{language}");
    ExitCode::SUCCESS
}

fn not_implemented(command: &str) -> ExitCode {
    tracing::error!(command, "not implemented yet");
    ExitCode::FAILURE
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
