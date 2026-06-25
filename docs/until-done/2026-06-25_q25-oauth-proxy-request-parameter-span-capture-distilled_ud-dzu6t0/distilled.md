# Q25 — OAuth-Proxy Request-Parameter Span Capture (distilled)

## Problem
Phoenix observability had zero visibility into model request parameters (thinking config, sampling settings, request shape) because Claude Code's OTel span schema omits them and the proxy spans recorded only routing metadata. The OAuth proxy is the single chokepoint where every `/v1/messages` request flows, so instrumenting it once yields coverage across all traffic.

## Solution shape (as built)
- New module `services/oauth-proxy/src/params.rs`: `ProxyParams` struct (14 nullable fields) + `extract(Option<&serde_json::Value>) -> Self` reading off the **existing** pristine `parsed_body` (proxy.rs:269, OAuth-mode pre-mutation parse), + `write_into(&mut Map)` emitting native-typed JSON.
- `SpanRecorder` (proxy.rs) gains a `params` field, populated via `ProxyParams::extract(parsed_body.as_ref())` **before** the clone+mutate at :298/:302; `impl Drop` → `record_span` → new pure `build_metadata` helper merges params into the `metadata` JSON dict Phoenix `load_json_strings` parses.
- Fields land as literal dotted `metadata.proxy.*` keys with native int/bool/float types (non-stringified), so `metadata.proxy.max_tokens > 8000` filters work downstream.

## Key learnings
- **`proxy.effort` does not exist on the wire.** G1 live capture (mitmproxy + `claude --model sonnet`, CC 2.1.191) showed top-level keys `[context_management, diagnostics, max_tokens, messages, metadata, model, output_config, stream, system, thinking, tools]` — no `effort`/`reasoning`. Dropped the field (documented in gate §Notes); clippy `-D warnings` enforces no dead member.
- **Thinking ships as `{type: "adaptive"}` with no `budget_tokens`** on modern CC — superseding the handoff's assumed `type=="enabled"` + `budget_tokens`. So `thinking_enabled` = *present AND type != "disabled"* (true for adaptive); `thinking_budget_tokens` reads null on the adaptive wire (no fabricated default).
- The gate's V1 bar (`fmt --check && clippy -D && build && test`) equals `mise run check`, **not** the full `mise run ci` task (which adds audit/build:release/k8s:validate).

## Gotchas to avoid on rebuild
- A literal `thinking.type == "enabled"` check is vacuously false on all real CC traffic (it's `adaptive`).
- Read `parsed_body`, never the mutated clone — OAuth mode injects a system prompt, so post-mutation `system_present`/`messages_count` misreport the client.
- No second `from_slice`: reuse the existing OAuth parse. `MAX_BODY_SIZE` (10 MiB) already bounds the body.
- `mise run ci` may fail on transitive CVEs (e.g. RUSTSEC-2026-0185 in `quinn-proto` via `reqwest`) unrelated to a change that adds no deps — fall back to the V1 bar with that noted.
- Binary crate has no lib target → unit tests must be inline; used `#[cfg(test)] #[path = "..."]` sibling test files to keep each file ≤200 LOC.

## Surfaces that mattered
- `scripts/capture-cc-headers.sh` / mitmproxy (pipx via mise) + `claude` CLI on PATH — the sanctioned wire-capture for G1 (extended with a body-dump addon).
- `services/oauth-proxy/src/{proxy.rs,telemetry.rs}`; `docs/audits/header-provenance.md` (on-wire beta flags).

## Follow-up tasks
- **P1 (operator-terminal):** confirm `metadata.proxy.*` land in Phoenix as filterable native types post-deploy; verify `thinking_*` populate on thinking-capable models, null otherwise.
- Consider capturing `thinking.type` verbatim (string) for sharper discrimination than the bool — the adaptive/enabled distinction is currently lost.
- Per the why-record revisit trigger: assess whether request-side params suffice to tune evals/workflows, or whether response-side metrics / per-domain attribution are needed (both deferred by this gate's carve).
- Address RUSTSEC-2026-0185 (`reqwest`/`quinn-proto` bump) separately so `mise run ci` audit is green.