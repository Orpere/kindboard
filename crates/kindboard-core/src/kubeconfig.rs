//! Kubeconfig load/merge/save (ADR-0007: kube-rs round-trip + atomic write
//! + `.bak`).
//!
//! [`KubeconfigStore`] wraps a `kube::config::Kubeconfig` plus the path it
//! lives at. All edits go through `kube::config::Kubeconfig` (never raw text
//! munging), so unknown fields survive a round-trip via the flattened
//! `other` map, and saves are crash-safe (sibling temp file + rename; the
//! previous file is copied to `<path>.bak` first).

use std::path::{Path, PathBuf};

use kube::config::{
    AuthInfo, Cluster, Context, Kubeconfig, NamedAuthInfo, NamedCluster, NamedContext,
};
use secrecy::SecretString;

use crate::error::{KubeconfigError, Result};
use crate::fsutil;

/// Prefix kind uses for context/cluster/user names of managed clusters.
pub const KIND_ENTRY_PREFIX: &str = "kind-";

/// A loaded kubeconfig plus the path it came from.
#[derive(Debug, Clone)]
pub struct KubeconfigStore {
    path: PathBuf,
    config: Kubeconfig,
}

impl KubeconfigStore {
    /// An empty store at the default path (`$KUBECONFIG` or
    /// `~/.kube/config`).
    pub fn empty() -> Self {
        KubeconfigStore {
            path: default_path(),
            config: Kubeconfig::default(),
        }
    }

    /// Load the default kubeconfig: `$KUBECONFIG` (first entry when
    /// colon-separated) or `~/.kube/config`. A missing file is *not* an
    /// error — it yields an empty store.
    pub fn load() -> Result<Self> {
        Self::load_from(default_path())
    }

    /// Load from a specific path. Missing/empty files yield an empty store;
    /// unparseable content is an error.
    pub fn load_from(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let config = load_kubeconfig(&path)?;
        Ok(KubeconfigStore { path, config })
    }

    /// Parse from an in-memory YAML document, tagged with a nominal path
    /// (used for `kind get kubeconfig` output).
    pub fn from_yaml(path: impl Into<PathBuf>, yaml: &str) -> Result<Self> {
        let config = if yaml.trim().is_empty() {
            Kubeconfig::default()
        } else {
            Kubeconfig::from_yaml(yaml).map_err(KubeconfigError::Kube)?
        };
        Ok(KubeconfigStore {
            path: path.into(),
            config,
        })
    }

    /// The file this store persists to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The wrapped kubeconfig.
    pub fn config(&self) -> &Kubeconfig {
        &self.config
    }

    /// The wrapped kubeconfig, mutably.
    pub fn config_mut(&mut self) -> &mut Kubeconfig {
        &mut self.config
    }

    /// Names of all contexts.
    pub fn context_names(&self) -> Vec<String> {
        self.config
            .contexts
            .iter()
            .map(|ctx| ctx.name.clone())
            .collect()
    }

    /// The current context name, if set.
    pub fn current_context(&self) -> Option<&str> {
        self.config.current_context.as_deref()
    }

    /// Set the current context.
    pub fn set_current_context(&mut self, name: impl Into<String>) {
        self.config.current_context = Some(name.into());
    }

    /// Whether a context with this exact name exists.
    pub fn verify_context(&self, name: &str) -> bool {
        self.config.contexts.iter().any(|ctx| ctx.name == name)
    }

    /// The kind-`<cluster_name>` context name.
    pub fn kind_context_name(cluster_name: &str) -> String {
        format!("{KIND_ENTRY_PREFIX}{cluster_name}")
    }

    /// Upsert the `kind-<cluster_name>` triple (context/cluster/user) into
    /// the store and make it the current context.
    ///
    /// Re-running with the same name replaces the previous entries (no
    /// duplicates). The server URL must parse as a URL and use the `https`
    /// scheme: a crafted context must not be able to persist a non-HTTPS
    /// server (plaintext/file URLs) into the user's kubeconfig (audit
    /// finding L4).
    pub fn ensure_context(
        &mut self,
        cluster_name: &str,
        server: &str,
        ca_data: &str,
        client_cert_data: &str,
        client_key_data: &str,
    ) -> Result<()> {
        let parsed = match url::Url::parse(server) {
            Ok(parsed) => parsed,
            Err(err) => {
                return Err(KubeconfigError::InvalidServer {
                    url: server.to_string(),
                    reason: err.to_string(),
                }
                .into());
            }
        };
        if parsed.scheme() != "https" {
            return Err(KubeconfigError::InvalidServer {
                url: server.to_string(),
                reason: "only https:// servers are accepted".to_string(),
            }
            .into());
        }
        let name = Self::kind_context_name(cluster_name);

        let mut triple = Kubeconfig::default();
        triple.clusters.push(NamedCluster {
            name: name.clone(),
            cluster: Some(Cluster {
                server: Some(server.to_string()),
                certificate_authority_data: Some(ca_data.to_string()),
                ..Cluster::default()
            }),
            ..NamedCluster::default()
        });
        triple.auth_infos.push(NamedAuthInfo {
            name: name.clone(),
            auth_info: Some(AuthInfo {
                client_certificate_data: Some(client_cert_data.to_string()),
                client_key_data: Some(SecretString::new(client_key_data.to_string().into())),
                ..AuthInfo::default()
            }),
            ..NamedAuthInfo::default()
        });
        triple.contexts.push(NamedContext {
            name: name.clone(),
            context: Some(Context {
                cluster: name.clone(),
                user: Some(name.clone()),
                namespace: Some("default".to_string()),
                ..Context::default()
            }),
            ..NamedContext::default()
        });
        self.merge(triple);
        self.config.current_context = Some(name);
        Ok(())
    }

    /// Merge the entries of `kind get kubeconfig --name <cluster>` (a
    /// kubeconfig YAML document on stdout) into the store.
    ///
    /// Extracts the cluster/user/context entries named `kind-<cluster_name>`
    /// (falling back to the first entry of each kind when the names differ)
    /// and upserts them; same-name entries are replaced, never duplicated.
    /// Does not touch other clusters/users/contexts.
    pub fn ensure_context_from_kind_output(
        &mut self,
        cluster_name: &str,
        kind_kubeconfig_yaml: &str,
    ) -> Result<()> {
        let incoming =
            Kubeconfig::from_yaml(kind_kubeconfig_yaml).map_err(KubeconfigError::Kube)?;
        let wanted = Self::kind_context_name(cluster_name);

        let take_entry = |entries: &[String]| -> Option<String> {
            entries
                .iter()
                .find(|name| name.as_str() == wanted)
                .or_else(|| entries.first())
                .cloned()
        };
        let cluster_entry = take_entry(
            &incoming
                .clusters
                .iter()
                .map(|c| c.name.clone())
                .collect::<Vec<_>>(),
        );
        let user_entry = take_entry(
            &incoming
                .auth_infos
                .iter()
                .map(|u| u.name.clone())
                .collect::<Vec<_>>(),
        );
        let context_entry = take_entry(
            &incoming
                .contexts
                .iter()
                .map(|c| c.name.clone())
                .collect::<Vec<_>>(),
        );

        let mut triple = Kubeconfig::default();
        if let Some(name) = &cluster_entry
            && let Some(cluster) = incoming.clusters.iter().find(|c| &c.name == name)
        {
            triple.clusters.push(cluster.clone());
        }
        if let Some(name) = &user_entry
            && let Some(user) = incoming.auth_infos.iter().find(|u| &u.name == name)
        {
            triple.auth_infos.push(user.clone());
        }
        if let Some(name) = &context_entry
            && let Some(context) = incoming.contexts.iter().find(|c| &c.name == name)
        {
            triple.contexts.push(context.clone());
        }
        self.merge(triple);
        if let Some(name) = context_entry {
            self.config.current_context = Some(name);
        }
        Ok(())
    }

    /// Merge another kubeconfig in with *upsert* semantics: same-named
    /// entries are replaced by the incoming ones, everything else is kept.
    /// (kube-rs `Kubeconfig::merge` is first-wins, which would silently
    /// keep stale entries on re-export; upsert is what ensure/repair needs.)
    fn merge(&mut self, next: Kubeconfig) {
        use std::collections::HashSet;
        let incoming_clusters: HashSet<String> =
            next.clusters.iter().map(|c| c.name.clone()).collect();
        let incoming_users: HashSet<String> =
            next.auth_infos.iter().map(|u| u.name.clone()).collect();
        let incoming_contexts: HashSet<String> =
            next.contexts.iter().map(|c| c.name.clone()).collect();
        self.config
            .clusters
            .retain(|c| !incoming_clusters.contains(&c.name));
        self.config
            .auth_infos
            .retain(|u| !incoming_users.contains(&u.name));
        self.config
            .contexts
            .retain(|c| !incoming_contexts.contains(&c.name));
        self.config.clusters.extend(next.clusters);
        self.config.auth_infos.extend(next.auth_infos);
        self.config.contexts.extend(next.contexts);
    }

    /// Remove the `kind-<cluster_name>` triple (context, cluster, user).
    ///
    /// Clears `current-context` if it pointed at the removed context.
    /// Returns whether anything was removed.
    pub fn remove_context(&mut self, cluster_name: &str) -> bool {
        let name = Self::kind_context_name(cluster_name);
        let before =
            self.config.clusters.len() + self.config.auth_infos.len() + self.config.contexts.len();
        self.config.clusters.retain(|c| c.name != name);
        self.config.auth_infos.retain(|u| u.name != name);
        self.config.contexts.retain(|c| c.name != name);
        if self.config.current_context.as_deref() == Some(name.as_str()) {
            self.config.current_context = None;
        }
        let after =
            self.config.clusters.len() + self.config.auth_infos.len() + self.config.contexts.len();
        before != after
    }

    /// Serialize to YAML.
    pub fn to_yaml(&self) -> Result<String> {
        Ok(serde_yaml::to_string(&self.config).map_err(KubeconfigError::Yaml)?)
    }

    /// Atomically write the store to its path.
    ///
    /// Serialize → write sibling temp file + fsync + rename; the previous
    /// file (if any) is copied to `<path>.bak` first (ADR-0007). The
    /// kubeconfig may contain client keys/tokens, so the file is created
    /// owner-only (`0600`) — matching the `~/.kube/config` convention and
    /// never loosening an existing mode.
    pub fn save(&self) -> Result<()> {
        let yaml = self.to_yaml()?;
        fsutil::atomic_write_private(&self.path, yaml.as_bytes()).map_err(KubeconfigError::Io)?;
        Ok(())
    }
}

/// Default kubeconfig path: `$KUBECONFIG` (first entry when
/// colon-separated) or `~/.kube/config`.
pub fn default_path() -> PathBuf {
    default_path_with(std::env::var_os("KUBECONFIG"))
}

/// Pure form of [`default_path`] (testable without touching the process
/// env).
pub fn default_path_with(kubeconfig_env: Option<std::ffi::OsString>) -> PathBuf {
    if let Some(kubeconfig) = kubeconfig_env {
        let entries: Vec<PathBuf> = std::env::split_paths(&kubeconfig).collect();
        if let Some(first) = entries.first() {
            return first.clone();
        }
    }
    match dirs::home_dir() {
        Some(home) => home.join(".kube").join("config"),
        None => PathBuf::from(".kube/config"),
    }
}

fn load_kubeconfig(path: &Path) -> Result<Kubeconfig> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Kubeconfig::default());
        }
        Err(err) => return Err(KubeconfigError::Io(err).into()),
    };
    let text = String::from_utf8_lossy(&bytes);
    if text.trim().is_empty() {
        return Ok(Kubeconfig::default());
    }
    Ok(Kubeconfig::from_yaml(&text).map_err(KubeconfigError::Kube)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialize tests that mutate the KUBECONFIG env var.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("kindboard-kubeconfig-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            TempDir { path }
        }

        fn join(&self, name: &str) -> PathBuf {
            self.path.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    const SYNTHETIC: &str = r#"apiVersion: v1
kind: Config
clusters:
- cluster:
    server: https://127.0.0.1:39999
  name: other-cluster
contexts:
- context:
    cluster: other-cluster
    user: other-user
  name: other-context
current-context: other-context
users:
- name: other-user
  user: {}
"#;

    #[test]
    fn load_missing_file_yields_empty_store() {
        let dir = TempDir::new("missing");
        let store = KubeconfigStore::load_from(dir.join("nope.yaml")).unwrap();
        assert_eq!(store.context_names(), Vec::<String>::new());
        assert!(store.current_context().is_none());
        assert!(!store.verify_context("kind-demo"));
    }

    #[test]
    fn load_empty_file_yields_empty_store() {
        let dir = TempDir::new("empty");
        let path = dir.join("config");
        std::fs::write(&path, "").unwrap();
        let store = KubeconfigStore::load_from(&path).unwrap();
        assert_eq!(store.context_names(), Vec::<String>::new());

        std::fs::write(&path, "  \n\t\n").unwrap();
        let store = KubeconfigStore::load_from(&path).unwrap();
        assert_eq!(store.context_names(), Vec::<String>::new());
    }

    #[test]
    fn load_garbage_yaml_is_error() {
        let dir = TempDir::new("garbage");
        let path = dir.join("config");
        std::fs::write(&path, ":::\n- not: [valid").unwrap();
        assert!(KubeconfigStore::load_from(&path).is_err());
    }

    #[test]
    fn load_reads_kubeconfig_env_var() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = TempDir::new("env");
        let path = dir.join("custom-config");
        std::fs::write(&path, SYNTHETIC).unwrap();
        let resolved = default_path_with(Some(path.as_os_str().to_owned()));
        assert_eq!(resolved, path);
        let store = KubeconfigStore::load_from(&resolved).unwrap();
        assert_eq!(store.path(), path.as_path());
        assert!(store.verify_context("other-context"));
    }

    #[test]
    fn default_path_falls_back_to_home_kube() {
        let resolved = default_path_with(None);
        assert!(resolved.ends_with(".kube/config"), "{resolved:?}");
        let multi = std::ffi::OsString::from("/a/config:/b/config");
        let resolved = default_path_with(Some(multi));
        assert!(resolved.ends_with("/a/config"), "{resolved:?}");
    }

    #[test]
    fn load_preserves_unknown_fields() {
        let dir = TempDir::new("unknown");
        let path = dir.join("config");
        let with_unknown = r#"apiVersion: v1
kind: Config
clusters: []
contexts:
- name: a
  context:
    cluster: a
    user: a
  some-custom-field:
    nested: value
users:
- name: a
  user: {}
preferences:
  colors: true
some-top-level-unknown: hello
"#;
        std::fs::write(&path, with_unknown).unwrap();
        let store = KubeconfigStore::load_from(&path).unwrap();
        store.save().unwrap();
        let roundtripped = std::fs::read_to_string(&path).unwrap();
        assert!(roundtripped.contains("some-custom-field"), "{roundtripped}");
        assert!(roundtripped.contains("nested: value"), "{roundtripped}");
        assert!(
            roundtripped.contains("some-top-level-unknown: hello"),
            "{roundtripped}"
        );
        assert!(roundtripped.contains("colors: true"), "{roundtripped}");
    }

    #[test]
    fn verify_context_matches_exact_name() {
        let dir = TempDir::new("verify");
        let store = KubeconfigStore::from_yaml(dir.path.clone(), SYNTHETIC).unwrap();
        assert!(store.verify_context("other-context"));
        assert!(!store.verify_context("other"));
        assert!(!store.verify_context("kind-other-context"));
    }

    #[test]
    fn ensure_context_adds_kind_triple_and_current() {
        let dir = TempDir::new("ensure");
        let mut store = KubeconfigStore::empty();
        store.path = dir.join("config");
        store
            .ensure_context("demo", "https://127.0.0.1:42345", "CA", "CERT", "KEY")
            .unwrap();

        assert!(store.verify_context("kind-demo"));
        assert_eq!(store.current_context(), Some("kind-demo"));

        let cluster = store
            .config()
            .clusters
            .iter()
            .find(|c| c.name == "kind-demo")
            .unwrap();
        let cluster = cluster.cluster.as_ref().unwrap();
        assert_eq!(cluster.server.as_deref(), Some("https://127.0.0.1:42345"));
        assert_eq!(cluster.certificate_authority_data.as_deref(), Some("CA"));

        let user = store
            .config()
            .auth_infos
            .iter()
            .find(|u| u.name == "kind-demo")
            .unwrap();
        let user = user.auth_info.as_ref().unwrap();
        assert_eq!(user.client_certificate_data.as_deref(), Some("CERT"));

        let context = store
            .config()
            .contexts
            .iter()
            .find(|c| c.name == "kind-demo")
            .unwrap();
        let context = context.context.as_ref().unwrap();
        assert_eq!(context.cluster, "kind-demo");
        assert_eq!(context.user.as_deref(), Some("kind-demo"));
    }

    #[test]
    fn ensure_context_is_idempotent() {
        let dir = TempDir::new("idempotent");
        let mut store = KubeconfigStore::empty();
        store.path = dir.join("config");
        store
            .ensure_context("demo", "https://127.0.0.1:1", "A", "B", "C")
            .unwrap();
        store
            .ensure_context("demo", "https://127.0.0.1:2", "D", "E", "F")
            .unwrap();
        assert_eq!(store.context_names().len(), 1);
        assert_eq!(store.config().clusters.len(), 1);
        assert_eq!(store.config().auth_infos.len(), 1);
        let cluster = &store.config().clusters[0].cluster.as_ref().unwrap().server;
        assert_eq!(cluster.as_deref(), Some("https://127.0.0.1:2"));
    }

    #[test]
    fn ensure_context_rejects_bad_server() {
        let mut store = KubeconfigStore::empty();
        let err = store
            .ensure_context("demo", "not a url", "A", "B", "C")
            .unwrap_err();
        match err {
            crate::CoreError::Kubeconfig(KubeconfigError::InvalidServer { url, .. }) => {
                assert_eq!(url, "not a url");
            }
            other => panic!("expected InvalidServer, got {other:?}"),
        }
    }

    #[test]
    fn ensure_context_rejects_non_https_servers() {
        // Audit finding L4: a crafted context must not persist a non-HTTPS
        // server into the user's kubeconfig.
        for server in [
            "http://127.0.0.1:1",
            "file:///etc/passwd",
            "ftp://example.com/config",
            "127.0.0.1:1",
        ] {
            let mut store = KubeconfigStore::empty();
            let err = store
                .ensure_context("demo", server, "A", "B", "C")
                .unwrap_err();
            match err {
                crate::CoreError::Kubeconfig(KubeconfigError::InvalidServer { url, .. }) => {
                    assert_eq!(url, server);
                }
                other => panic!("expected InvalidServer for {server}, got {other:?}"),
            }
            assert!(
                store.config().clusters.is_empty(),
                "nothing merged for {server}"
            );
        }
    }

    #[test]
    fn ensure_context_from_kind_output_merges_triple() {
        let dir = TempDir::new("kindoutput");
        let kind_output = r#"apiVersion: v1
clusters:
- cluster:
    certificate-authority-data: ca999
    server: https://127.0.0.1:41535
  name: kind-demo
contexts:
- context:
    cluster: kind-demo
    user: kind-demo
  name: kind-demo
current-context: kind-demo
kind: Config
users:
- name: kind-demo
  user:
    client-certificate-data: cert999
    client-key-data: key999
"#;
        let mut store = KubeconfigStore::empty();
        store.path = dir.join("config");
        store
            .ensure_context_from_kind_output("demo", kind_output)
            .unwrap();
        assert!(store.verify_context("kind-demo"));
        assert_eq!(store.current_context(), Some("kind-demo"));
        let cluster = store
            .config()
            .clusters
            .iter()
            .find(|c| c.name == "kind-demo")
            .unwrap()
            .cluster
            .as_ref()
            .unwrap();
        assert_eq!(cluster.certificate_authority_data.as_deref(), Some("ca999"));
        assert_eq!(cluster.server.as_deref(), Some("https://127.0.0.1:41535"));
    }

    #[test]
    fn remove_context_removes_triple_and_clears_current() {
        let dir = TempDir::new("remove");
        let mut store = KubeconfigStore::empty();
        store.path = dir.join("config");
        store
            .ensure_context("demo", "https://127.0.0.1:1", "A", "B", "C")
            .unwrap();
        assert!(store.remove_context("demo"));
        assert!(!store.verify_context("kind-demo"));
        assert!(store.current_context().is_none());
        assert!(store.config().clusters.is_empty());
        assert!(store.config().auth_infos.is_empty());
        assert!(store.config().contexts.is_empty());
        // Removing again is a no-op.
        assert!(!store.remove_context("demo"));
    }

    #[test]
    fn remove_context_leaves_other_entries_and_current() {
        let dir = TempDir::new("remove2");
        let mut store = KubeconfigStore::from_yaml(dir.path.clone(), SYNTHETIC).unwrap();
        store
            .ensure_context("demo", "https://127.0.0.1:1", "A", "B", "C")
            .unwrap();
        assert!(store.remove_context("demo"));
        assert!(store.verify_context("other-context"));
        assert_eq!(store.current_context(), None, "current-context cleared");
        assert_eq!(store.config().clusters.len(), 1);
    }

    #[test]
    fn save_is_atomic_and_backs_up() {
        let dir = TempDir::new("save");
        let path = dir.join("config");
        let mut store = KubeconfigStore::load_from(&path).unwrap();
        store
            .ensure_context("one", "https://127.0.0.1:1", "A", "B", "C")
            .unwrap();
        store.save().unwrap();
        assert!(path.is_file());
        // No backup for the first save.
        assert!(!dir.join("config.bak").is_file());

        store
            .ensure_context("two", "https://127.0.0.1:2", "D", "E", "F")
            .unwrap();
        store.save().unwrap();
        let bak = dir.join("config.bak");
        assert!(bak.is_file(), "second save must create a backup");
        let bak_text = std::fs::read_to_string(&bak).unwrap();
        assert!(bak_text.contains("kind-one"), "{bak_text}");
        assert!(!bak_text.contains("kind-two"), "{bak_text}");

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("kind-one") && text.contains("kind-two"));

        // Round-trip: loading the saved file preserves everything.
        let reloaded = KubeconfigStore::load_from(&path).unwrap();
        assert!(reloaded.verify_context("kind-one"));
        assert!(reloaded.verify_context("kind-two"));
    }

    #[test]
    fn kind_context_name_prefix() {
        assert_eq!(KubeconfigStore::kind_context_name("demo"), "kind-demo");
        assert_eq!(KubeconfigStore::kind_context_name("a-1"), "kind-a-1");
    }

    #[cfg(unix)]
    #[test]
    fn saved_kubeconfig_is_owner_only_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new("perms");
        let path = dir.join("config");
        let mut store = KubeconfigStore::load_from(&path).unwrap();
        store
            .ensure_context("demo", "https://127.0.0.1:1", "A", "B", "C")
            .unwrap();
        store.save().unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "kubeconfig must be owner-only");
    }

    #[test]
    fn from_yaml_empty_document_yields_empty_store() {
        let dir = TempDir::new("fromyaml");
        let store = KubeconfigStore::from_yaml(dir.path.clone(), "   \n").unwrap();
        assert!(store.config().clusters.is_empty());
    }

    #[test]
    fn load_file_without_trailing_newline_works() {
        let dir = TempDir::new("nonewline");
        let path = dir.join("config");
        std::fs::write(&path, SYNTHETIC.trim_end()).unwrap();
        let store = KubeconfigStore::load_from(&path).unwrap();
        assert!(store.verify_context("other-context"));
        assert_eq!(store.current_context(), Some("other-context"));
    }

    #[test]
    fn load_from_directory_is_error() {
        let dir = TempDir::new("dirpath");
        let err = KubeconfigStore::load_from(&dir.path).unwrap_err();
        match err {
            crate::CoreError::Kubeconfig(KubeconfigError::Io(source)) => {
                assert!(
                    source.kind() == std::io::ErrorKind::IsADirectory
                        || source.raw_os_error().is_some(),
                    "expected an io error, got {source:?}"
                );
            }
            other => panic!("expected Kubeconfig Io error, got {other:?}"),
        }
    }

    #[test]
    fn ensure_context_preserves_unknown_keys_on_save() {
        let dir = TempDir::new("unknown-add");
        let path = dir.join("config");
        let with_unknown = r#"apiVersion: v1
kind: Config
clusters: []
contexts: []
users: []
custom-top:
  keep: me
"#;
        std::fs::write(&path, with_unknown).unwrap();
        let mut store = KubeconfigStore::load_from(&path).unwrap();
        store
            .ensure_context("demo", "https://127.0.0.1:1", "A", "B", "C")
            .unwrap();
        store.save().unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("custom-top"), "{text}");
        assert!(text.contains("keep: me"), "{text}");
        assert!(text.contains("kind-demo"), "{text}");

        let reloaded = KubeconfigStore::load_from(&path).unwrap();
        assert!(reloaded.verify_context("kind-demo"));
        assert_eq!(reloaded.current_context(), Some("kind-demo"));
    }

    #[test]
    fn remove_context_not_current_keeps_current() {
        let dir = TempDir::new("keepcurrent");
        let mut store = KubeconfigStore::from_yaml(dir.path.clone(), SYNTHETIC).unwrap();
        store
            .ensure_context("demo", "https://127.0.0.1:1", "A", "B", "C")
            .unwrap();
        store.set_current_context("other-context");
        assert!(store.remove_context("demo"));
        assert_eq!(
            store.current_context(),
            Some("other-context"),
            "unrelated current context must be kept"
        );
        assert!(store.verify_context("other-context"));
        assert!(!store.verify_context("kind-demo"));
    }
}
