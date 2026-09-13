# kindboard

A fully open-source desktop dashboard to run and manage [kind](https://kind.sigs.k8s.io) (Kubernetes-in-Docker) clusters on Linux and macOS — built in Rust as a local development environment for testing your apps.

![License: MIT](https://img.shields.io/badge/license-MIT-blue)
![Rust](https://img.shields.io/badge/rust-1.98+-orange)
![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20macOS-9cf)

> by Orlando Rosa Pereira — [github.com/Orpere](https://github.com/Orpere)

---

## Features

- **Multiple clusters, different settings** — name, Kubernetes version, CNI (kindnet default, flannel, calico, cilium), pod/service CIDR, feature gates, extra port mappings, worker count.
- **Cilium extras** — checkboxes for API Gateway, Hubble, Ingress Controller and Mesh (clustermesh); enabling Mesh opens a per-cluster ID form (cluster-id + cluster-name).
- **Ingress controller choice** — nginx, traefik or cilium, with automatic host-port mapping (80/443).
- **Dependency manager** — detects and installs docker, kind, kubectl, helm, cilium CLI, k9s, kubectx and kustomize (brew / dnf / apt / pacman first, official binaries with SHA-256 verification as fallback), including the docker daemon state.
- **Kubeconfig lifecycle** — every cluster is added as a context to your default kubeconfig on create and removed on destroy (verified and repaired even for clusters created outside the app).
- **Per-cluster tabs with a live topology diagram** — namespaces, workloads, pods, services and ingresses as a layered graph with status colors, pan/zoom, click-to-inspect and opt-in auto-refresh.
- **Log watching** — node containers via `docker logs -f` and workload pods via `kubectl logs -f`, in bounded, follow-mode ring buffers.
- **Full cluster management** — scale workers up/down from the cluster card, delete individual worker nodes (click a node → guided, secure recreate — kind has no node-level commands), destroy with a type-the-name confirmation popup, and export logs/kubeconfig. Cluster creation output mirrors `kind create` (`✓`/`✗` steps + `kubectl cluster-info --context kind-<name>` footer) so a failed step reads exactly like the official tool text.
- **k9s integration** — an "Open in k9s" button on every cluster card and tab launches k9s for that cluster in a new terminal.
- **Official tool logos** — deps panel and buttons use the official project logos (kubectl uses the Kubernetes logo); tools without one (kubectx) get a monogram.

> **Scaling note:** kind fixes the node topology at creation time. kindboard implements
> "scale workers" / "delete node" as a one-click guided recreate that preserves all
> cluster settings, with an explicit workload-loss warning.

## Requirements

- **Docker** (running) — the only hard requirement; kindboard itself can install kind, kubectl, helm, cilium CLI, k9s, kubectx and kustomize from the Dependencies panel.
- Linux (Fedora / Ubuntu / Arch) or macOS.
- A graphical session (X11 or Wayland).

## Quick start

**Prebuilt binary** — download `kindboard-<os>-<arch>.tar.gz` from the [Releases](https://github.com/Orpere/kindboard/releases) page (or run `scripts/build.sh` to produce `dist/` yourself), extract, run `./kindboard`.

**From source** (Rust 1.98+):

```bash
make run        # run the app in debug mode
make build      # full gates + release artifacts in dist/ + SHA256SUMS
make build-all  # also try linux aarch64 and darwin targets (skips unbuildable ones)
```

## Command line

`kindboard` is a desktop app; by default it runs in the foreground of the
terminal that launched it. Everything else is opt-in:

| Flag | Effect |
|---|---|
| `--help`, `--version` | Usage / version (exits immediately) |
| `--detach` | Launch the GUI in the **background** (new session, stdio to `/dev/null`); the terminal prints the PID + log path and returns. Logs keep flowing to `<data_dir>/kindboard/kindboard.log` |
| `-v`, `--verbose` | Raise the log level — repeatable: `-v` → debug, `-vv` → trace. The simplest troubleshooting combo is `kindboard --detach -v --log-file ~/kindboard.log` |
| `--log-file <path>` | Also write log records (with timestamps) to `<path>`; in `--detach` mode defaults to `<data_dir>/kindboard/kindboard.log` |
| `--screenshot <file.png>` | Capture one screenshot then exit (CI / headless, e.g. on Xvfb) |
| `--screenshot-every <secs> [--screenshot-dir <dir>]` | Capture periodically into a directory, then exit |

Run the app from a terminal without any flag to troubleshoot interactively:
all `warn`+ records are printed to stderr; add `-v`/`-vv` for the full
subprocess and provisioning trace (exec args, provision steps, dependency
installs, topology watch errors). Detached runs always write to a log file,
so `tail -f ~/.local/share/kindboard/kindboard.log` after `--detach`.

> **Cluster creation output** mirrors `kind create cluster`: steps print as
> `Creating cluster "test" ...` with ` ✓` per completed step and ` ✗` + the
> error on failure, ending with `Set kubectl context to "kind-test"` /
> `kubectl cluster-info --context kind-test`.

## Make targets

| Target | Purpose |
|---|---|
| `make run` | Run the desktop app (debug) |
| `make check` | All quality gates: `fmt --check`, `clippy -D warnings`, tests |
| `make test` | Unit + integration tests (e2e skipped unless `KINDBOARD_E2E=1`) |
| `make e2e` | Full suite including real-kind e2e (requires docker + kind; throwaway `kbtest-*` clusters). Covers cluster create/delete, kubeconfig merge, topology, and the CNI matrix (flannel/calico/cilium full installs) |
| `make audit` | RustSec advisory scan (needs `cargo install cargo-audit`) |
| `make build` / `make dist` | Release build into `dist/` + `SHA256SUMS` (runs all gates first) |
| `make build-all` | Attempt all four targets: linux x86_64/aarch64, darwin x86_64/arm64 |
| `make assets` | Fetch + resize the official logos into `assets/` (ImageMagick) |
| `make clean` | Remove build artifacts and `dist/` |
| `make help` | List all targets |

Scripts: `scripts/build.sh` (release pipeline, reproducible tarballs) · `scripts/prepare-assets.sh` (logo pipeline). No GitHub Actions — builds are local by design.

## Documentation

| Doc | Contents |
|---|---|
| [docs/architecture.md](docs/architecture.md) | System graph, crate layout, async model, failure modes |
| [docs/contracts.md](docs/contracts.md) | Type-level contracts: spec, commands, provisioning, topology |
| [docs/dependency-install-matrix.md](docs/dependency-install-matrix.md) | Detection, package names, pinned binary downloads per tool |
| [docs/howtos/install-kubectx.md](docs/howtos/install-kubectx.md) | **How-to with screenshots:** install kubectx from the Dependencies panel |
| [docs/howtos/create-cni-clusters.md](docs/howtos/create-cni-clusters.md) | **How-to with screenshots:** flannel, calico and cilium clusters, end to end |
| [docs/adrs/](docs/adrs/) | Architecture decision records (ADR-0008 … 0013) |
| [assets/ATTRIBUTION.md](assets/ATTRIBUTION.md) | Logo ownership, sources and trademark notes |

```mermaid
graph LR
  UI[egui app] -->|CmdBus| CORE[kindboard-core]
  CORE -->|exec| KIND[kind / docker / kubectl / helm / cilium]
  CORE -->|kube-rs| API[Kubernetes API]
  CORE -->|KubeconfigStore| KC[~/.kube/config]
  CORE -->|JSON specs| STATE[~/.local/share/kindboard]
  KIND -->|create/delete/logs| CL[kind clusters]
  CORE -->|CoreEvent| UI
```

**Components** (official logos, see [assets/ATTRIBUTION.md](assets/ATTRIBUTION.md)):

| Core tooling | CNIs & ingress | Utilities |
|---|---|---|
| ![kind](assets/logos/kind-64.png) kind · ![docker](assets/logos/docker-64.png) docker · ![kubectl](assets/logos/kubectl-64.png) kubectl · ![helm](assets/logos/helm-64.png) helm · ![cilium](assets/logos/cilium-64.png) cilium CLI | ![flannel](assets/logos/flannel-64.png) flannel · ![calico](assets/logos/calico-64.png) calico · ![cilium](assets/logos/cilium-64.png) cilium · ![ingress-nginx](assets/logos/ingress-nginx-64.png) nginx ingress · ![traefik](assets/logos/traefik-64.png) traefik | ![k9s](assets/logos/k9s-64.png) k9s · ![kubectx](assets/logos/kubectx-64.png) kubectx · ![kustomize](assets/logos/kustomize-64.png) kustomize |

## License & attribution

[MIT](LICENSE) © 2026 Orlando Rosa Pereira. Project logos belong to their respective owners — see [assets/ATTRIBUTION.md](assets/ATTRIBUTION.md).
