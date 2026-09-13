# kindboard — build & quality pipeline
#
# Thin ergonomic wrapper over cargo and scripts/. The single source of truth
# for release artifacts is scripts/build.sh (dist/ + SHA256SUMS); the Makefile
# only exposes convenient entry points.
#
# No GitHub Actions by design: releases and the GitHub Pages deploy both run
# from here (see `make release` and `make publish`).

SHELL := /bin/bash
CARGO ?= cargo
CARGO_AUDIT ?= $(HOME)/.cargo/bin/cargo-audit
DIST_DIR := dist
ROOT := $(abspath .)
REPO ?= Orpere/kindboard
VERSION := $(shell grep -m1 '^version' crates/kindboard-app/Cargo.toml | cut -d'"' -f2)
PAGES_DIR := /tmp/kindboard-pages

.PHONY: help all build build-all run fmt fmt-check clippy test e2e audit check assets dist clean version release publish

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  %-12s %s\n", $$1, $$2}'

all: check build ## Quality gates + host release build into dist/

build: ## Host release build (fmt+clippy+test+audit gates, then dist/ + SHA256SUMS)
	./scripts/build.sh

build-all: ## Attempt all 4 targets: linux x86_64/aarch64, darwin x86_64/arm64 (skips unbuildable)
	./scripts/build.sh --all

run: ## Run the desktop app (debug build)
	cargo run -p kindboard-app

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

release: ## Tag v$(VERSION) + GitHub release with dist/ assets (local, no CI)
	@command -v gh >/dev/null 2>&1 || { echo "error: gh CLI required"; exit 1; }
	@git diff-index --quiet HEAD -- || { echo "error: working tree not clean — commit first"; exit 1; }
	@git rev-parse -q --verify "refs/tags/v$(VERSION)" >/dev/null && { echo "error: tag v$(VERSION) already exists"; exit 1; } || true
	$(MAKE) build
	git tag -a "v$(VERSION)" -m "kindboard v$(VERSION)"
	git push origin "v$(VERSION)"
	gh release create "v$(VERSION)" --title "kindboard v$(VERSION)" --generate-notes \
		$$(find $(DIST_DIR) -maxdepth 1 -name 'kindboard-*.tar.gz' | sort) $(DIST_DIR)/SHA256SUMS

publish: ## Deploy web/ to GitHub Pages via the gh-pages branch (no Actions)
	@command -v gh >/dev/null 2>&1 || { echo "error: gh CLI required"; exit 1; }
	@rm -rf $(PAGES_DIR) && mkdir -p $(PAGES_DIR)
	@cp -R web/* $(PAGES_DIR)/
	@cd $(PAGES_DIR) && git init -q && git add -A && \
		git -c user.name="kindboard" -c user.email="kindboard@users.noreply.github.com" commit -qm "publish $(VERSION)"
	@cd $(PAGES_DIR) && git push -q -f "$$(git -C $(ROOT) remote get-url origin)" HEAD:gh-pages
	@gh api -X PUT repos/$(REPO)/pages -f source[branch]=gh-pages -f source[path]=/ >/dev/null
	@echo "published -> https://orpere.github.io/kindboard/"
