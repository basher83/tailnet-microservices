# Deployment & Operations

> **Operational Runbook** · [Index](./README.md) · [Deployment](./deployment.md) · [Accounts](./accounts.md) · [Monitoring](./monitoring.md) · [Troubleshooting](./troubleshooting.md) · [Clients](./clients.md) · [Header Parity](./header-parity.md)

## How Deployments Work

Runtime-affecting commits to `main` trigger `.github/workflows/ci.yml`, which runs Rust lint, audit, test, and release build before building a container image and pushing it to GHCR as `sha-<7char>`. The deploy job then updates `k8s/kustomization.yaml` with the new tag and pushes a `[skip ci]` commit to `main`.

Changes under `k8s/` trigger the separate `.github/workflows/manifests.yml` workflow, which only renders and validates the manifests. A commit containing both runtime and Kubernetes changes triggers both workflows. Markdown-only changes trigger neither workflow.

ArgoCD watches `main` at path `k8s/` with automated sync, prune, and self-heal enabled. It reconciles direct manifest changes as well as image-tag commits produced by the runtime pipeline.

```text
runtime commit on main
  → CI required by docker: lint + audit + test + release build
  → CI docker job: build + push to ghcr.io (tagged sha-<7char>)
  → CI deploy job: update kustomization.yaml newTag, commit [skip ci]
  → ArgoCD: auto-sync from main, path k8s/

k8s-only commit on main
  → Kubernetes Manifests: render + validate
  → ArgoCD: auto-sync from main, path k8s/
```

The `newTag` field in `k8s/kustomization.yaml` is machine-managed by CI. Do not edit it manually — the next CI run will overwrite it.

## Bootstrap Deploy

For first-time setup before ArgoCD is configured, or to apply manifests directly:

```bash
kubectl apply -k k8s/
```

No secrets are required. The container image is public on GHCR (anonymous pull). Tailnet authentication is handled by the Tailscale Operator. This creates the namespace, ServiceAccount, ConfigMap, PVC, Deployment, Services (proxy + admin), and Ingress. The Tailscale Operator detects the Ingress and creates a StatefulSet to proxy from the tailnet to the ClusterIP.

## Verify Deployment

```bash
kubectl -n anthropic-oauth-proxy get pods
kubectl -n anthropic-oauth-proxy logs deployment/anthropic-oauth-proxy
```

A healthy startup sequence in the logs (JSON structured):

```text
{"message":"starting anthropic-oauth-proxy",...}
{"message":"loading configuration","path":"/etc/anthropic-oauth-proxy/config.toml",...}
{"message":"configuration loaded","listen_addr":"0.0.0.0:8080",...}
{"message":"state: Starting",...}
{"message":"state: Running — accepting requests","addr":"0.0.0.0:8080",...}
```

Verify the Tailscale Operator created its proxy StatefulSet:

```bash
kubectl -n anthropic-oauth-proxy get statefulset
```

## End-to-End Test

Test the full request path over the tailnet. Requests must use HTTPS against the Tailscale FQDN (not the short MagicDNS name, which lacks a valid TLS cert for curl). No credentials are needed in the request — the proxy injects everything.

```bash
curl -s -X POST "https://anthropic-oauth-proxy.tailfb3ea.ts.net/v1/messages" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-haiku-4-5-20251001",
    "max_tokens": 64,
    "messages": [{"role": "user", "content": "Say hello in exactly 5 words."}]
  }' | jq .
```

A successful response contains `"type": "message"` with a `content` array and `usage` block. The request path is:

```text
curl → tailnet (WireGuard) → Tailscale Ingress (TLS termination)
     → Service (ClusterIP :80) → proxy (:8080, OAuth token injection)
     → api.anthropic.com → 200 OK
```

Common failures at this stage:

| Symptom | Cause |
|---------|-------|
| Connection refused on port 80 | Using `http://` instead of `https://`, or using the short MagicDNS name without port 443 |
| TLS handshake error | Using the short name `anthropic-oauth-proxy` instead of the FQDN `anthropic-oauth-proxy.tailfb3ea.ts.net` |
| 400 from Cloudflare | Request reached Anthropic but was malformed — check proxy logs for the request_id |
| 401 Unauthorized | Token expired or invalid — check `curl -s http://localhost:9090/admin/pool \| jq .` (requires admin port-forward) |
| 503 Service Unavailable | Pool exhausted — no available accounts |


## Updating Configuration

The ConfigMap is generated from `k8s/config.toml` by kustomize. To change configuration, edit the file, commit, and push to `main`. ArgoCD detects the ConfigMap hash change and triggers a rollout.

To force a restart without a config change (e.g., to pick up refreshed credentials from the PVC):

```bash
kubectl -n anthropic-oauth-proxy rollout restart deployment/anthropic-oauth-proxy
```

ArgoCD will not revert a manual restart — the Deployment spec hasn't changed, only the pod template annotation has.

## Rollback

ArgoCD's self-heal will revert manual `rollout undo` commands within seconds. To roll back, revert the code commit on `main` and push. CI builds the previous code, updates the image tag, and ArgoCD syncs the rollback.

```bash
git revert HEAD
git push origin main
```

For emergencies where you need to act faster than the CI pipeline, disable ArgoCD auto-sync first:

```bash
kubectl -n argocd patch application anthropic-oauth-proxy --type merge -p '{"spec":{"syncPolicy":null}}'
kubectl -n anthropic-oauth-proxy rollout undo deployment/anthropic-oauth-proxy
```

Re-enable auto-sync after the situation is resolved. Any manual state will be overwritten when sync resumes.


## Graceful Shutdown

On SIGTERM (Kubernetes pod termination), the proxy stops accepting new connections and waits for in-flight requests to complete. The `in_flight` atomic counter tracks active requests. The proxy enforces a 5-second `DRAIN_TIMEOUT` starting from when it receives the signal. If in-flight requests complete within 5 seconds, shutdown is clean. If not, the proxy force-exits after 5 seconds regardless of the Kubernetes `terminationGracePeriodSeconds`.

The shutdown sequence logged:

```text
{"message":"received SIGTERM, shutting down",...}
{"message":"all in-flight requests drained",...}
{"message":"shutdown complete",...}
```

If requests are still in flight when the 5-second drain timeout expires:

```text
{"message":"drain timeout exceeded, forcing shutdown","remaining":3,"drain_timeout_secs":5,...}
```

## Resource Limits

Default resource configuration from `k8s/deployment.yaml`:

| Container | CPU request | CPU limit | Memory request | Memory limit |
|-----------|-------------|-----------|----------------|--------------|
| proxy | 50m | 500m | 32Mi | 128Mi |

The proxy binary is approximately 5MB and has minimal memory overhead. Increase memory limits if serving large request/response bodies concurrently, though the 10 MiB body size limit provides a natural ceiling.

