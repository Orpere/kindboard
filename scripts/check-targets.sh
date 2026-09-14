#!/usr/bin/env bash
# kindboard cross-target compile gate — host tests + every available cross check.
#
# Usage: ./scripts/check-targets.sh
#
# Runs, in order:
#   1. host:    cargo test --workspace (same as `make test`)
#   2. windows: when the rustup target x86_64-pc-windows-gnu AND the mingw64
#      linker (x86_64-w64-mingw32-gcc) are present —
#        cargo check --workspace --all-targets --target x86_64-pc-windows-gnu
#        cargo clippy --workspace --all-targets --target x86_64-pc-windows-gnu -- -D warnings
#      otherwise SKIPPED with guidance (rustup target add x86_64-pc-windows-gnu;
#      dnf install mingw64-gcc). The mingw64 linker is env-scoped to each cargo
#      invocation (CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER).
#   3. darwin:  when osxcross is detected (darwin-env.sh, the same detection
#      build.sh uses) —
#        cargo check --workspace --all-targets --target x86_64-apple-darwin
#        cargo check --workspace --all-targets --target aarch64-apple-darwin
#      with the same env-scoped CC/AR/CFLAGS/MACOSX_DEPLOYMENT_TARGET setup as
#      scripts/build.sh; otherwise SKIPPED with guidance (make darwin-bootstrap,
#      see docs/adrs/ADR-0017.md).
#
# Compile-only gate: no release artifacts are produced; target/ is the only
# side effect. Windows runtime behavior is NOT tested here — no Windows runner
# exists (ADR-0019); tests run on the host only.
#
# Exit status: 0 when every check that ran passed (or was skipped with a
# logged reason); 1 when any check that ran failed.
#
# Log prefix: `check-targets:`.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT_DIR"

log() { printf 'check-targets: %s\n' "$*"; }

source "$SCRIPT_DIR/darwin-env.sh"

command -v cargo >/dev/null || { log "ERROR: cargo is required"; exit 1; }

FAILURES=0

# ---------------------------------------------------------------------------
# 1. Host: tests (the only tests that can run — no Windows/macOS runners here)
# ---------------------------------------------------------------------------

log "== host: cargo test --workspace =="
cargo test --workspace || FAILURES=1

# ---------------------------------------------------------------------------
# 2. Windows: x86_64-pc-windows-gnu compile gate (ADR-0019)
# ---------------------------------------------------------------------------

log "== windows: x86_64-pc-windows-gnu =="
WIN_READY=0
if ! command -v rustup >/dev/null; then
    log "SKIPPED: rustup not found — needed to verify the windows rustup target"
elif ! rustup target list --installed 2>/dev/null | grep -qxF x86_64-pc-windows-gnu; then
    log "SKIPPED: rustup target x86_64-pc-windows-gnu missing — install it (rustup target add x86_64-pc-windows-gnu) plus the mingw64 linker (dnf install mingw64-gcc)"
elif ! command -v x86_64-w64-mingw32-gcc >/dev/null; then
    log "SKIPPED: mingw64 linker x86_64-w64-mingw32-gcc missing — install it (dnf install mingw64-gcc)"
else
    WIN_READY=1
fi
if [[ "$WIN_READY" == "1" ]]; then
    if CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
        cargo check --workspace --all-targets --target x86_64-pc-windows-gnu; then
        log "windows cargo check: OK"
    else
        FAILURES=1
    fi
    if CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
        cargo clippy --workspace --all-targets --target x86_64-pc-windows-gnu -- -D warnings; then
        log "windows clippy: OK"
    else
        FAILURES=1
    fi
fi

# ---------------------------------------------------------------------------
# 3. Darwin: x86_64 + aarch64 apple-darwin compile gate (ADR-0017)
# ---------------------------------------------------------------------------

log "== darwin: x86_64-apple-darwin + aarch64-apple-darwin =="
if osxcross_ready; then
    for pair in "x86_64 10.13" "aarch64 11.0"; do
        read -r arch min <<<"$pair"
        target="${arch}-apple-darwin"
        cc_wrap="$(osxcross_wrapper "$arch" -clang)" || { log "SKIPPED: no osxcross clang wrapper for $arch"; continue; }
        ar_wrap="$(osxcross_wrapper "$arch" -ar)" || { log "SKIPPED: no osxcross ar wrapper for $arch"; continue; }
        # same env conventions as scripts/build.sh: cc-crate triplet vars +
        # cargo linker var + deployment target; env-scoped per invocation
        linker_env="CARGO_TARGET_$(printf '%s' "$target" | tr '[:lower:]-' '[:upper:]_')_LINKER"
        cc_env="CC_${arch}_apple_darwin"
        ar_env="AR_${arch}_apple_darwin"
        cflags_env="CFLAGS_${arch}_apple_darwin"
        if env \
            PATH="$OSXCROSS_DIR/bin:$PATH" \
            "$linker_env=$cc_wrap" \
            "$cc_env=$cc_wrap" \
            "$ar_env=$ar_wrap" \
            "$cflags_env=-O2" \
            MACOSX_DEPLOYMENT_TARGET="$min" \
            cargo check --workspace --all-targets --target "$target"; then
            log "darwin cargo check ($target): OK"
        else
            FAILURES=1
        fi
    done
else
    log "SKIPPED: osxcross not found at $OSXCROSS_DIR — resolve it with make darwin-bootstrap (see docs/adrs/ADR-0017.md)"
fi

# ---------------------------------------------------------------------------

if (( FAILURES )); then
    log "one or more checks FAILED; exit 1"
    exit 1
fi
log "all checks passed or skipped; exit 0"
exit 0
