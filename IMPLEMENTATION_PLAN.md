# Implementation Plan

Previous build history archived at IMPLEMENTATION_PLAN_v2.md (v0.0.125, 199 tests, all specs complete, E2E cluster verified 2026-02-15). Previous v1 history archived at IMPLEMENTATION_PLAN_v1.md (81 audits, 111 tests, v0.0.102).

## Current Spec

Recently completed closeout: deployed proxy live validation and plan/spec evidence refresh.

2026-05-09 live evidence:
- `mise run live:validate` is the local/operator live gate. It is intentionally not a GitHub CI gate because it requires kube/Tailscale credentials; deterministic source gates remain in `mise run ci` and `.github/workflows/ci.yml`.
- ArgoCD reported `anthropic-oauth-proxy` as `Synced` and `Healthy` at revision `43e62ca97ac27e118e44525beb02b7ddc60a0c51`.
- Tailscale Ingress `anthropic-oauth-proxy` routed `/` to `anthropic-oauth-proxy:80 (10.244.1.109:8080)`; exactly one `ts-anthropic-oauth-proxy-*` pod was running.
- Tailnet health returned `HTTP/2 200` with `status: healthy`, `mode: anthropic`, and one available account.
- Pi traffic through `anthropic-proxy` returned `live-proxy-ok`; a scratch-dir Pi file-write/tool request created and read `live-validation.txt` successfully.
- Phoenix `default` project showed recent `proxy_request` spans with `http.request.method=POST`, `http.response.status_code=200`, `url.path=/v1/messages`, `server.address=https://api.anthropic.com`, and flattened `metadata.proxy.*` fields.

Remaining explicit deferrals:
- Current Claude Code header drift capture remains a separate sooner-rather-than-later follow-up; runtime `x-anthropic-billing-header` behavior stays unchanged.
- Extended multi-minute Claude Code long-session soak is deferred; the 2026-05-09 live smoke covered Pi file-write/tool traffic, not a broad soak.
- Phoenix controlled error/failover metadata validation is deferred; successful spans omit `metadata.proxy.error_type` because the source value is null.
- Full Prometheus event coverage is deferred until error/failover/quota events occur or are generated in a controlled test.

## Baseline

v0.0.128: 211 tests pass (138 oauth-proxy + 4 common + 9 provider + 22 anthropic-auth + 38 anthropic-pool), 2 ignored (load test, memory soak). OAuth mode removes Pi's local documentation-routing hint from system prompts because Anthropic's Max-plan classifier rejects that hint as extra usage even with Claude Code attribution.

v0.0.127: 209 tests pass (136 oauth-proxy + 4 common + 9 provider + 22 anthropic-auth + 38 anthropic-pool), 2 ignored (load test, memory soak). OAuth mode now injects Claude Code's `x-anthropic-billing-header` attribution so Max-plan OAuth requests route to plan usage instead of extra-usage rejection.

v0.0.126: 209 tests pass (136 oauth-proxy + 4 common + 9 provider + 22 anthropic-auth + 38 anthropic-pool), 2 ignored (load test, memory soak). Pipeline clean.

## Completed Specs

oauth-proxy.md, operator-migration.md, operator-migration-addendum.md, anthropic-oauth-gateway.md, rand-0.10-migration.md, generic-client-support.md, streaming-timeout-fix.md, otel-trace-instrumentation.md.

**otel-trace-instrumentation.md** — OTel OTLP trace instrumentation with env var toggle, metadata JSON dict, gRPC export via BatchSpanProcessor, SpanRecorder drop guard for reliable attribute emission, graceful shutdown flush.
