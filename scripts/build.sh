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
#   - darwin targets require rcodesign (cargo install apple-codesign --locked)
#     on the build host — missing rcodesign FAILS any darwin target that is
#     built, it is not skippable (targets skipped for lack of osxcross never
#     reach the signing step)
#   - darwin targets on macOS are built natively (no osxcross needed); a
#     native-cc pre-flight fails actionably before the build when Xcode
#     Command Line Tools are missing
#   - `make darwin-bootstrap` (scripts/bootstrap-darwin.sh) resolves ALL darwin
#     build dependencies (host packages, rustup targets, rcodesign, osxcross +
#     digest-pinned SDK) idempotently, and runs automatically before
#     dist-macos / build-all / release; this script remains a pure builder
#
# Artifacts land in dist/ (gitignored):
#   dist/<target>/kindboard          raw binary
#   dist/kindboard-<os>-<arch>.tar.gz  tarball containing just the binary
#   dist/kindboard-<os>-<arch>.zip   deterministic ZIP of the signed binary
#                                    (notarized release mode only, see below)
#   dist/SHA256SUMS                  sha256 of every tarball
# darwin binaries are ad-hoc signed with rcodesign (pure Rust) before
# tarballing; the signature is then verified (codesign --verify on macOS,
# structural LC_CODE_SIGNATURE + CSMAGIC check on Linux) — see ADR-0018.
# When the notarization env vars below are set, notarized (Developer ID)
# release mode SUPERSEDES the ad-hoc default of ADR-0018 for that build
# (ADR-0018 itself is unchanged: it remains the documented default for
# env-free builds).
#
# Developer ID + notarization (optional; darwin targets only):
#   KINDBOARD_APPLE_CERT_PEM           path to a PEM file containing the
#                                      Developer ID Application certificate
#                                      AND its private key
#        — or (alternative) —
#   KINDBOARD_APPLE_CERT_P12           path to a P12/PFX file containing the
#                                      same certificate + key
#   KINDBOARD_APPLE_CERT_P12_PASSWORD  password for that P12
#   KINDBOARD_APPLE_API_KEY_PATH       path to the JSON produced by
#                                      `rcodesign encode-app-store-connect-api-key`
#   With BOTH a certificate input and the API key path set, darwin targets
#   enter notarized release mode: (1) sign the binary with the Developer ID
#   (`rcodesign sign --pem-source <pem>` or `--p12-file`/`--p12-password-file`),
#   (2) verify structurally exactly as in ad-hoc mode, (3) after the tarball
#   is created, also produce a deterministic ZIP of the binary (Apple
#   notarizes ARCHIVES, not raw binaries; python3 zipfile with fixed
#   date_time=(1980,1,1,0,0,0); python3 is required and checked early),
#   (4) notarize + staple: `rcodesign notary-submit --staple --wait
#   --api-key-path <zip>` (can take minutes — it is logged). A notarization
#   failure FAILS the target and ships neither tarball nor zip for it.
#   ONE input without the other FAILS the darwin target (both required
#   together). NEITHER set -> the current ad-hoc behavior, unchanged.
#   Credentials are never committed: export them in the shell, keep the
#   files outside the repo. One-time Apple setup: docs/macos-distribution.md.
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
RCODESIGN_BIN="${KINDBOARD_RCODESIGN:-$(command -v rcodesign || true)}"

# ---------------------------------------------------------------------------
# Darwin cross-build via osxcross (docs/adrs/ADR-0017.md)
#
# All darwin toolchain pins (osxcross dir, SDK version/name/URL/digest, the
# manual fallback pin pair, SDK cache dir, osxcross source dir) live in
# scripts/darwin-env.sh — the single source of truth shared with
# scripts/bootstrap-darwin.sh. `make darwin-bootstrap`
# (scripts/bootstrap-darwin.sh) resolves all darwin deps (host packages,
# rustup targets, rcodesign, osxcross + digest-pinned SDK); this script
# remains a pure builder and only detects + builds (ensure_macosx_sdk below
# caches the SDK tarball when osxcross is detected).
#
# The Apple SDK tarball is a community redistribution (github.com/joseluisq/
# macosx-sdks) of Apple's macOS SDK; it is downloaded here only as a
# build-time convenience for the osxcross toolchain and is never shipped.
source "$SCRIPT_DIR/darwin-env.sh"
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

# verify_darwin_signature <binary>: confirm the Mach-O carries an embedded
# code signature. On macOS uses the canonical codesign verifier. On Linux
# performs a structural check (rcodesign's own `verify` is unreliable for
# ad-hoc signatures — it requires CMS data ad-hoc signatures don't have):
#   LC_CODE_SIGNATURE load command present + CSMAGIC_EMBEDDED magic at dataoff.
verify_darwin_signature() {
    local bin=$1 stanza off magic
    if [[ "$HOST_OS" == "Darwin" ]]; then
        codesign --verify --strict "$bin"
        return $?
    fi
    if ! command -v llvm-objdump >/dev/null; then
        echo "llvm-objdump required to verify darwin signatures (dnf install llvm)" >&2
        return 1
    fi
    stanza="$(llvm-objdump --macho --private-headers "$bin" 2>/dev/null \
        | grep -A3 'LC_CODE_SIGNATURE')" || {
        echo "no LC_CODE_SIGNATURE load command in $bin" >&2
        return 1
    }
    off="$(awk '/dataoff/{print $2}' <<<"$stanza")"
    if [[ ! "$off" =~ ^[0-9]+$ ]]; then
        echo "could not parse LC_CODE_SIGNATURE dataoff in $bin" >&2
        return 1
    fi
    magic="$(od -A n -t x1 -j "$off" -N 4 "$bin" | tr -d ' \n')"
    if [[ "$magic" != "fade0cc0" ]]; then
        echo "signature blob magic mismatch at dataoff $off: $magic (expected fade0cc0)" >&2
        return 1
    fi
    return 0
}

# native_cc_probe: on macOS hosts confirm a working C compiler (Xcode
# Command Line Tools) before building, so failures are actionable instead of
# opaque cc/libc crate errors.
native_cc_probe() {
    local tmp
    tmp="$(mktemp -d)" || return 1
    printf 'int main(void){return 0;}\n' > "$tmp/probe.c"
    if ! (cd "$tmp" && "${CC:-cc}" probe.c -o probe) >/dev/null 2>&1; then
        rm -rf "$tmp"
        return 1
    fi
    rm -rf "$tmp"
    return 0
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
NOTARIZE=0

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
    if [[ "$os" == "darwin" && "$HOST_OS" == "Darwin" ]] && ! native_cc_probe; then
        RESULTS["$target"]="FAILED: no working C compiler — install Xcode Command Line Tools (xcode-select --install) or run: make darwin-bootstrap"
        BUILD_FAILURES=$((BUILD_FAILURES+1))
        rm -f "$DIST_DIR/kindboard-${os}-${arch_display}.tar.gz"
        continue
    fi
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
        if [[ "$os" == "darwin" ]]; then
            # Never ship unsigned darwin assets — explicit failure over a
            # silently broken artifact. Unsigned + quarantine makes Gatekeeper
            # show "damaged" (hard error); an ad-hoc signature downgrades it
            # to "unidentified developer" (bypassable via right-click Open).
            # Sign the dist/ copy, not target/, to keep cargo output pristine.
            # Applies to both osxcross cross builds and native macOS builds
            # (same loop path).
            #
            # Notarization mode (Developer ID + notary) is env-gated and
            # SUPERSEDES the ADR-0018 ad-hoc default when enabled; with no
            # env vars set the ad-hoc path below is byte-for-byte unchanged.
            NOTARIZE=0
            HAVE_CERT=0
            HAVE_APIKEY=0
            PEM_SET=0
            P12_SET=0
            P12_PW_SET=0
            [[ -n "${KINDBOARD_APPLE_CERT_PEM:-}" ]] && PEM_SET=1
            [[ -n "${KINDBOARD_APPLE_CERT_P12:-}" ]] && P12_SET=1
            [[ -n "${KINDBOARD_APPLE_CERT_P12_PASSWORD:-}" ]] && P12_PW_SET=1
            [[ -n "${KINDBOARD_APPLE_API_KEY_PATH:-}" ]] && HAVE_APIKEY=1
            if (( PEM_SET )) || (( P12_SET && P12_PW_SET )); then
                HAVE_CERT=1
            fi
            if (( HAVE_CERT && HAVE_APIKEY )); then
                NOTARIZE=1
            elif (( HAVE_CERT || HAVE_APIKEY || P12_SET || P12_PW_SET )); then
                RESULTS["$target"]="FAILED: partial notarization config — KINDBOARD_APPLE_CERT_PEM (or KINDBOARD_APPLE_CERT_P12 + KINDBOARD_APPLE_CERT_P12_PASSWORD together) and KINDBOARD_APPLE_API_KEY_PATH must all be provided to enter notarized (Developer ID) release mode — see docs/macos-distribution.md"
                BUILD_FAILURES=$((BUILD_FAILURES+1))
                rm -f "$DIST_DIR/kindboard-${os}-${arch_display}.tar.gz" "$DIST_DIR/kindboard-${os}-${arch_display}.zip"
                continue
            fi
            if (( NOTARIZE )) && ! command -v python3 >/dev/null 2>&1; then
                RESULTS["$target"]="FAILED: notarized (Developer ID) release mode requires python3 to create the notarization ZIP — install python3"
                BUILD_FAILURES=$((BUILD_FAILURES+1))
                rm -f "$DIST_DIR/kindboard-${os}-${arch_display}.tar.gz" "$DIST_DIR/kindboard-${os}-${arch_display}.zip"
                continue
            fi
            if [[ -z "$RCODESIGN_BIN" ]]; then
                RESULTS["$target"]="FAILED: rcodesign not found on PATH — darwin binaries must be ad-hoc signed (cargo install apple-codesign --locked)"
                BUILD_FAILURES=$((BUILD_FAILURES+1))
                rm -f "$DIST_DIR/kindboard-${os}-${arch_display}.tar.gz" "$DIST_DIR/kindboard-${os}-${arch_display}.zip"
                continue
            fi
            if [[ ! -x "$RCODESIGN_BIN" ]]; then
                RESULTS["$target"]="FAILED: rcodesign not executable: $RCODESIGN_BIN — darwin binaries must be ad-hoc signed (cargo install apple-codesign --locked)"
                BUILD_FAILURES=$((BUILD_FAILURES+1))
                rm -f "$DIST_DIR/kindboard-${os}-${arch_display}.tar.gz" "$DIST_DIR/kindboard-${os}-${arch_display}.zip"
                continue
            fi
            RCODESIGN_ERR="$(mktemp)"
            P12_PW_FILE=""
            if (( NOTARIZE )); then
                log "notarized (Developer ID) release mode: signing $DIST_DIR/$target/kindboard"
                if [[ -n "${KINDBOARD_APPLE_CERT_PEM:-}" ]]; then
                    # --pem-source is the PEM cert+key source (listed among the
                    # global signing settings of `rcodesign help sign`; the
                    # options list spells it --pem-file — both are accepted)
                    if "$RCODESIGN_BIN" sign --pem-source "$KINDBOARD_APPLE_CERT_PEM" \
                        "$DIST_DIR/$target/kindboard" 2>"$RCODESIGN_ERR"; then
                        SIGN_RC=0
                    else
                        SIGN_RC=$?
                    fi
                else
                    # --p12-file + --p12-password-file: the password is passed
                    # via a 0600 temp file so it never lands in the process list
                    P12_PW_FILE="$(mktemp)"
                    printf '%s' "$KINDBOARD_APPLE_CERT_P12_PASSWORD" > "$P12_PW_FILE"
                    chmod 600 "$P12_PW_FILE"
                    if "$RCODESIGN_BIN" sign --p12-file "$KINDBOARD_APPLE_CERT_P12" \
                        --p12-password-file "$P12_PW_FILE" \
                        "$DIST_DIR/$target/kindboard" 2>"$RCODESIGN_ERR"; then
                        SIGN_RC=0
                    else
                        SIGN_RC=$?
                    fi
                fi
            else
                if "$RCODESIGN_BIN" sign "$DIST_DIR/$target/kindboard" 2>"$RCODESIGN_ERR"; then
                    SIGN_RC=0
                else
                    SIGN_RC=$?
                fi
            fi
            if (( SIGN_RC == 0 )) \
                && verify_darwin_signature "$DIST_DIR/$target/kindboard" 2>>"$RCODESIGN_ERR"; then
                rm -f "$RCODESIGN_ERR" "${P12_PW_FILE:-}"
                if (( NOTARIZE )); then
                    log "Developer ID signed + verified: $DIST_DIR/$target/kindboard"
                else
                    log "ad-hoc signed + verified: $DIST_DIR/$target/kindboard"
                fi
            else
                FAIL_MSG="rcodesign sign/verify failed: $(tr '\012' ' ' < "$RCODESIGN_ERR")"
                (( NOTARIZE )) && FAIL_MSG="notarized (Developer ID) release mode: $FAIL_MSG"
                RESULTS["$target"]="FAILED: $FAIL_MSG"
                rm -f "$RCODESIGN_ERR" "${P12_PW_FILE:-}"
                rm -f "$DIST_DIR/kindboard-${os}-${arch_display}.tar.gz" "$DIST_DIR/kindboard-${os}-${arch_display}.zip"
                BUILD_FAILURES=$((BUILD_FAILURES+1))
                continue
            fi
        fi
        rm -f "$DIST_DIR/kindboard-${os}-${arch_display}.tar.gz" "$DIST_DIR/kindboard-${os}-${arch_display}.zip"
        GZIP=-n tar --owner=0 --group=0 --numeric-owner --mtime='@0' \
            -C "$DIST_DIR/$target" -czf "$DIST_DIR/kindboard-${os}-${arch_display}.tar.gz" kindboard
        if [[ "$os" == "darwin" ]] && (( NOTARIZE )); then
            # Notarization mode: Apple notarizes ARCHIVES, not raw binaries,
            # so after the tarball we also produce a deterministic ZIP of the
            # signed binary (python3 zipfile, fixed timestamp 1980-01-01,
            # deflate, unix attrs) and notarize + staple it.
            # --staple implies --wait (rcodesign help notary-submit); the
            # round-trip to Apple's Notary API can take minutes.
            ZIP_PATH="$DIST_DIR/kindboard-${os}-${arch_display}.zip"
            rm -f "$ZIP_PATH"
            python3 - "$DIST_DIR/$target/kindboard" "$ZIP_PATH" <<'PYEOF'
import sys, zipfile
src, dst = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(dst, "w", zipfile.ZIP_DEFLATED) as zf:
    zi = zipfile.ZipInfo("kindboard", (1980, 1, 1, 0, 0, 0))
    zi.compress_type = zipfile.ZIP_DEFLATED
    zi.create_system = 3
    zi.external_attr = 0o755 << 16
    with open(src, "rb") as f:
        zf.writestr(zi, f.read())
PYEOF
            log "notarizing + stapling $ZIP_PATH (Developer ID; can take minutes)"
            NOTARY_ERR="$(mktemp)"
            # --api-key-path is the flag recommended by `rcodesign help
            # notary-submit` (the options list spells it --api-key-file;
            # both are accepted)
            if "$RCODESIGN_BIN" notary-submit --staple --wait \
                --api-key-path "$KINDBOARD_APPLE_API_KEY_PATH" \
                "$ZIP_PATH" 2>"$NOTARY_ERR"; then
                rm -f "$NOTARY_ERR"
                log "notarized + stapled: $ZIP_PATH"
            else
                RESULTS["$target"]="FAILED: notarized (Developer ID) release mode: notary-submit failed: $(tr '\012' ' ' < "$NOTARY_ERR")"
                rm -f "$NOTARY_ERR" "$ZIP_PATH" "$DIST_DIR/kindboard-${os}-${arch_display}.tar.gz"
                BUILD_FAILURES=$((BUILD_FAILURES+1))
                continue
            fi
        fi
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
    # notarized release mode adds kindboard-*.zip artifacts; include them in
    # SHA256SUMS when present (nullglob keeps the pattern harmless otherwise)
    (cd "$DIST_DIR" && sha256sum kindboard-*.tar.gz kindboard-*.zip > SHA256SUMS)
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
