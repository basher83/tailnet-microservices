# Documentation Audit Report

Generated: 2026-04-28 | Commit: 7b993c6

## Scope

This audit checked the user-facing repo documentation against the current Rust workspace, Kubernetes manifests, CI workflow, and GitOps deployment definition. The primary local targets were `README.md`, `RUNBOOK.md`, `specs/*.md`, `anthropic-oauth-proxy.example.toml`, `k8s/*.yaml`, and `k8s/config.toml`. A deployment cross-check was also performed against `/Users/basher8383/3I/lab/mothership-gitops` at commit `76805e5`. Implementation plans and prompt files were treated as planning artifacts rather than user-facing docs.

The audit used a two-pass approach. Pass 1 verified direct claims in the docs. Pass 2 expanded around the drift patterns found in Pass 1: tailnet exposure mechanism, timeout defaults, health JSON shape, OAuth pool behavior, metrics, CI/deploy behavior, and spec status checkboxes.

## Executive Summary

| Metric | Count |
|--------|-------|
| Documents scanned | 19 |
| Concrete claims sampled | 104 |
| Verified true or substantially current | 75 |
| Verified false or stale | 22 |
| Needs external/runtime verification | 7 |

The docs are broadly aligned on the repo's purpose, workspace layout, OAuth gateway architecture, admin endpoints, credential storage, and basic Kubernetes footprint. The main drift is concentrated in older specs and operational guidance that still describe prior states. The highest-impact operational errors are the default timeout value, pool-exhausted HTTP status, health response field names, current OAuth mode in `k8s/config.toml`, and tailnet exposure wording that still says Service annotations in places where the live manifests use Tailscale Ingress.

## False Claims Requiring Fixes

### README.md

| Line | Claim | Reality | Fix |
|------|-------|---------|-----|
| 8 | Tailnet exposure is handled by the Tailscale Operator via Kubernetes Service annotations. | Current `k8s/service.yaml` is a plain ClusterIP with no Tailscale annotations. `k8s/ingress.yaml` uses `ingressClassName: tailscale` and `tls.hosts: [anthropic-oauth-proxy]`. | Replace “Service annotations” with “Tailscale Ingress”. |

### RUNBOOK.md

| Line | Claim | Reality | Fix |
|------|-------|---------|-----|
| 31 | CI runs lint, audit, test, and build; on success the Docker job builds and deploy proceeds. | `.github/workflows/ci.yml` also has a `manifests` validation job, but `docker.needs` only includes lint, audit, test, and build. A failing manifests job does not block the Docker or deploy jobs. | Either document the actual dependency graph or update CI so `docker` also needs `manifests`. |
| 113 | To switch from passthrough to OAuth mode, uncomment `[oauth]` and `[admin]` in `k8s/config.toml`. | `k8s/config.toml` currently has active `[oauth]` and `[admin]` sections, so the deployed config is OAuth mode already. | Rewrite this as the inverse procedure, or state that the committed K8s config is OAuth mode by default. |
| 312 | OAuth health example uses `accounts_cooling`. | `Pool::health()` emits `accounts_cooling_down`. | Change example field to `accounts_cooling_down`. |
| 341 | `pool_failovers_total` increments due to quota exhaustion or permanent error. | `proxy.rs` records `pool_failovers_total` only on `QuotaExceeded`; permanent errors disable the account and return immediately. | Remove “or permanent error”. |
| 426 | `timeout_secs` default is 60s. | Code default is 180 seconds in `Config::default_timeout()`, and `k8s/config.toml` sets `timeout_secs = 180`. | Change default to 180s, and clarify the example TOML still uses 60s if that remains intentional. |
| 436 | If all OAuth accounts are exhausted after quota failover, the proxy returns 429. | If no account can be selected, `prepare_request()` fails with pool exhausted and `proxy.rs` returns 503. A final upstream 429 can pass through only when quota classification happens on the last selected account. | Document both cases: selection pool exhausted returns 503; upstream 429 can pass through after final quota attempt. |
| 442 | When all accounts are cooling or disabled, the proxy returns 429 to all requests. | With no selectable account, `Pool::select()` returns `PoolExhausted`, mapped to 503 Service Unavailable. | Change 429 to 503. |
| 445 | Add more accounts via the admin API PKCE flow. | The runbook’s own Known Issues section says the PKCE web flow is currently blocked and keychain extraction is the working provisioning path. | Point remediation to keychain extraction first, with PKCE noted as blocked until Anthropic policy changes. |

### specs/README.md

| Line | Claim | Reality | Fix |
|------|-------|---------|-----|
| 9 | `tailnet.md` code column is `(deleted)`. | `specs/tailnet.md` still exists in the repo. | Change the code/status wording to “superseded, retained for history” or delete the file if that is intended. |
| 13 | `otel-trace-instrumentation.md` is Active. | The source includes `telemetry.rs`, OTel dependencies, `OTEL_EXPORTER_OTLP_ENDPOINT` deployment env var, span metadata recording, and shutdown flushing. The spec may still be active for runtime validation, but implementation is not merely pending. | Change status to “Implemented, needs runtime validation” if Phoenix verification is still open. |

### specs/oauth-proxy.md

| Line | Claim | Reality | Fix |
|------|-------|---------|-----|
| 77 | `Config` contains only `proxy` and `headers`. | Current `Config` also contains `oauth: Option<OAuthConfig>` and `admin: Option<AdminConfig>`. | Update the type table or mark this spec as historical. |
| 251, 276 | `timeout_secs = 60` and default `timeout_secs` is 60. | Current code default is 180 seconds; K8s config also sets 180. | Change the default claim to 180, or mark the example as non-default. |
| 325 | Health has no degraded state. | OAuth provider health returns `healthy`, `degraded`, or `unhealthy`, while HTTP status remains 200. | Clarify this applies only to passthrough mode, or update to the current provider health model. |
| 342 | `toml = "0.9"`. | Workspace dependency is `toml = "1.0"`. | Update dependency version. |
| 391 | Kustomization includes namespace, serviceaccount, configmap, deployment, service, ingress. | Current `k8s/kustomization.yaml` also includes `pvc.yaml` and `admin-service.yaml`, and uses `configMapGenerator` rather than a checked-in `configmap.yaml`. | Update the Kubernetes manifest inventory. |
| 433, 444 | Kubernetes manifests use Tailscale Operator annotations. | Current Service has no Tailscale annotations; tailnet exposure is via Ingress. | Replace annotation wording with Ingress wording. |
| 454 | Operator handles connectivity externally via Service annotations. | Current implementation uses Tailscale Ingress; the addendum explicitly removed Service annotations. | Replace with Ingress-based wording. |

### specs/operator-migration.md

| Line | Claim | Reality | Fix |
|------|-------|---------|-----|
| 24 | Handoff artifact is a Tailscale Operator-annotated Service. | The current handoff artifact is a plain Service plus Tailscale Ingress. | Update boundary/handoff language to include the addendum outcome. |
| 58-66 | `k8s/service.yaml` should be annotated with `tailscale.com/expose` and hostname. | `specs/operator-migration-addendum.md` superseded this after dual-proxy conflict; current Service has no annotations. | Move the original annotation approach into a historical note and make Ingress the current requirement. |
| 144 | Config example uses `timeout_secs = 60`. | Current K8s config uses 180 and code default is 180. | Update or mark as historical. |
| 202 | Success criterion says Service is annotated for Tailscale exposure. | Current Service is intentionally not annotated. | Replace with “Ingress handles Tailscale exposure”. |

### specs/anthropic-oauth-gateway.md

| Line | Claim | Reality | Fix |
|------|-------|---------|-----|
| 90-117 | Provider trait sample uses async trait-style methods with `RequestBuilder`, and `report_error` lacks `account_id`. | Current trait is dyn-compatible with `Pin<Box<dyn Future...>>`, uses `HeaderMap` plus `serde_json::Value`, has `needs_body()`, and `report_error(account_id, classification)`. | Update the trait sample to match `crates/provider/src/lib.rs`. |
| 261 | A 401/403 permanent error triggers failover. | Current proxy disables the account and returns the upstream error immediately for `Permanent`. It does not continue to another account. | Change transition/action to “disable and return error” unless code is changed. |
| 284, 543 | Health/pool examples use `accounts_cooling`. | Current health JSON uses `accounts_cooling_down`. | Update field name. |
| 326-339 | System prompt prefix only applies to Opus/Sonnet; Haiku is not modified. | `provider_impl.rs` now injects the prefix for all models including Haiku, and converts string/absent system fields to array content blocks. | Update rules and code sample to current all-model array-block behavior. |
| 371 | Model extraction determines whether system prompt injection is needed. | Current code extracts model only to decide whether a request has a model at all; it no longer uses model family to skip Haiku. | Rewrite model extraction purpose. |
| 431 | Config example uses `timeout_secs = 60`. | Current K8s config and code default are 180. | Update or label as example override. |
| 536 | OAuth health mode is `oauth_pool`. | `health_handler()` emits `state.proxy.provider.id()`, which is `"anthropic"` for OAuth mode. | Change mode example to `"anthropic"` or change provider id if `oauth_pool` is desired. |
| 620-633 | PVC example uses namespace `tailnet`. | Current PVC namespace is `anthropic-oauth-proxy`. | Update namespace. |
| 711 | OAuth PKCE flow completes and admin adds account via CLI/API. | RUNBOOK Known Issues says Anthropic currently blocks the PKCE web flow; working provisioning is keychain extraction. | Mark PKCE as implemented but externally blocked, not successfully complete operationally. |
| 736 | Telemetry/session tracking is out of scope. | OTel trace instrumentation now exists in `services/oauth-proxy/src/telemetry.rs` and is configured in deployment. | Update out-of-scope language to reflect added D6 tracing. |

### specs/generic-client-support.md

| Line | Claim | Reality | Fix |
|------|-------|---------|-----|
| 70 | The struct-level doc comment still says non-Haiku models. | Current `provider_impl.rs` doc comment says injection is for all models. This note is stale. | Remove the stale note. |
| 72-75 | Array-format `system` fields are left as-is and string-only handling is sufficient. | Current `inject_system_prompt()` handles absent, string, and array `system` fields; it prepends the required block to arrays lacking the prefix. | Update R4 to implemented. |
| 115-116 | Architecture still labels body transformation as current plus future placeholder. | Current code performs all-model prefix injection and array handling. | Rewrite architecture notes to current behavior. |

### specs/streaming-timeout-fix.md

| Line | Claim | Reality | Fix |
|------|-------|---------|-----|
| 18, 245, 322 | `k8s/config.toml` sets `timeout_secs = 60`; 60s is the configured idle timeout. | Current `k8s/config.toml` sets `timeout_secs = 180`. | Update timeout value references or mark this as the historical pre-fix state. |
| 305-312 | Success criteria remain unchecked. | The implementation now contains `tokio::time::timeout`, `IdleTimeoutStream`, and direct dependencies, so at least source-level implementation criteria appear complete. Runtime criteria still require execution evidence. | Split source-complete criteria from runtime validation and update checkboxes based on current test results. |

### specs/otel-trace-instrumentation.md

| Line | Claim | Reality | Fix |
|------|-------|---------|-----|
| 142 | Default deployment has tracing unset until explicitly enabled. | Current `k8s/deployment.yaml` sets `OTEL_EXPORTER_OTLP_ENDPOINT` to Phoenix by default. | Update default-state language. |
| 204 | Toggle-off structural absence requires the Tower Router not to include the tracing instrumentation layer. | Current OTel layer is attached to the tracing subscriber, not the Tower Router. `build_router()` is unconditional; `telemetry::init_tracer()` returns `None` when env var is absent. | Rewrite criterion around subscriber-layer absence rather than Router-layer absence. |
| 206 | Example endpoint is `phoenix-helm-svc.phoenix.svc.cluster.local:4317`. | Current deployment sets `http://phoenix-helm-svc.phoenix.svc.cluster.local:4317`. The OTel exporter may require the scheme, but the spec and deployment should agree. | Normalize endpoint examples to match the runtime manifest. |

## Pattern Summary

| Pattern | Count | Root Cause |
|---------|-------|------------|
| Tailnet exposure described as Service annotations | 7 | Operator migration addendum superseded the original annotation plan, but older docs were not fully rewritten. |
| Timeout value drift between 60 and 180 seconds | 8 | Streaming timeout docs and older specs retained the old value after `k8s/config.toml` and code default moved to 180. |
| Health JSON field mismatch | 4 | Pool health implementation uses `accounts_cooling_down`; examples retained `accounts_cooling`. |
| OAuth mode behavior changed after spec completion | 8 | All-model system prompt injection, array system handling, permanent error behavior, and provider trait shape evolved after the gateway spec was marked complete. |
| Runtime status checkboxes overstate validation | 5 | Specs marked implementation complete or active without distinguishing source-level implementation from cluster/vendor validation. |

## GitOps Deployment Cross-Check

The ArgoCD management claim is confirmed at the manifest level. `/Users/basher8383/3I/lab/mothership-gitops/apps/anthropic-oauth-proxy.yaml` defines an ArgoCD `Application` named `anthropic-oauth-proxy` in namespace `argocd`, with sync wave `"8"`, source repo `https://github.com/basher83/tailnet-microservices.git`, `targetRevision: main`, `path: k8s`, destination namespace `anthropic-oauth-proxy`, automated sync, `prune: true`, `selfHeal: true`, retry backoff, `CreateNamespace=true`, and `ServerSideApply=true`.

The root app also supports the deployment path. `/Users/basher8383/3I/lab/mothership-gitops/bootstrap/bootstrap.yaml` points the bootstrap `root` Application at `mothership-gitops`, `targetRevision: main`, `path: apps`, with automated prune and self-heal. `/Users/basher8383/3I/lab/mothership-gitops/apps/root.yaml` documents wave 8 as “Anthropic OAuth Proxy (Kustomize from tailnet-microservices)”. The standalone `apps/anthropic-oauth-proxy.yaml` file lives directly under `apps/`, so it is part of the root app’s app-of-apps manifest set.

This confirms several RUNBOOK deployment claims that were previously only locally inferred: ArgoCD watches `main` at `k8s/`, uses automated sync, prune, and self-heal for this Application, and deploys it in wave 8 to namespace `anthropic-oauth-proxy`.

The GitOps repo also adds one new documentation drift item: `/Users/basher8383/3I/lab/mothership-gitops/specs/operator-migration-gitops.md` is stale. It is still marked Draft even though `apps/anthropic-oauth-proxy.yaml` exists, and its success criteria for Application YAML and root wave placement remain unchecked. It also says the proxy has no Ingress resources or supporting manifests, which no longer matches the source repo’s `k8s/` path consumed by ArgoCD. That spec should be updated or marked historical.

What remains unverified is live cluster state. The GitOps manifests prove intended ArgoCD configuration, not that the cluster is currently Healthy/Synced, that the Tailscale Ingress reconciled to one proxy pod, or that Phoenix receives spans.

## Gap Detection

The codebase has current implementation surfaces that are underrepresented or stale in docs. The OTel trace implementation exists in `services/oauth-proxy/src/telemetry.rs`, but `specs/README.md` still lists the spec as Active and the spec text partly describes pre-implementation design criteria. The Kubernetes deployment now includes `pvc.yaml`, `admin-service.yaml`, active OAuth/admin config, and default OTLP export, but older specs still describe a simpler passthrough-oriented deployment.

The docs also contain old historical specs that are useful as design history but dangerous as current reference material. `specs/oauth-proxy.md`, `specs/operator-migration.md`, and `specs/streaming-timeout-fix.md` mix implemented facts, historical context, and stale operational claims. The safest fix is not to erase history, but to add a current-state banner to superseded specs and move current operator instructions into `README.md` and `RUNBOOK.md`.

## Human Review Queue

- Verify whether `pool_failovers_total` should increment for permanent errors. Current code does not; if the intended metric semantics include permanent account switching, this is a code gap rather than a doc gap.
- Verify live cluster behavior for the Tailscale Ingress, since this audit checked source, manifests, and GitOps intent, but not current cluster state. The unchecked criteria in `specs/operator-migration-addendum.md` still need cluster evidence.
- Verify Phoenix span ingestion and queryability for `metadata[...]` fields. The source emits metadata JSON and the deployment enables OTLP, but the audit did not query Phoenix.
- Verify ArgoCD live status for `anthropic-oauth-proxy`. GitOps source confirms the Application definition, but Healthy/Synced and zero-downtime adoption require cluster evidence.
- Decide whether `timeout_secs = 180` is the desired long-term default. If 60 is still the intended production idle timeout, the code and K8s config should be changed instead of the docs.
- Decide whether `specs/*.md` should remain canonical specs or become historical design records. Several specs marked Complete are not safe as current operational references without a status banner.

## Recommended Fix Order

1. Fix `RUNBOOK.md` first, because it contains operator-facing commands and status-code expectations.
2. Fix `README.md` and `specs/README.md` next, because they route readers to current versus historical docs.
3. Add stale-status banners to superseded specs before line-editing every historical detail.
4. Update `specs/anthropic-oauth-gateway.md` where it is still used as the current OAuth contract.
5. Update mothership-gitops `specs/operator-migration-gitops.md` or mark it historical, because the Application exists and its source path now consumes supporting manifests from this repo.
6. Re-run `cargo test --workspace`, `cargo clippy --workspace -- -D warnings`, and `kubectl kustomize k8s/` after any code or manifest changes. This audit report itself did not change code.
