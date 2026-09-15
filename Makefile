# kindboard — build & quality pipeline
#
# Thin ergonomic wrapper over cargo and scripts/. The single source of truth
# for release artifacts is scripts/build.sh (dist/ + SHA256SUMS); the Makefile
# only exposes convenient entry points.
#
# Darwin (macOS) assets: on Linux hosts scripts/build.sh cross-builds them via
# osxcross when it is detected (~/.local/opt/osxcross, see ADR-0017), else
# skips them with guidance. `make darwin-bootstrap` (scripts/bootstrap-darwin.sh)
# idempotently resolves every darwin dependency — host packages, rustup targets,
# rcodesign, osxcross + digest-pinned SDK — and runs automatically before
# dist-macos, build-all, and release. KINDBOARD_REQUIRE_DARWIN=1 makes darwin
# mandatory (`make dist-macos`; see also `make release`).
#
# Windows assets: scripts/build.sh cross-builds x86_64-pc-windows-gnu via the
# mingw64 GCC toolchain (dnf install mingw64-gcc, see ADR-0019), producing
# dist/kindboard-windows-x86_64.zip (zip-only, unsigned — SmartScreen/MOTW is
# neutralized by scripts/install-windows.ps1 at install time).
# KINDBOARD_REQUIRE_WINDOWS=1 makes windows mandatory. `make win-install` /
# `make win-run` are the PowerShell prebuilt-installer entry points on Windows.
# `make check-targets` runs the cross-target compile gate (host tests + every
# available windows/darwin cargo check).
#
# Dual-producer release model (ADR-0027): tagged releases are built, signed and
# published by GitHub Actions (.github/workflows/release.yml) — CI is the
# canonical producer. Local `make release` remains for offline and cross builds
# (osxcross darwin, deterministic Linux tarball + Windows zip); both producers
# emit the same artifact contract (see the release.yml header). The GitHub
# Pages deploy still runs only from here (`make publish` — no Actions). On
# macOS, `make mac-app` (scripts/make-mac-app.sh) assembles a double-clickable
# dist/kindboard.app from a locally built binary — zero Gatekeeper prompts (see
# docs/macos-distribution.md). When the notarization env vars are set (see
# scripts/build.sh header), darwin release zips are notarized + stapled and
# `make release` ships them alongside the tarballs.

SHELL := /bin/bash
CARGO ?= cargo
CARGO_AUDIT ?= $(HOME)/.cargo/bin/cargo-audit
KINDBOARD_REQUIRE_DARWIN ?= 0
export KINDBOARD_REQUIRE_DARWIN
KINDBOARD_REQUIRE_WINDOWS ?= 0
export KINDBOARD_REQUIRE_WINDOWS
DIST_DIR := dist
ROOT := $(abspath .)
REPO ?= Orpere/kindboard
VERSION := $(shell grep -m1 '^version' crates/kindboard-app/Cargo.toml | cut -d'"' -f2)
UNAME_S := $(shell uname -s)
PAGES_DIR := /tmp/kindboard-pages

.PHONY: help all build build-all dist-macos darwin-bootstrap run mac-app mac-install mac-run win-install win-run fmt fmt-check clippy test e2e audit check check-targets assets dist clean version release publish

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  %-12s %s\n", $$1, $$2}'

all: check build ## Quality gates + host release build into dist/

build: ## Host release build (fmt+clippy+test+audit gates, then dist/ + SHA256SUMS)
	./scripts/build.sh

build-all: darwin-bootstrap ## Attempt all supported targets: linux x86_64/aarch64, darwin x86_64/arm64, windows x86_64 (deps auto-resolved; darwin builds when osxcross is present, windows when mingw64-gcc is present, else skipped)
	./scripts/build.sh --all

darwin-bootstrap: ## Resolve all darwin cross-build dependencies (host pkgs, rustup targets, rcodesign, osxcross + pinned SDK)
	./scripts/bootstrap-darwin.sh

dist-macos: darwin-bootstrap ## Cross-build darwin release assets (KINDBOARD_REQUIRE_DARWIN=1; deps auto-resolved via darwin-bootstrap)
	KINDBOARD_REQUIRE_DARWIN=1 ./scripts/build.sh aarch64-apple-darwin x86_64-apple-darwin

# On macOS, `make run` needs Xcode CLT + a recent rustc (macOS 26 SDK) — make
# darwin-bootstrap verifies and guides both before the app builds. No Xcode at
# all? `make mac-run` installs the prebuilt signed binary instead (no build).
ifeq ($(UNAME_S),Darwin)
run: darwin-bootstrap
endif
run: ## Run the desktop app (debug build)
	cargo run -p kindboard-app

mac-app: ## Build and open a double-clickable kindboard.app (macOS; locally built, zero Gatekeeper prompts)
	./scripts/make-mac-app.sh

mac-install: ## (macOS) install the prebuilt signed kindboard from GitHub releases — no Xcode, no Gatekeeper prompts, no Apple account
	./scripts/install-macos.sh

mac-run: ## (macOS) install (if needed) and launch kindboard — the zero-toolchain way to run
	./scripts/install-macos.sh --run

win-install: ## (Windows) install the prebuilt kindboard from GitHub releases — no admin, no SmartScreen prompt
	powershell -ExecutionPolicy Bypass -File scripts/install-windows.ps1

win-run: ## (Windows) install (if needed) + launch kindboard
	powershell -ExecutionPolicy Bypass -File scripts/install-windows.ps1 -Run

check-targets: ## Verify the code compiles for ALL supported OS targets (host tests + windows/darwin checks when toolchains present)
	./scripts/check-targets.sh

fmt: ## Format all code
	cargo fmt --all

fmt-check: ## Verify formatting (zero diff)
	cargo fmt --all -- --check

clippy: ## Lint with zero tolerance (-D warnings)
	cargo clippy --workspace --all-targets -- -D warnings

test: ## Unit + integration tests (e2e skipped unless KINDBOARD_E2E=1)
	cargo test --workspace

e2e: ## Full suite including real-kind e2e (requires docker + kind; uses kbtest-* throwaway clusters)
	KINDBOARD_E2E=1 cargo test --workspace

audit: ## RustSec advisory scan (requires cargo-audit)
	@command -v $(CARGO_AUDIT) >/dev/null 2>&1 || { echo "error: cargo-audit not found — run: cargo install cargo-audit"; exit 1; }
	$(CARGO_AUDIT) audit

check: fmt-check clippy test ## All quality gates (syntax + logic + style)

assets: ## Fetch and resize official logos/icons into assets/ (needs ImageMagick + network)
	./scripts/prepare-assets.sh

dist: build ## Alias: produce dist/ release artifacts + SHA256SUMS
	@ls -l $(DIST_DIR)/

clean: ## Remove build artifacts and dist/
	cargo clean
	rm -rf $(DIST_DIR)

version: ## Print the app version (from kindboard-app/Cargo.toml)
	@grep -m1 '^version' crates/kindboard-app/Cargo.toml | cut -d'"' -f2

release: ## Tag v$(VERSION) + GitHub release with dist/ assets (darwin required when KINDBOARD_REQUIRE_DARWIN=1)
	@command -v gh >/dev/null 2>&1 || { echo "error: gh CLI required"; exit 1; }
	@git diff-index --quiet HEAD -- || { echo "error: working tree not clean — commit first"; exit 1; }
	@git rev-parse -q --verify "refs/tags/v$(VERSION)" >/dev/null && { echo "error: tag v$(VERSION) already exists"; exit 1; } || true
	@rm -f $(DIST_DIR)/kindboard-darwin-*.tar.gz
	@rm -rf $(DIST_DIR)/aarch64-apple-darwin $(DIST_DIR)/x86_64-apple-darwin
ifeq ($(KINDBOARD_REQUIRE_DARWIN),1)
	@# bootstrap first, then the explicit knob: build host + both darwin targets in one pass
	$(MAKE) darwin-bootstrap && $(MAKE) build
	@test -n "$$(find $(DIST_DIR) -maxdepth 1 -name 'kindboard-darwin-*.tar.gz' 2>/dev/null)" || { echo "error: darwin builds required but missing — see docs/adrs/ADR-0017.md"; exit 1; }
else
	$(MAKE) build
	-$(MAKE) dist-macos
	@test -n "$$(find $(DIST_DIR) -maxdepth 1 -name 'kindboard-darwin-*.tar.gz' 2>/dev/null)" || { echo "WARNING: release will lack darwin assets (osxcross not detected — see docs/adrs/ADR-0017.md)"; }
endif
	git tag -a "v$(VERSION)" -m "kindboard v$(VERSION)"
	git push origin "v$(VERSION)"
	gh release create "v$(VERSION)" --title "kindboard v$(VERSION)" --generate-notes \
		$$(find $(DIST_DIR) -maxdepth 1 -name 'kindboard-*.tar.gz' | sort) \
		$$(find $(DIST_DIR) -maxdepth 1 -name 'kindboard-*.zip' -type f -print | sort) \
		$(DIST_DIR)/SHA256SUMS

publish: ## Deploy web/ to GitHub Pages via the gh-pages branch (no Actions)
	@command -v gh >/dev/null 2>&1 || { echo "error: gh CLI required"; exit 1; }
	@rm -rf $(PAGES_DIR) && mkdir -p $(PAGES_DIR)
	@cp -R web/* $(PAGES_DIR)/
	@cd $(PAGES_DIR) && git init -q && git add -A && \
		git -c user.name="kindboard" -c user.email="kindboard@users.noreply.github.com" commit -qm "publish $(VERSION)"
	@cd $(PAGES_DIR) && git push -q -f "$$(git -C $(ROOT) remote get-url origin)" HEAD:gh-pages
	@gh api -X PUT repos/$(REPO)/pages -f build_type=legacy -f 'source[branch]=gh-pages' -f 'source[path]=/' >/dev/null
	@echo "published -> https://orpere.github.io/kindboard/"
