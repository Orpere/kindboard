//! Error taxonomy for `kindboard-core`.
//!
//! A single enum tree rooted at [`CoreError`]. Each subsystem defines its own
//! error type (nested here) and converts into `CoreError` via `#[from]`, so a
//! caller can match either the coarse variant or the precise subsystem error
//! without losing context (`thiserror` source chaining).
//!
//! Design rules (see `docs/architecture.md` §4):
//! - Subprocess failures are [`ExecError::Command`] / [`ExecError::Timeout`] /
//!   [`ExecError::Cancelled`] with a bounded `stderr_tail` (last 2 KiB).
//! - Nothing is swallowed: every fallible operation surfaces an error.
//! - Messages are UI-friendly and never expose secrets.

use std::path::PathBuf;
use std::time::Duration;

/// Convenience result alias used across the crate.
pub type Result<T> = std::result::Result<T, CoreError>;

/// Render an optional exit code for error messages.
fn exit_code_text(code: Option<i32>) -> String {
    match code {
        Some(code) => format!("code {code}"),
        None => "a signal".to_string(),
    }
}

/// Root error type of `kindboard-core`.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// Invalid configuration content (env vars, files, or values).
    #[error("configuration error: {0}")]
    Config(String),

    /// Subprocess runner error (`exec` module).
    #[error(transparent)]
    Exec(#[from] ExecError),

    /// kind/docker/kubectl/helm/cilium invocation or output-parsing error
    /// (`kindctl` module).
    #[error(transparent)]
    Kind(#[from] KindError),

    /// Dependency detection/installation error (`deps` module).
    #[error(transparent)]
    Deps(#[from] DepsError),

    /// Kubeconfig load/merge/save error (`kubeconfig` module).
    #[error(transparent)]
    Kubeconfig(#[from] KubeconfigError),

    /// Provisioning sequence error (`provision` module).
    #[error(transparent)]
    Provision(#[from] ProvisionError),

    /// Kubernetes API / topology error (`k8s` module).
    #[error(transparent)]
    K8s(#[from] K8sError),

    /// Persistence error (`state` module).
    #[error(transparent)]
    State(#[from] StateError),

    /// Filesystem error with the affected path.
    #[error("io error at {path}: {source}")]
    Io {
        /// Path of the failing operation.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// Cluster spec failed validation (`spec::validate`).
    #[error("invalid cluster spec: {0}")]
    InvalidSpec(String),

    /// Attempt to create a cluster whose name already exists.
    #[error("cluster already exists: {0}")]
    ClusterExists(String),

    /// A named cluster does not exist.
    #[error("cluster not found: {0}")]
    ClusterNotFound(String),

    /// Live cluster properties differ from the stored spec.
    #[error("state drift detected for {cluster}: {detail}")]
    Drift {
        /// Cluster name.
        cluster: String,
        /// Human-readable description of the drift.
        detail: String,
    },
}

/// Subprocess execution errors (`exec` module).
#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    /// The process exited with a non-zero code. `stderr_tail` is the last
    /// 2 KiB of stderr.
    #[error(
        "subprocess `{prog}` exited with {}: {stderr_tail}",
        exit_code_text(*code)
    )]
    Command {
        /// Program that was run (argv[0]).
        prog: String,
        /// Exit code, if the process exited normally.
        code: Option<i32>,
        /// Tail of stderr (bounded), empty when the process wrote none.
        stderr_tail: String,
    },

    /// The process exceeded its deadline and was killed (TERM, then KILL
    /// after a grace period).
    #[error("command timed out after {elapsed:?}: {prog}")]
    Timeout {
        /// Program that was run (argv[0]).
        prog: String,
        /// How long the command was allowed to run.
        elapsed: Duration,
    },

    /// The operation was cancelled by the caller (same TERM→KILL ladder as
    /// timeouts).
    #[error("command cancelled: {prog}")]
    Cancelled {
        /// Program that was being run (argv[0]).
        prog: String,
    },

    /// The process could not be spawned.
    #[error("failed to spawn `{prog}`: {source}")]
    Spawn {
        /// Program that was to be run (argv[0]).
        prog: String,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// Output pipes failed while reading (e.g. closed stdout).
    #[error("failed to read output of `{prog}`: {source}")]
    Output {
        /// Program being read from (argv[0]).
        prog: String,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
}

/// kindctl errors: command construction and output parsing.
#[derive(Debug, thiserror::Error)]
pub enum KindError {
    /// Output of a command could not be parsed.
    #[error("failed to parse output of `{command}`: {detail}")]
    ParseOutput {
        /// Human-readable command description.
        command: &'static str,
        /// What went wrong while parsing.
        detail: String,
    },

    /// A node-name line from `kind get nodes` was not usable.
    #[error("unexpected {command} output line {line:?}: {detail}")]
    UnexpectedOutput {
        /// Human-readable command description.
        command: &'static str,
        /// The offending line (lossy-converted).
        line: String,
        /// What went wrong.
        detail: String,
    },
}

/// Dependency tool errors (`deps` module).
#[derive(Debug, thiserror::Error)]
pub enum DepsError {
    /// A tool binary was not found on PATH.
    #[error("dependency `{0}` not found on PATH")]
    NotFound(String),

    /// Version detection ran but produced no parseable version.
    #[error("could not parse version of `{tool}` from: {output}")]
    UnparseableVersion {
        /// Tool display name.
        tool: String,
        /// The (lossy) output that failed to parse.
        output: String,
    },

    /// Detection itself failed (non-zero exit or spawn failure).
    #[error("detection failed for `{tool}`: {reason}")]
    DetectFailed {
        /// Tool display name.
        tool: String,
        /// Underlying reason.
        reason: String,
    },

    /// No install recipe is applicable on this platform.
    #[error("no install recipe for `{tool}` on this platform: {reason}")]
    NoInstallRecipe {
        /// Tool display name.
        tool: String,
        /// Why nothing applies (missing package manager, no binary fallback).
        reason: String,
    },

    /// An install step failed.
    #[error("install step `{step}` failed for `{tool}`: {reason}")]
    InstallFailed {
        /// Tool display name.
        tool: String,
        /// Human-readable description of the failing step.
        step: String,
        /// Underlying reason.
        reason: String,
    },

    /// A downloaded artifact failed SHA-256 verification.
    #[error("SHA-256 verification failed for `{tool}` download {path}: expected {expected_sha256}")]
    ChecksumMismatch {
        /// Tool display name.
        tool: String,
        /// Downloaded file path.
        path: PathBuf,
        /// Expected hex digest.
        expected_sha256: &'static str,
    },
}

/// Kubeconfig errors (`kubeconfig` module).
#[derive(Debug, thiserror::Error)]
pub enum KubeconfigError {
    /// Underlying kube-rs kubeconfig error.
    #[error(transparent)]
    Kube(#[from] kube::config::KubeconfigError),

    /// YAML (de)serialization failure.
    #[error(transparent)]
    Yaml(#[from] serde_yaml::Error),

    /// Filesystem failure while loading/saving.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// A server URL was not a valid URL.
    #[error("invalid server url `{url}`: {reason}")]
    InvalidServer {
        /// The offending URL.
        url: String,
        /// Why it is invalid.
        reason: String,
    },

    /// A requested context does not exist in the loaded kubeconfig.
    #[error("context `{0}` not found in kubeconfig")]
    ContextNotFound(String),
}

/// Provisioning sequence errors (`provision` module).
#[derive(Debug, thiserror::Error)]
pub enum ProvisionError {
    /// A single provisioning step failed; the inner error carries the root
    /// cause (boxed to break the recursive `CoreError ↔ ProvisionError`
    /// size cycle).
    #[error("provisioning step `{id}` failed: {source}")]
    StepFailed {
        /// Id of the failing step.
        id: String,
        /// Root cause of the failure.
        #[source]
        source: Box<CoreError>,
    },

    /// A verification step (post-condition of a step) failed.
    #[error("verification of step `{id}` failed: {detail}")]
    VerifyFailed {
        /// Id of the step whose verification failed.
        id: String,
        /// Human-readable detail.
        detail: String,
    },

    /// The step DAG contained a cycle or a dependency on an unknown step.
    #[error("invalid provisioning plan: {0}")]
    InvalidPlan(String),

    /// A downloaded manifest failed SHA-256 verification (integrity).
    #[error(
        "SHA-256 verification failed for downloaded manifest {path}: expected {expected_sha256}"
    )]
    ChecksumMismatch {
        /// Downloaded file path.
        path: PathBuf,
        /// Expected hex digest.
        expected_sha256: &'static str,
    },
}

impl From<CoreError> for ProvisionError {
    fn from(source: CoreError) -> Self {
        ProvisionError::StepFailed {
            id: "<root>".to_string(),
            source: Box::new(source),
        }
    }
}

/// Kubernetes API / topology errors (`k8s` module).
#[derive(Debug, thiserror::Error)]
pub enum K8sError {
    /// Underlying kube-rs error.
    #[error(transparent)]
    Kube(#[from] kube::Error),

    /// Building the client failed for another reason.
    #[error("k8s client error: {0}")]
    Client(String),

    /// The requested kubeconfig context is not usable.
    #[error("k8s context `{0}` is not usable: {1}")]
    Context(String, String),
}

/// Persistence errors (`state` module).
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// JSON (de)serialization failure.
    #[error(transparent)]
    Json(#[from] serde_json::Error),

    /// A stored record failed schema-level validation on load.
    #[error("stored state for `{0}` is invalid: {1}")]
    InvalidRecord(String, String),

    /// A cluster name is not a valid file-name-safe label.
    #[error("cluster name `{0}` is not a valid state key: {1}")]
    InvalidName(String, String),
}
