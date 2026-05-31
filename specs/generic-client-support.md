# Spec: Generic Client Support for OAuth Mode

**Status:** Complete (tool name validation resolved client-side; gateway body handling current)
**Created:** 2026-02-12

---

## Why

The OAuth proxy was built to sit in front of Claude Code specifically. Claude Code sends requests with a known shape: specific user-agent, system prompt prefix, beta flags, and `anthropic-dangerous-direct-browser-access` header. The proxy injects these markers when forwarding requests with Claude Max OAuth tokens.

Non-Claude-Code clients (forgeflare, curl, custom agents) send requests through the same proxy but produce different request shapes. Anthropic validates that Claude Max OAuth credentials are used exclusively by Claude Code, rejecting requests that don't match the expected shape. The validation goes beyond headers — there's likely request body fingerprinting (system field format, tool schemas, or other structural checks).

Observed behavior during the investigation: the proxy injected Bearer token, user-agent, system prompt prefix, beta flags, and the browser-access header, but Anthropic still returned 400 with "This credential is only authorized for use with Claude Code." The proxy later also kept the Claude Code billing attribution marker used for Max-plan routing. Empirical testing isolated PascalCase Claude Code tool names as the remaining validation gate for generic clients that send tools.

---

## Investigation Result

The proxy now injects the required Claude Code credential markers:

1. `Authorization: Bearer {access_token}` — injected
2. `anthropic-beta: oauth-2025-04-20,...` — injected and merged
3. `anthropic-dangerous-direct-browser-access: true` — injected
4. `user-agent: claude-cli/2.0.76 (external, sdk-cli)` — injected
5. `anthropic-version: 2023-06-01` — injected
6. `x-anthropic-billing-header: cc_version=2.1.128.f82; cc_entrypoint=sdk-cli; cch=00000;` — injected as the current proxy attribution marker for Max-plan routing
7. System prompt prefix: "You are Claude Code..." — injected for all requests with a string `model` field

Header provenance caveat: a Claude Code v2.1.132 local MITM capture saw the billing-header debug attribution line but did not see `x-anthropic-billing-header` on the actual `/v1/messages` request. Treat `cch=00000` as debug-attribution data with unknown semantics; the proxy keeps the current injection unchanged until a separate live A/B validates a replacement or removal.

What was evaluated:

- `system` field format: the gateway handles missing, string, and array-format `system` values. It creates or prepends a Claude Code text block and preserves existing array entries, including cache-control metadata.
- Haiku system prompt requirement: the gateway no longer skips Haiku. Prefix injection applies to all models when the request body has a valid string `model` field.
- **Tool schemas**: Claude Code sends its specific tool definitions. Anthropic may validate that recognized tool schemas are present.
- **Request body structure**: Other structural checks on the JSON body that distinguish Claude Code requests from generic API requests.

### Investigation Results (2026-02-12)

Empirical testing isolated tool names as the remaining validation gate. With the Content-Length fix (commit 0d3d908) and x-api-key stripping already in place:

- `"tools":[{"name":"bash",...}]` → 400 ("This credential is only authorized for use with Claude Code")
- `"tools":[{"name":"Bash",...}]` → 200 (Sonnet responds with tool_use)
- No `tools` field → 200

Anthropic validates tool names against Claude Code's known PascalCase set (`Bash`, `Read`, `Write`, `Edit`, `Glob`, `Grep`, etc.). Only the `name` field matters — description, input_schema shape, and parameter names are not validated.

**Eliminated suspects:** OAuth token scopes (verified correct), `system` field format (plain string works), user-agent version (tested 2.0.76 and 2.1.39), `claude-code-20250219` beta flag, HTTP protocol version (HTTP/1.1 and HTTP/2), tool schema structure beyond name.

**Resolution:** Forgeflare renamed its tool names to PascalCase (commit dbd81e8, tag v0.0.47). This is a client-side change, amending the original "zero client-side changes" non-goal — the proxy cannot reasonably map arbitrary tool names to Claude Code equivalents without a fragile mapping table.

### Original Investigation Approach

1. Capture a working Claude Code request (via `claude --verbose` or by intercepting with the proxy's debug logging) and compare the full request body against what forgeflare sends.
2. Diff the two payloads to identify structural differences beyond what the proxy already transforms.
3. Test each difference in isolation to determine which ones trigger the credential rejection.

---

## Requirements

**R1. Identify Validation Requirements**
- Determine the exact set of request properties Anthropic validates for Claude Max OAuth credentials
- Document findings in this spec before proceeding with implementation

**R2. Transform Request Body for Credential Compliance**
- Modify `prepare_request` in `provider_impl.rs` to transform any additional body properties that Anthropic validates
- The proxy should make any client's request look like a Claude Code request from Anthropic's perspective

**R3. Haiku System Prompt Audit** (Resolved)
- Haiku skip removed. `inject_system_prompt()` now applies to all models including Haiku, with explicit tests (`inject_haiku_gets_prefix`, `inject_haiku_case_insensitive`, `inject_haiku_with_existing_system_gets_prefix`).

**R4. System Field Format Handling** (Resolved)
- Missing `system`: the gateway creates an array-format system prompt containing the required Claude Code prefix.
- String `system`: the gateway converts the string into an array-format text block and prepends the prefix when needed.
- Array `system`: the gateway preserves existing blocks and prepends the prefix block when no existing text block already contains it.
- Other `system` shapes: the gateway leaves the value unchanged rather than guessing at a lossy conversion.

**R5. Preserve Client Intent**
- All transformations must preserve the client's actual request intent (model, messages, tools, parameters)
- The proxy adds what's needed for credential compliance without removing or altering client functionality
- Existing test suite must continue to pass

---

## Success Criteria

- [x] Root cause identified and documented (tool name validation — PascalCase required)
- [x] Forgeflare requests succeed through the proxy (tool names renamed client-side, verified: Read and Glob tool calls return 200)
- [x] `curl` requests succeed through the proxy with a bare JSON body (model + messages only, verified during investigation: no `tools` field → 200)
- [x] Haiku, Sonnet, and Opus models all work through the proxy (system prompt injection confirmed for all models; Haiku tests: `inject_haiku_gets_prefix`, `oauth_provider_injects_system_prompt_for_haiku`)
- [x] Streaming (`"stream": true`) works through the proxy (streaming-timeout-fix.md addresses the remaining streaming issue separately)
- [x] Existing Claude Code requests continue to work (no regression — 125 tests passing)
- [x] All existing tests pass (2026-02-13: 125 passed, 0 failed)

---

## Non-Goals

- ~~Client-side changes to forgeflare or other consumers~~ — amended: tool names must match Claude Code's PascalCase convention. Proxy-side mapping rejected as fragile. Clients adopt PascalCase tool names directly.
- Supporting non-Anthropic providers (single provider for now)
- Caching or modifying response bodies (proxy is request-only transformation)

---

## Architecture

```text
Client (forgeflare, curl, Claude Code)
    │
    ├── sends: model, messages, system (string or array), tools, stream
    │
    ▼
AnthropicOAuthProvider::prepare_request()
    │
    ├── inject: Bearer token, user-agent, beta flags, browser-access, billing attribution marker
    ├── normalize: system prompt prefix for supported body shapes
    ├── preserve: model, messages, tools, parameters, streaming flag
    │
    ▼
api.anthropic.com
    │
    └── validates: request matches Claude Code shape → 200 or 400
```

Implemented code:

1. `services/oauth-proxy/src/provider_impl.rs` — `prepare_request` injects OAuth headers, required beta flags, the current billing attribution marker, and calls system prompt normalization for OAuth mode.
2. `crates/anthropic-auth/src/constants.rs` — shared OAuth constants, including the Claude Code system prompt prefix.
3. Tests cover required header injection, Haiku handling, string and array system prompts, cache-control preservation, and prefix de-duplication.
