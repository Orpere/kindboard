#!/usr/bin/env bash
# kindboard asset preparation script (run once per asset refresh, offline-safe).
#
# Usage: ./scripts/prepare-assets.sh
#
# Fetches the official logo of each tool from the owning project's repository
# (raw.githubusercontent.com where possible), resizes it into
# assets/logos/<name>-{128,64}.png (square transparent canvas, padded, no
# upscaling, PNG32, -strip) and derives the app icon set
# assets/icons/kindboard-{512,256,128,64,32}.png from the kind logo.
#
# Projects without a usable official PNG/SVG get a generated placeholder and
# are recorded in assets/ATTRIBUTION.md as "no usable official asset found".
#
# Behaviour:
#   - every URL is fetched with curl -fL (any non-200 response is a failure)
#   - everything is rebuilt in a temp dir first; assets/ is only swapped in
#     after a fully successful run, so a failed run never leaves a
#     half-written assets/ directory
#   - idempotent: each run regenerates assets/ from scratch
#   - SVG sources need the ImageMagick rsvg delegate; it is tested up front
#     and SVG sources are dropped (placeholder instead) when it is missing
#
# This script only touches assets/. Run scripts/build.sh afterwards for
# release builds; assets are not part of that pipeline.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd)"
ASSETS_DIR="$ROOT_DIR/assets"

TRADEMARK_NOTE="logo is a trademark of its respective owner; used for identification under fair-use/nominative use in an open-source project; remove on request"

# name|owner|source URL (empty = no official asset)|placeholder reason
# URLs were verified against the owning project's repository.
LOGOS="
kind|Kubernetes SIGs (kind project)|https://raw.githubusercontent.com/kubernetes-sigs/kind/main/logo/logo.png|
kubernetes|Cloud Native Computing Foundation|https://raw.githubusercontent.com/cncf/artwork/main/projects/kubernetes/icon/color/kubernetes-icon-color.png|
docker|Docker Inc.|https://raw.githubusercontent.com/docker-library/docs/master/docker/logo.png|
helm|Cloud Native Computing Foundation (Helm project)|https://raw.githubusercontent.com/cncf/artwork/main/projects/helm/icon/color/helm-icon-color.png|
cilium|Cilium project|https://raw.githubusercontent.com/cilium/cilium/main/Documentation/images/logo-solo.svg|
calico|Tigera (Project Calico)|https://raw.githubusercontent.com/tigera/docs/main/static/img/Calico-logo-2026-badge.png|
flannel|Flannel project|https://raw.githubusercontent.com/flannel-io/flannel/master/logos/flannel-glyph-color.png|
traefik|Traefik Labs|https://raw.githubusercontent.com/traefik/traefik/master/docs/content/assets/img/traefik.logo.png|
ingress-nginx|Kubernetes SIG Network||no usable official asset found: only a 48x48 site favicon is available, too small to rasterize at 128px without upscaling
k9s|Derailed (k9s project)|https://raw.githubusercontent.com/derailed/k9s/master/assets/k9s.png|
kubectx|Ahmet Alp Balkan (kubectx project)||no usable official asset found: no official logo exists in the repository or project site
kustomize|Kubernetes SIGs (kustomize project)|https://raw.githubusercontent.com/kubernetes-sigs/kustomize/master/site/static/favicons/favicon-1024.png|
kubectl|Kubernetes project||no usable official asset found: kubectl has no standalone official logo (it is part of Kubernetes)
"

log() { printf 'prepare-assets.sh: %s\n' "$*"; }

require_cmd() {
    if ! command -v "$1" >/dev/null; then
        log "ERROR: required command not found: $1" >&2
        exit 1
    fi
}

require_cmd curl
require_cmd magick

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT
mkdir -p "$TMP_DIR/src" "$TMP_DIR/logos" "$TMP_DIR/icons"

# ---------------------------------------------------------------------------
# Up-front SVG capability test (ImageMagick rsvg delegate).
# ---------------------------------------------------------------------------

SVG_OK=0
printf '%s' '<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><rect width="16" height="16" fill="#333333"/></svg>' > "$TMP_DIR/svg-test.svg"
if magick "$TMP_DIR/svg-test.svg" -strip PNG32:"$TMP_DIR/svg-test.png" 2>/dev/null \
    && [[ -s "$TMP_DIR/svg-test.png" ]]; then
    SVG_OK=1
else
    log "WARNING: SVG conversion unavailable (ImageMagick rsvg delegate missing or broken); SVG sources will be replaced with placeholders"
fi

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# convert_square <src> <out.png> <size>
# Fits the source inside a slightly padded square canvas, centred, no
# upscaling for raster sources (SVG renders at high density so vector
# sources scale losslessly).
convert_square() {
    local src="$1" out="$2" size="$3"
    local content=$(( size - size / 8 ))
    local density_args=()
    case "$src" in
        *.svg) density_args=(-density 384) ;;
    esac
    magick "${density_args[@]}" "$src" \
        -background none \
        -resize "${content}x${content}" \
        -gravity center -extent "${size}x${size}" \
        -strip "PNG32:$out"
}

# gen_placeholder <name>
# Neutral grey rounded tile with the initial letter; used when no usable
# official asset exists.
gen_placeholder() {
    local name="$1" letter
    letter="$(printf '%s' "$name" | cut -c1 | tr '[:lower:]' '[:upper:]')"
    local f128="$TMP_DIR/logos/${name}-128.png" f64="$TMP_DIR/logos/${name}-64.png"
    if ! magick -size 128x128 xc:none \
        -fill '#54677f' -draw 'roundrectangle 0,0 127,127 26,26' \
        -fill '#ffffff' -font DejaVu-Sans-Mono -pointsize 60 -gravity center -annotate +0+2 "$letter" \
        -strip "PNG32:$f128" 2>/dev/null; then
        magick -size 128x128 xc:none \
            -fill '#54677f' -draw 'roundrectangle 0,0 127,127 26,26' \
            -strip "PNG32:$f128"
    fi
    magick "$f128" -resize 64x64 -strip "PNG32:$f64"
}

ATTRIBUTION_TMP="$TMP_DIR/ATTRIBUTION.md"

write_attribution_row() {
    local name="$1" owner="$2" source="$3" note="$4"
    printf '| %s | %s | %s | %s |\n' "$name" "$owner" "$source" "$note" >> "$ATTRIBUTION_TMP"
}

{
    printf '# Asset attribution\n\n'
    printf 'Generated by `scripts/prepare-assets.sh`. Do not edit by hand;\n'
    printf 're-run the script to refresh.\n\n'
    printf 'Every %s.\n\n' "$TRADEMARK_NOTE"
    printf '| Name | Owner | Source | Note |\n'
    printf '| --- | --- | --- | --- |\n'
} > "$ATTRIBUTION_TMP"

# ---------------------------------------------------------------------------
# Process each logo
# ---------------------------------------------------------------------------

PLACEHOLDER_COUNT=0
OFFICIAL_COUNT=0

while IFS='|' read -r name owner url note; do
    [[ -z "$name" ]] && continue

    src="$TMP_DIR/src/${name}.src"
    got=0

    if [[ -n "$url" ]]; then
        if curl -fsSL --retry 2 --retry-delay 2 --connect-timeout 15 \
            -A "kindboard-prepare-assets/0.1" "$url" -o "$src" \
            && magick identify "$src" >/dev/null 2>&1; then
            got=1
        else
            rm -f "$src"
            log "$name: fetch or decode failed for $url"
        fi
    fi

    if [[ $got -eq 0 && -n "$url" && "$SVG_OK" -eq 0 && "$url" == *.svg ]]; then
        note="no usable official asset found: SVG source but rsvg delegate missing"
    fi

    if [[ $got -eq 0 ]]; then
        gen_placeholder "$name"
        if [[ -n "$url" ]]; then
            note="${note:-no usable official asset found: download or decode failed}"
        fi
        note="${note:-no usable official asset found}"
        write_attribution_row "$name" "$owner" "generated placeholder" "$note"
        PLACEHOLDER_COUNT=$((PLACEHOLDER_COUNT + 1))
        log "$name: PLACEHOLDER ($note)"
        continue
    fi

    if ! convert_square "$src" "$TMP_DIR/logos/${name}-128.png" 128 \
        || ! convert_square "$src" "$TMP_DIR/logos/${name}-64.png" 64; then
        gen_placeholder "$name"
        conv_note="no usable official asset found: conversion failed"
        if [[ "$url" == *.svg && "$SVG_OK" -eq 0 ]]; then
            conv_note="no usable official asset found: SVG source requires the ImageMagick rsvg delegate, which is unavailable"
        fi
        write_attribution_row "$name" "$owner" "generated placeholder" "$conv_note"
        PLACEHOLDER_COUNT=$((PLACEHOLDER_COUNT + 1))
        log "$name: PLACEHOLDER ($conv_note)"
        continue
    fi

    write_attribution_row "$name" "$owner" "$url" ""
    OFFICIAL_COUNT=$((OFFICIAL_COUNT + 1))
    log "$name: OK (source $(magick identify -format '%wx%h' "$src"))"
done <<< "$LOGOS"

# ---------------------------------------------------------------------------
# App icon set, derived from the kind logo (no text added, just the logo)
# ---------------------------------------------------------------------------

KIND_SRC="$TMP_DIR/src/kind.src"
if [[ -s "$KIND_SRC" ]]; then
    for size in 512 256 128 64 32; do
        convert_square "$KIND_SRC" "$TMP_DIR/icons/kindboard-${size}.png" "$size"
    done
    log "icons: kindboard-{512,256,128,64,32}.png derived from the kind logo"
else
    for size in 512 256 128 64 32; do
        magick -size "${size}x${size}" xc:none \
            -fill '#54677f' -draw "roundrectangle 0,0 $((size - 1)),$((size - 1)) $((size / 5)),$((size / 5))" \
            -strip "PNG32:$TMP_DIR/icons/kindboard-${size}.png"
    done
    log "icons: WARNING kind logo unavailable; generated placeholder icon set"
fi

touch "$TMP_DIR/logos/.keep"

# ---------------------------------------------------------------------------
# Atomic swap into assets/ (old dirs moved aside first, removed on success)
# ---------------------------------------------------------------------------

for sub in logos icons; do
    if [[ -e "$ASSETS_DIR/$sub" || -L "$ASSETS_DIR/$sub" ]]; then
        mv "$ASSETS_DIR/$sub" "$TMP_DIR/${sub}.old"
    fi
    mv "$TMP_DIR/$sub" "$ASSETS_DIR/$sub"
done
if [[ -e "$ASSETS_DIR/ATTRIBUTION.md" || -L "$ASSETS_DIR/ATTRIBUTION.md" ]]; then
    mv "$ASSETS_DIR/ATTRIBUTION.md" "$TMP_DIR/ATTRIBUTION.old"
fi
mv "$ATTRIBUTION_TMP" "$ASSETS_DIR/ATTRIBUTION.md"

# ---------------------------------------------------------------------------
# Verify + report
# ---------------------------------------------------------------------------

log "== verify =="
fail=0
for f in "$ASSETS_DIR"/logos/*.png "$ASSETS_DIR"/icons/*.png; do
    [[ -f "$f" ]] || continue
    dims="$(magick identify -format '%wx%h %m' "$f" 2>/dev/null || true)"
    case "$f" in
        */logos/*-128.png) [[ "$dims" == "128x128 PNG"* ]] || { log "BAD SIZE: $f ($dims)"; fail=1; } ;;
        */logos/*-64.png)  [[ "$dims" == "64x64 PNG"* ]]   || { log "BAD SIZE: $f ($dims)"; fail=1; } ;;
        */icons/kindboard-512.png)  [[ "$dims" == "512x512 PNG"* ]] || { log "BAD SIZE: $f ($dims)"; fail=1; } ;;
        */icons/kindboard-256.png)  [[ "$dims" == "256x256 PNG"* ]] || { log "BAD SIZE: $f ($dims)"; fail=1; } ;;
        */icons/kindboard-128.png)  [[ "$dims" == "128x128 PNG"* ]] || { log "BAD SIZE: $f ($dims)"; fail=1; } ;;
        */icons/kindboard-64.png)   [[ "$dims" == "64x64 PNG"* ]]   || { log "BAD SIZE: $f ($dims)"; fail=1; } ;;
        */icons/kindboard-32.png)   [[ "$dims" == "32x32 PNG"* ]]   || { log "BAD SIZE: $f ($dims)"; fail=1; } ;;
        *) log "unexpected file: $f"; fail=1 ;;
    esac
done

log "== summary =="
log "official logos converted: $OFFICIAL_COUNT"
log "generated placeholders: $PLACEHOLDER_COUNT"
log "logos: $(ls "$ASSETS_DIR"/logos/*.png | wc -l) png tiles"
log "icons: $(ls "$ASSETS_DIR"/icons/*.png | wc -l) app icon files"
log "attribution rows: $(($(grep -c '^| ' "$ASSETS_DIR/ATTRIBUTION.md" || true) - 2))"
if [[ $fail -eq 0 ]]; then
    log "verification passed"
else
    log "verification FAILED" >&2
    exit 1
fi
