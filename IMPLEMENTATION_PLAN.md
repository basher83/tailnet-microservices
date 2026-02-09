# Implementation Plan

Previous build history archived at IMPLEMENTATION_PLAN_v1.md (81 audits, 111 tests, v0.0.102, E2E verified 2026-02-06). Operator migration history (v0.0.107–v0.0.114) was in the previous version of this file.

## Current Spec

`specs/anthropic-oauth-gateway.md` (Draft) — evolve the proxy from static header injector to full OAuth 2.0 gateway with subscription pooling.

## Phase Tracking

See spec for full phase details. Implementation has not started.

- **Phase 1: Provider Abstraction + Mode Detection** — not started
- **Phase 2: OAuth Foundation** — not started
- **Phase 3: Subscription Pool** — not started
- **Phase 4: Gateway Integration** — not started
- **Phase 5: Admin API** — not started
- **Phase 6: Deployment** — not started

## Baseline

v0.0.114: 86 tests pass (82 oauth-proxy + 4 common), 2 ignored (load test, memory soak). Pipeline clean. `cargo fmt --all --check` clean, `cargo clippy --workspace -- -D warnings` clean.
