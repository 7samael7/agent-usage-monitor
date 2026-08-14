# Agent Usage Monitor — building and running it on this Mac.
#
# The short version:
#
#   make desktop    build it, install it to /Applications, open it
#   make run        run it from source with hot reload, for development
#
# Everything else is a step of one of those, exposed so a failure can be
# retried on its own rather than from the top.

SHELL := /bin/bash
.DEFAULT_GOAL := help

APP_NAME   := Agent Usage Monitor
ARCH       := $(shell uname -m)

# electron-builder.yml pins the sidecar it packages to the arm64 path, so this
# is deliberately not generic: an Intel build would produce an application whose
# backend binary is missing, and it would fail at launch rather than at build.
ifneq ($(ARCH),arm64)
$(error This Makefile builds for Apple Silicon. On $(ARCH), update the extraResources path in apps/desktop/electron-builder.yml first)
endif

RUST_TARGET   := aarch64-apple-darwin
SIDECAR_DEBUG := target/debug/aum-sidecar
SIDECAR_REL   := target/$(RUST_TARGET)/release/aum-sidecar
APP_BUNDLE    := release/mac-arm64/$(APP_NAME).app
INSTALLED     := /Applications/$(APP_NAME).app

.PHONY: help
help: ## Show this help
	@echo "Agent Usage Monitor"
	@echo
	@grep -hE '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[1m%-16s\033[0m %s\n", $$1, $$2}'
	@echo
	@echo "The application stores everything in:"
	@echo "  ~/Library/Application Support/agent-usage-monitor/"

# ── The two things you actually want ────────────────────────────────────────

.PHONY: desktop
desktop: install ## Build, install to /Applications, and open it
	@open -a "$(INSTALLED)"
	@echo "==> $(APP_NAME) is running. It is also in Launchpad and Spotlight now."

.PHONY: run
run: deps $(SIDECAR_DEBUG) ## Run from source with hot reload (development)
	@$(MAKE) --no-print-directory check-installed-not-running
	@$(MAKE) --no-print-directory check-dev-port
	bun run dev

# ── Building ────────────────────────────────────────────────────────────────

.PHONY: deps
deps: node_modules ## Install JavaScript dependencies

node_modules: package.json bun.lock
	bun install
	@touch node_modules

# Cargo does its own change detection and is fast when nothing moved, so this
# is phony rather than a file rule over every .rs in the workspace.
.PHONY: sidecar
sidecar: ## Build the Rust backend (debug, for `make run`)
	cargo build -p aum-sidecar

$(SIDECAR_DEBUG):
	@$(MAKE) --no-print-directory sidecar

.PHONY: sidecar-release
sidecar-release: ## Build the Rust backend (release, for the packaged app)
	cargo build --release --target $(RUST_TARGET) -p aum-sidecar

.PHONY: frontend
frontend: deps ## Typecheck and bundle the desktop app
	bun run build

.PHONY: app
app: frontend sidecar-release ## Build the .app bundle (ad-hoc signed, this machine only)
	@echo "==> packaging"
	@cd apps/desktop && bunx electron-builder --mac --dir --config electron-builder.yml
	@$(MAKE) --no-print-directory sign
	@echo "==> built $(APP_BUNDLE)"

# electron-builder skips signing entirely when `identity` is null, which leaves
# the bundle carrying Electron's own linker signature over contents that have
# since changed — `spctl` reports "code has no resources but signature indicates
# they must be present". It launches on this machine anyway, but a bundle whose
# seal does not match its contents is one Gatekeeper setting or one copy away
# from being refused, and the failure would arrive much later than the cause.
#
# An ad-hoc signature costs a second and needs no Developer ID. The nested
# binary under Resources is signed first: signing the outer bundle seals what is
# inside it, so doing it the other way round invalidates the outer signature
# immediately.
.PHONY: sign
sign: ## Ad-hoc sign the built bundle so its seal matches its contents
	@codesign --force --sign - --timestamp=none \
		"$(APP_BUNDLE)/Contents/Resources/sidecar/aum-sidecar"
	@codesign --force --deep --sign - --timestamp=none "$(APP_BUNDLE)"
	@codesign --verify --deep --strict "$(APP_BUNDLE)" \
		&& echo "==> signature verified (ad-hoc, valid on this Mac only)"

.PHONY: install
install: app ## Build and copy the app to /Applications
	@if pgrep -f "$(INSTALLED)" >/dev/null 2>&1; then \
		echo "==> $(APP_NAME) is running; quitting it so it can be replaced"; \
		osascript -e 'quit app "$(APP_NAME)"' >/dev/null 2>&1 || true; \
		sleep 2; \
	fi
	@rm -rf "$(INSTALLED)"
	@cp -R "$(APP_BUNDLE)" /Applications/
	@echo "==> installed $(INSTALLED)"

.PHONY: dmg
dmg: frontend sidecar-release ## Build a .dmg and .zip for copying to another Mac
	@cd apps/desktop && bunx electron-builder --mac --config electron-builder.yml
	@ls -1 release/*.dmg release/*.zip 2>/dev/null || true

# ── Running what is installed ───────────────────────────────────────────────

.PHONY: open
open: ## Open the installed app
	@test -d "$(INSTALLED)" || { echo "Not installed. Run: make desktop"; exit 1; }
	@open -a "$(INSTALLED)"

.PHONY: logs
logs: ## Follow the installed app's backend log
	@echo "The sidecar logs to the app's stderr. Launch from a terminal to see it:"
	@echo "  \"$(INSTALLED)/Contents/MacOS/$(APP_NAME)\""

# ── Checks ──────────────────────────────────────────────────────────────────

.PHONY: test
test: ## Run every test, backend and frontend
	cargo test
	bun run --cwd apps/desktop test

.PHONY: check
check: ## Everything CI would run: format, lint, typecheck, test, build
	cargo fmt --all --check
	cargo clippy --all-targets -- -D warnings
	cargo test
	bunx biome check .
	bun run typecheck
	bun run --cwd apps/desktop test
	bun run build

.PHONY: fmt
fmt: ## Format Rust and TypeScript
	cargo fmt --all
	bunx biome check --write .

.PHONY: doctor
doctor: ## Check that the tools this needs are present
	@echo "arch          $(ARCH)"
	@printf 'bun           '; bun --version 2>/dev/null || echo "MISSING — https://bun.sh"
	@printf 'cargo         '; cargo --version 2>/dev/null || echo "MISSING — https://rustup.rs"
	@printf 'rust target   '; rustup target list --installed 2>/dev/null | grep -q '$(RUST_TARGET)' \
		&& echo "$(RUST_TARGET)" \
		|| echo "MISSING — rustup target add $(RUST_TARGET)"
	@printf 'claude        '; command -v claude || echo "not found (only needed to launch Claude Code tasks)"
	@printf 'codex         '; test -x "/Applications/ChatGPT.app/Contents/Resources/codex" \
		&& echo "/Applications/ChatGPT.app/Contents/Resources/codex" \
		|| echo "not found (ships inside ChatGPT.app)"
	@printf 'installed     '; test -d "$(INSTALLED)" && echo "$(INSTALLED)" || echo "not installed"

# The installed app and a development run share one database file. SQLite copes,
# and ingest is idempotent so the totals converge either way — but two ingest
# loops reading the same transcripts is wasted work, and a puzzling one to
# diagnose later.
.PHONY: check-installed-not-running
check-installed-not-running:
	@if pgrep -f "$(INSTALLED)/Contents/MacOS" >/dev/null 2>&1; then \
		echo "The installed $(APP_NAME) is running, and shares this database."; \
		echo "Quit it first:  osascript -e 'quit app \"$(APP_NAME)\"'"; \
		exit 1; \
	fi

# `bun run dev` fails with a bare "Port 5273 is already in use" that does not
# say what is holding it. Usually it is a previous run of this same project.
.PHONY: check-dev-port
check-dev-port:
	@pid=$$(lsof -nP -iTCP:5273 -sTCP:LISTEN -t 2>/dev/null | head -1); \
	if [ -n "$$pid" ]; then \
		echo "Port 5273 is held by PID $$pid:"; \
		ps -o command= -p $$pid | cut -c1-100 | sed 's/^/    /'; \
		echo "Stop it with:  kill $$pid"; \
		exit 1; \
	fi

# ── Cleaning ────────────────────────────────────────────────────────────────

.PHONY: clean
clean: ## Remove build output (keeps your recorded usage)
	rm -rf release apps/desktop/out
	cargo clean
	@echo "==> your data in ~/Library/Application Support/agent-usage-monitor/ was not touched"

.PHONY: uninstall
uninstall: ## Remove the app from /Applications (keeps your recorded usage)
	@rm -rf "$(INSTALLED)"
	@echo "==> removed $(INSTALLED)"
	@echo "==> your data in ~/Library/Application Support/agent-usage-monitor/ was not touched"
