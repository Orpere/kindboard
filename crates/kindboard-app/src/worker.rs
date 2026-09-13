//! Core worker: owns the tokio runtime on a dedicated background thread,
//! receives [`CoreCommand`]s from the UI over a bounded crossbeam channel,
//! executes them exclusively through `kindboard-core`, and streams
//! [`CoreEvent`]s back.
//!
//! Concurrency model:
//! - The worker loop itself only dispatches; every operation runs as a
//!   spawned task on the multi-thread runtime.
//! - Each long-running op slot (create/destroy/recreate/logs/topology per
//!   cluster, installs per tool) owns a [`CancellationToken`]; issuing a
//!   new command for the slot cancels the previous token, so the old
//!   subprocess is killed (TERM → KILL ladder inside core's exec module).
//! - Events carry the op generation (`op_gen`) issued by the UI; the UI drops
//!   stale ones. Generation tags therefore bound *what is rendered* while
//!   tokens bound *what is running*.
//! - The event bus is drained by the UI every frame; progress lines are
//!   dropped when the bus is full (with a [`CoreEvent::BusFull`] notice),
//!   results briefly wait (bounded) so outcomes are never silently lost.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use kindboard_core::{
    self as core, ClusterRecord, ClusterSpec, Cmd, CmdOutput, DataDir, InstallEvent, K8sClient,
    KindCommand, KubeconfigStore, LogRing, LogSource, ProvisionEvent, ToolId, TopologyGraph,
};

use crate::bus::{ClusterLiveStatus, ContextStatus, CoreCommand, CoreEvent, OpGen};

/// Probe timeout for per-cluster status checks (short; the probe only asks
/// `kind get nodes`).
const STATUS_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Backstop deadline for a full detection run (worst legitimate case: 8
/// tools × the 30s per-tool detect timeout, plus margin).
const DETECT_ALL_DEADLINE: Duration = Duration::from_secs(300);
/// How long a full event bus may hold a *result* event before it is
/// dropped (progress lines are always dropped immediately).
const EVENT_SEND_GRACE: Duration = Duration::from_millis(1000);
/// How long the BusFull notice itself may wait for the UI to drain a frame
/// (short: it runs on the worker and must not stall progress emission).
const BUS_FULL_NOTICE_GRACE: Duration = Duration::from_millis(200);
/// Log-ring drain period for follow streams (ms).
const LOG_PUMP_MS: u64 = 120;
/// Minimum auto-refresh interval (s).
const MIN_AUTO_REFRESH_SECS: u64 = 2;

/// The worker's file-system environment: state dir + kubeconfig path.
/// Production uses the standard locations; tests point both at temp dirs
/// so they never touch the user's `~/.local/share/kindboard` or
/// `~/.kube/config`.
#[derive(Debug, Clone)]
pub struct WorkerEnv {
    /// State directory (specs, settings, tmp).
    pub data_dir: DataDir,
    /// Kubeconfig file the worker reads/merges/removes contexts from.
    pub kubeconfig_path: PathBuf,
}

impl WorkerEnv {
    /// Standard user locations.
    pub fn standard() -> Result<Self, String> {
        let data_dir = DataDir::standard().map_err(|err| err.to_string())?;
        Ok(WorkerEnv {
            data_dir,
            kubeconfig_path: kindboard_core::default_path(),
        })
    }

    /// Explicit locations (tests, portable installs).
    pub fn new(data_dir: DataDir, kubeconfig_path: impl Into<PathBuf>) -> Self {
        WorkerEnv {
            data_dir,
            kubeconfig_path: kubeconfig_path.into(),
        }
    }
}

/// Spawn the worker thread with the standard environment. Returns a handle
/// so `main` can join it (after sending [`CoreCommand::Shutdown`]) before
/// the process exits.
pub fn spawn(
    cmd_rx: Receiver<CoreCommand>,
    event_tx: Sender<CoreEvent>,
) -> std::thread::JoinHandle<()> {
    spawn_with_env(cmd_rx, event_tx, WorkerEnv::standard)
}

/// Spawn the worker thread with an explicit environment (used by e2e
/// tests).
pub fn spawn_with_env(
    cmd_rx: Receiver<CoreCommand>,
    event_tx: Sender<CoreEvent>,
    env_builder: impl FnOnce() -> Result<WorkerEnv, String> + Send + 'static,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("kindboard-core".to_string())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("failed to build the tokio runtime");
            runtime.block_on(run_loop(cmd_rx, event_tx, env_builder));
        })
        .expect("failed to spawn the core worker thread")
}

/// The worker loop: dispatch commands, run ops as tasks, join finished
/// tasks.
async fn run_loop(
    cmd_rx: Receiver<CoreCommand>,
    event_tx: Sender<CoreEvent>,
    env_builder: impl FnOnce() -> Result<WorkerEnv, String>,
) {
    let env = match env_builder() {
        Ok(env) => {
            if let Err(err) = env.data_dir.clean_tmp() {
                emit_important(
                    &event_tx,
                    CoreEvent::Error {
                        context: "startup".to_string(),
                        message: format!("failed to clean tmp dir: {err}"),
                    },
                );
            }
            env
        }
        Err(err) => {
            emit_important(
                &event_tx,
                CoreEvent::Error {
                    context: "startup".to_string(),
                    message: format!("state directory unavailable: {err}"),
                },
            );
            return;
        }
    };

    // Bridge: crossbeam recv is blocking; hand commands to a tokio mpsc so
    // the async loop can select between commands and task joins.
    let (tokio_tx, mut tokio_rx) = tokio::sync::mpsc::unbounded_channel::<CoreCommand>();
    std::thread::spawn(move || {
        while let Ok(cmd) = cmd_rx.recv() {
            if tokio_tx.send(cmd).is_err() {
                break;
            }
        }
    });

    let mut tasks: JoinSet<()> = JoinSet::new();
    let mut cancels: HashMap<String, CancellationToken> = HashMap::new();

    loop {
        tokio::select! {
            cmd = tokio_rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    CoreCommand::Shutdown => break,
                    CoreCommand::Reconcile => {
                        let tx = event_tx.clone();
                        let worker_env = env.clone();
                        tasks.spawn(async move { do_reconcile(tx, worker_env).await });
                    }
                    CoreCommand::DetectTools => {
                        let tx = event_tx.clone();
                        tasks.spawn(async move {
                            let result =
                                tokio::time::timeout(DETECT_ALL_DEADLINE, do_detect_all(tx.clone()))
                                    .await;
                            if result.is_err() {
                                // Backstop: even if a detect run stalls, the
                                // UI must un-stick. The UI-side watchdog
                                // re-issues detection on its own cadence.
                                log::warn!(
                                    "tool detection did not complete within {DETECT_ALL_DEADLINE:?}; emitting ToolsDetectDone"
                                );
                                emit_important(&tx, CoreEvent::ToolsDetectDone);
                            }
                        });
                    }
                    CoreCommand::CheckDockerDaemon => {
                        let tx = event_tx.clone();
                        tasks.spawn(async move {
                            let state = core::check_docker_daemon().await;
                            emit(&tx, CoreEvent::DockerDaemon { state });
                        });
                    }
                    CoreCommand::SetTheme { id } => {
                        let tx = event_tx.clone();
                        let worker_env = env.clone();
                        tasks.spawn(async move {
                            let result = worker_env
                                .data_dir
                                .load_settings()
                                .map(|mut settings| {
                                    settings.theme = Some(id);
                                    settings
                                })
                                .and_then(|settings| worker_env.data_dir.save_settings(&settings));
                            if let Err(err) = result {
                                emit(
                                    &tx,
                                    CoreEvent::Notice {
                                        message: format!("could not save theme: {err}"),
                                    },
                                );
                            }
                        });
                    }
                    CoreCommand::InstallTool { id } => {
                        let token = op_token(&mut cancels, &format!("install:{id}"));
                        let tx = event_tx.clone();
                        tasks.spawn(async move { do_install(id, tx, token).await });
                    }
                    CoreCommand::CreateCluster { spec, op_gen } => {
                        let name = spec.name.clone();
                        let token = op_token(&mut cancels, &format!("create:{name}"));
                        let tx = event_tx.clone();
                        let worker_env = env.clone();
                        tasks.spawn(async move { do_create(*spec, op_gen, worker_env, tx, token).await });
                    }
                    CoreCommand::DestroyCluster { name, op_gen } => {
                        let token = op_token(&mut cancels, &format!("destroy:{name}"));
                        let tx = event_tx.clone();
                        let worker_env = env.clone();
                        tasks.spawn(async move { do_destroy(name, op_gen, worker_env, tx, token).await });
                    }
                    CoreCommand::RecreateCluster { name, spec, op_gen } => {
                        let token = op_token(&mut cancels, &format!("recreate:{name}"));
                        let tx = event_tx.clone();
                        let worker_env = env.clone();
                        tasks.spawn(async move { do_recreate(name, *spec, op_gen, worker_env, tx, token).await });
                    }
                    CoreCommand::CancelOp { name } => {
                        for key in [
                            format!("create:{name}"),
                            format!("destroy:{name}"),
                            format!("recreate:{name}"),
                        ] {
                            if let Some(token) = cancels.remove(&key) {
                                token.cancel();
                            }
                        }
                    }
                    CoreCommand::CheckContext { name } => {
                        let tx = event_tx.clone();
                        let worker_env = env.clone();
                        tasks.spawn(async move { do_check_context(name, worker_env, tx).await });
                    }
                    CoreCommand::EnsureContext { name } => {
                        let tx = event_tx.clone();
                        let worker_env = env.clone();
                        tasks.spawn(async move { do_ensure_context(name, worker_env, tx).await });
                    }
                    CoreCommand::ExportLogs { name, dest } => {
                        let tx = event_tx.clone();
                        tasks.spawn(async move { do_export_logs(name, dest, tx).await });
                    }
                    CoreCommand::FetchTopology { name, op_gen } => {
                        let tx = event_tx.clone();
                        let kubeconfig_path = env.kubeconfig_path.clone();
                        tasks.spawn(async move {
                            let result = fetch_topology(&name, &kubeconfig_path).await;
                            emit(
                                &tx,
                                CoreEvent::Topology {
                                    name,
                                    op_gen,
                                    result: Box::new(result),
                                },
                            );
                        });
                    }
                    CoreCommand::SetAutoRefresh { name, interval_secs, enabled, op_gen } => {
                        let key = format!("topo:{name}");
                        if let Some(token) = cancels.remove(&key) {
                            token.cancel();
                        }
                        if enabled {
                            let token = CancellationToken::new();
                            cancels.insert(key, token.clone());
                            let tx = event_tx.clone();
                            let kubeconfig_path = env.kubeconfig_path.clone();
                            tasks.spawn(async move {
                                do_auto_refresh(name, interval_secs, op_gen, tx, token, kubeconfig_path)
                                    .await;
                            });
                        }
                    }
                    CoreCommand::StartLogs { name, source, follow, tail, op_gen } => {
                        let token = op_token(&mut cancels, &format!("logs:{name}"));
                        let tx = event_tx.clone();
                        tasks.spawn(async move {
                            do_logs(name, source, follow, tail, op_gen, tx, token).await;
                        });
                    }
                    CoreCommand::StopLogs { name } => {
                        if let Some(token) = cancels.remove(&format!("logs:{name}")) {
                            token.cancel();
                        }
                    }
                    CoreCommand::OpenK9s { name } => {
                        open_k9s(&name, &event_tx);
                    }
                }
            }
            result = tasks.join_next(), if !tasks.is_empty() => {
                // Never swallow task failures silently: a panicked/aborted
                // task must leave a trace (and the UI watchdog re-drives
                // detection when a run ends without ToolsDetectDone).
                if let Some(Err(err)) = result {
                    log::warn!("worker task failed: {err}");
                }
            }
        }
    }

    // Shutdown: cancel every running op, then abort the tasks. Cancelling
    // first lets core's exec module run its TERM → KILL ladder on the
    // subprocess groups.
    drop(cancels);
    tasks.abort_all();
}

/// Remove and cancel the previous token for an op slot, then mint a fresh
/// one. The old subprocess dies; the new op runs under the new token.
fn op_token(cancels: &mut HashMap<String, CancellationToken>, key: &str) -> CancellationToken {
    let token = CancellationToken::new();
    if let Some(old) = cancels.insert(key.to_string(), token.clone()) {
        old.cancel();
    }
    token
}

/// Open k9s for a cluster in a new terminal window (fire-and-forget: the
/// terminal outlives the app).
fn open_k9s(name: &str, event_tx: &Sender<CoreEvent>) {
    let context = KubeconfigStore::kind_context_name(name);
    let Some(terminal) = detect_terminal() else {
        emit_important(
            event_tx,
            CoreEvent::Error {
                context: "k9s".to_string(),
                message:
                    "no terminal emulator found (set $TERMINAL or install foot/kitty/alacritty)"
                        .to_string(),
            },
        );
        return;
    };
    let argv = k9s_argv(&terminal.to_string_lossy(), &context);
    match std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            // Reap the detached child in the background (fire-and-forget:
            // the terminal outlives the app).
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            emit_important(
                event_tx,
                CoreEvent::Notice {
                    message: format!("opened k9s for {name} (context {context}) in a new terminal"),
                },
            );
        }
        Err(err) => emit_important(
            event_tx,
            CoreEvent::Error {
                context: "k9s".to_string(),
                message: format!("failed to open {}: {err}", terminal.display()),
            },
        ),
    }
}

/// Terminals tried in order (env `TERMINAL` wins when set).
const TERMINAL_CANDIDATES: &[&str] = &[
    "foot",
    "kitty",
    "alacritty",
    "konsole",
    "gnome-terminal",
    "xterm",
    "x-terminal-emulator",
];

/// Resolve a terminal emulator to launch k9s in: `$TERMINAL` first, then a
/// known-good candidate list (first one present on PATH).
fn detect_terminal() -> Option<PathBuf> {
    let paths = kindboard_core::deps::path_entries();
    if let Some(term) = std::env::var_os("TERMINAL") {
        let term = term.to_string_lossy();
        if !term.is_empty() {
            // $TERMINAL may be a bare name or a path.
            if let Some(found) = kindboard_core::deps::find_in_path(&paths, &term) {
                return Some(found);
            }
            let candidate = PathBuf::from(term.as_ref());
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    TERMINAL_CANDIDATES
        .iter()
        .find_map(|candidate| kindboard_core::deps::find_in_path(&paths, candidate))
}

/// Build the terminal argv that runs `k9s --context <context>`.
///
/// Most terminals accept `-e <program> <args...>` (xterm-compatible); kitty
/// takes the program directly (its short opts have no `-e`) and
/// gnome-terminal uses `--`.
fn k9s_argv(terminal: &str, context: &str) -> Vec<String> {
    let exec_flag = if terminal.ends_with("gnome-terminal") || terminal.ends_with("kitty") {
        "--"
    } else {
        "-e"
    };
    vec![
        terminal.to_string(),
        exec_flag.to_string(),
        "k9s".to_string(),
        "--context".to_string(),
        context.to_string(),
    ]
}

/// Send a droppable event (progress lines, notices).
pub(crate) fn emit(tx: &Sender<CoreEvent>, event: CoreEvent) {
    if tx.try_send(event).is_err() {
        // Bus full: the UI drains every frame, so this is transient; the
        // dropped event is a progress line, which is safe to lose. The
        // BusFull notice goes through a bounded wait so it is actually
        // delivered once the UI drains (a plain try_send here could never
        // succeed while the bus is still full).
        if let Err(crossbeam_channel::TrySendError::Full(notice)) = tx.try_send(CoreEvent::BusFull)
        {
            let _ = tx.send_timeout(notice, BUS_FULL_NOTICE_GRACE);
        }
    }
}

/// Send an outcome event; briefly wait when the bus is full so results are
/// not lost, then give up (never block the worker indefinitely).
pub(crate) fn emit_important(tx: &Sender<CoreEvent>, event: CoreEvent) {
    match tx.try_send(event) {
        Ok(()) => {}
        Err(crossbeam_channel::TrySendError::Full(event)) => {
            let _ = tx.send_timeout(event, EVENT_SEND_GRACE);
        }
        Err(crossbeam_channel::TrySendError::Disconnected(_)) => {}
    }
}

/// Run a command; map errors to display strings.
async fn run_cmd(command: KindCommand) -> Result<CmdOutput, String> {
    command.to_cmd().run().await.map_err(|err| err.to_string())
}

/// Run a command with cancellation attached.
async fn run_cmd_cancel(
    command: KindCommand,
    token: &CancellationToken,
) -> Result<CmdOutput, String> {
    command
        .to_cmd()
        .cancel(token.clone())
        .run()
        .await
        .map_err(|err| err.to_string())
}

/// `kind get clusters` + status probes + reconcile.
async fn do_reconcile(event_tx: Sender<CoreEvent>, env: WorkerEnv) {
    match run_cmd(KindCommand::KindGetClusters).await {
        Ok(output) => {
            let live = core::parse_kind_get_clusters(&output.stdout());
            emit(
                &event_tx,
                CoreEvent::LiveClusters {
                    names: live.clone(),
                },
            );
            for name in &live {
                let status = probe_status(name).await;
                emit(
                    &event_tx,
                    CoreEvent::ClusterStatus {
                        name: name.clone(),
                        status,
                    },
                );
            }
            match env.data_dir.reconcile(&live) {
                Ok(report) => emit_important(
                    &event_tx,
                    CoreEvent::ReconcileDone {
                        result: Box::new(Ok(report)),
                    },
                ),
                Err(err) => emit_important(
                    &event_tx,
                    CoreEvent::ReconcileDone {
                        result: Box::new(Err(err.to_string())),
                    },
                ),
            }
        }
        Err(err) => {
            emit_important(
                &event_tx,
                CoreEvent::ReconcileDone {
                    result: Box::new(Err(err)),
                },
            );
        }
    }
}

/// Probe the live state of one cluster via `kind get nodes`.
async fn probe_status(name: &str) -> ClusterLiveStatus {
    let command = KindCommand::KindGetNodes {
        cluster: name.to_string(),
    };
    match command.to_cmd().timeout(STATUS_PROBE_TIMEOUT).run().await {
        Ok(output) if output.success() => {
            let nodes = core::parse_kind_get_nodes(&output.stdout());
            if nodes.is_empty() {
                ClusterLiveStatus::Creating
            } else {
                ClusterLiveStatus::Running {
                    node_count: nodes.len(),
                }
            }
        }
        _ => ClusterLiveStatus::Absent,
    }
}

/// Detect every registry tool, emitting per-tool events. Tools are probed
/// concurrently so the whole run is bounded by the slowest single tool
/// (30 s timeout), not the sum of all eight — the UI watchdog thresholds
/// rely on this.
async fn do_detect_all(event_tx: Sender<CoreEvent>) {
    let mut tasks = Vec::new();
    for tool in core::registry() {
        let tx = event_tx.clone();
        tasks.push(tokio::spawn(async move {
            let result = core::detect(tool.id).await.map_err(|err| err.to_string());
            // Per-tool outcomes are results, not progress: they must never
            // be dropped under bus pressure or the UI shows stale data
            // after a refresh (TRACE-013).
            emit_important(
                &tx,
                CoreEvent::DetectedTool {
                    id: tool.id,
                    result,
                },
            );
        }));
    }
    for task in tasks {
        let _ = task.await;
    }
    emit_important(&event_tx, CoreEvent::ToolsDetectDone);
}

/// Install one tool, forwarding its streamed install events.
async fn do_install(id: ToolId, event_tx: Sender<CoreEvent>, token: CancellationToken) {
    let (plan_tx, mut plan_rx) = tokio::sync::mpsc::channel::<InstallEvent>(64);
    let forward_tx = event_tx.clone();
    let forward = tokio::spawn(async move {
        while let Some(event) = plan_rx.recv().await {
            emit(&forward_tx, CoreEvent::InstallEvent { id, event });
        }
    });
    let result = core::install_with_progress(id, Some(token), &plan_tx).await;
    drop(plan_tx);
    let _ = forward.await;
    emit_important(
        &event_tx,
        CoreEvent::InstallDone {
            id,
            result: result.map_err(|err| err.to_string()),
        },
    );
}

/// Create a cluster from a validated spec, streaming provision events.
async fn do_create(
    spec: ClusterSpec,
    op_gen: OpGen,
    env: WorkerEnv,
    event_tx: Sender<CoreEvent>,
    token: CancellationToken,
) {
    let name = spec.name.clone();

    // Idempotency pre-check (contracts §6): fail fast, no side effects.
    if let Ok(output) = run_cmd(KindCommand::KindGetClusters).await {
        let live = core::parse_kind_get_clusters(&output.stdout());
        if live.iter().any(|existing| existing == &name) {
            emit_important(
                &event_tx,
                CoreEvent::ProvisionDone {
                    name,
                    op_gen,
                    result: Err("a cluster with this name already exists".to_string()),
                },
            );
            return;
        }
    }

    match create_phase(&spec, op_gen, &env, &event_tx, &token).await {
        Ok(()) => {
            let save = env
                .data_dir
                .save_cluster(&ClusterRecord::managed(spec.clone()));
            emit_important(
                &event_tx,
                CoreEvent::ProvisionDone {
                    name: name.clone(),
                    op_gen,
                    result: save.map_err(|err| err.to_string()),
                },
            );
        }
        Err(err) => {
            emit_important(
                &event_tx,
                CoreEvent::ProvisionDone {
                    name,
                    op_gen,
                    result: Err(err),
                },
            );
        }
    }
}

/// The provision-plan half of create/recreate: build + run the plan,
/// forwarding [`ProvisionEvent`]s.
async fn create_phase(
    spec: &ClusterSpec,
    op_gen: OpGen,
    env: &WorkerEnv,
    event_tx: &Sender<CoreEvent>,
    token: &CancellationToken,
) -> Result<(), String> {
    let cilium_version = if spec.cni == core::Cni::Cilium {
        core::detect_cilium_version().await
    } else {
        None
    };
    let plan = core::build_plan_with_cilium_version(
        spec,
        env.data_dir.root(),
        &env.kubeconfig_path,
        cilium_version.as_deref(),
    )
    .map_err(|err| err.to_string())?;
    let (plan_tx, mut plan_rx) = tokio::sync::mpsc::channel::<ProvisionEvent>(64);
    let name = spec.name.clone();
    let forward_tx = event_tx.clone();
    let forward = tokio::spawn(async move {
        while let Some(event) = plan_rx.recv().await {
            emit(
                &forward_tx,
                CoreEvent::Provision {
                    name: name.clone(),
                    op_gen,
                    event,
                },
            );
        }
    });
    let result = core::run_plan(&plan, &spec.name, Some(token.clone()), &plan_tx).await;
    drop(plan_tx);
    let _ = forward.await;
    result.map_err(|err| err.to_string())
}

/// Destroy a cluster: `kind delete`, kubeconfig context removal, optional
/// stored-record removal. Streams synthetic provision steps so the UI
/// timeline stays uniform.
async fn do_destroy(
    name: String,
    op_gen: OpGen,
    env: WorkerEnv,
    event_tx: Sender<CoreEvent>,
    token: CancellationToken,
) {
    let result = destroy_phase(&name, op_gen, true, &env, &event_tx, &token).await;
    emit_important(
        &event_tx,
        CoreEvent::ProvisionDone {
            name,
            op_gen,
            result,
        },
    );
}

/// The destroy half of destroy/recreate.
async fn destroy_phase(
    name: &str,
    op_gen: OpGen,
    remove_record: bool,
    env: &WorkerEnv,
    event_tx: &Sender<CoreEvent>,
    token: &CancellationToken,
) -> Result<(), String> {
    let provision = |event| {
        emit(
            event_tx,
            CoreEvent::Provision {
                name: name.to_string(),
                op_gen,
                event,
            },
        );
    };

    provision(ProvisionEvent::StepStarted {
        id: "destroy-kind".to_string(),
    });
    let deleted = match run_cmd_cancel(
        KindCommand::KindDelete {
            name: name.to_string(),
        },
        token,
    )
    .await
    {
        Ok(_) => true,
        Err(err) => {
            if token.is_cancelled() {
                provision(ProvisionEvent::StepFailed {
                    id: "destroy-kind".to_string(),
                    error: "cancelled".to_string(),
                });
                return Err("cancelled".to_string());
            }
            // kind delete failed. Only tolerable when the cluster is
            // verifiably gone; a genuine failure (docker daemon down,
            // timeout, permissions) must abort here so the kubeconfig
            // context and stored record are not silently destroyed while
            // the cluster may still exist.
            match run_cmd(KindCommand::KindGetClusters).await {
                Ok(output)
                    if !core::parse_kind_get_clusters(&output.stdout())
                        .iter()
                        .any(|live| live == name) =>
                {
                    provision(ProvisionEvent::StepOutput {
                        id: "destroy-kind".to_string(),
                        line: "cluster was already gone; cleanup continues".to_string(),
                    });
                    true
                }
                _ => {
                    provision(ProvisionEvent::StepFailed {
                        id: "destroy-kind".to_string(),
                        error: err.clone(),
                    });
                    return Err(err);
                }
            }
        }
    };
    if deleted {
        provision(ProvisionEvent::StepFinished {
            id: "destroy-kind".to_string(),
        });
    }

    provision(ProvisionEvent::StepStarted {
        id: "remove-kubeconfig".to_string(),
    });
    match remove_context(name, &env.kubeconfig_path) {
        Ok(()) => {
            provision(ProvisionEvent::StepFinished {
                id: "remove-kubeconfig".to_string(),
            });
        }
        Err(err) => {
            provision(ProvisionEvent::StepFailed {
                id: "remove-kubeconfig".to_string(),
                error: err.clone(),
            });
            return Err(err);
        }
    }

    if remove_record {
        provision(ProvisionEvent::StepStarted {
            id: "remove-record".to_string(),
        });
        match env.data_dir.remove_cluster(name) {
            Ok(_) => {
                provision(ProvisionEvent::StepFinished {
                    id: "remove-record".to_string(),
                });
            }
            Err(err) => {
                provision(ProvisionEvent::StepFailed {
                    id: "remove-record".to_string(),
                    error: err.to_string(),
                });
                return Err(err.to_string());
            }
        }
    }

    Ok(())
}

/// Remove the cluster's kubeconfig triple.
fn remove_context(name: &str, kubeconfig_path: &std::path::Path) -> Result<(), String> {
    let mut store = KubeconfigStore::load_from(kubeconfig_path).map_err(|err| err.to_string())?;
    let _ = store.remove_context(name);
    store.save().map_err(|err| err.to_string())
}

/// Guided recreate (scale workers): destroy + create from the new spec.
/// The stored record is deliberately NOT removed during the destroy phase;
/// it is only replaced after the new cluster is up (ADR-0002).
async fn do_recreate(
    name: String,
    spec: ClusterSpec,
    op_gen: OpGen,
    env: WorkerEnv,
    event_tx: Sender<CoreEvent>,
    token: CancellationToken,
) {
    let result = match destroy_phase(&name, op_gen, false, &env, &event_tx, &token).await {
        Ok(()) => match create_phase(&spec, op_gen, &env, &event_tx, &token).await {
            Ok(()) => env
                .data_dir
                .save_cluster(&ClusterRecord::managed(spec))
                .map_err(|err| err.to_string()),
            Err(err) => Err(err),
        },
        Err(err) => Err(err),
    };
    emit_important(
        &event_tx,
        CoreEvent::ProvisionDone {
            name,
            op_gen,
            result,
        },
    );
}

/// Verify the kubeconfig context state of a cluster.
async fn do_check_context(name: String, env: WorkerEnv, event_tx: Sender<CoreEvent>) {
    let state = match KubeconfigStore::load_from(&env.kubeconfig_path) {
        Ok(store) => {
            let context = KubeconfigStore::kind_context_name(&name);
            if !store.verify_context(&context) {
                ContextStatus::Absent
            } else if store.current_context() == Some(context.as_str()) {
                ContextStatus::Current
            } else {
                ContextStatus::Present
            }
        }
        Err(_) => ContextStatus::Absent,
    };
    emit(&event_tx, CoreEvent::ContextState { name, state });
}

/// `kind get kubeconfig` + merge + set current.
async fn do_ensure_context(name: String, env: WorkerEnv, event_tx: Sender<CoreEvent>) {
    let result: core::Result<()> = async {
        let mut store = KubeconfigStore::load_from(&env.kubeconfig_path)?;
        let output = KindCommand::KindExportKubeconfig {
            name: name.clone(),
            internal: false,
        }
        .to_cmd()
        .run()
        .await?;
        store.ensure_context_from_kind_output(&name, &output.stdout())?;
        store.save()
    }
    .await;
    emit_important(
        &event_tx,
        CoreEvent::ContextEnsured {
            name,
            result: result.map_err(|err| err.to_string()),
        },
    );
}

/// `kind export logs` into a directory. (Core gap: [`KindCommand`] has no
/// export-logs variant; the exec module — core's sanctioned spawn surface —
/// builds the argv directly. See report.)
async fn do_export_logs(name: String, dest: PathBuf, event_tx: Sender<CoreEvent>) {
    let command = Cmd::new("kind")
        .args(["export", "logs"])
        .arg(dest.to_string_lossy().to_string())
        .args(["--name", name.as_str()])
        .timeout(Duration::from_secs(300));
    let result = command
        .run()
        .await
        .map(|_| ())
        .map_err(|err| err.to_string());
    emit_important(&event_tx, CoreEvent::LogsExported { name, dest, result });
}

/// One topology snapshot for a cluster.
async fn fetch_topology(
    name: &str,
    kubeconfig_path: &std::path::Path,
) -> Result<TopologyGraph, String> {
    let store = KubeconfigStore::load_from(kubeconfig_path).map_err(|err| err.to_string())?;
    let context = KubeconfigStore::kind_context_name(name);
    if !store.verify_context(&context) {
        return Err(format!(
            "kubeconfig context `{context}` is missing; use \"Export kubeconfig\" first"
        ));
    }
    let client = K8sClient::for_context(store.config().clone(), context)
        .await
        .map_err(|err| err.to_string())?;
    client.poll_topology().await.map_err(|err| err.to_string())
}

/// Periodic topology snapshots until cancelled.
async fn do_auto_refresh(
    name: String,
    interval_secs: u64,
    op_gen: OpGen,
    event_tx: Sender<CoreEvent>,
    token: CancellationToken,
    kubeconfig_path: PathBuf,
) {
    let interval = Duration::from_secs(interval_secs.max(MIN_AUTO_REFRESH_SECS));
    loop {
        if token.is_cancelled() {
            break;
        }
        let result = fetch_topology(&name, &kubeconfig_path).await;
        emit(
            &event_tx,
            CoreEvent::Topology {
                name: name.clone(),
                op_gen,
                result: Box::new(result),
            },
        );
        tokio::select! {
            _ = token.cancelled() => break,
            _ = tokio::time::sleep(interval) => {}
        }
    }
}

/// Log reading: one-shot tail, or follow mode with a ring pump.
async fn do_logs(
    name: String,
    source: LogSource,
    follow: bool,
    tail: Option<u32>,
    op_gen: OpGen,
    event_tx: Sender<CoreEvent>,
    token: CancellationToken,
) {
    if follow {
        let ring = Arc::new(Mutex::new(LogRing::new(
            kindboard_core::k8s::DEFAULT_LOG_RING_CAP,
        )));
        let pump_ring = Arc::clone(&ring);
        let pump_tx = event_tx.clone();
        let pump_name = name.clone();
        let pump_token = token.clone();
        let pump = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = pump_token.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_millis(LOG_PUMP_MS)) => {
                        let batch = match pump_ring.lock() {
                            Ok(mut ring) => {
                                let lines = ring.snapshot();
                                ring.clear();
                                lines
                            }
                            Err(_) => continue,
                        };
                        for line in batch {
                            emit(
                                &pump_tx,
                                CoreEvent::LogLine {
                                    name: pump_name.clone(),
                                    op_gen,
                                    line,
                                },
                            );
                        }
                    }
                }
            }
        });
        let result = core::watch_logs(&source, ring, token)
            .await
            .map_err(|err| err.to_string());
        let _ = pump.await;
        emit_important(
            &event_tx,
            CoreEvent::LogEnded {
                name,
                op_gen,
                result,
            },
        );
    } else {
        let result = core::read_logs(&source, tail).await;
        match result {
            Ok(lines) => {
                for line in lines {
                    emit(
                        &event_tx,
                        CoreEvent::LogLine {
                            name: name.clone(),
                            op_gen,
                            line,
                        },
                    );
                }
                emit_important(
                    &event_tx,
                    CoreEvent::LogEnded {
                        name,
                        op_gen,
                        result: Ok(()),
                    },
                );
            }
            Err(err) => {
                emit_important(
                    &event_tx,
                    CoreEvent::LogEnded {
                        name,
                        op_gen,
                        result: Err(err.to_string()),
                    },
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn temp_file(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("kindboard-worker-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("kubeconfig")
    }

    #[test]
    fn op_token_cancels_previous_token_for_same_slot() {
        let mut cancels = HashMap::new();
        let first = op_token(&mut cancels, "create:demo");
        let second = op_token(&mut cancels, "create:demo");
        assert!(first.is_cancelled(), "replaced token must be cancelled");
        assert!(!second.is_cancelled());

        // A different slot is untouched.
        let other = op_token(&mut cancels, "destroy:demo");
        assert!(!second.is_cancelled(), "other slots must not be cancelled");
        assert!(!other.is_cancelled());
    }

    #[test]
    fn remove_context_removes_triple_and_saves() {
        let path = temp_file("remove");
        let mut store = KubeconfigStore::load_from(&path).unwrap();
        store
            .ensure_context("demo", "https://127.0.0.1:1234", "CA", "CERT", "KEY")
            .unwrap();
        store
            .ensure_context("other", "https://127.0.0.1:9999", "CA", "CERT", "KEY")
            .unwrap();
        store.save().unwrap();

        remove_context("demo", &path).unwrap();

        let reloaded = KubeconfigStore::load_from(&path).unwrap();
        assert!(!reloaded.verify_context("kind-demo"));
        assert!(
            reloaded.verify_context("kind-other"),
            "other contexts survive"
        );
        assert_eq!(
            reloaded.current_context(),
            Some("kind-other"),
            "removing a non-current context keeps the current one"
        );
    }

    #[test]
    fn remove_context_with_missing_kubeconfig_is_ok() {
        let path = temp_file("missing");
        // No file at all: load yields an empty store, removal is a no-op,
        // save writes an (empty) kubeconfig without error.
        remove_context("demo", &path).unwrap();
        let reloaded = KubeconfigStore::load_from(&path).unwrap();
        assert!(!reloaded.verify_context("kind-demo"));
        assert!(path.is_file());
    }

    #[test]
    fn destroy_remove_context_path_uses_core_remove() {
        // The worker's destroy flow must go through the core store's
        // remove_context (triple removal), verified end-to-end at the file
        // level: context, cluster, and user entries all disappear.
        let path = temp_file("triple");
        let mut store = KubeconfigStore::load_from(&path).unwrap();
        store
            .ensure_context("demo", "https://127.0.0.1:1234", "CA", "CERT", "KEY")
            .unwrap();
        store.save().unwrap();
        assert!(store.verify_context("kind-demo"));

        remove_context("demo", &path).unwrap();

        let reloaded = KubeconfigStore::load_from(&path).unwrap();
        assert!(reloaded.context_names().is_empty());
        assert!(reloaded.config().clusters.is_empty());
        assert!(reloaded.config().auth_infos.is_empty());
    }

    #[test]
    fn emit_bus_full_notice_is_bounded_and_delivered() {
        // A disconnected sender must not make emit panic or block.
        let (tx, _rx) = crossbeam_channel::bounded::<CoreEvent>(1);
        drop(_rx);
        emit(&tx, CoreEvent::ToolsDetectDone);
        emit_important(&tx, CoreEvent::ToolsDetectDone);

        // And the BusFull grace path is exercised through the bus tests in
        // `bus.rs`; here we only pin the no-panic property.
        let (tx, rx) = crossbeam_channel::bounded::<CoreEvent>(1);
        let stop = Arc::new(AtomicBool::new(false));
        let drainer_stop = stop.clone();
        let drainer = std::thread::spawn(move || {
            let _ = rx; // keep alive; not drained → stays full
            while !drainer_stop.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        });
        tx.try_send(CoreEvent::ToolsDetectDone).unwrap();
        let started = std::time::Instant::now();
        emit(&tx, CoreEvent::ToolsDetectDone);
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(500),
            "BusFull grace wait must be bounded: {elapsed:?}"
        );
        stop.store(true, Ordering::Relaxed);
        drainer.join().unwrap();
    }

    #[test]
    fn k9s_argv_uses_e_flag_and_context() {
        assert_eq!(
            k9s_argv("foot", "kind-demo"),
            vec!["foot", "-e", "k9s", "--context", "kind-demo"]
        );
        assert_eq!(
            k9s_argv("alacritty", "kind-demo"),
            vec!["alacritty", "-e", "k9s", "--context", "kind-demo"]
        );
        // kitty has no -e short flag: the program goes after `--`.
        assert_eq!(
            k9s_argv("kitty", "kind-demo"),
            vec!["kitty", "--", "k9s", "--context", "kind-demo"]
        );
        assert_eq!(
            k9s_argv("gnome-terminal", "kind-demo"),
            vec!["gnome-terminal", "--", "k9s", "--context", "kind-demo"]
        );
    }

    #[test]
    fn terminal_candidates_include_common_terminals() {
        assert!(TERMINAL_CANDIDATES.contains(&"foot"));
        assert!(TERMINAL_CANDIDATES.contains(&"kitty"));
        assert!(TERMINAL_CANDIDATES.contains(&"alacritty"));
    }
}
