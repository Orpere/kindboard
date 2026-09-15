---
name: Bug report
about: Something is broken — help us reproduce it
title: "[bug] "
labels: bug
assignees: Orpere
---

**Describe the bug**
A clear description of what happened.

**To reproduce**
Steps, e.g.:
1. Open kindboard, create cluster `X` with CNI `cilium`
2. Enable Gateway API
3. See the step fail with `...`

**Expected behavior**
What should have happened.

**Environment**
- OS + version (e.g. `macOS 15 arm64`, `Ubuntu 24.04`):
- kindboard version (`kindboard --version`):
- kind version (`kind version`):
- Docker version (`docker version --format '{{.Server.Version}}'`):

**Logs / screenshots**
kindboard logs live in the Troubleshooting panel; paste the failing step's
output here (scrub any tokens — kubeconfig output is already scrubbed by the
app, but double-check anything you copy by hand).

**Security note**
If this bug is a security vulnerability, do NOT open it here — follow the
private disclosure process in [SECURITY.md](../SECURITY.md) instead.
