# Running kindboard on macOS

Four ways to run kindboard on a Mac, in order of least friction:

| Path | Command | Gatekeeper | Needs |
|---|---|---|---|
| Release installer | `make mac-run` / `make mac-install` | **Zero prompts** | nothing beyond a Mac — macOS ships curl, xattr and shasum |
| Local debug build | `make run` | **Zero prompts** | Xcode CLT + rustc ≥ 1.98 — both auto-guided by `make darwin-bootstrap` (runs first on macOS) |
| Double-clickable app | `make mac-app` | **Zero prompts** | Same as above; builds natively + ad-hoc signs a bundle at `dist/kindboard.app` |
| Downloaded release | extract tarball, run `./kindboard` | One-time right-click → Open (ad-hoc signed) | nothing beyond a Mac |

## The release installer (zero-toolchain)

`make mac-run` / `make mac-install`
(`scripts/install-macos.sh`) fetches the **prebuilt** ad-hoc-signed darwin
binary from GitHub releases (latest, or `KINDBOARD_VERSION=vX.Y.Z` to pin),
**verifies its sha256** against the release `SHA256SUMS`, installs it to
`~/.local/bin/kindboard`, and **clears the `com.apple.quarantine` xattr at
install time** — Gatekeeper only evaluates files carrying that flag, so the
first launch runs **silently, for any user**: no Apple account, no
certificate, no notarization, no Xcode tools, no sudo. Repeat runs are
idempotent (an already-installed binary with the right version is skipped),
and `--run` execs the app in the foreground.

This is the zero-prompt path for the downloaded binary. Honest limits:
building **from source** (`make run`, `make mac-app`) still needs Xcode CLT —
no toolchain, no source build; and Developer ID **notarization** (below) is
only for distributing to *other* machines that download the binary directly
from the browser — the installer path clears quarantine itself, so it needs
no Apple identity at all.

## Why local builds have zero prompts

Gatekeeper only prompts for binaries carrying the `com.apple.quarantine`
xattr — macOS stamps it on **downloaded** files. Binaries built locally
(`cargo build`, `make run`, `make mac-app`) never get the xattr, so they run
with no prompts at all. `make run` and `make mac-app` are therefore the
friction-free path for developers.

## Downloaded release binaries

> Prefer `make mac-run` / `make mac-install` — it downloads the same tarball,
> verifies it, and clears the quarantine flag for you. This section is for
> the raw browser-download path.

Release tarballs are ad-hoc signed (ADR-0018): Gatekeeper shows
"unidentified developer" and you bypass it **once** via right-click → Open
(or `xattr -d com.apple.quarantine ./kindboard`).

The **permanent zero-prompt fix** for downloaded binaries is a **Developer ID
signature + notarization + stapling**. One-time setup:

1. Join the **Apple Developer Program** (paid; developer.apple.com).
2. Create a **Developer ID Application** certificate, then export the
   certificate + private key as a **PEM** (or a P12/PFX with a password).
3. Create an **App Store Connect API key**
   (appstoreconnect.apple.com/access/api); download the `.p8` file and note
   the Issuer ID + Key ID.
4. Encode the API key into one JSON file:
   `rcodesign encode-app-store-connect-api-key <issuer-id> <key-id> AuthKey_<id>.p8 --output-path ~/.config/kindboard/apple-api-key.json`
5. Export the credentials in the shell and release:
   ```bash
   export KINDBOARD_APPLE_CERT_PEM="$HOME/.config/kindboard/devid.pem"     # or KINDBOARD_APPLE_CERT_P12 + KINDBOARD_APPLE_CERT_P12_PASSWORD
   export KINDBOARD_APPLE_API_KEY_PATH="$HOME/.config/kindboard/apple-api-key.json"
   KINDBOARD_REQUIRE_DARWIN=1 make release
   ```
   With both set, `scripts/build.sh` signs each darwin binary with the
   Developer ID, produces a deterministic ZIP (`dist/kindboard-darwin-*.zip`;
   Apple notarizes archives, not raw binaries), and
   `rcodesign notary-submit --staple --wait` notarizes + staples it — the
   release ships the notarized zips alongside the tarballs. Only one of the
   two inputs set → that darwin target fails (both are required together).

## Credentials are never committed

The certificate/key PEM (or P12 + password) and the API-key JSON contain
private key material: keep them **outside the repo** (e.g. `~/.config/`),
export them only in the shell that runs the release, and never add them to
git. `scripts/build.sh` reads them only via environment variables.
