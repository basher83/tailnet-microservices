# Spec: Tailscale Operator Migration

**Status:** Draft
**Created:** 2026-02-07
**Author:** Brent + Claude

---

## Overview

Remove the `tailscaled` sidecar from the anthropic-oauth-proxy and delegate tailnet exposure to the cluster's Tailscale Operator. The proxy becomes a single-container pod with zero secrets, managed by ArgoCD in mothership-gitops.

---

## Context

The proxy currently runs a `tailscaled` sidecar container that independently joins the tailnet using its own auth key. The Rust service queries the sidecar via `tailscale-localapi` over a Unix socket to obtain tailnet identity (hostname, IP) for health reporting and metrics.

The talos-prod-01 cluster already runs a Tailscale Operator (sync wave 4 in mothership-gitops) that manages tailnet exposure for all other services. The sidecar approach is an outlier that requires its own auth key secret and adds operational complexity.

---

## Requirements

### R1: Remove tailscaled sidecar dependency

The proxy must run as a single-container pod. It must not depend on a `tailscaled` sidecar, a shared Unix socket, or the `tailscale-localapi` crate for any functionality.

### R2: Remove all secrets

The deployment must require zero Kubernetes secrets:

| Current Secret | Disposition |
|----------------|-------------|
| `tailscale-authkey` (TS_AUTHKEY) | Eliminated — Tailscale Operator handles auth |
| `ghcr-pull-secret` (dockerconfigjson) | Eliminated — container image is public |

### R3: Tailnet exposure via Tailscale Operator

The proxy's Kubernetes Service must be exposed on the tailnet by the Tailscale Operator. The MagicDNS hostname must remain `anthropic-oauth-proxy` so existing Aperture routing continues to work without reconfiguration.

### R4: Core proxy functionality unchanged

Header injection, request proxying, response streaming, error handling, body size limits, and upstream retry behavior must not change. The proxy's core job — injecting `anthropic-beta: oauth-2025-04-20` and forwarding to `api.anthropic.com` — is unaffected by this migration.

### R5: Health endpoint adaptation

The `/health` endpoint currently reports tailnet identity (hostname, IP, connected status) obtained from the sidecar. Without the sidecar, the health endpoint must still function and report meaningful status, but it must not fail or degrade due to the absence of a `tailscaled` socket.

### R6: State machine simplification

The service state machine currently transitions through `ConnectingTailnet` on startup, which depends on querying the sidecar. Without the sidecar, the startup lifecycle must not block on tailnet connectivity. The proxy is ready to serve when its HTTP listener binds.

### R7: Metrics continuity

The `tailnet_connected` Prometheus gauge currently reflects sidecar connectivity. This metric must either be removed or repurposed to reflect a meaningful signal. The remaining metrics (`proxy_requests_total`, `proxy_request_duration_seconds`, `proxy_upstream_errors_total`) are unaffected.

### R8: Configuration cleanup

The `[tailscale]` config section (`hostname`, `auth_key`, `auth_key_file`, `state_dir`) and the `TAILSCALE_SOCKET` environment variable are sidecar-specific. Configuration must not require fields that serve no purpose.

### R9: Kubernetes manifest updates

The `k8s/` manifests must reflect the single-container deployment:

- Single container (proxy only)
- No `imagePullSecrets`
- No shared volumes for sidecar communication (`tailscale-socket`, `tailscale-state`)
- Service annotated for Tailscale Operator exposure
- ConfigMap updated to match the simplified configuration

### R10: ArgoCD GitOps management

After this migration, the deployment must be managed by ArgoCD via mothership-gitops:

- ArgoCD Application in mothership-gitops pointing to this repo's `k8s/` Kustomize directory
- Added to `apps/root.yaml` with a sync wave after the Tailscale Operator (wave 4)
- Automated sync with prune and self-heal
- No ExternalSecrets required (zero secrets)

### R11: Zero-downtime migration

The migration must not break existing Aperture routing. The proxy must remain reachable at `anthropic-oauth-proxy` on the tailnet throughout the transition.

### R12: macOS development parity

The proxy must continue to work for local development on macOS. Currently the macOS code path falls back to `tailscale status --json` CLI. Without the sidecar dependency, local development should be simpler — the proxy listens on localhost without tailnet integration.

---

## Out of Scope

- Changes to Aperture configuration
- Changes to Tailscale ACLs
- Multi-replica deployment (remains single replica)
- Adding persistent storage
- Adding a web UI or Tailscale Ingress (this is a headless service)

---

## Success Criteria

- [ ] Single-container pod running in `anthropic-oauth-proxy` namespace
- [ ] Zero secrets in the deployment
- [ ] Reachable at `anthropic-oauth-proxy` on the tailnet via Tailscale Operator
- [ ] Aperture routes to it without reconfiguration
- [ ] Claude Max OAuth tokens work end-to-end (same as current)
- [ ] Health endpoint returns 200 when proxy is ready to serve
- [ ] All existing proxy metrics (requests, duration, errors) continue to emit
- [ ] `tailscale-localapi` crate removed from dependencies
- [ ] ArgoCD Application synced and healthy in mothership-gitops
- [ ] Local `cargo run` works on macOS without tailscaled

---

## References

- `specs/oauth-proxy.md` — Current proxy spec (Phase 3: Tailnet Integration)
- `specs/tailnet.md` — Tailnet integration strategy (Option B being retired)
- mothership-gitops `apps/root.yaml` — App of Apps sync wave ordering
- mothership-gitops `apps/homarr/proxmox-egress.yaml` — Tailscale Operator Service exposure pattern
