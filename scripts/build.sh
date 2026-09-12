#!/usr/bin/env bash
# kindboard release build script.
#
# Usage: ./scripts/build.sh [--all] [--release] [target...]
#
#   --all        build all supported targets (see SUPPORTED_TARGETS below)
#   --release    build release profile (this is the default)
#   target...    one or more rustup target triples; defaults to the host target
#
# Default behaviour: gates + host target release build.
#
# Gates run before any build, in order:
#   1. cargo fmt --all -- --check
#   2. cargo clippy --workspace --all-targets -- -D warnings
#   3. cargo test --workspace
#   4. cargo audit (only if ~/.cargo/bin/cargo-audit exists;
#      network errors are warned about and do not fail the build,
#      reported vulnerabilities DO fail the build)
#
# Cross targets:
#   - missing rustup targets are installed automatically (rustup target add)
#   - linux targets need a cross C toolchain; if the prefixed linker is
#     missing the target is SKIPPED with the package name to install
#     (e.g. aarch64-unknown-linux-gnu needs gcc-aarch64-linux-gnu)
#   - darwin targets cannot be built on Linux; they are SKIPPED with
#     guidance to run this script on macOS
#
# Artifacts land in dist/ (gitignored):
#   dist/<target>/kindboard          raw binary
#   dist/kindboard-<os>-<arch>.tar.gz  tarball containing just the binary
#   dist/SHA256SUMS                  sha256 of every tarball
#
# Exit status: 0 if the host target built successfully (even when other
# targets were skipped or failed), 1 if the host target failed or any gate
# failed.
#
# Assets: icons and logos under assets/ are prepared separately by
# scripts/prepare-assets.sh (network fetch + resize). They are NOT part of
# this build script; run prepare-assets.sh once and commit the results.
#
# This script never modifies the workspace sources or Cargo files; the only
# side effect outside dist/ is `rustup target add` for missing targets.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd)"
DIST_DIR="$ROOT_DIR/dist"
AUDIT_BIN="${CARGO_AUDIT:-$HOME/.cargo/bin/cargo-audit}"

SUPPORTED_TARGETS=(
    x86_64-unknown-linux-gnu
    aarch64-unknown-linux-gnu
    x86_64-apple-darwin
    aarch64-apple-darwin
)

HOST_TARGET="$(rustc -vV | sed -n 's/^host: //p')"
HOST_OS="$(uname -s)"

usage() {
    cat <<'EOF'
Usage: ./scripts/build.sh [--all] [--release] [target...]
  --all        build all supported targets
  --release    build release profile (default)
  target...    one or more rustup target triples; defaults to the host target
Default behaviour: gates + host target release build.
EOF
}

log() { printf 'build.sh: %s\n' "$*"; }

BUILD_ALL=0
TARGETS=()

while (($#)); do
    case "$1" in
        --all) BUILD_ALL=1 ;;
        --release) ;; # release is the default; flag accepted for explicitness
        -h|--help) usage; exit 0 ;;
        --) shift; TARGETS+=("$@"); break ;;
        -*) echo "build.sh: unknown option: $1" >&2; usage >&2; exit 2 ;;
        *) TARGETS+=("$1") ;;
    esac
    shift
done

if (( BUILD_ALL )); then
    TARGETS+=("${SUPPORTED_TARGETS[@]}")
fi
if ((${#TARGETS[@]} == 0)); then
    TARGETS=("$HOST_TARGET")
fi

# de-duplicate, keep order
mapfile -t TARGETS < <(printf '%s\n' "${TARGETS[@]}" | awk '!seen[$0]++')

if ! command -v cargo >/dev/null || ! command -v rustup >/dev/null; then
    echo "build.sh: cargo and rustup are required" >&2
    exit 1
fi

cd "$ROOT_DIR"

# ---------------------------------------------------------------------------
# Gates
# ---------------------------------------------------------------------------

log "== gates: fmt =="
cargo fmt --all -- --check

log "== gates: clippy =="
cargo clippy --workspace --all-targets -- -D warnings

log "== gates: test =="
cargo test --workspace

if [[ -x "$AUDIT_BIN" ]]; then
    log "== gates: cargo audit =="
    AUDIT_LOG="$(mktemp)"
    if "$AUDIT_BIN" audit >"$AUDIT_LOG" 2>&1; then
        log "audit: OK (no vulnerabilities)"
    else
        AUDIT_RC=$?
        if grep -Eqi 'advisory database|network|timed out|couldn.t (download|fetch)|connection' "$AUDIT_LOG"; then
            log "audit: WARNING: could not fetch the advisory database; continuing (no audit result)"
        elif [[ $AUDIT_RC -eq 1 ]]; then
            cat "$AUDIT_LOG" >&2
            rm -f "$AUDIT_LOG"
            log "audit: FAILED: vulnerabilities found"
            exit 1
        else
            cat "$AUDIT_LOG" >&2
            rm -f "$AUDIT_LOG"
            log "audit: FAILED: unexpected error (exit $AUDIT_RC)"
            exit 1
        fi
    fi
    rm -f "$AUDIT_LOG"
else
    log "audit: skipped (cargo-audit not found at $AUDIT_BIN)"
fi

# ---------------------------------------------------------------------------
# Build per target
# ---------------------------------------------------------------------------

mkdir -p "$DIST_DIR"

declare -A RESULTS
HOST_OK=0

for target in "${TARGETS[@]}"; do
    [[ -z "$target" ]] && continue
    case "$target" in
        *-linux-*|*-linux)  os=linux ;;
        *-darwin)           os=darwin ;;
        *-windows-*)        os=windows ;;
        *)                  os=unknown ;;
    esac
    arch="${target%%-*}"

    if [[ "$os" == "unknown" ]]; then
        RESULTS["$target"]="SKIPPED: unrecognised target triple"
        continue
    fi
    if [[ "$os" == "windows" ]]; then
        RESULTS["$target"]="SKIPPED: windows targets are not supported by this script"
        continue
    fi
    if [[ "$os" == "darwin" && "$HOST_OS" != "Darwin" ]]; then
        RESULTS["$target"]="SKIPPED: darwin binaries cannot be built on Linux; run this script on macOS to produce $target"
        continue
    fi

    if [[ "$target" != "$HOST_TARGET" ]]; then
        if ! rustup target list --installed | grep -qxF "$target"; then
            log "installing rustup target: $target"
            rustup target add "$target"
        fi
        if [[ "$os" == "linux" ]]; then
            LINKER="${arch}-linux-gnu-gcc"
            if ! command -v "$LINKER" >/dev/null; then
                if [[ "$arch" == "aarch64" ]]; then
                    RESULTS["$target"]="SKIPPED: cross linker aarch64-linux-gnu-gcc not found; install gcc-aarch64-linux-gnu (dnf install gcc-aarch64-linux-gnu)"
                else
                    RESULTS["$target"]="SKIPPED: cross linker $LINKER not found; install the $arch cross toolchain"
                fi
                continue
            fi
            export "CARGO_TARGET_$(printf '%s' "$target" | tr '[:lower:]-' '[:upper:]_')_LINKER=$LINKER"
        fi
    fi

    log "== build: $target =="
    if cargo build --release --target "$target" --package kindboard-app; then
        mkdir -p "$DIST_DIR/$target"
        cp "target/$target/release/kindboard" "$DIST_DIR/$target/kindboard"
        rm -f "$DIST_DIR/kindboard-${os}-${arch}.tar.gz"
        GZIP=-n tar --owner=0 --group=0 --numeric-owner --mtime='@0' \
            -C "$DIST_DIR/$target" -czf "$DIST_DIR/kindboard-${os}-${arch}.tar.gz" kindboard
        RESULTS["$target"]="OK"
        [[ "$target" == "$HOST_TARGET" ]] && HOST_OK=1
    else
        RESULTS["$target"]="FAILED: cargo build exited non-zero"
    fi
done

# ---------------------------------------------------------------------------
# Checksums + summary
# ---------------------------------------------------------------------------

log "== checksums =="
shopt -s nullglob
TARBALLS=("$DIST_DIR"/kindboard-*.tar.gz)
if ((${#TARBALLS[@]})); then
    (cd "$DIST_DIR" && sha256sum kindboard-*.tar.gz > SHA256SUMS)
    log "wrote $DIST_DIR/SHA256SUMS"
else
    rm -f "$DIST_DIR/SHA256SUMS"
    log "no tarballs produced; SHA256SUMS not written"
fi

log "== summary =="
for target in "${!RESULTS[@]}"; do
    printf 'build.sh: %s: %s\n' "$target" "${RESULTS[$target]}"
done | sort

log "== dist contents =="
(cd "$DIST_DIR" && ls -la)

if (( HOST_OK )); then
    log "host target ($HOST_TARGET) built successfully; exit 0"
    exit 0
fi
log "host target ($HOST_TARGET) did not build; exit 1"
exit 1
