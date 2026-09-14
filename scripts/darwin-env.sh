# kindboard darwin (macOS) cross-build toolchain pins — single source of truth.
#
# Sourced (never executed) by scripts/build.sh and scripts/bootstrap-darwin.sh
# so the osxcross/SDK pins can never drift between bootstrap and builder.
# Every variable defaults only when unset, so callers can override anything:
#
#   OSXCROSS_DIR         ${KINDBOARD_OSXCROSS_DIR:-${OSXCROSS_DIR:-$HOME/.local/opt/osxcross}}
#   OSXCROSS_SRC_DIR     ${KINDBOARD_OSXCROSS_SRC:-$HOME/.local/src/osxcross}
#   MACOSX_SDK_CACHE_DIR ${KINDBOARD_MACOSX_SDK_CACHE_DIR:-$HOME/.local/share/kindboard/sdk}
#
# Precedence: KINDBOARD_OSXCROSS_DIR > OSXCROSS_DIR > default;
# KINDBOARD_MACOSX_SDK_CACHE_DIR > default; KINDBOARD_OSXCROSS_SRC > default.
#
# The Apple SDK tarball is a community redistribution (github.com/joseluisq/
# macosx-sdks) of Apple's macOS SDK; it is downloaded only as a build-time
# convenience for the osxcross toolchain and is never shipped. See
# docs/adrs/ADR-0017.md for the full design.

# Sourced-only guard: this file must never be executed directly.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    echo "darwin-env.sh: this file must be sourced, not executed" >&2
    return 1 2>/dev/null || exit 1
fi

OSXCROSS_DIR="${KINDBOARD_OSXCROSS_DIR:-${OSXCROSS_DIR:-$HOME/.local/opt/osxcross}}"
OSXCROSS_SRC_DIR="${KINDBOARD_OSXCROSS_SRC:-$HOME/.local/src/osxcross}"
# Proven-good osxcross commit: the exact revision this repo's darwin toolchain
# (~/.local/opt/osxcross) was built from and verified against (2026-09-14).
# bootstrap-darwin.sh clones and checks out this commit so the toolchain
# source is as pinned as the SDK digest (osxcross publishes no release tags).
OSXCROSS_PIN="27d21e4977c9751d01199c7a226a6faf494c3dd9"

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
