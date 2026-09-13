//! Persistence: data dir layout, settings, per-cluster spec store, and
//! reconciliation (ADR-0006/0007/0011).
//!
//! Layout (architecture §5):
//!
//! ```text
//! <data dir>/kindboard/
//! ├── settings.json
//! ├── clusters/<name>.json
//! ├── logs/
//! └── tmp/
//! ```
//!
//! All writes go through the shared atomic writer (`fsutil`): serialize to a
//! sibling temp file, fsync, rename. The stored spec is the source of truth
//! for *how a cluster was created*; `kind get clusters` is the source of
//! truth for *what exists*. Reconciliation classifies clusters but never
//! mutates anything on drift.

use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Result, StateError};
use crate::fsutil;
use crate::spec::{self, ClusterSpec};

/// Directory name inside the XDG data dir.
pub const DATA_DIR_NAME: &str = "kindboard";

/// Default reconciliation poll interval (seconds).
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 5;

/// Whether `name` is safe to use as a single path component for a state
/// file (`clusters/<name>.json`): non-empty, no `/`, no `\`, no NUL, and
/// not `.` or `..`.
fn safe_state_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
}

/// The root data dir (`~/.local/share/kindboard` on Linux,
/// `~/Library/Application Support/kindboard` on macOS).
#[derive(Debug, Clone)]
pub struct DataDir {
    root: PathBuf,
}

impl DataDir {
    /// Create (on disk) or open the standard data dir under the XDG data
    /// dir (`dirs::data_local_dir()`).
    pub fn standard() -> Result<Self> {
        let base = dirs::data_local_dir().ok_or_else(|| {
            crate::error::CoreError::Config(
                "no local data dir available (XDG_DATA_HOME/HOME unset?)".to_string(),
            )
        })?;
        Self::new(base.join(DATA_DIR_NAME))
    }

    /// Open (and create) a data dir at an explicit root (used by tests and
    /// portable installs).
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|source| crate::error::CoreError::Io {
            path: root.clone(),
            source,
        })?;
        for sub in ["clusters", "logs", "tmp"] {
            let path = root.join(sub);
            std::fs::create_dir_all(&path).map_err(|source| crate::error::CoreError::Io {
                path: path.clone(),
                source,
            })?;
        }
        Ok(DataDir { root })
    }

    /// The root path.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path of `settings.json`.
    pub fn settings_path(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    /// The `clusters/` dir.
    pub fn clusters_dir(&self) -> PathBuf {
        self.root.join("clusters")
    }

    /// The `tmp/` dir (staging for manifests/configs; cleaned on start).
    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    /// The `logs/` dir.
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// Path of the spec file for a cluster.
    ///
    /// Fails when `name` is not a safe single path component (rejects `/`,
    /// `\`, NUL, and the `.`/`..` names). Defense-in-depth: cluster names
    /// that reach this point usually passed `spec::validate_name`, but
    /// names read back from `kind get clusters` or the cluster dir are
    /// external input and must never influence the filesystem path.
    pub fn cluster_path(&self, name: &str) -> Result<PathBuf> {
        if !safe_state_name(name) {
            return Err(StateError::InvalidName(
                name.to_string(),
                "cluster names must not contain '/', '\\', NUL, or be '.' or '..'".to_string(),
            )
            .into());
        }
        Ok(self.clusters_dir().join(format!("{name}.json")))
    }

    /// Remove stale staging files (called once on startup).
    pub fn clean_tmp(&self) -> Result<()> {
        let tmp = self.tmp_dir();
        match std::fs::remove_dir_all(&tmp) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(StateError::Io(err).into()),
        }
        std::fs::create_dir_all(&tmp).map_err(StateError::Io)?;
        Ok(())
    }

    /// Load settings, defaulting when the file is missing or empty.
    pub fn load_settings(&self) -> Result<Settings> {
        let path = self.settings_path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Settings::default());
            }
            Err(err) => return Err(StateError::Io(err).into()),
        };
        let text = String::from_utf8_lossy(&bytes);
        if text.trim().is_empty() {
            return Ok(Settings::default());
        }
        serde_json::from_str(&text).map_err(|err| {
            StateError::InvalidRecord("settings.json".to_string(), err.to_string()).into()
        })
    }

    /// Atomically save settings.
    pub fn save_settings(&self, settings: &Settings) -> Result<()> {
        let json = serde_json::to_vec_pretty(settings).map_err(StateError::Json)?;
        fsutil::atomic_write(&self.settings_path(), &json).map_err(StateError::Io)?;
        Ok(())
    }

    /// Save a cluster record (atomic; replaces any previous record).
    pub fn save_cluster(&self, record: &ClusterRecord) -> Result<()> {
        spec::validate_name(&record.spec.name)
            .map_err(|err| StateError::InvalidName(record.spec.name.clone(), err.to_string()))?;
        let json = serde_json::to_vec_pretty(record).map_err(StateError::Json)?;
        fsutil::atomic_write(&self.cluster_path(&record.spec.name)?, &json)
            .map_err(StateError::Io)?;
        Ok(())
    }

    /// Load a cluster record; missing file → `None`.
    pub fn load_cluster(&self, name: &str) -> Result<Option<ClusterRecord>> {
        let path = self.cluster_path(name)?;
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(StateError::Io(err).into()),
        };
        let text = String::from_utf8_lossy(&bytes);
        if text.trim().is_empty() {
            return Ok(None);
        }
        serde_json::from_str(&text)
            .map_err(|err| StateError::InvalidRecord(name.to_string(), err.to_string()).into())
    }

    /// All stored cluster names.
    pub fn stored_cluster_names(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();
        let dir = self.clusters_dir();
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(names),
            Err(err) => return Err(StateError::Io(err).into()),
        };
        for entry in entries {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(stem) = path.file_stem() else {
                continue;
            };
            let name = stem.to_string_lossy().to_string();
            if path.extension().is_some_and(|ext| ext == "json") {
                names.push(name);
            }
        }
        names.sort();
        Ok(names)
    }

    /// Delete a stored cluster record (returns whether it existed).
    pub fn remove_cluster(&self, name: &str) -> Result<bool> {
        let path = self.cluster_path(name)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(StateError::Io(err).into()),
        }
    }

    /// Reconcile `kind get clusters` output against the stored specs
    /// (ADR-0011). Pure classification — never mutates anything.
    pub fn reconcile(&self, live_cluster_names: &[String]) -> Result<ReconcileReport> {
        let mut entries = Vec::new();
        let stored = self.stored_cluster_names()?;
        for name in live_cluster_names {
            match self.load_cluster(name)? {
                Some(record) => entries.push(ClusterEntry {
                    name: name.clone(),
                    state: ClusterState::Managed(record),
                }),
                None => entries.push(ClusterEntry {
                    name: name.clone(),
                    state: ClusterState::Adopted,
                }),
            }
        }
        for name in &stored {
            if live_cluster_names.iter().any(|live| live == name) {
                continue;
            }
            let record = self.load_cluster(name)?.ok_or_else(|| {
                StateError::InvalidRecord(
                    name.clone(),
                    "listed in cluster dir but not loadable".to_string(),
                )
            })?;
            entries.push(ClusterEntry {
                name: name.clone(),
                state: ClusterState::Missing(record),
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(ReconcileReport { clusters: entries })
    }
}

/// App preferences persisted in `settings.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// Default k8s version for the create wizard.
    pub default_k8s_version: Option<String>,
    /// Remember the last wizard values (CNI, ingress, ports, …).
    pub remember_last_wizard: bool,
    /// Reconciliation poll interval in seconds.
    pub poll_interval_secs: u64,
    /// Active theme id ("dark" | "light" | "high-contrast"). `None` =
    /// default (dark). Introduced in 0.1.3 — older files deserialize to
    /// `None` without migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// Extra fields, preserved on save.
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            default_k8s_version: Some(spec::DEFAULT_K8S_VERSION.to_string()),
            remember_last_wizard: true,
            poll_interval_secs: DEFAULT_POLL_INTERVAL_SECS,
            theme: None,
            other: serde_json::Map::new(),
        }
    }
}

/// How a cluster came to exist (stored in the spec file).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClusterSource {
    /// Created by kindboard from a spec.
    Managed,
    /// Adopted from an existing kind cluster (spec written later, or the
    /// record was created by adoption).
    Adopted,
}

/// The persisted record for one cluster.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClusterRecord {
    /// The spec as-created (source of truth for recreate).
    pub spec: ClusterSpec,
    /// Creation timestamp (RFC 3339).
    pub created_at: String,
    /// Where the cluster came from.
    pub source: ClusterSource,
    /// The kind node image used (e.g. `kindest/node:v1.37.0`), for drift
    /// comparison.
    pub kind_node_image: String,
    /// Extra fields, preserved on save.
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

impl ClusterRecord {
    /// Build a record for a freshly-created managed cluster.
    pub fn managed(spec: ClusterSpec) -> Self {
        let kind_node_image = spec.k8s_version.node_image_tag();
        ClusterRecord {
            spec,
            created_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            source: ClusterSource::Managed,
            kind_node_image,
            other: serde_json::Map::new(),
        }
    }

    /// The creation timestamp as a datetime (1970 epoch when unparseable).
    pub fn created_at_datetime(&self) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&self.created_at)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_default()
    }
}

/// Classification of one cluster after reconciliation.
#[derive(Debug, Clone)]
pub enum ClusterState {
    /// Stored spec + live kind cluster.
    Managed(ClusterRecord),
    /// Live kind cluster without a stored spec.
    Adopted,
    /// Stored spec but the kind cluster is gone.
    Missing(ClusterRecord),
}

/// One row of the reconciliation report.
#[derive(Debug, Clone)]
pub struct ClusterEntry {
    /// Cluster name.
    pub name: String,
    /// Classification.
    pub state: ClusterState,
}

/// The reconciliation report (ADR-0011 classification matrix).
#[derive(Debug, Clone, Default)]
pub struct ReconcileReport {
    /// All classified clusters.
    pub clusters: Vec<ClusterEntry>,
}

impl ReconcileReport {
    /// Names of managed clusters.
    pub fn managed_names(&self) -> Vec<String> {
        self.clusters
            .iter()
            .filter(|entry| matches!(entry.state, ClusterState::Managed(_)))
            .map(|entry| entry.name.clone())
            .collect()
    }

    /// Names of adopted clusters.
    pub fn adopted_names(&self) -> Vec<String> {
        self.clusters
            .iter()
            .filter(|entry| matches!(entry.state, ClusterState::Adopted))
            .map(|entry| entry.name.clone())
            .collect()
    }

    /// Names of missing (spec-only) clusters.
    pub fn missing_names(&self) -> Vec<String> {
        self.clusters
            .iter()
            .filter(|entry| matches!(entry.state, ClusterState::Missing(_)))
            .map(|entry| entry.name.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("kindboard-state-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            TempDir { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn spec_named(name: &str) -> ClusterSpec {
        ClusterSpec {
            name: name.to_string(),
            ..ClusterSpec::default()
        }
    }

    #[test]
    fn data_dir_creates_layout() {
        let dir = TempDir::new("layout");
        let data = DataDir::new(dir.path.join("root")).unwrap();
        assert!(!data.settings_path().is_dir());
        assert!(data.clusters_dir().is_dir());
        assert!(data.logs_dir().is_dir());
        assert!(data.tmp_dir().is_dir());
    }

    #[test]
    fn settings_roundtrip_with_unknown_fields() {
        let dir = TempDir::new("settings");
        let data = DataDir::new(dir.path.clone()).unwrap();
        assert_eq!(data.load_settings().unwrap(), Settings::default());

        let mut settings = Settings {
            default_k8s_version: Some("1.30.0".to_string()),
            remember_last_wizard: false,
            poll_interval_secs: 10,
            theme: Some("light".to_string()),
            other: serde_json::Map::new(),
        };
        settings
            .other
            .insert("custom_key".to_string(), serde_json::json!({"a": 1}));
        data.save_settings(&settings).unwrap();

        let loaded = data.load_settings().unwrap();
        assert_eq!(loaded, settings);
        assert!(data.settings_path().is_file());
    }

    #[test]
    fn settings_without_theme_deserializes_to_none_and_none_is_omitted() {
        let dir = TempDir::new("settings-theme");
        let data = DataDir::new(dir.path.clone()).unwrap();

        // An older settings.json (no `theme` key) still loads.
        std::fs::write(
            data.settings_path(),
            r#"{"remember_last_wizard":true,"poll_interval_secs":5,"foo":1}"#,
        )
        .unwrap();
        let loaded = data.load_settings().unwrap();
        assert_eq!(loaded.theme, None);
        assert_eq!(
            loaded.other.get("foo"),
            Some(&serde_json::json!(1)),
            "unknown keys must still flow into `other`"
        );

        // A serialized default Settings omits the `theme` key entirely.
        let json = serde_json::to_vec_pretty(&Settings::default()).unwrap();
        let text = String::from_utf8(json).unwrap();
        assert!(!text.contains("theme"), "None theme must not be serialized");
    }

    #[test]
    fn cluster_record_roundtrip_and_backup() {
        let dir = TempDir::new("record");
        let data = DataDir::new(dir.path.clone()).unwrap();
        let record = ClusterRecord::managed(spec_named("demo"));
        data.save_cluster(&record).unwrap();

        let loaded = data.load_cluster("demo").unwrap().unwrap();
        assert_eq!(loaded, record);
        assert_eq!(loaded.source, ClusterSource::Managed);
        assert_eq!(loaded.kind_node_image, "kindest/node:v1.37.0");
        assert!(loaded.created_at_datetime().year() >= 2026);
        assert!(!loaded.created_at.is_empty());

        // Re-saving backs up the previous file.
        let record2 = ClusterRecord::managed(spec_named("demo"));
        data.save_cluster(&record2).unwrap();
        let bak = dir.path.join("clusters/demo.json.bak");
        assert!(bak.is_file());

        assert_eq!(data.stored_cluster_names().unwrap(), vec!["demo"]);
        assert!(data.remove_cluster("demo").unwrap());
        assert!(data.load_cluster("demo").unwrap().is_none());
        assert!(!data.remove_cluster("demo").unwrap());
    }

    #[test]
    fn invalid_cluster_name_rejected() {
        let dir = TempDir::new("invalid");
        let data = DataDir::new(dir.path.clone()).unwrap();
        let record = ClusterRecord::managed(spec_named("NOT VALID"));
        assert!(data.save_cluster(&record).is_err());
    }

    #[test]
    fn path_traversal_names_rejected_at_boundary() {
        let dir = TempDir::new("traversal");
        let data = DataDir::new(dir.path.clone()).unwrap();
        for evil in ["../evil", "a/b", "a\\b", "..", ".", "a\0b", ""] {
            assert!(
                data.load_cluster(evil).is_err(),
                "load_cluster({evil:?}) must be rejected"
            );
            assert!(
                data.remove_cluster(evil).is_err(),
                "remove_cluster({evil:?}) must be rejected"
            );
            assert!(
                data.cluster_path(evil).is_err(),
                "cluster_path({evil:?}) must be rejected"
            );
        }
        // No file escaped the cluster dir.
        assert!(!dir.path.join("evil.json").exists());
        assert!(!dir.path.parent().unwrap().join("evil.json").exists());
    }

    #[test]
    fn traversal_names_do_not_touch_existing_files() {
        let dir = TempDir::new("traversal-noop");
        let data = DataDir::new(dir.path.clone()).unwrap();
        // A file outside clusters/ that a traversal would hit if unchecked.
        let victim = dir.path.join("victim.json");
        std::fs::write(&victim, "important").unwrap();
        let err = data.load_cluster("../victim").unwrap_err();
        assert!(
            err.to_string().contains("victim") || err.to_string().contains("state key"),
            "{err}"
        );
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "important");
        assert!(data.remove_cluster("../victim").is_err());
        assert!(victim.is_file(), "victim must be untouched");
    }

    #[test]
    fn reconcile_classifies_managed_adopted_missing() {
        let dir = TempDir::new("reconcile");
        let data = DataDir::new(dir.path.clone()).unwrap();
        data.save_cluster(&ClusterRecord::managed(spec_named("mine")))
            .unwrap();
        data.save_cluster(&ClusterRecord::managed(spec_named("gone")))
            .unwrap();

        let report = data
            .reconcile(&["mine".to_string(), "someone-elses".to_string()])
            .unwrap();
        assert_eq!(
            report.managed_names(),
            vec!["mine"],
            "live + spec → Managed"
        );
        assert_eq!(
            report.adopted_names(),
            vec!["someone-elses"],
            "live, no spec → Adopted"
        );
        assert_eq!(
            report.missing_names(),
            vec!["gone"],
            "spec, no live → Missing"
        );
        assert_eq!(report.clusters.len(), 3);
        // Adopted entries keep no spec.
        let adopted = report
            .clusters
            .iter()
            .find(|entry| entry.name == "someone-elses")
            .unwrap();
        assert!(matches!(adopted.state, ClusterState::Adopted));
        // Missing entries keep the stored spec (for recreate).
        let missing = report
            .clusters
            .iter()
            .find(|entry| entry.name == "gone")
            .unwrap();
        assert!(matches!(missing.state, ClusterState::Missing(_)));
    }

    #[test]
    fn reconcile_empty_both_sides() {
        let dir = TempDir::new("reconcile-empty");
        let data = DataDir::new(dir.path.clone()).unwrap();
        let report = data.reconcile(&[]).unwrap();
        assert!(report.clusters.is_empty());
    }

    #[test]
    fn reconcile_never_mutates() {
        let dir = TempDir::new("reconcile-noop");
        let data = DataDir::new(dir.path.clone()).unwrap();
        data.save_cluster(&ClusterRecord::managed(spec_named("gone")))
            .unwrap();
        let report = data.reconcile(&["other".to_string()]).unwrap();
        assert_eq!(report.missing_names(), vec!["gone"]);
        // The stored spec is untouched.
        assert!(data.load_cluster("gone").unwrap().is_some());
        // And no record was created for the adopted cluster.
        assert!(data.load_cluster("other").unwrap().is_none());
    }

    #[test]
    fn clean_tmp_removes_stale_files() {
        let dir = TempDir::new("tmp");
        let data = DataDir::new(dir.path.clone()).unwrap();
        let stale = data.tmp_dir().join("kind-demo.yaml");
        std::fs::write(&stale, "x").unwrap();
        data.clean_tmp().unwrap();
        assert!(!stale.exists());
        assert!(data.tmp_dir().is_dir());
    }

    #[test]
    fn settings_garbage_is_error() {
        let dir = TempDir::new("settings-garbage");
        let data = DataDir::new(dir.path.clone()).unwrap();
        std::fs::write(data.settings_path(), "{not json").unwrap();
        assert!(data.load_settings().is_err());
    }

    #[test]
    fn stored_cluster_names_ignores_non_json_files() {
        let dir = TempDir::new("stored-filter");
        let data = DataDir::new(dir.path.clone()).unwrap();
        std::fs::write(data.clusters_dir().join("notes.txt"), "x").unwrap();
        std::fs::write(data.clusters_dir().join("demo.json"), "{}").unwrap();
        std::fs::write(data.clusters_dir().join("demo.json.bak"), "{}").unwrap();
        std::fs::write(data.clusters_dir().join("other.json"), "{}").unwrap();
        let names = data.stored_cluster_names().unwrap();
        assert_eq!(names, vec!["demo".to_string(), "other".to_string()]);
    }
}
