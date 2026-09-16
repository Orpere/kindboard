//! kindboard-core: UI-free orchestration library for managing kind clusters.
//!
//! Owns all subprocess spawning, kubeconfig edits, k8s API reads and
//! persistence. Never touches egui/eframe. See `docs/contracts.md` and
//! `docs/architecture.md` for the locked design.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod curl;
pub mod deps;
pub mod error;
pub mod exec;
pub mod k8s;
pub mod kindctl;
pub mod kubeconfig;
pub mod provision;
pub mod spec;
pub mod state;

mod fsutil;

pub use error::{CoreError, Result};

pub use deps::{
    DockerDaemonState, InstallEvent, InstallPlan, InstallStep, Platform, Tool, ToolId, ToolStatus,
    Version, check_docker_daemon, detect, detect_platform, install, install_with_progress,
    local_bin_dir, plan_install, registry, tool,
};
pub use exec::{Cmd, CmdOutput, ProcessHandle};
pub use k8s::{
    K8sClient, K8sEvent, Layout, LayoutKind, LogRing, LogSource, Namespace, Node, NodeRole,
    OwnerRef, Pod, PodPhase, Service, ServicePort, TopologyGraph, Workload, WorkloadKind,
    read_logs, watch_logs,
};
pub use kindctl::{KindCommand, parse_kind_get_clusters, parse_kind_get_nodes};
pub use kubeconfig::{KubeconfigStore, default_path};
pub use provision::{
    CreatePlan, ProvisionAction, ProvisionEvent, ProvisionStep, StepId, VerifySpec, build_plan,
    build_plan_with_cilium_version, cilium_version_for_kernel, detect_cilium_version, run_plan,
};
pub use spec::{
    CiliumOptions, ClusterSpec, Cni, DEFAULT_K8S_VERSION, DEFAULT_POD_CIDR, DEFAULT_SERVICE_CIDR,
    IngressController, KindConfig, KubernetesVersion, MAX_NAME_LEN, MAX_WORKERS, PortMapping,
    Protocol, to_kind_config, to_kind_yaml, validate, validate_name,
};
pub use state::{
    ClusterEntry, ClusterRecord, ClusterSource, ClusterState, DataDir, ReconcileReport, Settings,
};
