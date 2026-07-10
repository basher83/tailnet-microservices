# Repository Orientation Audit

Generated: 2026-07-09 | Commit: `4abf487` | Scope: repository structure, load-bearing code, history signals, and orientation risks

## Executive Summary

Tailnet Microservices is a spec-driven Rust workspace that produces one deployable binary, `anthropic-oauth-proxy`. The service accepts unauthenticated tailnet requests, manages Claude Max OAuth credentials, selects accounts from a round-robin pool with quota failover, rewrites requests to resemble Claude Code, and streams Anthropic API responses back to clients.

The implementation has a clear architectural seam around the `Provider` trait, but most operational risk converges in three places: the proxy request loop, OAuth account/token state, and the empirically maintained Claude Code wire contract. The source baseline is healthy under local deterministic checks, while live vendor behavior, cluster state, security auditing, and release-manifest validation were outside this audit.

## Scope and Evidence Standard

This audit examined the current implementation, tests, specifications, runbooks, Kubernetes manifests, CI workflow, and reachable Git history. Documentation and commit messages were treated as claims to verify, not as evidence by themselves.

A tooling incident occurred during parallel scouting: two async `scout` tasks inherited the same `context.md` output path, and the later history report overwrote the earlier architecture report. The architecture report was recovered from the child transcript. Its claims were not accepted wholesale: useful observations were re-opened in the current source, while an incorrect claim that OAuth failover lacked HTTP-level integration coverage was discarded after checking the complete `main.rs` test module. This incident is itself a practical example of the evidence standard used here.

Findings use these annotations:

| Annotation | Meaning |
|---|---|
| **Verified** | Directly established from current files, Git object relationships, or a command run during the audit. |
| **Observed drift** | Two current repository surfaces make conflicting claims. |
| **Risk / inference** | The implementation permits a concerning condition, but impact depends on external behavior or runtime concurrency not reproduced here. |
| **Historical signal** | Supported by commits and diffs; useful for orientation but not necessarily a current defect. |

## Repository Purpose and Structure

The Cargo workspace contains four library crates and one binary crate:

| Path | Responsibility | Load-bearing notes |
|---|---|---|
| `services/oauth-proxy/` | Axum service, request proxying, admin API, configuration, telemetry | Sole deployable binary and primary integration surface. |
| `crates/provider/` | Authentication-provider abstraction and passthrough implementation | `Provider` is the architectural boundary between generic proxy behavior and Anthropic OAuth behavior. |
| `crates/anthropic-auth/` | PKCE, token exchange/refresh, credential persistence | Owns durable OAuth token correctness and file permissions. |
| `crates/anthropic-pool/` | Account selection, cooldown, disabling, quota classification, proactive refresh | Owns failover and account state transitions. |
| `crates/common/` | Shared error types | Small support crate. |
| `k8s/` | Deployment, PVC, Services, Tailscale Ingress, generated configuration | Defines the production process boundary and credential persistence model. |
| `specs/` | Current and historical design records | `specs/README.md` must be consulted before treating an individual spec as current. |
| `docs/runbook/` | Operator procedures | Current operational entry point. |
| `docs/audits/` | Forensic evidence and drift reports | Important context, but some narratives preserve earlier states. |

The request path is:

```text
Tailnet client
  → Tailscale Ingress
  → Axum fallback handler
  → proxy::proxy_request
  → Provider::prepare_request
  → AnthropicOAuthProvider
  → Pool::select / CredentialStore / token refresh
  → Anthropic API
  → idle-timeout-wrapped response stream
```

The separate admin listener is exposed through a ClusterIP Service on port 9090 and is not routed by the Tailscale Ingress. The committed runbook accesses it through `kubectl port-forward`.

## Load-Bearing Code

`services/oauth-proxy/src/proxy.rs` is the core transaction engine. It strips transport headers, enforces the request body limit, parses OAuth-mode JSON, invokes provider mutation, retries initial-response timeouts, performs quota failover, streams successful responses, and records metrics and spans. A change here can affect HTTP framing, account transitions, retries, telemetry, and SSE behavior simultaneously.

`services/oauth-proxy/src/provider_impl.rs` is the compatibility contract with Anthropic. It injects OAuth authorization, ten `anthropic-beta` flags, Claude Code identity headers, the required system prompt, and Pi-specific prompt sanitization. Unlike ordinary internal configuration, these constants and transformations derive from observed vendor behavior and can drift without a compiler or unit test failure.

`crates/anthropic-pool/src/pool.rs`, `refresh.rs`, and `quota.rs` collectively decide which account is charged, when an account cools down or becomes disabled, and whether a refresh failure is permanent. `crates/anthropic-auth/src/token.rs` and `credentials.rs` are equally critical because token classification and atomic persistence determine whether the pool can recover.

`k8s/kustomization.yaml` and `.github/workflows/ci.yml` form the deployment handoff. CI publishes an immutable SHA image and commits the corresponding tag back to `main`; external ArgoCD reconciliation consumes `k8s/` afterward.

## Git History Signals

The reachable `main` history begins with `fc62c6a` on 2026-02-06. The first day contains a dense Ralph-style implementation and audit sequence, ending with `d5089af`, which declared the original project complete after 81 audits. This is evidence of iterative spec reconciliation, not evidence that present behavior still matches those early completion claims.

The architecture changed materially on 2026-02-08. Commit `b8991c1` removed the embedded tailscaled sidecar and roughly 1,850 lines, shifting connectivity to the Tailscale Operator. OAuth then arrived in phases: `5c26ee4` added the auth foundation, `c5fa894` added the pool, and `3739e73` integrated the gateway. Commit `3067e4f` enabled OAuth pool mode operationally, while `0d3d908` fixed request-framing and client-auth leakage discovered against the real upstream.

Later work is more operational and empirical. Commit `0889c10` added OTel/Phoenix tracing, `6a5f812` corrected refresh-failure classification, and `84fd062` added content-free request parameter capture. The 2026-07-02 sequence `0fb754b`, `438561d`, `17e7b5d`, `70fe9ea`, and `fc871b5` synchronized the outbound request fingerprint with Claude Code 2.1.198.

History consumers must account for rewritten/replayed early lineage. The repository has 126 `v0.0.*` tags, but only `v0.0.123` through `v0.0.126` are ancestors of current `main`. Several detached tagged commits have tree-identical reachable replacements with different parents; for example, tagged `16f3cba` and reachable `3739e73` have the same tree. The unrelated `entire/checkpoints/v1` branch has no merge base with `main` and should be treated as session metadata rather than product history.

## Annotated Findings

### ORIENT-01 — Integration concentration in `main.rs`

**Annotation:** Verified | **Impact:** Maintainability | **Priority:** Medium

`services/oauth-proxy/src/main.rs` is 3,488 lines. It contains the process bootstrap and a large integration-test module, making the service entry point the largest Rust file by a wide margin.

This is not evidence of a runtime defect. It does raise navigation and review cost because startup behavior and broad integration fixtures share one compilation unit. A future cleanup could move integration fixtures into focused test modules without changing behavior.

### ORIENT-02 — Nominal unit tests contact the real Anthropic token endpoint

**Annotation:** Verified | **Impact:** Test determinism and external side effects | **Priority:** Medium

`crates/anthropic-auth/src/token.rs` includes `exchange_code_rejects_invalid_code` and `refresh_token_rejects_invalid_token`. Both construct a normal `reqwest::Client` and call the configured real token endpoint with bogus credentials.

The tests passed during this audit, but their result depends on network reachability and vendor behavior. They also generate external requests during `cargo test --workspace`. A local mock token endpoint or injectable endpoint would make the suite hermetic while preserving classification coverage.

### ORIENT-03 — Inline and background refresh are not deduplicated per account

**Annotation:** Risk / inference | **Impact:** OAuth account availability | **Priority:** Investigate

`Pool::select()` can refresh an expiring credential inline, while `refresh_cycle()` can refresh the same credential in the background. Both read a credential, call the token endpoint, then persist returned tokens; there is no visible per-account refresh lock or single-flight mechanism spanning the two paths.

No failure was reproduced. The impact depends on Anthropic refresh-token rotation and concurrent timing. If refresh tokens are single-use or rotate on every successful refresh, concurrent refreshes could cause one request to persist stale or rejected state. This deserves a deterministic concurrency test before deciding whether synchronization is needed.

### ORIENT-04 — MIT is declared without a tracked license text

**Annotation:** Verified | **Impact:** Distribution hygiene | **Priority:** Low

`Cargo.toml` and `README.md` declare MIT, but no tracked `LICENSE` or `COPYING` file exists. Adding the canonical MIT license text would make the distribution claim self-contained.

### ORIENT-05 — Repository metadata understates the current test count

**Annotation:** Observed drift | **Impact:** Orientation accuracy | **Priority:** Low

The Forge `PIN.md` describes this project as having 198 tests. The current `mise run check` execution passed 231 tests and reported 2 additional ignored tests. This is metadata drift rather than a product defect.

### ORIENT-06 — The header provenance audit contains a stale `x-app` statement

**Annotation:** Observed drift | **Impact:** Wire-contract maintenance | **Priority:** Medium

`docs/audits/header-provenance.md` retains a 2026-07-02 statement that `x-app: cli` is still not injected. Current `services/oauth-proxy/src/provider_impl.rs` defines `X_APP` and injects it in `prepare_request`, introduced by `438561d`.

This is a concrete example of why forensic narratives should identify the historical state they describe and why maintainers must open the implementation before acting on an audit assertion.

### ORIENT-07 — Claude Code fingerprint parity is inherently perishable

**Annotation:** Verified design constraint | **Impact:** Vendor compatibility | **Priority:** Ongoing

The proxy deliberately mirrors Claude Code’s User-Agent, `x-app`, beta flags, and related request behavior. The July campaign verified parity with Claude Code 2.1.198, not with all future releases.

Changes to `REQUIRED_BETA_FLAGS`, `USER_AGENT`, `X_APP`, `ANTHROPIC_BILLING_HEADER`, or system-prompt mutation should not be accepted from prose or copied constants alone. Re-run `mise run headers:capture`, inspect `provider_impl.rs` and its tests, and perform a live non-streaming and streaming smoke before claiming parity.

### ORIENT-08 — Most historical tags do not describe current `main` ancestry

**Annotation:** Verified historical signal | **Impact:** Git archaeology and release interpretation | **Priority:** Low

Of 126 tags, 122 are not merged into current `main`. This does not affect the current tree, but it makes tag-based ancestry and release comparisons misleading. Use reachable commit hashes, tree IDs, and explicit diffs for historical analysis.

### ORIENT-09 — The service state machine only partially drives runtime behavior

**Annotation:** Verified | **Impact:** Maintainability and misleading ownership | **Priority:** Low

`services/oauth-proxy/src/service.rs` defines `Running`, `Draining`, and request/shutdown events, but its own comments state that several events are constructed only in tests. `main.rs` uses the machine for `ConfigLoaded` and `ListenerReady`; actual request tracking and drain enforcement are handled separately by atomics, Axum graceful shutdown, and `tokio::time::timeout`.

The abstraction is not wholly dead: it still produces the listener-start action and preserves startup state transitions. The smell is split ownership—its type model implies responsibility for runtime request and drain behavior that the process does not route through it. Future work should either reconnect the machine to real control flow or narrow it to the startup behavior it actually owns.

### ORIENT-10 — `proxy_request` concentrates several independent correctness concerns

**Annotation:** Verified | **Impact:** Change risk | **Priority:** Medium

`services/oauth-proxy/src/proxy.rs::proxy_request` handles transport-header filtering, body collection and JSON parsing, provider mutation, account failover, timeout retries, response classification, metric emission, and span completion in one large function with many early returns.

The existing RAII guards reduce counter/span omission risk, and the test suite covers substantial behavior. Even so, a future change to error handling or telemetry can require coordinated edits across multiple branches. Extracting response finalization or attempt execution behind behavior-preserving tests would reduce that review surface without introducing a new architectural layer.

## Anti-Overfitting Guidance for Future Agents

A source that points at another source has provided a route, not verification. The minimum confidence ladder for this repository is:

1. Use `README.md`, `specs/README.md`, runbooks, audits, and commit subjects to locate the asserted behavior.
2. Open the named implementation and follow its direct callers and tests.
3. Inspect the introducing or correcting Git diff when intent matters.
4. Run a focused source-level check.
5. For vendor wire behavior, deployment state, or telemetry ingestion, touch the live verification surface before claiming completion.

Confidence should rise only at the level supported by the strongest evidence touched. A passing unit test cannot establish current Claude Code wire parity; a Kubernetes manifest cannot establish live ArgoCD health; a historical audit cannot override current source.

## Validation Performed

The repository was clean and synchronized with `origin/main` before this report was created. The following command passed:

```bash
mise run check
```

That command verified formatting, Clippy with warnings denied, workspace build, and workspace tests. Results were 231 passed and 2 ignored. This audit did not run `cargo audit`, the release-size build, Kubernetes schema validation, header capture, live tailnet smoke, ArgoCD inspection, or Phoenix trace queries.

## Recommended Follow-Up

1. Replace the two real-token-endpoint tests with a local mock or injectable endpoint.
2. Add a concurrent inline/background refresh test that models rotating refresh tokens.
3. Correct the stale `x-app` statement in `header-provenance.md` while preserving its historical chronology.
4. Add a canonical MIT `LICENSE` file.
5. Decide whether to split the `main.rs` integration test module for maintainability.
6. Narrow or reconnect the partially vestigial service state machine.
7. Consider behavior-preserving decomposition of `proxy_request` around attempt execution and response finalization.
8. Refresh the Forge `PIN.md` metrics separately in the workspace repository.
