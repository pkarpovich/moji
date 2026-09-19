//! The launchd agent `moji install` writes, and the paths it lives at.
//!
//! macOS keys a permission grant to the program launchd starts, so the agent names the running
//! binary with its symlinks resolved: inside `Moji.app` that is the bundle path both TCC grants
//! belong to, while a loose binary is identified by a path that moves with every version.
//! `KeepAlive` is a `PathState` on that same program, so launchd does not restart the daemon while
//! an upgrade is still swapping the bundle underneath it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use crate::macos::user;

const LAUNCHCTL: &str = "/bin/launchctl";
const UNLOAD_TIMEOUT: Duration = Duration::from_secs(5);
const UNLOAD_POLL: Duration = Duration::from_millis(100);

/// The launchd label of the agent `moji install` writes.
pub const LABEL: &str = "dev.pkarpovich.moji";

/// What can go wrong while loading or unloading the agent.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// `HOME` is not set, so neither the agent nor the logs have a place to live.
    #[error("HOME is not set, so the service paths cannot be resolved")]
    NoHome,
    /// The running binary could not be located, so the agent has no program to name.
    #[error("the running binary could not be located: {source}")]
    Executable {
        /// Why `current_exe` or its canonicalization failed.
        source: std::io::Error,
    },
    /// A directory the agent or its logs need could not be created.
    #[error("{} could not be created: {source}", path.display())]
    Directory {
        /// The directory that was refused.
        path: PathBuf,
        /// Why the file system refused it.
        source: std::io::Error,
    },
    /// The agent file could not be written.
    #[error("{} could not be written: {source}", path.display())]
    Write {
        /// The file that was refused.
        path: PathBuf,
        /// Why the file system refused it.
        source: std::io::Error,
    },
    /// The agent file could not be removed.
    #[error("{} could not be removed: {source}", path.display())]
    Remove {
        /// The file that could not be removed.
        path: PathBuf,
        /// Why the file system refused it.
        source: std::io::Error,
    },
    /// `launchctl` itself could not be run.
    #[error("launchctl could not be run: {source}")]
    Launchctl {
        /// Why the process could not be spawned.
        source: std::io::Error,
    },
    /// `launchctl` ran and refused to load the agent.
    #[error("launchctl refused to load {}", path.display())]
    Bootstrap {
        /// The agent it refused.
        path: PathBuf,
    },
    /// `launchctl` ran and the job is still loaded after the wait.
    #[error("{label} is still loaded, so launchctl refused to unload it")]
    Bootout {
        /// The label that would not go away.
        label: String,
    },
}

/// Whether the running binary sits inside an application bundle.
///
/// It decides whether the Input Monitoring and Accessibility grants survive an upgrade: a bundle
/// is identified by its bundle id at a path that does not move, while a loose binary is identified
/// by its path alone, and every new version gets a path of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Housing {
    /// The binary is inside a `.app`, so the grants follow the bundle id.
    Bundle,
    /// The binary is loose, so the grants follow a path that changes with every version.
    Loose,
}

/// Where the agent and its logs live under a home directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The agent file launchd reads.
    pub agent: PathBuf,
    /// Where the daemon's standard output is captured.
    pub log: PathBuf,
    /// Where the daemon's standard error is captured.
    pub errors: PathBuf,
}

/// What [`install`] loaded, for the caller to report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// The program the agent names.
    pub program: PathBuf,
    /// The agent file that was written.
    pub agent: PathBuf,
    /// Whether that program is inside an application bundle.
    pub housing: Housing,
}

/// Resolves the agent layout under a home directory.
pub fn layout(home: &Path) -> Layout {
    let agents = home.join("Library").join("LaunchAgents");
    let logs = home.join("Library").join("Logs").join("moji");
    Layout {
        agent: agents.join(format!("{LABEL}.plist")),
        log: logs.join("moji.log"),
        errors: logs.join("moji.err.log"),
    }
}

/// Loads the running binary as a launchd agent, replacing whatever was loaded under the label.
///
/// # Errors
///
/// Returns [`ServiceError::NoHome`] when `HOME` is not set, [`ServiceError::Executable`] when the
/// running binary cannot be located, [`ServiceError::Directory`] or [`ServiceError::Write`] when
/// the agent cannot be written, [`ServiceError::Bootout`] when a job already loaded under the
/// label will not go away, and [`ServiceError::Launchctl`] or [`ServiceError::Bootstrap`] when
/// launchd refuses to load it.
pub fn install() -> Result<Installed, ServiceError> {
    let home = home_dir()?;
    let layout = layout(&home);
    let program = executable()?;

    unload(LABEL)?;
    let Unloaded::Yes = wait_unloaded(LABEL)? else {
        return Err(ServiceError::Bootout {
            label: LABEL.to_string(),
        });
    };

    let Layout { agent, log, errors } = &layout;
    create_dir(parent_of(agent))?;
    create_dir(parent_of(log))?;

    let contents = agent_plist(&program, log, errors);
    std::fs::write(agent, contents).map_err(|source| ServiceError::Write {
        path: agent.clone(),
        source,
    })?;

    bootstrap(agent)?;
    Ok(Installed {
        program: program.clone(),
        agent: agent.clone(),
        housing: housing(&program),
    })
}

/// Unloads the agent and removes it, leaving the binary and the logs alone.
///
/// # Errors
///
/// Returns [`ServiceError::NoHome`] when `HOME` is not set, [`ServiceError::Launchctl`] when
/// launchctl cannot be run, [`ServiceError::Bootout`] when the job is still loaded after the
/// wait - the agent is left in place then, so a retry still has something to unload - and
/// [`ServiceError::Remove`] when the agent file cannot be removed.
pub fn uninstall() -> Result<Layout, ServiceError> {
    let home = home_dir()?;
    let layout = layout(&home);

    unload(LABEL)?;
    let Unloaded::Yes = wait_unloaded(LABEL)? else {
        return Err(ServiceError::Bootout {
            label: LABEL.to_string(),
        });
    };

    let Layout {
        agent,
        log: _,
        errors: _,
    } = &layout;
    remove_file(agent)?;
    Ok(layout)
}

fn executable() -> Result<PathBuf, ServiceError> {
    let program = std::env::current_exe().map_err(|source| ServiceError::Executable { source })?;
    std::fs::canonicalize(program).map_err(|source| ServiceError::Executable { source })
}

enum Unloaded {
    Yes,
    No,
}

fn wait_unloaded(label: &str) -> Result<Unloaded, ServiceError> {
    let target = format!("gui/{}/{label}", user::uid());
    let deadline = Instant::now() + UNLOAD_TIMEOUT;
    while Instant::now() < deadline {
        let status = Command::new(LAUNCHCTL)
            .args(["print", &target])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|source| ServiceError::Launchctl { source })?;
        if !status.success() {
            return Ok(Unloaded::Yes);
        }
        thread::sleep(UNLOAD_POLL);
    }
    Ok(Unloaded::No)
}

fn housing(program: &Path) -> Housing {
    for component in program.ancestors() {
        let Some(name) = component.file_name() else {
            continue;
        };
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.ends_with(".app") {
            return Housing::Bundle;
        }
    }
    Housing::Loose
}

fn home_dir() -> Result<PathBuf, ServiceError> {
    let Some(home) = std::env::var_os("HOME") else {
        return Err(ServiceError::NoHome);
    };
    Ok(PathBuf::from(home))
}

fn parent_of(path: &Path) -> &Path {
    let Some(parent) = path.parent() else {
        return path;
    };
    parent
}

fn create_dir(path: &Path) -> Result<(), ServiceError> {
    std::fs::create_dir_all(path).map_err(|source| ServiceError::Directory {
        path: path.to_path_buf(),
        source,
    })
}

fn remove_file(path: &Path) -> Result<(), ServiceError> {
    let removed = std::fs::remove_file(path);
    let Err(source) = removed else {
        return Ok(());
    };
    if source.kind() == std::io::ErrorKind::NotFound {
        return Ok(());
    }
    Err(ServiceError::Remove {
        path: path.to_path_buf(),
        source,
    })
}

fn unload(label: &str) -> Result<(), ServiceError> {
    let target = format!("gui/{}/{label}", user::uid());
    let status = Command::new(LAUNCHCTL)
        .args(["bootout", &target])
        .status()
        .map_err(|source| ServiceError::Launchctl { source })?;
    tracing::debug!(label, code = status.code(), "asked launchctl to unload");
    Ok(())
}

fn bootstrap(agent: &Path) -> Result<(), ServiceError> {
    let target = format!("gui/{}", user::uid());
    let Some(agent_arg) = agent.to_str() else {
        return Err(ServiceError::Bootstrap {
            path: agent.to_path_buf(),
        });
    };
    let status = Command::new(LAUNCHCTL)
        .args(["bootstrap", &target, agent_arg])
        .status()
        .map_err(|source| ServiceError::Launchctl { source })?;
    if status.success() {
        return Ok(());
    }
    Err(ServiceError::Bootstrap {
        path: agent.to_path_buf(),
    })
}

fn agent_plist(program: &Path, log: &Path, errors: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{label}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{program}</string>
		<string>run</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>PathState</key>
		<dict>
			<key>{program}</key>
			<true/>
		</dict>
	</dict>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{errors}</string>
</dict>
</plist>
"#,
        label = LABEL,
        program = escape(&program.display().to_string()),
        log = escape(&log.display().to_string()),
        errors = escape(&errors.display().to_string()),
    )
}

fn escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundled() -> String {
        agent_plist(
            Path::new("/Applications/Moji.app/Contents/MacOS/moji"),
            Path::new("/Users/tester/Library/Logs/moji/moji.log"),
            Path::new("/Users/tester/Library/Logs/moji/moji.err.log"),
        )
    }

    #[test]
    fn the_agent_and_its_logs_live_under_the_home_directory() {
        let Layout { agent, log, errors } = layout(Path::new("/Users/tester"));

        assert_eq!(
            agent,
            Path::new("/Users/tester/Library/LaunchAgents/dev.pkarpovich.moji.plist")
        );
        assert_eq!(log, Path::new("/Users/tester/Library/Logs/moji/moji.log"));
        assert_eq!(
            errors,
            Path::new("/Users/tester/Library/Logs/moji/moji.err.log")
        );
    }

    #[test]
    fn the_agent_runs_the_daemon_it_was_given() {
        let plist = bundled();

        assert!(plist.contains("<string>dev.pkarpovich.moji</string>"));
        assert!(plist.contains("<string>/Applications/Moji.app/Contents/MacOS/moji</string>"));
        assert!(
            plist.contains("<string>run</string>"),
            "the agent has to start the daemon, not the bare binary"
        );
        assert!(plist.contains("<string>/Users/tester/Library/Logs/moji/moji.log</string>"));
        assert!(plist.contains("<string>/Users/tester/Library/Logs/moji/moji.err.log</string>"));
    }

    #[test]
    fn the_agent_lives_only_while_the_binary_it_names_is_there() {
        let plist = bundled();

        assert!(plist.contains("<key>PathState</key>"));
        assert!(
            plist.contains("<key>/Applications/Moji.app/Contents/MacOS/moji</key>\n\t\t\t<true/>")
        );
        assert!(
            !plist.contains("<key>KeepAlive</key>\n\t<true/>"),
            "a plain KeepAlive restarts the daemon while the upgrade is still swapping the bundle"
        );
    }

    #[test]
    fn a_path_that_needs_escaping_still_yields_a_parsable_agent() {
        let plist = agent_plist(
            Path::new("/Users/a&b/Moji.app/Contents/MacOS/moji"),
            Path::new("/Users/a&b/log"),
            Path::new("/Users/a&b/err"),
        );

        assert!(plist.contains("/Users/a&amp;b/Moji.app/Contents/MacOS/moji"));
        assert!(!plist.contains("/Users/a&b/"));
    }

    #[test]
    fn escaping_leaves_an_ordinary_path_untouched() {
        assert_eq!(escape("/Applications/Moji.app"), "/Applications/Moji.app");
        assert_eq!(escape("a<b>c&d"), "a&lt;b&gt;c&amp;d");
    }

    #[test]
    fn a_binary_inside_an_application_bundle_keeps_its_permissions() {
        assert_eq!(
            housing(Path::new("/Applications/Moji.app/Contents/MacOS/moji")),
            Housing::Bundle
        );
    }

    #[test]
    fn a_binary_in_the_homebrew_cellar_does_not() {
        assert_eq!(
            housing(Path::new("/opt/homebrew/Cellar/moji/0.1.0/bin/moji")),
            Housing::Loose
        );
        assert_eq!(housing(Path::new("target/debug/moji")), Housing::Loose);
    }

    #[test]
    fn a_directory_merely_containing_app_is_not_a_bundle() {
        assert_eq!(
            housing(Path::new("/Users/tester/apps/moji/bin/moji")),
            Housing::Loose
        );
    }

    #[test]
    fn removing_a_file_that_was_never_there_is_not_a_failure() {
        let absent = std::env::temp_dir().join("moji-service-test-absent");

        assert!(remove_file(&absent).is_ok());
    }
}
