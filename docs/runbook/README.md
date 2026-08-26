# Anthropic OAuth Proxy — Operational Runbook

This runbook covers deployment, operation, monitoring, and troubleshooting of the anthropic-oauth-proxy service. The proxy supports two modes: passthrough (static header injection) and OAuth pool (PKCE auth, token refresh, subscription pooling).

## Runbook index

| Doc | Covers |
|---|---|
| [Deployment & Operations](./deployment.md) | How deploys work, bootstrap, verify, end-to-end test, config updates, rollback, graceful shutdown, resource limits |
| [OAuth Account Management](./accounts.md) | Admin API, adding/removing/listing accounts, refresh-token lifetime & re-auth cadence, pool status, credential persistence |
| [Monitoring & Endpoints](./monitoring.md) | Endpoint reference, health response, Prometheus metrics, alerts, structured logs, token-refresh troubleshooting |
| [Troubleshooting & Known Issues](./troubleshooting.md) | Pod / tailnet / 502 / 504 / 400 / 429 / pool-exhausted / latency, refresh-token expiry, PKCE (dated) + credential-file issues |
| [Client Semantics (Pi / OpenClaw)](./clients.md) | Pi provider setup, smoke tests, auth mode, extra-usage classification |
| [Header Capture & Parity Checks](./header-parity.md) | Keeping injected headers in parity with genuine Claude Code (mitmproxy, drift checks) |

Forensic/analysis records live under [`../audits/`](../audits/): header provenance and the `anthropic-beta` flag analysis.

## Architecture

The pod contains a single container. The Tailscale Operator manages tailnet connectivity via an Ingress resource that creates a proxy StatefulSet routing traffic from the tailnet to the Service ClusterIP.

```text
                    Tailnet
      +----------+         +---------------------+
      | Aperture | ------> | Tailscale Operator   |
      | (http://ai/)       | proxy (StatefulSet)  |
      +----------+         +----------+----------+
                                      |
                                      v
                           +---------------------+       +-----------+
                           | anthropic-oauth-proxy| ----> | Anthropic |
                           | (single container)   |      | API       |
                           +---------------------+       +-----------+
                            MagicDNS: anthropic-oauth-proxy
                            Proxy: 8080  |  Admin: 9090
```

In passthrough mode, the proxy injects the `anthropic-beta: oauth-2025-04-20` header and forwards to `https://api.anthropic.com`. In OAuth mode, it manages Bearer tokens from a pool of Claude Max subscriptions, handles automatic token refresh, and injects the full Anthropic header contract (anthropic-beta, anthropic-version, user-agent, billing attribution marker, system prompt). TLS termination for inbound traffic is handled by the tailnet WireGuard encryption. Outbound TLS to Anthropic uses `reqwest` with `rustls`.


## Quick reference

Health / pool status (HTTP 200 even when unhealthy — read `.status`):

```bash
curl -fsS https://anthropic-oauth-proxy.tailfb3ea.ts.net/health | jq '{status, pool}'
```

Clients getting `503 pool_exhausted` with `accounts_disabled ≥ 1` → the refresh token expired (expect this every ~4–6 weeks); re-auth via keychain extraction ([details](./accounts.md#refresh-token-lifetime-and-re-auth)):

```bash
# on the workstation whose `claude` login is currently working
# → follow accounts.md "Adding an Account (Keychain Extraction)" Steps 1–4
kubectl -n anthropic-oauth-proxy logs deploy/anthropic-oauth-proxy --since=720h \
  | grep -E 'refresh token rejected|refresh succeeded' | sed -n '1p;$p'   # when did it die?
```

End-to-end smoke ([details](./deployment.md#end-to-end-test)):

```bash
curl -s -X POST https://anthropic-oauth-proxy.tailfb3ea.ts.net/v1/messages \
  -H 'Content-Type: application/json' \
  -d '{"model":"claude-haiku-4-5-20251001","max_tokens":64,"messages":[{"role":"user","content":"Say hello in exactly 5 words."}]}' | jq .
```

Admin API (port-forward, [details](./accounts.md#accessing-the-admin-api)):

```bash
kubectl -n anthropic-oauth-proxy port-forward deployment/anthropic-oauth-proxy 9090:9090
```

Restart / ArgoCD-safe rollback ([details](./deployment.md#rollback)):

```bash
kubectl -n anthropic-oauth-proxy rollout restart deployment/anthropic-oauth-proxy
git revert HEAD && git push origin main
```

Header parity check ([details](./header-parity.md)):

```bash
scripts/capture-cc-headers.sh --debug-only
```
