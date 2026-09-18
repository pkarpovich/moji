//! Text Input Sources, the only door moji has to the keyboard layout list.
//!
//! No objc2 crate covers HIToolbox, so the handful of functions and property keys moji needs are
//! declared here by hand and linked against the Carbon framework. TIS is documented as
//! main-thread-only, so every call here belongs on the main run loop.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::fmt;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2_core_foundation::{CFArray, CFBoolean, CFRetained, CFString, CFType};
use objc2_foundation::{NSDistributedNotificationCenter, NSNotification, NSString};

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    #[link_name = "TISCreateInputSourceList"]
    fn create_input_source_list(
        properties: *const c_void,
        include_all_installed: u8,
    ) -> *mut CFArray;

    #[link_name = "TISCopyCurrentKeyboardInputSource"]
    fn copy_current_keyboard_input_source() -> *mut CFType;

    #[link_name = "TISSelectInputSource"]
    fn select_input_source(source: NonNull<CFType>) -> i32;

    #[link_name = "TISGetInputSourceProperty"]
    fn input_source_property(source: NonNull<CFType>, key: &CFString) -> *mut c_void;

    #[link_name = "kTISPropertyInputSourceID"]
    static PROPERTY_INPUT_SOURCE_ID: &'static CFString;

    #[link_name = "kTISPropertyLocalizedName"]
    static PROPERTY_LOCALIZED_NAME: &'static CFString;

    #[link_name = "kTISPropertyInputSourceCategory"]
    static PROPERTY_CATEGORY: &'static CFString;

    #[link_name = "kTISPropertyInputSourceIsEnabled"]
    static PROPERTY_IS_ENABLED: &'static CFString;

    #[link_name = "kTISPropertyInputSourceIsSelectCapable"]
    static PROPERTY_IS_SELECT_CAPABLE: &'static CFString;

    #[link_name = "kTISPropertyInputSourceLanguages"]
    static PROPERTY_LANGUAGES: &'static CFString;

    #[link_name = "kTISCategoryKeyboardInputSource"]
    static CATEGORY_KEYBOARD_INPUT_SOURCE: &'static CFString;

    #[link_name = "kTISNotifySelectedKeyboardInputSourceChanged"]
    static NOTIFY_SELECTED_KEYBOARD_INPUT_SOURCE_CHANGED: &'static CFString;
}

/// An enabled, selectable keyboard layout as Text Input Sources describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The localized name, which is what the configuration matches on.
    pub name: String,
    /// The input source ID, printed by `moji list` and never matched on.
    pub id: String,
    /// The first language the source declares, if it declares any.
    pub language: Option<String>,
}

/// The short name the configuration gives a layout.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LayoutTag(pub String);

impl fmt::Display for LayoutTag {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let LayoutTag(tag) = self;
        formatter.write_str(tag)
    }
}

/// What can go wrong when selecting a layout.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TisError {
    /// No enabled keyboard layout carries the requested name.
    #[error("no enabled keyboard layout is named {name}")]
    NotEnabled {
        /// The localized name that was looked for.
        name: String,
    },
    /// Text Input Sources refused the selection.
    #[error("selecting {name} failed with OSStatus {status}")]
    Refused {
        /// The localized name of the layout that was requested.
        name: String,
        /// The `OSStatus` `TISSelectInputSource` returned.
        status: i32,
    },
}

/// What can go wrong when matching configured tags against the enabled layouts.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ResolveError {
    /// At least one tag names a layout that is not enabled.
    #[error("{}", describe_unresolved(.missing, .enabled))]
    Unresolved {
        /// Every tag that found no layout, with the name it asked for.
        missing: Vec<(LayoutTag, String)>,
        /// The localized names that are enabled, so the configuration can be fixed.
        enabled: Vec<String>,
    },
    /// Several tags name the same layout, which would make the cycle ambiguous.
    #[error("{}", describe_duplicate(.name, .tags))]
    Duplicate {
        /// The localized name several tags share.
        name: String,
        /// The tags that share it.
        tags: Vec<LayoutTag>,
    },
}

/// Removes the input source observer from the distributed notification center when dropped.
pub struct ChangeObserver {
    token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

impl Drop for ChangeObserver {
    fn drop(&mut self) {
        let center = NSDistributedNotificationCenter::defaultCenter();
        unsafe { center.removeObserver(self.token.as_ref()) };
    }
}

/// Returns every keyboard layout that is enabled and can be selected.
pub fn enabled_layouts() -> Vec<Layout> {
    let sources = unsafe { create_input_source_list(std::ptr::null(), 0) };
    let Some(sources) = NonNull::new(sources) else {
        return Vec::new();
    };
    let sources = unsafe { CFRetained::from_raw(sources) };

    let mut layouts = Vec::new();
    for index in 0..sources.len() {
        let Some(source) = source_at(&sources, index) else {
            continue;
        };
        if !source.is_selectable_keyboard_layout() {
            continue;
        }
        let Some(layout) = source.layout() else {
            continue;
        };
        layouts.push(layout);
    }
    layouts
}

/// Returns the keyboard layout that is selected right now.
pub fn current() -> Option<Layout> {
    let source = unsafe { copy_current_keyboard_input_source() };
    let source = NonNull::new(source)?;
    let source = InputSource::owned(source);
    source.layout()
}

/// Selects a layout, looking it up by localized name again rather than caching the source.
///
/// # Errors
///
/// Returns [`TisError::NotEnabled`] when no enabled layout carries the name, and
/// [`TisError::Refused`] when `TISSelectInputSource` returns a non-zero `OSStatus`.
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "moji set and moji run select layouts from Task 5 on"
    )
)]
pub fn select(layout: &Layout) -> Result<(), TisError> {
    let Layout {
        name,
        id: _,
        language: _,
    } = layout;
    let Some(source) = source_named(name) else {
        return Err(TisError::NotEnabled { name: name.clone() });
    };
    let status = source.select();
    if status != 0 {
        return Err(TisError::Refused {
            name: name.clone(),
            status,
        });
    }
    Ok(())
}

/// Calls `on_change` on the main run loop whenever the selected keyboard layout changes.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "moji run installs the observer in Task 5")
)]
pub fn observe_changes(on_change: impl Fn() + 'static) -> ChangeObserver {
    let center = NSDistributedNotificationCenter::defaultCenter();
    center.setSuspended(false);

    let name = unsafe { NOTIFY_SELECTED_KEYBOARD_INPUT_SOURCE_CHANGED }.to_string();
    let name = NSString::from_str(&name);
    let block = RcBlock::new(move |_: NonNull<NSNotification>| on_change());
    let token = unsafe {
        center.addObserverForName_object_queue_usingBlock(Some(&name), None, None, &block)
    };
    ChangeObserver { token }
}

/// Matches every configured tag against the enabled layouts by localized name.
///
/// # Errors
///
/// Returns [`ResolveError::Duplicate`] when several tags name one layout, and
/// [`ResolveError::Unresolved`] naming every tag whose layout is not enabled.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "the configuration resolves its tags in Task 6")
)]
pub fn resolve(
    names: &BTreeMap<LayoutTag, String>,
    layouts: &[Layout],
) -> Result<BTreeMap<LayoutTag, Layout>, ResolveError> {
    let mut by_name: BTreeMap<&String, Vec<LayoutTag>> = BTreeMap::new();
    for (tag, name) in names {
        by_name.entry(name).or_default().push(tag.clone());
    }
    for (name, tags) in by_name {
        if tags.len() > 1 {
            return Err(ResolveError::Duplicate {
                name: name.clone(),
                tags,
            });
        }
    }

    let mut resolved = BTreeMap::new();
    let mut missing = Vec::new();
    for (tag, name) in names {
        let Some(layout) = layout_named(layouts, name) else {
            missing.push((tag.clone(), name.clone()));
            continue;
        };
        resolved.insert(tag.clone(), layout);
    }

    if !missing.is_empty() {
        let mut enabled = Vec::new();
        for layout in layouts {
            let Layout {
                name,
                id: _,
                language: _,
            } = layout;
            enabled.push(name.clone());
        }
        return Err(ResolveError::Unresolved { missing, enabled });
    }
    Ok(resolved)
}

fn layout_named(layouts: &[Layout], name: &str) -> Option<Layout> {
    for layout in layouts {
        let Layout {
            name: candidate,
            id: _,
            language: _,
        } = layout;
        if candidate == name {
            return Some(layout.clone());
        }
    }
    None
}

fn describe_unresolved(missing: &[(LayoutTag, String)], enabled: &[String]) -> String {
    let mut asked = String::new();
    for (tag, name) in missing {
        if !asked.is_empty() {
            asked.push_str(", ");
        }
        asked.push_str(&format!("{tag} = {name}"));
    }

    let mut available = String::new();
    for name in enabled {
        if !available.is_empty() {
            available.push_str(", ");
        }
        available.push_str(name);
    }

    format!("no enabled keyboard layout is named {asked}; enabled: {available}")
}

fn describe_duplicate(name: &str, tags: &[LayoutTag]) -> String {
    let mut sharing = String::new();
    for tag in tags {
        if !sharing.is_empty() {
            sharing.push_str(" and ");
        }
        sharing.push_str(&tag.to_string());
    }
    format!("tags {sharing} both name the layout {name}")
}

fn source_named(name: &str) -> Option<InputSource> {
    let sources = unsafe { create_input_source_list(std::ptr::null(), 0) };
    let sources = NonNull::new(sources)?;
    let sources = unsafe { CFRetained::from_raw(sources) };

    for index in 0..sources.len() {
        let Some(source) = source_at(&sources, index) else {
            continue;
        };
        if !source.is_selectable_keyboard_layout() {
            continue;
        }
        let Some(candidate) = source.string(unsafe { PROPERTY_LOCALIZED_NAME }) else {
            continue;
        };
        if candidate == name {
            return Some(source);
        }
    }
    None
}

fn source_at(sources: &CFArray, index: usize) -> Option<InputSource> {
    let source = unsafe { sources.value_at_index(index as isize) };
    let source = NonNull::new(source.cast_mut())?;
    Some(InputSource::borrowed(source.cast::<CFType>()))
}

struct InputSource(CFRetained<CFType>);

impl InputSource {
    fn borrowed(source: NonNull<CFType>) -> Self {
        Self(unsafe { CFRetained::retain(source) })
    }

    fn owned(source: NonNull<CFType>) -> Self {
        Self(unsafe { CFRetained::from_raw(source) })
    }

    fn property(&self, key: &CFString) -> Option<&CFType> {
        let InputSource(source) = self;
        let value = unsafe { input_source_property(CFRetained::as_ptr(source), key) };
        let value = NonNull::new(value)?;
        Some(unsafe { value.cast::<CFType>().as_ref() })
    }

    fn string(&self, key: &CFString) -> Option<String> {
        let value = self.property(key)?;
        let value = value.downcast_ref::<CFString>()?;
        Some(value.to_string())
    }

    fn flag(&self, key: &CFString) -> bool {
        let Some(value) = self.property(key) else {
            return false;
        };
        let Some(value) = value.downcast_ref::<CFBoolean>() else {
            return false;
        };
        value.value()
    }

    fn language(&self) -> Option<String> {
        let languages = self.property(unsafe { PROPERTY_LANGUAGES })?;
        let languages = languages.downcast_ref::<CFArray>()?;
        if languages.is_empty() {
            return None;
        }
        let language = unsafe { languages.value_at_index(0) };
        let language = NonNull::new(language.cast_mut())?;
        let language = unsafe { language.cast::<CFType>().as_ref() };
        let language = language.downcast_ref::<CFString>()?;
        Some(language.to_string())
    }

    fn is_selectable_keyboard_layout(&self) -> bool {
        let Some(category) = self.string(unsafe { PROPERTY_CATEGORY }) else {
            return false;
        };
        if category != unsafe { CATEGORY_KEYBOARD_INPUT_SOURCE }.to_string() {
            return false;
        }
        if !self.flag(unsafe { PROPERTY_IS_ENABLED }) {
            return false;
        }
        self.flag(unsafe { PROPERTY_IS_SELECT_CAPABLE })
    }

    fn layout(&self) -> Option<Layout> {
        let name = self.string(unsafe { PROPERTY_LOCALIZED_NAME })?;
        let id = self.string(unsafe { PROPERTY_INPUT_SOURCE_ID })?;
        Some(Layout {
            name,
            id,
            language: self.language(),
        })
    }

    fn select(&self) -> i32 {
        let InputSource(source) = self;
        unsafe { select_input_source(CFRetained::as_ptr(source)) }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use super::*;

    fn tag(tag: &str) -> LayoutTag {
        LayoutTag(tag.to_string())
    }

    fn layout(name: &str, id: &str, language: &str) -> Layout {
        Layout {
            name: name.to_string(),
            id: id.to_string(),
            language: Some(language.to_string()),
        }
    }

    fn universal() -> Vec<Layout> {
        vec![
            layout(
                "English - Universal",
                "me.tonsky.keyboardlayout.universal.keylayout.English-Universal",
                "en",
            ),
            layout(
                "Russian - Universal",
                "me.tonsky.keyboardlayout.universal.keylayout.Russian-Universal",
                "ru",
            ),
            layout("ABC", "com.apple.keylayout.ABC", "en"),
        ]
    }

    fn names(pairs: &[(&str, &str)]) -> BTreeMap<LayoutTag, String> {
        let mut names = BTreeMap::new();
        for (key, name) in pairs {
            names.insert(tag(key), (*name).to_string());
        }
        names
    }

    #[test]
    fn every_tag_resolves_to_the_layout_it_names() {
        let layouts = universal();
        let names = names(&[("en", "English - Universal"), ("ru", "Russian - Universal")]);

        let Ok(resolved) = resolve(&names, &layouts) else {
            panic!("the sample configuration does not resolve");
        };

        assert_eq!(resolved.len(), 2);
        assert_eq!(
            resolved.get(&tag("en")).map(|layout| layout.id.clone()),
            Some("me.tonsky.keyboardlayout.universal.keylayout.English-Universal".to_string())
        );
        assert_eq!(
            resolved.get(&tag("ru")),
            Some(&layout(
                "Russian - Universal",
                "me.tonsky.keyboardlayout.universal.keylayout.Russian-Universal",
                "ru",
            ))
        );
    }

    #[test]
    fn every_unresolved_tag_is_reported_with_the_enabled_names() {
        let layouts = universal();
        let names = names(&[
            ("de", "German"),
            ("en", "English - Universal"),
            ("fr", "French"),
        ]);

        let Err(error) = resolve(&names, &layouts) else {
            panic!("a configuration naming German and French resolves");
        };

        assert_eq!(
            error,
            ResolveError::Unresolved {
                missing: vec![
                    (tag("de"), "German".to_string()),
                    (tag("fr"), "French".to_string()),
                ],
                enabled: vec![
                    "English - Universal".to_string(),
                    "Russian - Universal".to_string(),
                    "ABC".to_string(),
                ],
            }
        );

        let reported = error.to_string();
        assert!(reported.contains("de = German"), "{reported}");
        assert!(reported.contains("fr = French"), "{reported}");
        assert!(reported.contains("Russian - Universal"), "{reported}");
    }

    #[test]
    fn two_tags_naming_one_layout_are_rejected() {
        let layouts = universal();
        let names = names(&[("en", "English - Universal"), ("us", "English - Universal")]);

        let Err(error) = resolve(&names, &layouts) else {
            panic!("two tags naming one layout resolve");
        };

        assert_eq!(
            error,
            ResolveError::Duplicate {
                name: "English - Universal".to_string(),
                tags: vec![tag("en"), tag("us")],
            }
        );
        assert!(error.to_string().contains("English - Universal"));
    }

    #[test]
    fn a_layout_sharing_only_the_language_does_not_satisfy_a_tag() {
        let layouts = vec![layout("ABC", "com.apple.keylayout.ABC", "en")];
        let names = names(&[("en", "English - Universal")]);

        let Err(error) = resolve(&names, &layouts) else {
            panic!("ABC satisfies a tag that names English - Universal");
        };

        assert_eq!(
            error,
            ResolveError::Unresolved {
                missing: vec![(tag("en"), "English - Universal".to_string())],
                enabled: vec!["ABC".to_string()],
            }
        );
    }

    #[test]
    fn an_empty_configuration_resolves_to_nothing() {
        let layouts = universal();
        let names = BTreeMap::new();

        let Ok(resolved) = resolve(&names, &layouts) else {
            panic!("an empty configuration does not resolve");
        };

        assert!(resolved.is_empty());
    }

    fn wait_for_current(name: &str) -> Option<Layout> {
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            let layout = current()?;
            let Layout {
                name: selected,
                id: _,
                language: _,
            } = &layout;
            if selected == name {
                return Some(layout);
            }
            if Instant::now() > deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn another_layout_than(started_on: &Layout) -> Layout {
        let layouts = enabled_layouts();
        let mut other = None;
        for layout in &layouts {
            if layout != started_on {
                other = Some(layout.clone());
                break;
            }
        }
        let Some(other) = other else {
            panic!("only one keyboard layout is enabled, so a switch proves nothing");
        };
        other
    }

    #[test]
    #[ignore = "needs the live machine: it reads and switches the real input sources"]
    fn the_enabled_layouts_contain_the_current_one_and_a_round_trip_restores_it() {
        let layouts = enabled_layouts();
        assert!(!layouts.is_empty(), "no enabled keyboard layout at all");

        let Some(started_on) = current() else {
            panic!("no keyboard layout is selected");
        };
        assert!(
            layouts.contains(&started_on),
            "the current layout {started_on:?} is not among the enabled ones {layouts:?}"
        );

        let other = another_layout_than(&started_on);

        let Ok(()) = select(&other) else {
            panic!("selecting {other:?} failed");
        };
        assert_eq!(
            wait_for_current(&other.name),
            Some(other.clone()),
            "selecting {other:?} did not take within 500 ms"
        );

        let Ok(()) = select(&started_on) else {
            panic!("selecting {started_on:?} back failed");
        };
        assert_eq!(
            wait_for_current(&started_on.name),
            Some(started_on.clone()),
            "the layout did not come back to {started_on:?} within 500 ms"
        );
    }

    #[test]
    #[ignore = "needs the live machine: it registers against the real distributed notification center"]
    fn installing_the_observer_wakes_the_distributed_center_and_removing_it_is_quiet() {
        let center = NSDistributedNotificationCenter::defaultCenter();
        center.setSuspended(true);

        let changed = Rc::new(Cell::new(false));
        let seen = Rc::clone(&changed);
        let observer = observe_changes(move || seen.set(true));

        assert!(
            !center.suspended(),
            "the observer left the distributed center suspended, so a layout change would queue up"
        );

        drop(observer);
        assert!(
            !changed.get(),
            "a notification arrived before any layout changed"
        );
    }

    #[test]
    #[ignore = "needs the live machine: it asks Text Input Sources for a layout that is not enabled"]
    fn selecting_a_layout_that_is_not_enabled_is_an_error() {
        let missing = layout("Klingon - Universal", "invalid.klingon", "tlh");

        assert_eq!(
            select(&missing),
            Err(TisError::NotEnabled {
                name: "Klingon - Universal".to_string()
            })
        );
    }
}
