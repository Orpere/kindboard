# Security Policy

kindboard manages local Kubernetes clusters, your `~/.kube/config`, and
installs developer tools on your machine. Security reports are welcome and
treated seriously.

## Reporting a vulnerability

**Do not open a public issue for a security vulnerability.**

Report it privately through GitHub Security Advisories:

1. Go to <https://github.com/Orpere/kindboard/security/advisories>
2. Click **Report a vulnerability** and describe the issue:
   - affected version(s)
   - steps to reproduce
   - impact (what an attacker gains, and what access they already need)
   - any suggested fix

You will receive a response within **7 days**. Please allow up to **90 days**
before disclosing publicly so a fix and release can be prepared; we will
coordinate the disclosure and credit you in the release notes (unless you ask
to stay anonymous).

## Supported versions

| Version | Supported |
|---|---|
| Latest release (v0.x) | ✅ |
| Older releases | ❌ — upgrade to the latest release |

There is no LTS track; fixes are released as the next patch/minor version.

## Scope

In scope: the kindboard desktop app, its installers (`scripts/install-*`),
the release pipeline (`scripts/build.sh`, `Makefile`), and the website
(`web/`). Findings in the binary dependencies kindboard drives (kind,
kubectl, helm, Cilium CLI, Docker) should be reported upstream to those
projects; kindboard pins their versions and verifies their digests, but does
not ship their code.

Out of scope: social engineering, denial-of-service of the GitHub Pages site,
and theoretical attacks that require the attacker to already control your
user account (that is game over regardless of any tool).

## What kindboard already does (design guarantees)

- **No shell interpolation** — every subprocess runs via an args array
  (ADR-0009); there is no quoting-injection bug class.
- **Pinned + verified downloads** — every tool download is pinned to a
  version *and* a SHA-256 digest transcribed from the upstream project's
  official checksum asset (`crates/kindboard-core/src/deps/registry.rs`).
  Installers verify
  `SHA256SUMS` and refuse to install on mismatch.
- **Kubeconfig safety** — edits to `~/.kube/config` are atomic, backup the
  previous file, and write with user-private permissions; tests cover the
  symlink-attack case.
- **Secret hygiene** — kubeconfig tokens are scrubbed from error tails, and
  CI scans every commit for leaked secrets (gitleaks).
- **Supply-chain gates in CI** — `cargo audit`, `cargo deny`, dependency
  review, keyless-signed releases, SLSA provenance attestation, and an SPDX
  SBOM per release.
- **Threat model** — see [docs/threat-model.md](docs/threat-model.md).
