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
#   - darwin targets on Linux are built via osxcross when it is detected
#     (OSXCROSS_DIR below); otherwise they are SKIPPED with guidance, or the
#     build FAILS when KINDBOARD_REQUIRE_DARWIN=1
#   - darwin targets on macOS are built natively (no osxcross needed)
#
# Artifacts land in dist/ (gitignored):
#   dist/<target>/kindboard          raw binary
#   dist/kindboard-<os>-<arch>.tar.gz  tarball containing just the binary
#   dist/SHA256SUMS                  sha256 of every tarball
#
# Exit status: 0 if the host target built successfully, or when the host was
# not requested and every requested target built or was gracefully skipped.
# 1 if the host target was requested and failed, if the host was not requested
# and any requested target failed, if any gate failed, or when
# KINDBOARD_REQUIRE_DARWIN=1 and a requested darwin target is missing or failed.
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

# ---------------------------------------------------------------------------
# Darwin cross-build via osxcross (docs/adrs/ADR-0017.md)
#
# The Apple SDK tarball is a community redistribution (github.com/joseluisq/
# macosx-sdks) of Apple's macOS SDK; it is downloaded here only as a
# build-time convenience for the osxcross toolchain and is never shipped.
#
# Host prerequisites (Fedora, one-time, outside the repo):
#   sudo dnf install clang cmake lld llvm libstdc++-devel libstdc++-static \
#     zlib-devel openssl-devel libxml2-devel
#   rustup target add aarch64-apple-darwin x86_64-apple-darwin
# Toolchain (one-time, outside the repo):
#   git clone https://github.com/tpoechtrager/osxcross ~/.local/src/osxcross
#   curl --proto '=https' -fL "$MACOSX_SDK_URL" -o ~/.local/src/osxcross/tarballs/MacOSX26.1.sdk.tar.xz
#   echo "$MACOSX_SDK_DIGEST  ~/.local/src/osxcross/tarballs/MacOSX26.1.sdk.tar.xz" \
#     | sha256sum -c
#   cd ~/.local/src/osxcross && UNATTENDED=1 \
#     TARGET_DIR="$OSXCROSS_DIR" OSX_VERSION_MIN=10.13 ./build.sh
OSXCROSS_DIR="${OSXCROSS_DIR:-${KINDBOARD_OSXCROSS_DIR:-$HOME/.local/opt/osxcross}}"
MACOSX_SDK_VERSION="26.1"
MACOSX_SDK_NAME="MacOSX${MACOSX_SDK_VERSION}.sdk.tar.xz"
MACOSX_SDK_URL="https://github.com/joseluisq/macosx-sdks/releases/download/${MACOSX_SDK_VERSION}/${MACOSX_SDK_NAME}"
MACOSX_SDK_DIGEST="beee7212d265a6d2867d0236cc069314b38d5fb3486a6515734e76fa210c784c"
# Manual fallback pin pair (MacOSX15.5): swap in by hand if the primary
# SDK/mirror above ever changes. Deliberately NOT an automatic fallback — an
# unverified SDK must never be used silently.
MACOSX_SDK_FALLBACK_VERSION="15.5"
MACOSX_SDK_FALLBACK_NAME="MacOSX${MACOSX_SDK_FALLBACK_VERSION}.sdk.tar.xz"
MACOSX_SDK_FALLBACK_URL="https://github.com/joseluisq/macosx-sdks/releases/download/${MACOSX_SDK_FALLBACK_VERSION}/${MACOSX_SDK_FALLBACK_NAME}"
MACOSX_SDK_FALLBACK_DIGEST="c15cf0f3f17d714d1aa5a642da8e118db53d79429eb015771ba816aa7c6c1cbd"
MACOSX_SDK_CACHE_DIR="${KINDBOARD_MACOSX_SDK_CACHE_DIR:-$HOME/.local/share/kindboard/sdk}"
KINDBOARD_REQUIRE_DARWIN="${KINDBOARD_REQUIRE_DARWIN:-0}"

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

# osxcross_wrapper <arch> <suffix>: print the matching osxcross wrapper path
# (e.g. osxcross_wrapper aarch64 -clang) or nothing. The cmake-clang wrapper is
# ignored — the plain clang wrapper is the cargo linker/CC.
osxcross_wrapper() {
    local arch=$1 suffix=$2 c
    for c in "$OSXCROSS_DIR"/bin/${arch}-apple-darwin*${suffix}; do
        [[ -e "$c" ]] || continue
        [[ "${c##*/}" == *cmake* ]] && continue
        printf '%s\n' "$c"
        return 0
    done
    return 1
}

# osxcross_ready: both darwin arch clang wrappers exist.
osxcross_ready() {
    [[ -n "$(osxcross_wrapper x86_64 -clang)" ]] \
        && [[ -n "$(osxcross_wrapper aarch64 -clang)" ]]
}

# ensure_macosx_sdk: cache the digest-verified SDK tarball next to the
# toolchain (build-time convenience only; the built toolchain already embeds
# the SDK). Fail hard on digest mismatch and on download failure under
# KINDBOARD_REQUIRE_DARWIN=1; warn and continue on download failure otherwise
# (the installed toolchain is self-contained).
ensure_macosx_sdk() {
    local sdk_path="$MACOSX_SDK_CACHE_DIR/$MACOSX_SDK_NAME"
    if [[ -f "$sdk_path" ]] \
        && [[ "$(sha256sum "$sdk_path")" == "$MACOSX_SDK_DIGEST  "* ]]; then
        log "osxcross: cached SDK verified: $sdk_path"
        return 0
    fi
    mkdir -p "$MACOSX_SDK_CACHE_DIR"
    log "osxcross: downloading macOS SDK $MACOSX_SDK_VERSION from $MACOSX_SDK_URL"
    if ! curl --proto '=https' -fL --retry 3 -o "$sdk_path" "$MACOSX_SDK_URL"; then
        if [[ "$KINDBOARD_REQUIRE_DARWIN" == "1" ]]; then
            echo "build.sh: ERROR: SDK download failed ($MACOSX_SDK_URL) and KINDBOARD_REQUIRE_DARWIN=1" >&2
            return 1
        fi
        log "osxcross: WARNING: SDK download failed; the installed toolchain already embeds the SDK, continuing"
        return 0
    fi
    if [[ "$(sha256sum "$sdk_path")" == "$MACOSX_SDK_DIGEST  "* ]]; then
        log "osxcross: SDK digest verified"
    else
        echo "build.sh: ERROR: macOS SDK digest mismatch: $sdk_path" >&2
        echo "build.sh: expected $MACOSX_SDK_DIGEST (macOS $MACOSX_SDK_VERSION); re-download from $MACOSX_SDK_URL" >&2
        return 1
    fi
}

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
    if [[ "$KINDBOARD_REQUIRE_DARWIN" == "1" ]]; then
        TARGETS+=(x86_64-apple-darwin aarch64-apple-darwin)
    fi
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

DARWIN_REQUESTED_COUNT=0
HOST_REQUESTED=0
for t in "${TARGETS[@]}"; do
    [[ "$t" == *-apple-darwin ]] && DARWIN_REQUESTED_COUNT=$((DARWIN_REQUESTED_COUNT+1))
    [[ "$t" == "$HOST_TARGET" ]] && HOST_REQUESTED=1
done

if [[ "$HOST_OS" != "Darwin" ]] && (( DARWIN_REQUESTED_COUNT > 0 )); then
    if osxcross_ready; then
        ensure_macosx_sdk
    elif [[ "$KINDBOARD_REQUIRE_DARWIN" == "1" ]]; then
        echo "build.sh: ERROR: KINDBOARD_REQUIRE_DARWIN=1 but osxcross was not found at $OSXCROSS_DIR" >&2
        echo "build.sh: install it per docs/adrs/ADR-0017.md (dnf deps + osxcross build.sh)" >&2
        echo "build.sh: or point KINDBOARD_OSXCROSS_DIR at an existing osxcross install" >&2
        exit 1
    fi
fi

mkdir -p "$DIST_DIR"

declare -A RESULTS
HOST_OK=0
DARWIN_OK_COUNT=0
BUILD_FAILURES=0

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
    DARWIN_CROSS=0
    if [[ "$os" == "darwin" && "$HOST_OS" != "Darwin" ]]; then
        DARWIN_CC="$(osxcross_wrapper "$arch" -clang || true)"
        DARWIN_AR="$(osxcross_wrapper "$arch" -ar || true)"
        if [[ -z "$DARWIN_CC" || -z "$DARWIN_AR" ]]; then
            if [[ "$KINDBOARD_REQUIRE_DARWIN" == "1" ]]; then
                echo "build.sh: ERROR: darwin target $target is REQUIRED (KINDBOARD_REQUIRE_DARWIN=1)" >&2
                echo "build.sh: but osxcross wrappers for $arch are missing from $OSXCROSS_DIR/bin" >&2
                echo "build.sh: install osxcross per docs/adrs/ADR-0017.md or set KINDBOARD_OSXCROSS_DIR" >&2
                exit 1
            fi
            RESULTS["$target"]="SKIPPED: osxcross not found at $OSXCROSS_DIR (darwin binaries on Linux need it) — see docs/adrs/ADR-0017.md"
            continue
        fi
        # cc-crate convention: aarch64-apple-darwin -> CC_aarch64_apple_darwin
        DARWIN_CC_ENV="CC_${arch}_apple_darwin"
        DARWIN_AR_ENV="AR_${arch}_apple_darwin"
        DARWIN_CFLAGS_ENV="CFLAGS_${arch}_apple_darwin"
        # cargo convention: triple upper-cased with '-' -> '_'
        DARWIN_LINKER_ENV="CARGO_TARGET_$(printf '%s' "$target" | tr '[:lower:]-' '[:upper:]_')_LINKER"
        if [[ "$arch" == "aarch64" ]]; then
            DARWIN_MIN="11.0"
        else
            DARWIN_MIN="10.13"
        fi
        DARWIN_CROSS=1
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

    arch_display="$arch"
    if [[ "$os" == "darwin" && "$arch" == "aarch64" ]]; then
        arch_display="arm64" # tarball name uses macOS's arm64 convention
    fi

    log "== build: $target =="
    if [[ "$DARWIN_CROSS" == "1" ]]; then
        # env-scoped to this single cargo invocation: darwin toolchain vars
        # never leak into other targets' builds
        if env \
            PATH="$OSXCROSS_DIR/bin:$PATH" \
            "$DARWIN_LINKER_ENV=$DARWIN_CC" \
            "$DARWIN_CC_ENV=$DARWIN_CC" \
            "$DARWIN_AR_ENV=$DARWIN_AR" \
            "$DARWIN_CFLAGS_ENV=-O2" \
            MACOSX_DEPLOYMENT_TARGET="$DARWIN_MIN" \
            cargo build --release --target "$target" --package kindboard-app; then
            BUILD_RC=0
        else
            BUILD_RC=1
        fi
    elif cargo build --release --target "$target" --package kindboard-app; then
        BUILD_RC=0
    else
        BUILD_RC=1
    fi
    if (( BUILD_RC == 0 )); then
        mkdir -p "$DIST_DIR/$target"
        cp "target/$target/release/kindboard" "$DIST_DIR/$target/kindboard"
        rm -f "$DIST_DIR/kindboard-${os}-${arch_display}.tar.gz"
        GZIP=-n tar --owner=0 --group=0 --numeric-owner --mtime='@0' \
            -C "$DIST_DIR/$target" -czf "$DIST_DIR/kindboard-${os}-${arch_display}.tar.gz" kindboard
        RESULTS["$target"]="OK"
        [[ "$target" == "$HOST_TARGET" ]] && HOST_OK=1
        [[ "$os" == "darwin" ]] && DARWIN_OK_COUNT=$((DARWIN_OK_COUNT+1))
    else
        RESULTS["$target"]="FAILED: cargo build exited non-zero"
        BUILD_FAILURES=$((BUILD_FAILURES+1))
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

if [[ "$KINDBOARD_REQUIRE_DARWIN" == "1" ]] \
    && (( DARWIN_REQUESTED_COUNT > 0 )) \
    && (( DARWIN_OK_COUNT != DARWIN_REQUESTED_COUNT )); then
    log "KINDBOARD_REQUIRE_DARWIN=1 and not all requested darwin targets built ($DARWIN_OK_COUNT/$DARWIN_REQUESTED_COUNT); exit 1"
    exit 1
fi

if (( HOST_OK )); then
    log "host target ($HOST_TARGET) built successfully; exit 0"
    exit 0
fi

if (( HOST_REQUESTED )); then
    log "host target ($HOST_TARGET) did not build; exit 1"
    exit 1
fi

if (( BUILD_FAILURES )); then
    log "$BUILD_FAILURES requested target(s) failed to build; exit 1"
    exit 1
fi

log "no host target requested and all requested targets built or skipped; exit 0"
exit 0
