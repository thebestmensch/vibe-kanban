# vibe-kanban fork — one-word commands for the local board.
# `just` lists these; every recipe wraps an existing package.json script.

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
