# How to create clusters with flannel, calico or cilium

Kind clusters are born with kindnet — kind's built-in, minimal CNI. If you
want the networking features the ecosystem actually runs on (NetworkPolicy,
encryption, eBPF service handling), you swap in a real CNI: this guide shows
all three, which kindboard installs for you automatically.

**Prerequisites:** Docker running, kind + kubectl installed (kindboard can
install those from the Dependencies panel — see
[install-kubectx.md](install-kubectx.md)). Time: ~5 minutes per cluster.

---

## 1. Open the create wizard

In the **Overview** tab, click **Create cluster**. Set a name, pick a
Kubernetes version, and choose the CNI:

![Create wizard — CNI selector](screenshots/05-wizard-cni.png)

The **Cilium extras** section appears only for cilium:

| Option | What it does |
|---|---|
| Gateway API | Installs the Gateway API CRDs and enables Cilium's Gateway API controller |
| Hubble | Enables Hubble observability (UI + relay) |
| Ingress controller | Installs Cilium's built-in ingress controller — **enabled by default** (the default ingress when none is chosen) |
| Mesh (clustermesh) | Adds a per-cluster cluster-id/cluster-name pair |

## 2. What kindboard does per CNI

Each CNI install follows the same shape — download a pinned manifest, apply
it, verify nodes become Ready — with CNI-specific details:

| CNI | Provisioning steps |
|---|---|
| **flannel** | Downloads `kube-flannel.yml` (release v0.28.9, SHA-256 pinned) → `kubectl apply` → waits for nodes Ready. The pod CIDR is patched into `net-conf.json.Network` when it differs from flannel's default `10.244.0.0/16`. |
| **calico** | Downloads the tigera operator manifest (v3.32.0, pinned) → `kubectl apply` → **waits for the operator's CRDs to become established** (the operator registers them at runtime; applying too early fails with "no matches for kind Installation") → applies the `Installation` custom resource (with your pod CIDR) → waits for nodes Ready. |
| **cilium** | Renders the kind config with `networking.kubeProxyMode: none` (Cilium fully replaces kube-proxy), then, when Gateway API is enabled, downloads and applies the Gateway API **v1.6.2** CRDs → one `cilium install` carrying **all** values: `kubeProxyReplacement=true`, `ingressController.enabled=true` (on by default), `gatewayAPI.enabled=true` when selected, and for mesh `cluster.name`/`cluster.id` + `clustermesh.apiserver.service.type=NodePort`. It adds `--version v1.21.0-pre.2` when the Docker host kernel is ≥ 7.2 (§5). Then `cilium hubble enable --relay --ui` → `cilium clustermesh enable --service-type NodePort` → verifies with `cilium status --wait`. There are no post-install `cilium upgrade` steps. |

Everything downloads over HTTPS with a pinned SHA-256; a digest mismatch
aborts the step before anything is executed.

## 3. Result — live clusters, live details

After provisioning, the Overview shows all clusters (including adopted
ones — any kind cluster on the system) with their live state, a per-cluster
node-count control (scale up/down), an **Open in k9s** button, and a guided
destroy. Every cluster opens a tab with the node list, a live topology
diagram (auto-centred), an inspectable detail panel and a log viewer:

![Overview with flannel and calico clusters](screenshots/06-overview-cni-clusters.png)

![flannel cluster tab](screenshots/07-cluster-flannel.png)

![calico cluster tab](screenshots/08-cluster-calico.png)

Clicking a node shows its role and readiness. Worker nodes offer a
**Delete node** action (a guided, type-the-name-confirmed recreate with one
fewer worker — kind has no node-level removal); the control-plane node
cannot be removed:

![Node details — control-plane node](screenshots/09-node-details.png)

## 4. Verified by the test suite

The CNI paths are covered by an end-to-end test
(`e2e_cni_matrix_flannel_calico_cilium`, gated by `KINDBOARD_E2E=1`) that
creates one cluster per CNI, runs the **full** provision plan including CNI
install and readiness verification, then confirms each cluster live: kind
lists it, the merged kubeconfig context exists, and a kube-rs topology read
returns ready nodes. The cilium leg enables the complete cilium stack
(Gateway API + Hubble + ingress controller + clustermesh).

```bash
# Run the full matrix (requires docker + kind + kubectl):
KINDBOARD_E2E=1 cargo test -p kindboard-core --test e2e \
  e2e_cni_matrix_flannel_calico_cilium -- --nocapture

# Same, but keep the clusters running afterwards (names kbcn-*):
KINDBOARD_E2E=1 KINDBOARD_E2E_KEEP=1 cargo test -p kindboard-core --test e2e \
  e2e_cni_matrix_flannel_calico_cilium -- --nocapture
```

Latest live runs (2026-09-13, kernel 7.2.4): flannel ✓ · calico ✓ · cilium ✓
(full stack — Hubble UI, Gateway API with `GatewayClass Accepted=True`, ingress
controller, clustermesh).

## 5. Known environment limitations

**cilium on kernel ≥ 7.2 — handled automatically.** Linux 7.2 added verifier
validation for the `bpf_set_retval` helper (kernel commit `b1f7f67b74c2e`).
Cilium's startup probe emits a bare `call bpf_set_retval` and treats the
verifier rejection as fatal, so **stable releases up to v1.20.1** crash-loop
the agent:

```
level=fatal msg="failed to probe helper" ... error="detect support for
FnSetRetval for program type CGroupSock: load program: invalid argument:
0: (85) call bpf_set_retval#187: R1 is not a scalar" progType=CGroupSock
helper=FnSetRetval
```

Upstream fixed this in commit `67c619cb` (issue cilium#48016); the first
release containing the fix is **v1.21.0-pre.2** (2026-09-09) — no stable
release has it as of 2026-09-13.

**kindboard handles this for you ([ADR-0014](../adrs/ADR-0014.md)).** Before building the plan,
kindboard probes the kernel the cluster's nodes run on
(`docker info --format '{{.KernelVersion}}'` — on macOS that is the Docker
Desktop VM kernel) and, for kernel ≥ 7.2, passes `--version v1.21.0-pre.2` to
the single cilium install. That install carries every value at once
(`kubeProxyReplacement=true`, ingress, Gateway API, clustermesh), so
post-install `cilium upgrade` steps no longer exist
([ADR-0015](../adrs/ADR-0015.md)) — which also removes the CLI's "release
name ... still in use" failure mode. Verified live on kernel 7.2.4: the full cilium
stack (Hubble UI, Gateway API, ingress controller, clustermesh) comes up,
`GatewayClass Accepted=True`, DNS resolves, and `cilium status` exits 0.

**Gateway API on kind — the controller works, the LoadBalancer does not
exist.** Cilium's Gateway API controller is fully functional (a `GatewayClass`
becomes `Accepted=True` and `HTTPRoute`s reconcile), but kind has no cloud
LoadBalancer: the service a user-created `Gateway` provisions stays
`EXTERNAL-IP: <pending>` until it is configured for NodePort or hostNetwork
(e.g. via a `CiliumGatewayClassConfig`). That is a kind/host limitation, not a
kindboard provisioning failure.

If the crash-loop diagnosis still fires, the automatic version selection did
not apply — check the kernel the agent sees and the fallbacks:

- `docker info --format '{{.KernelVersion}}'` — confirm the host kernel;
- boot an older kernel from the boot menu if one is installed (e.g.
  `6.19.10-300.fc44`), then re-create the cluster, or
- choose the **flannel** or **calico** CNI instead, or
- skip the cilium leg in automated runs on affected hosts:
  `KINDBOARD_E2E_SKIP_CILIUM=1`.

## Cleanup

```bash
kind delete cluster --name kbcn-flannel
kind delete cluster --name kbcn-calico
kind delete cluster --name kbcn-cilium
```

(or use the Destroy button on each cluster tab in kindboard.)
