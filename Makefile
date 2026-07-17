# DS Code — Makefile
#
# Quick commands for building, testing, and development.

.PHONY: help build test lint fmt clean check all count-rs dev-tui dev-desktop dev-desktop-setup docs

# ── Default target ────────────────────────────────────────────
help:
	@echo "DS Code — Build Commands"
	@echo ""
	@echo "  make build            Build all crates (release)"
	@echo "  make build-debug      Build all crates (debug)"
	@echo "  make check            Check all crates compile"
	@echo "  make test             Run all tests"
	@echo "  make test-core        Run core engine tests only"
	@echo "  make lint             Run clippy + fmt checks"
	@echo "  make fmt              Format all code"
	@echo "  make clean            Clean build artifacts"
	@echo "  make dev-tui          Run TUI in development"
	@echo "  make dev-desktop      Run Desktop GUI in development"
	@echo "  make dev-desktop-setup Install desktop UI dependencies"
	@echo "  make docs             Generate rustdoc documentation"
	@echo "  make count-rs         Count Rust source files"
	@echo "  make all              Build + lint + test"

# ── Build ─────────────────────────────────────────────────────
build:
	cargo build --workspace --release

build-debug:
	cargo build --workspace

check:
	cargo check --workspace --all-targets

# ── Test ──────────────────────────────────────────────────────
test:
	cargo test --workspace

test-core:
	cargo test -p dscode-core

test-core-nocapture:
	cargo test -p dscode-core -- --nocapture

# ── Lint & Format ─────────────────────────────────────────────
lint:
	cargo clippy --workspace --all-targets -- -D warnings
	cargo fmt --all -- --check

fmt:
	cargo fmt --all

# ── Clean ─────────────────────────────────────────────────────
clean:
	cargo clean

# ── Development ───────────────────────────────────────────────
dev-tui:
	cargo run -p dscode-tui

dev-cli:
	cargo run -p dscode-cli

dev-desktop-setup:
	cd crates/dscode-desktop/ui && npm install

dev-desktop:
	cd crates/dscode-desktop && cargo tauri dev

# ── Documentation ─────────────────────────────────────────────
docs:
	cargo doc --workspace --no-deps --open

# ── Utilities ─────────────────────────────────────────────────
count-rs:
	find crates -name '*.rs' -type f | wc -l > rs_count.txt
	@cat rs_count.txt

# ── CI Pipeline (local) ───────────────────────────────────────
all: build-debug test lint
	@echo "✅ All checks passed!"
