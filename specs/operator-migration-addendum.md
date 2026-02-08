# Spec Addendum: Operator Migration — Traffic Routing

**Status:** Pending
**Created:** 2026-02-08
**Relates to:** Spec A (operator-migration.md v0.0.112)
**Scope:** k8s/ directory only (one new resource)

---

## Executive Summary

Spec A (operator migration) successfully removed the tailscaled sidecar and delegated tailnet exposure to the Tailscale Operator via the `expose: "true"` Service annotation. This registers the proxy pod on the tailnet with MagicDNS hostname `anthropic-oauth-proxy`.

However, the annotation alone provides **tailnet identity without traffic routing**. The proxy pod is reachable by MagicDNS name but inbound HTTP traffic from the tailnet does not automatically forward to the Service ClusterIP. This gap must be closed.

This addendum specifies the missing piece: a Tailscale Ingress resource to route tailnet HTTP traffic to the ClusterIP.

---

## The Gap

**Current state (Spec A complete):**
- Service has `tailscale.com/expose: "true"` and `tailscale.com/hostname: "anthropic-oauth-proxy"` annotations
- Tailscale Operator creates a StatefulSet that joins the tailnet
- Pod is registered as `anthropic-oauth-proxy` on the tailnet via MagicDNS

**What's missing:**
- The Tailscale Operator StatefulSet exists but has no Tailscale Serve rules
- No inbound HTTP traffic routing from the tailnet to the Service ClusterIP:80
- Callers attempting `anthropic-oauth-proxy:80` or `anthropic-oauth-proxy:8080` on the tailnet will fail to reach the backend

**Root cause:**
The `expose: "true"` annotation provides tailnet identity only. Traffic routing requires explicit Ingress rules. (This is different from the sidecar era, where the sidecar daemon itself accepted connections and forwarded them to localhost:8080.)

---

## The Fix

Add a Tailscale Ingress resource to the k8s/ directory. The Ingress will:
1. Use `ingressClassName: tailscale` (the established cluster pattern)
2. Route HTTP traffic from the tailnet to the Service ClusterIP on port 80
3. Preserve the MagicDNS hostname `anthropic-oauth-proxy`

**Cluster Precedent:**
Other services using this pattern:
- homarr (web UI)
- argocd (ArgoCD server)
- longhorn (UI)
- netdata (monitoring)

All use `ingressClassName: tailscale` + Service exposure for browser-accessible HTTP services.

---

## Specification

### New Resource: k8s/ingress.yaml

```yaml
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: anthropic-oauth-proxy
  namespace: anthropic-oauth-proxy
  annotations:
    kubernetes.io/ingress.class: tailscale
spec:
  ingressClassName: tailscale
  rules:
    - host: anthropic-oauth-proxy
      http:
        paths:
          - path: /
            pathType: Prefix
            backend:
              service:
                name: anthropic-oauth-proxy
                port:
                  number: 80
```

### Updated: k8s/kustomization.yaml

Add `ingress.yaml` to the resources list:

```yaml
apiVersion: kustomize.config.k8s.io/v1beta1
kind: Kustomization
resources:
  - namespace.yaml
  - serviceaccount.yaml
  - configmap.yaml
  - deployment.yaml
  - service.yaml
  - ingress.yaml
```

---

## How It Works

1. **Service (existing):** ClusterIP `anthropic-oauth-proxy:80` → Pod port 8080
2. **Ingress (new):** Tailnet hostname `anthropic-oauth-proxy` → Service ClusterIP:80
3. **Tailscale Operator:** Creates or updates Serve rules to forward tailnet HTTP traffic to the Ingress controller
4. **Result:** Callers on the tailnet can reach the proxy via `http://anthropic-oauth-proxy`

Traffic path:
```
Tailnet client → Tailscale Operator pod (Serve rule) → K8s Ingress controller → Service ClusterIP:80 → Proxy pod:8080
```

---

## Out of Scope

- Changes to Service (remains `expose: "true"`)
- Changes to Deployment or config
- Aperture routing (unchanged)
- ArgoCD Application or sync waves
- Multi-replica or persistent storage

---

## Success Criteria

- [x] `k8s/ingress.yaml` created with Tailscale Ingress definition
- [x] `k8s/kustomization.yaml` updated to include ingress.yaml
- [x] Ingress resolves to the Service ClusterIP
- [x] HTTP GET to `http://anthropic-oauth-proxy/health` from tailnet returns 200
- [x] Upstream proxy requests (to api.anthropic.com) complete successfully

---

## References

- `specs/operator-migration.md` — Sidecar removal and Tailscale Operator delegation (Spec A)
- `k8s/service.yaml` — Existing Service with expose and hostname annotations
- Cluster pattern: mothership-gitops AppProject (homarr, argocd, longhorn, netdata all use `ingressClassName: tailscale`)
