# Specifications

| Spec | Status | Code | Purpose |
|------|--------|------|---------|
| [openclaw-tool-rewrite.md](./openclaw-tool-rewrite.md) | Draft; I1–I3 investigations pending, implementation deferred | services/oauth-proxy/ | Per-client tool-name rewrite profile so OpenClaw's distinctive tool set passes Anthropic's fingerprint gate |
| [anthropic-oauth-gateway.md](./anthropic-oauth-gateway.md) | Current OAuth design, needs drift cleanup | crates/, services/oauth-proxy/, k8s/ | OAuth pool gateway: PKCE, token refresh, subscription pooling, admin API |
| [generic-client-support.md](./generic-client-support.md) | Current behavior, minor cleanup needed | services/oauth-proxy/ | Request-shape compatibility for generic clients using Claude Max OAuth credentials |
| [streaming-timeout-fix.md](./streaming-timeout-fix.md) | Implemented; 2026-05-09 Pi file-write/tool live smoke passed; extended Claude Code long-session soak deferred | services/oauth-proxy/ | Replace wall-clock timeout with initial-response timeout plus stream idle timeout |
| [otel-trace-instrumentation.md](./otel-trace-instrumentation.md) | Implemented; 2026-05-09 Phoenix span smoke passed; controlled error/failover metadata and full Prometheus event coverage deferred | services/oauth-proxy/, k8s/ | OTLP trace span emission to Phoenix via env-var-controlled OpenTelemetry setup |
| [operator-migration-addendum.md](./operator-migration-addendum.md) | Complete; 2026-05-09 live ArgoCD/Tailscale/health/proxy path verified | k8s/ | Tailscale Ingress for traffic routing; Service annotations removed |
| [operator-migration.md](./operator-migration.md) | Historical; superseded by addendum for traffic exposure | services/oauth-proxy/, k8s/ | Remove tailscaled sidecar and delegate tailnet identity/routing to the Tailscale Operator |
| [oauth-proxy.md](./oauth-proxy.md) | Historical passthrough-era spec; superseded by OAuth gateway | services/oauth-proxy/ | Original Anthropic header injection proxy |
| [tailnet.md](./tailnet.md) | Historical; superseded and retained for context | specs/ | Original tailscaled sidecar integration strategy |
| [rand-0.10-migration.md](./rand-0.10-migration.md) | Complete | crates/anthropic-auth/ | Migrate rand 0.9 to 0.10 breaking API names |

Use `RUNBOOK.md` for current operator procedures. Several older specs are retained as design history and still contain stale values or pre-addendum deployment assumptions.
