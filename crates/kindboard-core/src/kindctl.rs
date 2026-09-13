//! The complete subprocess surface, encoded as one enum (contracts §2).
//!
//! Every CLI invocation the app needs lives here so argument construction is
//! centralized, reviewable, and testable. [`KindCommand::to_cmd`] turns a
//! variant into an [`exec::Cmd`] with an args-only argv (ADR-0009 — never
//! shell interpolation) and a per-variant timeout.
//!
//! Parsers for human-oriented `kind` output (`kind get clusters`,
//! `kind get nodes --name X`) are provided as pure functions so tests never
//! need a running cluster.

use std::path::PathBuf;
use std::time::Duration;

use crate::exec::Cmd;

/// Timeout for cheap version probes.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
/// Timeout for helm/cilium operations.
pub const INSTALL_TIMEOUT: Duration = Duration::from_secs(600);
/// Timeout for `kind create cluster` (provisioning can be slow).
pub const CREATE_TIMEOUT: Duration = Duration::from_secs(300);
/// Timeout for everything else.
pub const GENERAL_TIMEOUT: Duration = Duration::from_secs(60);

/// Exec budget for `kubectl wait` steps: must exceed the longest internal
/// `--timeout` (node-ready waits use `2m`) plus polling margin. The internal
/// kubectl timeout bounds the real work; this only prevents the runner from
/// killing a legitimately slow wait.
pub const WAIT_TIMEOUT: Duration = Duration::from_secs(180);
/// `--wait` value passed to `kind create cluster`.
pub const KIND_WAIT_FLAG: &str = "5m";

/// Every invocation the app needs, as a typed enum.
///
/// Each variant carries the *minimum* args required; the global
/// context/namespace flags are part of the variant fields where applicable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KindCommand {
    // ---- kind (cluster lifecycle) ----
    /// `kind version`
    KindVersion,
    /// `kind get clusters`
    KindGetClusters,
    /// `kind get nodes --name <cluster>`
    KindGetNodes {
        /// Cluster name.
        cluster: String,
    },
    /// `kind create cluster --config <p> [--wait 5m]`
    KindCreate {
        /// Path to the rendered v1alpha4 config YAML.
        config_path: PathBuf,
        /// Add `--wait 5m`.
        wait: bool,
    },
    /// `kind delete cluster --name <name>`
    KindDelete {
        /// Cluster name.
        name: String,
    },
    /// `kind get kubeconfig --name <name> [--internal]` (prints YAML on
    /// stdout; the kubeconfig module merges it).
    KindExportKubeconfig {
        /// Cluster name.
        name: String,
        /// Ask for the internal (node-IP) server address.
        internal: bool,
    },
    /// `kind load docker-image <img> --name <name>`
    KindLoadImage {
        /// Cluster name.
        name: String,
        /// Docker image reference.
        image: String,
    },

    // ---- docker (node/log inspection) ----
    /// `docker version --format {{.Client.Version}}`
    DockerVersion,
    /// `docker info --format {{.KernelVersion}}` — the kernel of the Docker
    /// host (on Linux the host kernel, on macOS the Docker Desktop VM
    /// kernel), i.e. the kernel kind nodes and the Cilium agent run on.
    DockerKernelVersion,
    /// `docker ps --filter name=<name>- --format json`
    DockerPs {
        /// Cluster-name prefix filter (`name=<name>-`).
        name_filter: String,
    },
    /// `docker logs [--follow] [--tail <n>] <container>`
    DockerLogs {
        /// Container name (a kind node container).
        container: String,
        /// Follow the stream instead of exiting after the current tail.
        follow: bool,
        /// Show only the last N lines (non-follow mode).
        tail: Option<u32>,
    },
    /// `docker inspect <container>`
    DockerInspect {
        /// Container name.
        container: String,
    },

    // ---- kubectl (post-create verification, apply, logs) ----
    /// `kubectl version --client --output=json`
    KubectlVersion,
    /// `kubectl --context <c> get nodes -o json`
    KubectlGetNodes {
        /// Kubeconfig context.
        context: String,
    },
    /// `kubectl --context <c> apply -f <p>` (optionally `--server-side`,
    /// needed for Gateway API CRDs).
    KubectlApply {
        /// Kubeconfig context.
        context: String,
        /// Manifest path.
        manifest_path: PathBuf,
        /// Use `kubectl apply --server-side`.
        server_side: bool,
    },
    /// `kubectl --context <c> wait <kind> <name> -n <ns> --for=<condition>
    /// --timeout=<timeout>` (omit `-n` when ns is empty; `<name>` may be
    /// `--all`).
    KubectlWait {
        /// Kubeconfig context.
        context: String,
        /// Resource kind (e.g. `nodes`, `deployment`).
        kind: String,
        /// Resource name, or `--all`.
        name: String,
        /// Namespace (empty = cluster-scoped / default).
        ns: String,
        /// Condition, e.g. `condition=Ready`.
        condition: String,
        /// `--timeout` value, e.g. `2m`.
        timeout: String,
    },
    /// `kubectl --context <c> logs <pod> [-n <ns>] [-c <container>]
    /// [-f] [--tail <N>]`
    KubectlLogs {
        /// Kubeconfig context.
        context: String,
        /// Pod name.
        pod: String,
        /// Namespace.
        ns: String,
        /// Container (omit for single-container pods).
        container: Option<String>,
        /// Follow the stream.
        follow: bool,
        /// Show the last N lines (only when not following).
        tail: Option<u32>,
    },
    /// `kubectl --context <c> get events -n <ns> -o json`
    KubectlGetEvents {
        /// Kubeconfig context.
        context: String,
        /// Namespace (empty = all namespaces).
        ns: String,
    },
    /// `kubectl --context <c> get crd <name>` (exit 0 = the CRD exists).
    /// Used by the provision runner to poll for CRDs registered at runtime
    /// by operators (e.g. tigera-operator).
    KubectlGetCrd {
        /// Kubeconfig context.
        context: String,
        /// CRD name (e.g. `installations.operator.tigera.io`).
        name: String,
    },
    /// `kubectl --context <c> get pods -n <ns> -l <label> -o json`.
    /// Used by the provision runner to diagnose failed CNI installs
    /// (e.g. crash-looping Cilium agent pods).
    KubectlGetPodsByLabel {
        /// Kubeconfig context.
        context: String,
        /// Namespace (empty = all namespaces).
        ns: String,
        /// Label selector (e.g. `k8s-app=cilium`).
        label: String,
    },

    // ---- helm (ingress + generic chart install) ----
    /// `helm version --short`
    HelmVersion,
    /// `helm install <release> <chart> [--repo <repo>] [--version <v>]
    /// -n <ns> [--create-namespace] [--set ...] [--wait watcher]
    /// --kube-context <ctx>`
    HelmInstall {
        /// Release name.
        release: String,
        /// Chart reference (repo/name or URL).
        chart: String,
        /// `--repo` URL (for charts pulled from a repo without a local
        /// `helm repo add`).
        repo: Option<String>,
        /// `--version` pin.
        version: Option<String>,
        /// Target namespace.
        ns: String,
        /// `--create-namespace`.
        create_namespace: bool,
        /// `--set key=value` values.
        sets: Vec<String>,
        /// `--wait watcher` (helm 4: wait for all resources, not just
        /// hooks).
        wait_watcher: bool,
        /// `--kube-context`.
        kube_context: String,
    },
    /// `helm upgrade --install` (idempotent re-runs); same fields as
    /// [`KindCommand::HelmInstall`].
    HelmUpgrade {
        /// Release name.
        release: String,
        /// Chart reference.
        chart: String,
        /// `--repo` URL.
        repo: Option<String>,
        /// `--version` pin.
        version: Option<String>,
        /// Target namespace.
        ns: String,
        /// `--create-namespace`.
        create_namespace: bool,
        /// `--set key=value` values.
        sets: Vec<String>,
        /// `--wait watcher`.
        wait_watcher: bool,
        /// `--kube-context`.
        kube_context: String,
    },
    /// `helm uninstall <release> -n <ns> --kube-context <ctx>`
    HelmUninstall {
        /// Release name.
        release: String,
        /// Namespace.
        ns: String,
        /// `--kube-context`.
        kube_context: String,
    },
    /// `helm repo add <name> <url>`
    HelmRepoAdd {
        /// Local repo name.
        name: String,
        /// Chart repo URL.
        url: String,
    },

    // ---- cilium CLI (CNI + extras) ----
    /// `cilium version --client`
    CiliumVersion,
    /// `cilium install [--context <c>] [--version <v>] [--set ...]
    /// [--wait]`
    CiliumInstall {
        /// Kubeconfig context.
        context: String,
        /// Cilium version to install (defaults to CLI's pinned default).
        version: Option<String>,
        /// `--set key=value` values.
        sets: Vec<String>,
        /// Add `--wait`.
        wait: bool,
    },
    /// `cilium status [--context <c>] [--wait]`
    CiliumStatus {
        /// Kubeconfig context.
        context: String,
        /// Add `--wait`.
        wait: bool,
    },
    /// `cilium hubble enable [--context <c>] [--relay] [--ui]`
    CiliumHubbleEnable {
        /// Kubeconfig context.
        context: String,
        /// Also install the Hubble UI.
        ui: bool,
        /// Also install the Hubble relay (default on upstream).
        relay: bool,
    },
    /// `cilium clustermesh enable [--context <c>] [--service-type <t>]`
    CiliumClustermeshEnable {
        /// Kubeconfig context.
        context: String,
        /// `--service-type` (kind has no LoadBalancer; use `NodePort`).
        service_type: Option<String>,
    },
    /// `cilium clustermesh connect --context <c>
    /// --destination-context <dst>`
    CiliumClustermeshConnect {
        /// Kubeconfig context.
        context: String,
        /// Destination cluster context.
        destination_context: String,
    },
    /// `cilium clustermesh disconnect --context <c>
    /// --destination-context <dst>`
    CiliumClustermeshDisconnect {
        /// Kubeconfig context.
        context: String,
        /// Destination cluster context.
        destination_context: String,
    },
}

impl KindCommand {
    /// Convert into a runnable [`Cmd`] with the exact args and the
    /// per-variant timeout (ADR-0009).
    pub fn to_cmd(&self) -> Cmd {
        let (program, args, timeout) = self.to_program_args_and_timeout();
        let mut cmd = Cmd::new(program).args(args);
        cmd = cmd.timeout(timeout);
        cmd
    }

    /// The (program, argv) pair for this variant.
    pub fn to_program_and_args(&self) -> (&'static str, Vec<String>) {
        let (program, args, _) = self.to_program_args_and_timeout();
        (program, args)
    }

    /// The per-variant timeout (ADR-0009: 30 s probes, 2 min helm/cilium,
    /// 5 min `kind create`, 60 s default).
    pub fn default_timeout(&self) -> Duration {
        self.to_program_args_and_timeout().2
    }

    fn to_program_args_and_timeout(&self) -> (&'static str, Vec<String>, Duration) {
        match self {
            KindCommand::KindVersion => ("kind", vec!["version".into()], PROBE_TIMEOUT),
            KindCommand::KindGetClusters => (
                "kind",
                vec!["get".into(), "clusters".into()],
                GENERAL_TIMEOUT,
            ),
            KindCommand::KindGetNodes { cluster } => (
                "kind",
                vec![
                    "get".into(),
                    "nodes".into(),
                    "--name".into(),
                    cluster.clone(),
                ],
                GENERAL_TIMEOUT,
            ),
            KindCommand::KindCreate { config_path, wait } => {
                let mut args = vec![
                    "create".to_string(),
                    "cluster".to_string(),
                    "--config".to_string(),
                    config_path.to_string_lossy().to_string(),
                ];
                if *wait {
                    args.push("--wait".to_string());
                    args.push(KIND_WAIT_FLAG.to_string());
                }
                ("kind", args, CREATE_TIMEOUT)
            }
            KindCommand::KindDelete { name } => (
                "kind",
                vec![
                    "delete".into(),
                    "cluster".into(),
                    "--name".into(),
                    name.clone(),
                ],
                CREATE_TIMEOUT,
            ),
            KindCommand::KindExportKubeconfig { name, internal } => {
                let mut args = vec![
                    "get".to_string(),
                    "kubeconfig".to_string(),
                    "--name".to_string(),
                    name.clone(),
                ];
                if *internal {
                    args.push("--internal".to_string());
                }
                ("kind", args, GENERAL_TIMEOUT)
            }
            KindCommand::KindLoadImage { name, image } => (
                "kind",
                vec![
                    "load".into(),
                    "docker-image".into(),
                    image.clone(),
                    "--name".into(),
                    name.clone(),
                ],
                GENERAL_TIMEOUT,
            ),
            KindCommand::DockerVersion => (
                "docker",
                vec![
                    "version".into(),
                    "--format".into(),
                    "{{.Client.Version}}".into(),
                ],
                PROBE_TIMEOUT,
            ),
            KindCommand::DockerKernelVersion => (
                "docker",
                vec![
                    "info".into(),
                    "--format".into(),
                    "{{.KernelVersion}}".into(),
                ],
                PROBE_TIMEOUT,
            ),
            KindCommand::DockerPs { name_filter } => (
                "docker",
                vec![
                    "ps".into(),
                    "--filter".into(),
                    format!("name={name_filter}-"),
                    "--format".into(),
                    "json".into(),
                ],
                GENERAL_TIMEOUT,
            ),
            KindCommand::DockerLogs {
                container,
                follow,
                tail,
            } => {
                let mut args = vec!["logs".to_string()];
                if *follow {
                    args.push("--follow".to_string());
                }
                if let Some(tail) = tail {
                    args.push("--tail".to_string());
                    args.push(tail.to_string());
                }
                args.push(container.clone());
                ("docker", args, GENERAL_TIMEOUT)
            }
            KindCommand::DockerInspect { container } => (
                "docker",
                vec!["inspect".into(), container.clone()],
                GENERAL_TIMEOUT,
            ),
            KindCommand::KubectlVersion => (
                "kubectl",
                vec!["version".into(), "--client".into(), "--output=json".into()],
                PROBE_TIMEOUT,
            ),
            KindCommand::KubectlGetNodes { context } => (
                "kubectl",
                vec![
                    "--context".into(),
                    context.clone(),
                    "get".into(),
                    "nodes".into(),
                    "-o".into(),
                    "json".into(),
                ],
                GENERAL_TIMEOUT,
            ),
            KindCommand::KubectlApply {
                context,
                manifest_path,
                server_side,
            } => {
                let mut args = vec![
                    "--context".to_string(),
                    context.clone(),
                    "apply".to_string(),
                ];
                if *server_side {
                    args.push("--server-side".to_string());
                }
                args.push("-f".to_string());
                args.push(manifest_path.to_string_lossy().to_string());
                ("kubectl", args, GENERAL_TIMEOUT)
            }
            KindCommand::KubectlWait {
                context,
                kind,
                name,
                ns,
                condition,
                timeout,
            } => {
                let mut args = vec![
                    "--context".to_string(),
                    context.clone(),
                    "wait".to_string(),
                    kind.clone(),
                    name.clone(),
                ];
                if !ns.is_empty() {
                    args.push("-n".to_string());
                    args.push(ns.clone());
                }
                args.push("--for".to_string());
                args.push(condition.clone());
                args.push("--timeout".to_string());
                args.push(timeout.clone());
                ("kubectl", args, WAIT_TIMEOUT)
            }
            KindCommand::KubectlLogs {
                context,
                pod,
                ns,
                container,
                follow,
                tail,
            } => {
                let mut args = vec![
                    "--context".to_string(),
                    context.clone(),
                    "logs".to_string(),
                    pod.clone(),
                ];
                if !ns.is_empty() {
                    args.push("-n".to_string());
                    args.push(ns.clone());
                }
                if let Some(container) = container {
                    args.push("-c".to_string());
                    args.push(container.clone());
                }
                if *follow {
                    args.push("-f".to_string());
                } else if let Some(tail) = tail {
                    args.push("--tail".to_string());
                    args.push(tail.to_string());
                }
                ("kubectl", args, GENERAL_TIMEOUT)
            }
            KindCommand::KubectlGetEvents { context, ns } => {
                let mut args = vec![
                    "--context".to_string(),
                    context.clone(),
                    "get".to_string(),
                    "events".to_string(),
                ];
                if !ns.is_empty() {
                    args.push("-n".to_string());
                    args.push(ns.clone());
                }
                args.push("-o".to_string());
                args.push("json".to_string());
                ("kubectl", args, GENERAL_TIMEOUT)
            }
            KindCommand::KubectlGetCrd { context, name } => (
                "kubectl",
                vec![
                    "--context".to_string(),
                    context.clone(),
                    "get".to_string(),
                    "crd".to_string(),
                    name.clone(),
                ],
                GENERAL_TIMEOUT,
            ),
            KindCommand::KubectlGetPodsByLabel { context, ns, label } => {
                let mut args = vec![
                    "--context".to_string(),
                    context.clone(),
                    "get".to_string(),
                    "pods".to_string(),
                ];
                if !ns.is_empty() {
                    args.push("-n".to_string());
                    args.push(ns.clone());
                }
                args.push("-l".to_string());
                args.push(label.clone());
                args.push("-o".to_string());
                args.push("json".to_string());
                ("kubectl", args, GENERAL_TIMEOUT)
            }
            KindCommand::HelmVersion => (
                "helm",
                vec!["version".into(), "--short".into()],
                PROBE_TIMEOUT,
            ),
            KindCommand::HelmInstall {
                release,
                chart,
                repo,
                version,
                ns,
                create_namespace,
                sets,
                wait_watcher,
                kube_context,
            } => (
                "helm",
                helm_install_args(
                    "install",
                    release,
                    chart,
                    repo,
                    version,
                    ns,
                    create_namespace,
                    sets,
                    wait_watcher,
                    kube_context,
                ),
                INSTALL_TIMEOUT,
            ),
            KindCommand::HelmUpgrade {
                release,
                chart,
                repo,
                version,
                ns,
                create_namespace,
                sets,
                wait_watcher,
                kube_context,
            } => (
                "helm",
                helm_install_args(
                    "upgrade",
                    release,
                    chart,
                    repo,
                    version,
                    ns,
                    create_namespace,
                    sets,
                    wait_watcher,
                    kube_context,
                ),
                INSTALL_TIMEOUT,
            ),
            KindCommand::HelmUninstall {
                release,
                ns,
                kube_context,
            } => (
                "helm",
                vec![
                    "uninstall".into(),
                    release.clone(),
                    "-n".into(),
                    ns.clone(),
                    "--kube-context".into(),
                    kube_context.clone(),
                ],
                INSTALL_TIMEOUT,
            ),
            KindCommand::HelmRepoAdd { name, url } => (
                "helm",
                vec!["repo".into(), "add".into(), name.clone(), url.clone()],
                PROBE_TIMEOUT,
            ),
            KindCommand::CiliumVersion => (
                "cilium",
                vec!["version".into(), "--client".into()],
                PROBE_TIMEOUT,
            ),
            KindCommand::CiliumInstall {
                context,
                version,
                sets,
                wait,
            } => {
                let mut args = vec![
                    "install".to_string(),
                    "--context".to_string(),
                    context.clone(),
                ];
                if let Some(version) = version {
                    args.push("--version".to_string());
                    args.push(version.clone());
                }
                for set in sets {
                    args.push("--set".to_string());
                    args.push(set.clone());
                }
                if *wait {
                    args.push("--wait".to_string());
                }
                ("cilium", args, INSTALL_TIMEOUT)
            }
            KindCommand::CiliumStatus { context, wait } => {
                let mut args = vec![
                    "status".to_string(),
                    "--context".to_string(),
                    context.clone(),
                ];
                if *wait {
                    args.push("--wait".to_string());
                }
                ("cilium", args, INSTALL_TIMEOUT)
            }
            KindCommand::CiliumHubbleEnable { context, ui, relay } => {
                let mut args = vec![
                    "hubble".to_string(),
                    "enable".to_string(),
                    "--context".to_string(),
                    context.clone(),
                ];
                if *relay {
                    args.push("--relay".to_string());
                }
                if *ui {
                    args.push("--ui".to_string());
                }
                ("cilium", args, INSTALL_TIMEOUT)
            }
            KindCommand::CiliumClustermeshEnable {
                context,
                service_type,
            } => {
                let mut args = vec![
                    "clustermesh".to_string(),
                    "enable".to_string(),
                    "--context".to_string(),
                    context.clone(),
                ];
                if let Some(service_type) = service_type {
                    args.push("--service-type".to_string());
                    args.push(service_type.clone());
                }
                ("cilium", args, INSTALL_TIMEOUT)
            }
            KindCommand::CiliumClustermeshConnect {
                context,
                destination_context,
            } => (
                "cilium",
                vec![
                    "clustermesh".into(),
                    "connect".into(),
                    "--context".into(),
                    context.clone(),
                    "--destination-context".into(),
                    destination_context.clone(),
                ],
                INSTALL_TIMEOUT,
            ),
            KindCommand::CiliumClustermeshDisconnect {
                context,
                destination_context,
            } => (
                "cilium",
                vec![
                    "clustermesh".into(),
                    "disconnect".into(),
                    "--context".into(),
                    context.clone(),
                    "--destination-context".into(),
                    destination_context.clone(),
                ],
                INSTALL_TIMEOUT,
            ),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn helm_install_args(
    verb: &str,
    release: &str,
    chart: &str,
    repo: &Option<String>,
    version: &Option<String>,
    ns: &str,
    create_namespace: &bool,
    sets: &[String],
    wait_watcher: &bool,
    kube_context: &str,
) -> Vec<String> {
    let mut args = vec![verb.to_string(), release.to_string(), chart.to_string()];
    if let Some(repo) = repo {
        args.push("--repo".to_string());
        args.push(repo.clone());
    }
    if let Some(version) = version {
        args.push("--version".to_string());
        args.push(version.clone());
    }
    args.push("-n".to_string());
    args.push(ns.to_string());
    if *create_namespace {
        args.push("--create-namespace".to_string());
    }
    for set in sets {
        args.push("--set".to_string());
        args.push(set.clone());
    }
    if *wait_watcher {
        // helm 4: `--wait` defaults to hook-only; watcher waits for all
        // resources (provisioning semantics).
        args.push("--wait".to_string());
        args.push("watcher".to_string());
    }
    args.push("--kube-context".to_string());
    args.push(kube_context.to_string());
    args
}

/// Parse `kind get clusters` output: one cluster name per line.
///
/// Tolerates empty output (no clusters) and blank lines. Non-UTF-8 bytes are
/// lossy-converted.
pub fn parse_kind_get_clusters(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// Parse `kind get nodes --name <cluster>` output: one node name per line
/// (control-plane first, then workers).
///
/// Tolerates empty output and blank lines. Non-UTF-8 bytes are
/// lossy-converted.
pub fn parse_kind_get_nodes(stdout: &str) -> Vec<String> {
    parse_kind_get_clusters(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_version_args() {
        let (prog, args) = KindCommand::KindVersion.to_program_and_args();
        assert_eq!(prog, "kind");
        assert_eq!(args, vec!["version"]);
        assert_eq!(KindCommand::KindVersion.default_timeout(), PROBE_TIMEOUT);
    }

    #[test]
    fn kind_get_clusters_args() {
        let (prog, args) = KindCommand::KindGetClusters.to_program_and_args();
        assert_eq!(prog, "kind");
        assert_eq!(args, vec!["get", "clusters"]);
    }

    #[test]
    fn kind_get_nodes_args() {
        let (prog, args) = KindCommand::KindGetNodes {
            cluster: "demo".into(),
        }
        .to_program_and_args();
        assert_eq!(args, vec!["get", "nodes", "--name", "demo"]);
        assert_eq!(prog, "kind");
    }

    #[test]
    fn kind_create_args_with_and_without_wait() {
        let (_, args) = KindCommand::KindCreate {
            config_path: PathBuf::from("/tmp/kind-demo.yaml"),
            wait: true,
        }
        .to_program_and_args();
        assert_eq!(
            args,
            vec![
                "create",
                "cluster",
                "--config",
                "/tmp/kind-demo.yaml",
                "--wait",
                "5m"
            ]
        );

        let (_, args) = KindCommand::KindCreate {
            config_path: PathBuf::from("/tmp/kind-demo.yaml"),
            wait: false,
        }
        .to_program_and_args();
        assert_eq!(
            args,
            vec!["create", "cluster", "--config", "/tmp/kind-demo.yaml"]
        );
    }

    #[test]
    fn kind_delete_args() {
        let (prog, args) = KindCommand::KindDelete {
            name: "demo".into(),
        }
        .to_program_and_args();
        assert_eq!(prog, "kind");
        assert_eq!(args, vec!["delete", "cluster", "--name", "demo"]);
    }

    #[test]
    fn export_kubeconfig_args() {
        let (_, args) = KindCommand::KindExportKubeconfig {
            name: "demo".into(),
            internal: false,
        }
        .to_program_and_args();
        assert_eq!(args, vec!["get", "kubeconfig", "--name", "demo"]);

        let (_, args) = KindCommand::KindExportKubeconfig {
            name: "demo".into(),
            internal: true,
        }
        .to_program_and_args();
        assert_eq!(
            args,
            vec!["get", "kubeconfig", "--name", "demo", "--internal"]
        );
    }

    #[test]
    fn load_image_args() {
        let (_, args) = KindCommand::KindLoadImage {
            name: "demo".into(),
            image: "nginx:latest".into(),
        }
        .to_program_and_args();
        assert_eq!(
            args,
            vec!["load", "docker-image", "nginx:latest", "--name", "demo"]
        );
    }

    #[test]
    fn docker_args() {
        let (prog, args) = KindCommand::DockerVersion.to_program_and_args();
        assert_eq!(prog, "docker");
        assert_eq!(args, vec!["version", "--format", "{{.Client.Version}}"]);

        let (prog, args) = KindCommand::DockerKernelVersion.to_program_and_args();
        assert_eq!(prog, "docker");
        assert_eq!(args, vec!["info", "--format", "{{.KernelVersion}}"]);

        let (_, args) = KindCommand::DockerPs {
            name_filter: "demo".into(),
        }
        .to_program_and_args();
        assert_eq!(
            args,
            vec!["ps", "--filter", "name=demo-", "--format", "json"]
        );

        let (_, args) = KindCommand::DockerLogs {
            container: "demo-control-plane".into(),
            follow: true,
            tail: None,
        }
        .to_program_and_args();
        assert_eq!(args, vec!["logs", "--follow", "demo-control-plane"]);

        let (_, args) = KindCommand::DockerLogs {
            container: "demo-control-plane".into(),
            follow: false,
            tail: Some(200),
        }
        .to_program_and_args();
        assert_eq!(args, vec!["logs", "--tail", "200", "demo-control-plane"]);

        let (_, args) = KindCommand::DockerInspect {
            container: "demo-worker".into(),
        }
        .to_program_and_args();
        assert_eq!(args, vec!["inspect", "demo-worker"]);
    }

    #[test]
    fn kubectl_args() {
        let (_, args) = KindCommand::KubectlVersion.to_program_and_args();
        assert_eq!(args, vec!["version", "--client", "--output=json"]);

        let (_, args) = KindCommand::KubectlGetNodes {
            context: "kind-demo".into(),
        }
        .to_program_and_args();
        assert_eq!(
            args,
            vec!["--context", "kind-demo", "get", "nodes", "-o", "json"]
        );

        let (_, args) = KindCommand::KubectlApply {
            context: "kind-demo".into(),
            manifest_path: PathBuf::from("/tmp/a.yaml"),
            server_side: false,
        }
        .to_program_and_args();
        assert_eq!(
            args,
            vec!["--context", "kind-demo", "apply", "-f", "/tmp/a.yaml"]
        );

        let (_, args) = KindCommand::KubectlApply {
            context: "kind-demo".into(),
            manifest_path: PathBuf::from("/tmp/gw.yaml"),
            server_side: true,
        }
        .to_program_and_args();
        assert_eq!(
            args,
            vec![
                "--context",
                "kind-demo",
                "apply",
                "--server-side",
                "-f",
                "/tmp/gw.yaml"
            ]
        );

        let (_, args) = KindCommand::KubectlWait {
            context: "kind-demo".into(),
            kind: "nodes".into(),
            name: "--all".into(),
            ns: String::new(),
            condition: "condition=Ready".into(),
            timeout: "2m".into(),
        }
        .to_program_and_args();
        assert_eq!(
            args,
            vec![
                "--context",
                "kind-demo",
                "wait",
                "nodes",
                "--all",
                "--for",
                "condition=Ready",
                "--timeout",
                "2m"
            ]
        );

        let (_, args) = KindCommand::KubectlLogs {
            context: "kind-demo".into(),
            pod: "web-0".into(),
            ns: "default".into(),
            container: None,
            follow: true,
            tail: None,
        }
        .to_program_and_args();
        assert_eq!(
            args,
            vec![
                "--context",
                "kind-demo",
                "logs",
                "web-0",
                "-n",
                "default",
                "-f"
            ]
        );

        let (_, args) = KindCommand::KubectlLogs {
            context: "kind-demo".into(),
            pod: "web-0".into(),
            ns: "default".into(),
            container: Some("app".into()),
            follow: false,
            tail: Some(200),
        }
        .to_program_and_args();
        assert_eq!(
            args,
            vec![
                "--context",
                "kind-demo",
                "logs",
                "web-0",
                "-n",
                "default",
                "-c",
                "app",
                "--tail",
                "200"
            ]
        );

        let (_, args) = KindCommand::KubectlGetEvents {
            context: "kind-demo".into(),
            ns: "kube-system".into(),
        }
        .to_program_and_args();
        assert_eq!(
            args,
            vec![
                "--context",
                "kind-demo",
                "get",
                "events",
                "-n",
                "kube-system",
                "-o",
                "json"
            ]
        );
    }

    #[test]
    fn kubectl_get_pods_by_label_args() {
        let (prog, args) = KindCommand::KubectlGetPodsByLabel {
            context: "kind-demo".into(),
            ns: "kube-system".into(),
            label: "app=x".into(),
        }
        .to_program_and_args();
        assert_eq!(prog, "kubectl");
        assert_eq!(
            args,
            vec![
                "--context",
                "kind-demo",
                "get",
                "pods",
                "-n",
                "kube-system",
                "-l",
                "app=x",
                "-o",
                "json"
            ]
        );

        let (_, args) = KindCommand::KubectlGetPodsByLabel {
            context: "kind-demo".into(),
            ns: String::new(),
            label: "app=x".into(),
        }
        .to_program_and_args();
        assert_eq!(
            args,
            vec![
                "--context",
                "kind-demo",
                "get",
                "pods",
                "-l",
                "app=x",
                "-o",
                "json"
            ]
        );
    }

    #[test]
    fn helm_args() {
        let (prog, args) = KindCommand::HelmVersion.to_program_and_args();
        assert_eq!(prog, "helm");
        assert_eq!(args, vec!["version", "--short"]);

        let (_, args) = KindCommand::HelmInstall {
            release: "traefik".into(),
            chart: "traefik/traefik".into(),
            repo: None,
            version: None,
            ns: "kube-system".into(),
            create_namespace: false,
            sets: vec!["ports.web.nodePort=30080".into()],
            wait_watcher: true,
            kube_context: "kind-demo".into(),
        }
        .to_program_and_args();
        assert_eq!(
            args,
            vec![
                "install",
                "traefik",
                "traefik/traefik",
                "-n",
                "kube-system",
                "--set",
                "ports.web.nodePort=30080",
                "--wait",
                "watcher",
                "--kube-context",
                "kind-demo"
            ]
        );

        let (_, args) = KindCommand::HelmUpgrade {
            release: "traefik".into(),
            chart: "traefik/traefik".into(),
            repo: None,
            version: Some("35.1.0".into()),
            ns: "kube-system".into(),
            create_namespace: true,
            sets: vec![],
            wait_watcher: false,
            kube_context: "kind-demo".into(),
        }
        .to_program_and_args();
        assert_eq!(
            args,
            vec![
                "upgrade",
                "traefik",
                "traefik/traefik",
                "--version",
                "35.1.0",
                "-n",
                "kube-system",
                "--create-namespace",
                "--kube-context",
                "kind-demo"
            ]
        );

        let (_, args) = KindCommand::HelmUninstall {
            release: "traefik".into(),
            ns: "kube-system".into(),
            kube_context: "kind-demo".into(),
        }
        .to_program_and_args();
        assert_eq!(
            args,
            vec![
                "uninstall",
                "traefik",
                "-n",
                "kube-system",
                "--kube-context",
                "kind-demo"
            ]
        );

        let (_, args) = KindCommand::HelmRepoAdd {
            name: "traefik".into(),
            url: "https://traefik.github.io/charts".into(),
        }
        .to_program_and_args();
        assert_eq!(
            args,
            vec!["repo", "add", "traefik", "https://traefik.github.io/charts"]
        );
    }

    #[test]
    fn cilium_args() {
        let (prog, args) = KindCommand::CiliumVersion.to_program_and_args();
        assert_eq!(prog, "cilium");
        assert_eq!(args, vec!["version", "--client"]);

        let (_, args) = KindCommand::CiliumInstall {
            context: "kind-demo".into(),
            version: None,
            sets: vec!["cluster.name=demo".into(), "cluster.id=1".into()],
            wait: true,
        }
        .to_program_and_args();
        assert_eq!(
            args,
            vec![
                "install",
                "--context",
                "kind-demo",
                "--set",
                "cluster.name=demo",
                "--set",
                "cluster.id=1",
                "--wait"
            ]
        );

        let (_, args) = KindCommand::CiliumStatus {
            context: "kind-demo".into(),
            wait: true,
        }
        .to_program_and_args();
        assert_eq!(args, vec!["status", "--context", "kind-demo", "--wait"]);

        let (_, args) = KindCommand::CiliumHubbleEnable {
            context: "kind-demo".into(),
            ui: true,
            relay: true,
        }
        .to_program_and_args();
        assert_eq!(
            args,
            vec![
                "hubble",
                "enable",
                "--context",
                "kind-demo",
                "--relay",
                "--ui"
            ]
        );

        let (_, args) = KindCommand::CiliumClustermeshEnable {
            context: "kind-demo".into(),
            service_type: Some("NodePort".into()),
        }
        .to_program_and_args();
        assert_eq!(
            args,
            vec![
                "clustermesh",
                "enable",
                "--context",
                "kind-demo",
                "--service-type",
                "NodePort"
            ]
        );

        let (_, args) = KindCommand::CiliumClustermeshEnable {
            context: "kind-demo".into(),
            service_type: None,
        }
        .to_program_and_args();
        assert_eq!(
            args,
            vec!["clustermesh", "enable", "--context", "kind-demo"]
        );

        let (_, args) = KindCommand::CiliumClustermeshConnect {
            context: "kind-a".into(),
            destination_context: "kind-b".into(),
        }
        .to_program_and_args();
        assert_eq!(
            args,
            vec![
                "clustermesh",
                "connect",
                "--context",
                "kind-a",
                "--destination-context",
                "kind-b"
            ]
        );
    }

    #[test]
    fn parse_clusters_output() {
        assert_eq!(
            parse_kind_get_clusters("demo\nother\n\ndemo-2 \n"),
            vec!["demo", "other", "demo-2"]
        );
        assert_eq!(parse_kind_get_clusters(""), Vec::<String>::new());
        assert_eq!(parse_kind_get_clusters("  \n\t\n"), Vec::<String>::new());
        assert_eq!(parse_kind_get_clusters("single"), vec!["single"]);
    }

    #[test]
    fn parse_nodes_output() {
        assert_eq!(
            parse_kind_get_nodes("demo-control-plane\ndemo-worker\ndemo-worker2\n"),
            vec!["demo-control-plane", "demo-worker", "demo-worker2"]
        );
        assert_eq!(parse_kind_get_nodes(""), Vec::<String>::new());
    }

    #[test]
    fn timeouts_follow_adr() {
        assert_eq!(KindCommand::KindVersion.default_timeout(), PROBE_TIMEOUT);
        assert_eq!(
            KindCommand::KindCreate {
                config_path: PathBuf::from("/x.yaml"),
                wait: true,
            }
            .default_timeout(),
            CREATE_TIMEOUT
        );
        assert_eq!(
            KindCommand::CiliumInstall {
                context: "c".into(),
                version: None,
                sets: vec![],
                wait: false,
            }
            .default_timeout(),
            INSTALL_TIMEOUT
        );
        assert_eq!(
            KindCommand::HelmInstall {
                release: "r".into(),
                chart: "c".into(),
                repo: None,
                version: None,
                ns: "n".into(),
                create_namespace: false,
                sets: vec![],
                wait_watcher: false,
                kube_context: "k".into(),
            }
            .default_timeout(),
            INSTALL_TIMEOUT
        );
        assert_eq!(
            KindCommand::DockerPs {
                name_filter: "x".into(),
            }
            .default_timeout(),
            GENERAL_TIMEOUT
        );
    }

    #[test]
    fn to_cmd_builds_runnable_cmd() {
        let cmd = KindCommand::KindGetClusters.to_cmd();
        assert_eq!(cmd.argv(), vec!["kind", "get", "clusters"]);
        assert_eq!(cmd.deadline(), GENERAL_TIMEOUT);
    }
}
