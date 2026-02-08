# Specifications

| Spec | Status | Code | Purpose |
|------|--------|------|---------|
| [operator-migration.md](./operator-migration.md) | **Active** | services/oauth-proxy/, k8s/ | Remove tailscaled sidecar, delegate to Tailscale Operator |
| [oauth-proxy.md](./oauth-proxy.md) | Complete — Phase 3 superseded by operator-migration | services/oauth-proxy/ | Anthropic OAuth header injection proxy |
| [tailnet.md](./tailnet.md) | Superseded by operator-migration | services/oauth-proxy/src/tailnet.rs | Tailnet integration via tailscaled sidecar (Option B) |
