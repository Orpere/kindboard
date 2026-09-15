# Contributing to kindboard

Thanks for wanting to contribute. This file covers the house rules; the
architecture is documented in [docs/architecture.md](docs/architecture.md),
the contracts in [docs/contracts.md](docs/contracts.md), and decisions in
[docs/adrs/](docs/adrs/).

## Quality gates — every PR must pass

All of these run in CI (`.github/workflows/ci.yml`) and must be green to merge:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
shellcheck -S warning scripts/*.sh   # (darwin-env.sh excluded: sourced-only)
```

Plus the supply-chain gates (`.github/workflows/security.yml`):

```sh
cargo audit          # RustSec advisories
cargo deny check     # licenses / bans / sources (deny.toml)
```

Run them locally before pushing — the same commands in the workflows.

## Ground rules

1. **The risky code lives in `kindboard-core`.** It is UI-free,
   `#![forbid(unsafe_code)]`, and must never panic on external input — no
   `unwrap`/`expect`/`panic!` on anything that came from a subprocess, a
   file, or the Kubernetes API. Every failure is a typed error.
2. **No shell interpolation.** Subprocess invocations are args arrays, always
   (ADR-0009). If you need a pipeline, use the exec module's stdin/stdout
   plumbing.
3. **Downloads are pinned AND verified.** Any new download needs a pinned
   version *and* a SHA-256 digest transcribed from the upstream project's
   official checksum asset, with a comment linking the source (see
   `crates/kindboard-core/src/deps/registry.rs`).
4. **Tests for every fix.** A bug fix ships with a regression test; a feature
   ships with tests for the happy path *and* the failure paths (null, empty,
   malformed input). E2E tests self-skip unless `KINDBOARD_E2E=1` and a
   Docker daemon are present.
5. **ADR for decisions.** Anything that changes how the system works —
   architecture, security posture, release process — gets a short ADR in
   `docs/adrs/` (see ADR-0001 for the format).
6. **Security findings** go through the private advisory process — see
   [SECURITY.md](SECURITY.md).

## Release process

Releases are tag-driven: `git tag vX.Y.Z && git push --tags` builds, signs,
attests, and publishes via `.github/workflows/release.yml` (SHA256SUMS,
cosign signatures, SLSA provenance, SPDX SBOM). Local cross-builds remain
available via `make release` (see the Makefile targets); the local flow must
not diverge from the artifact contract in `scripts/build.sh`.
