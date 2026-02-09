# Specifications

| Spec | Status | Code | Purpose |
|------|--------|------|---------|
| [oauth-proxy.md](./oauth-proxy.md) | **Complete** | services/oauth-proxy/ | Anthropic OAuth header injection proxy |
| [operator-migration.md](./operator-migration.md) | Complete | services/oauth-proxy/, k8s/ | Remove tailscaled sidecar, delegate to Tailscale Operator |
| [operator-migration-addendum.md](./operator-migration-addendum.md) | **Manifests complete** | k8s/ | Tailscale Ingress for traffic routing (extends Spec A) |
| [anthropic-oauth-gateway.md](./anthropic-oauth-gateway.md) | **Draft** | crates/, services/oauth-proxy/ | OAuth pool gateway — PKCE, token refresh, subscription pooling (supersedes oauth-proxy.md) |
| [tailnet.md](./tailnet.md) | Superseded by operator-migration | (deleted) | Tailnet integration via tailscaled sidecar (Option B) |
