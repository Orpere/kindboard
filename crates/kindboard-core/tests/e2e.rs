//! E2E integration tests against a real kind cluster.
//!
//! Skipped (with a notice) unless `KINDBOARD_E2E=1` is set AND docker is
//! reachable. All clusters use throwaway `kbtest-*` names and are deleted at
//! the end of each test (even on failure, best effort).
//!
//! Run with:
//! `KINDBOARD_E2E=1 cargo test -p kindboard-core --test e2e -- --test-threads 1 --nocapture`

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use kindboard_core::exec::Cmd;
use kindboard_core::kindctl::{KindCommand, parse_kind_get_clusters};
use kindboard_core::kubeconfig::KubeconfigStore;
use kindboard_core::spec::{self, ClusterSpec};
use kindboard_core::{K8sClient, ProvisionAction, ProvisionEvent};

fn e2e_enabled() -> bool {
    std::env::var("KINDBOARD_E2E").as_deref() == Ok("1")
}

async fn docker_available() -> bool {
    Cmd::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .timeout(Duration::from_secs(10))
        .run()
        .await
        .is_ok()
}

fn require_e2e() -> Option<()> {
    if !e2e_enabled() {
        eprintln!(
            "SKIP: E2E tests need KINDBOARD_E2E=1 (and a running docker daemon); \
             set it and run `cargo test -p kindboard-core --test e2e -- --test-threads 1`"
        );
        return None;
    }
    Some(())
}

static E2E_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

fn e2e_lock() -> &'static tokio::sync::Mutex<()> {
    E2E_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn test_data_dir() -> PathBuf {
    std::env::temp_dir().join("kindboard-e2e-data")
}

fn test_kubeconfig_path() -> PathBuf {
    test_data_dir().join("kubeconfig-e2e")
}

async fn cleanup_cluster(name: &str) {
    let _ = KindCommand::KindDelete {
        name: name.to_string(),
    }
    .to_cmd()
    .run()
    .await;
    let _ = std::fs::remove_file(
        test_data_dir()
            .join("clusters")
            .join(format!("{name}.json")),
    );
}

async fn create_cluster(_name: &str, spec: &ClusterSpec) {
    let data_dir = test_data_dir();
    let kubeconfig_path = test_kubeconfig_path();
    std::fs::create_dir_all(&data_dir).unwrap();

    let mut plan = kindboard_core::build_plan(spec, &data_dir, &kubeconfig_path).unwrap();
    // Strip CNI/ingress/verify steps: E2E runs the real `kind create` +
    // merge flow; CNI installs are exercised by their own E2E tests.
    let keep = ["write-kind-config", "kind-create", "merge-kubeconfig"];
    plan.steps.retain(|step| keep.contains(&step.id.as_str()));
    for step in &mut plan.steps {
        step.depends_on.retain(|dep| keep.contains(&dep.as_str()));
    }

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let result = kindboard_core::run_plan(&plan, None, &tx).await;
    while let Ok(event) = rx.try_recv() {
        if let ProvisionEvent::StepOutput { line, .. } = event {
            eprintln!("[kind] {line}");
        }
    }
    assert!(result.is_ok(), "create plan failed: {result:?}");
}

#[tokio::test]
async fn e2e_kindnet_create_topology_kubeconfig_delete() {
    let Some(()) = require_e2e() else { return };
    let _guard = e2e_lock().lock().await;
    if !docker_available().await {
        eprintln!("SKIP: docker daemon not reachable");
        return;
    }
    let name = format!("kbtest-{}", std::process::id());
    let spec = ClusterSpec {
        name: name.clone(),
        ..ClusterSpec::default()
    };
    let mut cleanup = true;
    let run = async {
        create_cluster(&name, &spec).await;

        // kind lists the cluster.
        let output = KindCommand::KindGetClusters.to_cmd().run().await.unwrap();
        let clusters = parse_kind_get_clusters(&output.stdout());
        assert!(clusters.contains(&name), "clusters: {clusters:?}");

        // Kubeconfig has the merged context.
        let store = KubeconfigStore::load_from(test_kubeconfig_path()).unwrap();
        assert!(
            store.verify_context(&format!("kind-{name}")),
            "contexts: {:?}",
            store.context_names()
        );

        // Topology read via kube-rs.
        let client = K8sClient::from_kubeconfig(store.config().clone())
            .await
            .unwrap();
        assert_eq!(client.context(), format!("kind-{name}"));
        let topology = client.poll_topology().await.unwrap();
        assert!(!topology.namespaces.is_empty(), "no namespaces");
        assert!(
            topology
                .namespaces
                .iter()
                .any(|ns| ns.name == "kube-system"),
            "kube-system missing"
        );
        assert!(!topology.nodes.is_empty(), "no nodes");
        assert!(topology.nodes.iter().any(|node| node.ready));
        assert!(!topology.layout.nodes.is_empty());
        assert!(!topology.workloads.is_empty(), "no workloads");
        assert!(!topology.pods.is_empty(), "no pods");

        // Node list via kind CLI.
        let output = KindCommand::KindGetNodes {
            cluster: name.clone(),
        }
        .to_cmd()
        .run()
        .await
        .unwrap();
        let nodes = kindboard_core::parse_kind_get_nodes(&output.stdout());
        assert_eq!(
            nodes,
            vec![format!("{name}-control-plane")],
            "nodes: {nodes:?}"
        );
        cleanup = true;
    };
    // Run, then always delete the throwaway cluster.
    let result: std::result::Result<(), ()> = tokio::select! {
        result = run => Ok(result),
        _ = tokio::time::sleep(Duration::from_secs(600)) => Err(()),
    };
    if cleanup {
        cleanup_cluster(&name).await;
    }
    // Re-check the cluster is gone.
    let output = KindCommand::KindGetClusters.to_cmd().run().await.unwrap();
    let clusters = parse_kind_get_clusters(&output.stdout());
    assert!(
        !clusters.contains(&name),
        "cluster {name} was not cleaned up: {clusters:?}"
    );
    if result.is_err() {
        panic!("E2E timed out");
    }
}

#[tokio::test]
async fn e2e_plan_builder_produces_valid_config_for_kind() {
    let Some(()) = require_e2e() else { return };
    let _guard = e2e_lock().lock().await;
    if !docker_available().await {
        eprintln!("SKIP: docker daemon not reachable");
        return;
    }
    let name = format!("kbtest-plan-{}", std::process::id());
    // Exercise the spec→config mapping against real kind: workers + ports +
    // flannel-shaped config (but no CNI install).
    let spec = ClusterSpec {
        name: name.clone(),
        worker_count: 1,
        extra_port_mappings: vec![
            kindboard_core::PortMapping {
                container_port: 8080,
                host_port: 18080,
                listen_address: "127.0.0.1".to_string(),
                protocol: kindboard_core::Protocol::Tcp,
            },
            kindboard_core::PortMapping {
                container_port: 443,
                host_port: 443,
                listen_address: "127.0.0.1".to_string(),
                protocol: kindboard_core::Protocol::Tcp,
            },
            kindboard_core::PortMapping {
                container_port: 80,
                host_port: 80,
                listen_address: "127.0.0.1".to_string(),
                protocol: kindboard_core::Protocol::Tcp,
            },
        ],
        ingress: Some(kindboard_core::IngressController::Nginx),
        ..ClusterSpec::default()
    };
    spec::validate(&spec).unwrap();

    let data_dir = test_data_dir();
    let plan = kindboard_core::build_plan(&spec, &data_dir, &test_kubeconfig_path()).unwrap();
    let config_yaml = match &plan
        .steps
        .iter()
        .find(|step| step.id == "write-kind-config")
        .unwrap()
        .command
    {
        ProvisionAction::WriteFile { content, .. } => content.clone(),
        other => panic!("expected WriteFile, got {other:?}"),
    };
    let config_path = data_dir.join("tmp").join(format!("kbtest-{name}.yaml"));
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, &config_yaml).unwrap();

    // Real kind create from the rendered config, then delete.
    let create = KindCommand::KindCreate {
        config_path,
        wait: true,
    }
    .to_cmd()
    .run()
    .await;
    match create {
        Ok(output) => assert!(output.success()),
        Err(err) => panic!("kind create failed with rendered config: {err}"),
    }
    let delete = KindCommand::KindDelete { name: name.clone() }
        .to_cmd()
        .run()
        .await;
    assert!(delete.is_ok(), "delete failed: {delete:?}");
}

#[tokio::test]
async fn e2e_run_plan_executes_full_flow() {
    let Some(()) = require_e2e() else { return };
    let _guard = e2e_lock().lock().await;
    if !docker_available().await {
        eprintln!("SKIP: docker daemon not reachable");
        return;
    }
    let name = format!("kbtest-flow-{}", std::process::id());
    let spec = ClusterSpec {
        name: name.clone(),
        ..ClusterSpec::default()
    };
    let data_dir = test_data_dir();
    std::fs::create_dir_all(&data_dir).unwrap();
    let plan = kindboard_core::build_plan(&spec, &data_dir, &test_kubeconfig_path()).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let result = kindboard_core::run_plan(&plan, None, &tx).await;
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let ProvisionEvent::StepOutput { line, .. } = event {
            events.push(line);
        }
    }
    match result {
        Ok(()) => {}
        Err(err) => {
            cleanup_cluster(&name).await;
            panic!("full plan failed: {err}\noutput: {events:?}");
        }
    }
    // The cluster must exist and respond.
    let output = KindCommand::KindGetClusters.to_cmd().run().await.unwrap();
    let clusters = parse_kind_get_clusters(&output.stdout());
    assert!(clusters.contains(&name));
    cleanup_cluster(&name).await;
}
