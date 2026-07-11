# Repository Orientation Findings Remediation Plan

Created: 2026-07-09 | Source audit: [`../audits/REPOSITORY_ORIENTATION_AUDIT_2026-07-09.md`](../audits/REPOSITORY_ORIENTATION_AUDIT_2026-07-09.md) | Source commit: `a8151d2`

## Goal

Work through `ORIENT-01` through `ORIENT-10` in an order that first repairs the evidence used to make decisions, then strengthens deterministic verification, and only afterward restructures load-bearing code. The plan distinguishes findings that require implementation from findings that should be corrected, investigated, or explicitly accepted as historical constraints.

This is an execution order, not a severity ranking. A low-risk documentation correction appears early because incorrect orientation material can contaminate every later phase, while a more important structural cleanup appears later because it should not begin until the behavior it might disturb is well verified.

## Planning Principles

Each phase should leave the repository in a valid, independently committable state. Mechanical movement and behavioral changes should not share a commit. Findings classified as risks must be reproduced before a fix is designed; the plan must not turn a plausible concurrency concern into an asserted defect without evidence.

The Claude Code wire contract is an external runtime contract. Unit tests can verify what the proxy emits, but only capture and live smoke evidence can establish current parity. Likewise, historical Git anomalies should be documented rather than “cleaned up” unless the operator explicitly chooses a history rewrite or tag migration.

## Priority and Phase Map

| Execution order | Finding | Working priority | Phase | Intended disposition |
|---:|---|---|---:|---|
| 1 | `ORIENT-06` stale `x-app` audit statement | P1 | 1 | Correct current documentation while preserving chronology. |
| 2 | `ORIENT-05` stale test-count metadata | P3 | 1 | Update the external Forge index with an as-of qualifier. |
| 3 | `ORIENT-04` missing MIT license text | P2 | 1 | Add the canonical license artifact. |
| 4 | `ORIENT-08` detached historical tags | P3 | 1 | Explicitly accept/document; do not rewrite history by default. |
| 5 | `ORIENT-02` tests contact the real token endpoint | P1 | 2 | Build a deterministic local token-endpoint test seam. |
| 6 | `ORIENT-03` refresh concurrency risk | P1 | 2 | Reproduce deterministically, then fix only if demonstrated. |
| 7 | `ORIENT-07` perishable Claude Code fingerprint | P1 | 3 | Refresh capture evidence and define the compatibility gate. |
| 8 | `ORIENT-01` oversized `main.rs` test concentration | P2 | 4 | Move tests mechanically without changing behavior. |
| 9 | `ORIENT-10` concentrated `proxy_request` logic | P1 | 4 | Decompose behind established tests and live baseline. |
| 10 | `ORIENT-09` partially vestigial service state machine | P2 | 5 | Reconcile the lifecycle model after request flow stabilizes. |

P1 means the finding directly affects verification quality or a load-bearing runtime path. P2 denotes maintainability or repository-hygiene work worth completing but not ahead of behavioral verification. P3 denotes metadata or historical guidance whose primary value is preventing future misinterpretation.

## Phase 1 — Repair the Maps Before Following Them

### Why this phase comes first

The repository is deliberately spec- and audit-led, so stale orientation material has disproportionate leverage. These changes are low-risk, establish trustworthy navigation for later work, and can be completed without touching runtime behavior. They also separate factual cleanup from subsequent engineering changes, keeping later diffs easier to review.

### 1.1 Correct `ORIENT-06`: stale `x-app` provenance statement

Update `docs/audits/header-provenance.md` so its chronological narrative clearly separates the pre-`438561d` state from current behavior. Preserve the original finding as historical evidence, but add an explicit resolution stating that `provider_impl.rs` now injects `x-app: cli`.

Completion evidence:

- The audit no longer presents “still not injected” as current truth.
- The resolution cites `438561d` and `services/oauth-proxy/src/provider_impl.rs`.
- `rg -n "x-app|X_APP" docs/audits services/oauth-proxy/src/provider_impl.rs` shows a consistent present-state claim.

### 1.2 Correct `ORIENT-05`: stale test-count metadata

Update the Forge workspace `PIN.md` entry outside this repository in a separate workspace-level change. Record the count as an as-of value tied to a commit, for example “231 passing, 2 ignored as of `4abf487`,” rather than implying it is a permanently maintained invariant.

This item is intentionally separate from the repository commit because the Forge root is not a Git repository and manages current-state metadata independently.

Completion evidence:

- `mise run check` supplies the count being recorded.
- `PIN.md` includes an as-of commit or date.
- No source change is bundled with the metadata update.

### 1.3 Resolve `ORIENT-04`: add the MIT license artifact

Add a canonical `LICENSE` file matching the MIT declaration in `Cargo.toml` and `README.md`. Confirm the copyright holder and year before committing rather than guessing from package authorship.

Completion evidence:

- A tracked `LICENSE` file exists.
- Cargo metadata, README, and license text agree.
- `git diff --check` passes.

### 1.4 Close `ORIENT-08` as an accepted historical constraint

Do not retag, delete tags, or rewrite history as routine cleanup. The orientation audit already records that 122 of 126 tags are not ancestors of current `main`, that some tagged and reachable commits have identical trees, and that `entire/checkpoints/v1` is metadata history.

Close this finding by adopting the operating rule that archaeology uses reachable `main` commits, tree IDs, and explicit diffs. Any future tag migration should be a separately approved release-management task with a compatibility and remote-impact plan.

Completion evidence:

- The finding has an explicit “accepted/documented” disposition in the execution record.
- No Git references are mutated during this plan.

## Phase 2 — Make OAuth Verification Deterministic

### Why this phase precedes concurrency fixes and refactoring

`ORIENT-03` cannot be evaluated reliably while token tests depend on the real Anthropic endpoint. A controllable endpoint is required to model token rotation, barriers, failures, and request counts. Building that seam first turns a hypothesis into something falsifiable and also removes external traffic from the default test suite.

### 2.1 Resolve `ORIENT-02`: remove real token-endpoint calls from tests

Introduce the smallest test seam that allows token exchange and refresh logic to target a local Axum or TCP test server. Production wrappers should continue using the immutable Anthropic endpoint; test configurability must not become a production configuration option unless a separate requirement calls for it.

A likely shape is an internal endpoint-parameterized helper with existing public functions delegating to the production constant. Reuse existing local-server test patterns before introducing a new dependency or abstraction.

Required cases:

- Successful authorization-code exchange.
- Successful refresh and token rotation.
- `invalid_grant`, 401, and 403 permanent classification.
- 429, 5xx, malformed response, and transport failure behavior.
- Proof that `cargo test --workspace` succeeds without outbound network access.

Completion evidence:

```bash
cargo test -p anthropic-auth
mise run check
```

The two tests that previously sent bogus credentials to Anthropic must no longer contact a public endpoint.

### 2.2 Investigate and resolve `ORIENT-03`: refresh concurrency

Use the deterministic token endpoint to coordinate simultaneous refresh attempts for one account. Test the overlap between request-time selection and proactive refresh, including a server that invalidates a refresh token after first use and rotates it on success.

The first deliverable is evidence, not a lock. Determine whether two refresh calls can be in flight for the same account and whether the resulting persistence order can leave the store with rejected or stale credentials.

If the risk reproduces, implement per-account single-flight refresh coordination. The coordination must serialize refresh for the same account without serializing unrelated accounts, and it must not hold the pool-wide account/status locks across network I/O. Waiting callers should consume the credential produced by the successful refresh rather than issue another refresh immediately.

If the risk does not reproduce because Anthropic’s token semantics or existing synchronization makes it safe, record the discriminator and close the finding without adding machinery.

Completion evidence:

- A deterministic concurrency test demonstrates either the failure or the safety invariant.
- Same-account and different-account concurrency are both covered.
- Permanent and transient failures preserve the classifications established by `6a5f812`.
- `mise run check` passes.

## Phase 3 — Refresh the External Compatibility Baseline

### Why this phase gates structural proxy work

The proxy intentionally imitates Claude Code, and its most important compatibility properties cannot be proven offline. Capturing the current baseline before reorganizing `main.rs` or `proxy_request` creates an external before-state. If later smoke behavior changes, the team can distinguish refactor regression from vendor drift.

### 3.1 Resolve `ORIENT-07`: establish current wire evidence

Run the maintained header capture against the installed Claude Code version and compare it with `REQUIRED_BETA_FLAGS`, `USER_AGENT`, `X_APP`, `ANTHROPIC_BILLING_HEADER`, and system-prompt behavior in `provider_impl.rs`.

Then run one non-streaming and one streaming request through the live proxy. Record the Claude Code version, date, command shape, response status, streaming completion evidence, and any intentional divergence from genuine wire behavior in `docs/audits/`.

Do not blindly synchronize newly observed headers. For each difference, classify whether it is authentication-critical, identity/fingerprint behavior, inert feature gating, or response-shape changing. Response-shape changes require client compatibility evidence before promotion.

Completion evidence:

```bash
mise run headers:capture
mise run live:validate
```

If those commands require unavailable credentials or surfaces, block the phase rather than substituting source assertions for live evidence.

## Phase 4 — Reduce Complexity Without Changing Contracts

### Why `ORIENT-01` comes before `ORIENT-10`

Moving tests is primarily mechanical and reduces the amount of context required to inspect startup code. Decomposing `proxy_request` changes code boundaries inside the most load-bearing function. Keeping those operations in separate commits makes failures attributable and prevents a large “cleanup” diff from hiding behavior changes.

This phase follows the deterministic auth harness and live compatibility baseline so that both internal state and external behavior are protected before structural work begins.

### 4.1 Resolve `ORIENT-01`: split the `main.rs` test concentration

Move the large `#[cfg(test)]` module into focused test modules under `services/oauth-proxy/src/` without broadening production visibility solely to satisfy integration tests. Group fixtures and tests by concern where natural: proxy transport, OAuth provider behavior, health/metrics, timeout/streaming, and concurrency.

Keep this as a movement-only change. Do not rename behavior, alter fixtures, change timeouts, or refactor production functions in the same commit.

Completion evidence:

- `main.rs` becomes a navigable process entry point rather than the dominant test container.
- Test names and effective coverage are preserved.
- `git diff --color-moved` or equivalent review confirms mechanical movement.
- `mise run check` passes with the same pass/ignored counts unless an independently explained test addition occurred earlier.

### 4.2 Resolve `ORIENT-10`: decompose `proxy_request`

Decompose around existing responsibilities rather than inventing a new framework. Candidate seams are attempt execution, response classification/finalization, and common metric/span error completion. Preserve the current `Provider` boundary and avoid introducing another trait unless duplication across real implementations proves it necessary.

Apply one extraction at a time. After each extraction, run focused proxy tests before continuing. Preserve these invariants:

- Three initial-response timeout attempts with fixed backoff.
- Failover only on quota exhaustion.
- Permanent errors disable and return immediately.
- Fresh headers/body per failover attempt.
- Streaming success and passthrough error behavior.
- Request/error/in-flight counters and span completion on every exit.
- Existing error envelope and request ID semantics.

Completion evidence:

```bash
cargo test -p oauth-proxy proxy
cargo test -p oauth-proxy oauth_provider
mise run check
```

After source checks pass, rerun the Phase 3 live non-streaming and streaming smoke. The goal is reduced change surface, not fewer lines at the cost of obscured control flow.

## Phase 5 — Reconcile the Lifecycle Model

### Why this phase is last

The state machine overlaps with startup, request tracking, and shutdown behavior. Changing it while proxy control flow is also moving would make failures difficult to localize. Once `proxy_request` and the test layout are stable, the actual lifecycle ownership is easier to see and the state-machine decision can be made from current call sites rather than historical spec shape.

### 5.1 Resolve `ORIENT-09`: narrow or reconnect the state machine

Inventory every production construction of `ServiceEvent` and every consumed `ServiceAction`. Decide explicitly between two coherent models:

1. **Narrow the machine to startup ownership**—retain only transitions and actions actually driven by `main.rs`, while Axum and atomics remain authoritative for request/drain behavior. This is the recommended default because it matches current runtime ownership.
2. **Reconnect the full lifecycle machine**—route shutdown and drain events through it and make its actions authoritative. Choose this only if it simplifies rather than duplicates Axum’s graceful-shutdown mechanism.

Do not keep test-only events merely because an older spec names them. If historical behavior remains useful, preserve it in the superseded spec or Git history rather than an inactive runtime model.

Completion evidence:

- Every remaining event/action has a production caller or a documented defensive reason.
- Shutdown still stops accepting requests and enforces the five-second drain bound.
- Health, metrics, and in-flight accounting retain current behavior.
- `mise run check` and a SIGTERM shutdown smoke pass.

## Commit and Review Strategy

Use small commits aligned to evidence boundaries:

1. Documentation drift correction.
2. License artifact.
3. Hermetic token endpoint tests.
4. Refresh concurrency reproduction test.
5. Refresh synchronization fix, only if required by the reproduction.
6. Wire-capture audit update.
7. Mechanical test-module split.
8. Incremental `proxy_request` decomposition.
9. Lifecycle state-machine reconciliation.

The Forge `PIN.md` update is outside this repository and should remain a separate workspace-state change. `ORIENT-08` should normally close without a code commit.

## Global Guardrails

- Do not modify `mothership-gitops`; this repository is consumed by it.
- Do not mutate historical tags or rewrite `main` as part of remediation.
- Do not change Claude Code fingerprint constants without capture evidence and client-impact classification.
- Do not add synchronization for `ORIENT-03` until a deterministic test demonstrates the needed invariant.
- Do not combine mechanical file movement with production behavior changes.
- Run `mise run check` after every code phase; use live gates where the claim is inherently runtime-dependent.

## Completion Definition

The plan is complete when every `ORIENT-01` through `ORIENT-10` finding has one of three explicit outcomes: implemented and verified, investigated and disproven with durable evidence, or accepted/documented with a stated reason. Completion does not require eliminating every historical anomaly; it requires that no finding remains ambiguous about ownership, evidence, or next action.
