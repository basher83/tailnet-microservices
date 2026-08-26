# Client Semantics (Pi / OpenClaw)

> **Operational Runbook** · [Index](./README.md) · [Deployment](./deployment.md) · [Accounts](./accounts.md) · [Monitoring](./monitoring.md) · [Troubleshooting](./troubleshooting.md) · [Clients](./clients.md) · [Header Parity](./header-parity.md)

## Pi / OpenClaw Client Semantics

Future-me intent: OpenAI Codex stays the default Pi model. Claude Max is available on demand through a separate Pi provider named `anthropic-proxy`; do not point Pi's built-in `anthropic` provider at this gateway unless you intentionally want proxy-Claude to become the default Anthropic path.

Local Pi config lives outside this repo:

- `~/.pi/agent/models.json` defines provider `anthropic-proxy`
- `~/.pi/agent/settings.json` enables `anthropic-proxy/*` models while keeping the default model on `openai-codex/gpt-5.5`

The provider shape is intentionally Anthropic-compatible but uses a fake OAuth-looking key:

```json
{
  "providers": {
    "anthropic-proxy": {
      "type": "anthropic",
      "baseUrl": "https://anthropic-oauth-proxy.tailfb3ea.ts.net",
      "apiKey": "sk-ant-oat-proxy-placeholder"
    }
  }
}
```

The fake `sk-ant-oat-*` key is not a secret. It is a client-shaping hint for Pi/pi-ai: Pi emits Claude-Code/OAuth-style request headers, beta flags, user-agent, and tool naming. In OAuth mode the proxy strips/replaces client `Authorization` and injects the real Claude Max bearer token from the credential pool, so clients should not send real Anthropic API keys to this gateway.

Use proxy-Claude explicitly from Pi:

```bash
pi -p \
  --provider anthropic-proxy \
  --model claude-haiku-4-5 \
  --thinking off \
  --no-tools \
  --no-context-files \
  'Reply exactly: ok'
```

Expected smoke-test result:

```text
ok
```

A useful routing check is the health request counter:

```bash
before=$(curl -fsS https://anthropic-oauth-proxy.tailfb3ea.ts.net/health | jq -r .requests_served)
pi -p --provider anthropic-proxy --model claude-haiku-4-5 --thinking off --no-tools --no-context-files 'Reply exactly: ok'
after=$(curl -fsS https://anthropic-oauth-proxy.tailfb3ea.ts.net/health | jq -r .requests_served)
echo "before=$before after=$after"
```

For Pi/OpenClaw traffic, the proxy does more than token injection:

- injects Claude Code attribution via `x-anthropic-billing-header`
- injects required OAuth Anthropic beta/version/user-agent headers
- prepends the required Claude Code system prompt prefix when absent
- removes Pi's local documentation-routing hint from system prompts, because Anthropic's Max-plan classifier treats that hint as extra-usage traffic even with Claude Code attribution

If Pi returns:

```text
400 You're out of extra usage. Add more at claude.ai/settings/usage and keep going.
```

then the request was classified as billable API/extra usage rather than Claude Max plan usage. Check that ArgoCD has deployed an image containing the billing-header and Pi prompt-sanitizer fixes, then rerun the smoke test above. A raw `curl` request may still succeed while Pi fails if only the Pi-specific system prompt sanitizer is missing.

Do not use `claude -p --bare` as a validation substitute. `--bare` bypasses Claude Code's normal OAuth/keychain path and can fail with `Not logged in` even when regular Claude Code and the proxy are healthy.

## Authentication Mode

The committed Kubernetes config in `k8s/config.toml` runs OAuth mode by default. The `[oauth]` and `[admin]` sections are active, and `[[headers]]` is ignored automatically because `[oauth]` takes precedence. To run passthrough mode instead, remove or comment the `[oauth]` and `[admin]` sections and keep the `[[headers]]` section. Commit and push to `main`; ArgoCD will roll out the ConfigMap change.

OAuth mode starts with whatever accounts exist in `/data/credentials.json` on the PVC. An empty credential file is valid, but the pool is unhealthy until an account is loaded. The preferred provisioning path is the PKCE admin flow, with keychain extraction as fallback; PKCE works as of 2026-08-26 (three request-shape bugs fixed, not policy — see [Known Issues](./troubleshooting.md#pkce-web-flow-failed-on-request-shape-not-policy-fixed-2026-08-26)) and is the preferred provisioning path.

## When the Client Gets 503 `pool_exhausted`

If Pi (or any client) returns:

```text
Error: 503 {"error":{"message":"provider error: pool exhausted: {\"error\":{\"message\":\"All accounts exhausted\",\"pool\":{\"accounts_available\":0,\"accounts_cooling_down\":0,\"accounts_disabled\":1,\"accounts_total\":1},\"type\":\"pool_exhausted\"}}","request_id":"req_…","type":"proxy_error"}}
```

nothing is wrong with the client or its config. Read the embedded `pool` summary: `accounts_disabled ≥ 1` means the proxy's refresh token for that account has expired or been invalidated, and the account will stay disabled until someone re-auths it — this does not clear on its own, and Pi retries will keep failing identically. Fix: re-extract fresh tokens from a *working* local Claude Code login and reload them ([Accounts → Keychain Extraction](./accounts.md#adding-an-account-keychain-extraction)), then rerun the smoke test above. `accounts_cooling_down ≥ 1` with `accounts_disabled = 0` is the other case — quota, which clears by itself after the cooldown (default 2 h).

Because the extracted credential is the same OAuth grant your local Claude Code uses, keep that local login healthy: if you `claude /logout` or let it lapse, the proxy's copy dies with it at the next refresh.

