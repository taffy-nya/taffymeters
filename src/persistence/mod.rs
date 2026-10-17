//! The saved form of a workspace and the directory that holds it.
//!
//! One format serves the session (`<root>/session.json`, saved automatically) and
//! presets (`<root>/presets/<name>.json`, saved only on request, plus the built-in
//! ones compiled into the program). A file stores logical configuration only:
//! windows (frame relative to their display's work area, flags, group) with their
//! BSP trees, and in each leaf the Module's type, its `DataSpec`, style, view
//! settings and color overrides. Process identities (instance, window, split,
//! subscription IDs), readers, results, caches and hover state are never written.
//! Bindings are renumbered per file.

use std::{collections::BTreeMap, fmt, fs, io, path::PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::theme::ThemeSettings;

/// Version written by this build; other versions are not read.
pub(crate) const FORMAT_VERSION: u32 = 2;
/// Input kind of the product's system loopback.
pub(crate) const SYSTEM_INPUT: &str = "system_default";
/// The built-in preset a new session starts from.
pub(crate) const DEFAULT_PRESET: &str = "Default";

/// Presets compiled into the program, in the order the settings list them.
const BUILTIN_PRESETS: [(&str, &str); 3] = [
    (DEFAULT_PRESET, include_str!("presets/default.json")),
    ("Bar - Top", include_str!("presets/bar-top.json")),
    ("Bar - Bottom", include_str!("presets/bar-bottom.json")),
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct WorkspaceFile {
    pub(crate) format_version: u32,
    /// The preset the session was loaded from (session file only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) preset: Option<PresetRef>,
    #[serde(default)]
    pub(crate) input: InputConfig,
    /// Display reference level (dBFS); absent follows the build's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reference_dbfs: Option<f32>,
    #[serde(default = "ThemeSettings::dark")]
    pub(crate) theme: ThemeSettings,
    /// The displays the windows were on, in the order windows refer to them.
    pub(crate) displays: Vec<DisplayFile>,
    pub(crate) windows: Vec<WindowFile>,
}

/// A preset by origin. User presets are named by their file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PresetRef {
    Builtin(String),
    Saved(String),
}

impl PresetRef {
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Builtin(name) | Self::Saved(name) => name,
        }
    }
}

/// A display as it was when the file was saved.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct DisplayFile {
    /// Size of its work area, logical points.
    pub(crate) work_area: [f32; 2],
    /// Identifies the same display again; built-in presets have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) hint: Option<DisplayHint>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct DisplayHint {
    /// Top-left of the work area on the desktop, physical pixels.
    pub(crate) origin_px: [f32; 2],
    pub(crate) scale: f32,
}

/// The workspace's input. Kinds this build does not provide are kept verbatim.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct InputConfig {
    pub(crate) kind: String,
    #[serde(flatten)]
    pub(crate) options: BTreeMap<String, Value>,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            kind: SYSTEM_INPUT.to_owned(),
            options: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct WindowFile {
    /// Index into `displays`.
    #[serde(default)]
    pub(crate) display: usize,
    /// Outer left, top, right and bottom edge in logical points of that display,
    /// relative to the top-left of its work area. For a maximized window, the
    /// frame it had when saved.
    pub(crate) frame: [f32; 4],
    #[serde(default)]
    pub(crate) always_on_top: bool,
    /// Body drags do not move the window (or its group).
    #[serde(default)]
    pub(crate) pinned: bool,
    #[serde(default)]
    pub(crate) maximized: bool,
    /// Windows with the same number move together.
    #[serde(default)]
    pub(crate) group: Option<u32>,
    /// Index of the active leaf in left-to-right / top-to-bottom order.
    #[serde(default)]
    pub(crate) active: Option<u32>,
    #[serde(default)]
    pub(crate) layout: Option<LayoutFile>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SplitAxis {
    LeftRight,
    TopBottom,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "node", rename_all = "snake_case")]
pub(crate) enum LayoutFile {
    Leaf {
        module: ModuleFile,
    },
    Split {
        axis: SplitAxis,
        ratio: f32,
        #[serde(default)]
        manual: bool,
        first: Box<LayoutFile>,
        second: Box<LayoutFile>,
    },
}

/// One Module instance. `data`, `view` and `colors` stay raw JSON here so a
/// Module that cannot be restored keeps its configuration byte for byte.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ModuleFile {
    pub(crate) type_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) style: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) view: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) colors: BTreeMap<String, Value>,
}

impl ModuleFile {
    /// Binding numbers its raw `DataSpec` uses.
    pub(crate) fn bindings(&self) -> Vec<u64> {
        self.data
            .as_ref()
            .and_then(|data| data.get("outputs"))
            .and_then(Value::as_array)
            .map(|outputs| {
                outputs
                    .iter()
                    .filter_map(|output| output.get("binding").and_then(Value::as_u64))
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[derive(Debug)]
pub(crate) enum PersistError {
    Io(String, io::Error),
    Parse(String, String),
    /// Written in another format version (an older one, or by a newer build).
    Version(String, u32),
    InvalidName(String),
}

impl fmt::Display for PersistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(path, error) => write!(f, "{path}: {error}"),
            Self::Parse(path, error) => write!(f, "{path} is not a readable workspace: {error}"),
            Self::Version(path, version) => write!(
                f,
                "{path} uses format version {version}; this build reads version {FORMAT_VERSION}"
            ),
            Self::InvalidName(name) => write!(f, "invalid preset name: {name:?}"),
        }
    }
}

impl std::error::Error for PersistError {}

/// A workspace from the bytes of a file (`label` names it in errors). Besides
/// the format, the geometry must be usable: at least one window, each on a listed
/// display, with a finite frame of positive size.
fn parse(label: &str, bytes: &[u8]) -> Result<WorkspaceFile, PersistError> {
    let error = |reason: String| PersistError::Parse(label.to_owned(), reason);
    let value: Value = serde_json::from_slice(bytes).map_err(|e| error(e.to_string()))?;
    let version = value
        .get("format_version")
        .and_then(Value::as_u64)
        .ok_or_else(|| error("missing format_version".into()))?;
    if version != u64::from(FORMAT_VERSION) {
        return Err(PersistError::Version(
            label.to_owned(),
            u32::try_from(version).unwrap_or(u32::MAX),
        ));
    }
    let file: WorkspaceFile = serde_json::from_value(value).map_err(|e| error(e.to_string()))?;
    if file.windows.is_empty() {
        return Err(error("no windows".into()));
    }
    for display in &file.displays {
        let usable = display.work_area.iter().all(|v| v.is_finite() && *v > 0.0)
            && display.hint.as_ref().is_none_or(|hint| {
                hint.origin_px.iter().all(|v| v.is_finite())
                    && hint.scale.is_finite()
                    && hint.scale > 0.0
            });
        if !usable {
            return Err(error("a display has no usable work area".into()));
        }
    }
    for window in &file.windows {
        if window.display >= file.displays.len() {
            return Err(error(format!(
                "window on display {}, not listed",
                window.display
            )));
        }
        let [left, top, right, bottom] = window.frame;
        if !window.frame.iter().all(|v| v.is_finite()) || right <= left || bottom <= top {
            return Err(error(format!(
                "window frame {:?} is not usable",
                window.frame
            )));
        }
    }
    Ok(file)
}

/// Built-in preset names, in the order the settings list them.
pub(crate) fn builtin_presets() -> impl Iterator<Item = &'static str> {
    BUILTIN_PRESETS.iter().map(|(name, _)| *name)
}

/// The built-in preset `name`, `None` when there is none.
pub(crate) fn builtin_preset(name: &str) -> Option<Result<WorkspaceFile, PersistError>> {
    BUILTIN_PRESETS
        .iter()
        .find(|(builtin, _)| *builtin == name)
        .map(|(name, json)| parse(&format!("built-in preset {name:?}"), json.as_bytes()))
}

/// Preset names double as file names: printable, no path or reserved characters,
/// no trailing dot or space, at most 64 characters.
pub(crate) fn valid_name(name: &str) -> bool {
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    !name.trim().is_empty()
        && name.chars().count() <= 64
        && !name.ends_with(['.', ' '])
        && !name.starts_with(' ')
        && name.chars().all(|c| {
            !c.is_control() && !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
        })
        && !RESERVED
            .iter()
            .any(|reserved| reserved.eq_ignore_ascii_case(name))
}

/// Whether two preset names denote the same file (Windows file names are
/// case-insensitive).
pub(crate) fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// The configuration directory: `<root>/session.json` and
/// `<root>/presets/<name>.json`.
#[derive(Clone, Debug)]
pub(crate) struct WorkspaceStore {
    root: PathBuf,
}

impl WorkspaceStore {
    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `%APPDATA%\taffymeters`.
    pub(crate) fn default_root() -> Result<PathBuf, String> {
        std::env::var_os("APPDATA")
            .map(|appdata| PathBuf::from(appdata).join("taffymeters"))
            .ok_or_else(|| "APPDATA is not set; pass --config-dir".to_owned())
    }

    fn presets(&self) -> PathBuf {
        self.root.join("presets")
    }

    pub(crate) fn preset_path(&self, name: &str) -> PathBuf {
        self.presets().join(format!("{name}.json"))
    }

    pub(crate) fn session_path(&self) -> PathBuf {
        self.root.join("session.json")
    }

    /// Saved preset names, sorted.
    pub(crate) fn list_presets(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(self.presets()) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                (path.extension()? == "json")
                    .then(|| path.file_stem()?.to_str().map(str::to_owned))
                    .flatten()
            })
            .filter(|name| valid_name(name))
            .collect();
        names.sort_by_key(|name| name.to_lowercase());
        names
    }

    /// The saved preset `name`, `None` when there is no such file.
    pub(crate) fn load_preset(&self, name: &str) -> Result<Option<WorkspaceFile>, PersistError> {
        if !valid_name(name) {
            return Err(PersistError::InvalidName(name.to_owned()));
        }
        read(&self.preset_path(name))
    }

    pub(crate) fn save_preset(&self, name: &str, file: &WorkspaceFile) -> Result<(), PersistError> {
        if !valid_name(name) {
            return Err(PersistError::InvalidName(name.to_owned()));
        }
        write(&self.preset_path(name), file)
    }

    pub(crate) fn delete_preset(&self, name: &str) -> Result<(), PersistError> {
        if !valid_name(name) {
            return Err(PersistError::InvalidName(name.to_owned()));
        }
        let path = self.preset_path(name);
        fs::remove_file(&path).map_err(|error| PersistError::Io(path.display().to_string(), error))
    }

    /// The last session, `None` when there is none yet.
    pub(crate) fn load_session(&self) -> Result<Option<WorkspaceFile>, PersistError> {
        read(&self.session_path())
    }

    pub(crate) fn save_session(&self, file: &WorkspaceFile) -> Result<(), PersistError> {
        write(&self.session_path(), file)
    }
}

fn read(path: &std::path::Path) -> Result<Option<WorkspaceFile>, PersistError> {
    let label = path.display().to_string();
    match fs::read(path) {
        Ok(bytes) => parse(&label, &bytes).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(PersistError::Io(label, error)),
    }
}

/// Write `file` to `path`: a temporary file next to it, then a rename, so an
/// interrupted save never leaves a truncated file.
fn write(path: &std::path::Path, file: &WorkspaceFile) -> Result<(), PersistError> {
    let label = path.display().to_string();
    let io = |error| PersistError::Io(label.clone(), error);
    let directory = path.parent().expect("files live in a directory");
    fs::create_dir_all(directory).map_err(io)?;
    let name = path.file_name().expect("a file name").to_string_lossy();
    let temporary = directory.join(format!(".{name}.tmp"));
    let bytes = serde_json::to_vec_pretty(file).expect("a workspace file serializes");
    fs::write(&temporary, bytes).map_err(io)?;
    fs::rename(&temporary, path).map_err(io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A unique temporary directory (not created until used) that is removed on
    /// drop, also when the test fails.
    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "taffymeters-store-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&path);
            Self(path)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn one_window() -> WorkspaceFile {
        WorkspaceFile {
            format_version: FORMAT_VERSION,
            preset: None,
            input: InputConfig::default(),
            reference_dbfs: None,
            theme: ThemeSettings::dark(),
            displays: vec![DisplayFile {
                work_area: [1920.0, 1040.0],
                hint: None,
            }],
            windows: vec![WindowFile {
                display: 0,
                frame: [100.0, 100.0, 500.0, 300.0],
                always_on_top: false,
                pinned: false,
                maximized: false,
                group: None,
                active: None,
                layout: None,
            }],
        }
    }

    #[test]
    fn names_reject_paths_reserved_words_and_edge_whitespace() {
        assert!(valid_name("Mastering 2"));
        assert!(valid_name("混音"));
        assert!(valid_name("Bar - Top"));
        for bad in [
            "", " ", "a/b", "a\\b", "..", "x.", "x ", " x", "CON", "nul", "a:b", "a?",
        ] {
            assert!(!valid_name(bad), "{bad:?} accepted");
        }
        assert!(!valid_name(&"x".repeat(65)));
    }

    #[test]
    fn same_name_ignores_case_but_distinguishes_different_names() {
        assert!(same_name("Mix", "mix"));
        assert!(same_name("MIX", "mIx"));
        assert!(same_name("Ärger", "ärger"));
        assert!(same_name("ΣΑΣ", "σας"));
        assert!(!same_name("混音", "混音2"));
        assert!(!same_name("Mix", "Mix 2"));
        assert!(!same_name("Mix", "Mi"));
        assert!(!same_name("Mix", ""));
    }

    #[test]
    fn every_builtin_preset_parses_and_has_a_usable_name() {
        let names: Vec<_> = builtin_presets().collect();
        assert_eq!(names[0], DEFAULT_PRESET);
        for name in names {
            assert!(valid_name(name), "{name:?}");
            let file = builtin_preset(name).expect("listed").expect("parses");
            assert!(file.displays.iter().all(|display| display.hint.is_none()));
        }
        assert!(builtin_preset("Missing").is_none());
    }

    #[test]
    fn unusable_geometry_is_rejected() {
        let file = one_window();
        let reject = |change: &dyn Fn(&mut WorkspaceFile)| {
            let mut file = file.clone();
            change(&mut file);
            let bytes = serde_json::to_vec(&file).unwrap();
            assert!(
                matches!(parse("test", &bytes), Err(PersistError::Parse(..))),
                "{file:?} accepted"
            );
        };
        reject(&|file| file.windows.clear());
        reject(&|file| file.windows[0].display = 1);
        reject(&|file| file.windows[0].frame = [10.0, 0.0, 10.0, 50.0]);
        reject(&|file| file.windows[0].frame[3] = f32::NAN);
        reject(&|file| file.displays[0].work_area = [0.0, 1040.0]);
        reject(&|file| {
            file.displays[0].hint = Some(DisplayHint {
                origin_px: [0.0, 0.0],
                scale: 0.0,
            })
        });
        assert_eq!(
            parse("test", &serde_json::to_vec(&file).unwrap()).unwrap(),
            file
        );
    }

    #[test]
    fn saving_a_preset_with_an_invalid_name_writes_nothing() {
        let dir = TestDir::new();
        let store = WorkspaceStore::new(dir.path());
        for bad in ["", "a/b", "..", "CON", "x."] {
            assert!(
                matches!(store.save_preset(bad, &one_window()), Err(PersistError::InvalidName(name)) if name == bad),
                "{bad:?} was not rejected"
            );
        }
        assert!(!dir.path().exists(), "the store root was created");
        assert!(store.list_presets().is_empty());
    }

    #[test]
    fn other_versions_and_broken_files_are_reported_and_left_untouched() {
        let dir = TestDir::new();
        let store = WorkspaceStore::new(dir.path());
        assert!(store.load_preset("Missing").unwrap().is_none());
        assert!(store.load_session().unwrap().is_none());
        fs::create_dir_all(store.presets()).unwrap();
        fs::write(
            store.preset_path("Future"),
            br#"{"format_version": 99, "windows": []}"#,
        )
        .unwrap();
        fs::write(
            store.preset_path("Old"),
            br#"{"format_version": 1, "name": "Old", "windows": []}"#,
        )
        .unwrap();
        fs::write(store.preset_path("Broken"), b"{ not json").unwrap();
        assert!(matches!(
            store.load_preset("Future"),
            Err(PersistError::Version(_, 99))
        ));
        assert!(matches!(
            store.load_preset("Old"),
            Err(PersistError::Version(_, 1))
        ));
        assert!(matches!(
            store.load_preset("Broken"),
            Err(PersistError::Parse(..))
        ));
        assert_eq!(
            fs::read(store.preset_path("Broken")).unwrap(),
            b"{ not json"
        );
        let file = one_window();
        store.save_preset("Saved", &file).unwrap();
        assert_eq!(store.load_preset("Saved").unwrap(), Some(file.clone()));
        assert_eq!(
            store.list_presets(),
            vec!["Broken", "Future", "Old", "Saved"]
        );
        store.delete_preset("Saved").unwrap();
        assert!(store.load_preset("Saved").unwrap().is_none());
        store.save_session(&file).unwrap();
        assert_eq!(store.load_session().unwrap(), Some(file));
        assert_eq!(store.list_presets(), vec!["Broken", "Future", "Old"]);
    }
}
