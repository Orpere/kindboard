#!/usr/bin/env bash
# kindboard macOS installer — prebuilt, checksum-verified, quarantine-cleared.
#
# Usage: ./scripts/install-macos.sh [--run | --help]
#
#   (no args)  install kindboard to $HOME/.local/bin/kindboard
#   --run      install (if needed), then exec kindboard in the foreground
#   --help     show this help
#
# What it does:
#   1. resolves the release to install: KINDBOARD_VERSION env override
#      (e.g. v0.1.6), else the latest GitHub release via the releases API
#   2. downloads kindboard-darwin-<arch>.tar.gz + SHA256SUMS for that tag
#   3. verifies the tarball sha256 against SHA256SUMS (sha256sum when
#      present, else shasum -a 256 — macOS ships the latter)
#   4. extracts to $HOME/.local/bin/kindboard (user-scoped, no sudo) and
#      verifies the binary is a 64-bit Mach-O of the right CPU arch — a
#      wrong-platform download fails here with an actionable error, never
#      as `zsh: exec format error` at first launch
#   5. clears com.apple.quarantine (xattr -c): the release binary is
#      ad-hoc signed (ADR-0018); Gatekeeper only evaluates files carrying
#      com.apple.quarantine, so clearing it at install time means the
#      first launch shows NO prompt — no Apple account, no certificate,
#      no notarization, no Xcode tools needed
#
# Idempotent: an existing binary already reporting the resolved version is
# left alone (and is exec'd directly under --run, so offline reuse works).
# Everything is user-scoped: no sudo anywhere.
#
# Env:
#   KINDBOARD_VERSION=vX.Y.Z  pin a specific release (default: latest)
#   KINDBOARD_INSTALL_DIR     install dir (default: $HOME/.local/bin;
#                             test/override knob)
#
# Log prefix: `install-macos:`.

set -euo pipefail

log() { printf 'install-macos: %s\n' "$*"; }

err() { # err <message> [exit-code]
    printf 'install-macos: ERROR: %s\n' "$1" >&2
    exit "${2:-1}"
}

usage() {
    sed -n '2,34p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
}

# ---------------------------------------------------------------------------
# 1. Args
# ---------------------------------------------------------------------------

RUN=0
while (($#)); do
    case "$1" in
        --run) RUN=1 ;;
        -h|--help) usage ;;
        *) echo "install-macos: unknown option: $1 (use --run or --help)" >&2; exit 2 ;;
    esac
    shift
done

# ---------------------------------------------------------------------------
# 2. Platform + arch
# ---------------------------------------------------------------------------

[[ "$(uname -s)" == "Darwin" ]] || err "run this on macOS — this installer fetches the prebuilt darwin binary"

case "$(uname -m)" in
    arm64)  ARCH=arm64 ;;
    x86_64) ARCH=x86_64 ;;
    *) err "unsupported architecture '$(uname -m)' (need arm64 or x86_64)" ;;
esac

INSTALL_DIR="${KINDBOARD_INSTALL_DIR:-$HOME/.local/bin}"
DEST="$INSTALL_DIR/kindboard"

# ---------------------------------------------------------------------------
# 3. Release version
# ---------------------------------------------------------------------------

TAG="${KINDBOARD_VERSION:-}"
if [[ -z "$TAG" ]]; then
    log "no KINDBOARD_VERSION set — resolving the latest release via the GitHub API"
    if ! TAG="$(curl -fsSL --proto '=https' \
        https://api.github.com/repos/Orpere/kindboard/releases/latest \
        | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p')"; then
        err "could not resolve the latest release — set KINDBOARD_VERSION=vX.Y.Z and retry (offline?)"
    fi
    log "latest release: $TAG"
else
    [[ "$TAG" == v* ]] || err "KINDBOARD_VERSION must look like vX.Y.Z (got '$TAG')"
fi
VERNUM="${TAG#v}"
[[ -n "$VERNUM" ]] || err "bad release tag '$TAG'"

# ---------------------------------------------------------------------------
# 4. Idempotency: skip when the installed binary already reports the version
# ---------------------------------------------------------------------------

if [[ -x "$DEST" ]]; then
    VER_OUT="$("$DEST" --version 2>/dev/null || true)"
    VER_NUM="$(printf '%s\n' "$VER_OUT" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -n1 || true)"
    if [[ "$VER_NUM" == "$VERNUM" ]]; then
        log "already installed ($VERNUM) — nothing to do"
        if [[ "$RUN" == "1" ]]; then
            exec "$DEST"
        fi
        exit 0
    fi
    log "existing $DEST does not report $VERNUM (got: '${VER_OUT:-<no output>}') — reinstalling"
fi

# ---------------------------------------------------------------------------
# 5. Download tarball + checksums
# ---------------------------------------------------------------------------

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

BASE="https://github.com/Orpere/kindboard/releases/download/${TAG}"
TARBALL="kindboard-darwin-${ARCH}.tar.gz"

log "downloading $BASE/$TARBALL"
curl -fsSL --proto '=https' -o "$WORK_DIR/$TARBALL" "$BASE/$TARBALL" \
    || err "download failed: $BASE/$TARBALL"
log "downloading $BASE/SHA256SUMS"
curl -fsSL --proto '=https' -o "$WORK_DIR/SHA256SUMS" "$BASE/SHA256SUMS" \
    || err "download failed: $BASE/SHA256SUMS"

# ---------------------------------------------------------------------------
# 6. Checksum verification
# ---------------------------------------------------------------------------

sha256_of() { # sha256_of <file> -> hex digest
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

EXPECTED="$(awk -v f="$TARBALL" '$2==f{print $1}' "$WORK_DIR/SHA256SUMS")"
[[ -n "$EXPECTED" ]] || err "SHA256SUMS has no entry for $TARBALL — refusing to install"
ACTUAL="$(sha256_of "$WORK_DIR/$TARBALL")"
if [[ "$ACTUAL" != "$EXPECTED" ]]; then
    err "checksum mismatch for $TARBALL (expected $EXPECTED, got $ACTUAL) — refusing to install"
fi
log "checksum verified: $ACTUAL"

# ---------------------------------------------------------------------------
# 7. Extract + install
# ---------------------------------------------------------------------------

log "extracting $TARBALL"
tar -xzf "$WORK_DIR/$TARBALL" -C "$WORK_DIR"
[[ -f "$WORK_DIR/kindboard" ]] || err "tarball does not contain a 'kindboard' binary — refusing to install"

# ---------------------------------------------------------------------------
# 7b. Mach-O format guard — a wrong-platform tarball must fail HERE, with an
# actionable message, never later as `zsh: exec format error` at launch.
# ---------------------------------------------------------------------------

# First 8 bytes of a 64-bit Mach-O (little-endian MH_MAGIC_64 + cputype):
#   arm64   cffaedfe 0c000001   (CPU_TYPE_ARM64  0x0100000C)
#   x86_64  cffaedfe 07000001   (CPU_TYPE_X86_64 0x01000007)
MACHO_MAGIC_ARM64="cffaedfe0c000001"
MACHO_MAGIC_X86_64="cffaedfe07000001"

verify_macho() { # verify_macho <file> <arch>   (arch: arm64|x86_64)
    local file="$1" arch="$2" head
    command -v od >/dev/null 2>&1 || err "od not found — cannot verify the downloaded binary"
    head="$(od -An -tx1 -N8 "$file" | tr -d ' \n')"
    case "$head" in
        cafebabe*|bebafeca*)
            err "downloaded binary is a universal (fat) Mach-O — kindboard ships thin binaries; download the $arch tarball for your Mac from https://github.com/Orpere/kindboard/releases"
            ;;
        cffaedfe*|cefaedfe*)
            if [[ "$arch" == "arm64" ]]; then
                [[ "$head" == "$MACHO_MAGIC_ARM64" ]] \
                    || err "downloaded binary is not an arm64 Mach-O (different CPU type) — you likely grabbed the Intel tarball; get the Apple Silicon one from https://github.com/Orpere/kindboard/releases"
            else
                [[ "$head" == "$MACHO_MAGIC_X86_64" ]] \
                    || err "downloaded binary is not an x86_64 Mach-O (different CPU type) — you likely grabbed the Apple Silicon tarball; get the Intel one from https://github.com/Orpere/kindboard/releases"
            fi
            ;;
        *)
            err "downloaded file is not a macOS (Mach-O) binary — wrong platform tarball? expected kindboard-darwin-$arch.tar.gz from https://github.com/Orpere/kindboard/releases"
            ;;
    esac
}

verify_macho "$WORK_DIR/kindboard" "$ARCH"

mkdir -p "$INSTALL_DIR"
mv "$WORK_DIR/kindboard" "$DEST"
chmod 755 "$DEST"
log "installed $DEST ($VERNUM)"

# ---------------------------------------------------------------------------
# 8. Quarantine clear — the whole point of this installer
# ---------------------------------------------------------------------------

# The release binary is ad-hoc signed (ADR-0018). Gatekeeper only evaluates
# files carrying the com.apple.quarantine xattr (macOS stamps it on
# downloaded files), so clearing it here means the first launch shows NO
# prompt — no Apple account, certificate, or notarization needed.
if command -v xattr >/dev/null 2>&1; then
    if xattr -c "$DEST" 2>/dev/null; then
        log "cleared the download quarantine flag ($DEST)"
    else
        log "WARNING: could not clear the quarantine flag on $DEST — first launch may show a Gatekeeper prompt"
    fi
else
    log "xattr not found — skipping the quarantine clear (non-macOS host?)"
fi

# ---------------------------------------------------------------------------
# 9. PATH hint (never touches shell files)
# ---------------------------------------------------------------------------

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) printf 'install-macos: note: %s is not on your PATH — add it with: export PATH="%s:$PATH"\n' "$INSTALL_DIR" "$INSTALL_DIR" ;;
esac

# ---------------------------------------------------------------------------
# 10. --run: launch in the foreground (cleanup before exec — EXIT traps do
# not fire across exec)
# ---------------------------------------------------------------------------

if [[ "$RUN" == "1" ]]; then
    rm -rf "$WORK_DIR"
    trap - EXIT
    exec "$DEST"
fi
