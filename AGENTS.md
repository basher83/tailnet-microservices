# Tailnet Microservices — Agent Guidelines

## Overview

Single-binary Rust services that embed Tailscale connectivity. The binary IS a tailnet node.

## Build & Test

```bash
# Build all
cargo build --workspace

# Build release
cargo build --workspace --release

# Test all
cargo test --workspace

# Test specific crate
cargo test -p common
cargo test -p oauth-proxy

# Lint
cargo clippy --workspace -- -D warnings

# Format
cargo fmt --all

# Check (format + lint + build + test)
cargo fmt --all --check && cargo clippy --workspace -- -D warnings && cargo build --workspace && cargo test --workspace

# Cross-compile for Linux (requires cargo-zigbuild + zig)
cargo zigbuild --workspace --release --target x86_64-unknown-linux-gnu
cargo zigbuild --workspace --release --target aarch64-unknown-linux-gnu
```

## Project Structure

```
crates/
  common/           # Shared types: Config, Secret<T>, error types
services/
  oauth-proxy/      # Anthropic OAuth header injection proxy
specs/
  *.md              # One spec per service/component
```

## Code Patterns

### Secret Wrapper
Use `common::Secret<T>` for sensitive values. Auto-redacts in Debug/Display/logs.

### Error Handling
- Define errors with `thiserror` in each crate
- Use `anyhow` for propagation in binaries
- Define `type Result<T> = std::result::Result<T, Error>` per crate

### Async
- Tokio runtime
- Rust 2024 edition supports native async traits (no `async-trait` crate needed)

### Logging
- Use `tracing` with structured fields
- Always skip secrets: `#[instrument(skip(secret, config.auth))]`

## Specs

Before implementing any feature, consult `specs/`. Specs describe intent; code describes reality.

- **Assume NOT implemented** — many specs describe planned features
- **Check code first** — search before concluding something exists/doesn't exist
- **Use specs as guidance** — follow types, states, transitions defined there

## Live Infrastructure Status

**2026-02-06 11:30 EST**: Cluster access restored. Talos Omni back online, kubectl authenticated, ArgoCD healthy. Remaining work items requiring live tailnet are now deployable. Suggest verifying deployment with `kubectl apply -k k8s/` when ready.
