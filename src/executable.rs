//! The binary this process runs from, watched so an upgrade replaces the daemon instead of
//! outliving it.
//!
//! launchd keeps the agent alive on the path state of the program it started, so a daemon that
//! stops once its binary was replaced is started again from the new one. Nothing here is async:
//! v1 has no runtime, and the daemon polls [`Executable::swapped`] from a run loop timer, which is
//! the only clock it has.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How often the running binary is checked for having been replaced under the daemon.
pub const POLL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}

/// The binary this process is running from, as it was found at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Executable {
    path: PathBuf,
    identity: Identity,
}

impl Executable {
    /// Reads the running binary, symlinks resolved, or nothing when it cannot be located.
    pub fn current() -> Option<Executable> {
        let path = std::env::current_exe().ok()?;
        let path = fs::canonicalize(path).ok()?;
        let identity = identity(&path)?;
        Some(Executable { path, identity })
    }

    /// Returns the path the binary was found at, which is the one the agent names.
    pub fn path(&self) -> &Path {
        let Executable { path, identity: _ } = self;
        path
    }

    /// Returns whether the file at that path is no longer the one this process started from.
    ///
    /// A removed binary counts as replaced: an upgrade that unpacks a new bundle is a new inode at
    /// the same path, and a moment where nothing is there at all is the same upgrade seen earlier.
    pub fn swapped(&self) -> bool {
        let Executable { path, identity } = self;
        replaced(*identity, self::identity(path))
    }
}

fn identity(path: &Path) -> Option<Identity> {
    let found = fs::symlink_metadata(path).ok()?;
    Some(Identity {
        device: found.dev(),
        inode: found.ino(),
    })
}

fn replaced(original: Identity, current: Option<Identity>) -> bool {
    current != Some(original)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    static NEXT_DIR: AtomicU32 = AtomicU32::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> TempDir {
            let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("moji-executable-test-{}-{id}", std::process::id()));
            fs::create_dir_all(&path).expect("create the temporary directory");
            TempDir(path)
        }

        fn binary(&self) -> PathBuf {
            let TempDir(path) = self;
            let binary = path.join("moji");
            fs::write(&binary, b"build").expect("write the binary");
            binary
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let TempDir(path) = self;
            let _ = fs::remove_dir_all(path);
        }
    }

    fn swap_in_a_new_file(path: &Path) {
        let staged = path.with_extension("new");
        fs::write(&staged, b"new build").expect("write the replacement");
        fs::rename(&staged, path).expect("rename it over the original");
    }

    fn watched(path: &Path) -> Executable {
        Executable {
            path: path.to_path_buf(),
            identity: identity(path).expect("the original identity"),
        }
    }

    #[test]
    fn a_file_has_an_identity_and_a_missing_one_has_none() {
        let directory = TempDir::new();
        let binary = directory.binary();

        assert!(identity(&binary).is_some());
        assert_eq!(identity(&binary.with_file_name("absent")), None);
    }

    #[test]
    fn an_untouched_binary_is_the_one_the_daemon_started_from() {
        let directory = TempDir::new();
        let binary = directory.binary();
        let executable = watched(&binary);

        assert!(!executable.swapped(), "an untouched binary is left alone");
    }

    #[test]
    fn the_watch_reports_a_binary_swapped_at_the_same_path() {
        let directory = TempDir::new();
        let binary = directory.binary();
        let executable = watched(&binary);

        swap_in_a_new_file(&binary);

        assert!(
            executable.swapped(),
            "the way an upgrade swaps a bundle is a new inode at the same path"
        );
    }

    #[test]
    fn a_removed_binary_counts_as_swapped() {
        let directory = TempDir::new();
        let binary = directory.binary();
        let executable = watched(&binary);

        fs::remove_file(&binary).expect("remove the binary");

        assert!(executable.swapped());
    }

    #[test]
    fn rewriting_the_same_file_in_place_is_not_a_swap() {
        let directory = TempDir::new();
        let binary = directory.binary();
        let executable = watched(&binary);

        fs::write(&binary, b"same inode, new bytes").expect("rewrite the binary");

        assert!(!executable.swapped());
    }

    #[test]
    fn the_running_binary_can_be_located_and_is_not_swapped() {
        let Some(executable) = Executable::current() else {
            panic!("the test binary could not locate itself");
        };

        assert!(executable.path().is_absolute());
        assert!(!executable.swapped());
    }
}
