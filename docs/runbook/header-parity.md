# Header Capture & Parity Checks

> **Operational Runbook** · [Index](./README.md) · [Deployment](./deployment.md) · [Accounts](./accounts.md) · [Monitoring](./monitoring.md) · [Troubleshooting](./troubleshooting.md) · [Clients](./clients.md) · [Header Parity](./header-parity.md)

The proxy impersonates Claude Code on the wire: it injects the User-Agent, `x-app`,
`anthropic-version` and the `anthropic-beta` flag set that genuine Claude Code sends on
`POST /v1/messages`. It does **not** send an `x-anthropic-billing-header` (retired 2026-09-23, see
below) and strips one if a client supplies it. The injected values are
**hardcoded constants** in `services/oauth-proxy/src/provider_impl.rs` and drift out of parity
every time Claude Code updates. This doc is the operational procedure for detecting and closing
that drift.

Forensic records (the *what/why*, not the *how*):
- [`../audits/header-provenance.md`](../audits/header-provenance.md) — where each constant came from, plus on-wire captures.
- [`../audits/anthropic-beta-flags.md`](../audits/anthropic-beta-flags.md) — per-flag cause/effect for the 10 `anthropic-beta` flags.

## Constants that must stay in parity

| Constant (`provider_impl.rs`) | Mirrors CC wire header |
|---|---|
| `USER_AGENT` | `user-agent` |
| `X_APP` | `x-app` |
| `ANTHROPIC_VERSION` | `anthropic-version` |
| `REQUIRED_BETA_FLAGS` | `anthropic-beta` |
| *(none)* | `x-anthropic-billing-header` — **not sent**; client-supplied values are stripped |

**Retired constant (2026-09-23).** `ANTHROPIC_BILLING_HEADER` (`cc_version=2.1.198.bb7; …`) was
injected from May to September 2026. Genuine Claude Code never sent it as a header; since 2.1.280 the
same string travels as the first `system` block with conversation-derived digests. A present/absent
A/B through the real seam against `claude-fable-5-1` returned HTTP 200 both ways, so the injection
was removed rather than bumped or mirrored. Decision and evidence: Lab Operations `incidents/2026/INC-2026-001/references/D003-attribution-decision.md`. The
`~/.pi/agent/models.json` client-side mirror is now redundant; the proxy strips it either way.

## Fast drift check (no mitmproxy)

The outage-relevant signal is **`USER_AGENT` drift**: Anthropic enforces per-model Claude Code
minimum versions on the `user-agent` header, and a stale value produces HTTP 400
`claude_code_version_too_old` for newer models (see [Troubleshooting](./troubleshooting.md)). The
script also prints Claude Code's `--debug-file` attribution line for reference:

```bash
mise run headers:capture          # wraps scripts/capture-cc-headers.sh
# cc_version only, fastest:
scripts/capture-cc-headers.sh --debug-only
```

This prints the live attribution line and the proxy's `USER_AGENT`. If the `claude --version`
reported there is newer than the one in `USER_AGENT`, run the on-wire capture below and update
`USER_AGENT` to the captured `user-agent` value.

> **macOS gotcha:** the full (non-`--debug-only`) script path calls `timeout`, which macOS lacks by
> default — this silently produces "no capture." Use `--debug-only`, install coreutils
> (`brew install coreutils` → `gtimeout`), put a small `timeout` shell shim on `PATH`, or use the
> manual capture below.

## On-wire capture (mitmproxy)

To confirm the actual `/v1/messages` headers (User-Agent, `x-app`, `anthropic-beta`), capture live
traffic rather than guessing from version numbers:

```bash
mitmdump --set flow_detail=4 -p 8888 &
HTTPS_PROXY=http://127.0.0.1:8888 \
  NODE_EXTRA_CA_CERTS=~/.mitmproxy/mitmproxy-ca-cert.pem \
  claude -p --strict-mcp-config --model claude-haiku-4-5 'Reply exactly: ok'
```

Lessons from the 2026-07-02 capture:
- `--strict-mcp-config` disables MCP servers so the CLI reaches `/v1/messages` quickly instead of
  spending the whole window on MCP/bootstrap/telemetry (the failure mode that stalls naïve captures).
- Do **not** rely on a fixed `timeout`; poll for the captured flow, then kill the processes.
- Genuine CC does **not** send `x-anthropic-billing-header` as an HTTP header. As of 2.1.280 it sends
  the same attribution string as the **first `system` block** of the request body, with an
  input-dependent `cch` (the `--debug-file` line shows `cch=00000`; the wire showed `cch=d9c31`).
  The proxy still injects the header form; see `header-provenance.md` (2026-09-23 update).

Compare the captured headers against `provider_impl.rs`. Update constants only when evidence shows
they changed, then run `mise run ci`. Do not add an `x-anthropic-billing-header` back: the capture
will keep showing it absent on the wire, and the body-block form is not a proxy target (D003).

## Parity checklist

Current parity target: `USER_AGENT` mirrors genuine Claude Code **2.1.280** (verified on-wire
2026-09-23). `X_APP` and `REQUIRED_BETA_FLAGS` mirror **2.1.198** (2026-07-02); 2.1.280 sends two
additional beta flags that are not forced. No billing header is sent (retired 2026-09-23).

- [ ] `USER_AGENT` matches on-wire `user-agent`. **This is the one that causes outages when stale.**
- [ ] `x-anthropic-billing-header` still absent from the genuine on-wire capture (if it ever
      reappears as a real header, that is new evidence; reopen D003 rather than hardcoding a value).
- [ ] `X_APP` present (`cli`).
- [ ] `REQUIRED_BETA_FLAGS` mirrors the on-wire `anthropic-beta` set — consult
      [`anthropic-beta-flags.md`](../audits/anthropic-beta-flags.md) for which flags are safe to force
      before adding any.
- [ ] `mise run ci` green.
