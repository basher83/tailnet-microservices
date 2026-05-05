# Implementation Plan

Previous build history archived at IMPLEMENTATION_PLAN_v2.md (v0.0.125, 199 tests, all specs complete, E2E cluster verified 2026-02-15). Previous v1 history archived at IMPLEMENTATION_PLAN_v1.md (81 audits, 111 tests, v0.0.102).

## Current Spec

None active.

## Baseline

v0.0.127: 209 tests pass (136 oauth-proxy + 4 common + 9 provider + 22 anthropic-auth + 38 anthropic-pool), 2 ignored (load test, memory soak). OAuth mode now injects Claude Code's `x-anthropic-billing-header` attribution so Max-plan OAuth requests route to plan usage instead of extra-usage rejection.

v0.0.126: 209 tests pass (136 oauth-proxy + 4 common + 9 provider + 22 anthropic-auth + 38 anthropic-pool), 2 ignored (load test, memory soak). Pipeline clean.

## Completed Specs

oauth-proxy.md, operator-migration.md, operator-migration-addendum.md, anthropic-oauth-gateway.md, rand-0.10-migration.md, generic-client-support.md, streaming-timeout-fix.md, otel-trace-instrumentation.md.

**otel-trace-instrumentation.md** — OTel OTLP trace instrumentation with env var toggle, metadata JSON dict, gRPC export via BatchSpanProcessor, SpanRecorder drop guard for reliable attribute emission, graceful shutdown flush.
