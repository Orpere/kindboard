# kindboard — Type-Level Contracts

> Rust-like signatures only (no implementation code). All flag names/URLs
> verified 2026-09-12; provisioning flows exercised live 2026-09-13 (see
> `architecture.md` §9).
>
> **How to read this document:** these are the *seams* — the public types and
> command surfaces that modules agree on. Changing a type here is a breaking
> change until 1.0, so this file is the arbiter whenever two modules disagree.
> Where a type is obvious, the comment explains the *why* (kind quirks,
> compatibility constraints), not just the *what*.

## 1. Cluster specification

```rust
// kindboard_core::spec

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClusterSpec {
    pub name: String,                    // DNS-ish, [a-z0-9-], kind cluster name == kube context name
    pub k8s_version: KubernetesVersion,  // maps to node image tag kindest/node:v<version>
    pub cni: Cni,
    pub pod_cidr: String,                // "10.244.0.0/16" etc.; must match CNI network (see §4)
    pub service_cidr: String,            // "10.96.0.0/16"
    pub worker_count: u32,               // >=0; control-plane is ALWAYS exactly 1
    pub extra_port_mappings: Vec<PortMapping>, // 80/443 etc. for ingress
    pub feature_gates: BTreeMap<String, bool>,
    pub ingress: Option<IngressController>,     // None = no *explicit* ingress choice
    pub cilium: Option<CiliumOptions>,          // Some only when cni == Cilium; None resolves to defaults
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Cni {
    KindnetDefault,  // kind's built-in default CNI (disableDefaultCNI stays false)
    Flannel,
    Calico,
    Cilium,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum IngressController {
    Nginx,
    Traefik,
    Cilium,   // only valid when cni == Cilium; requires CiliumOptions.ingress == true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CiliumOptions {
    pub api_gateway: bool,     // gateway API controller (gatewayAPI.enabled)
    pub hubble: bool,          // + hubble relay & UI
    pub ingress: bool,         // ingressController.enabled
    pub mesh: bool,            // clustermesh
    pub cluster_id: u16,       // 1..=255, required iff mesh
    pub cluster_name: String,  // <=32 chars, lowercase alphanum + '-', required iff mesh
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortMapping {
    pub container_port: u16,
    pub host_port: u16,
    pub listen_address: String,   // "127.0.0.1" or "0.0.0.0"
    pub protocol: Protocol,       // TCP | UDP | SCTP
}
```

**Invariants (validated in `spec::validate`, never violated at runtime):**

- `cni == Cilium` ⇒ `cilium.is_none()` resolves to `CiliumOptions::default()` at plan build (the default enables the **Cilium ingress controller** — the default ingress when none is chosen); any other CNI ⇒ `cilium.is_none()`.
- `IngressController::Cilium` ⇒ `cni == Cilium && cilium.ingress`.
- `cilium.mesh` ⇒ `cilium.cluster_id ∈ 1..=255 && !cilium.cluster_name.is_empty()`.
- `extra_port_mappings` host ports must be unique; 80/443 must be present iff `ingress == Nginx || Traefik`.
- `pod_cidr` / `service_cidr` must be valid CIDRs and non-overlapping.
- `name` matches `^[a-z0-9][a-z0-9-]*$`, length ≤ 32 (kind requires a valid DNS label).

### 1.1 spec → kind config mapping (v1alpha4)

`spec::to_kind_config(spec) -> KindConfig` produces exactly the YAML shape kind accepts (field names verified against `pkg/apis/config/v1alpha4/types.go`):

```yaml
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
name: <spec.name>
featureGates: { ... }               # only if non-empty
networking:
  podSubnet: <spec.pod_cidr>        # only if != default or CNI != kindnet
  serviceSubnet: <spec.service_cidr>
  disableDefaultCNI: <cni != KindnetDefault>
  kubeProxyMode: none               # only when cni == Cilium (Cilium replaces kube-proxy)
nodes:
  - role: control-plane
    # k8s version is set via the node image, not a field:
    image: kindest/node:v<spec.k8s_version>
    extraPortMappings: [ ... ]      # from spec, on the control-plane node
  # then spec.worker_count × { role: worker, image: ... }
```

> **k8s version is the node image tag** (`kindest/node:vX.Y.Z`), not a dedicated config field. `KubernetesVersion` is a validated `String` (e.g. `"1.37.0"`); kind validates the tag at create time. Default for kind 0.33.x is `kindest/node:v1.37.0`.

## 2. `KindCommand` — the complete subprocess surface

Every invocation the app needs, encoded as one enum so argument construction is centralized and testable. `exec` turns each variant into an args array (ADR-0009 — never shell interpolation).

```rust
pub enum KindCommand {
    // ---- kind (cluster lifecycle) ----
    KindVersion,
    KindGetClusters,                                   // "kind get clusters"
    KindGetNodes { cluster: String },                  // "kind get nodes --name <cluster>"
    KindCreate { config_path: PathBuf, wait: bool },   // "kind create cluster --config <p> [--wait 5m]"
    KindDelete { name: String },                       // "kind delete cluster --name <name>"
    KindExportKubeconfig { name: String, internal: bool }, // "kind get kubeconfig --name <name> [--internal]"
    KindLoadImage { name: String, image: String },     // "kind load docker-image <img> --name <name>"

    // ---- docker (node/log inspection) ----
    DockerVersion,
    DockerKernelVersion,                               // "docker info --format {{.KernelVersion}}" (host/VM kernel kind nodes run on)
    DockerPs { name_filter: String },                  // "docker ps --filter name=<cluster>- --format json"
    DockerLogs { container: String, follow: bool, tail: Option<u32> }, // "docker logs [--follow] [--tail <n>] <container>"
    DockerInspect { container: String },               // "docker inspect <container>"

    // ---- kubectl (post-create verification, port-forward, logs) ----
    KubectlVersion,                                    // "kubectl version --client --output=json"
    KubectlGetNodes { context: String },               // "kubectl --context <c> get nodes -o json"
    KubectlApply { context: String, manifest_path: PathBuf, server_side: bool }, // "kubectl --context <c> apply -f <p> [--server-side]"
    KubectlWait { context: String, kind: String, name: String, ns: String, condition: String, timeout: String },
    KubectlLogs { context: String, pod: String, ns: String, container: Option<String>, follow: bool, tail: Option<u32> },
    KubectlGetEvents { context: String, ns: String },  // "kubectl --context <c> get events -n <ns> -o json"
    KubectlGetCrd { context: String, name: String },   // "kubectl --context <c> get crd <name>" (exit 0 = CRD exists)
    KubectlGetPodsByLabel { context: String, ns: String, label: String },
    // "kubectl --context <c> get pods [-n <ns>] -l <label> -o json" —
    // used to diagnose failed CNI installs (e.g. crash-looping cilium agent pods).

    // ---- helm (ingress + generic chart install) ----
    HelmVersion,                                       // "helm version --short"
    HelmInstall { release: String, chart: String, repo: Option<String>, version: Option<String>,
                  ns: String, create_namespace: bool, sets: Vec<String>, wait_watcher: bool,
                  kube_context: String },
    HelmUpgrade { /* same fields */ },
    HelmUninstall { release: String, ns: String, kube_context: String },
    HelmRepoAdd { name: String, url: String },

    // ---- cilium CLI (CNI + extras) ----
    CiliumVersion,                                     // "cilium version --client"
    CiliumInstall { context: String, version: Option<String>, sets: Vec<String>, wait: bool },
    CiliumStatus { context: String, wait: bool },      // "cilium status [--wait]"
    CiliumHubbleEnable { context: String, ui: bool, relay: bool },
    CiliumClustermeshEnable { context: String, service_type: Option<String> },
    // "cilium clustermesh enable [--context <c>] [--service-type <t>]" —
    // kind has no LoadBalancer, so service_type is NodePort (CLI cannot auto-detect).
    CiliumClustermeshConnect { context: String, destination_context: String },
    CiliumClustermeshDisconnect { context: String, destination_context: String },
}
```

Each variant carries the *minimum* args required, including its kube context where applicable (there is no separate `ClusterTarget` struct — context is per-variant). Exit code 0 is the only success signal; stderr is captured (last 2 KiB) for `Error::Command`.

## 3. Dependency tool registry

```rust
pub struct Tool {
    pub id: ToolId,                 // Docker, Kind, Kubectl, Helm, Cilium, K9s, Kubectx, Kustomize
    pub display: &'static str,
    pub detect: DetectSpec,         // command + version flag + regex to parse
    pub install: InstallRecipe,     // per-platform package + binary fallback
}

pub enum ToolId { Docker, Kind, Kubectl, Helm, Cilium, K9s, Kubectx, Kustomize }

pub struct DetectSpec {
    pub command: &'static str,          // e.g. "kubectl"
    pub version_args: &'static [&'static str], // e.g. ["version","--client","--output=json"]
    pub parse: VersionParse,            // JsonField("clientVersion.gitVersion") | Regex(...) | ShortLine
}

pub enum VersionParse {
    JsonField(&'static str),            // dot-path into JSON output
    Regex(&'static str),                // capture group 1
    ShortLine,                          // strip prefix up to first 'v'
}

pub struct InstallRecipe {
    pub brew: Option<&'static str>,     // formula/cask name
    pub dnf: Option<&'static str>,
    pub apt: Option<&'static str>,
    pub pacman: Option<&'static str>,
    pub binary: Option<BinaryDownload>, // fallback to ~/.local/bin
    pub pkg_manager_only: bool,         // true when no binary fallback exists
    pub post_install: Option<&'static str>, // e.g. docker daemon enable/start note
}
```

Exact values (per-platform package names, detect commands, and fallback URLs) live in **`docs/dependency-install-matrix.md`** and are embedded here by reference. The registry is data, not scattered `match` arms, so adding a tool is one struct literal + one test.

## 4. Provisioning order matrix

`provision::build_plan(spec) -> Vec<ProvisionStep>` where `ProvisionStep { id, command: ProvisionAction, depends_on: Vec<StepId>, verify: VerifySpec }`. The DAG is topologically sorted; each step runs only after its dependencies' verification passes.

`ProvisionAction` is a superset of `KindCommand` (the subprocess surface) plus in-process steps the create flow needs: `Download` (pinned manifest fetch), `WriteFile` (rendered manifests/CRs), `PatchFlannel` (pod-CIDR override in `kube-flannel.yml`), and `ExportAndMergeKubeconfig`.

| CNI | disableDefaultCNI | post-create CNI step(s) | ingress (nginx/traefik) | ingress (cilium) | cilium extras |
|---|---|---|---|---|---|
| kindnet-default | false | — | helm install (80/443 already mapped) | n/a | n/a |
| flannel | true | `kubectl apply kube-flannel.yml` | helm install | n/a | n/a |
| calico | true | `kubectl create tigera-operator.yaml` → apply `Installation` CR (cidr=pod_cidr) | helm install | n/a | n/a |
| cilium | true (`kubeProxyMode: none`) | Gateway API v1.6.2 CRDs (server-side) when `api_gateway`, then one `cilium install --set kubeProxyReplacement=true [--set cluster.name/--set cluster.id if mesh] [--set ingressController.enabled=true] [--set gatewayAPI.enabled=true]` | n/a | value carried by the base install | hubble / clustermesh after install |

**Pod-CIDR consistency rule (applies to flannel & calico):** with `disableDefaultCNI: true`, kind does **not** install a CNI, but `kube-controller-manager --cluster-cidr` still allocates node `podCIDR`s from `podSubnet`. The CNI's own network must match:

- **flannel**: default network is `10.244.0.0/16`. If `pod_cidr != 10.244.0.0/16`, the app must post-process `kube-flannel.yml`'s `net-conf.json.Network` before apply.
- **calico**: default pool is `192.168.0.0/16`. The app sets the `Installation.spec.calicoNetwork.ipPools[0].cidr = pod_cidr`.
- **cilium**: auto-derives from node `podCIDR`; no extra pod-CIDR wiring.

**Calico install method — decision: Tigera operator (not classic `calico.yaml`).** Justification: the operator is Calico's officially recommended install path, keeps the same two-step shape on every cluster (`kubectl create tigera-operator.yaml` → apply an `Installation` CR whose `ipPools[0].cidr` cleanly encodes the pod-CIDR override), and self-heals/upgrades. The classic single-file `calico.yaml` manifest is rejected because pod-CIDR changes require editing an embedded manifest field — the operator exposes it as a first-class CR field. Both manifests verified present at `projectcalico/calico v3.32.0`; operator chosen for the cleaner CIDR override, not for extra features.

**Cilium extras (all via cilium CLI, all `--context <cluster>`):**

| Extra | Commands | Notes |
|---|---|---|
| Hubble | `cilium hubble enable --relay --ui` | `--relay` default true, `--ui` optional; runs after the base install |
| Ingress controller | `--set ingressController.enabled=true` on the base install | creates a LoadBalancer service. kind renders `kubeProxyMode: none` and kindboard installs Cilium with `kubeProxyReplacement=true`, so Cilium fully replaces kube-proxy. **Note:** the `cilium ingress enable` subcommand was removed from current cilium-cli — use `--set`; values ride the single base install, no post-install `cilium upgrade`. |
| API Gateway | apply Gateway API **v1.6.2** standard CRDs (`kubectl apply --server-side …`) **before** the base install, then `--set gatewayAPI.enabled=true` on the base install | v1.6.2 is required because Cilium >= 1.21 needs `tlsroutes`/`referencegrants` v1; v1.4.0 ships only v1alpha3/v1beta1. The operator caches CRD discovery at startup, so the CRDs must exist before the install (a post-install `cilium upgrade` does not restart the operator). Cilium's Gateway API controller is disabled unless kube-proxy replacement is on. **no `--gateway-api` flag and no `cilium gateway-api` command exist** in current cilium-cli — see §6 |
| Mesh (clustermesh) | `--set cluster.name=X --set cluster.id=N --set clustermesh.apiserver.service.type=NodePort` on the base install → `cilium clustermesh enable --service-type NodePort` → `cilium clustermesh connect --destination-context <other>` | NodePort required on kind (no LoadBalancer); IDs unique across mesh |

**Kernel >= 7.2 version pinning:** on Linux kernels >= 7.2 stable Cilium crash-loops at startup on the `bpf_set_retval` probe (upstream cilium#48016; no stable release contains the fix as of 2026-09-13). The plan probes the Docker host kernel (`docker info --format '{{.KernelVersion}}'`) and installs `CILIUM_VERSION_KERNEL_72` (`v1.21.0-pre.2`) when `(major, minor) >= (7, 2)`, otherwise the cilium CLI default. The version is pinned on the single base install; only hubble/mesh use post-install CLI commands.

### 4.1 Create flow (sequence)

```mermaid
sequenceDiagram
  participant U as User (UI)
  participant P as provision
  participant K as kind
  participant C as kubectl/helm/cilium
  participant KC as kubeconfig

  U->>P: Create(spec)
  P->>P: validate(spec) — fail fast on invariant
  P->>K: kind create cluster --config tmp/kind-<name>.yaml --wait 5m
  K-->>P: exit 0 (else Error::Command)
  P->>KC: merge context (kube::config::Kubeconfig::merge + atomic write)
  alt CNI != kindnet-default
    P->>C: install CNI (flannel/calico/cilium per matrix)
    C-->>P: verify: kubectl get nodes Ready / cilium status --wait
  end
  opt ingress nginx/traefik
    P->>C: helm repo add + helm install (wait watcher)
  end
  opt cilium extras
    P->>C: Gateway API CRDs (server-side, before install); base install carries ingress/gateway/mesh values; hubble/clustermesh after
  end
  P->>P: persist ClusterSpec (ADR-0011), emit Event::ClusterReady
```

**Scale/delete-node (ADR-0011):** kind has **no** node add/remove commands (verified against `kind --help`). The flow is an explicit guided recreate: `persist spec → warn user (workload loss) → kind delete --name → kind create with new worker_count → re-provision CNI/ingress/cilium`. The stored spec is the only input; it is not mutated until the recreate succeeds.

## 5. Topology data model

```rust
pub struct TopologyGraph {
    pub namespaces: Vec<Namespace>,
    pub workloads: Vec<Workload>,   // Deployment | StatefulSet | DaemonSet | Job
    pub pods: Vec<Pod>,
    pub services: Vec<Service>,
    pub ingresses: Vec<Ingress>,
    pub nodes: Vec<Node>,
    pub events: Vec<K8sEvent>,
    pub layout: Layout,             // computed positions for rendering
}

pub struct Namespace { pub name: String }
pub struct Workload { pub kind: WorkloadKind, pub name: String, pub ns: String, pub selector: BTreeMap<String,String> }
pub struct Pod { pub name: String, pub ns: String, pub owner: Option<OwnerRef>, pub phase: PodPhase, pub node: Option<String> }
pub struct Service { pub name: String, pub ns: String, pub selector: BTreeMap<String,String>, pub ports: Vec<ServicePort> }
pub struct Ingress { pub name: String, pub ns: String, pub class: String, pub backend_services: Vec<String>, pub hosts: Vec<String> }
pub struct Node { pub name: String, pub role: NodeRole, pub ready: bool }
pub struct K8sEvent { pub ns: String, pub kind: String, pub name: String, pub reason: String, pub message: String, pub timestamp: chrono::DateTime<chrono::Utc> }

pub enum WorkloadKind { Deployment, StatefulSet, DaemonSet, Job }
pub enum PodPhase { Running, Pending, Succeeded, Failed, Unknown }
pub enum NodeRole { ControlPlane, Worker }
```

**Edges (selector-based, computed not stored):**

- `Workload → Pod` via `ownerReferences` (direct owner).
- `Service → Workload` via `service.selector` matching `workload` pod template labels.
- `Ingress → Service` via `ingress` backend service names.
- `Pod → Node` via `pod.spec.nodeName`.

**Layout approach (layered DAG, deterministic):**

- Layers: `namespaces (rank 0) → workloads (1) → pods (2) → services (3) → ingresses (4)`. Nodes within a layer are laid out top-to-bottom by name (Sugiyama-style: minimize edge crossings with a simple barycenter pass; no force simulation in v1 — YAGNI).
- Compute in core (`topology::layout`) so the app's `diagram.rs` is a pure painter. Recompute only on graph change; the UI caches positions between polls.
- Status colors derive from `PodPhase`/`Node.ready`/workload readiness (aggregated): ready=green, pending=amber, failed=red, unknown=grey.

## 6. Cross-cutting contract notes

- **All paths** are `PathBuf` resolved against `dirs::data_local_dir()`; never string-joined with `/`.
- **All external versions** flow through `DetectSpec::parse` into a `Version { major, minor, patch }` struct, never raw strings, so "is kind ≥ 0.20?" is a typed comparison.
- **Idempotency:** `kind create` fails if the cluster exists (`Error::ClusterExists`) — the app pre-checks `kind get clusters`. `helm install` is `helm upgrade --install` for idempotent re-runs. `cilium install` is idempotent (helm under the hood).
- **Atomic writes** everywhere (spec JSON, settings, kubeconfig) per ADR-0021; kubeconfig via `kube::config::Kubeconfig` (serde round-trip), never raw text munging.

## 7. Corrections to the brief (verified against current cilium-cli)

Two product-brief assumptions do **not** match the current cilium-cli (main, cilium 1.20.1 era). Flagging for client sign-off:

1. **"Gateway API via `--gateway-api`"** — current cilium-cli has **no** `--gateway-api` flag and no `cilium gateway-api` subcommand. Gateway API is enabled with the Helm value `gatewayAPI.enabled=true` (via `cilium install --set gatewayAPI.enabled=true`, plus installing the Gateway API CRDs first). Source: `cilium-cli/cli/install.go` flag list; cilium `Documentation/network/servicemesh/gateway-api/installation.rst`.
2. **"cilium ingress enable"** — the `cilium ingress` subcommand was removed from cilium-cli. Ingress is enabled with `--set ingressController.enabled=true` (documented in cilium ingress guide). Source: cilium-cli `cli/` directory contains only `clustermesh`, `hubble`, `install`, `status`, `sysdump`, `uninstall`, `upgrade`; cilium `Documentation/network/servicemesh/ingress.rst`.

Both are absorbed into the `KindCommand`/provisioning matrix (§2, §4) using `--set` flags, so no feature is lost — only the invocation mechanism changes.
