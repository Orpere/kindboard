## Summary

<!-- One line: what and why. Link the issue if there is one (fixes #NNN). -->

## Checklist (all required before review)

- [ ] `cargo fmt --all --check` clean
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo test --workspace` green
- [ ] `cargo audit` clean; `cargo deny check` clean (new deps only)
- [ ] New behavior has tests (happy path + failure paths)
- [ ] No panics on external input in `kindboard-core`; typed errors used
- [ ] New downloads pinned with a SHA-256 digest from the official source
- [ ] Docs/ADRs updated when architecture or release behavior changes

## Test plan

<!-- How did you verify beyond the unit tests? E.g. live kind cluster run,
     manual UI pass on which OS, etc. Or "CI matrix covers it". -->
