# Spec: Generic Client Support for OAuth Mode

**Status:** Active
**Created:** 2026-02-12

---

## Why

The OAuth proxy was built to sit in front of Claude Code specifically. Claude Code sends requests with a known shape: specific user-agent, system prompt prefix, beta flags, and `anthropic-dangerous-direct-browser-access` header. The proxy injects these markers when forwarding requests with Claude Max OAuth tokens.

Non-Claude-Code clients (forgeflare, curl, custom agents) send requests through the same proxy but produce different request shapes. Anthropic validates that Claude Max OAuth credentials are used exclusively by Claude Code, rejecting requests that don't match the expected shape. The validation goes beyond headers — there's likely request body fingerprinting (system field format, tool schemas, or other structural checks).

Observed behavior: the proxy injects Bearer token, user-agent, system prompt prefix, beta flags, and the browser-access header. Anthropic still returns 400 with "This credential is only authorized for use with Claude Code." The proxy correctly reports these as upstream 400s in its logs. The issue is that the request body shape doesn't match what Anthropic expects from a Claude Code client.

---

## Investigation Required

Before implementing, determine exactly which request properties Anthropic validates for Claude Max OAuth credentials. The proxy already injects:

1. `Authorization: Bearer {access_token}` — injected
2. `anthropic-beta: oauth-2025-04-20,...` — injected and merged
3. `anthropic-dangerous-direct-browser-access: true` — injected
4. `user-agent: claude-cli/2.0.76 (external, sdk-cli)` — injected
5. `anthropic-version: 2023-06-01` — injected
6. System prompt prefix: "You are Claude Code..." — injected for non-Haiku models

What may also be validated:

- **`system` field format**: Claude Code may send `system` as an array of content blocks rather than a plain string. The proxy currently injects/prepends a string. If Anthropic validates the format, the proxy needs to handle array-format system prompts.
- **Haiku system prompt requirement**: The proxy skips system prompt injection for Haiku models (line 181 of `provider_impl.rs`). Anthropic may have tightened validation to require the prefix on all models including Haiku.
- **Tool schemas**: Claude Code sends its specific tool definitions. Anthropic may validate that recognized tool schemas are present.
- **Request body structure**: Other structural checks on the JSON body that distinguish Claude Code requests from generic API requests.

### Investigation Approach

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

**R3. Haiku System Prompt Audit**
- Test whether Anthropic requires the system prompt prefix for Haiku models under OAuth credentials
- If required: remove the Haiku skip in `inject_system_prompt()` (line 181)
- If not required: document the exception

**R4. System Field Format Handling**
- If Anthropic validates the `system` field format, handle both string and array formats:
  - String: current behavior (prepend prefix)
  - Array: inject prefix as first content block if not already present
- Test both formats against the live API

**R5. Preserve Client Intent**
- All transformations must preserve the client's actual request intent (model, messages, tools, parameters)
- The proxy adds what's needed for credential compliance without removing or altering client functionality
- Existing test suite must continue to pass

---

## Success Criteria

- [ ] Root cause identified and documented (which validation check(s) fail)
- [ ] Forgeflare requests succeed through the proxy with zero client-side changes
- [ ] `curl` requests succeed through the proxy with a bare JSON body (model + messages only)
- [ ] Haiku, Sonnet, and Opus models all work through the proxy
- [ ] Streaming (`"stream": true`) works through the proxy
- [ ] Existing Claude Code requests continue to work (no regression)
- [ ] All existing tests pass, new tests for the identified validation requirements

---

## Non-Goals

- Client-side changes to forgeflare or other consumers (the proxy is the transformation layer)
- Supporting non-Anthropic providers (single provider for now)
- Caching or modifying response bodies (proxy is request-only transformation)

---

## Architecture

```text
Client (forgeflare, curl, Claude Code)
    │
    ├── sends: model, messages, system (string), tools, stream
    │
    ▼
AnthropicOAuthProvider::prepare_request()
    │
    ├── inject: Bearer token, user-agent, beta flags, browser-access
    ├── inject: system prompt prefix (current)
    ├── transform: [NEW] additional body/header properties per R2-R4
    │
    ▼
api.anthropic.com
    │
    └── validates: request matches Claude Code shape → 200 or 400
```

Changes to existing code:

1. `services/oauth-proxy/src/provider_impl.rs` — extend `prepare_request` with additional transformations per investigation findings
2. `crates/anthropic-auth/src/constants.rs` — add any new constants discovered during investigation
3. Tests for all new transformations
