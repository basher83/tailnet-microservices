# Specifications

| Spec | Status | Code | Purpose |
|------|--------|------|---------|
| [oauth-proxy.md](./oauth-proxy.md) | **Complete** | services/oauth-proxy/ | Anthropic OAuth header injection proxy |
| [operator-migration.md](./operator-migration.md) | **Complete** | services/oauth-proxy/, k8s/ | Remove tailscaled sidecar, delegate to Tailscale Operator |
| [operator-migration-addendum.md](./operator-migration-addendum.md) | **Manifests complete** | k8s/ | Tailscale Ingress for traffic routing (extends Spec A) |
| [anthropic-oauth-gateway.md](./anthropic-oauth-gateway.md) | **Complete** | crates/, services/oauth-proxy/, k8s/ | OAuth pool gateway — PKCE, token refresh, subscription pooling (supersedes oauth-proxy.md) |
| [tailnet.md](./tailnet.md) | Superseded by operator-migration | (deleted) | Tailnet integration via tailscaled sidecar (Option B) |
| [rand-0.10-migration.md](./rand-0.10-migration.md) | **Complete** | crates/anthropic-auth/ | Migrate rand 0.9 → 0.10 (breaking API renames) |
| [generic-client-support.md](./generic-client-support.md) | **Complete** | services/oauth-proxy/ | Transform generic client requests to pass Claude Max OAuth credential validation |
| [streaming-timeout-fix.md](./streaming-timeout-fix.md) | **Active** | services/oauth-proxy/ | Replace wall-clock timeout with three-phase idle timeout for SSE streaming |
