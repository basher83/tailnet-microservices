0a. Study `specs/*` with up to 250 parallel Sonnet subagents to learn the service specifications.
0b. Study @IMPLEMENTATION_PLAN.md (if present) to understand the plan so far.
0c. Study `crates/` and `services/` with up to 250 parallel Sonnet subagents to understand existing code.

1. Study @IMPLEMENTATION_PLAN.md (if present; it may be incorrect) and use up to 500 Sonnet subagents to study existing source code in `crates/` and `services/` and compare it against `specs/*`. Use an Opus subagent to analyze findings, prioritize tasks, and create/update @IMPLEMENTATION_PLAN.md as a bullet point list sorted in priority of items yet to be implemented. Ultrathink. Consider searching for TODO, minimal implementations, placeholders, and inconsistent patterns. Study @IMPLEMENTATION_PLAN.md to determine starting point for research and keep it up to date with items considered complete/incomplete using subagents.

IMPORTANT: Plan only. Do NOT implement anything. Do NOT assume functionality is missing; confirm with code search first. Prefer consolidated, idiomatic implementations in `crates/common/` over ad-hoc copies.

ULTIMATE GOAL: Evolve the anthropic-oauth-proxy from a static header injector into a full OAuth 2.0 gateway with subscription pooling. The gateway manages its own OAuth credentials: PKCE auth, automatic token refresh, round-robin subscription pool with quota failover, and the full Anthropic header contract. Clients send bare requests; the gateway handles everything. See specs/anthropic-oauth-gateway.md for all requirements. Previous specs (oauth-proxy.md, operator-migration.md) are Complete — do NOT re-implement them.
