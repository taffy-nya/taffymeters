use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

use super::{InputConfig, PersistError, WorkspaceStore, read_bytes, versioned, write};

pub(crate) const LOCAL_FORMAT_VERSION: u32 = 1;

/// The chosen input, frame rate limit and reference levels of this machine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct LocalSettings {
    pub(crate) format_version: u32,
    #[serde(default)]
    pub(crate) input: InputConfig,
    /// Frames per second; absence follows the build's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) fps_limit: Option<f32>,
    /// By device ID (`cpal::DeviceId` text), or by input kind for inputs without one;
    /// absent entries follow the build's default.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) reference_dbfs: BTreeMap<String, f32>,
}

impl Default for LocalSettings {
    fn default() -> Self {
        Self {
            format_version: LOCAL_FORMAT_VERSION,
            input: InputConfig::default(),
            fps_limit: None,
            reference_dbfs: BTreeMap::new(),
        }
    }
}

/// What sessions kept before `local.json` existed.
#[derive(Default, Deserialize)]
struct LegacySession {
    #[serde(default)]
    input: InputConfig,
    reference_dbfs: Option<f32>,
    fps_limit: Option<f32>,
}

#[derive(Debug)]
pub(crate) enum LocalState {
    Read,
    /// There was no `local.json`; the settings come from the session, which no longer
    /// keeps them, so they must be written at once. The reference level belongs to
    /// whichever device opens.
    Migrated {
        reference_dbfs: Option<f32>,
    },
    /// Runs on defaults; the file must not be overwritten.
    Unreadable(PersistError),
}

#[derive(Debug)]
pub(crate) struct LocalLoad {
    pub(crate) settings: LocalSettings,
    pub(crate) state: LocalState,
}

impl WorkspaceStore {
    pub(crate) fn local_path(&self) -> PathBuf {
        self.root.join("local.json")
    }

    pub(crate) fn load_local(&self) -> LocalLoad {
        let unreadable = |error| LocalLoad {
            settings: LocalSettings::default(),
            state: LocalState::Unreadable(error),
        };
        match read_bytes(&self.local_path()) {
            Ok(Some((label, bytes))) => match versioned(&label, &bytes, LOCAL_FORMAT_VERSION)
                .and_then(|value| {
                    serde_json::from_value(value)
                        .map_err(|error| PersistError::Parse(label, error.to_string()))
                }) {
                Ok(settings) => LocalLoad {
                    settings,
                    state: LocalState::Read,
                },
                Err(error) => unreadable(error),
            },
            Ok(None) => self.migrate_local(),
            Err(error) => unreadable(error),
        }
    }

    /// Takes the fields from any session that still parses as JSON, whatever its version.
    fn migrate_local(&self) -> LocalLoad {
        let legacy: LegacySession = read_bytes(&self.session_path())
            .ok()
            .flatten()
            .and_then(|(_, bytes)| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        LocalLoad {
            settings: LocalSettings {
                input: legacy.input,
                fps_limit: legacy.fps_limit,
                ..LocalSettings::default()
            },
            state: LocalState::Migrated {
                reference_dbfs: legacy.reference_dbfs,
            },
        }
    }

    pub(crate) fn save_local(&self, settings: &LocalSettings) -> Result<(), PersistError> {
        write(&self.local_path(), settings)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    use serde_json::json;

    use super::*;
    use crate::persistence::{CAPTURE_INPUT, DEFAULT_PLAYBACK_INPUT};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "taffymeters-local-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_missing_local_file_migrates_the_session_fields() {
        let dir = TestDir::new();
        let store = WorkspaceStore::new(&dir.0);
        fs::write(
            store.session_path(),
            json!({"format_version":2,"input":{"kind":CAPTURE_INPUT,"device":"wasapi:mic"},
                "reference_dbfs":-12.0,"fps_limit":30.0,"windows":[]})
            .to_string(),
        )
        .unwrap();
        let load = store.load_local();
        assert_eq!(load.settings.input.kind, CAPTURE_INPUT);
        assert_eq!(load.settings.input.option("device"), Some("wasapi:mic"));
        assert_eq!(load.settings.fps_limit, Some(30.0));
        assert!(load.settings.reference_dbfs.is_empty());
        assert!(matches!(
            load.state,
            LocalState::Migrated {
                reference_dbfs: Some(-12.0)
            }
        ));
    }

    #[test]
    fn without_local_file_or_session_the_defaults_migrate() {
        let dir = TestDir::new();
        let load = WorkspaceStore::new(&dir.0).load_local();
        assert_eq!(load.settings, LocalSettings::default());
        assert_eq!(load.settings.input.kind, DEFAULT_PLAYBACK_INPUT);
        assert!(matches!(
            load.state,
            LocalState::Migrated {
                reference_dbfs: None
            }
        ));
    }

    #[test]
    fn a_saved_local_file_is_read_back_and_the_session_ignored() {
        let dir = TestDir::new();
        let store = WorkspaceStore::new(&dir.0);
        fs::write(
            store.session_path(),
            json!({"format_version":2,"fps_limit":30.0,"windows":[]}).to_string(),
        )
        .unwrap();
        let settings = LocalSettings {
            fps_limit: Some(90.0),
            reference_dbfs: BTreeMap::from([("wasapi:speakers".to_owned(), -12.0)]),
            ..LocalSettings::default()
        };
        store.save_local(&settings).unwrap();
        let load = store.load_local();
        assert_eq!(load.settings, settings);
        assert!(matches!(load.state, LocalState::Read));
    }

    #[test]
    fn other_versions_and_broken_local_files_are_unreadable() {
        let dir = TestDir::new();
        let store = WorkspaceStore::new(&dir.0);
        for (bytes, version) in [
            (&br#"{"format_version":2}"#[..], Some(2)),
            (b"{ not json", None),
            (br#"{"format_version":1,"fps_limit":"fast"}"#, None),
        ] {
            fs::write(store.local_path(), bytes).unwrap();
            let load = store.load_local();
            assert_eq!(load.settings, LocalSettings::default());
            match (load.state, version) {
                (LocalState::Unreadable(PersistError::Version(_, found, 1)), Some(expected)) => {
                    assert_eq!(found, expected);
                }
                (LocalState::Unreadable(PersistError::Parse(..)), None) => {}
                (state, _) => panic!("{state:?} for {:?}", String::from_utf8_lossy(bytes)),
            }
            assert_eq!(fs::read(store.local_path()).unwrap(), bytes);
        }
    }
}
