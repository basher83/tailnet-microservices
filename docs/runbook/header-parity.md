# Header Capture & Parity Checks

> **Operational Runbook** · [Index](./README.md) · [Deployment](./deployment.md) · [Accounts](./accounts.md) · [Monitoring](./monitoring.md) · [Troubleshooting](./troubleshooting.md) · [Clients](./clients.md) · [Header Parity](./header-parity.md)

The proxy impersonates Claude Code on the wire: it injects the User-Agent, `x-app`,
`anthropic-version`, the `anthropic-beta` flag set, and the `x-anthropic-billing-header`
attribution marker that genuine Claude Code sends on `POST /v1/messages`. Those values are
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
| `ANTHROPIC_BILLING_HEADER` | `x-anthropic-billing-header` (debug-only; injected deliberately) |

The billing header is also mirrored in `~/.pi/agent/models.json` (the client-side
`x-anthropic-billing-header`). Update both together.

## Fast drift check (no mitmproxy)

The outage-relevant signal is **`USER_AGENT` drift**: Anthropic enforces per-model Claude Code
minimum versions on the `user-agent` header, and a stale value produces HTTP 400
`claude_code_version_too_old` for newer models (see [Troubleshooting](./troubleshooting.md)). The
`cc_version` attribution line needs only Claude Code's `--debug-file` output, no traffic capture:

```bash
mise run headers:capture          # wraps scripts/capture-cc-headers.sh
# cc_version only, fastest:
scripts/capture-cc-headers.sh --debug-only
```

This prints the live `cc_version` attribution line and diffs it against the proxy constants. If it
reports drift, update `ANTHROPIC_BILLING_HEADER` (and the `~/.pi/agent/models.json` mirror), then
rebuild.

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
they changed, then run `mise run ci`.

## Parity checklist

Current parity target: `USER_AGENT` mirrors genuine Claude Code **2.1.280** (verified on-wire
2026-09-23). `X_APP` and `REQUIRED_BETA_FLAGS` mirror **2.1.198** (2026-07-02); 2.1.280 sends two
additional beta flags that are not forced. `ANTHROPIC_BILLING_HEADER` is deliberately held at
2.1.198.bb7 (its version is not what the server checks).

- [ ] `USER_AGENT` matches on-wire `user-agent`. **This is the one that causes outages when stale.**
- [ ] `cc_version` in `ANTHROPIC_BILLING_HEADER` compared against the live `--debug-file` line
      (informational; bump only with a deliberate attribution decision).
- [ ] `X_APP` present (`cli`).
- [ ] `REQUIRED_BETA_FLAGS` mirrors the on-wire `anthropic-beta` set — consult
      [`anthropic-beta-flags.md`](../audits/anthropic-beta-flags.md) for which flags are safe to force
      before adding any.
- [ ] `~/.pi/agent/models.json` billing header updated to match.
- [ ] `mise run ci` green.
