#!/usr/bin/env bash
# kindboard Linux desktop installer — local, user-scoped, freedesktop-compliant.
#
# Usage: ./scripts/install-linux.sh [--build | --run | --help]
#
#   (no args)  build (if needed) + install kindboard and its launcher icon
#              into $HOME/.local (bin/ + share/), no sudo, no system paths
#   --build    force a release rebuild even if target/release/kindboard exists
#   --run      install (if needed), then exec kindboard in the foreground
#   --help     show this help
#
# What it does:
#   1. builds the release binary when target/release/kindboard is missing or
#      --build is passed (cargo build --release --package kindboard-app);
#      KINDBOARD_NO_BUILD=1 skips the build entirely (fails if the binary is
#      missing)
#   2. installs the binary to $prefix/bin/kindboard (mode 755)
#   3. installs the five icon sizes as $prefix/share/icons/hicolor/<s>x<s>/
#      apps/kindboard.png (the freedesktop icon NAME is "kindboard"; the
#      artwork is the ORP mark)
#   4. installs the desktop entry with the absolute Exec= path substituted
#      (desktop files cannot expand ~ or $HOME)
#   5. refreshes the desktop/icon caches, best-effort (update-desktop-database
#      and gtk-update-icon-cache when present; never fatal)
#
# Zero-cost, zero-sudo: everything lands under $HOME/.local, and the launcher
# resolves it via the standard XDG data dirs (works with fuzzel/niri, GNOME,
# KDE, etc.). No network at install time — the binary is built locally from
# this checkout.
#
# Idempotent: safe to re-run; every installed file is overwritten in place.
#
# Env:
#   KINDBOARD_PREFIX   install prefix (default: $HOME/.local)
#   KINDBOARD_NO_BUILD skip the cargo build (default: unset)
#
# Log prefix: `install-linux:`.

set -euo pipefail

log() { printf 'install-linux: %s\n' "$*"; }

err() { # err <message> [exit-code]
    printf 'install-linux: ERROR: %s\n' "$1" >&2
    exit "${2:-1}"
}

usage() {
    cat <<'EOF'
Usage: ./scripts/install-linux.sh [--build | --run | --help]

  (no args)  build (if needed) + install kindboard + launcher icon locally
  --build    force a release rebuild
  --run      install (if needed), then launch kindboard
  --help     show this help

Env:
  KINDBOARD_PREFIX   install prefix (default: $HOME/.local)
  KINDBOARD_NO_BUILD skip the cargo build (default: unset)
EOF
    exit 0
}

# ---------------------------------------------------------------------------
# 0. Resolve the repo root from the script's own path (safe from any CWD)
# ---------------------------------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(dirname "$SCRIPT_DIR")"

# ---------------------------------------------------------------------------
# 1. Args
# ---------------------------------------------------------------------------

BUILD=0
RUN=0
while (($#)); do
    case "$1" in
        --build) BUILD=1 ;;
        --run) RUN=1 ;;
        -h|--help) usage ;;
        *) err "unknown option: $1 (use --build, --run, or --help)" 2 ;;
    esac
    shift
done

PREFIX="${KINDBOARD_PREFIX:-$HOME/.local}"

# ---------------------------------------------------------------------------
# 2. Build (when needed)
# ---------------------------------------------------------------------------

BIN_SRC="$ROOT/target/release/kindboard"
BIN_DST="$PREFIX/bin/kindboard"

if [[ "${KINDBOARD_NO_BUILD:-0}" == "1" ]]; then
    [[ -f "$BIN_SRC" ]] || err "target/release/kindboard is missing and KINDBOARD_NO_BUILD=1 — build it first, or drop KINDBOARD_NO_BUILD"
    log "KINDBOARD_NO_BUILD=1 — using the existing $BIN_SRC (build skipped)"
elif [[ "$BUILD" == "1" || ! -f "$BIN_SRC" ]]; then
    if [[ "$BUILD" == "1" ]]; then
        log "--build passed — forcing a release rebuild"
    else
        log "$BIN_SRC not found — building it"
    fi
    ( cd "$ROOT" && cargo build --release --package kindboard-app )
else
    log "$BIN_SRC present — skipping the build (pass --build to force)"
fi

# ---------------------------------------------------------------------------
# 3. Install the binary
# ---------------------------------------------------------------------------

mkdir -p "$PREFIX/bin"
install -m755 "$BIN_SRC" "$BIN_DST"
log "installed $BIN_DST"

# ---------------------------------------------------------------------------
# 4. Icons (freedesktop hicolor theme; icon name stays "kindboard")
# ---------------------------------------------------------------------------

ICON_BASE="$PREFIX/share/icons/hicolor"
for s in 32 64 128 256 512; do
    size="${s}x${s}"
    mkdir -p "$ICON_BASE/$size/apps"
    cp "$ROOT/assets/icons/orp-mark-${s}.png" "$ICON_BASE/$size/apps/kindboard.png"
done
log "installed icons to $ICON_BASE/{32x32,64x64,128x128,256x256,512x512}/apps/kindboard.png"

# ---------------------------------------------------------------------------
# 5. Desktop entry (substitute the absolute Exec= path)
# ---------------------------------------------------------------------------

mkdir -p "$PREFIX/share/applications"
sed "s|@KINDBOARD_EXEC@|$PREFIX/bin/kindboard|" \
    "$ROOT/packaging/linux/kindboard.desktop" \
    > "$PREFIX/share/applications/kindboard.desktop"
log "installed $PREFIX/share/applications/kindboard.desktop"

# ---------------------------------------------------------------------------
# 6. Refresh caches (best-effort, never fatal)
# ---------------------------------------------------------------------------

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$PREFIX/share/applications" || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -t "$ICON_BASE" || true
fi

# ---------------------------------------------------------------------------
# 7. Summary + PATH hint (never touches shell files)
# ---------------------------------------------------------------------------

log "done:"
log "  binary:        $BIN_DST"
log "  desktop entry: $PREFIX/share/applications/kindboard.desktop"
log "  icons:         $ICON_BASE/<size>/apps/kindboard.png (32..512)"

case ":$PATH:" in
    *":$PREFIX/bin:"*) ;;
    *) printf 'install-linux: note: %s is not on your PATH — add it with: export PATH="%s:$PATH"\n' "$PREFIX/bin" "$PREFIX/bin" ;;
esac

# ---------------------------------------------------------------------------
# 8. --run: launch in the foreground
# ---------------------------------------------------------------------------

if [[ "$RUN" == "1" ]]; then
    exec "$BIN_DST"
fi
