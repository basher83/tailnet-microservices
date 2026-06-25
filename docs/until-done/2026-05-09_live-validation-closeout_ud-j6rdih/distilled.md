# Deployed Proxy Live Validation Closeout

## Problem

The tailnet microservices repo had source-level implementation complete for the Anthropic OAuth proxy, but several open questions required live evidence rather than local build/test confidence: ArgoCD/Tailscale deployment health, tailnet routing, Pi/Claude traffic through the deployed proxy, Phoenix/OTel span ingestion, and the correct place to track remaining deferrals.

## Solution Shape

A local/operator-only live validation gate was added to `mise.toml` as `mise run live:validate`. It intentionally stays out of GitHub CI because it requires local kube/Tailscale/Phoenix/Pi credentials. The deterministic source gates remain `mise run ci` and `.github/workflows/ci.yml`.

`live:validate` now performs read-only checks across the approved live surfaces:

- current kube context
- ArgoCD Application sync/health
- Kubernetes namespace resources
- proxy pod count
- Tailscale proxy pod count
- Ingress backend route to Service/pod
- tailnet `/health` HTTP 200 and JSON health body
- Pi headless proxy smoke through `anthropic-proxy`
- Phoenix recent span query for `proxy_request` spans with `/v1/messages` and `metadata.proxy.pool_mode=oauth`

Current plan/spec evidence was updated:

- `IMPLEMENTATION_PLAN.md` records recently completed live validation and explicit deferrals.
- `specs/README.md` remains the spec index/status router.
- `specs/operator-migration-addendum.md` closes live Argo/Tailscale/health/proxy criteria.
- `specs/streaming-timeout-fix.md` records Pi file-write/tool smoke but keeps extended Claude Code long-session soak deferred.
- `specs/otel-trace-instrumentation.md` records Phoenix success-span evidence and defers controlled error/failover metadata plus full Prometheus event-series coverage.

Runtime `x-anthropic-billing-header` behavior was not changed.

## Key Evidence

Final `mise run live:validate` passed and showed:

- ArgoCD app `anthropic-oauth-proxy`: `sync=Synced health=Healthy`
- Ingress address: `anthropic-oauth-proxy.tailfb3ea.ts.net`
- Proxy pod: `anthropic-oauth-proxy-6694d95f6b-45n9n 1/1 Running`
- `Proxy pod count: 1`
- `Tailscale proxy pod count: 1`
- Ingress backend: `/ anthropic-oauth-proxy:80 (10.244.1.109:8080)`
- `Tailnet health HTTP 200`
- health JSON with `status=healthy`, `mode=anthropic`, one available account
- `Pi proxy smoke: live-proxy-ok`
- `Phoenix proxy span count: 10`
- representative Phoenix span: `proxy_request`, `status_code=OK`, `method=POST`, `status=200`, `path=/v1/messages`, `pool=oauth`

Final `mise run ci` passed after live gate edits, with 211 passing tests and 2 ignored tests across the workspace plus valid k8s manifest rendering.

## Learnings

- The live gate must include all required live surfaces or the completion judge correctly rejects the goal. Initial `live:validate` only covered kube/Tailscale/health; it had to be expanded to include Pi and Phoenix.
- Phoenix CLI span attribute filters were not reliable enough for dotted metadata attributes in this context; querying recent spans and filtering with `jq` produced deterministic proof.
- Phoenix flattens metadata JSON into `metadata.proxy.*` attributes and omits `metadata.proxy.error_type` on successful spans because the source value is null.
- `metrics_exporter_prometheus` only renders observed series; no error/failover/quota event metrics appear until those events occur or are generated.
- Pi emits model-pattern warnings from local defaults before successful `anthropic-proxy` output. They are client config noise, not proxy failure.

## Gotchas

- Do not add `live:validate` to normal GitHub CI unless credentials and flake tolerance are explicitly designed; it is an operator/local gate.
- Do not close the extended Claude Code long-session soak based on a Pi file-write/tool smoke. It is useful partial evidence but not a broad multi-minute soak.
- Do not claim full Phoenix metadata coverage from success spans alone; controlled error/failover traffic is needed for `metadata.proxy.error_type` and related metric series.
- Do not interpret the cumulative dirty working tree as only live-validation work; earlier closeout changes remain present.
- Scratch files are written under `/tmp` by the live gate and cleanup should remove them after manual runs if needed.

## Remaining Follow-Ups

1. Current Claude Code header drift capture — sooner rather than later, but not part of this live validation closeout.
2. Extended multi-minute Claude Code long-session/file-write/multi-tool soak through the deployed proxy.
3. Controlled error/failover Phoenix metadata validation, including `metadata.proxy.error_type`.
4. Full Prometheus event-series validation once error/failover/quota events are safely generated.

## Useful Surfaces

- `mise run live:validate` — canonical local/operator live validation gate.
- `mise run ci` — deterministic source/release readiness gate.
- `https://anthropic-oauth-proxy.tailfb3ea.ts.net/health` — tailnet health endpoint.
- `https://phoenix.tailfb3ea.ts.net` with `PHOENIX_PROJECT=default` — Phoenix query surface.
- `kubectl -n argocd get app anthropic-oauth-proxy -o wide` — ArgoCD live app status.
- `kubectl -n anthropic-oauth-proxy describe ingress anthropic-oauth-proxy` — ingress backend route evidence.
