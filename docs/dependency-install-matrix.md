# kindboard — Dependency Install Matrix

> The exact detection commands, package names, and fallback binary URLs
> kindboard uses for its eight managed tools.
>
> Verified 2026-09-12 against the host (Fedora 44), the Homebrew API, and each
> project's own release pages. `~/.local/bin` is prepended to `PATH` for binary
> fallbacks; every install is idempotent (skip if detection already succeeds).
>
> **How to read this document:** the summary table is the quick answer for any
> tool; the per-tool sections carry the details that bite — version schemes
> that differ from the tool's name (look at cilium, k9s, and kubectx), missing
> packages in default repos, and binary layout inside archives.

## Integrity verification (enforced since security audit 2026-09-12)

Every binary fallback download is verified against a **pinned SHA-256 digest** before it is made executable or extracted. Digests are hardcoded constants in `deps/registry.rs`, transcribed from the upstream release checksum assets:

| Tool | Digest source |
|---|---|
| kind | `kind.sigs.k8s.io/dl/v0.33.0/kind-<os>-<arch>.sha256sum` |
| kubectl | `dl.k8s.io/release/v1.37.0/bin/<os>/<arch>/kubectl.sha256` |
| helm | `get.helm.sh/helm-v4.2.2-<os>-<arch>.tar.gz.sha256sum` |
| cilium CLI | GitHub release asset `<tarball>.sha256sum` |
| k9s | GitHub release asset `checksums.sha256` |
| kubectx | GitHub release asset `checksums.txt` |
| kustomize | GitHub release asset `checksums.txt` |

All four platform variants (linux/darwin × amd64/arm64) are pinned. A digest mismatch aborts the install step. Downloads are HTTPS-only (`curl --proto=https --proto-redir=https`) with a 256 MiB size cap; provisioned manifests (flannel v0.28.9, calico v3.32.0, ingress-nginx v1.12.1, gateway-api v1.6.2) are tag-pinned **and** digest-checked at 64 MiB caps.

## Summary table

| Tool | Detect command | Version flag | brew | dnf (Fedora) | apt (Ubuntu/Debian) | pacman (Arch) | Binary fallback |
|---|---|---|---|---|---|---|---|
| docker | `docker version` | `--format '{{.Client.Version}}'` | formula `docker` (CLI) / cask `docker-desktop` | `moby-engine` *(or `docker-ce` from Docker repo)* | `docker.io` *(or `docker-ce`)* | `docker` | **none — pkg-manager-only** |
| kind | `kind version` | *(embedded)* | formula `kind` | `kind` | — (binary) | AUR `kind` | `kind.sigs.k8s.io/dl/<ver>/kind-{linux,darwin}-{amd64,arm64}` |
| kubectl | `kubectl version --client` | `--output=json` | formula `kubernetes-cli` | — *(not in default repos)* | `kubectl` (pkgs.k8s.io repo) | `kubectl` (community) | `dl.k8s.io/release/v<ver>/bin/{linux,darwin}/{amd64,arm64}/kubectl` |
| helm | `helm version` | `--short` | formula `helm` | `helm` | `helm` (helm apt repo) | `helm` (community) | `get.helm.sh/helm-v<ver>-{linux,darwin}-{amd64,arm64}.tar.gz` |
| cilium | `cilium version` | `--client` | formula `cilium-cli` | — | — | AUR `cilium-cli` | `github.com/cilium/cilium-cli/releases/download/v<ver>/cilium-{linux,darwin}-{amd64,arm64}.tar.gz` |
| k9s | `k9s version` | `--short` | formula `k9s` | `k9s` | — (or `.deb` asset) | `k9s` (community) | `github.com/derailed/k9s/releases/download/v<ver>/k9s_{Linux,Darwin}_{amd64,arm64}.tar.gz` |
| kubectx | `kubectx` | `--version` | formula `kubectx` | — | — | AUR `kubectx` | `github.com/ahmetb/kubectx/releases/download/v<ver>/kubectx_v<ver>_{linux,darwin}_{x86_64,arm64}.tar.gz` |
| kustomize | `kustomize version` | *(embedded)* | formula `kustomize` | `kustomize` | — | `kustomize` (community) | `github.com/kubernetes-sigs/kustomize/releases/download/kustomize/v<ver>/kustomize_v<ver>_{linux,darwin}_{amd64,arm64}.tar.gz` |

## Windows (ADR-0019)

Resolution order on Windows: **winget → choco → binary fallback** into
`%USERPROFILE%\.local\bin` (digests pinned as `windows-amd64` sha256, same
verification discipline). The binary fallback uses the bundled `curl.exe` +
`tar.exe` (Windows 10 1803+) and PowerShell one-liners — no chmod.

| Tool | winget id | choco pkg | Windows notes |
|---|---|---|---|
| docker | `Docker.DockerDesktop` | `docker-desktop` | Docker Desktop only (Linux containers); the UAC prompt belongs to Docker Desktop's installer, never kindboard |
| kind | `Kubernetes.kind` | `kind` | |
| kubectl | `Kubernetes.kubectl` | `kubernetes-cli` | |
| helm | `Helm.Helm` | `kubernetes-helm` | |
| cilium CLI | `Cilium.CiliumCLI` | `cilium-cli` | |
| k9s | `Derailed.k9s` | `k9s` | |
| kubectx | — | — | **unsupported on Windows** (shell scripts) — the app reports an honest error instead of an install plan |
| kustomize | `Kubernetes.kustomize` | `kustomize` | |

**Version parse results (how the app reads the detect output):**

- `docker`: `29.8.0` (bare semver).
- `kind`: `kind v0.33.0 go1.26.7 linux/amd64` → regex `v(\d+\.\d+\.\d+)`.
- `kubectl`: JSON `clientVersion.gitVersion` → `v1.37.0`.
- `helm`: `v4.2.2+gb05881c` → regex `v(\d+\.\d+\.\d+)`.
- `cilium`: `cilium version` client output (format `cilium-cli: v0.20.0 …`).
- `k9s`: `Version 0.51.0` → regex `Version\s+(\S+)`.
- `kubectx`: script; `--version` prints `v0.11.0` (v0.9.5+). Older scripts have no flag → fall back to presence + `kubectx --help` exit 0.
- `kustomize`: `v5.8.1` (bare).

## Per-tool detail

### docker

- **Detect:** `docker version --format '{{.Client.Version}}'` (also probe the **daemon**: `docker info --format '{{.ServerVersion}}'`; exit != 0 → "daemon not running").
- **Package-manager-only** (no static binary fallback — the daemon needs systemd/launchd integration and cgroups; shipping a static daemon is unsupported and dangerous).
- **Linux (dnf/apt/pacman):** `dnf install moby-engine` (Fedora-native) or `docker-ce` from Docker's repo; `apt install docker.io` (Ubuntu) or `docker-ce`; `pacman -S docker`. Post-install: `sudo systemctl enable --now docker`, and add user to the `docker` group (re-login required).
- **macOS:** no native Docker daemon. `brew install --cask docker-desktop` (Docker Desktop), or `brew install colima` (+ `brew install docker` for the CLI) or `brew install --cask orbstack`. The daemon-state check should detect whichever is running via `docker info`.

### kind

- **Detect:** `kind version` → `kind v0.33.0 …`.
- **dnf:** `kind` (in Fedora 44 repos — verified; note version may lag upstream, e.g. fc44 ships 0.31.0 while upstream is 0.33.0).
- **brew:** `brew install kind`.
- **apt:** no official package → binary fallback.
- **Binary:** `https://kind.sigs.k8s.io/dl/v0.33.0/kind-{linux,darwin}-{amd64,arm64}` (single-file binary; chmod +x into `~/.local/bin`).

### kubectl

- **Detect:** `kubectl version --client --output=json` → `clientVersion.gitVersion`.
- **brew:** `brew install kubernetes-cli`.
- **dnf:** **not in Fedora default repos** (verified) → binary fallback (or the pkgs.k8s.io `kubectl` dnf repo, which the app may optionally configure — but binary is simpler and boring).
- **apt/pacman:** `kubectl` (via pkgs.k8s.io apt repo / Arch community).
- **Binary:** `https://dl.k8s.io/release/v1.37.0/bin/{linux,darwin}/{amd64,arm64}/kubectl`.

### helm

- **Detect:** `helm version --short` → `v4.2.2+gb05881c`.
- **dnf/brew/pacman:** `helm` (Fedora 44 verified, Homebrew, Arch community).
- **apt:** `helm` via the official `baltocdn.com/helm` apt repo.
- **Binary:** `https://get.helm.sh/helm-v4.2.2-{linux,darwin}-{amd64,arm64}.tar.gz` (extract the `{os}-{arch}/helm` binary).

### cilium CLI

- **Detect:** `cilium version --client`.
- **Version scheme note:** the CLI versions independently (`v0.20.0` current) from Cilium itself (`1.20.1`). The install `--version` flag selects the *Cilium* version; the CLI's own version is separate. Do not conflate them.
- **Cilium version selection:** kindboard normally omits `--version` (CLI default = latest stable), but on Docker-host kernels ≥ 7.2 it passes `--version v1.21.0-pre.2` — the first release with the kernel-7.2 `bpf_set_retval` fix (ADR-0014).
- **brew:** `brew install cilium-cli`. **dnf/apt/pacman:** none official (AUR `cilium-cli`) → binary.
- **Binary:** `https://github.com/cilium/cilium-cli/releases/download/v0.20.0/cilium-{linux,darwin}-{amd64,arm64}.tar.gz` (+ same `.sha256sum`); extract `cilium`. Latest stable tag resolvable from `https://raw.githubusercontent.com/cilium/cilium-cli/main/stable.txt`.

### k9s

- **Detect:** `k9s version --short` → `Version 0.51.0`.
- **dnf/brew/pacman:** `k9s` (Fedora 44 verified, Homebrew, Arch community).
- **apt:** not in default repos → binary (or the `.deb` release asset).
- **Binary:** `https://github.com/derailed/k9s/releases/download/v0.51.0/k9s_{Linux,Darwin}_{amd64,arm64}.tar.gz` (extract `k9s`); `.deb`/`.rpm`/`.apk` assets also published.

### kubectx

- **Detect:** `kubectx --version` → `0.11.0`.
- **brew:** `brew install kubectx`. **dnf/apt/pacman:** not in default repos (AUR `kubectx`) → binary.
- **Binary:** `https://github.com/ahmetb/kubectx/releases/download/v0.11.0/kubectx_v0.11.0_{linux,darwin}_{x86_64,arm64}.tar.gz` — a compiled Go binary (since v0.10.0); kindboard extracts `kubectx` to `~/.local/bin` and `chmod +x`.
- **kubens:** ships in a separate upstream archive (`kubens_v0.11.0_*.tar.gz`) and is **not** installed by kindboard; available via `brew install kubectx` if needed.
- **Walkthrough:** [docs/howtos/install-kubectx.md](howtos/install-kubectx.md) (step-by-step with screenshots).

### kustomize

- **Detect:** `kustomize version` → `v5.8.1`.
- **dnf/brew/pacman:** `kustomize` (Fedora 44 verified, Homebrew, Arch community).
- **apt:** not in default repos → binary.
- **Binary:** `https://github.com/kubernetes-sigs/kustomize/releases/download/kustomize/v5.8.1/kustomize_v5.8.1_{linux,darwin}_{amd64,arm64}.tar.gz`.
- **Note:** `kubectl` v1.37.0 already embeds kustomize v5.8.1 (`kubectl version --client` reports `kustomizeVersion`); the standalone binary is only needed for direct use.

## Package-manager-only tools

- **docker** — only package-manager (daemon integration). No binary fallback; macOS uses Docker Desktop/colima/OrbStack.
- Everything else has a binary fallback to `~/.local/bin`.

## Install order & post-install

1. Detect all → show status.
2. Install missing tools in dependency order: **docker first** (kind needs it), then **kind**, **kubectl**, **helm**, **cilium**, then optional **k9s/kubectx/kustomize**.
3. After any `PATH`-changing install (`~/.local/bin`), re-run detection against the updated `PATH`.
4. Docker daemon check is separate and repeated (daemon may start after install).

## Detection semantics (implemented 2026-09-15, v0.1.10)

- Detection searches an **effective PATH**: the process PATH plus `~/.local/bin`
  (binary-fallback installs) and, on macOS, `/opt/homebrew/bin` +
  `/usr/local/bin` when present — GUI-launched apps get a minimal PATH, and
  raw-PATH-only detection produced false "not installed" results.
- The version probe parses **stdout first, then the stderr tail** when stdout
  is empty (several CLIs log their version banner to stderr).
- A **non-zero exit is not absence**: kubectx (POSIX script; old releases have
  no `--version`) reports *Installed* on presence alone; any other tool
  reports *Broken* with the exit detail.
- Installs are **idempotent and gated**: a pre-flight detection skips the
  whole plan when the tool is already installed (zero steps), and a failed
  install step re-checks reality — if the tool is present the outcome is
  success, so a package manager's "already installed" error can never read as
  a false failure.
