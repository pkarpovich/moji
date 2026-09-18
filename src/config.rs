//! The TOML file that names the layouts, the toggle order and the per-application pins.
//!
//! Nothing here talks to Text Input Sources: the file carries localized layout names as plain
//! strings, and matching them against the enabled sources is `tis::resolve`. A tag that the
//! `[layouts]` table does not carry is rejected here, so the barrier never names a layout the
//! daemon cannot select.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::macos::tis::LayoutTag;
use crate::macos::workspace::BundleId;

const CONFIG_VAR: &str = "MOJI_CONFIG";
const DEFAULT_CONFIG_RELATIVE: &str = ".config/moji/config.toml";
const MINIMUM_CYCLE: usize = 2;

/// Everything moji reads out of the configuration file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The order the switch key and `moji toggle` walk, as tags.
    pub cycle: Vec<LayoutTag>,
    /// Every tag the configuration knows, with the localized layout name it stands for.
    pub layouts: BTreeMap<LayoutTag, String>,
    /// The layout pinned to a bundle id; every other application is remembered instead.
    pub apps: BTreeMap<BundleId, LayoutTag>,
}

/// What can go wrong between the configuration file and a usable [`Config`].
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Neither the override nor `HOME` says where the file is.
    #[error("neither {CONFIG_VAR} nor HOME is set, so the configuration file cannot be found")]
    NoHome,
    /// The file could not be read.
    #[error("the configuration file {} could not be read: {source}", path.display())]
    Read {
        /// The path that was tried.
        path: PathBuf,
        /// Why reading it failed.
        source: io::Error,
    },
    /// The file is not valid TOML, or does not carry the fields moji needs.
    #[error("the configuration file {} is not valid: {source}", path.display())]
    Parse {
        /// The file that was parsed.
        path: PathBuf,
        /// What toml made of it.
        source: toml::de::Error,
    },
    /// A tag is used somewhere without `[layouts]` giving it a layout.
    #[error("{field} names the layout tag {tag}, which [layouts] does not carry; it carries {}", describe(.known))]
    UnknownTag {
        /// Where the tag was used, as a path into the file.
        field: String,
        /// The tag that names no layout.
        tag: LayoutTag,
        /// Every tag `[layouts]` does carry, so the file can be fixed.
        known: Vec<LayoutTag>,
    },
    /// The cycle is too short to toggle along.
    #[error(
        "cycle carries {length} entries, and toggling needs at least {MINIMUM_CYCLE}: selecting the layout that is already selected is confirmed by no notification"
    )]
    ShortCycle {
        /// How many entries the cycle carries.
        length: usize,
    },
}

/// Returns the path the configuration is read from.
///
/// # Errors
///
/// Returns [`ConfigError::NoHome`] when `MOJI_CONFIG` is unset and `HOME` is too.
pub fn path() -> Result<PathBuf, ConfigError> {
    let configured = env::var(CONFIG_VAR).ok();
    let home = env::var("HOME").ok();
    resolve(configured, home.map(PathBuf::from))
}

/// Loads the configuration from the path `MOJI_CONFIG` names, or from the default one.
///
/// # Errors
///
/// Returns whatever [`path`] and [`load_from`] report.
pub fn load() -> Result<Config, ConfigError> {
    let path = path()?;
    load_from(&path)
}

/// Loads the configuration from `path`.
///
/// # Errors
///
/// Returns [`ConfigError::Read`] when the file cannot be read, [`ConfigError::Parse`] when it is
/// not the TOML moji expects, and [`ConfigError::UnknownTag`] or [`ConfigError::ShortCycle`] when
/// it parses but says something moji cannot act on.
pub fn load_from(path: &Path) -> Result<Config, ConfigError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(source) => {
            return Err(ConfigError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    parse(&text, path)
}

fn resolve(configured: Option<String>, home: Option<PathBuf>) -> Result<PathBuf, ConfigError> {
    let Some(configured) = configured else {
        let Some(home) = home else {
            return Err(ConfigError::NoHome);
        };
        return Ok(home.join(DEFAULT_CONFIG_RELATIVE));
    };
    Ok(PathBuf::from(configured))
}

fn parse(text: &str, path: &Path) -> Result<Config, ConfigError> {
    let file: FileConfig = match toml::from_str(text) {
        Ok(file) => file,
        Err(source) => {
            return Err(ConfigError::Parse {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let FileConfig {
        cycle,
        layouts,
        apps,
    } = file;

    let mut named = BTreeMap::new();
    for (tag, name) in layouts {
        named.insert(LayoutTag(tag), name);
    }

    let mut order = Vec::new();
    for (index, tag) in cycle.into_iter().enumerate() {
        let tag = LayoutTag(tag);
        if !named.contains_key(&tag) {
            return Err(ConfigError::UnknownTag {
                field: format!("cycle[{index}]"),
                tag,
                known: every_tag(&named),
            });
        }
        order.push(tag);
    }
    if order.len() < MINIMUM_CYCLE {
        return Err(ConfigError::ShortCycle {
            length: order.len(),
        });
    }

    let mut pinned = BTreeMap::new();
    for (bundle, tag) in apps {
        let tag = LayoutTag(tag);
        if !named.contains_key(&tag) {
            return Err(ConfigError::UnknownTag {
                field: format!("apps.\"{bundle}\""),
                tag,
                known: every_tag(&named),
            });
        }
        pinned.insert(BundleId(bundle), tag);
    }

    Ok(Config {
        cycle: order,
        layouts: named,
        apps: pinned,
    })
}

fn every_tag(layouts: &BTreeMap<LayoutTag, String>) -> Vec<LayoutTag> {
    let mut tags = Vec::new();
    for tag in layouts.keys() {
        tags.push(tag.clone());
    }
    tags
}

fn describe(tags: &[LayoutTag]) -> String {
    let mut listed = String::new();
    for tag in tags {
        if !listed.is_empty() {
            listed.push_str(", ");
        }
        listed.push_str(&tag.to_string());
    }
    if listed.is_empty() {
        return "nothing".to_string();
    }
    listed
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    cycle: Vec<String>,
    layouts: BTreeMap<String, String>,
    #[serde(default)]
    apps: BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
cycle = ["en", "ru"]

[layouts]
en = "English - Universal"
ru = "Russian - Universal"

[apps]
"com.brnbw.Tuna" = "en"
"#;

    fn tag(tag: &str) -> LayoutTag {
        LayoutTag(tag.to_string())
    }

    fn bundle(bundle: &str) -> BundleId {
        BundleId(bundle.to_string())
    }

    fn parsed(text: &str) -> Config {
        let Ok(config) = parse(text, Path::new("config.toml")) else {
            panic!("the configuration does not parse: {text}");
        };
        config
    }

    fn rejected(text: &str) -> ConfigError {
        let Err(error) = parse(text, Path::new("config.toml")) else {
            panic!("the configuration parses when it should not: {text}");
        };
        error
    }

    #[test]
    fn the_sample_configuration_parses() {
        let Config {
            cycle,
            layouts,
            apps,
        } = parsed(SAMPLE);

        assert_eq!(cycle, vec![tag("en"), tag("ru")]);
        assert_eq!(
            layouts.get(&tag("en")),
            Some(&"English - Universal".to_string())
        );
        assert_eq!(
            layouts.get(&tag("ru")),
            Some(&"Russian - Universal".to_string())
        );
        assert_eq!(apps.get(&bundle("com.brnbw.Tuna")), Some(&tag("en")));
    }

    #[test]
    fn a_configuration_without_apps_pins_nothing() {
        let Config {
            cycle: _,
            layouts: _,
            apps,
        } = parsed(
            r#"
cycle = ["en", "ru"]

[layouts]
en = "English - Universal"
ru = "Russian - Universal"
"#,
        );

        assert!(apps.is_empty());
    }

    #[test]
    fn an_app_pinned_to_a_tag_no_layout_carries_is_rejected() {
        let error = rejected(
            r#"
cycle = ["en", "ru"]

[layouts]
en = "English - Universal"
ru = "Russian - Universal"

[apps]
"com.brnbw.Tuna" = "de"
"#,
        );

        match error {
            ConfigError::UnknownTag { field, tag, known } => {
                assert_eq!(field, "apps.\"com.brnbw.Tuna\"");
                assert_eq!(tag, LayoutTag("de".to_string()));
                assert_eq!(
                    known,
                    vec![LayoutTag("en".to_string()), LayoutTag("ru".to_string())]
                );
            }
            ConfigError::NoHome
            | ConfigError::Read { .. }
            | ConfigError::Parse { .. }
            | ConfigError::ShortCycle { .. } => panic!("the wrong error: {error}"),
        }

        let reported = rejected(
            r#"
cycle = ["en", "ru"]

[layouts]
en = "English - Universal"
ru = "Russian - Universal"

[apps]
"com.brnbw.Tuna" = "de"
"#,
        )
        .to_string();
        assert!(reported.contains("com.brnbw.Tuna"), "{reported}");
        assert!(reported.contains("de"), "{reported}");
        assert!(reported.contains("en, ru"), "{reported}");
    }

    #[test]
    fn a_cycle_entry_no_layout_carries_is_rejected() {
        let error = rejected(
            r#"
cycle = ["en", "de"]

[layouts]
en = "English - Universal"
ru = "Russian - Universal"
"#,
        );

        match error {
            ConfigError::UnknownTag {
                field,
                tag,
                known: _,
            } => {
                assert_eq!(field, "cycle[1]");
                assert_eq!(tag, LayoutTag("de".to_string()));
            }
            ConfigError::NoHome
            | ConfigError::Read { .. }
            | ConfigError::Parse { .. }
            | ConfigError::ShortCycle { .. } => panic!("the wrong error: {error}"),
        }
    }

    #[test]
    fn an_unknown_field_is_rejected() {
        let error = rejected(
            r#"
cycle = ["en", "ru"]
hold_ms = 80

[layouts]
en = "English - Universal"
ru = "Russian - Universal"
"#,
        );

        let reported = error.to_string();
        match error {
            ConfigError::Parse { path, source: _ } => {
                assert_eq!(path, PathBuf::from("config.toml"));
            }
            ConfigError::NoHome
            | ConfigError::Read { .. }
            | ConfigError::UnknownTag { .. }
            | ConfigError::ShortCycle { .. } => panic!("the wrong error: {reported}"),
        }
        assert!(reported.contains("hold_ms"), "{reported}");
    }

    #[test]
    fn a_cycle_of_one_entry_is_rejected() {
        let error = rejected(
            r#"
cycle = ["en"]

[layouts]
en = "English - Universal"
ru = "Russian - Universal"
"#,
        );

        match error {
            ConfigError::ShortCycle { length } => assert_eq!(length, 1),
            ConfigError::NoHome
            | ConfigError::Read { .. }
            | ConfigError::Parse { .. }
            | ConfigError::UnknownTag { .. } => panic!("the wrong error: {error}"),
        }
    }

    #[test]
    fn an_empty_cycle_is_rejected() {
        let error = rejected(
            r#"
cycle = []

[layouts]
en = "English - Universal"
ru = "Russian - Universal"
"#,
        );

        match error {
            ConfigError::ShortCycle { length } => assert_eq!(length, 0),
            ConfigError::NoHome
            | ConfigError::Read { .. }
            | ConfigError::Parse { .. }
            | ConfigError::UnknownTag { .. } => panic!("the wrong error: {error}"),
        }
    }

    #[test]
    fn a_file_without_layouts_is_rejected() {
        let error = rejected("cycle = [\"en\", \"ru\"]\n");

        match error {
            ConfigError::Parse { path: _, source: _ } => {}
            ConfigError::NoHome
            | ConfigError::Read { .. }
            | ConfigError::UnknownTag { .. }
            | ConfigError::ShortCycle { .. } => panic!("the wrong error: {error}"),
        }
    }

    #[test]
    fn the_override_wins_over_the_default_path() {
        let Ok(path) = resolve(
            Some("/tmp/moji/acceptance.toml".to_string()),
            Some(PathBuf::from("/Users/someone")),
        ) else {
            panic!("the override does not resolve");
        };

        assert_eq!(path, PathBuf::from("/tmp/moji/acceptance.toml"));
    }

    #[test]
    fn without_the_override_the_path_sits_under_home() {
        let Ok(path) = resolve(None, Some(PathBuf::from("/Users/someone"))) else {
            panic!("the default path does not resolve");
        };

        assert_eq!(
            path,
            PathBuf::from("/Users/someone/.config/moji/config.toml")
        );
    }

    #[test]
    fn without_the_override_and_without_home_there_is_no_path() {
        let Err(error) = resolve(None, None) else {
            panic!("a path resolves out of nothing");
        };

        match error {
            ConfigError::NoHome => {}
            ConfigError::Read { .. }
            | ConfigError::Parse { .. }
            | ConfigError::UnknownTag { .. }
            | ConfigError::ShortCycle { .. } => panic!("the wrong error: {error}"),
        }
    }

    #[test]
    fn a_file_that_is_not_there_is_reported_with_its_path() {
        let Err(error) = load_from(Path::new("/nowhere/moji/config.toml")) else {
            panic!("a configuration loads from a path that does not exist");
        };

        match error {
            ConfigError::Read { path, source: _ } => {
                assert_eq!(path, PathBuf::from("/nowhere/moji/config.toml"));
            }
            ConfigError::NoHome
            | ConfigError::Parse { .. }
            | ConfigError::UnknownTag { .. }
            | ConfigError::ShortCycle { .. } => panic!("the wrong error: {error}"),
        }
    }
}
