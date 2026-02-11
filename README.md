# Tailnet Microservices

> [!CAUTION]
> **Tailnet Microservices is a research project. If your name is not basher83 then do not use.**
>
> This software is experimental, unstable, and under active development. APIs will change without notice. Features may be incomplete or broken. There is no support, no documentation guarantees, and no warranty of any kind. Use at your own risk.

Single-binary Rust services that act as infrastructure proxies on a Tailscale tailnet. Tailnet exposure is handled by the Tailscale Operator via Kubernetes Service annotations. Each service includes Prometheus metrics and structured JSON logging.

## Services

`anthropic-oauth-proxy` is an OAuth 2.0 gateway that manages Claude Max subscription credentials and proxies authenticated requests to `https://api.anthropic.com`. It handles PKCE authentication, automatic token refresh, round-robin subscription pooling with quota failover, and the full Anthropic header contract. Clients on the tailnet send unauthenticated requests; the gateway handles everything. Runs as a single-container Kubernetes pod with credentials persisted on a PVC.

## Quick Start

```bash
git clone https://github.com/basher83/tailnet-microservices.git
cd tailnet-microservices
cargo build --workspace
cargo test --workspace
```

## Project Structure

```text
crates/
  common/           # Shared types: error types
  provider/         # Provider trait, ErrorClassification
  anthropic-auth/   # OAuth PKCE, token exchange/refresh, credential storage
  anthropic-pool/   # Subscription pool: round-robin, quota detection, cooldown
services/
  oauth-proxy/      # Anthropic OAuth gateway proxy
specs/
  *.md              # Service specifications
k8s/                # Kubernetes deployment manifests
```

## Configuration

Copy `anthropic-oauth-proxy.example.toml` to configure the proxy. See `specs/oauth-proxy.md` for the full configuration reference and `RUNBOOK.md` for operational guidance.

## Deployment

Kubernetes manifests live in `k8s/`. Apply with `kubectl apply -k k8s/`. No secrets required. See `RUNBOOK.md` for the complete deployment procedure.

## License

MIT
