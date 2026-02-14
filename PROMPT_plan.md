0a. Study `specs/*` with up to 250 parallel Sonnet subagents to learn the service specifications.
0b. Study @IMPLEMENTATION_PLAN.md (if present) to understand the plan so far.
0c. Study `crates/` and `services/` with up to 250 parallel Sonnet subagents to understand existing code.

1. Study @IMPLEMENTATION_PLAN.md (if present; it may be incorrect) and use up to 500 Sonnet subagents to study existing source code in `crates/` and `services/` and compare it against `specs/*`. Use an Opus subagent to analyze findings, prioritize tasks, and create/update @IMPLEMENTATION_PLAN.md as a bullet point list sorted in priority of items yet to be implemented. Ultrathink. Consider searching for TODO, minimal implementations, placeholders, and inconsistent patterns. Study @IMPLEMENTATION_PLAN.md to determine starting point for research and keep it up to date with items considered complete/incomplete using subagents.

IMPORTANT: Plan only. Do NOT implement anything. Do NOT assume functionality is missing; confirm with code search first. Prefer consolidated, idiomatic implementations in `crates/common/` over ad-hoc copies.

ULTIMATE GOAL: Implement remaining Active specs for the OAuth gateway. The core gateway is code-complete (PKCE, token refresh, subscription pooling, quota failover, full header contract — see specs/anthropic-oauth-gateway.md, Complete). Current Active work: replace the wall-clock timeout with a three-phase idle timeout model for SSE streaming (specs/streaming-timeout-fix.md). Completed specs (oauth-proxy.md, operator-migration.md, operator-migration-addendum.md, anthropic-oauth-gateway.md, rand-0.10-migration.md, generic-client-support.md) — do NOT re-implement them. Check specs/README.md for current status of all specs.

999999999. Keep @IMPLEMENTATION_PLAN.md current with learnings using a subagent — future work depends on this to avoid duplicating efforts.

9999999999. When you learn something new about how to run the application, update @AGENTS.md using a subagent but keep it brief.

99999999999. For any bugs you notice, document them in @IMPLEMENTATION_PLAN.md using a subagent even if unrelated to the current planning work.

999999999999. IMPORTANT: Keep @AGENTS.md operational only — status updates and progress notes belong in IMPLEMENTATION_PLAN.md. A bloated AGENTS.md pollutes every future loop's context.
