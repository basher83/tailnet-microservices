---
status: Active
created: 2026-04-09
---

# Spec: OTel Trace Instrumentation

Author: Workshop (workshop Q43 gate). Forge Q22. Project: anthropic-oauth-proxy.
Depends on: Q41 (local OTel Collector — CLEARED), Q42 (PostToolUse hook trace producer — CLEARED).
Ensue context: `decisions/telemetry-wiring/d6-proxy-span-instrumentation`, `research/otel-protocol-tailscale/`.

## Overview

Add OTLP trace span emission to the anthropic-oauth-proxy so that every proxied API request produces a trace span visible in Phoenix. This is the D6 (API-boundary) layer of the two-layer telemetry architecture — D5 (hook-level, Q42) captures tool call spans from Claude Code sessions, D6 captures API request spans from the proxy. The two layers produce independent trace trees (D5a Option C — disconnected traces, no cross-layer programmatic correlation).

The instrumentation is additive. Existing Prometheus metrics are unchanged. The proxy's functional behavior (header injection, OAuth pooling, failover, streaming) is unaffected. Trace emission is controlled by an environment variable — when unset, the proxy behaves identically to pre-D6.

**Design constraints carried from the telemetry-wiring decision tree:**

| Constraint | Source | Impact |
|-----------|--------|--------|
| No request body parsing | D5a Option C | Proxy remains a transparent body relay. No session.id extraction. |
| Metadata JSON dict for custom attributes | Phoenix issue #8969 | Custom attributes stored in metadata dict, not top-level dot-separated OTLP attributes. |
| Runtime toggle via env var | adversarial-review-7-forge F2 | Zero overhead when disabled. No OTel SDK initialization. |
| Spec before code | adversarial-review-7-forge F1 | This document. |
| Uniform client treatment | adversarial-review-7-forge F4, D5a Option C | All clients (CC, forgeflare-hooks, future) produce identical spans. No client-aware logic. |

---

## Span Schema

Each proxied API request emits one OTLP span. One request, one span — no trace grouping, no parent context injection, no multi-span traces. This is simpler than D5's session-grouped traces (Q42 E4).

### Failover Semantics

The span captures final-attempt state — the outcome the client received. When failover occurs (account A returns 429, failover to account B which returns 200), the span reflects account B's successful response:

- `proxy.account_id` = account B (the account that produced the final response)
- `proxy.error_type` = null (final attempt succeeded)
- `proxy.failover_attempt` = total failover count (1 in this example — how many times the proxy switched accounts)
- `http.response.status_code` = 200 (final response status)
- `otel.status_code` = OK (derived from final response)

If all failover attempts fail (e.g., all accounts exhausted), the span reflects the last attempt's failure state. The intermediate attempts are not recorded in the span. Per-attempt events (quota exhaustion, account failover) are already captured by Prometheus metrics (`pool_quota_exhaustions_total`, `pool_failovers_total`) and structured log lines — the span does not duplicate this per-attempt detail.

Rationale: the span represents the request's outcome as observed by the client. The client received one response; the span describes that response. The failover count provides a signal that retries occurred without requiring the consumer to reconstruct the retry history.

### Top-Level Span Fields

These use standard OpenTelemetry semantic conventions and are set as top-level span attributes (not in the metadata dict). Phoenix renders these natively.

| Attribute | Type | Value Source | Notes |
|-----------|------|-------------|-------|
| `http.request.method` | string | `request.method()` (`proxy.rs:162`) | HTTP method: GET, POST, etc. |
| `http.response.status_code` | int | `upstream_response.status()` (`proxy.rs:292`) | Upstream HTTP status code. In failover scenarios, this is the final attempt's status. |
| `url.path` | string | `request.uri().path()` (`proxy.rs:164`) | Request path (e.g., `/v1/messages`) |
| `server.address` | string | `state.upstream_url` (`proxy.rs:167`) | Upstream target (api.anthropic.com) |
| `otel.status_code` | string | derived from final HTTP status | "OK" for 2xx, "ERROR" for 4xx/5xx. Derived independently from `proxy.error_type` — see attribute interaction table below. |

Request duration is captured by the OTLP span's intrinsic timing (end_time minus start_time), not by an explicit attribute. The span starts when the Tower tracing layer intercepts the request and ends when the response is returned. This avoids drift between an explicit duration attribute and the span's intrinsic duration, and is consistent with how Phoenix displays span timing. The Prometheus `proxy_request_duration_seconds` histogram continues to use `start.elapsed()` independently.

### Metadata JSON Dict

Custom proxy-specific attributes are emitted as a single OTLP string attribute named `metadata` whose value is a JSON-serialized dict containing the keys defined below. Phoenix's `load_json_strings` deserializes this attribute, making keys queryable via `metadata["key"] == "value"` DSL syntax (PR #2268). This is the same encoding used by Q42's trace daemon. Top-level dot-separated custom attributes are not filterable in Phoenix (issue #8969) — the single-string JSON approach avoids this limitation.

| Key | Type | Value Source | Notes |
|-----|------|-------------|-------|
| `proxy.account_id` | string or null | `account_id` from `provider.prepare_request()` (`proxy.rs:245-264`) | OAuth account used for this request. Null in passthrough mode. |
| `proxy.error_type` | string or null | `provider.classify_error()` (`proxy.rs:304-306`) | Error classification: "quota_exhausted", "permanent", "transient", or null for success. |
| `proxy.failover_attempt` | int | `failover` loop counter (`proxy.rs:238`) | 0 for first attempt, increments on quota failover. |
| `proxy.request_id` | string | `request_id` (`proxy.rs:148`) | Unique request identifier. |
| `proxy.pool_mode` | string | `state.provider.needs_body()`: `true` → `"oauth"`, `false` → `"passthrough"` | Operating mode of the proxy for this request. |

### Attribute Interaction: `otel.status_code` vs `proxy.error_type`

These two attributes are independently derived and can produce the following combinations:

| Scenario | `otel.status_code` | `proxy.error_type` | `proxy.failover_attempt` |
|----------|--------------------|--------------------|--------------------------|
| Success, no failover | OK | null | 0 |
| Success after failover | OK | null | > 0 |
| Quota exhausted (all accounts) | ERROR | "quota_exhausted" | max failovers |
| Permanent upstream error | ERROR | "permanent" | attempt where error occurred |
| Transient upstream error | ERROR | "transient" | attempt where error occurred |
| Upstream 401 (no error_type match) | ERROR | null | current attempt |
| Passthrough mode error | ERROR | null | 0 |

`otel.status_code` is derived from the final HTTP response status. `proxy.error_type` is derived from the provider's error classification of that response. They can disagree when the upstream returns an error status that doesn't match any classification category (e.g., 401 → ERROR status, but no error_type classification). This is expected — `otel.status_code` is a standard OTel convention, `proxy.error_type` is proxy-specific business logic.

### Resource Attributes

Set once at TracerProvider initialization, applied to all spans.

| Attribute | Value | Source |
|-----------|-------|--------|
| `service.name` | `anthropic-oauth-proxy` | Fixed string. Distinguishes proxy spans from D5 hook spans (`service.name=claude-code`) in Phoenix. |
| `service.version` | Cargo package version | `env!("CARGO_PKG_VERSION")` or equivalent |
| `collector.source` | `talos-cluster` | Per `research/otel-collector-resource-attributes/` Q3 findings. Not a standard OTel convention — carried for consistency with the telemetry stack's resource attribute schema. Distinguishes cluster-origin spans from Mac Studio-origin spans (Q42 daemon uses `collector.source=mac-studio` equivalent via the local collector's resource processor). |

### What Is NOT in the Schema

No client-aware logic. No request body inspection. No session.id extraction. No `metadata.user_id` parsing. No conditional attribute extraction based on request origin. All clients (Claude Code, forgeflare-hooks, any future tailnet client) produce spans with the identical schema above. This is a direct consequence of D5a Option C (disconnected traces) and the forge design principle of single responsibility.

---

## Export Configuration

The proxy runs on-cluster (Talos, namespace `anthropic-oauth-proxy`, pod on `talos-uv1-t2i`). It exports OTLP traces directly to Phoenix via cluster-internal gRPC — no Tailscale hop, no local collector relay.

| Setting | Value | Rationale |
|---------|-------|-----------|
| Endpoint | `phoenix-helm-svc.phoenix.svc.cluster.local:4317` | Cluster-internal service DNS. gRPC port per Phoenix Helm chart defaults. |
| Protocol | gRPC | `research/otel-protocol-tailscale/` confirms gRPC works cluster-internal. HTTP/protobuf is the alternative but gRPC provides streaming, flow control, and is the OTel default. |
| Configuration | `OTEL_EXPORTER_OTLP_ENDPOINT` env var | Standard OTel SDK env var. Consistent with proxy's existing env var pattern (CONFIG_PATH, LOG_LEVEL). |

**Why this differs from Q41/Q42:** Q41's local collector on Mac Studio exports to Phoenix over Tailscale via OTLP/HTTP (`https://phoenix.tailfb3ea.ts.net`). Q42's trace daemon exports to the local collector on `:4318`. The proxy is already on the same cluster as Phoenix — no collector relay needed, no Tailscale transit. Direct gRPC to the service is the shortest path.

### BatchSpanProcessor Configuration

Spans are batched for export using the OTel SDK's BatchSpanProcessor. Proxy request volume is low-to-moderate (operator CC sessions + eval runs, not high-throughput production traffic).

| Setting | Value |
|---------|-------|
| Max queue size | 2048 |
| Scheduled delay | 5000ms |
| Max export batch size | 512 |

These match Q42's daemon configuration for consistency across the telemetry stack. Adjustable via OTel SDK env vars (`OTEL_BSP_*`) if needed.

---

## Runtime Toggle

The proxy's OTel trace instrumentation is controlled entirely by the `OTEL_EXPORTER_OTLP_ENDPOINT` environment variable.

### When OTEL_EXPORTER_OTLP_ENDPOINT is UNSET

- No OTel SDK initialization (no TracerProvider, no BatchSpanProcessor, no exporter)
- No tracing instrumentation layer in the Tower stack
- Zero overhead — proxy behavior is identical to pre-D6
- No new dependencies loaded at runtime
- This is the default state after deployment until the operator explicitly enables tracing

### When OTEL_EXPORTER_OTLP_ENDPOINT is SET

- TracerProvider initialized with BatchSpanProcessor exporting to the configured endpoint
- Tracing instrumentation layer added to the Tower service stack
- Each proxied request produces one OTLP span with the schema above
- Prometheus metrics continue independently — additive, not a replacement

### Export Failure Behavior

When tracing is enabled but the export endpoint is unreachable or returns errors:

- Spans are buffered in the BatchSpanProcessor queue (up to max queue size)
- When the queue is full, new incoming spans are dropped (the OTel Rust SDK uses `try_send` on a bounded channel — newest spans are lost, not oldest)
- The proxy continues serving requests normally — no cascading failure, no request blocking, no error propagation to clients
- Export failures are logged at WARN level via `tracing` (already configured for JSON structured logging). This requires wiring the OTel SDK's error handler (`opentelemetry::global::set_error_handler`) to emit through the `tracing` subscriber so export errors appear in the proxy's structured log output

### Graceful Shutdown

When the proxy receives a shutdown signal, `TracerProvider::shutdown()` must be called to flush any buffered spans before the process exits. The proxy already has graceful shutdown logic (axum server shutdown signal handling). The TracerProvider shutdown hooks into that sequence — flush spans, then exit. Without this, every deploy loses an average of 2.5s of trailing spans (half the 5s BatchSpanProcessor flush interval). The shutdown flush has a timeout (OTel SDK default: 5s) to prevent hanging on an unreachable export endpoint.

### Deployment Risk Context

The proxy is the sole API gateway — single pod, Recreate deployment strategy, automated ArgoCD sync from main branch (adversarial-review-4-lab evidence). A bad deploy means all CC sessions and CIAB eval runs lose API access until the operator notices and reverts. The env var toggle is the blast-radius containment: deploy the instrumented binary with `OTEL_EXPORTER_OTLP_ENDPOINT` unset, verify the proxy operates normally, then set the env var to enable tracing. If tracing causes issues, unset the env var — no code revert needed.

---

## Operational Constraints

### Latency Budget

Per-request overhead from trace instrumentation must be under 5ms at p99. This budget covers span creation, attribute population, and enqueueing to the BatchSpanProcessor. It does not include export latency (async, handled by the BatchSpanProcessor thread).

Reference: Q42's PostToolUse hook achieves 3-5ms total execution time including socket write. The proxy's instrumentation is lighter — no socket I/O, just in-process span creation and attribute setting.

### Memory Overhead

OTel SDK steady-state memory overhead must be under 10MB. This covers the TracerProvider, BatchSpanProcessor queue (2048 spans max), and exporter state. The proxy's current RSS is modest (single-binary Rust service) — 10MB is a generous ceiling that should not be reached under normal operation.

Reference: Q42's trace daemon (Node.js, heavier runtime) showed RSS delta of ~1.3MB after 30 spans across 3 sessions.

### Prometheus Independence

The 6 existing Prometheus metrics (`proxy_requests_total`, `proxy_request_duration_seconds`, `proxy_upstream_errors_total`, `pool_account_status`, `pool_failovers_total`, `pool_quota_exhaustions_total`) must continue to function identically. The `metrics` crate and `metrics-exporter-prometheus` are independent of the `tracing`/`opentelemetry` stack. OTel instrumentation is additive — it reads the same request/response data that Prometheus metrics read, but writes to a different output (OTLP spans vs Prometheus counters/histograms). No Prometheus metric is modified, removed, or re-routed.

---

## Validation Criteria

Forge must prove these when implementing. Each criterion names what to observe and how to measure it.

1. **Spans in Phoenix with correct attribute names.** After a proxied request, Phoenix shows a span with `service.name=anthropic-oauth-proxy`, `http.request.method` matching the request's HTTP method, and `http.response.status_code` matching the upstream response status. These exact attribute names (not legacy conventions like `http.method` or `http.status_code`) must be present.

2. **All metadata dict keys queryable.** All five metadata dict keys (`proxy.account_id`, `proxy.error_type`, `proxy.failover_attempt`, `proxy.request_id`, `proxy.pool_mode`) are queryable via Phoenix DSL. Verify each: `metadata["proxy.account_id"]`, `metadata["proxy.error_type"]`, etc. All five must return expected values for a known span.

3. **Prometheus unaffected.** Scrape `/metrics` before and after enabling OTel tracing. The same 6 metric names appear with the same label schema. Metric values increment normally for proxied requests. No new metrics introduced by OTel instrumentation.

4. **Latency within budget.** Measure span creation and BatchSpanProcessor enqueue overhead in-process using `std::time::Instant` around the span creation and attribute-setting code path, excluding upstream API latency. Alternatively, use a mock upstream with deterministic latency (e.g., 10ms fixed delay) to control for noise. Overhead must be under 5ms at p99 across 100+ requests. Do not measure via `proxy_request_duration_seconds` — upstream API latency (hundreds of ms to seconds) makes 5ms overhead undetectable in that histogram.

5. **Memory bounded (design-time constraint).** The BatchSpanProcessor queue (2048 max spans) times estimated span size provides the theoretical memory ceiling. This is a design-time bound, not a runtime RSS measurement — Rust allocator behavior (arena retention, page faults, TLS cache) creates RSS noise in the 5-10MB range that would confound measurement. Document the calculated overhead (queue size * span size + exporter state) and confirm it is under 10MB.

6. **Toggle off = structural absence.** With `OTEL_EXPORTER_OTLP_ENDPOINT` unset, the Tower `Router` must not include the tracing instrumentation layer. Verify structurally: the `build_router` (or equivalent) function conditionally applies the tracing layer only when the env var is set. A unit test or code review confirming the layer is absent from the service stack when the env var is unset satisfies this criterion. Behavioral verification (no spans, no logs) is supplementary but insufficient alone — a no-op layer still has per-request dispatch overhead.

7. **Toggle on = spans flow.** With `OTEL_EXPORTER_OTLP_ENDPOINT` set to `phoenix-helm-svc.phoenix.svc.cluster.local:4317`, spans appear in Phoenix within the BatchSpanProcessor flush interval (5s default).

8. **Export failure resilience.** With `OTEL_EXPORTER_OTLP_ENDPOINT` set to an unreachable endpoint: (a) the proxy continues serving requests normally with no client-visible errors, and (b) export failures are observable — the OTel SDK's error handler is wired into the `tracing` subscriber so that export failures produce WARN-level structured log entries. These are separate properties: (a) is resilience, (b) is observability.

9. **No client-specific attribute extraction (code review).** The proxy source contains no conditional logic that inspects request origin, User-Agent, request body content, or any other property to vary span attributes per client. This is a code review criterion, not a runtime test — the proxy has no mechanism to distinguish clients by architecture (D5a Option C, adversarial-review-7-forge F4). Verify by reviewing the span creation code path for absence of client-identification logic.

---

## Source References

| Reference | What it provides |
|-----------|-----------------|
| `decisions/telemetry-wiring/d6-proxy-span-instrumentation` (ensue) | Decision node: scope, options, adversarial review evidence, D5a impact |
| `decisions/telemetry-wiring/d5a-session-id-propagation` (ensue) | Option C decision: disconnected traces, no body parsing |
| `decisions/telemetry-wiring/overview` (ensue) | Three data paths architecture, two execution environments, scope boundaries |
| `research/otel-protocol-tailscale/` (ensue) | gRPC works cluster-internal, protocol selection rationale |
| Q41-gate.md (workshop, CLEARED) | Local collector deployment, E1 encoding constraints, collector config |
| Q42-gate.md (workshop, CLEARED) | Hook trace producer, E4 deterministic trace_id, operational validation |
| adversarial-review-4-lab | Single-replica deployment risk, blast-radius containment |
| adversarial-review-7-forge | Spec-before-code, runtime toggle, multi-client traffic, single responsibility |
| Phoenix issue #8969 | Dot-separated custom attributes not filterable in Phoenix DSL |
