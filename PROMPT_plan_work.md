0a. Study `specs/*` with up to 250 parallel Sonnet subagents to learn the service specifications.
0b. Study @IMPLEMENTATION_PLAN.md (if present) to understand the plan so far.
0c. Study `crates/` and `services/` with up to 250 parallel Sonnet subagents to understand existing code.

1. You are creating a SCOPED implementation plan for work: "${WORK_SCOPE}". Study @IMPLEMENTATION_PLAN.md (if present; it may be incorrect) and use up to 500 Sonnet subagents to study existing source code in `crates/` and `services/` and compare it against `specs/*`. Use an Opus subagent to analyze findings, prioritize tasks, and create/update @IMPLEMENTATION_PLAN.md as a bullet point list sorted in priority of items yet to be implemented. Ultrathink. Consider searching for TODO, minimal implementations, placeholders, and inconsistent patterns.

IMPORTANT: This is SCOPED PLANNING for "${WORK_SCOPE}" only. Create a plan containing ONLY tasks directly related to this work scope. Be conservative — if uncertain whether a task belongs to this work, exclude it. Do NOT assume functionality is missing; confirm with code search first. Prefer consolidated, idiomatic implementations in `crates/common/` over ad-hoc copies.

ULTIMATE GOAL: We want to achieve the scoped work "${WORK_SCOPE}". Consider missing elements related to this work and plan accordingly. See specs/anthropic-oauth-gateway.md for overall gateway requirements, but plan ONLY the subset relevant to the scoped work.

999999999. Keep @IMPLEMENTATION_PLAN.md current with learnings using a subagent — future work depends on this to avoid duplicating efforts.

9999999999. When you learn something new about how to run the application, update @AGENTS.md using a subagent but keep it brief.

99999999999. For any bugs you notice, document them in @IMPLEMENTATION_PLAN.md using a subagent even if unrelated to the current planning work.

999999999999. IMPORTANT: Keep @AGENTS.md operational only — status updates and progress notes belong in IMPLEMENTATION_PLAN.md. A bloated AGENTS.md pollutes every future loop's context.
