# vibe-kanban fork — one-word commands for the local board.
# `just` lists these; recipes wrap package.json scripts + local tooling.

# Tailscale binary (macOS app bundle — not on PATH by default)
tailscale := "/Applications/Tailscale.app/Contents/MacOS/Tailscale"
# MagicDNS name the board is reachable at (rename via Tailscale admin console)
host := "mac"
# Local port the prod server binds; tailscale serve proxies tailnet :80 -> here
port := "8080"

# List available recipes
default:
    @just --list

# Install JS deps (no-op when the pnpm lockfile is already satisfied)
_deps:
    pnpm i

# Boot the local board end-to-end: install deps, then run the dev stack (backend + web)
start: _deps
    pnpm run dev

# Build the native macOS desktop app (.app bundle via Tauri)
build: _deps
    pnpm run tauri:build

# Verify the local board (mirrors `pnpm run check`, minus the remote-web + crates/remote steps)
check: _deps
    cargo check --workspace
    cargo test --workspace
    cargo clippy --workspace --all-targets
    pnpm run prepare-db:check
    pnpm run local-web:legacy-path-guard
    pnpm run web-core:check
    pnpm run local-web:check
    pnpm run ui:check
    pnpm run local-web:lint
    pnpm run ui:lint

# Format the local board (Rust workspace + web packages, minus remote-web + crates/remote)
fmt: _deps
    cargo fmt --all
    pnpm run web-core:format
    pnpm run local-web:format

# Build the browser-mode prod bundle (single server + web UI) into npx-cli/
remote-build: _deps
    pnpm run build:npx

# Auto-builds the bundle on first run; use `remote-build` to rebuild after changes.
# Serve the board on your tailnet at http://{{host}} — no port, any tailnet device.
remote:
    @[ -f npx-cli/bin/cli.js ] || just remote-build
    {{tailscale}} serve --bg --http 80 {{port}}
    cd npx-cli && HOST=127.0.0.1 PORT={{port}} VK_ALLOWED_ORIGINS="http://{{host}},http://{{host}}.tailaa1238.ts.net" node bin/cli.js

# Stop serving on your tailnet (leaves the app process untouched)
remote-off:
    {{tailscale}} serve --http=80 off
