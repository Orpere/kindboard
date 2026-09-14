#!/usr/bin/env bash
# kindboard macOS app bundler (macOS-only convenience; run it on a Mac).
#
# Usage: ./scripts/make-mac-app.sh   (or: make mac-app)
#
# Builds the native release binary WITHOUT the full quality gates
# (incremental-friendly) and assembles a double-clickable bundle at
# dist/kindboard.app, ad-hoc signed with rcodesign (pure Rust). Because the
# binary is built locally it carries no com.apple.quarantine xattr, so
# Gatekeeper shows ZERO prompts — no "unidentified developer", no
# right-click → Open dance.
#
# Out of scope: repackaging Linux-built dist/ binaries into a bundle
# (dist/<triple>/kindboard from an osxcross build could be wired in later).
#
# Steps:
#   1. macOS guard
#   2. tool guard: rcodesign (make darwin-bootstrap installs it) + cargo
#   3. cargo build --release -p kindboard-app (no gates)
#   4. bundle skeleton: Contents/MacOS/kindboard + Contents/Info.plist
#   5. optional icon from assets/ (only .icns is usable as CFBundleIconFile)
#   6. ad-hoc sign the BUNDLE: `rcodesign sign <bundle dir>` — rcodesign signs
#      a bundle directory RECURSIVELY ("If the input is a bundle, the bundle
#      will be recursively signed", rcodesign help sign)
#   7. structural verification of the inner binary:
#      LC_CODE_SIGNATURE load command + CSMAGIC_EMBEDDED magic at its dataoff,
#      exactly like build.sh's verify_darwin_signature. llvm-objdump may be
#      absent on macOS (Xcode CLT does not always ship it), so the check
#      falls back to `rcodesign print-signature-info | grep -q
#      macho_signature_start_offset` — rcodesign's own reader — when
#      llvm-objdump is missing.
#   8. open the bundle
#
# Log prefix: `make-mac-app:`.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd)"

log() { printf 'make-mac-app: %s\n' "$*"; }
error() { printf 'make-mac-app: ERROR: %s\n' "$*" >&2; exit 1; }

# ---------------------------------------------------------------------------
# 1. macOS guard
# ---------------------------------------------------------------------------

[[ "$(uname -s)" == "Darwin" ]] \
    || error "run this on macOS — a kindboard.app bundle must be assembled and signed on a Mac"

# ---------------------------------------------------------------------------
# 2. tool guard
# ---------------------------------------------------------------------------

command -v rcodesign >/dev/null 2>&1 \
    || error "rcodesign not found — run: make darwin-bootstrap"
command -v cargo >/dev/null 2>&1 \
    || error "cargo not found — install Rust via https://rustup.rs"

APP_NAME="kindboard"
APP_VERSION="$(grep -m1 '^version' "$ROOT_DIR/crates/kindboard-app/Cargo.toml" | cut -d'"' -f2)"
[[ -n "$APP_VERSION" ]] || error "could not read the app version from crates/kindboard-app/Cargo.toml"
APP_BUNDLE="$ROOT_DIR/dist/kindboard.app"
CONTENTS="$APP_BUNDLE/Contents"
MACOS_DIR="$CONTENTS/MacOS"
RESOURCES_DIR="$CONTENTS/Resources"

# ---------------------------------------------------------------------------
# 3. native release build (no gates — incremental-friendly)
# ---------------------------------------------------------------------------

log "building native release binary (cargo build --release -p kindboard-app; gates skipped)"
if ! (cd "$ROOT_DIR" && cargo build --release -p kindboard-app); then
    error "cargo build failed — make darwin-bootstrap first if the C compiler is missing"
fi

HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
BIN_SRC="$ROOT_DIR/target/$HOST_TRIPLE/release/kindboard"
[[ -f "$BIN_SRC" ]] \
    || error "built binary not found at $BIN_SRC (rustc host: $HOST_TRIPLE)"

# ---------------------------------------------------------------------------
# 4. bundle skeleton + Info.plist
# ---------------------------------------------------------------------------

# Optional icon: only an .icns is usable as CFBundleIconFile (a bare PNG in
# Resources/ is ignored); PNGs in assets/ need an icns conversion first.
ICON_SRC=""
for c in "$ROOT_DIR"/assets/*.icns "$ROOT_DIR"/assets/icons/*.icns; do
    if [[ -e "$c" ]]; then
        ICON_SRC="$c"
        break
    fi
done

rm -rf "$APP_BUNDLE"
mkdir -p "$MACOS_DIR" "$RESOURCES_DIR"
cp "$BIN_SRC" "$MACOS_DIR/$APP_NAME"
if [[ -n "$ICON_SRC" ]]; then
    ICON_NAME="$(basename "$ICON_SRC")"
    cp "$ICON_SRC" "$RESOURCES_DIR/$ICON_NAME"
    log "icon: $RESOURCES_DIR/$ICON_NAME"
else
    ICON_NAME=""
    log "note: no .icns icon found in assets/ (PNGs need icns conversion) — bundling without an icon"
fi

cat > "$CONTENTS/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key><string>kindboard</string>
	<key>CFBundleDisplayName</key><string>kindboard</string>
	<key>CFBundleIdentifier</key><string>io.github.orpere.kindboard</string>
	<key>CFBundleExecutable</key><string>kindboard</string>
	<key>CFBundlePackageType</key><string>APPL</string>
	<key>CFBundleShortVersionString</key><string>$APP_VERSION</string>
	<key>CFBundleVersion</key><string>$APP_VERSION</string>
	<key>LSMinimumSystemVersion</key><string>11.0</string>
	<key>NSHighResolutionCapable</key><true/>
	<key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
EOF
if [[ -n "$ICON_NAME" ]]; then
    cat >> "$CONTENTS/Info.plist" <<EOF
	<key>CFBundleIconFile</key><string>$ICON_NAME</string>
EOF
fi
cat >> "$CONTENTS/Info.plist" <<'EOF'
</dict>
</plist>
EOF
log "bundle assembled: $APP_BUNDLE (kindboard $APP_VERSION)"

# ---------------------------------------------------------------------------
# 6. ad-hoc sign the bundle (recursive)
# ---------------------------------------------------------------------------

# `rcodesign sign` accepts a bundle DIRECTORY path and signs it recursively:
# "If the input is a bundle, the bundle will be recursively signed. If the
# bundle contains nested bundles or Mach-O binaries, those will be signed
# automatically." (rcodesign help sign). No identity → ad-hoc signature.
if ! rcodesign sign "$APP_BUNDLE"; then
    error "rcodesign sign failed for $APP_BUNDLE"
fi
log "ad-hoc signed: $APP_BUNDLE"

# ---------------------------------------------------------------------------
# 7. structural verification of the inner binary
# ---------------------------------------------------------------------------

INNER_BIN="$MACOS_DIR/$APP_NAME"
if command -v llvm-objdump >/dev/null 2>&1; then
    # Same gate as build.sh on Linux: LC_CODE_SIGNATURE load command present
    # + CSMAGIC_EMBEDDED magic (fade0cc0) at its dataoff.
    stanza="$(llvm-objdump --macho --private-headers "$INNER_BIN" 2>/dev/null \
        | grep -A3 'LC_CODE_SIGNATURE')" \
        || error "no LC_CODE_SIGNATURE load command in $INNER_BIN"
    off="$(awk '/dataoff/{print $2}' <<<"$stanza")"
    if [[ ! "$off" =~ ^[0-9]+$ ]]; then
        error "could not parse LC_CODE_SIGNATURE dataoff in $INNER_BIN"
    fi
    magic="$(od -A n -t x1 -j "$off" -N 4 "$INNER_BIN" | tr -d ' \n')"
    [[ "$magic" == "fade0cc0" ]] \
        || error "signature blob magic mismatch at dataoff $off: $magic (expected fade0cc0)"
    log "verified: LC_CODE_SIGNATURE + CSMAGIC_EMBEDDED present in $INNER_BIN"
else
    # Xcode CLT does not always ship llvm-objdump on macOS; fall back to
    # rcodesign's own reader — a non-zero macho_signature_start_offset proves
    # an embedded signature blob exists.
    sig_off="$(rcodesign print-signature-info "$INNER_BIN" 2>/dev/null \
        | sed -n 's/^[[:space:]]*macho_signature_start_offset:[[:space:]]*\([0-9]*\).*/\1/p')"
    if [[ -z "$sig_off" || "$sig_off" == "0" ]]; then
        error "rcodesign print-signature-info shows no macho_signature_start_offset in $INNER_BIN"
    fi
    log "verified via rcodesign print-signature-info (llvm-objdump not found): $INNER_BIN (offset $sig_off)"
fi

# ---------------------------------------------------------------------------
# 8. open the bundle
# ---------------------------------------------------------------------------

if ! open "$APP_BUNDLE"; then
    log "could not open the bundle automatically — open it manually: open $APP_BUNDLE"
fi
log "double-clickable app at dist/kindboard.app — no Gatekeeper prompt because it was built locally; after code changes re-run: make mac-app"
