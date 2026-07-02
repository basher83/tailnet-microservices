# Anthropic `anthropic-beta` Flag Audit

Cause/effect analysis of the `anthropic-beta` flags genuine Claude Code sends on
`POST /v1/messages`, versus what this proxy injects. Written to answer: *which of
the missing flags should the proxy force, and what does each one actually do?*

**Do not blind-sync this set.** This document exists so the decision to add each
flag is deliberate and reversible. Adding a flag is a one-line change to
`REQUIRED_BETA_FLAGS` in `services/oauth-proxy/src/provider_impl.rs`; understanding
the blast radius is the hard part.

## Ground truth (on-wire capture, 2026-07-02, CC v2.1.198)

Genuine Claude Code 2.1.198 sent this exact `anthropic-beta` value on `/v1/messages`
(Max OAuth → `api.anthropic.com`, HTTP 200):

```
oauth-2025-04-20,
interleaved-thinking-2025-05-14,
thinking-token-count-2026-05-13,
context-management-2025-06-27,
prompt-caching-scope-2026-01-05,
claude-code-20250219,
advisor-tool-2026-03-01,
advanced-tool-use-2025-11-20,
extended-cache-ttl-2025-04-11,
cache-diagnosis-2026-04-07
```

Proxy `REQUIRED_BETA_FLAGS` today injects **3 of 10**:
`oauth-2025-04-20`, `interleaved-thinking-2025-05-14`, `context-management-2025-06-27`.

## Risk framing (read this first)

There are three independent axes. A flag can be risky on one and inert on another.

1. **Acceptance** — will the request be rejected (400 `Unexpected value(s)` / `invalid beta flag`)?
   - On **this proxy's path** (Max OAuth → `api.anthropic.com`): **~nil for all 10** — genuine CC
     sends the full set to this exact endpoint and gets 200. Rejections in the wild
     (`claude-code-20250219`, `advanced-tool-use-…`) are **Bedrock/Vertex/Opus-4.7** cases, which
     this proxy does not target.
2. **Response-shape** — does the flag change what the client *receives*? The proxy fans out to
   heterogeneous clients (pi + generic), so an unconditional response-shape change is imposed on all
   of them.
3. **Request-field dependency** — does the flag only *do* anything if the request body also carries a
   matching field/tool? If so, the bare header is **inert** until a client opts in (and genuine CC
   sending it proves the bare header is harmless).

> Note: betas can travel in **both** the `anthropic-beta` header and the `anthropic_beta` body array,
> and some gate tool-schema fields. `merge_beta_headers` already **preserves** client-supplied flags;
> the only question here is which to **force** for clients that don't send them.

## Per-flag analysis

| Flag | Proxy | Primary effect | Axis | Force? |
|---|---|---|---|---|
| `oauth-2025-04-20` | ✅ have | Enables OAuth/subscription bearer-token auth on the request | Auth (required) | **required** |
| `interleaved-thinking-2025-05-14` | ✅ have | Interleaved extended thinking between tool calls (Claude 4) | Response | keep |
| `context-management-2025-06-27` | ✅ have | Server-side context editing / tool-result clearing | Request+Response | keep |
| `claude-code-20250219` | ❌ missing | **Claude Code identity/mode** beta — most fingerprint-relevant; genuine CC always sends it | Identity | **Tier 1 — add** |
| `advanced-tool-use-2025-11-20` | ❌ missing | Gates advanced tool-use body fields (tool-search, deferred/programmatic tool loading). Inert unless client sends those fields; forcing it *prevents* the "body field without header → 400" class | Request-dep (inert) | **Tier 1 — add** |
| `extended-cache-ttl-2025-04-11` | ❌ missing | Unlocks 1-hour `cache_control.ttl:"1h"`. Inert unless body sets it. Billing: 1h cache writes cost more, but only when opted in | Request-dep (inert) | **Tier 1 — add** |
| `prompt-caching-scope-2026-01-05` | ❌ missing | Controls prompt-cache breakpoint scoping. Additive/default; observed sent even when related CC settings off | Caching (near-inert) | **Tier 1 — add** |
| `thinking-token-count-2026-05-13` | ❌ missing | Adds `estimated_tokens` to **streamed thinking deltas** (coarse hint; `usage.output_tokens` stays authoritative) | **Response-shape (unconditional)** | **Tier 2 — verify clients first** |
| `advisor-tool-2026-03-01` | ❌ missing | Enables the advisor tool. Bare header inert (CC proves it); **400 `invalid_request_error`** only if the advisor tool is *invoked* with a bad executor/advisor model pairing | Feature-gate (inert) | **Tier 3 — fidelity only** |
| `cache-diagnosis-2026-04-07` | ❌ missing | Client passes `diagnostics.previous_message_id`; API returns `diagnostics.cache_miss_reason` on misses. Inert unless client opts in | Request-dep + Response (opt-in) | **Tier 3 — fidelity only** |

## Recommendation

The proxy's design intent is to *mimic Claude Code*. Mirroring the full set is defensible because
genuine CC does exactly that on this endpoint and succeeds. The blast radius is dominated by **one**
flag, not seven:

- **Tier 1 — safe to force now** (identity + inert-unless-opted-into, all close fingerprint gaps):
  `claude-code-20250219`, `advanced-tool-use-2025-11-20`, `extended-cache-ttl-2025-04-11`,
  `prompt-caching-scope-2026-01-05`.
- **Tier 2 — force after one validation** (`thinking-token-count-2026-05-13`): it is the **only**
  unconditional response-shape change (adds `estimated_tokens` to thinking-delta SSE events). Confirm
  pi and the generic clients tolerate an unknown additive field on thinking deltas before enabling.
- **Tier 3 — include for exact fidelity, with eyes open** (`advisor-tool-2026-03-01`,
  `cache-diagnosis-2026-04-07`): bare headers are inert on this path (CC proves it), but each gates a
  feature that *could* 400 or change responses **if a client actually used it**. The proxy itself
  never invokes these, so risk is theoretical — but document that we are not exercising them.

### Before enabling any tier — verification checklist
- [ ] Re-confirm the request still returns 200 through the proxy (live smoke) after each tier.
- [ ] For Tier 2: capture a streaming response through the proxy and confirm pi renders it (unknown
      `estimated_tokens` field tolerated, no parse error).
- [ ] Confirm no generic client strict-validates the SSE delta schema.
- [ ] Keep flags additive to `REQUIRED_BETA_FLAGS`; `merge_beta_headers` dedupes, so client-sent
      duplicates are safe.
- [ ] Update the `merge_beta_headers` test in `provider_impl.rs` / the echoed-headers assertions.

## Open questions / re-verify on next CC upgrade
- Does `claude-code-20250219` unlock server-side system-prompt handling that interacts with
  `sanitize_system_prompt_for_plan_usage`? (Currently: no observed interaction; re-check.)
- Exact semantics of `prompt-caching-scope-2026-01-05` scoping — Anthropic docs are thin; treat as
  default-preserving until proven otherwise.
- `extended-cache-ttl` real-world TTL honoring is reported as flaky (1h configs landing in 5m
  buckets) — irrelevant unless a client sets `ttl:"1h"`, but note it.

## Sources
- Anthropic API beta headers: <https://platform.claude.com/docs/en/api/beta-headers>, <https://platform.claude.com/docs/en/api/beta>
- Prompt caching / extended TTL: <https://platform.claude.com/docs/en/build-with-claude/prompt-caching>
- Extended thinking / interleaved: <https://platform.claude.com/docs/en/build-with-claude/extended-thinking>
- Advisor tool (model-pairing 400): <https://platform.claude.com/docs/en/agents-and-tools/tool-use/advisor-tool>
- `thinking-token-count` `estimated_tokens`: anthropic-sdk-python commit 80d0fdf
- Gateway beta-rejection / `CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS`: claude-code issues #49370, #49648, #30926, #21644; higress PR #3904; vercel/ai #16245
- On-wire ground truth: `docs/audits/header-provenance.md` (2026-07-02 capture)
