# Contributing to kindboard

Thanks for wanting to contribute. This file covers the house rules; the
architecture is documented in [docs/architecture.md](docs/architecture.md),
the contracts in [docs/contracts.md](docs/contracts.md), and decisions in
[docs/adrs/](docs/adrs/).

## Quality gates — run locally before you push

There is no CI; run these locally and keep them green before pushing. The
maintainer re-runs all three before every release.

```sh
make check      # cargo fmt --all --check · clippy -D warnings · cargo test --workspace
make audit      # RustSec advisory scan (cargo-audit)
make deny       # licenses / bans / sources / advisories against deny.toml (cargo-deny)
```

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
   `docs/adrs/` (see docs/adrs/ADR-0008.md for the format).
6. **Security findings** go through the private advisory process — see
   [SECURITY.md](SECURITY.md).

## Release process

Releases are built and published locally with `make release` (ADR-0028): it
tags `v$(VERSION)`, runs the gates and cross-builds via `scripts/build.sh`,
and publishes the `dist/` artifacts + `SHA256SUMS` to GitHub Releases as the
distribution endpoint. The GitHub Pages site ships via `make publish`. The
artifact contract lives in `scripts/build.sh` — keep `make release` in sync
with it.
