# Spec Addendum: Operator Migration — Traffic Routing

**Status:** Complete — Ingress-only tailnet exposure, live cluster checks still open
**Created:** 2026-02-08
**Relates to:** Spec A (operator-migration.md v0.0.112)
**Scope:** k8s/ directory only

---

## Executive Summary

Spec A (operator migration) successfully removed the `tailscaled` sidecar, but its initial Service-annotation exposure model was not the final routing shape. During cluster deployment, the annotated Service and the Tailscale Ingress each created a proxy device claiming `anthropic-oauth-proxy`, producing a dual-proxy conflict.

The current manifest shape is Ingress-only tailnet exposure: `k8s/service.yaml` is a plain ClusterIP Service with no Tailscale annotations, and `k8s/ingress.yaml` uses `ingressClassName: tailscale` with `tls.hosts: [anthropic-oauth-proxy]`. The Ingress owns the MagicDNS hostname and routes tailnet HTTP traffic to the Service.

The source manifests are complete. The remaining unchecked criteria require live cluster evidence: only one Tailscale proxy pod, Ingress routing to the Service, tailnet `/health` returning 200, and upstream proxy requests completing.

---

## The Gap

**Historical state after Spec A:**
- Service had `tailscale.com/expose: "true"` and `tailscale.com/hostname: "anthropic-oauth-proxy"` annotations
- Tailscale Operator created a StatefulSet that joined the tailnet
- The annotated Service claimed `anthropic-oauth-proxy` on the tailnet via MagicDNS

**What was missing:**
- The Tailscale Operator StatefulSet existed but had no Tailscale Serve rules
- No inbound HTTP traffic routing from the tailnet to the Service ClusterIP:80
- Callers attempting `anthropic-oauth-proxy:80` or `anthropic-oauth-proxy:8080` on the tailnet failed to reach the backend

**First root cause:**
The `expose: "true"` annotation provides tailnet identity only. Traffic routing requires explicit Ingress rules. (This is different from the sidecar era, where the sidecar daemon itself accepted connections and forwarded them to localhost:8080.)

**Final root cause:**
Keeping both the annotated Service and the Tailscale Ingress creates two Tailscale proxy devices for the same hostname. The final manifests remove Service annotations and let the Ingress own tailnet exposure exclusively.

---

## The Fix

Add a Tailscale Ingress resource to the k8s/ directory and keep the Service unannotated. The Ingress will:
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

### Resource: k8s/ingress.yaml

The Tailscale Operator derives the MagicDNS hostname from `tls[0].hosts[0]`, not from `rules[].host`. The deprecated `kubernetes.io/ingress.class` annotation is omitted in favor of `ingressClassName`.

```yaml
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: anthropic-oauth-proxy
  namespace: anthropic-oauth-proxy
spec:
  ingressClassName: tailscale
  tls:
    - hosts:
        - anthropic-oauth-proxy
  rules:
    - http:
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

`ingress.yaml` is included in the resources list. Current kustomization also includes the OAuth credential PVC and admin Service:

```yaml
apiVersion: kustomize.config.k8s.io/v1beta1
kind: Kustomization
resources:
  - namespace.yaml
  - serviceaccount.yaml
  - pvc.yaml
  - deployment.yaml
  - service.yaml
  - admin-service.yaml
  - ingress.yaml
```

---

## How It Works

1. **Service:** plain ClusterIP `anthropic-oauth-proxy:80` → Pod port 8080
2. **Ingress:** Tailnet hostname `anthropic-oauth-proxy` → Service ClusterIP:80
3. **Tailscale Operator:** Creates or updates Serve rules to forward tailnet HTTP traffic to the Ingress controller
4. **Result:** Callers on the tailnet can reach the proxy via `https://anthropic-oauth-proxy`

Traffic path:
```text
Tailnet client → Tailscale Operator pod (Serve rule) → K8s Ingress controller → Service ClusterIP:80 → Proxy pod:8080
```

---

## Additional Fix: Remove Service Expose Annotations

**Problem discovered during cluster deployment:** The `tailscale.com/expose: "true"` annotation on `k8s/service.yaml` creates its own Tailscale proxy pod (dm75l). The Ingress also creates a Tailscale proxy pod (mrcz2). Both claim the same `anthropic-oauth-proxy` hostname, causing a dual-proxy conflict.

**Fix:** Remove `tailscale.com/expose` and `tailscale.com/hostname` annotations from `k8s/service.yaml`. The Ingress now handles tailnet exposure — the Service annotations are redundant and create a conflicting device.

### Updated: k8s/service.yaml

```yaml
apiVersion: v1
kind: Service
metadata:
  name: anthropic-oauth-proxy
  namespace: anthropic-oauth-proxy
  labels:
    app: anthropic-oauth-proxy
spec:
  type: ClusterIP
  selector:
    app: anthropic-oauth-proxy
  ports:
    - name: http
      port: 80
      targetPort: http
      protocol: TCP
```

The annotations block is removed entirely. The Service remains a plain ClusterIP service; tailnet exposure is handled exclusively by the Ingress.

---

## Out of Scope

- Changes to Deployment or config
- Aperture routing (unchanged)
- ArgoCD Application or sync waves
- Multi-replica or persistent storage

---

## Success Criteria

- [x] `k8s/ingress.yaml` created with Tailscale Ingress definition
- [x] `k8s/kustomization.yaml` updated to include ingress.yaml
- [x] `kubectl kustomize k8s/` validates successfully
- [x] `k8s/service.yaml` — remove `tailscale.com/expose` and `tailscale.com/hostname` annotations
- [ ] Only one Tailscale proxy pod exists for `anthropic-oauth-proxy` (the Ingress one)
- [ ] Ingress resolves to the Service ClusterIP (requires cluster deployment)
- [ ] HTTP GET to `https://anthropic-oauth-proxy/health` from tailnet returns 200 (requires cluster deployment)
- [ ] Upstream proxy requests (to api.anthropic.com) complete successfully (requires cluster deployment)

---

## References

- `specs/operator-migration.md` — Historical sidecar removal spec; original Service-annotation plan is superseded by this addendum
- `k8s/service.yaml` — Current plain ClusterIP Service with no Tailscale annotations
- `k8s/ingress.yaml` — Current Tailscale Ingress that owns tailnet exposure
- Cluster pattern: mothership-gitops AppProject (homarr, argocd, longhorn, netdata all use `ingressClassName: tailscale`)
