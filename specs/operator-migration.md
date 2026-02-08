# Spec: Operator Migration

**Status:** Complete
**Created:** 2026-02-07
**Author:** Brent + Claude
**Scope:** tailnet-microservices repo only

---

## Overview

Remove the `tailscaled` sidecar from the anthropic-oauth-proxy and delegate tailnet exposure to the cluster's Tailscale Operator. The proxy becomes a single-container pod with zero secrets.

This spec covers the Rust code refactor and Kubernetes manifest changes within this repo. It does NOT cover ArgoCD management — that is a separate spec executed in mothership-gitops after this work is deployed and verified.

---

## Boundary

**In scope:** All code and manifest changes in this repo (services/, crates/, k8s/, config).

**Out of scope:** mothership-gitops, ArgoCD Application, sync wave ordering. Those are handled by a separate spec with precondition "Spec A deployed and verified reachable."

**Handoff artifact:** Updated `k8s/` directory with single-container Deployment and Tailscale Operator-annotated Service. The mothership-gitops spec consumes this directory via Kustomize.

---

## Requirements

### R1: Remove tailscaled sidecar dependency

The proxy must run as a single-container pod. It must not depend on a `tailscaled` sidecar, a shared Unix socket, or the `tailscale-localapi` crate for any functionality.

Concrete changes:
- Delete `services/oauth-proxy/src/tailnet.rs`
- Remove `tailscale-localapi` from workspace and service `Cargo.toml`
- Remove the `mod tailnet;` declaration from `main.rs`

### R2: Remove all secrets

The deployment must require zero Kubernetes secrets:

| Current Secret | Disposition |
|----------------|-------------|
| `tailscale-authkey` (TS_AUTHKEY) | Eliminated — Tailscale Operator handles auth |
| `ghcr-pull-secret` (dockerconfigjson) | Eliminated — container image is public |

Concrete changes:
- Delete `k8s/secret.yaml`
- Remove `imagePullSecrets` from `k8s/deployment.yaml`
- Remove auth key resolution logic from `config.rs` (TS_AUTHKEY env var, auth_key_file)
- Remove `Secret<T>` usage from config if no longer needed

### R3: Tailnet exposure via Tailscale Operator

The proxy's Kubernetes Service must be exposed on the tailnet by the Tailscale Operator. The MagicDNS hostname must remain `anthropic-oauth-proxy` so existing Aperture routing continues to work.

Concrete changes to `k8s/service.yaml` — annotate for operator exposure:
```yaml
metadata:
  annotations:
    tailscale.com/expose: "true"
    tailscale.com/hostname: "anthropic-oauth-proxy"
```

Service remains `type: ClusterIP`. The Tailscale Operator creates a StatefulSet that proxies from the tailnet to the Service.

> **Note:** This is a new pattern in the cluster. Existing Tailscale Operator usage falls into two other categories: egress (`tailscale.com/tailnet-fqdn` on proxmox-egress) for reaching tailnet hosts from inside the cluster, and Ingress (`ingressClassName: tailscale` on homarr, longhorn, etc.) for browser-accessible HTTP services. The proxy needs neither — it's a headless service that must be reachable by other tailnet nodes (Aperture) directly. The `expose` annotation is the correct mechanism for this.

### R4: Core proxy functionality unchanged

Header injection, request proxying, response streaming, error handling, body size limits, and upstream retry behavior must not change. The proxy's core job — injecting `anthropic-beta: oauth-2025-04-20` and forwarding to `api.anthropic.com` — is unaffected by this migration.

No changes to `proxy.rs`.

### R5: Health endpoint adaptation

The `/health` endpoint currently reports tailnet identity (hostname, IP, connected status) obtained from the sidecar. Without the sidecar, the health endpoint must still return 200 when the proxy is ready to serve.

New health response (200):
```json
{
  "status": "healthy",
  "uptime_seconds": 3600,
  "requests_served": 12345,
  "errors_total": 0
}
```

The `tailnet`, `tailnet_hostname`, and `tailnet_ip` fields are removed. The health endpoint returns 200 when the HTTP listener is bound. There is no 503 "degraded" state — without the sidecar, the proxy is either up or not.

### R6: State machine simplification

The service state machine currently transitions through `ConnectingTailnet` on startup. Without the sidecar, the startup lifecycle is:

```
Initializing → Starting → Running → Draining → Stopped
```

Remove from `service.rs`:
- `ConnectingTailnet` state
- `TailnetConnected` / `TailnetError` events
- `ConnectTailnet` action
- `RetryTimer` event (only used for tailnet retry)
- `ScheduleRetry` action
- `ErrorOrigin` enum (only variant was `Tailnet`)
- `Error` state (only used for tailnet retry)
- `TailnetHandle` struct
- `MAX_TAILNET_RETRIES` constant

New state machine:
```
Initializing + ConfigLoaded → Starting (action: StartListener)
Starting + ListenerReady → Running (action: None)
Running + ShutdownSignal → Draining (action: None)
Draining + DrainTimeout → Stopped (action: Shutdown(0))
Any + ShutdownSignal → Stopped (action: Shutdown(0))
Stopped + Any → Stopped (action: None)
```

Update `main.rs` to match: remove tailnet connection orchestration, retry loop, backoff logic. After config loads, go directly to binding the listener.

### R7: Metrics continuity

Remove the `tailnet_connected` gauge — it has no meaning without the sidecar.

The remaining metrics are unaffected:
- `proxy_requests_total{status, method}` (counter)
- `proxy_request_duration_seconds{status}` (histogram)
- `proxy_upstream_errors_total{error_type}` (counter)

Concrete changes:
- Remove `set_tailnet_connected()` from `metrics.rs`
- Remove calls to `set_tailnet_connected()` in `main.rs`

### R8: Configuration cleanup

Remove the `[tailscale]` config section entirely. The config becomes:

```toml
[proxy]
listen_addr = "0.0.0.0:8080"
upstream_url = "https://api.anthropic.com"
timeout_secs = 60
max_connections = 1000

[[headers]]
name = "anthropic-beta"
value = "oauth-2025-04-20"
```

Concrete changes:
- Remove `TailscaleConfig` struct from `config.rs`
- Remove `tailscale` field from `Config` struct
- Remove auth key resolution logic
- Remove `TAILSCALE_SOCKET` env var handling from `main.rs`
- Update `k8s/configmap.yaml` to match
- Update `anthropic-oauth-proxy.example.toml` to match

### R9: Kubernetes manifest updates

**`k8s/deployment.yaml`:**
- Single container (proxy only) — remove entire `tailscaled` container
- Remove `imagePullSecrets` (image is public)
- Remove `TAILSCALE_SOCKET` env var from proxy container
- Remove shared volumes: `tailscale-socket`, `tailscale-state`
- Keep: config volume, security context, probes, resources, prometheus annotations

**`k8s/service.yaml`:**
- Add Tailscale Operator annotations (see R3)

**`k8s/configmap.yaml`:**
- Remove `[tailscale]` section from config.toml (see R8)

**`k8s/kustomization.yaml`:**
- Remove commented `secret.yaml` reference and imperative creation instructions

### R12: macOS development parity

Without the sidecar dependency, local development is simpler. The proxy listens on localhost without tailnet integration. `cargo run` with a config pointing `listen_addr` at `127.0.0.1:8080` works immediately.

The macOS-specific code paths in `tailnet.rs` (CLI fallback, socket discovery) are deleted along with the file.

---

## Out of Scope

- ArgoCD Application (Spec B — mothership-gitops)
- Migration ordering and zero-downtime sequencing (Spec B)
- Changes to Aperture configuration
- Changes to Tailscale ACLs
- Multi-replica deployment (remains single replica)
- Persistent storage
- Web UI or Tailscale Ingress

---

## Success Criteria

- [x] Single-container pod (no sidecar) in `k8s/deployment.yaml`
- [x] Zero secrets in the deployment
- [x] Service annotated for Tailscale Operator exposure with hostname `anthropic-oauth-proxy`
- [x] `tailscale-localapi` removed from `Cargo.toml`
- [x] `tailnet.rs` deleted
- [x] State machine has no tailnet states (no `ConnectingTailnet`, no `Error`, no retry logic)
- [x] Health endpoint returns 200 with no tailnet fields
- [x] `tailnet_connected` metric removed
- [x] Config has no `[tailscale]` section
- [x] All tests pass (`cargo test --workspace`)
- [x] `cargo clippy --workspace -- -D warnings` clean
- [x] Local `cargo run` works on macOS without tailscaled

---

## Test Impact

Tests that must be updated or removed:
- `tailnet.rs` tests — deleted with the file (3 tests)
- `config.rs` tests referencing `TailscaleConfig`, `auth_key`, `TS_AUTHKEY` — rewrite for simplified config
- `service.rs` tests for `ConnectingTailnet`, `TailnetConnected`, `TailnetError`, `RetryTimer`, `ErrorOrigin` — rewrite for simplified state machine
- `main.rs` health endpoint tests — update expected JSON (no tailnet fields)
- `metrics.rs` `set_tailnet_connected` test — delete

Tests that are unaffected:
- All `proxy.rs` tests (header injection, body limits, streaming, retries)
- `metrics.rs` tests for `record_request`, `record_upstream_error`, histogram buckets
- `config.rs` tests for `resolve_path`, validation (upstream_url, timeout, max_connections)
- `service.rs` tests for `Starting→Running`, `Running→Draining`, `Draining→Stopped`, shutdown signals

---

## References

- `specs/oauth-proxy.md` — Current proxy spec (Phase 3: Tailnet Integration being retired)
- `specs/tailnet.md` — Tailnet integration strategy (Option B being retired)
- mothership-gitops `specs/operator-migration-gitops.md` — Companion spec for ArgoCD adoption (Spec B)
