# Tailnet Microservices — Agent Guidelines

## Overview

Rust HTTP proxy on the tailnet. Currently injects static OAuth headers and forwards to api.anthropic.com. Evolving into a full OAuth 2.0 gateway with PKCE auth, token refresh, and subscription pooling. Multi-provider interface designed in, Anthropic-only for now.

**Do NOT** modify anything in mothership-gitops. ArgoCD syncs this repo via wave 8.

## Build & Test

```bash
cargo build --workspace                # Build all
cargo build --workspace --release      # Release build
cargo test --workspace                 # Test all
cargo test -p common                   # Test specific crate
cargo clippy --workspace -- -D warnings  # Lint
cargo fmt --all                        # Format

# Full check (format + lint + build + test)
cargo fmt --all --check && cargo clippy --workspace -- -D warnings && cargo build --workspace && cargo test --workspace

# Cross-compile (requires cargo-zigbuild + zig)
cargo zigbuild --workspace --release --target x86_64-unknown-linux-gnu
cargo zigbuild --workspace --release --target aarch64-unknown-linux-gnu
```

## Project Structure

```
crates/
  common/           # Shared types: error types
  provider/         # Provider trait, ErrorClassification, PassthroughProvider
  anthropic-auth/   # OAuth PKCE, token exchange/refresh, credential storage
  anthropic-pool/   # Subscription pool: round-robin, quota detection, cooldown, refresh
services/
  oauth-proxy/      # Anthropic OAuth header injection proxy
specs/
  *.md              # One spec per service/component
```

Dependencies: hyper, axum, reqwest, tokio, thiserror/anyhow, tracing, serde/serde_json
