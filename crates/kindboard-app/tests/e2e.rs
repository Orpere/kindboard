//! E2E test for kindboard-app: exercises the *real* UI-to-core plumbing
//! (bus + worker + kindboard-core) against a throwaway kind cluster.
//! This is the same path the GUI drives, minus the window.
//!
//! Gated like the core e2e: skipped unless `KINDBOARD_E2E=1` and docker is
//! reachable. The worker is pointed at temp dirs ([`WorkerEnv::new`]) so
//! the user's `~/.local/share/kindboard` and `~/.kube/config` are never
//! touched. Clusters are deleted even on failure (best effort).
//!
//! Run with:
//! `KINDBOARD_E2E=1 cargo test -p kindboard-app --test e2e -- --test-threads 1 --nocapture`

use std::time::{Duration, Instant};

use kindboard_app::bus::{Buses, ClusterLiveStatus, ContextStatus, CoreCommand, CoreEvent};
use kindboard_app::worker::{self, WorkerEnv};
use kindboard_core::{ClusterSpec, DataDir};

fn e2e_enabled() -> bool {
    std::env::var("KINDBOARD_E2E").as_deref() == Ok("1")
}

async fn docker_available() -> bool {
    kindboard_core::Cmd::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .timeout(Duration::from_secs(10))
        .run()
        .await
        .is_ok()
}

fn require_e2e() -> bool {
    if !e2e_enabled() {
        eprintln!(
            "SKIP: app E2E tests need KINDBOARD_E2E=1 (and a running docker daemon); \
             set it and run `cargo test -p kindboard-app --test e2e -- --test-threads 1`"
        );
        return false;
    }
    true
}

/// Test-local state dir + kubeconfig so nothing user-owned is touched.
fn test_env(tag: &str) -> WorkerEnv {
    let root = std::env::temp_dir().join(format!("kindboard-app-e2e-{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    let data_dir = DataDir::new(&root).expect("create test data dir");
    WorkerEnv::new(data_dir, root.join("kubeconfig"))
}

/// Wait for the first event matching `predicate`, failing after `timeout`.
/// Blocking on purpose: the test thread has nothing else to do and the
/// worker runs on its own thread.
fn wait_for(
    rx: &crossbeam_channel::Receiver<CoreEvent>,
    timeout: Duration,
    mut predicate: impl FnMut(&CoreEvent) -> bool,
) -> Result<CoreEvent, String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining.min(Duration::from_secs(1))) {
            Ok(event) => {
                if predicate(&event) {
                    return Ok(event);
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                return Err("event bus disconnected".to_string());
            }
        }
    }
    Err("timed out waiting for event".to_string())
}

/// Serialize app e2e tests (kind cluster creation is heavy).
async fn e2e_lock() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}

/// Full lifecycle through the worker: create → reconcile → topology →
/// context → destroy → reconcile.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn create_inspect_destroy_via_worker() {
    if !require_e2e() || !docker_available().await {
        return;
    }
    let _guard = e2e_lock().await;

    let name = format!("kbtest-app-{}", std::process::id());
    let env = test_env(&name);
    let env_for_test = env.clone();
    let buses = Buses::new();
    let cmd_tx = buses.cmd_tx.clone();
    let event_rx = buses.event_rx.clone();
    let worker_handle = worker::spawn_with_env(buses.cmd_rx, buses.event_tx, move || Ok(env));

    let spec = ClusterSpec {
        name: name.clone(),
        worker_count: 0,
        ..ClusterSpec::default()
    };

    let cleanup = |cmd_tx: &crossbeam_channel::Sender<CoreCommand>,
                   event_rx: &crossbeam_channel::Receiver<CoreEvent>| {
        let _ = cmd_tx.try_send(CoreCommand::DestroyCluster {
            name: name.clone(),
            op_gen: 99,
        });
        // Drain until the destroy reports done. `kind delete` can run for
        // many seconds without emitting anything, so silence between events
        // must not end the wait (a premature break would Shutdown the
        // worker mid-delete and leak the cluster).
        let deadline = Instant::now() + Duration::from_secs(120);
        while Instant::now() < deadline {
            match event_rx.recv_timeout(Duration::from_millis(1000)) {
                Ok(CoreEvent::ProvisionDone { op_gen: 99, .. }) => break,
                Ok(_) | Err(_) => continue,
            }
        }
    };

    let result = (async {
        // 1. Create.
        cmd_tx
            .try_send(CoreCommand::CreateCluster {
                spec: Box::new(spec.clone()),
                op_gen: 1,
            })
            .expect("enqueue create");
        let done = wait_for(&event_rx, Duration::from_secs(420), |event| {
            matches!(event, CoreEvent::ProvisionDone { name: n, op_gen: 1, .. } if n == &name)
        })?;
        match done {
            CoreEvent::ProvisionDone { result, .. } => {
                result.map_err(|err| format!("create failed: {err}"))?;
            }
            _ => unreachable!(),
        }

        // 2. Reconcile → the cluster is Managed.
        cmd_tx.try_send(CoreCommand::Reconcile).expect("enqueue reconcile");
        let report = wait_for(&event_rx, Duration::from_secs(60), |event| {
            matches!(event, CoreEvent::ReconcileDone { .. })
        })?;
        match report {
            CoreEvent::ReconcileDone { result } => {
                let report = result.map_err(|err| format!("reconcile failed: {err}"))?;
                assert_eq!(report.managed_names(), vec![name.clone()]);
            }
            _ => unreachable!(),
        }

        // 3. Topology snapshot (the create flow merged the context).
        cmd_tx
            .try_send(CoreCommand::FetchTopology {
                name: name.clone(),
                op_gen: 2,
            })
            .expect("enqueue topology");
        let topo = wait_for(&event_rx, Duration::from_secs(120), |event| {
            matches!(event, CoreEvent::Topology { name: n, op_gen: 2, .. } if n == &name)
        })?;
        match topo {
            CoreEvent::Topology { result, .. } => {
                let graph = result.map_err(|err| format!("topology failed: {err}"))?;
                assert!(
                    graph.nodes.iter().any(|node| node.name.ends_with("-control-plane")),
                    "control-plane node must be visible, got {:?}",
                    graph.nodes
                );
                assert!(!graph.layout.nodes.is_empty(), "layout must be computed");
            }
            _ => unreachable!(),
        }

        // 4. Context check → current (create sets it).
        cmd_tx
            .try_send(CoreCommand::CheckContext {
                name: name.clone(),
            })
            .expect("enqueue context check");
        let ctx = wait_for(&event_rx, Duration::from_secs(60), |event| {
            matches!(event, CoreEvent::ContextState { name: n, .. } if n == &name)
        })?;
        match ctx {
            CoreEvent::ContextState { state, .. } => {
                assert_eq!(state, ContextStatus::Current);
            }
            _ => unreachable!(),
        }

        // 5. Guided recreate (scale to 1 worker) — R7.
        let mut scaled = spec.clone();
        scaled.worker_count = 1;
        cmd_tx
            .try_send(CoreCommand::RecreateCluster {
                name: name.clone(),
                spec: Box::new(scaled.clone()),
                op_gen: 4,
            })
            .expect("enqueue recreate");
        let done = wait_for(&event_rx, Duration::from_secs(600), |event| {
            matches!(event, CoreEvent::ProvisionDone { name: n, op_gen: 4, .. } if n == &name)
        })?;
        match done {
            CoreEvent::ProvisionDone { result, .. } => {
                result.map_err(|err| format!("recreate failed: {err}"))?;
            }
            _ => unreachable!(),
        }

        // The stored record reflects the new spec; other fields preserved.
        let record = env_for_test
            .data_dir
            .load_cluster(&name)
            .unwrap()
            .expect("record must exist after recreate");
        assert_eq!(record.spec.worker_count, 1);
        assert_eq!(record.spec.name, name);
        assert_eq!(record.spec.cni, scaled.cni);
        assert_eq!(record.spec.k8s_version, scaled.k8s_version);
        assert_eq!(record.spec.pod_cidr, scaled.pod_cidr);

        // The recreate flow re-merged the kubeconfig context (current).
        cmd_tx
            .try_send(CoreCommand::CheckContext {
                name: name.clone(),
            })
            .expect("enqueue context check after recreate");
        let ctx = wait_for(&event_rx, Duration::from_secs(60), |event| {
            matches!(event, CoreEvent::ContextState { name: n, .. } if n == &name)
        })?;
        match ctx {
            CoreEvent::ContextState { state, .. } => {
                assert_eq!(state, ContextStatus::Current);
            }
            _ => unreachable!(),
        }

        // 6. Destroy.
        cmd_tx
            .try_send(CoreCommand::DestroyCluster {
                name: name.clone(),
                op_gen: 5,
            })
            .expect("enqueue destroy");
        let done = wait_for(&event_rx, Duration::from_secs(180), |event| {
            matches!(event, CoreEvent::ProvisionDone { name: n, op_gen: 5, .. } if n == &name)
        })?;
        match done {
            CoreEvent::ProvisionDone { result, .. } => {
                result.map_err(|err| format!("destroy failed: {err}"))?;
            }
            _ => unreachable!(),
        }

        // 7. Reconcile → no clusters left.
        cmd_tx.try_send(CoreCommand::Reconcile).expect("enqueue reconcile");
        let report = wait_for(&event_rx, Duration::from_secs(60), |event| {
            matches!(event, CoreEvent::ReconcileDone { .. })
        })?;
        match report {
            CoreEvent::ReconcileDone { result } => {
                let report = result.map_err(|err| format!("reconcile failed: {err}"))?;
                assert!(
                    !report.clusters.iter().any(|entry| entry.name == name),
                    "cluster must be gone after destroy"
                );
            }
            _ => unreachable!(),
        }

        // 8. The kubeconfig context is removed by destroy.
        cmd_tx
            .try_send(CoreCommand::CheckContext {
                name: name.clone(),
            })
            .expect("enqueue context check after destroy");
        let ctx = wait_for(&event_rx, Duration::from_secs(60), |event| {
            matches!(event, CoreEvent::ContextState { name: n, .. } if n == &name)
        })?;
        match ctx {
            CoreEvent::ContextState { state, .. } => {
                assert_eq!(state, ContextStatus::Absent);
            }
            _ => unreachable!(),
        }

        Ok::<(), String>(())
    })
    .await;

    if result.is_err() {
        cleanup(&cmd_tx, &event_rx);
    }
    let _ = cmd_tx.try_send(CoreCommand::Shutdown);
    let _ = worker_handle.join();
    result.expect("app e2e lifecycle must pass");
}

/// The overview's status probe path (`kind get nodes`) for a live cluster.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_probe_reports_running() {
    if !require_e2e() || !docker_available().await {
        return;
    }
    let _guard = e2e_lock().await;

    let name = format!("kbtest-app-{}", std::process::id());
    let env = test_env(&name);
    let buses = Buses::new();
    let cmd_tx = buses.cmd_tx.clone();
    let event_rx = buses.event_rx.clone();
    let worker_handle =
        worker::spawn_with_env(buses.cmd_rx, buses.event_tx, move || Ok(env.clone()));

    let result = (async {
        let spec = ClusterSpec {
            name: name.clone(),
            worker_count: 1,
            ..ClusterSpec::default()
        };
        cmd_tx
            .try_send(CoreCommand::CreateCluster {
                spec: Box::new(spec),
                op_gen: 1,
            })
            .expect("enqueue create");
        let _ = wait_for(&event_rx, Duration::from_secs(420), |event| {
            matches!(event, CoreEvent::ProvisionDone { name: n, op_gen: 1, .. } if n == &name)
        })?;

        cmd_tx.try_send(CoreCommand::Reconcile).expect("enqueue reconcile");
        let status = wait_for(&event_rx, Duration::from_secs(60), |event| {
            matches!(event, CoreEvent::ClusterStatus { name: n, .. } if n == &name)
        })?;
        match status {
            CoreEvent::ClusterStatus { status, .. } => match status {
                ClusterLiveStatus::Running { node_count } => {
                    // 1 control-plane + 1 worker.
                    assert_eq!(node_count, 2);
                }
                other => panic!("expected Running, got {other:?}"),
            },
            _ => unreachable!(),
        }

        Ok::<(), String>(())
    })
    .await;

    let _ = cmd_tx.try_send(CoreCommand::DestroyCluster {
        name: name.clone(),
        op_gen: 99,
    });
    // Wait for the destroy to finish even through long silent stretches
    // (kind delete emits nothing while it runs); leaving early would
    // Shutdown the worker mid-delete and leak the cluster.
    let deadline = Instant::now() + Duration::from_secs(180);
    while Instant::now() < deadline {
        match event_rx.recv_timeout(Duration::from_millis(1000)) {
            Ok(CoreEvent::ProvisionDone { op_gen: 99, .. }) => break,
            Ok(_) | Err(_) => continue,
        }
    }
    let _ = cmd_tx.try_send(CoreCommand::Shutdown);
    let _ = worker_handle.join();
    result.expect("status probe e2e must pass");
}
