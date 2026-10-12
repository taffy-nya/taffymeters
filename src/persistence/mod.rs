//! Saved workspaces: the file format and the directory that holds them.
//!
//! A file stores logical configuration only: windows (desktop placement, size,
//! flags, group) with their BSP trees, and in each leaf the Module's type, its
//! `DataSpec`, style, view settings and color overrides. Process identities
//! (instance, window, split, subscription IDs), readers, results, caches and hover
//! state are never written. Bindings are renumbered per file.

use std::{collections::BTreeMap, fmt, fs, io, path::PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::theme::ThemeSettings;

/// Version written by this build; newer files are not read.
pub(crate) const FORMAT_VERSION: u32 = 1;
/// Input kind of the product's system loopback.
pub(crate) const SYSTEM_INPUT: &str = "system_default";
pub(crate) const DEFAULT_WORKSPACE: &str = "Default";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct WorkspaceFile {
    pub(crate) format_version: u32,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) input: InputConfig,
    #[serde(default = "ThemeSettings::dark")]
    pub(crate) theme: ThemeSettings,
    #[serde(default)]
    pub(crate) windows: Vec<WindowFile>,
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
    /// Outer top-left on the desktop, physical pixels.
    #[serde(default)]
    pub(crate) outer_position: Option<[f32; 2]>,
    /// Scale of the window when saved; converts the position for the builder.
    #[serde(default = "one")]
    pub(crate) pixels_per_point: f32,
    /// Content size in logical points.
    pub(crate) inner_size: [f32; 2],
    #[serde(default)]
    pub(crate) always_on_top: bool,
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

fn one() -> f32 {
    1.0
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
    /// Written by a newer build.
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
                "{path} uses format version {version}; this build reads up to {FORMAT_VERSION}"
            ),
            Self::InvalidName(name) => write!(f, "invalid workspace name: {name:?}"),
        }
    }
}

impl std::error::Error for PersistError {}

/// Workspace names double as file names: printable, no path or reserved
/// characters, no trailing dot or space, at most 64 characters.
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

/// Whether two workspace names denote the same file (Windows file names are
/// case-insensitive).
pub(crate) fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

#[derive(Default, Serialize, Deserialize)]
struct StoreState {
    #[serde(default)]
    last_workspace: Option<String>,
}

/// The directory of saved workspaces: `<root>/workspaces/<name>.json` and
/// `<root>/state.json` (the workspace to open next time).
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

    fn workspaces(&self) -> PathBuf {
        self.root.join("workspaces")
    }

    pub(crate) fn path_of(&self, name: &str) -> PathBuf {
        self.workspaces().join(format!("{name}.json"))
    }

    /// Saved workspace names, sorted.
    pub(crate) fn list(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(self.workspaces()) else {
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

    /// The saved workspace `name`, `None` when there is no such file.
    pub(crate) fn load(&self, name: &str) -> Result<Option<WorkspaceFile>, PersistError> {
        if !valid_name(name) {
            return Err(PersistError::InvalidName(name.to_owned()));
        }
        let path = self.path_of(name);
        let label = path.display().to_string();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(PersistError::Io(label, error)),
        };
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| PersistError::Parse(label.clone(), error.to_string()))?;
        let version = value
            .get("format_version")
            .and_then(Value::as_u64)
            .ok_or_else(|| PersistError::Parse(label.clone(), "missing format_version".into()))?;
        if version > u64::from(FORMAT_VERSION) {
            return Err(PersistError::Version(
                label,
                u32::try_from(version).unwrap_or(u32::MAX),
            ));
        }
        let mut file: WorkspaceFile = serde_json::from_value(value)
            .map_err(|error| PersistError::Parse(label, error.to_string()))?;
        // The file name is the workspace's identity.
        file.name = name.to_owned();
        Ok(Some(file))
    }

    /// Write `file` under its name: a temporary file next to it, then a rename,
    /// so an interrupted save never leaves a truncated workspace.
    pub(crate) fn save(&self, file: &WorkspaceFile) -> Result<(), PersistError> {
        if !valid_name(&file.name) {
            return Err(PersistError::InvalidName(file.name.clone()));
        }
        let directory = self.workspaces();
        let io = |error| PersistError::Io(directory.display().to_string(), error);
        fs::create_dir_all(&directory).map_err(io)?;
        let path = self.path_of(&file.name);
        let temporary = directory.join(format!(".{}.json.tmp", file.name));
        let bytes = serde_json::to_vec_pretty(file)
            .map_err(|error| PersistError::Parse(path.display().to_string(), error.to_string()))?;
        let label = path.display().to_string();
        fs::write(&temporary, bytes).map_err(|error| PersistError::Io(label.clone(), error))?;
        fs::rename(&temporary, &path).map_err(|error| PersistError::Io(label, error))
    }

    pub(crate) fn last_used(&self) -> Option<String> {
        let bytes = fs::read(self.root.join("state.json")).ok()?;
        let state: StoreState = serde_json::from_slice(&bytes).ok()?;
        state.last_workspace.filter(|name| valid_name(name))
    }

    /// Record the workspace to open next time: a temporary file, then a rename, so
    /// a failed or interrupted write keeps the previous selection.
    pub(crate) fn set_last_used(&self, name: &str) -> Result<(), PersistError> {
        let io = |error| PersistError::Io(self.root.display().to_string(), error);
        fs::create_dir_all(&self.root).map_err(io)?;
        let state = StoreState {
            last_workspace: Some(name.to_owned()),
        };
        let bytes = serde_json::to_vec_pretty(&state).expect("state serializes");
        let temporary = self.root.join(".state.json.tmp");
        fs::write(&temporary, bytes).map_err(io)?;
        fs::rename(&temporary, self.root.join("state.json")).map_err(io)
    }
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

    fn empty_workspace(name: &str) -> WorkspaceFile {
        WorkspaceFile {
            format_version: FORMAT_VERSION,
            name: name.into(),
            input: InputConfig::default(),
            theme: ThemeSettings::dark(),
            windows: Vec::new(),
        }
    }

    #[test]
    fn names_reject_paths_reserved_words_and_edge_whitespace() {
        assert!(valid_name("Mastering 2"));
        assert!(valid_name("混音"));
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
        assert!(same_name("混音", "混音"));
        assert!(!same_name("Mix", "Mix 2"));
        assert!(!same_name("Mix", "Mi"));
        assert!(!same_name("Mix", ""));
    }

    #[test]
    fn last_used_is_none_without_a_usable_selection() {
        let dir = TestDir::new();
        let store = WorkspaceStore::new(dir.path());
        assert_eq!(store.last_used(), None, "no state.json yet");

        fs::create_dir_all(dir.path()).unwrap();
        let state = dir.path().join("state.json");
        for unusable in [
            &b"{ not json"[..],
            b"",
            b"{}",
            br#"{"last_workspace": null}"#,
            br#"{"last_workspace": 7}"#,
            // A name that is not a valid file name is never handed out.
            br#"{"last_workspace": "..\\evil"}"#,
            br#"{"last_workspace": "a/b"}"#,
            br#"{"last_workspace": ""}"#,
        ] {
            fs::write(&state, unusable).unwrap();
            assert_eq!(
                store.last_used(),
                None,
                "{:?}",
                String::from_utf8_lossy(unusable)
            );
        }
    }

    #[test]
    fn last_used_names_a_workspace_that_does_not_exist_and_loading_it_finds_nothing() {
        // The store does not check that the file exists: the caller falls back to a
        // default layout when `load` reports `None` (`Workspace::open`).
        let dir = TestDir::new();
        let store = WorkspaceStore::new(dir.path());
        store.set_last_used("Deleted").unwrap();
        assert_eq!(store.last_used().as_deref(), Some("Deleted"));
        assert!(matches!(store.load("Deleted"), Ok(None)));
        assert!(store.list().is_empty());
    }

    #[test]
    fn saving_with_an_invalid_name_writes_nothing() {
        let dir = TestDir::new();
        let store = WorkspaceStore::new(dir.path());
        for bad in ["", "a/b", "..", "CON", "x."] {
            let file = WorkspaceFile {
                name: bad.into(),
                ..empty_workspace("Valid")
            };
            assert!(
                matches!(store.save(&file), Err(PersistError::InvalidName(name)) if name == bad),
                "{bad:?} was not rejected"
            );
        }
        assert!(!store.workspaces().exists(), "a directory was created");
        assert!(!dir.path().exists(), "the store root was created");
        assert!(store.list().is_empty());
    }

    #[test]
    fn newer_or_broken_files_are_reported_and_left_untouched() {
        let dir = TestDir::new();
        let store = WorkspaceStore::new(dir.path());
        assert!(store.load("Missing").unwrap().is_none());
        fs::create_dir_all(store.workspaces()).unwrap();
        fs::write(
            store.path_of("Future"),
            br#"{"format_version": 99, "name": "Future"}"#,
        )
        .unwrap();
        fs::write(store.path_of("Broken"), b"{ not json").unwrap();
        assert!(matches!(
            store.load("Future"),
            Err(PersistError::Version(_, 99))
        ));
        assert!(matches!(store.load("Broken"), Err(PersistError::Parse(..))));
        assert_eq!(fs::read(store.path_of("Broken")).unwrap(), b"{ not json");
        let file = empty_workspace("Saved");
        store.save(&file).unwrap();
        assert_eq!(store.load("Saved").unwrap(), Some(file));
        assert_eq!(store.list(), vec!["Broken", "Future", "Saved"]);
        store.set_last_used("Saved").unwrap();
        assert_eq!(store.last_used().as_deref(), Some("Saved"));
    }
}
