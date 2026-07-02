# Header Provenance Investigation

Investigates how the hardcoded header constants in `services/oauth-proxy/src/provider_impl.rs` were originally obtained.

> **Summary — provenance for both constants is established below.**
> - `USER_AGENT = "claude-cli/2.0.76 (external, sdk-cli)"` → §1 (mitmproxy capture, via Loom)
> - `ANTHROPIC_BILLING_HEADER = "cc_version=...; cc_entrypoint=sdk-cli; cch=00000;"` → §2 (live `--debug-file` capture)
> - Known, accepted wire drift vs. genuine current Claude Code → §3 / "wire drift" sections
> - Re-capture/maintenance → `scripts/capture-cc-headers.sh` (`mise run headers:capture`); also cited from `RUNBOOK.md`.

> **Note on commit SHAs.** The task names commit `16f3cba` for the `USER_AGENT` literal. That commit exists (`16f3cba8098da1355d25db1dfdd298699406c118`, "feat: Phase 4 — Gateway integration (AnthropicOAuthProvider)", 2026-02-08 21:45:37 -0500), but its sibling `3739e7379b...` carries the same message and timestamp and is what is reachable from `main` for the file. Both share content. Neither carries an `Entire-Checkpoint` trailer (`entire checkpoint explain --commit 3739e73` and `--commit df6435d` both return *"no Entire-Checkpoint trailer ... created outside of an Entire session"*), so this investigation relied on Pi/Claude Code session transcripts directly rather than the Entire CLI.

---

## 1. `USER_AGENT = "claude-cli/2.0.76 (external, sdk-cli)"` provenance

**Commit `3739e73` / `16f3cba` (Feb 8, 2026, 21:45 EST) was authored by Brent + Claude Opus 4.6 in a Ralph-runner session.** The literal was *not* discovered in that commit — it was lifted from a spec document that was already in the repo six hours earlier, which itself was lifted from a sister project.

### Direct provenance chain

1. **The Phase 4 implementation lifted the value from a spec doc.** The earliest tailnet-microservices commit that introduces `claude-cli/2.0.76` is `f97daa9` ("spec: new spec", 2026-02-08 19:08:17 -0500), which adds `specs/anthropic-oauth-gateway.md`. That spec contains the table:

   ```
   | `user-agent` | `claude-cli/2.0.76 (external, sdk-cli)` | Must match Claude CLI format |
   ```

   Every Feb 8 Claude Code subagent (`~/.claude/projects/-Users-basher8383-dev-personal-ralph-tailnet-microservices/.../subagents/agent-*.jsonl`, earliest hit Feb 8 19:26) shows agents *reading* line 304 of `specs/anthropic-oauth-gateway.md` to obtain the literal — they do not capture or derive it. Example excerpt from `agent-a98b127.jsonl`:

   > `303→| anthropic-dangerous-direct-browser-access | true | Required for OAuth |`
   > `304→| user-agent | claude-cli/2.0.76 (external, sdk-cli) | Must match Claude CLI format |`
   > `305→| anthropic-version | 2023-06-01 | API version |`

2. **The spec doc lifted the value from the sibling Loom project.** `specs/anthropic-oauth-gateway.md` cites under `## References`:

   - "Loom `specs/claude-subscription-auth.md` — OAuth 2.0 PKCE implementation details"
   - "Loom `specs/anthropic-oauth-pool.md` — Subscription pooling architecture"

   `/Users/basher8383/dev/personal/ralph/loom/specs/anthropic-oauth-pool.md` contains the actual capture writeup. Verbatim from lines 539–577:

   > **#### Sniffing Claude CLI Traffic with mitmproxy**
   >
   > To identify what headers the official Claude CLI sends:
   >
   > 1. Install mitmproxy: `nix-env -iA nixos.mitmproxy`
   > 2. Start mitmproxy in dump mode: `mitmdump --set flow_detail=4 -p 8888 2>&1 &`
   > 3. Configure environment for Claude CLI to use the proxy:
   >    ```
   >    export HTTPS_PROXY=http://127.0.0.1:8888
   >    export HTTP_PROXY=http://127.0.0.1:8888
   >    export SSL_CERT_FILE=~/.mitmproxy/mitmproxy-ca-cert.pem
   >    export REQUESTS_CA_BUNDLE=~/.mitmproxy/mitmproxy-ca-cert.pem
   >    export NODE_EXTRA_CA_CERTS=~/.mitmproxy/mitmproxy-ca-cert.pem
   >    ```
   > 4. Run a Claude CLI command: `claude --print "hello"`
   > 5. Observe the headers in mitmproxy output, particularly for `POST https://api.anthropic.com/v1/messages`
   >
   > **#### Key Observations from Traffic Analysis**
   >
   > The Claude CLI (v2.0.76) sends these specific headers for `/v1/messages`:
   >
   > | Header | Value |
   > |--------|-------|
   > | `anthropic-beta` | `oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27` |
   > | `anthropic-dangerous-direct-browser-access` | `true` |
   > | `user-agent` | `claude-cli/2.0.76 (external, sdk-cli)` |
   > | `x-app` | `cli` |

3. **Loom embeds the same literal in production Rust code.** `loom/crates/loom-server-llm-anthropic/src/auth/scheme.rs:33`:

   ```rust
   /// User-Agent to use when talking to Anthropic API.
   /// Must match Claude CLI format exactly.
   pub const ANTHROPIC_USER_AGENT: &str = "claude-cli/2.0.76 (external, sdk-cli)";
   ```

   So `provider_impl.rs::USER_AGENT` is a re-implementation of `loom`'s `ANTHROPIC_USER_AGENT`, sharing the exact same captured value.

### Origin: mitmproxy capture of Claude CLI v2.0.76 against `/v1/messages`

The literal value is the User-Agent string emitted by `claude --print "hello"` running through `mitmdump --set flow_detail=4 -p 8888` with mitmproxy CA bundle injected via `SSL_CERT_FILE` / `NODE_EXTRA_CA_CERTS`. The capture is in the Loom project — written by the same author (Brent) before the tailnet-microservices repo existed.

### Empirical confirmation later

The May 5 session (`2026-05-05T19-59-37-540Z_019df9b9-...jsonl`, line 94 — a re-read of `specs/generic-client-support.md` from 2026-02-12) records:

> **Eliminated suspects:** OAuth token scopes (verified correct), `system` field format (plain string works), **user-agent version (tested 2.0.76 and 2.1.39)**, `claude-code-20250219` beta flag, HTTP protocol version (HTTP/1.1 and HTTP/2), tool schema structure beyond name.

So the `2.0.76` value continued to be tested empirically against `/v1/messages` and confirmed to work alongside the more current `2.1.39`. There is no session evidence that anyone ever rotated `USER_AGENT` after the original mitmproxy capture — the constant has stayed at `2.0.76` even though, by May 2026, the local `claude` binary reports version `2.1.128` (line 115 of the same session: `2.1.128 (Claude Code) ... Login method: Claude Max account`).

### Negative findings

- No session evidence of npm tarball grep, binary string extraction (`strings`), or Discord/gist screenshot for the `USER_AGENT` value.
- No Codex (`~/.codex/sessions/2026/02/...`) sessions on Feb 7–9 contain the literal.
- The Feb 8 Ralph parent-session jsonls are not present in `~/.claude/projects/-Users-basher8383-dev-personal-ralph-tailnet-microservices/` at the top level (only per-task subagent logs are stored). The session that wrote `f97daa9` "spec: new spec" itself is therefore not directly inspectable; we can only confirm via grep that every Feb 8 child agent was *consuming*, not *producing*, the literal.

---

## 2. `ANTHROPIC_BILLING_HEADER = "cc_version=2.1.128.f82; cc_entrypoint=sdk-cli; cch=00000;"` provenance

**Strong, fully-documented provenance.** Captured live by Brent + a Codex (gpt-5.5) Pi session on 2026-05-05 between 21:38 and 21:42 EST, by reading Claude Code's own `--debug-file` output. The full trail is in `/Users/basher8383/.pi/agent/sessions/--Users-basher8383-3I-forge-tailnet-microservices--/2026-05-05T19-59-37-540Z_019df9b9-8244-70ab-8504-1f9658a3478c.jsonl`.

### Capture procedure (verbatim from session)

The agent ran a matrix of `claude -p` invocations with `--debug-file` to a tempfile, then tailed the debug log. Line 175 (timestamp `2026-05-05T21:38:42.915Z`) shows the tool call:

```
bash {"command": "for src in none '' user; do echo ===$src===; tmp=$(mktemp);
   timeout 35s claude -p --output-format json --no-session-persistence
   --tools \"\" --model haiku --setting-sources \"$src\"
   --debug-file \"$tmp\" 'Reply exactly: ok' > /tmp/cc-$src.out 2>/tmp/cc-$src.err;
   echo CODE=$?; echo OUT; head -20 /tmp/cc-$src.out;
   echo ERR; head -40 /tmp/cc-$src.err;
   echo DEBUGTAIL; tail -30 \"$tmp\"; done", "timeout": 130}
```

The tool result on line 176 (timestamp `2026-05-05T21:38:52.720Z`) contains the captured debug line **emitted by Claude Code itself**:

> ```
> 2026-05-05T21:38:45.165Z [DEBUG] Tool search disabled for model 'claude-haiku-4-5-20251001': model does not support tool_reference blocks. ...
> 2026-05-05T21:38:45.165Z [DEBUG] attribution header x-anthropic-billing-header: cc_version=2.1.128.f82; cc_entrypoint=sdk-cli; cch=00000;
> 2026-05-05T21:38:45.165Z [DEBUG] Fast mode unavailable: Fast mode is not available in the Agent SDK
> ```

The agent then ran a sanity check via direct `curl` to the proxy with that header (line 184, timestamp `2026-05-05T21:39:22.706Z`):

```
curl -sS -D /tmp/proxy-billing-test.headers ... \
  https://anthropic-oauth-proxy.tailfb3ea.ts.net/v1/messages \
  -H 'Content-Type: application/json' \
  -H 'x-anthropic-billing-header: cc_version=2.1.128.f82; cc_entrypoint=sdk-cli; cch=00000;' \
  -H 'anthropic-dangerous-direct-browser-access: true' \
  -d '{...}'
```

…which returned 200, confirming the header was the missing discriminator. The agent's text reply on line 214 (`2026-05-05T21:41:14.950Z`) summarizes:

> Non-bare Claude Code succeeds and debug showed the missing discriminator:
> ```
> x-anthropic-billing-header: cc_version=2.1.128.f82; cc_entrypoint=sdk-cli; cch=00000;
> ```
> Then I tested your proxy directly with that header: ...
> Result: **200 OK** through your proxy, no extra-usage rejection.
> So the missing thing was not `--bare`; it was Claude Code's billing attribution header.

Commit `df6435d` ("fix: route OAuth requests as Claude Code usage") then patched `provider_impl.rs` and `main.rs` to inject the literal centrally (line 205–208 show the edit transcripts).

### Source identity for each field

- `cc_version=2.1.128.f82` — the `2.1.128` matches the locally-installed Claude Code version. Line 115 (`2026-05-05T20:41:45.378Z`) of the same session captures the version probe: `claude --version` → `2.1.128 (Claude Code) ... Login method: Claude Max account`. The `.f82` build-suffix component was not separately verified — it appeared verbatim in the `[DEBUG] attribution header ...` line and was copied as-is into the constant.
- `cc_entrypoint=sdk-cli` — emitted by Claude Code in its non-interactive (`-p` / SDK) mode. The session does not investigate whether interactive `claude` would emit something else, but the constant matches the entrypoint of the code path the proxy actually serves (SDK-style headless calls from Pi/forgeflare).
- `cch=00000` — see §3 below.

### Attempts that failed (worth noting for future capture)

The agent also tried to extract the literal directly from the `claude` binary (line 181, `2026-05-05T21:39:08.702Z`):

```
rg -n "x-anthropic-billing-header|cc_entrypoint|source=sdk|x-client-request-id|ANTHROPIC_CUSTOM_HEADERS|billing-header" \
  /Users/basher8383/.local/share/claude/versions/2.1.128 /Users/basher8383/.local/bin/claude -S
```

…and the followup `strings | rg ...`. Tool result on line 182:

> ```
> /Users/basher8383/.local/share/claude/versions/2.1.128: binary file matches (found "\0" byte around offset 5)
> /Users/basher8383/.local/bin/claude: binary file matches (found "\0" byte around offset 5)
> ```

i.e. `rg` refused to extract strings (they appear to be obfuscated/minified inside the bundled Node binary). **No npm tarball was inspected.** The literal value was therefore not reverse-engineered from binary; it came purely from the runtime debug log line that Claude Code itself prints when constructing the `[DEBUG] attribution header` line.

### Documentation that was added afterwards

The commit also added a documentation block (e.g. line 497 of the same session shows the README addition):

> When the Claude CLI updates, the required headers may change. To discover the current header contract:
> ```
> # Install the updated Claude CLI, then sniff traffic
> mitmdump --set flow_detail=4 -p 8888
> HTTPS_PROXY=http://127.0.0.1:8888 claude --print "hello"
> ```
> Compare the captured headers against the constants in `services/oauth-proxy/src/provider_impl.rs`. Update the constants and run tests if anything has changed.

Note this is **the recommended future-maintenance procedure**, not the procedure that was actually used in May 2026 — that one used `--debug-file` instead of mitmproxy.

---

## 3. `cch=00000` field semantics — **RESOLVED 2026-05-31 via binary decompilation**

**`cch=00000` is a hardcoded constant string in Claude Code. It carries no account, session, or request data and cannot flag or identify an account.**

### How it was resolved

The earlier audit treated the `claude` binary as un-extractable (the Feb/May `rg` attempts choked on null bytes). That was wrong: `strings` on the current **v2.1.158** Node-SEA Mach-O binary (`~/.local/share/claude/versions/2.1.158`) cleanly recovers the minified builder. The attribution header is assembled by one function, `po_(H)`:

```js
// builder po_(H); H = build-hash suffix ("c5c"@2.1.158, "f82"@2.1.128, "9a9"@2.1.132)
function po_(H) {
  if (yK(process.env.CLAUDE_CODE_ATTRIBUTION_HEADER)) return "";   // kill-switch: emit nothing
  _ = `${VERSION}.${H}`;                                 // cc_version = "2.1.158.c5c"
  q = process.env.CLAUDE_CODE_ENTRYPOINT ?? "unknown";   // cc_entrypoint = "sdk-cli"
  K = Wq();                                              // deployment discriminator (env-based)
  T = !(K==="bedrock" || K==="anthropicAws" || K==="mantle") ? " cch=00000;" : "";
  $ = uo_(); z = $ ? ` cc_workload=${$};` : "";          // workload tag; absent in normal use
  Y = `x-anthropic-billing-header: cc_version=${_}; cc_entrypoint=${q};${T}${z}`;
  return N(`attribution header ${Y}`), Y;
}
// Wq() = CLAUDE_CODE_USE_BEDROCK?"bedrock" : ...FOUNDRY?"foundry" : ...ANTHROPIC_AWS?"anthropicAws"
//        : ...MANTLE?"mantle" : ...VERTEX?"vertex" : "firstParty"   <-- our Max/OAuth path
```

### What this establishes

- **`cch=00000` is a literal** — the source contains the fixed string `" cch=00000;"`. It is not computed, hashed, counted, or derived from the account/token/request. It is the same five zeros for every Claude Code user.
- It occurs exactly **twice** in the 215 MB binary (once here, once in unrelated AWS-SDK code). No code path assigns `cch` any other value.
- It is appended for the **`firstParty`** path (plain `api.anthropic.com`, i.e. Max/OAuth + API-key) and omitted only for `bedrock` / `anthropicAws` / `mantle` deployments.
- **Cross-version stable:** the debug attribution line shows `cch=00000` unchanged on v2.1.128 (`.f82`), v2.1.132 (`.9a9`), and v2.1.158 (`.c5c`). Only the `cc_version` build-hash suffix moves between releases. (v2.1.158 fresh capture this session: `cc_version=2.1.158.c5c; cc_entrypoint=sdk-cli; cch=00000;`.)
- **Official kill-switch exists:** genuine Claude Code emits no attribution header at all when `CLAUDE_CODE_ATTRIBUTION_HEADER` is truthy — so the header is optional first-party telemetry, not a hard auth requirement. (Its *presence* still flipped Max-plan routing in the May-5 proxy curl test, so Anthropic's billing classifier does consume it when present.)
- `cc_entrypoint` is pure `process.env.CLAUDE_CODE_ENTRYPOINT` (hence `sdk-cli` for headless `-p`/SDK calls — exactly the path this proxy serves). `cc_workload` only appears under an AsyncLocalStorage workload tag (e.g. `cron`); absent for normal traffic.

### On-wire confirmation (RESOLVED 2026-05-31) — genuine CC does NOT send this header

A fresh mitmproxy capture of **Claude Code v2.1.158** routed through `mitmdump`
(`scripts/capture-cc-headers.sh`) settled the wire question. The actual
`POST /v1/messages` request carried:

```
User-Agent: claude-cli/2.1.158 (external, sdk-cli)
x-app: cli
anthropic-version: 2023-06-01
anthropic-beta: oauth-2025-04-20,interleaved-thinking-2025-05-14,thinking-token-count-2026-05-13,context-management-...
anthropic-dangerous-direct-browser-access: true
```

…and **did NOT contain `x-anthropic-billing-header`.** So the `[DEBUG] attribution
header ...` line that `po_()` logs is **debug-only** — current Claude Code builds
the attribution string but does not put it on the wire for the OAuth `/v1/messages`
path. This corroborates the v2.1.132 May-9 capture (it was NOT a capture artifact),
across two independent versions.

### Safety verdict

`cch=00000` **cannot flag or identify the Max account** — it is a hardcoded
constant with no account-specific bits (proven by decompilation above). That is the
answer to the original concern, and it is unchanged.

What the wire capture *corrects*: the proxy is **not** "faithfully replicating
current genuine Claude Code wire behavior." Genuine CC v2.1.158 does not send
`x-anthropic-billing-header` on `/v1/messages` at all. The proxy injects it anyway —
a benign extra header (the May-5 curl test showed its presence returned 200 and did
not trigger extra-usage classification). It is harmless, but it is an *addition*
relative to real CC, not a mirror of it. No flagging observed in 3+ weeks of Max
usage as of 2026-05-31.

### What `cch` abbreviates

Still not literally spelled out in the bundle, but moot: the shipping value is a
fixed `00000` placeholder (a reserved attribution sub-field zeroed in release
builds, sibling to `cc_version`/`cc_entrypoint`/`cc_workload`). Not security-relevant
either way — and it never reaches the wire from genuine CC regardless.

### Other wire drift noticed in the same capture (out of scope, logged for later)

Genuine CC v2.1.158 vs. what this proxy injects (`provider_impl.rs`):
- **User-Agent:** real CC sends `claude-cli/2.1.158 (external, sdk-cli)`; proxy hardcodes `claude-cli/2.0.76 ...`. Old but accepted.
- **`x-app: cli`** — real CC sends it; proxy does not inject it.
- **anthropic-beta** — real CC now includes `thinking-token-count-2026-05-13`; proxy's `REQUIRED_BETA_FLAGS` does not.
These are not security issues (requests succeed) but are fidelity gaps if exact CC mimicry is ever required.

> **Update 2026-07-02 (fresh on-wire capture, CC v2.1.198).** Re-ran the capture against genuine Claude Code **2.1.198**; on-wire `POST /v1/messages` carried `User-Agent: claude-cli/2.1.198 (external, sdk-cli)`, `x-app: cli`, and `anthropic-beta: oauth-2025-04-20,interleaved-thinking-2025-05-14,thinking-token-count-2026-05-13,context-management-2025-06-27,prompt-caching-scope-2026-01-05,claude-code-20250219,advisor-tool-2026-03-01,advanced-tool-use-2025-11-20,extended-cache-ttl-2025-04-11,cache-diagnosis-2026-04-07` (still no `x-anthropic-billing-header` on the wire). Resolutions:
> - **User-Agent drift RESOLVED** — bumped `USER_AGENT` 2.0.76 → 2.1.198 to lock-step with `cc_version` (also bumped 2.1.158 → 2.1.198.bb7).
> - **`x-app: cli` — still not injected** by the proxy (open fidelity gap).
> - **anthropic-beta — all 10 now forced (was 3).** Tiers 1–3 promoted 2026-07-02; the proxy mirrors genuine CC 2.1.198's full beta set. Per-flag cause/effect + streaming/smoke evidence: `docs/audits/anthropic-beta-flags.md`.

> Maintenance / re-run: use **`scripts/capture-cc-headers.sh`** (or `mise run
> headers:capture`) after a Claude Code upgrade. It captures both the
> `[DEBUG] attribution header` line and the real on-wire `/v1/messages` headers and
> diffs them against the proxy constants. The constants live in
> `services/oauth-proxy/src/provider_impl.rs` and are mirrored in
> `~/.pi/agent/models.json`.

---

## Sources reviewed
Repo Pi sessions, broader Pi session index, Feb 2026 Claude Code subagents, Feb 2026 Codex sessions, the Loom sister project, and Entire CLI checkpoint lookups were reviewed. Key source paths are cited inline above; no Entire-Checkpoint trailer existed for the relevant commits.
