#!/usr/bin/env bash
# kindboard darwin cross-build dependency resolver.
#
# Usage: ./scripts/bootstrap-darwin.sh [--check]
#
#   --check   verify only: report what is missing, install nothing, exit 1
#             if anything is missing (exit 0 when everything is present)
#
# Idempotent resolver of every dependency the darwin (macOS) cross-build
# needs. Each step VERIFIES first and installs only what is missing, so a
# complete machine is a fast no-op. Called by `make darwin-bootstrap` and
# automatically by `make dist-macos` / `make build-all` / `make release`.
#
# Steps, in order:
#   1. platform detection: Linux (dnf or apt) or native macOS
#   2. Xcode Command Line Tools (macOS only) — `xcode-select -p` + a real C
#      compile probe; missing CLT is resolved with `xcode-select --install`
#      under the same install policy as host packages (below; never hangs
#      automation, exit 1 until the GUI installer dialog is completed)
#   3. rust toolchain version (macOS only) — the macOS 26 SDK needs a recent
#      rustc (>= 1.98, RUST_MIN_VERSION below); full mode runs
#      `rustup update stable` when the installed rustc is too old
#   4. host packages (Linux only; Fedora/apt lists below) — verified with
#      `rpm -q --whatprovides` / `dpkg -s`, installed with sudo only when
#      something is missing. Install policy: KINDBOARD_BOOTSTRAP_YES=1 or
#      passwordless `sudo -n true` -> install silently; interactive tty ->
#      print the exact command + y/N prompt; otherwise print the exact
#      command and exit 1 (never hangs automation).
#   5. rustup targets — Linux: aarch64-apple-darwin + x86_64-apple-darwin;
#      macOS: the other darwin arch (the native rustc host is already
#      usable), so a Mac can cross-build both darwin arches
#   6. rcodesign (cargo install apple-codesign --locked; honours
#      KINDBOARD_RCODESIGN when set and executable)
#   7. osxcross toolchain (Linux only): clone ~/.local/src/osxcross at the
#      pinned commit OSXCROSS_PIN (darwin-env.sh), fetch the digest-pinned SDK
#      into the cache dir, and UNATTENDED=1 ./build.sh into OSXCROSS_DIR. A
#      digest mismatch is a hard error — the fallback SDK pin is never used
#      automatically (see docs/adrs/ADR-0017.md).
#   8. status table, one line per dependency; exit 0 only when all resolved
#
# All pins come from ../scripts/darwin-env.sh (single source of truth, shared
# with build.sh). Everything installs into $HOME except host packages and the
# Xcode CLT installer (system-level; the only steps that may need credentials).
# Log prefix: `bootstrap-darwin:`.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=darwin-env.sh
source "$SCRIPT_DIR/darwin-env.sh"

log() { printf 'bootstrap-darwin: %s\n' "$*"; }

CHECK_ONLY=0
while (($#)); do
    case "$1" in
        --check) CHECK_ONLY=1 ;;
        -h|--help)
            sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "bootstrap-darwin: unknown option: $1 (use --check or nothing)" >&2; exit 2 ;;
    esac
    shift
done

MISSING=0
ROWS=()

report() { # report <label> <status> <detail>
    ROWS+=("$1|$2|$3")
    if [[ "$2" == "MISSING" ]]; then
        MISSING=$((MISSING + 1))
    fi
}

# ---------------------------------------------------------------------------
# 1. Platform detection
# ---------------------------------------------------------------------------

HOST_OS="$(uname -s)"
case "$HOST_OS" in
    Darwin)
        PLATFORM=darwin
        ;;
    Linux)
        if command -v dnf >/dev/null 2>&1; then
            PLATFORM=dnf
        elif command -v apt-get >/dev/null 2>&1; then
            PLATFORM=apt
        else
            echo "bootstrap-darwin: ERROR: unsupported Linux host — neither dnf nor apt-get found" >&2
            echo "bootstrap-darwin: install the osxcross host packages manually (see docs/adrs/ADR-0017.md), then re-run" >&2
            exit 1
        fi
        ;;
    *)
        echo "bootstrap-darwin: ERROR: unsupported platform '$HOST_OS' — darwin bootstrap needs macOS or a Linux host with dnf/apt" >&2
        exit 1
        ;;
esac

if [[ "$PLATFORM" == "darwin" ]]; then
    log "native macOS host: verifying Xcode CLT + rustup targets; skipping host-package and osxcross steps (darwin builds natively)"
else
    log "platform: Linux ($PLATFORM)"
    [[ "$CHECK_ONLY" == "1" ]] && log "check mode: verifying only, nothing will be installed"
fi

# ---------------------------------------------------------------------------
# 2. Xcode Command Line Tools (native macOS only)
# ---------------------------------------------------------------------------

if [[ "$PLATFORM" == "darwin" ]]; then
    clt_ok() {
        # working CLT = valid developer dir + a working C compiler
        local dev tmp probe_rc=0
        dev="$(xcode-select -p 2>/dev/null || true)"
        [[ -n "$dev" && -d "$dev" ]] || return 1
        tmp="$(mktemp -d)" || return 1
        printf 'int main(void){return 0;}\n' > "$tmp/probe.c"
        (cd "$tmp" && "${CC:-cc}" probe.c -o probe) >/dev/null 2>&1 || probe_rc=1
        rm -rf "$tmp"
        return "$probe_rc"
    }

    if clt_ok; then
        report "Xcode CLT" "OK" "$("${CC:-cc}" --version 2>/dev/null | head -n1 || true)"
    elif [[ "$CHECK_ONLY" == "1" ]]; then
        report "Xcode CLT" "MISSING" "run: xcode-select --install"
    else
        log "Xcode Command Line Tools required (opens the GUI installer dialog)"
        log 'no Xcode? skip building entirely: `make mac-run` fetches the prebuilt signed binary (no CLT needed)'
        if [[ "${KINDBOARD_BOOTSTRAP_YES:-0}" == "1" ]] \
            || { command -v sudo >/dev/null 2>&1 && sudo -n true >/dev/null 2>&1; }; then
            xcode-select --install
            echo "bootstrap-darwin: complete the Command Line Tools installation dialog, then re-run ./scripts/bootstrap-darwin.sh" >&2
            exit 1
        elif [[ -t 0 && -t 1 ]]; then
            printf 'bootstrap-darwin: run: xcode-select --install\n'
            printf 'bootstrap-darwin: no Xcode? skip building entirely: `make mac-run` fetches the prebuilt signed binary (no CLT needed)\n'
            printf 'bootstrap-darwin: run it now? [y/N] '
            read -r ans
            [[ "$ans" == "y" || "$ans" == "Y" ]] \
                || { log "aborted: re-run after installing Xcode Command Line Tools"; exit 1; }
            xcode-select --install
            echo "bootstrap-darwin: complete the Command Line Tools installation dialog, then re-run ./scripts/bootstrap-darwin.sh" >&2
            exit 1
        else
            echo "bootstrap-darwin: ERROR: Xcode Command Line Tools missing and no way to install them (non-interactive; set KINDBOARD_BOOTSTRAP_YES=1 to auto-install)" >&2
            echo "bootstrap-darwin: run: xcode-select --install" >&2
            echo "bootstrap-darwin: no Xcode? skip building entirely: \`make mac-run\` fetches the prebuilt signed binary (no CLT needed)" >&2
            exit 1
        fi
    fi
fi

# ---------------------------------------------------------------------------
# 3. rust toolchain version (native macOS only)
# ---------------------------------------------------------------------------

RUST_MIN_VERSION="1.98" # macOS 26 SDK needs a recent rustc (>= 1.98)

version_ge() { # version_ge <a> <b>: true when a >= b (numeric major.minor)
    [[ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -n1)" == "$1" ]]
}

if [[ "$PLATFORM" == "darwin" ]]; then
    if ! command -v rustc >/dev/null 2>&1; then
        # rustc absent: skip this row entirely — the rustup targets step
        # (step 5) already reports rustup missing
        log "rustc not found — skipping the rust toolchain row (the rustup targets step reports rustup missing)"
    else
        rustc_full="$(rustc --version 2>/dev/null \
            | sed -n 's/^rustc \([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\).*/\1/p' || true)"
        rustc_mm="${rustc_full%.*}"
        if [[ -z "$rustc_mm" ]]; then
            report "rust toolchain" "MISSING" "could not parse 'rustc --version' — run: rustup update stable (need rustc >= $RUST_MIN_VERSION for the macOS 26 SDK)"
        elif version_ge "$rustc_mm" "$RUST_MIN_VERSION"; then
            report "rust toolchain" "OK" "rustc ${rustc_full:-$rustc_mm}"
        elif [[ "$CHECK_ONLY" == "1" ]]; then
            report "rust toolchain" "MISSING" "run: rustup update stable (need rustc >= $RUST_MIN_VERSION for the macOS 26 SDK)"
        else
            log "rustc ${rustc_full:-$rustc_mm} < $RUST_MIN_VERSION; running: rustup update stable"
            RUSTUP_OK=0
            rustup update stable 2>&1 | sed 's/^/bootstrap-darwin: rustup: /' || RUSTUP_OK=1
            rustc_new="$(rustc --version 2>/dev/null \
                | sed -n 's/^rustc \([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\).*/\1/p' || true)"
            if [[ "$RUSTUP_OK" == "0" ]] && version_ge "${rustc_new%.*}" "$RUST_MIN_VERSION"; then
                report "rust toolchain" "OK" "rustc ${rustc_new:-updated} (updated via rustup update stable)"
            else
                report "rust toolchain" "MISSING" "rustup update stable failed — need rustc >= $RUST_MIN_VERSION for the macOS 26 SDK"
            fi
        fi
    fi
fi

# ---------------------------------------------------------------------------
# 4. Host packages (Linux only)
# ---------------------------------------------------------------------------

if [[ "$PLATFORM" == "dnf" || "$PLATFORM" == "apt" ]]; then
    if [[ "$PLATFORM" == "dnf" ]]; then
        HOST_PKGS=(clang cmake lld llvm libstdc++-devel libstdc++-static \
            zlib-devel openssl-devel libxml2-devel)
    else
        HOST_PKGS=(clang cmake lld llvm libstdc++-dev zlib1g-dev libssl-dev \
            libxml2-dev)
    fi

    host_pkg_installed() {
        if [[ "$PLATFORM" == "dnf" ]]; then
            rpm -q --whatprovides "$1" >/dev/null 2>&1
        else
            dpkg -s "$1" >/dev/null 2>&1
        fi
    }

    MISSING_PKGS=()
    for pkg in "${HOST_PKGS[@]}"; do
        host_pkg_installed "$pkg" || MISSING_PKGS+=("$pkg")
    done

    if ((${#MISSING_PKGS[@]} == 0)); then
        clang_ver="$(clang --version 2>/dev/null | head -n1 || true)"
        report "host packages" "OK" "${clang_ver:-present}"
    elif [[ "$CHECK_ONLY" == "1" ]]; then
        report "host packages" "MISSING" "install: sudo ${PLATFORM/apt/apt-get} install -y ${MISSING_PKGS[*]}"
    else
        PM_BIN=dnf; [[ "$PLATFORM" == "apt" ]] && PM_BIN=apt-get
        INSTALL_CMD="sudo $PM_BIN install -y ${MISSING_PKGS[*]}"
        if [[ "${KINDBOARD_BOOTSTRAP_YES:-0}" == "1" ]] \
            || { command -v sudo >/dev/null 2>&1 && sudo -n true >/dev/null 2>&1; }; then
            log "installing missing host packages: ${MISSING_PKGS[*]}"
            sudo "$PM_BIN" install -y "${MISSING_PKGS[@]}"
        elif [[ -t 0 && -t 1 ]]; then
            printf 'bootstrap-darwin: host packages required:\n'
            printf 'bootstrap-darwin:   %s\n' "$INSTALL_CMD"
            printf 'bootstrap-darwin: run it now? [y/N] '
            read -r ans
            [[ "$ans" == "y" || "$ans" == "Y" ]] \
                || { log "aborted: re-run after installing host packages"; exit 1; }
            sudo "$PM_BIN" install -y "${MISSING_PKGS[@]}"
        else
            echo "bootstrap-darwin: ERROR: missing host packages and no way to install them (non-interactive; set KINDBOARD_BOOTSTRAP_YES=1 to auto-install)" >&2
            echo "bootstrap-darwin: run: $INSTALL_CMD" >&2
            exit 1
        fi
        STILL_MISSING=()
        for pkg in "${MISSING_PKGS[@]}"; do
            host_pkg_installed "$pkg" || STILL_MISSING+=("$pkg")
        done
        if ((${#STILL_MISSING[@]})); then
            echo "bootstrap-darwin: ERROR: package install finished but still missing: ${STILL_MISSING[*]}" >&2
            exit 1
        fi
        clang_ver="$(clang --version 2>/dev/null | head -n1 || true)"
        report "host packages" "OK" "installed ${MISSING_PKGS[*]}; ${clang_ver:-clang present}"
    fi
fi

# ---------------------------------------------------------------------------
# 5. rustup darwin targets
# ---------------------------------------------------------------------------

if [[ "$PLATFORM" == "darwin" ]]; then
    NATIVE_HOST="$(rustc -vV 2>/dev/null | sed -n 's/^host: //p' || true)"
    OTHER=""
    if [[ "$NATIVE_HOST" == "aarch64-apple-darwin" ]]; then
        OTHER="x86_64-apple-darwin"
    elif [[ "$NATIVE_HOST" == "x86_64-apple-darwin" ]]; then
        OTHER="aarch64-apple-darwin"
    fi
    if [[ -z "$NATIVE_HOST" ]]; then
        report "rustup targets" "MISSING" "rustup not found — install Rust via https://rustup.rs"
    elif [[ -z "$OTHER" ]]; then
        report "rustup targets" "MISSING" "unsupported rustc host '$NATIVE_HOST' — expected an apple-darwin host"
    elif ! command -v rustup >/dev/null 2>&1; then
        report "rustup targets" "MISSING" "rustup not found — install Rust via https://rustup.rs (needed to add target $OTHER)"
    elif [[ "$CHECK_ONLY" == "1" ]]; then
        if rustup target list --installed 2>/dev/null | grep -qxF "$OTHER"; then
            report "rustup targets" "OK" "native $NATIVE_HOST + $OTHER installed"
        else
            report "rustup targets" "MISSING" "run: rustup target add $OTHER"
        fi
    else
        if ! rustup target list --installed 2>/dev/null | grep -qxF "$OTHER"; then
            log "ensuring rustup target for the non-native darwin arch"
            rustup target add "$OTHER"
        fi
        if rustup target list --installed 2>/dev/null | grep -qxF "$OTHER"; then
            report "rustup targets" "OK" "native $NATIVE_HOST + $OTHER installed"
        else
            report "rustup targets" "MISSING" "rustup target add $OTHER did not install the target — re-run or add manually"
        fi
    fi
else
    if ! command -v rustup >/dev/null 2>&1; then
        report "rustup targets" "MISSING" "rustup not found — install Rust via https://rustup.rs"
    elif [[ "$CHECK_ONLY" == "1" ]]; then
        INSTALLED="$(rustup target list --installed 2>/dev/null || true)"
        TARGETS_MISSING=()
        for t in aarch64-apple-darwin x86_64-apple-darwin; do
            grep -qxF "$t" <<<"$INSTALLED" || TARGETS_MISSING+=("$t")
        done
        if ((${#TARGETS_MISSING[@]})); then
            report "rustup targets" "MISSING" "run: rustup target add ${TARGETS_MISSING[*]}"
        else
            report "rustup targets" "OK" "aarch64-apple-darwin, x86_64-apple-darwin installed"
        fi
    else
        log "ensuring rustup darwin targets"
        rustup target add aarch64-apple-darwin x86_64-apple-darwin
        report "rustup targets" "OK" "aarch64-apple-darwin, x86_64-apple-darwin installed"
    fi
fi

# ---------------------------------------------------------------------------
# 6. rcodesign
# ---------------------------------------------------------------------------

rcodesign_present() {
    local bin="${KINDBOARD_RCODESIGN:-}"
    [[ -z "$bin" ]] && bin="$(command -v rcodesign 2>/dev/null || true)"
    [[ -n "$bin" && -x "$bin" ]]
}

if rcodesign_present; then
    rc_bin="${KINDBOARD_RCODESIGN:-$(command -v rcodesign || true)}"
    rc_ver="$("$rc_bin" --version 2>/dev/null | head -n1 || true)"
    report "rcodesign" "OK" "$rc_bin${rc_ver:+ ($rc_ver)}"
elif [[ "$CHECK_ONLY" == "1" ]]; then
    report "rcodesign" "MISSING" "run: cargo install apple-codesign --locked"
else
    if ! command -v cargo >/dev/null 2>&1; then
        echo "bootstrap-darwin: ERROR: rcodesign missing and cargo not found — install Rust (https://rustup.rs), then: cargo install apple-codesign --locked" >&2
        exit 1
    fi
    log "installing rcodesign: cargo install apple-codesign --locked"
    cargo install apple-codesign --locked
    if ! rcodesign_present; then
        echo "bootstrap-darwin: ERROR: rcodesign still unavailable after install — ensure ~/.cargo/bin is on PATH and KINDBOARD_RCODESIGN (if set) points at an executable rcodesign" >&2
        exit 1
    fi
    rc_bin="${KINDBOARD_RCODESIGN:-$(command -v rcodesign || true)}"
    report "rcodesign" "OK" "$rc_bin (installed)"
fi

# ---------------------------------------------------------------------------
# 7. osxcross toolchain (Linux only)
# ---------------------------------------------------------------------------

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

osxcross_ready() {
    [[ -n "$(osxcross_wrapper x86_64 -clang)" ]] \
        && [[ -n "$(osxcross_wrapper aarch64 -clang)" ]]
}

sdk_cached_ok() {
    local sdk_path="$MACOSX_SDK_CACHE_DIR/$MACOSX_SDK_NAME"
    [[ -f "$sdk_path" ]] \
        && [[ "$(sha256sum "$sdk_path" 2>/dev/null || true)" == "$MACOSX_SDK_DIGEST  "* ]]
}

fetch_sdk() {
    local sdk_path="$MACOSX_SDK_CACHE_DIR/$MACOSX_SDK_NAME"
    mkdir -p "$MACOSX_SDK_CACHE_DIR"
    log "downloading macOS SDK $MACOSX_SDK_VERSION ($MACOSX_SDK_URL)"
    if ! curl --proto '=https' -fL --retry 3 -o "$sdk_path" "$MACOSX_SDK_URL"; then
        rm -f "$sdk_path"
        echo "bootstrap-darwin: ERROR: macOS SDK download failed: $MACOSX_SDK_URL" >&2
        return 1
    fi
    if [[ "$(sha256sum "$sdk_path")" != "$MACOSX_SDK_DIGEST  "* ]]; then
        rm -f "$sdk_path"
        echo "bootstrap-darwin: ERROR: macOS SDK digest mismatch for $MACOSX_SDK_NAME" >&2
        echo "bootstrap-darwin: expected $MACOSX_SDK_DIGEST (macOS $MACOSX_SDK_VERSION); re-pin per docs/adrs/ADR-0017.md — the fallback is never used automatically" >&2
        return 1
    fi
    log "SDK digest verified: $sdk_path"
}

if [[ "$PLATFORM" == "darwin" ]]; then
    report "osxcross" "n/a" "native macOS host"
elif osxcross_ready; then
    cc_wrap="$(osxcross_wrapper x86_64 -clang)"
    cc_ver="$("$cc_wrap" --version 2>/dev/null | head -n1 || true)"
    report "osxcross wrappers" "OK" "$OSXCROSS_DIR${cc_ver:+ ($cc_ver)}"
    if sdk_cached_ok; then
        report "SDK cache" "OK" "$MACOSX_SDK_CACHE_DIR/$MACOSX_SDK_NAME (digest verified)"
    elif [[ "$CHECK_ONLY" == "1" ]]; then
        report "SDK cache" "MISSING" "$MACOSX_SDK_CACHE_DIR/$MACOSX_SDK_NAME (build.sh will download it at build time)"
    else
        fetch_sdk || exit 1
        report "SDK cache" "OK" "$MACOSX_SDK_CACHE_DIR/$MACOSX_SDK_NAME (downloaded, digest verified)"
    fi
elif [[ "$CHECK_ONLY" == "1" ]]; then
    report "osxcross wrappers" "MISSING" "$OSXCROSS_DIR (run ./scripts/bootstrap-darwin.sh)"
    if sdk_cached_ok; then
        report "SDK cache" "OK" "$MACOSX_SDK_CACHE_DIR/$MACOSX_SDK_NAME (digest verified)"
    else
        report "SDK cache" "MISSING" "$MACOSX_SDK_CACHE_DIR/$MACOSX_SDK_NAME"
    fi
else
    if ! command -v git >/dev/null 2>&1; then
        echo "bootstrap-darwin: ERROR: git is required to clone osxcross" >&2
        exit 1
    fi
    if [[ ! -d "$OSXCROSS_SRC_DIR" ]]; then
        log "cloning osxcross -> $OSXCROSS_SRC_DIR"
        git clone https://github.com/tpoechtrager/osxcross "$OSXCROSS_SRC_DIR"
        git -C "$OSXCROSS_SRC_DIR" checkout --detach "$OSXCROSS_PIN"
    else
        head_sha="$(git -C "$OSXCROSS_SRC_DIR" rev-parse HEAD 2>/dev/null || true)"
        if [[ "$head_sha" != "$OSXCROSS_PIN"* ]]; then
            log "WARNING: existing osxcross source at $OSXCROSS_SRC_DIR is not at the pinned commit $OSXCROSS_PIN (found ${head_sha:-unknown}); using it as-is"
        fi
    fi
    if [[ ! -x "$OSXCROSS_SRC_DIR/build.sh" ]]; then
        echo "bootstrap-darwin: ERROR: $OSXCROSS_SRC_DIR/build.sh missing — remove the directory and re-run" >&2
        exit 1
    fi
    sdk_cached_ok || fetch_sdk || exit 1
    TARBALLS_DIR="$OSXCROSS_SRC_DIR/tarballs"
    mkdir -p "$TARBALLS_DIR"
    if [[ ! -f "$TARBALLS_DIR/$MACOSX_SDK_NAME" ]] \
        || [[ "$(sha256sum "$TARBALLS_DIR/$MACOSX_SDK_NAME" 2>/dev/null || true)" != "$MACOSX_SDK_DIGEST  "* ]]; then
        log "staging SDK tarball into $TARBALLS_DIR"
        cp "$MACOSX_SDK_CACHE_DIR/$MACOSX_SDK_NAME" "$TARBALLS_DIR/$MACOSX_SDK_NAME"
    fi
    BUILD_LOG="$(mktemp)"
    log "building osxcross into $OSXCROSS_DIR (one-time; can take 30+ minutes)"
    if ! (cd "$OSXCROSS_SRC_DIR" && UNATTENDED=1 TARGET_DIR="$OSXCROSS_DIR" OSX_VERSION_MIN=10.13 ./build.sh 2>&1 | tee "$BUILD_LOG"); then
        echo "bootstrap-darwin: ERROR: osxcross build failed — last 40 lines of the build log:" >&2
        tail -40 "$BUILD_LOG" >&2
        rm -f "$BUILD_LOG"
        exit 1
    fi
    rm -f "$BUILD_LOG"
    if ! osxcross_ready; then
        echo "bootstrap-darwin: ERROR: osxcross build finished but the clang wrappers are missing from $OSXCROSS_DIR/bin" >&2
        exit 1
    fi
    cc_wrap="$(osxcross_wrapper x86_64 -clang)"
    cc_ver="$("$cc_wrap" --version 2>/dev/null | head -n1 || true)"
    report "osxcross wrappers" "OK" "$OSXCROSS_DIR (built)${cc_ver:+ ($cc_ver)}"
    report "SDK cache" "OK" "$MACOSX_SDK_CACHE_DIR/$MACOSX_SDK_NAME (digest verified)"
fi

# ---------------------------------------------------------------------------
# 8. Status table
# ---------------------------------------------------------------------------

log "== status =="
for row in "${ROWS[@]}"; do
    IFS='|' read -r label status detail <<<"$row"
    printf 'bootstrap-darwin: %-18s %-8s %s\n' "$label" "$status" "$detail"
done

if (( MISSING > 0 )); then
    if [[ "$CHECK_ONLY" == "1" ]]; then
        log "check: $MISSING dependency/dependencies missing; run ./scripts/bootstrap-darwin.sh to resolve (exit 1)"
    else
        log "ERROR: $MISSING dependency/dependencies unresolved (exit 1)"
    fi
    exit 1
fi

if [[ "$CHECK_ONLY" == "1" ]]; then
    log "check: all darwin dependencies present (exit 0)"
else
    log "all darwin cross-build dependencies resolved (exit 0)"
fi
exit 0
