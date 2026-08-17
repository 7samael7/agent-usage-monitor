# agent-usage-monitor — building and running it.
#
#   make install   put `aum` on your PATH
#   make run       run it from source
#
# Everything else is a step of one of those, exposed so a failure can be retried
# where it happened rather than from the top.

SHELL := /bin/bash
.DEFAULT_GOAL := help

BIN := aum

.PHONY: help
help: ## Show this help
	@echo "agent-usage-monitor"
	@echo
	@grep -hE '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[1m%-12s\033[0m %s\n", $$1, $$2}'
	@echo
	@echo "Everything it records lives in:"
	@echo "  ~/Library/Application Support/agent-usage-monitor/"

.PHONY: install
install: ## Build and install `aum` to ~/.cargo/bin
	cargo install --path crates/aum-tui --locked
	@echo "==> $$(command -v $(BIN) || echo 'not on PATH — add ~/.cargo/bin')"

.PHONY: run
run: ## Run from source
	cargo run -p aum-tui

.PHONY: build
build: ## Build everything
	cargo build

.PHONY: test
test: ## Run every test
	cargo test

.PHONY: check
check: ## Everything CI would run: format, lint, test
	cargo fmt --all --check
	cargo clippy --all-targets -- -D warnings
	cargo test

.PHONY: fmt
fmt: ## Format
	cargo fmt --all

.PHONY: redact
redact: ## Make a fixture from a real transcript: make redact IN=... OUT=...
	@test -n "$(IN)" -a -n "$(OUT)" \
		|| { echo "usage: make redact IN=<transcript.jsonl> OUT=tests/fixtures/<name>.jsonl"; exit 2; }
	cargo xtask redact "$(IN)" "$(OUT)"

.PHONY: doctor
doctor: ## Check that the tools this needs are present
	@printf 'cargo       '; cargo --version 2>/dev/null || echo "MISSING — https://rustup.rs"
	@printf 'toolchain   '; rustc --version 2>/dev/null || echo "MISSING"
	@printf 'claude      '; command -v claude || echo "not found (Claude Code transcripts will be absent)"
	@printf 'codex       '; test -x "/Applications/ChatGPT.app/Contents/Resources/codex" \
		&& echo "/Applications/ChatGPT.app/Contents/Resources/codex" \
		|| command -v codex || echo "not found (ships inside ChatGPT.app)"
	@printf 'installed   '; command -v $(BIN) || echo "not installed — run: make install"

.PHONY: clean
clean: ## Remove build output (keeps your recorded usage)
	cargo clean
	@echo "==> your data in ~/Library/Application Support/agent-usage-monitor/ was not touched"

.PHONY: uninstall
uninstall: ## Remove `aum` (keeps your recorded usage)
	cargo uninstall aum-tui 2>/dev/null || true
	@echo "==> your data in ~/Library/Application Support/agent-usage-monitor/ was not touched"
