//! Command/event buses between the UI (eframe thread) and the core worker
//! (background tokio thread).
//!
//! Both channels are crossbeam and bounded: the UI enqueues commands with
//! `try_send` (a full bus shows a warning badge instead of blocking) and
//! drains events with `try_iter` every frame. Long-running operations carry
//! a generation counter ([`OpGen`]); the UI bumps it per new request and
//! drops any event whose generation is stale, so output of a replaced
//! operation can never clobber newer state.

use std::path::PathBuf;

use crossbeam_channel::{Receiver, Sender, bounded};
use kindboard_core::{
    ClusterSpec, DockerDaemonState, InstallEvent, LogSource, ProvisionEvent, ReconcileReport,
    ToolId, ToolStatus, TopologyGraph,
};

/// Command queue capacity (UI → core).
pub const CMD_BUS_CAP: usize = 64;
/// Event stream capacity (core → UI).
pub const EVENT_BUS_CAP: usize = 256;

/// Generation tag of a long-running operation (create, recreate, destroy,
/// topology snapshots, log streams). See module docs for drop semantics.
pub type OpGen = u64;

/// Live state of one kind cluster as probed from the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClusterLiveStatus {
    /// Node containers answer; `node_count` is control-plane + workers.
    Running { node_count: usize },
    /// The cluster exists but has no nodes yet (mid-creation).
    Creating,
    /// `kind` has no record of the cluster (deleted externally, or
    /// starting up).
    Absent,
}

/// Whether the kubeconfig holds the cluster's context, and whether it is
/// the current-context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextStatus {
    /// Context exists and is the active one.
    Current,
    /// Context exists but another context is active.
    Present,
    /// No context for this cluster in the kubeconfig.
    Absent,
}

/// Commands the UI enqueues for the core worker.
#[derive(Debug)]
pub enum CoreCommand {
    /// Re-run `kind get clusters` + status probes + reconcile against the
    /// stored records.
    Reconcile,
    /// Detect all registry tools.
    DetectTools,
    /// Probe the docker daemon.
    CheckDockerDaemon,
    /// Install one tool (package manager first, binary fallback).
    InstallTool {
        /// Tool to install.
        id: ToolId,
    },
    /// Create a cluster from a spec, streaming [`ProvisionEvent`]s.
    CreateCluster {
        /// Validated spec.
        spec: Box<ClusterSpec>,
        /// Generation for stale-event dropping.
        op_gen: OpGen,
    },
    /// Delete a cluster and clean up kubeconfig + stored record.
    DestroyCluster {
        /// Cluster name.
        name: String,
        /// Generation for stale-event dropping.
        op_gen: OpGen,
    },
    /// Guided recreate (scale workers): destroy + create from the new spec.
    RecreateCluster {
        /// Cluster name.
        name: String,
        /// Spec with the new values (e.g. `worker_count`).
        spec: Box<ClusterSpec>,
        /// Generation for stale-event dropping.
        op_gen: OpGen,
    },
    /// Cancel the running create/destroy/recreate operation of a cluster.
    CancelOp {
        /// Cluster name.
        name: String,
    },
    /// Verify the kubeconfig context state for a cluster.
    CheckContext {
        /// Cluster name.
        name: String,
    },
    /// `kind get kubeconfig` + merge into the kubeconfig + set current.
    EnsureContext {
        /// Cluster name.
        name: String,
    },
    /// `kind export logs` into a directory.
    ExportLogs {
        /// Cluster name.
        name: String,
        /// Destination directory.
        dest: PathBuf,
    },
    /// One topology snapshot.
    FetchTopology {
        /// Cluster name.
        name: String,
        /// Generation for stale-event dropping.
        op_gen: OpGen,
    },
    /// Enable/disable periodic topology snapshots.
    SetAutoRefresh {
        /// Cluster name.
        name: String,
        /// Poll interval in seconds (clamped to ≥ 2).
        interval_secs: u64,
        /// Whether polling should run.
        enabled: bool,
        /// Generation; events only accepted while this is the active
        /// auto-refresh generation.
        op_gen: OpGen,
    },
    /// Read/stream logs for a cluster. Replaces any running stream.
    StartLogs {
        /// Cluster name.
        name: String,
        /// Which source (docker node container vs kubectl pod).
        source: LogSource,
        /// Follow the stream instead of a one-shot tail.
        follow: bool,
        /// Tail line count for one-shot reads.
        tail: Option<u32>,
        /// Generation for stale-event dropping.
        op_gen: OpGen,
    },
    /// Stop the running log stream of a cluster (no-op when none runs).
    StopLogs {
        /// Cluster name.
        name: String,
    },
    /// Open k9s for a cluster in a new terminal window (fire-and-forget).
    OpenK9s {
        /// Cluster name (context: `kind-<name>`).
        name: String,
    },
    /// Stop the worker loop (sent on app exit).
    Shutdown,
}

/// Events the core worker pushes back to the UI.
#[derive(Debug)]
pub enum CoreEvent {
    /// `kind get clusters` + reconcile finished.
    ReconcileDone {
        /// Classification (managed/adopted/missing).
        result: Box<Result<ReconcileReport, String>>,
    },
    /// Live cluster names as reported by `kind get clusters`.
    LiveClusters {
        /// Cluster names.
        names: Vec<String>,
    },
    /// Probing status of one live cluster.
    ClusterStatus {
        /// Cluster name.
        name: String,
        /// Live state.
        status: ClusterLiveStatus,
    },
    /// One tool finished detecting.
    DetectedTool {
        /// Tool id.
        id: ToolId,
        /// Detection outcome.
        result: Result<ToolStatus, String>,
    },
    /// All tools finished detecting.
    ToolsDetectDone,
    /// Docker daemon probe finished.
    DockerDaemon {
        /// Daemon state.
        state: DockerDaemonState,
    },
    /// Progress of a running tool install.
    InstallEvent {
        /// Tool id.
        id: ToolId,
        /// Streamed install event.
        event: InstallEvent,
    },
    /// A tool install finished.
    InstallDone {
        /// Tool id.
        id: ToolId,
        /// Post-install status (re-detected) or the failure.
        result: Result<ToolStatus, String>,
    },
    /// Progress of a create/destroy/recreate operation.
    Provision {
        /// Cluster name.
        name: String,
        /// Generation.
        op_gen: OpGen,
        /// Streamed provision event.
        event: ProvisionEvent,
    },
    /// A create/destroy/recreate operation finished. The UI matches the
    /// panel kind it opened for this cluster to decide follow-ups
    /// (refresh, open/close tab).
    ProvisionDone {
        /// Cluster name.
        name: String,
        /// Generation.
        op_gen: OpGen,
        /// Outcome.
        result: Result<(), String>,
    },
    /// Kubeconfig context state of a cluster (reply to
    /// [`CoreCommand::CheckContext`]).
    ContextState {
        /// Cluster name.
        name: String,
        /// Context state.
        state: ContextStatus,
    },
    /// `kind get kubeconfig` was merged and set current.
    ContextEnsured {
        /// Cluster name.
        name: String,
        /// Outcome.
        result: Result<(), String>,
    },
    /// `kind export logs` finished.
    LogsExported {
        /// Cluster name.
        name: String,
        /// Destination directory.
        dest: PathBuf,
        /// Outcome.
        result: Result<(), String>,
    },
    /// One topology snapshot result.
    Topology {
        /// Cluster name.
        name: String,
        /// Generation.
        op_gen: OpGen,
        /// Snapshot or error.
        result: Box<Result<TopologyGraph, String>>,
    },
    /// One log line of a stream.
    LogLine {
        /// Cluster name.
        name: String,
        /// Generation.
        op_gen: OpGen,
        /// The line (no trailing newline).
        line: String,
    },
    /// A log stream ended (process exit, error, or stop).
    LogEnded {
        /// Cluster name.
        name: String,
        /// Generation.
        op_gen: OpGen,
        /// Outcome; `Err` carries the failure reason.
        result: Result<(), String>,
    },
    /// The event bus overflowed and some progress lines were dropped
    /// (outcome events are retried briefly by the worker before dropping).
    BusFull,
    /// A non-fatal error with context (shown as a dismissable banner).
    Error {
        /// Where it happened (e.g. "reconcile").
        context: String,
        /// Human-readable message.
        message: String,
    },
    /// An informational notice (shown as a dismissable banner, not an
    /// error).
    Notice {
        /// Human-readable message.
        message: String,
    },
}

/// The two buses, grouped for convenient startup wiring.
pub struct Buses {
    /// UI → core commands.
    pub cmd_tx: Sender<CoreCommand>,
    /// Worker-side command receiver.
    pub cmd_rx: Receiver<CoreCommand>,
    /// Core → UI events.
    pub event_tx: Sender<CoreEvent>,
    /// UI-side event receiver.
    pub event_rx: Receiver<CoreEvent>,
}

impl Default for Buses {
    fn default() -> Self {
        Self::new()
    }
}

impl Buses {
    /// Create bounded channel pairs.
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = bounded(CMD_BUS_CAP);
        let (event_tx, event_rx) = bounded(EVENT_BUS_CAP);
        Buses {
            cmd_tx,
            cmd_rx,
            event_tx,
            event_rx,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::{emit, emit_important};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// Drain `rx` into a shared Vec until `stop` is set.
    fn spawn_drainer(
        rx: Receiver<CoreEvent>,
        collected: Arc<Mutex<Vec<CoreEvent>>>,
        stop: Arc<std::sync::atomic::AtomicBool>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                if let Ok(event) = rx.recv_timeout(Duration::from_millis(20)) {
                    collected.lock().unwrap().push(event);
                }
            }
        })
    }

    #[test]
    fn emit_drops_progress_when_full_and_delivers_notice() {
        let (tx, rx) = bounded(EVENT_BUS_CAP);
        for i in 0..EVENT_BUS_CAP {
            tx.try_send(CoreEvent::Error {
                context: "fill".to_string(),
                message: i.to_string(),
            })
            .unwrap();
        }
        let collected = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let drainer = spawn_drainer(rx, collected.clone(), stop.clone());

        // The bus is full: the progress line is dropped, the notice waits
        // for the drainer to free a slot and is then delivered.
        emit(&tx, CoreEvent::ToolsDetectDone);

        std::thread::sleep(Duration::from_millis(500));
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        drainer.join().unwrap();

        let events = collected.lock().unwrap();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, CoreEvent::BusFull)),
            "BusFull notice must be delivered, got {:?} events",
            events.len()
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, CoreEvent::ToolsDetectDone)),
            "full-bus progress lines must be dropped"
        );
        assert_eq!(
            events
                .iter()
                .filter(
                    |event| matches!(event, CoreEvent::Error { context, .. } if context == "fill")
                )
                .count(),
            EVENT_BUS_CAP,
            "all filler events must be drained"
        );
    }

    #[test]
    fn emit_important_delivers_outcome_through_full_bus() {
        let (tx, rx) = bounded(EVENT_BUS_CAP);
        for i in 0..EVENT_BUS_CAP {
            tx.try_send(CoreEvent::Error {
                context: "fill".to_string(),
                message: i.to_string(),
            })
            .unwrap();
        }
        let collected = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let drainer = spawn_drainer(rx, collected.clone(), stop.clone());

        // Outcome events briefly wait for a free slot instead of being lost.
        emit_important(&tx, CoreEvent::ToolsDetectDone);

        std::thread::sleep(Duration::from_millis(300));
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        drainer.join().unwrap();

        let events = collected.lock().unwrap();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, CoreEvent::ToolsDetectDone)),
            "outcome events must get through a full bus (grace wait)"
        );
    }
}
