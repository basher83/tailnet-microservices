# Spec: OpenClaw Tool-Name Rewrite

**Status:** Draft
**Created:** 2026-05-10
**Related:** [`generic-client-support.md`](./generic-client-support.md) — same class of problem, different resolution

---

## Why

OpenClaw (the npm-distributed agent runtime) sends requests through the OAuth proxy and consistently receives:

```json
{"type":"error","error":{"type":"invalid_request_error",
 "message":"You're out of extra usage. Add more at claude.ai/settings/usage and keep going."},
 "request_id":"req_011Cat..."}
```

This is the same anti-fingerprint gate documented in `generic-client-support.md`, but with a different surface: instead of "This credential is only authorized for use with Claude Code" the gate now returns the "out of extra usage" wording. Empirical bisection isolated the trigger to **OpenClaw's distinctive tool-name set**:

| Test (same proxy, same OAuth account, same headers, same body shape) | Result |
|---|---|
| 12 generic tool names (`read,write,edit,exec,bash,grep,glob,ls,cat,find,mv,cp`) | HTTP 200 |
| 12 OpenClaw-distinctive names (`agents_list,canvas,cron,gateway,sessions_list,...`) | HTTP 400 |
| 6 generic + 6 OpenClaw mixed | HTTP 400 |
| OpenClaw schemas with names renamed to `mytool_0..27` | HTTP 200 |
| OpenClaw names with empty schemas | HTTP 400 |

Pi (which OpenClaw embeds) works through the same proxy because Pi exposes a leaner, generic-named tool set. Claude Code works because it sends its own names. OpenClaw fails because its multi-agent / gateway / sessions surface is distinctive enough for Anthropic to fingerprint and route to PAYG, which is unavailable on this account.

The `generic-client-support.md` spec reached the same conclusion for forgeflare and resolved it with a **client-side rename** (commit `dbd81e8`, tag `v0.0.47`), explicitly rejecting proxy-side mapping as "fragile". That decision stands for clients the operator controls. OpenClaw is not such a client: it ships from upstream npm, registers tool names through its plugin architecture, and its tool surface grows with installed plugins. A client-side fork is high-cost and bit-rots against every release.

This spec proposes a **bounded, opt-in, profile-driven** rewrite layer in the proxy itself, accepting the fragility documented in the prior spec as a known trade-off in exchange for keeping unmodified upstream OpenClaw working.

---

## Background: relationship to `generic-client-support.md`

The prior spec's resolution included this Non-Goal amendment:

> ~~Client-side changes to forgeflare or other consumers~~ — amended: tool names must match Claude Code's PascalCase convention. Proxy-side mapping rejected as fragile. Clients adopt PascalCase tool names directly.

Concrete reasons that argument was correct for forgeflare:
1. The mapping table is fragile if the client adds tools the proxy doesn't know about.
2. Reversibility: tool-use blocks in responses must be renamed back, including in SSE streams.
3. Round-trip integrity: subsequent turns replay prior `tool_use` blocks; the proxy must rewrite both directions consistently.

OpenClaw forces a re-evaluation because:
1. It is not the operator's code; rebuilds and forks track an active npm release.
2. The number of tools is bounded (~28 currently) and the set is enumerable from a captured request — fragility is finite, not unbounded.
3. The proxy already does request-body manipulation (system-prompt prefix injection); adding tool-name rewriting is the same kind of operation, not a new architectural axis.

The "proxy-side mapping rejected" decision is preserved as the **default**. This spec adds a **per-client profile** that opt-in clients can request via configuration. Forgeflare and arbitrary generic clients keep their current behavior.

---

## Investigation Required

Before locking the design, three questions need empirical answers. Each is a quick proxy-curl test and should be folded into the implementation PR's notes.

**I1. What is the accepted PascalCase set?**

`generic-client-support.md` documents `Bash, Read, Write, Edit, Glob, Grep, ...` as accepted. The "etc." is doing a lot of work. The full enumeration matters because the rewrite map's target alphabet is constrained by it. Probe candidates: `LS, Task, TodoWrite, WebSearch, WebFetch, NotebookEdit, MultiEdit, ExitPlanMode, Agent, AskUserQuestion`. Goal: a verified list of N PascalCase names Anthropic accepts when present alone in `tools[]`.

**I2. Are arbitrary PascalCase names accepted?**

Send `[{name:"OpenclawTool1",...}]`. If 200, the mapping target alphabet is unbounded and we can use synthetic Claude-Code-shaped names per OpenClaw tool. If 400, we are constrained to the known set from I1, and the map is bijection-into-finite-pool.

**I3. Does Anthropic validate name + description / schema consistency?**

`generic-client-support.md` says no — only `name` matters. Re-verify under the current upstream (validation has tightened once already). Send `{name:"Read", description:"shoot lasers at the moon", input_schema:{...arbitrary...}}` and check status.

I1 and I2 together determine whether the rewrite map is a **rename** (free choice of targets) or a **slot allocation** (multiplexing OpenClaw's tools into a fixed pool). The implementation differs significantly.

---

## Design

The rewrite layer activates only when the request matches a configured **client profile**. Profiles are detected from request shape (e.g. presence of OpenClaw-distinctive tool names) or selected explicitly via a request header. Other clients see no change in behavior.

### Activation

Two activation modes, in priority order:

1. **Explicit:** request includes `x-tailnet-client-profile: openclaw`. The proxy is the trust boundary on the tailnet, so a client can self-identify.
2. **Heuristic:** if any of a configured "fingerprint name set" appears in `tools[].name`, activate the matching profile. The fingerprint set is just a few highly-distinctive OpenClaw names (`sessions_spawn`, `agents_list`, `subagents`) — false positives are tolerable because the rewrite is also harmless for non-OpenClaw clients that happen to use those names.

Activation is per-request. The proxy stays stateless across requests.

### Static bidirectional name map

The proxy holds a `BiMap<String, String>` per profile loaded from config. Forward direction is `OpenClaw → Anthropic-accepted`; reverse is `Anthropic-accepted → OpenClaw`. The map must be a strict bijection — no two OpenClaw names map to the same upstream name and vice versa. Startup validates this and fails fast on collision.

Initial proposed map (subject to I1/I2 outcome — if I2 says "any PascalCase is fine," prefer column A; if I2 says "only known set," fall back to column B with collisions resolved by suffixing or by mapping the rarer OpenClaw tools to disabled).

| OpenClaw name | A. Synthetic PascalCase | B. Known Claude Code |
|---|---|---|
| `agents_list` | `AgentsList` | `Agent` |
| `canvas` | `Canvas` | *(no slot — drop)* |
| `cron` | `Cron` | *(no slot — drop)* |
| `edit` | `Edit` | `Edit` |
| `exec` | `Exec` | `Bash` |
| `gateway` | `Gateway` | *(no slot — drop)* |
| `image` | `Image` | *(no slot — drop)* |
| `image_generate` | `ImageGenerate` | *(no slot — drop)* |
| `memory_get` | `MemoryGet` | *(no slot — drop)* |
| `memory_search` | `MemorySearch` | *(no slot — drop)* |
| `message` | `Message` | *(no slot — drop)* |
| `music_generate` | `MusicGenerate` | *(no slot — drop)* |
| `nodes` | `Nodes` | *(no slot — drop)* |
| `pdf` | `Pdf` | *(no slot — drop)* |
| `process` | `Process` | *(no slot — drop)* |
| `read` | `Read` | `Read` |
| `session_status` | `SessionStatus` | *(no slot — drop)* |
| `sessions_history` | `SessionsHistory` | `TaskOutput` |
| `sessions_list` | `SessionsList` | `TaskList` |
| `sessions_send` | `SessionsSend` | *(no slot — drop)* |
| `sessions_spawn` | `SessionsSpawn` | `Task` |
| `sessions_yield` | `SessionsYield` | *(no slot — drop)* |
| `subagents` | `Subagents` | *(no slot — drop)* |
| `tts` | `Tts` | *(no slot — drop)* |
| `video_generate` | `VideoGenerate` | *(no slot — drop)* |
| `web_fetch` | `WebFetch` | `WebFetch` |
| `web_search` | `WebSearch` | `WebSearch` |
| `write` | `Write` | `Write` |

If we land in column B, the proxy must drop unmapped tools from the request (with a `WARN` log per drop), accepting that OpenClaw loses functionality the model cannot invoke. Column A is the preferred outcome.

### Request transformation

In `provider_impl.rs::prepare_request` (the same hook that already injects the OAuth Bearer + system prefix), after activating the profile:

1. **`tools[].name`** — for each tool definition, if `name` is in the forward map, replace with the mapped value. Tools whose name is not in the map are passed through unchanged (same generic-client behavior as today). If column B is in effect and a name is unmapped, drop the tool from the array and log.

2. **`messages[].content[]` walk** — for each block where `type == "tool_use"`, replace `name` with the mapped value. This handles assistant turns being replayed in multi-turn conversations.

3. **`messages[].content[].tool_use_id`** — left unchanged. Tool-use IDs are opaque and round-trip through both directions verbatim.

The transformation is destructive on the body buffer; the proxy already serializes a transformed body for OAuth header injection, so this is an additive pass over the parsed JSON.

### Response transformation — non-streaming

Non-streaming responses (where `stream != true`) return a single JSON body with `content[]` blocks. For each block where `type == "tool_use"`, replace `name` with the **reverse** map value before returning to the client. Unknown names pass through.

This requires the proxy to buffer the response body, which it currently does not for streaming. Non-streaming bodies are already buffered by reqwest — minor change.

### Response transformation — streaming SSE

Streaming is the load-bearing case (Claude Code and OpenClaw default to `stream: true`). The relevant SSE event is `content_block_start` carrying a `tool_use` block:

```text
event: content_block_start
data: {"type":"content_block_start","index":N,"content_block":{"type":"tool_use","id":"toolu_...","name":"Read","input":{}}}
```

The proxy must parse SSE frames, identify `content_block_start` events whose `content_block.type == "tool_use"`, rewrite `content_block.name`, and re-serialize the `data:` line. All other event types (`message_start`, `content_block_delta`, `content_block_stop`, `message_delta`, `message_stop`, `ping`, `error`) pass through untouched.

This is a real cost: today the streaming path in `proxy.rs::build_streaming_response` byte-forwards through `IdleTimeoutStream`. Adding SSE-aware rewriting means parsing each frame. Implementation sketch:

```rust
// New stream wrapper, composed inside IdleTimeoutStream
pub struct ToolNameRewriteStream<S> {
    inner: S,
    profile: Arc<ClientProfile>,    // contains reverse map
    buffer: BytesMut,                // holds partial SSE frames across chunks
}

// poll_next:
// 1. Read chunk from inner; append to buffer.
// 2. Find complete SSE frames (delimited by \n\n).
// 3. For each frame: if first line is `event: content_block_start`,
//    parse the `data:` JSON, walk to content_block.name, rewrite via reverse map,
//    re-serialize. Other frames re-emit unchanged.
// 4. Emit rewritten frames; keep partial trailing data in buffer for next chunk.
```

The wrapper sits **inside** `IdleTimeoutStream` (rewrite first, then idle-timeout) so a stalled rewrite path is still subject to the existing idle deadline. Frame parsing must tolerate the chunk boundary landing mid-frame, mid-`data:`-line, or mid-multi-line-event.

A correctness concern: re-serialized JSON may differ in whitespace from the upstream. SSE clients (`anthropic-sdk-typescript`, `eventsource-parser`, etc.) parse `data:` payloads as JSON and don't care about whitespace, but byte-for-byte parity is lost. The streaming-timeout-fix.md non-goal "no SSE-aware streaming logic" is consciously breached here.

### Configuration

Extend `config.toml`:

```toml
[client_profiles.openclaw]
# Heuristic activation: if any of these names appear in tools[], activate this profile.
fingerprint_names = ["sessions_spawn", "agents_list", "subagents"]

# Bidirectional name map. Keys = OpenClaw, values = upstream-accepted.
[client_profiles.openclaw.tool_names]
agents_list      = "AgentsList"
canvas           = "Canvas"
cron             = "Cron"
edit             = "Edit"
exec             = "Exec"
gateway          = "Gateway"
image            = "Image"
image_generate   = "ImageGenerate"
memory_get       = "MemoryGet"
memory_search    = "MemorySearch"
message          = "Message"
music_generate   = "MusicGenerate"
nodes            = "Nodes"
pdf              = "Pdf"
process          = "Process"
read             = "Read"
session_status   = "SessionStatus"
sessions_history = "SessionsHistory"
sessions_list    = "SessionsList"
sessions_send    = "SessionsSend"
sessions_spawn   = "SessionsSpawn"
sessions_yield   = "SessionsYield"
subagents        = "Subagents"
tts              = "Tts"
video_generate   = "VideoGenerate"
web_fetch        = "WebFetch"
web_search       = "WebSearch"
write            = "Write"
```

Profiles are loaded at startup. Bijection is validated at load — duplicate values across keys is a fatal config error. Adding a new client is a config-only operation.

---

## Architecture

```text
Client (OpenClaw)
   │  tools=[{name:"sessions_spawn",...},{name:"read",...},...]
   │  messages=[..., {role:"assistant", content:[{type:"tool_use", name:"sessions_spawn",...}]}, ...]
   ▼
prepare_request()
   │  profile = activate_profile(req)        ← heuristic or explicit header
   │  if profile:
   │    rewrite_tools_forward(tools, profile)
   │    rewrite_assistant_tool_use_forward(messages, profile)
   ▼
Anthropic upstream
   │  sees Claude-Code-shaped tools[].name
   │  responds with tool_use blocks using upstream names
   ▼
build_streaming_response() / build_response()
   │  if profile and stream:
   │    stream = ToolNameRewriteStream::new(stream, profile)
   │  if profile and not stream:
   │    rewrite_tool_use_reverse(body, profile)
   ▼
Client (OpenClaw)
   │  sees its original tool names everywhere — round-trip transparent
```

The proxy stays stateless across requests; the profile is reconstructed per-request from headers and tool fingerprint.

---

## Changes

### `crates/anthropic-proxy/src/profile.rs` (new)

`ClientProfile { fingerprint: HashSet<String>, forward: BiMap<String,String> }` plus a `ProfileRegistry` that loads and validates the map at startup.

### `services/oauth-proxy/src/provider_impl.rs::prepare_request`

After existing OAuth header injection and system-prompt normalization, before serializing the body:

```rust
let profile = state.profiles.match_request(&headers, &body);
if let Some(profile) = &profile {
    body = rewrite_request_body_forward(body, profile)?;
}
// Stash profile in request extensions for the response side.
```

The profile is attached to the request so the response handler can reverse-rewrite without re-detecting.

### `services/oauth-proxy/src/proxy.rs::build_streaming_response`

When the request had an active profile, wrap the upstream stream with `ToolNameRewriteStream` *before* `IdleTimeoutStream`:

```rust
let upstream_bytes = upstream_response.bytes_stream();
let rewritten = if let Some(profile) = req_profile {
    Box::pin(ToolNameRewriteStream::new(upstream_bytes, profile))
        as Pin<Box<dyn Stream<Item = ...>>>
} else {
    Box::pin(upstream_bytes)
};
let idle_stream = IdleTimeoutStream::new(rewritten, timeout);
```

Non-streaming responses get a similar reverse-rewrite call before the body is returned to the client.

### `crates/anthropic-proxy/src/sse_rewrite.rs` (new)

`ToolNameRewriteStream` implementation. Buffer-and-split SSE frames. Parse only `content_block_start` events; pass everything else through. Delivers `Result<Bytes, _>` to match the `IdleTimeoutStream` contract.

### `services/oauth-proxy/src/config.rs`

Parse the new `[client_profiles.<name>]` table. Validate bijection at load (no duplicate values, no key=value collision in the open set, fingerprint names must be valid keys in the same map).

### Tests

- `forward_rewrite_renames_known_tools` — request with `sessions_spawn` becomes upstream request with mapped name.
- `forward_rewrite_passes_through_unknown_tools` — unmapped names are unchanged.
- `forward_rewrite_renames_assistant_tool_use_history` — multi-turn replay of prior tool_use blocks gets renamed.
- `reverse_rewrite_renames_response_tool_use_blocks` — non-streaming response gets reverse-rename.
- `sse_rewrite_handles_chunk_boundary_in_event_name` — chunk split mid-frame still produces correct rewrite.
- `sse_rewrite_handles_chunk_boundary_mid_data_line` — chunk split inside the JSON body of a `data:` line.
- `sse_rewrite_passes_through_non_tool_use_events` — `message_start`, `content_block_delta`, etc. are byte-identical out.
- `profile_activation_via_header` — `x-tailnet-client-profile: openclaw` activates without fingerprint match.
- `profile_activation_via_fingerprint` — presence of `sessions_spawn` in tools[] activates without header.
- `profile_inactive_for_other_clients` — Claude Code request without profile markers is byte-identical through the proxy (no rewrite path executed).
- `bijection_validation_rejects_duplicate_targets` — startup test: two OpenClaw names mapping to the same upstream name is a load error.
- `idle_timeout_still_fires_through_rewrite_wrapper` — stall mid-stream after rewrite wrapper still terminates within `state.timeout`.

---

## Success Criteria

- [ ] I1, I2, I3 investigation results documented in this spec.
- [ ] Default config column (A or B) selected based on I2.
- [ ] OpenClaw end-to-end: a captured failing request body, replayed through the gateway with the profile active, returns 200 from upstream.
- [ ] OpenClaw multi-turn: a session that uses tool_use → tool_result → next-turn replay completes without error.
- [ ] Streaming: OpenClaw streaming responses arrive at the client with original OpenClaw tool names, in the same byte timing as upstream within idle-timeout tolerances.
- [ ] Forgeflare regression: forgeflare requests through the gateway behave identically to today (no profile activates for them, no rewrite path executes).
- [ ] Claude Code regression: Claude Code requests through the gateway behave identically to today.
- [ ] All existing tests pass (`cargo test --workspace`).
- [ ] `cargo clippy --workspace -- -D warnings` clean.
- [ ] Streaming idle-timeout test (`proxy_stream_idle_timeout_terminates_stalled_stream`) still passes with the rewrite wrapper composed inside.

---

## Non-Goals

- **Auto-discovery of upstream's accepted name set.** The map is operator-curated. If Anthropic adds or removes accepted names, the operator updates config.
- **Generic name canonicalization** (lowercase → PascalCase, snake_case → camelCase, etc.). The forgeflare resolution showed name validation isn't algorithmic — it's set-membership. Mechanical canonicalization will produce names outside the accepted set for some inputs.
- **Per-session state.** The profile is per-request, derived from request shape and headers. No session tracking, no client identity persistence beyond the request.
- **Description / schema rewriting.** Per `generic-client-support.md`, only `name` is validated. If I3 contradicts that, this spec needs revision.
- **Fixing the underlying anti-fingerprint behavior in Anthropic.** That is upstream policy. This spec is a workaround for clients on the operator's tailnet that the operator does not control.
- **Generalizing to non-Anthropic providers.** Single provider, consistent with the `oauth-proxy.md` non-goal.

---

## Open Questions

1. **Activation precedence:** if the explicit header names a profile that doesn't exist in config, should the proxy 400 the request, log-and-fall-through, or activate the heuristic match? Default proposal: log warning, fall through to heuristic.
2. **Map drift:** OpenClaw adds tools across releases. Should the proxy emit a metric (`unmapped_tool_name_total{profile,name}`) so the operator notices drift before users do? Probably yes; cost is one counter increment per request.
3. **Tool-result payloads:** if a future Anthropic feature expands `tool_result` blocks to include the tool name (currently they only carry `tool_use_id`), the rewrite passes need extending. Out of scope until observed.
4. **Failed-rename behavior:** column-B mode drops unmapped tools from the request. Is the operator OK with the model never seeing those tools, or should the proxy refuse the request with a 400 explaining which tools couldn't be carried? Default proposal: drop + WARN; this matches the existing "fail open" disposition of the proxy.
5. **Profile auto-detect-only mode:** the heuristic alone (`fingerprint_names`) may be sufficient and the explicit header optional. Worth keeping the explicit header for testability and emergency overrides regardless.
