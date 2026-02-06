# Tailnet Microservices

Single-binary Rust services that join a Tailscale tailnet and act as infrastructure proxies. Each service is a tailnet node, reachable via MagicDNS, with Prometheus metrics and structured JSON logging.

## Services

`anthropic-oauth-proxy` injects the `anthropic-beta: oauth-2025-04-20` header into requests proxied to `https://api.anthropic.com`. This enables Claude Max OAuth token authentication through proxies like Aperture that lack custom header injection. Runs as a Kubernetes pod with a `tailscaled` sidecar sharing a Unix socket.

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
  common/           # Shared types: Config, Secret<T>, error types
services/
  oauth-proxy/      # Anthropic OAuth header injection proxy
specs/
  *.md              # Service specifications
k8s/                # Kubernetes deployment manifests
```

## Configuration

Copy `anthropic-oauth-proxy.example.toml` and set the Tailscale auth key via the `TS_AUTHKEY` environment variable. See `specs/oauth-proxy.md` for the full configuration reference and `RUNBOOK.md` for operational guidance.

## Deployment

Kubernetes manifests live in `k8s/`. Apply with `kubectl apply -k k8s/` after creating the required secrets. See `RUNBOOK.md` for the complete deployment procedure.

## License

MIT
