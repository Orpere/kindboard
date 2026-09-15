# Architecture Decision Records — index

Living index of every ADR in this repository. ADRs are append-only: superseded
decisions stay in place as historical records with a `Supersedes:` /
`Superseded by` chain.

> **Note on ADR-0001–ADR-0007.** The ADR numbering started before this
> repository was published. ADRs 0001–0007 predate the public history and are
> **not included here** — they cover the initial workspace split (0001),
> scale-as-recreate (0002), the dependency-tool registry (0003), kubeconfig
> handling (0007), and related early decisions. References to them appear only
> inside older ADR bodies and are navigational dead-ends by design; the
> decisions they encoded live on in the code and in the ADRs below that
> superseded or absorbed them.

| ADR | Title | Status |
|---|---|---|
| [ADR-0008](ADR-0008.md) | Module boundaries: UI-free `kindboard-core`, thin `kindboard-app` | Accepted (supersedes unpublished ADR-0001) |
| [ADR-0009](ADR-0009.md) | Subprocess policy | Accepted |
| [ADR-0010](ADR-0010.md) | kube-rs vs kubectl-JSON for cluster reads | Accepted (supersedes unpublished ADR-0007) |
| [ADR-0011](ADR-0011.md) | State reconciliation model | Accepted (absorbs unpublished ADR-0002/0006) |
| [ADR-0012](ADR-0012.md) | Log watching design | Accepted |
| [ADR-0013](ADR-0013.md) | CLI detach, verbosity and file logging | Accepted |
| [ADR-0014](ADR-0014.md) | Cilium version selection for Linux kernel 7.2+ | Accepted (superseded in part by ADR-0015) |
| [ADR-0015](ADR-0015.md) | Cilium on kind: kube-proxy replacement and Gateway API CRDs | Accepted |
| [ADR-0016](ADR-0016.md) | Multi-theme palette registry + full viewport fit | Accepted |
| [ADR-0017](ADR-0017.md) | Darwin release builds via osxcross (local, Makefile-driven) | Accepted |
| [ADR-0018](ADR-0018.md) | Darwin release binaries are ad-hoc code-signed with rcodesign | Accepted |
| [ADR-0019](ADR-0019.md) | Windows support: windows-gnu cross-build, cfg-gated core, user-scope installer | Accepted |
| [ADR-0020](ADR-0020.md) | Cross-platform theme determinism | Accepted |
| [ADR-0021](ADR-0021.md) | Security audit (2026-09-14): Windows sha256 gate + hardening | Accepted (absorbs unpublished ADR-0007) |
| [ADR-0022](ADR-0022.md) | Hover text: gray on a neutral fill | Accepted |
| [ADR-0023](ADR-0023.md) | k9s always opens in the OS-default terminal | Accepted |
| [ADR-0024](ADR-0024.md) | Theme-aware logo tint | Superseded by ADR-0025 |
| [ADR-0025](ADR-0025.md) | Light theme logos: original brand colors | Accepted |
| [ADR-0026](ADR-0026.md) | Strong text must carry an explicit theme color | Accepted |
| [ADR-0027](ADR-0027.md) | Enterprise compliance hardening & CI adoption | Superseded by ADR-0028 |
| [ADR-0028](ADR-0028.md) | Zero paid/usage-based GitHub features: revert CI adoption, local-first pipeline | Accepted |
