# Tailnet Microservices

> [!CAUTION]
> **Tailnet Microservices is a research project. If your name is not basher83 then do not use.**
>
> This software is experimental, unstable, and under active development. APIs will change without notice. Features may be incomplete or broken. There is no support, no documentation guarantees, and no warranty of any kind. Use at your own risk.

Single-binary Rust services that act as infrastructure proxies on a Tailscale tailnet. Tailnet exposure is handled by the Tailscale Operator via Kubernetes Ingress resources; Services remain plain ClusterIP backends. Each service includes Prometheus metrics and structured JSON logging.

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

Copy `anthropic-oauth-proxy.example.toml` to configure a local proxy. The committed Kubernetes config in `k8s/config.toml` runs OAuth mode with admin API enabled. See `docs/runbook/README.md` for operational guidance and `specs/README.md` for current versus historical design specs.

## Deployment

Kubernetes manifests live in `k8s/`. Apply with `kubectl apply -k k8s/`. No secrets required. See `docs/runbook/README.md` for the complete deployment procedure.

## License

MIT
