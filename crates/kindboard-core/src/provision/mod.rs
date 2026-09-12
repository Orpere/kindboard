//! Provisioning: the create-flow as an explicit step DAG, executed through
//! the exec runner with per-step events (contracts §4).
//!
//! [`build_plan`] turns a validated [`ClusterSpec`] into a topologically
//! sorted [`CreatePlan`] (file writes, downloads, CLI commands, kubeconfig
//! merge, verification), following the provisioning order matrix:
//!
//! | CNI | post-create steps | ingress |
//! |---|---|---|
//! | kindnet-default | — | helm install / kind deploy.yaml |
//! | flannel | apply kube-flannel.yml (pod CIDR patched if needed) | same |
//! | calico | tigera-operator + Installation CR (cidr=pod_cidr) | same |
//! | cilium | `cilium install --set ...` (+ hubble/gateway/ingress/mesh) | cilium ingress controller |
//!
//! [`run_plan`] executes the plan sequentially (steps are already in
//! dependency order; a failing step aborts the run), streaming
//! [`ProvisionEvent`]s. Scale/recreate (ADR-0002) reuses this flow with a
//! new spec.

pub mod manifests;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::error::{CoreError, ProvisionError, Result};
use crate::exec::Cmd;
use crate::kindctl::KindCommand;
use crate::kubeconfig::KubeconfigStore;
use crate::spec::{self, ClusterSpec, Cni, IngressController};

/// Id of a provisioning step (stable across runs).
pub type StepId = String;

/// One action a step performs.
///
/// Deviates from the contracts' `command: KindCommand` field by widening it
/// to an enum: the create flow needs file writes (rendered kind config,
/// Calico Installation CR, patched flannel manifest), downloads (CNI/ingress
/// manifests, Gateway API CRDs) and kubeconfig merges that are not CLI
/// invocations in the `KindCommand` surface. CLI steps are exactly
/// [`KindCommand`] variants.
#[derive(Debug, Clone)]
pub enum ProvisionAction {
    /// Run a CLI command through exec (args-only; per-variant timeout).
    Command(KindCommand),
    /// Download `url` to `dest` via `curl -L --fail` (through exec). When
    /// `expected_sha256` is set, the downloaded file must match it or the
    /// step fails (integrity check).
    Download {
        /// Source URL.
        url: &'static str,
        /// Destination file.
        dest: PathBuf,
        /// Expected SHA-256 hex digest of the downloaded content, or `None`
        /// for no check (unused — every production download pins a digest).
        expected_sha256: Option<&'static str>,
    },
    /// Write `content` to `path` (atomic-ish temp write for small files).
    WriteFile {
        /// Destination file.
        path: PathBuf,
        /// Exact content.
        content: String,
    },
    /// Run `kind get kubeconfig --name <cluster>` and merge the printed
    /// kubeconfig into `store_path` (ADR-0007/0011 safety net).
    ExportAndMergeKubeconfig {
        /// Cluster name.
        cluster_name: String,
        /// Kubeconfig file to merge into.
        store_path: PathBuf,
    },
    /// Read `source`, patch flannel's `net-conf.json.Network` to
    /// `pod_cidr`, write to `dest` (pod-CIDR consistency rule).
    PatchFlannel {
        /// Downloaded manifest.
        source: PathBuf,
        /// Patched manifest.
        dest: PathBuf,
        /// Target pod CIDR.
        pod_cidr: String,
    },
}

/// Post-condition verification of a step.
#[derive(Debug, Clone)]
pub enum VerifySpec {
    /// No verification.
    None,
    /// Run this command; exit 0 = verified.
    Command(KindCommand),
}

/// One step in the create plan.
#[derive(Debug, Clone)]
pub struct ProvisionStep {
    /// Stable step id.
    pub id: StepId,
    /// The action to perform (contract field name `command`, widened to
    /// [`ProvisionAction`]).
    pub command: ProvisionAction,
    /// Ids of steps that must succeed first (the plan is topologically
    /// sorted; this encodes the DAG edges).
    pub depends_on: Vec<StepId>,
    /// Verification run after the action succeeds.
    pub verify: VerifySpec,
}

/// The full, topologically sorted create plan for one cluster.
#[derive(Debug, Clone)]
pub struct CreatePlan {
    /// Steps in execution order.
    pub steps: Vec<ProvisionStep>,
    /// Working directory for manifests/configs (the state `tmp/` dir).
    pub data_dir: PathBuf,
    /// Kubeconfig file the merge step writes into.
    pub kubeconfig_path: PathBuf,
}

impl CreatePlan {
    /// Step ids in execution order.
    pub fn step_ids(&self) -> Vec<StepId> {
        self.steps.iter().map(|step| step.id.clone()).collect()
    }

    /// Render the plan's CLI steps as shell-ish command lines (display only;
    /// execution never goes through a shell).
    pub fn render(&self) -> Vec<String> {
        self.steps
            .iter()
            .map(|step| match &step.command {
                ProvisionAction::Command(command) => {
                    let (program, args) = command.to_program_and_args();
                    format!("[{}] {program} {}", step.id, args.join(" "))
                }
                ProvisionAction::Download { url, dest, .. } => {
                    format!("[{}] curl -L --fail -o {} {url}", step.id, dest.display())
                }
                ProvisionAction::WriteFile { path, .. } => {
                    format!("[{}] write {}", step.id, path.display())
                }
                ProvisionAction::ExportAndMergeKubeconfig { cluster_name, .. } => format!(
                    "[{}] kind get kubeconfig --name {cluster_name} → merge",
                    step.id
                ),
                ProvisionAction::PatchFlannel {
                    source,
                    dest,
                    pod_cidr,
                } => format!(
                    "[{}] patch {} → {} (network={pod_cidr})",
                    step.id,
                    source.display(),
                    dest.display()
                ),
            })
            .collect()
    }
}
/// Progress event emitted while a plan runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisionEvent {
    /// A step started.
    StepStarted {
        /// Step id.
        id: StepId,
    },
    /// A line of output from a running step.
    StepOutput {
        /// Step id.
        id: StepId,
        /// Output line.
        line: String,
    },
    /// A step finished (action + verification succeeded).
    StepFinished {
        /// Step id.
        id: StepId,
    },
    /// A step failed; the run aborts.
    StepFailed {
        /// Step id.
        id: StepId,
        /// Human-readable error.
        error: String,
    },
    /// The whole plan finished (sent exactly once, after the last step or
    /// on abort).
    PlanFinished {
        /// Whether all steps succeeded.
        success: bool,
    },
}

/// Build the create plan for a spec (validating it first).
///
/// `data_dir` is the state root (`~/.local/share/kindboard`); manifests and
/// the rendered kind config go under its `tmp/` dir. `kubeconfig_path` is
/// where the merged context is persisted.
pub fn build_plan(
    spec: &ClusterSpec,
    data_dir: &Path,
    kubeconfig_path: &Path,
) -> Result<CreatePlan> {
    spec::validate(spec)?;
    let context = KubeconfigStore::kind_context_name(&spec.name);
    let tmp = data_dir.join("tmp");
    let kind_config_path = tmp.join(format!("kind-{}.yaml", spec.name));
    let mut steps: Vec<ProvisionStep> = Vec::new();

    // 1. Render + write the kind config.
    let config_id = step(
        &mut steps,
        "write-kind-config",
        &[],
        ProvisionAction::WriteFile {
            path: kind_config_path.clone(),
            content: spec::to_kind_yaml(spec),
        },
        VerifySpec::None,
    );

    // 2. kind create.
    let create_id = step(
        &mut steps,
        "kind-create",
        &[&config_id],
        ProvisionAction::Command(KindCommand::KindCreate {
            config_path: kind_config_path,
            wait: true,
        }),
        VerifySpec::None,
    );

    // 3. Export + merge kubeconfig (ADR-0007/0011).
    let merge_id = step(
        &mut steps,
        "merge-kubeconfig",
        &[&create_id],
        ProvisionAction::ExportAndMergeKubeconfig {
            cluster_name: spec.name.clone(),
            store_path: kubeconfig_path.to_path_buf(),
        },
        VerifySpec::None,
    );

    // 4. CNI chain (per matrix).
    let mut previous = merge_id;
    let mut last_cni: Option<StepId> = None;
    match spec.cni {
        Cni::KindnetDefault => {}
        Cni::Flannel => {
            let flannel_yaml = tmp.join(format!("kube-flannel-{}.yml", spec.name));
            let download = step(
                &mut steps,
                "cni-flannel-download",
                &[&previous],
                ProvisionAction::Download {
                    url: manifests::FLANNEL_MANIFEST_URL,
                    dest: flannel_yaml.clone(),
                    expected_sha256: Some(manifests::FLANNEL_MANIFEST_SHA256),
                },
                VerifySpec::None,
            );
            let apply_path = if spec.pod_cidr != manifests::FLANNEL_DEFAULT_NETWORK {
                // Pod-CIDR consistency rule (contracts §4): patch
                // net-conf.json.Network at run time after download.
                let patched_path = tmp.join(format!("kube-flannel-{}-patched.yml", spec.name));
                let patch_id = step(
                    &mut steps,
                    "cni-flannel-patch",
                    &[&download],
                    ProvisionAction::PatchFlannel {
                        source: flannel_yaml.clone(),
                        dest: patched_path.clone(),
                        pod_cidr: spec.pod_cidr.clone(),
                    },
                    VerifySpec::None,
                );
                previous = patch_id;
                patched_path
            } else {
                previous = download;
                flannel_yaml
            };
            let apply = step(
                &mut steps,
                "cni-flannel-apply",
                &[&previous],
                ProvisionAction::Command(KindCommand::KubectlApply {
                    context: context.clone(),
                    manifest_path: apply_path,
                    server_side: false,
                }),
                VerifySpec::Command(nodes_ready(&context)),
            );
            previous = apply.clone();
            last_cni = Some(apply);
        }
        Cni::Calico => {
            let operator_yaml = tmp.join(format!("tigera-operator-{}.yaml", spec.name));
            let download = step(
                &mut steps,
                "cni-calico-operator-download",
                &[&previous],
                ProvisionAction::Download {
                    url: manifests::TIGERA_OPERATOR_URL,
                    dest: operator_yaml.clone(),
                    expected_sha256: Some(manifests::TIGERA_OPERATOR_SHA256),
                },
                VerifySpec::None,
            );
            let apply_operator = step(
                &mut steps,
                "cni-calico-operator-apply",
                &[&download],
                ProvisionAction::Command(KindCommand::KubectlApply {
                    context: context.clone(),
                    manifest_path: operator_yaml,
                    server_side: false,
                }),
                VerifySpec::None,
            );
            let cr_path = tmp.join(format!("calico-installation-{}.yaml", spec.name));
            let write_cr = step(
                &mut steps,
                "cni-calico-cr-write",
                &[&apply_operator],
                ProvisionAction::WriteFile {
                    path: cr_path.clone(),
                    content: manifests::calico_installation_cr(&spec.pod_cidr),
                },
                VerifySpec::None,
            );
            let apply_cr = step(
                &mut steps,
                "cni-calico-cr-apply",
                &[&write_cr],
                ProvisionAction::Command(KindCommand::KubectlApply {
                    context: context.clone(),
                    manifest_path: cr_path,
                    server_side: false,
                }),
                VerifySpec::Command(nodes_ready(&context)),
            );
            previous = apply_cr.clone();
            last_cni = Some(apply_cr);
        }
        Cni::Cilium => {
            let mut sets = vec![manifests::CILIUM_SET_KUBE_PROXY_DISABLED.to_string()];
            let mesh_sets: Vec<String> = match (&spec.cilium, spec.cni) {
                (Some(options), Cni::Cilium) if options.mesh => vec![
                    format!("cluster.name={}", options.cluster_name),
                    format!("cluster.id={}", options.cluster_id),
                    manifests::CILIUM_SET_MESH_NODE_PORT.to_string(),
                ],
                _ => Vec::new(),
            };
            sets.extend(mesh_sets);
            let install = step(
                &mut steps,
                "cni-cilium-install",
                &[&previous],
                ProvisionAction::Command(KindCommand::CiliumInstall {
                    context: context.clone(),
                    version: None,
                    sets,
                    wait: true,
                }),
                VerifySpec::Command(KindCommand::CiliumStatus {
                    context: context.clone(),
                    wait: true,
                }),
            );
            previous = install.clone();
            last_cni = Some(install);

            if let Some(options) = &spec.cilium {
                if options.hubble {
                    let hubble = step(
                        &mut steps,
                        "cilium-hubble-enable",
                        &[&previous],
                        ProvisionAction::Command(KindCommand::CiliumHubbleEnable {
                            context: context.clone(),
                            ui: options.hubble,
                            relay: options.hubble,
                        }),
                        VerifySpec::None,
                    );
                    previous = hubble;
                }
                if options.ingress {
                    let ingress = step(
                        &mut steps,
                        "cilium-ingress-enable",
                        &[&previous],
                        ProvisionAction::Command(KindCommand::CiliumInstall {
                            context: context.clone(),
                            version: None,
                            sets: vec![manifests::CILIUM_SET_INGRESS_ENABLED.to_string()],
                            wait: true,
                        }),
                        VerifySpec::None,
                    );
                    previous = ingress;
                }
                if options.api_gateway {
                    let crds_path = tmp.join(format!("gateway-api-crds-{}.yaml", spec.name));
                    let download = step(
                        &mut steps,
                        "cilium-gateway-crds-download",
                        &[&previous],
                        ProvisionAction::Download {
                            url: manifests::GATEWAY_API_CRDS_URL,
                            dest: crds_path.clone(),
                            expected_sha256: Some(manifests::GATEWAY_API_CRDS_SHA256),
                        },
                        VerifySpec::None,
                    );
                    let apply_crds = step(
                        &mut steps,
                        "cilium-gateway-crds-apply",
                        &[&download],
                        ProvisionAction::Command(KindCommand::KubectlApply {
                            context: context.clone(),
                            manifest_path: crds_path,
                            server_side: true,
                        }),
                        VerifySpec::None,
                    );
                    let enable = step(
                        &mut steps,
                        "cilium-gateway-enable",
                        &[&apply_crds],
                        ProvisionAction::Command(KindCommand::CiliumInstall {
                            context: context.clone(),
                            version: None,
                            sets: vec![manifests::CILIUM_SET_GATEWAY_API_ENABLED.to_string()],
                            wait: true,
                        }),
                        VerifySpec::None,
                    );
                    previous = enable;
                }
                if options.mesh {
                    let mesh = step(
                        &mut steps,
                        "cilium-clustermesh-enable",
                        &[&previous],
                        ProvisionAction::Command(KindCommand::CiliumClustermeshEnable {
                            context: context.clone(),
                        }),
                        VerifySpec::None,
                    );
                    previous = mesh;
                }
            }
        }
    }

    // 5. Ingress controller (nginx/traefik; cilium handled above).
    match spec.ingress {
        None => {}
        Some(IngressController::Cilium) => {}
        Some(IngressController::Nginx) => {
            let deploy_yaml = tmp.join(format!("ingress-nginx-{}.yaml", spec.name));
            let download = step(
                &mut steps,
                "ingress-nginx-download",
                &[&previous],
                ProvisionAction::Download {
                    url: manifests::INGRESS_NGINX_KIND_DEPLOY_URL,
                    dest: deploy_yaml.clone(),
                    expected_sha256: Some(manifests::INGRESS_NGINX_KIND_DEPLOY_SHA256),
                },
                VerifySpec::None,
            );
            let apply = step(
                &mut steps,
                "ingress-nginx-apply",
                &[&download],
                ProvisionAction::Command(KindCommand::KubectlApply {
                    context: context.clone(),
                    manifest_path: deploy_yaml,
                    server_side: false,
                }),
                VerifySpec::Command(KindCommand::KubectlWait {
                    context: context.clone(),
                    kind: "deployment".to_string(),
                    name: "ingress-nginx-controller".to_string(),
                    ns: manifests::INGRESS_NGINX_NAMESPACE.to_string(),
                    condition: "condition=Available".to_string(),
                    timeout: manifests::NODE_READY_TIMEOUT.to_string(),
                }),
            );
            previous = apply;
        }
        Some(IngressController::Traefik) => {
            let repo_add = step(
                &mut steps,
                "traefik-repo-add",
                &[&previous],
                ProvisionAction::Command(KindCommand::HelmRepoAdd {
                    name: manifests::TRAEFIK_HELM_REPO_NAME.to_string(),
                    url: manifests::TRAEFIK_HELM_REPO_URL.to_string(),
                }),
                VerifySpec::None,
            );
            let install = step(
                &mut steps,
                "traefik-install",
                &[&repo_add],
                ProvisionAction::Command(KindCommand::HelmInstall {
                    release: "traefik".to_string(),
                    chart: manifests::TRAEFIK_HELM_CHART.to_string(),
                    repo: None,
                    version: None,
                    ns: manifests::TRAEFIK_NAMESPACE.to_string(),
                    create_namespace: false,
                    sets: vec![],
                    wait_watcher: true,
                    kube_context: context.clone(),
                }),
                VerifySpec::None,
            );
            previous = install;
        }
    }

    // 6. Final readiness gate (cilium already verified after install; a
    // cheap nodes-ready check covers flannel/calico/kindnet).
    let final_verify = match spec.cni {
        Cni::Cilium => KindCommand::CiliumStatus {
            context: context.clone(),
            wait: true,
        },
        _ => nodes_ready(&context),
    };
    let _final_id = step(
        &mut steps,
        "final-verify",
        &[&previous],
        ProvisionAction::Command(final_verify),
        VerifySpec::None,
    );

    let _ = last_cni;
    Ok(CreatePlan {
        steps,
        data_dir: data_dir.to_path_buf(),
        kubeconfig_path: kubeconfig_path.to_path_buf(),
    })
}

fn nodes_ready(context: &str) -> KindCommand {
    KindCommand::KubectlWait {
        context: context.to_string(),
        kind: "nodes".to_string(),
        name: "--all".to_string(),
        ns: String::new(),
        condition: "condition=Ready".to_string(),
        timeout: manifests::NODE_READY_TIMEOUT.to_string(),
    }
}

fn step(
    steps: &mut Vec<ProvisionStep>,
    id: &str,
    depends_on: &[&StepId],
    command: ProvisionAction,
    verify: VerifySpec,
) -> StepId {
    let id = id.to_string();
    steps.push(ProvisionStep {
        id: id.clone(),
        command,
        depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
        verify,
    });
    id
}

/// Execute a plan sequentially (already in dependency order), streaming
/// events. A failing step aborts the run. `cancel` (optional) is attached
/// to every spawned subprocess and checked between steps.
pub async fn run_plan(
    plan: &CreatePlan,
    cancel: Option<CancellationToken>,
    tx: &mpsc::Sender<ProvisionEvent>,
) -> Result<()> {
    let sorted = topological_sort(plan)?;
    for id in &sorted {
        let Some(step) = plan.steps.iter().find(|s| &s.id == id) else {
            return Err(ProvisionError::InvalidPlan(format!("unknown step {id}")).into());
        };
        if let Some(token) = &cancel
            && token.is_cancelled()
        {
            let _ = tx
                .send(ProvisionEvent::PlanFinished { success: false })
                .await;
            return Err(ProvisionError::StepFailed {
                id: step.id.clone(),
                source: Box::new(CoreError::Config("cancelled".to_string())),
            }
            .into());
        }
        let _ = tx
            .send(ProvisionEvent::StepStarted {
                id: step.id.clone(),
            })
            .await;
        let result = execute_step(step, plan, cancel.as_ref(), tx).await;
        match result {
            Ok(()) => {
                let _ = tx
                    .send(ProvisionEvent::StepFinished {
                        id: step.id.clone(),
                    })
                    .await;
            }
            Err(err) => {
                let message = err.to_string();
                let _ = tx
                    .send(ProvisionEvent::StepFailed {
                        id: step.id.clone(),
                        error: message.clone(),
                    })
                    .await;
                let _ = tx
                    .send(ProvisionEvent::PlanFinished { success: false })
                    .await;
                return Err(ProvisionError::StepFailed {
                    id: step.id.clone(),
                    source: Box::new(err),
                }
                .into());
            }
        }
    }
    let _ = tx
        .send(ProvisionEvent::PlanFinished { success: true })
        .await;
    Ok(())
}

async fn execute_step(
    step: &ProvisionStep,
    plan: &CreatePlan,
    cancel: Option<&CancellationToken>,
    tx: &mpsc::Sender<ProvisionEvent>,
) -> Result<()> {
    match &step.command {
        ProvisionAction::Command(command) => {
            let mut cmd = command.to_cmd();
            if let Some(token) = cancel {
                cmd = cmd.cancel(token.clone());
            }
            run_streaming(&cmd, &step.id, tx).await?;
        }
        ProvisionAction::Download {
            url,
            dest,
            expected_sha256,
        } => {
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|source| CoreError::Io {
                        path: parent.to_path_buf(),
                        source,
                    })?;
            }
            let mut cmd = Cmd::new("curl")
                .args(["-L", "--fail", "--silent", "--show-error"])
                // HTTPS-only (initial URL and redirect targets) + a size cap
                // so a hostile or broken endpoint cannot balloon disk usage.
                .args(["--proto=https", "--proto-redir=https"])
                .args(["--max-filesize", "67108864"])
                .args(["-o"])
                .arg(dest.to_string_lossy().to_string())
                .arg(*url);
            if let Some(token) = cancel {
                cmd = cmd.cancel(token.clone());
            }
            run_streaming(&cmd, &step.id, tx).await?;
            if let Some(expected) = expected_sha256 {
                let digest = crate::fsutil::sha256_file_hex(dest)
                    .await
                    .map_err(|source| CoreError::Io {
                        path: dest.clone(),
                        source,
                    })?;
                if !digest.eq_ignore_ascii_case(expected) {
                    let _ = tokio::fs::remove_file(dest).await;
                    return Err(ProvisionError::ChecksumMismatch {
                        path: dest.clone(),
                        expected_sha256: expected,
                    }
                    .into());
                }
            }
        }
        ProvisionAction::WriteFile { path, content } => {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|source| CoreError::Io {
                        path: parent.to_path_buf(),
                        source,
                    })?;
            }
            tokio::fs::write(path, content)
                .await
                .map_err(|source| CoreError::Io {
                    path: path.clone(),
                    source,
                })?;
        }
        ProvisionAction::ExportAndMergeKubeconfig {
            cluster_name,
            store_path,
        } => {
            let mut cmd = KindCommand::KindExportKubeconfig {
                name: cluster_name.clone(),
                internal: false,
            }
            .to_cmd();
            if let Some(token) = cancel {
                cmd = cmd.cancel(token.clone());
            }
            let output = cmd.run().await?;
            let mut store = KubeconfigStore::load_from(store_path.clone())?;
            store.ensure_context_from_kind_output(cluster_name, &output.stdout())?;
            store.save()?;
        }
        ProvisionAction::PatchFlannel {
            source,
            dest,
            pod_cidr,
        } => {
            let manifest =
                tokio::fs::read_to_string(source)
                    .await
                    .map_err(|err| CoreError::Io {
                        path: source.clone(),
                        source: err,
                    })?;
            let patched =
                manifests::patch_flannel_network(&manifest, pod_cidr).ok_or_else(|| {
                    ProvisionError::InvalidPlan(format!(
                        "flannel manifest {} does not contain the expected network key",
                        source.display()
                    ))
                })?;
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|err| CoreError::Io {
                        path: parent.to_path_buf(),
                        source: err,
                    })?;
            }
            tokio::fs::write(dest, patched)
                .await
                .map_err(|err| CoreError::Io {
                    path: dest.clone(),
                    source: err,
                })?;
        }
    }

    if let VerifySpec::Command(verify) = &step.verify {
        let mut cmd = verify.to_cmd();
        if let Some(token) = cancel {
            cmd = cmd.cancel(token.clone());
        }
        cmd.run()
            .await
            .map_err(|err| ProvisionError::VerifyFailed {
                id: step.id.clone(),
                detail: err.to_string(),
            })?;
    }
    let _ = plan;
    Ok(())
}

/// Run a command, forwarding stdout lines as [`ProvisionEvent::StepOutput`].
///
/// Every forwarded line is scrubbed of kubeconfig secrets (client
/// certificates, client keys, tokens, passwords) before it enters the event
/// stream — subprocess stdout is the one channel through which kubeconfig
/// bytes could reach UI progress events, and it must stay inert there even
/// if a future step streams `kind get kubeconfig` output.
async fn run_streaming(cmd: &Cmd, step_id: &str, tx: &mpsc::Sender<ProvisionEvent>) -> Result<()> {
    let mut handle = cmd.spawn().await?;
    while let Some(line) = handle.stdout_lines().recv().await {
        let _ = tx
            .send(ProvisionEvent::StepOutput {
                id: step_id.to_string(),
                line: crate::exec::redact_kubeconfig_secrets(&line),
            })
            .await;
    }
    handle.wait().await?;
    Ok(())
}

/// Kahn's algorithm: verify the DAG is acyclic and return step ids in
/// dependency order (stable: dependencies before dependents, insertion
/// order for ties).
fn topological_sort(plan: &CreatePlan) -> Result<Vec<StepId>> {
    let mut in_degree: BTreeMap<&str, usize> = BTreeMap::new();
    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut order: Vec<&ProvisionStep> = Vec::new();
    for step in &plan.steps {
        in_degree.entry(&step.id).or_insert(0);
        order.push(step);
    }
    for step in &plan.steps {
        for dep in &step.depends_on {
            if !in_degree.contains_key(dep.as_str()) {
                return Err(ProvisionError::InvalidPlan(format!(
                    "step {} depends on unknown step {dep}",
                    step.id
                ))
                .into());
            }
            dependents.entry(dep).or_default().push(&step.id);
            *in_degree.entry(&step.id).or_default() += 1;
        }
    }
    let mut ready: Vec<&str> = plan
        .steps
        .iter()
        .filter(|step| in_degree[step.id.as_str()] == 0)
        .map(|step| step.id.as_str())
        .collect();
    let mut sorted = Vec::new();
    while let Some(next) = ready.pop() {
        sorted.push(next.to_string());
        if let Some(deps) = dependents.get(next) {
            for dependent in deps {
                if let Some(degree) = in_degree.get_mut(*dependent) {
                    *degree = degree.saturating_sub(1);
                    if *degree == 0 {
                        ready.push(dependent);
                    }
                }
            }
        }
    }
    if sorted.len() != plan.steps.len() {
        return Err(ProvisionError::InvalidPlan(
            "provisioning plan contains a dependency cycle".to_string(),
        )
        .into());
    }
    Ok(sorted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{CiliumOptions, PortMapping, Protocol};

    fn data_dir() -> PathBuf {
        std::env::temp_dir().join(format!("kindboard-provision-{}", std::process::id()))
    }

    fn base_spec() -> ClusterSpec {
        ClusterSpec {
            name: "demo".to_string(),
            ..ClusterSpec::default()
        }
    }

    fn ports() -> Vec<PortMapping> {
        vec![
            PortMapping {
                container_port: 80,
                host_port: 80,
                listen_address: "127.0.0.1".to_string(),
                protocol: Protocol::Tcp,
            },
            PortMapping {
                container_port: 443,
                host_port: 443,
                listen_address: "127.0.0.1".to_string(),
                protocol: Protocol::Tcp,
            },
        ]
    }

    fn ids(plan: &CreatePlan) -> Vec<String> {
        plan.step_ids()
    }

    fn find<'a>(plan: &'a CreatePlan, id: &str) -> &'a ProvisionStep {
        plan.steps
            .iter()
            .find(|step| step.id == id)
            .unwrap_or_else(|| panic!("step {id} not found in {:?}", plan.step_ids()))
    }

    #[test]
    fn plan_kindnet_minimal() {
        let spec = base_spec();
        let plan = build_plan(&spec, &data_dir(), Path::new("/home/u/.kube/config")).unwrap();
        assert_eq!(
            ids(&plan),
            vec![
                "write-kind-config",
                "kind-create",
                "merge-kubeconfig",
                "final-verify",
            ]
        );
        // No CNI steps for kindnet.
        let rendered = plan.render();
        assert!(!rendered.iter().any(|l| l.contains("cilium")));
        // The config file carries the rendered yaml.
        match &find(&plan, "write-kind-config").command {
            ProvisionAction::WriteFile { content, .. } => {
                assert!(content.contains("kind: Cluster"));
                assert!(!content.contains("disableDefaultCNI: true"));
            }
            other => panic!("expected WriteFile, got {other:?}"),
        }
        // create depends on write, merge on create, final on merge.
        assert_eq!(
            find(&plan, "kind-create").depends_on,
            vec!["write-kind-config"]
        );
        assert_eq!(
            find(&plan, "merge-kubeconfig").depends_on,
            vec!["kind-create"]
        );
    }

    #[test]
    fn plan_flannel_default_cidr_no_patch() {
        let mut spec = base_spec();
        spec.cni = Cni::Flannel;
        spec.pod_cidr = "10.244.0.0/16".to_string();
        let plan = build_plan(&spec, &data_dir(), Path::new("/x/config")).unwrap();
        assert_eq!(
            ids(&plan),
            vec![
                "write-kind-config",
                "kind-create",
                "merge-kubeconfig",
                "cni-flannel-download",
                "cni-flannel-apply",
                "final-verify",
            ]
        );
        match &find(&plan, "cni-flannel-apply").command {
            ProvisionAction::Command(KindCommand::KubectlApply {
                context,
                server_side,
                ..
            }) => {
                assert_eq!(context, "kind-demo");
                assert!(!server_side);
            }
            other => panic!("expected apply, got {other:?}"),
        }
        match &find(&plan, "cni-flannel-apply").verify {
            VerifySpec::Command(KindCommand::KubectlWait {
                kind,
                name,
                condition,
                ..
            }) => {
                assert_eq!(kind, "nodes");
                assert_eq!(name, "--all");
                assert_eq!(condition, "condition=Ready");
            }
            other => panic!("expected nodes-ready verify, got {other:?}"),
        }
    }

    #[test]
    fn plan_flannel_custom_cidr_patches() {
        let mut spec = base_spec();
        spec.cni = Cni::Flannel;
        spec.pod_cidr = "10.99.0.0/16".to_string();
        let plan = build_plan(&spec, &data_dir(), Path::new("/x/config")).unwrap();
        assert!(ids(&plan).contains(&"cni-flannel-patch".to_string()));
        match &find(&plan, "cni-flannel-patch").command {
            ProvisionAction::PatchFlannel { pod_cidr, .. } => {
                assert_eq!(pod_cidr, "10.99.0.0/16");
            }
            other => panic!("expected patch, got {other:?}"),
        }
        assert_eq!(
            find(&plan, "cni-flannel-apply").depends_on,
            vec!["cni-flannel-patch"]
        );
    }

    #[test]
    fn plan_calico_with_cidr_in_cr() {
        let mut spec = base_spec();
        spec.cni = Cni::Calico;
        spec.pod_cidr = "192.168.0.0/16".to_string();
        let plan = build_plan(&spec, &data_dir(), Path::new("/x/config")).unwrap();
        let expect = vec![
            "write-kind-config",
            "kind-create",
            "merge-kubeconfig",
            "cni-calico-operator-download",
            "cni-calico-operator-apply",
            "cni-calico-cr-write",
            "cni-calico-cr-apply",
            "final-verify",
        ];
        assert_eq!(ids(&plan), expect);
        match &find(&plan, "cni-calico-cr-write").command {
            ProvisionAction::WriteFile { content, .. } => {
                assert!(content.contains("cidr: 192.168.0.0/16"), "{content}");
            }
            other => panic!("expected WriteFile, got {other:?}"),
        }
    }

    #[test]
    fn plan_cilium_base_install_sets() {
        let mut spec = base_spec();
        spec.cni = Cni::Cilium;
        spec.cilium = Some(CiliumOptions::default());
        let plan = build_plan(&spec, &data_dir(), Path::new("/x/config")).unwrap();
        match &find(&plan, "cni-cilium-install").command {
            ProvisionAction::Command(KindCommand::CiliumInstall {
                context,
                version,
                sets,
                wait,
            }) => {
                assert_eq!(context, "kind-demo");
                assert!(version.is_none());
                assert_eq!(sets, &vec!["kubeProxyReplacement=disabled"]);
                assert!(*wait);
            }
            other => panic!("expected CiliumInstall, got {other:?}"),
        }
        match &find(&plan, "cni-cilium-install").verify {
            VerifySpec::Command(KindCommand::CiliumStatus { wait, .. }) => assert!(*wait),
            other => panic!("expected CiliumStatus verify, got {other:?}"),
        }
        match &find(&plan, "final-verify").command {
            ProvisionAction::Command(KindCommand::CiliumStatus { .. }) => {}
            other => panic!("expected CiliumStatus final verify, got {other:?}"),
        }
    }

    #[test]
    fn plan_cilium_all_extras_and_mesh() {
        let mut spec = base_spec();
        spec.cni = Cni::Cilium;
        spec.cilium = Some(CiliumOptions {
            api_gateway: true,
            hubble: true,
            ingress: true,
            mesh: true,
            cluster_id: 7,
            cluster_name: "mesh-a".to_string(),
        });
        let plan = build_plan(&spec, &data_dir(), Path::new("/x/config")).unwrap();
        let expect = vec![
            "write-kind-config",
            "kind-create",
            "merge-kubeconfig",
            "cni-cilium-install",
            "cilium-hubble-enable",
            "cilium-ingress-enable",
            "cilium-gateway-crds-download",
            "cilium-gateway-crds-apply",
            "cilium-gateway-enable",
            "cilium-clustermesh-enable",
            "final-verify",
        ];
        assert_eq!(ids(&plan), expect);
        match &find(&plan, "cni-cilium-install").command {
            ProvisionAction::Command(KindCommand::CiliumInstall { sets, .. }) => {
                assert_eq!(
                    sets,
                    &vec![
                        "kubeProxyReplacement=disabled",
                        "cluster.name=mesh-a",
                        "cluster.id=7",
                        "clustermesh.apiserver.service.type=NodePort",
                    ]
                );
            }
            other => panic!("expected CiliumInstall, got {other:?}"),
        }
        match &find(&plan, "cilium-hubble-enable").command {
            ProvisionAction::Command(KindCommand::CiliumHubbleEnable { ui, relay, .. }) => {
                assert!(*ui);
                assert!(*relay);
            }
            other => panic!("expected hubble, got {other:?}"),
        }
        match &find(&plan, "cilium-gateway-crds-apply").command {
            ProvisionAction::Command(KindCommand::KubectlApply { server_side, .. }) => {
                assert!(*server_side, "Gateway CRDs need --server-side");
            }
            other => panic!("expected apply, got {other:?}"),
        }
        match &find(&plan, "cilium-gateway-enable").command {
            ProvisionAction::Command(KindCommand::CiliumInstall { sets, .. }) => {
                assert_eq!(sets, &vec!["gatewayAPI.enabled=true"]);
            }
            other => panic!("expected CiliumInstall, got {other:?}"),
        }
        match &find(&plan, "cilium-clustermesh-enable").command {
            ProvisionAction::Command(KindCommand::CiliumClustermeshEnable { context }) => {
                assert_eq!(context, "kind-demo");
            }
            other => panic!("expected clustermesh, got {other:?}"),
        }
    }

    #[test]
    fn plan_ingress_nginx() {
        let mut spec = base_spec();
        spec.ingress = Some(IngressController::Nginx);
        spec.extra_port_mappings = ports();
        let plan = build_plan(&spec, &data_dir(), Path::new("/x/config")).unwrap();
        assert!(ids(&plan).contains(&"ingress-nginx-download".to_string()));
        match &find(&plan, "ingress-nginx-apply").verify {
            VerifySpec::Command(KindCommand::KubectlWait {
                kind,
                name,
                ns,
                condition,
                ..
            }) => {
                assert_eq!(kind, "deployment");
                assert_eq!(name, "ingress-nginx-controller");
                assert_eq!(ns, "ingress-nginx");
                assert_eq!(condition, "condition=Available");
            }
            other => panic!("expected wait verify, got {other:?}"),
        }
    }

    #[test]
    fn plan_ingress_traefik() {
        let mut spec = base_spec();
        spec.ingress = Some(IngressController::Traefik);
        spec.extra_port_mappings = ports();
        let plan = build_plan(&spec, &data_dir(), Path::new("/x/config")).unwrap();
        match &find(&plan, "traefik-repo-add").command {
            ProvisionAction::Command(KindCommand::HelmRepoAdd { name, url }) => {
                assert_eq!(name, "traefik");
                assert_eq!(url, "https://traefik.github.io/charts");
            }
            other => panic!("expected repo add, got {other:?}"),
        }
        match &find(&plan, "traefik-install").command {
            ProvisionAction::Command(KindCommand::HelmInstall {
                release,
                chart,
                ns,
                wait_watcher,
                kube_context,
                ..
            }) => {
                assert_eq!(release, "traefik");
                assert_eq!(chart, "traefik/traefik");
                assert_eq!(ns, "kube-system");
                assert!(*wait_watcher);
                assert_eq!(kube_context, "kind-demo");
            }
            other => panic!("expected helm install, got {other:?}"),
        }
    }

    #[test]
    fn plan_invalid_spec_fails() {
        let mut spec = base_spec();
        spec.name = "UPPER".to_string();
        let err = build_plan(&spec, &data_dir(), Path::new("/x/config")).unwrap_err();
        assert!(matches!(err, CoreError::InvalidSpec(_)));
    }

    #[test]
    fn topological_sort_orders_and_detects_cycles() {
        let spec = base_spec();
        let plan = build_plan(&spec, &data_dir(), Path::new("/x/config")).unwrap();
        let sorted = topological_sort(&plan).unwrap();
        assert_eq!(sorted, plan.step_ids());
        // Each step's dependencies appear before it.
        for step in &plan.steps {
            let pos = sorted.iter().position(|id| id == &step.id).unwrap();
            for dep in &step.depends_on {
                let dep_pos = sorted.iter().position(|id| id == dep).unwrap();
                assert!(dep_pos < pos, "{} must precede {}", dep, step.id);
            }
        }

        // A cycle is rejected.
        let cyclic = CreatePlan {
            steps: vec![
                ProvisionStep {
                    id: "a".to_string(),
                    command: ProvisionAction::WriteFile {
                        path: PathBuf::from("/tmp/a"),
                        content: String::new(),
                    },
                    depends_on: vec!["b".to_string()],
                    verify: VerifySpec::None,
                },
                ProvisionStep {
                    id: "b".to_string(),
                    command: ProvisionAction::WriteFile {
                        path: PathBuf::from("/tmp/b"),
                        content: String::new(),
                    },
                    depends_on: vec!["a".to_string()],
                    verify: VerifySpec::None,
                },
            ],
            data_dir: PathBuf::from("/tmp"),
            kubeconfig_path: PathBuf::from("/tmp/config"),
        };
        assert!(matches!(
            topological_sort(&cyclic),
            Err(CoreError::Provision(ProvisionError::InvalidPlan(_)))
        ));

        // Unknown dependency is rejected.
        let dangling = CreatePlan {
            steps: vec![ProvisionStep {
                id: "a".to_string(),
                command: ProvisionAction::WriteFile {
                    path: PathBuf::from("/tmp/a"),
                    content: String::new(),
                },
                depends_on: vec!["ghost".to_string()],
                verify: VerifySpec::None,
            }],
            data_dir: PathBuf::from("/tmp"),
            kubeconfig_path: PathBuf::from("/tmp/config"),
        };
        assert!(topological_sort(&dangling).is_err());
    }

    #[tokio::test]
    async fn run_plan_executes_steps_and_streams_events() {
        let dir =
            std::env::temp_dir().join(format!("kindboard-provision-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let out_file = dir.join("out.txt");
        let plan = CreatePlan {
            steps: vec![
                ProvisionStep {
                    id: "write".to_string(),
                    command: ProvisionAction::WriteFile {
                        path: out_file.clone(),
                        content: "hello plan".to_string(),
                    },
                    depends_on: vec![],
                    verify: VerifySpec::None,
                },
                ProvisionStep {
                    id: "echo".to_string(),
                    command: ProvisionAction::Command(crate::kindctl::KindCommand::KindGetClusters),
                    depends_on: vec!["write".to_string()],
                    verify: VerifySpec::None,
                },
            ],
            data_dir: dir.clone(),
            kubeconfig_path: dir.join("config"),
        };
        let (tx, mut rx) = mpsc::channel(64);
        run_plan(&plan, None, &tx).await.unwrap();
        assert_eq!(std::fs::read_to_string(&out_file).unwrap(), "hello plan");
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        assert!(events.contains(&ProvisionEvent::StepStarted {
            id: "write".to_string()
        }));
        assert!(events.contains(&ProvisionEvent::StepFinished {
            id: "write".to_string()
        }));
        assert!(events.contains(&ProvisionEvent::StepFinished {
            id: "echo".to_string()
        }));
        assert!(events.contains(&ProvisionEvent::PlanFinished { success: true }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn run_plan_fails_fast_and_reports() {
        let dir =
            std::env::temp_dir().join(format!("kindboard-provision-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plan = CreatePlan {
            steps: vec![
                ProvisionStep {
                    id: "boom".to_string(),
                    command: ProvisionAction::Command(KindCommand::KindGetClusters),
                    depends_on: vec![],
                    verify: VerifySpec::Command(KindCommand::KubectlApply {
                        context: "definitely-nonexistent-context".to_string(),
                        manifest_path: std::env::temp_dir().join("kindboard-does-not-exist.yaml"),
                        server_side: false,
                    }),
                },
                ProvisionStep {
                    id: "never".to_string(),
                    command: ProvisionAction::WriteFile {
                        path: dir.join("never.txt"),
                        content: String::new(),
                    },
                    depends_on: vec!["boom".to_string()],
                    verify: VerifySpec::None,
                },
            ],
            data_dir: dir.clone(),
            kubeconfig_path: dir.join("config"),
        };
        let (tx, mut rx) = mpsc::channel(64);
        let err = run_plan(&plan, None, &tx).await.unwrap_err();
        assert!(
            matches!(err, CoreError::Provision(ProvisionError::StepFailed { .. })),
            "{err:?}"
        );
        assert!(!dir.join("never.txt").exists(), "later steps must not run");
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        assert!(events.iter().any(|event| matches!(
            event,
            ProvisionEvent::StepFailed { id, .. } if id == "boom"
        )));
        assert!(events.contains(&ProvisionEvent::PlanFinished { success: false }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn run_plan_cancel_skips_remaining_steps() {
        let dir =
            std::env::temp_dir().join(format!("kindboard-provision-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plan = CreatePlan {
            steps: vec![
                ProvisionStep {
                    id: "first".to_string(),
                    command: ProvisionAction::Command(KindCommand::KindGetClusters),
                    depends_on: vec![],
                    verify: VerifySpec::None,
                },
                ProvisionStep {
                    id: "second".to_string(),
                    command: ProvisionAction::WriteFile {
                        path: dir.join("second.txt"),
                        content: "x".to_string(),
                    },
                    depends_on: vec!["first".to_string()],
                    verify: VerifySpec::None,
                },
            ],
            data_dir: dir.clone(),
            kubeconfig_path: dir.join("config"),
        };
        let token = CancellationToken::new();
        token.cancel();
        let (tx, _rx) = mpsc::channel(64);
        let err = run_plan(&plan, Some(token), &tx).await.unwrap_err();
        assert!(matches!(err, CoreError::Provision(_)), "{err:?}");
        assert!(!dir.join("second.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn assert_plan_invariants(plan: &CreatePlan) {
        let ids: Vec<StepId> = plan.step_ids();

        // Unique step ids.
        let mut uniq = ids.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), ids.len(), "duplicate step ids: {ids:?}");

        // Dependencies only reference known steps.
        for step in &plan.steps {
            for dep in &step.depends_on {
                assert!(
                    ids.contains(dep),
                    "{} depends on unknown step {dep}",
                    step.id
                );
            }
        }

        // Already topologically sorted (no re-ordering needed at run time).
        let sorted = topological_sort(plan).unwrap();
        assert_eq!(sorted, ids, "plan must be built in dependency order");

        // Every step renders a non-empty description.
        let rendered = plan.render();
        assert_eq!(rendered.len(), plan.steps.len());
        for line in &rendered {
            assert!(!line.trim().is_empty(), "empty render line");
        }

        // Command steps carry no empty argv entries (empty args would be a
        // silent subprocess corruption).
        for step in &plan.steps {
            if let ProvisionAction::Command(command) = &step.command {
                let (program, args) = command.to_program_and_args();
                assert!(!program.is_empty(), "{}: empty program", step.id);
                for arg in &args {
                    assert!(
                        !arg.is_empty(),
                        "{}: empty arg in {program} {args:?}",
                        step.id
                    );
                }
            }
        }
    }

    #[test]
    fn plan_invariants_hold_across_cni_ingress_matrix() {
        let data = data_dir();
        for cni in [Cni::KindnetDefault, Cni::Flannel, Cni::Calico, Cni::Cilium] {
            for ingress in [
                None,
                Some(IngressController::Nginx),
                Some(IngressController::Traefik),
            ] {
                let mut spec = base_spec();
                spec.cni = cni;
                if cni == Cni::Cilium {
                    spec.cilium = Some(CiliumOptions::default());
                }
                spec.ingress = ingress;
                if ingress.is_some() {
                    spec.extra_port_mappings = ports();
                }
                let plan = build_plan(&spec, &data, Path::new("/x/config")).unwrap();
                assert_plan_invariants(&plan);

                // CNI steps must precede ingress steps in every combo.
                let cni_pos = ids(&plan)
                    .iter()
                    .position(|id| id.starts_with("cni-") || id.starts_with("cilium-"));
                let ingress_pos = ids(&plan)
                    .iter()
                    .position(|id| id.starts_with("ingress-") || id.starts_with("traefik-"));
                if let (Some(cni), Some(ingress)) = (cni_pos, ingress_pos) {
                    assert!(
                        cni < ingress,
                        "{cni:?} × {ingress:?}: CNI must run before ingress"
                    );
                }
            }
        }

        // The cilium ingress controller is only valid with cilium CNI.
        let mut spec = base_spec();
        spec.cni = Cni::Cilium;
        spec.cilium = Some(CiliumOptions {
            ingress: true,
            ..CiliumOptions::default()
        });
        spec.ingress = Some(IngressController::Cilium);
        let plan = build_plan(&spec, &data, Path::new("/x/config")).unwrap();
        assert_plan_invariants(&plan);
    }

    #[test]
    fn plan_cilium_without_mesh_has_no_clustermesh_step() {
        let mut spec = base_spec();
        spec.cni = Cni::Cilium;
        spec.cilium = Some(CiliumOptions {
            mesh: false,
            ..CiliumOptions::default()
        });
        let plan = build_plan(&spec, &data_dir(), Path::new("/x/config")).unwrap();
        assert!(
            !ids(&plan).contains(&"cilium-clustermesh-enable".to_string()),
            "clustermesh must only appear when mesh=true: {:?}",
            ids(&plan)
        );
    }

    #[test]
    fn plan_cilium_ingress_controller_has_no_helm_ingress_steps() {
        let mut spec = base_spec();
        spec.cni = Cni::Cilium;
        spec.cilium = Some(CiliumOptions {
            ingress: true,
            ..CiliumOptions::default()
        });
        spec.ingress = Some(IngressController::Cilium);
        let plan = build_plan(&spec, &data_dir(), Path::new("/x/config")).unwrap();
        assert!(ids(&plan).contains(&"cilium-ingress-enable".to_string()));
        assert!(
            !ids(&plan).iter().any(|id| id.starts_with("ingress-nginx-")),
            "{:?}",
            ids(&plan)
        );
        assert!(
            !ids(&plan).iter().any(|id| id.starts_with("traefik-")),
            "{:?}",
            ids(&plan)
        );
    }

    #[test]
    fn plan_cilium_extras_ordering_invariants() {
        // cni-cilium-install must precede hubble/gateway/ingress/clustermesh;
        // every cilium extra must precede the final verify.
        let mut spec = base_spec();
        spec.cni = Cni::Cilium;
        spec.cilium = Some(CiliumOptions {
            api_gateway: true,
            hubble: true,
            ingress: true,
            mesh: true,
            cluster_id: 9,
            cluster_name: "mesh-z".to_string(),
        });
        let plan = build_plan(&spec, &data_dir(), Path::new("/x/config")).unwrap();
        let all = ids(&plan);
        let pos = |id: &str| all.iter().position(|step| step == id).unwrap();
        assert!(pos("cni-cilium-install") < pos("cilium-hubble-enable"));
        assert!(pos("cni-cilium-install") < pos("cilium-ingress-enable"));
        assert!(pos("cni-cilium-install") < pos("cilium-gateway-crds-download"));
        assert!(pos("cni-cilium-install") < pos("cilium-clustermesh-enable"));
        assert!(pos("cilium-gateway-crds-apply") < pos("cilium-gateway-enable"));
        assert!(pos("cilium-clustermesh-enable") < pos("final-verify"));
    }
}
