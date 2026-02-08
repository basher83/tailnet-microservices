# Tailnet Microservices

Single-binary Rust services that act as infrastructure proxies on a Tailscale tailnet. Tailnet exposure is handled by the Tailscale Operator via Kubernetes Service annotations. Each service includes Prometheus metrics and structured JSON logging.

## Services

`anthropic-oauth-proxy` injects the `anthropic-beta: oauth-2025-04-20` header into requests proxied to `https://api.anthropic.com`. This enables Claude Max OAuth token authentication through proxies like Aperture that lack custom header injection. Runs as a single-container Kubernetes pod with zero secrets.

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
services/
  oauth-proxy/      # Anthropic OAuth header injection proxy
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
