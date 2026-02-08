# Tailnet Microservices — Agent Guidelines

## Current Work

**Ingress manifests complete (v0.0.113).** Tailscale Ingress routes tailnet HTTP traffic to the proxy. Remaining: cluster deployment verification (Ingress resolution, health from tailnet, upstream proxy). See `specs/operator-migration-addendum.md`.

**Do NOT** modify anything in mothership-gitops. ArgoCD syncs this repo via wave 8.

---

## Overview

Rust HTTP proxy that injects OAuth headers and forwards to api.anthropic.com. Tailnet exposure is handled by the Tailscale Operator (not the binary).

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
  common/           # Shared types: error types
services/
  oauth-proxy/      # Anthropic OAuth header injection proxy
specs/
  *.md              # One spec per service/component
```

## Code Patterns

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

## Deployment

Service is live in production. GHCR package is public (anonymous pull works). Aperture routes traffic through the proxy. See IMPLEMENTATION_PLAN.md for deployment history and status details.
