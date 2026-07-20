# Tailnet Microservices — Agent Guidelines

## Overview

Tailnet Microservices is a Rust workspace that produces one deployable binary, `anthropic-oauth-proxy`. The service accepts unauthenticated tailnet requests, manages Claude Max OAuth credentials, selects accounts from a round-robin pool with quota failover, rewrites requests to match the required Anthropic and Claude Code contracts, and streams upstream responses back to clients.

The Tailscale Operator owns tailnet identity and ingress. The service remains a plain Kubernetes workload with ClusterIP backends. OAuth credentials persist on a PVC.

## Non-negotiable boundaries

- **Do not modify anything in `mothership-gitops`.** ArgoCD syncs this repository at wave 8.
- Do not manually deploy, push commits, or change external infrastructure unless the task explicitly requires it.
- Never log, trace, snapshot, commit, or return OAuth access tokens or refresh tokens.
- Preserve restrictive credential-file permissions and atomic credential writes.
- In OAuth mode, client `Authorization` and `x-api-key` headers must never reach Anthropic.
- Do not expose the admin listener through the Tailscale Ingress or a public Service. The runbook accesses it through `kubectl port-forward`.
- Keep logs, metrics, and traces content-free: do not record prompts, request or response bodies, credentials, or authorization headers.

## Architecture and ownership

The workspace contains four libraries and one binary:

```text
crates/
  common/           Shared configuration/error support
  provider/         Provider trait, error classification, passthrough provider
  anthropic-auth/   OAuth PKCE, token exchange/refresh, credential persistence
  anthropic-pool/   Account selection, cooldown, disabling, quota handling, refresh
services/
  oauth-proxy/      Axum service, admin API, proxy transaction loop, telemetry
specs/              Current and historical design specifications
k8s/                Deployment, Services, Ingress, PVC, and generated configuration
docs/runbook/       Current operational procedures
docs/audits/        Timestamped investigations and compatibility evidence
```

Respect these ownership boundaries:

- Keep generic forwarding, retry, streaming, and response behavior in `services/oauth-proxy/src/proxy.rs`.
- Keep provider-specific authentication and request mutation behind `provider::Provider`.
- Keep token exchange and credential durability in `anthropic-auth`.
- Keep account selection and account-state transitions in `anthropic-pool`.
- Keep process assembly and listener startup in `main.rs`; do not put reusable domain logic there.
- Search for an existing abstraction before adding helpers or plumbing state across layers.
- Keep crate API surfaces small. Default modules, types, and helpers to private unless another crate genuinely requires them.

The most load-bearing surfaces are `proxy.rs::proxy_request`, `provider_impl.rs`, the pool and refresh state machines, credential persistence, and the `k8s/`/CI deployment handoff. Changes there require focused review beyond compilation success.

## Specification and documentation authority

- Read `specs/README.md` before treating an individual spec as current. Several specs are historical or superseded.
- Historical specs are design evidence, not current requirements.
- Use `docs/runbook/README.md` as the entry point for current operator procedures.
- Treat audits and commit messages as timestamped claims. Open the referenced implementation and tests before acting on them.
- When intent matters, inspect the introducing or correcting Git diff.
- Update the governing current spec or runbook when behavior or operations change.

A document that points to source provides a route to evidence, not verification. Confidence should rise only with the strongest surface actually checked: source and deterministic tests for local behavior; header capture for Claude Code wire parity; Kubernetes, tailnet, and Phoenix checks for live behavior.

## Development workflow

Use the tasks in `mise.toml` as the canonical tool and command interface. Mise provisions Rust, Clippy, rustfmt, cargo-audit, cargo-zigbuild, kubeconform, and header-capture tooling.

```bash
mise run fmt             # Format source
mise run fmt:check       # Check formatting without mutation
mise run lint            # Clippy, warnings denied
mise run build           # Debug workspace build
mise run test            # Workspace tests
mise run check           # fmt check -> lint -> build -> test
mise run ci              # check + audit + release size + Kubernetes validation
```

Focused Rust commands are appropriate during development:

```bash
cargo test -p provider
cargo test -p anthropic-auth
cargo test -p anthropic-pool
cargo test -p oauth-proxy
cargo test -p oauth-proxy test_name
```

Validation escalation:

- For ordinary source changes, run focused tests while iterating and `mise run check` before finalizing.
- For dependency, release, Docker, or Kubernetes changes, run `mise run ci`.
- For cross-compilation work, use `mise run build:cross-x86` and/or `mise run build:cross-arm64`.
- `mise run headers:capture` is a vendor-wire investigation requiring mitmproxy and a current Claude Code client.
- `mise run live:validate` is read-only but requires cluster, tailnet, Pi, and Phoenix access. Run it only when the task requires live evidence and those surfaces are available.
- Do not claim live deployment health, telemetry ingestion, or current Claude Code parity from deterministic source checks alone.

Rust builds can wait on Cargo locks. Treat slow progress as normal; do not kill compiler or test processes solely because they are quiet.

## Rust and API conventions

- Follow rustfmt and Clippy with warnings denied.
- Prefer exhaustive `match` statements for account, lifecycle, and error state machines. Avoid wildcard arms that hide newly added variants.
- Prefer self-documenting API shapes over positional boolean or ambiguous `Option` parameters. Use enums, newtypes, builders, or named methods where they improve call sites.
- New traits must document their role, invariants, and implementation contract.
- Preserve object safety where dynamic providers require `Arc<dyn Provider>`; do not mechanically replace boxed futures with an incompatible trait shape.
- Prefer private modules and explicit exports.
- Avoid one-off abstractions that merely move code. Extract cohesive responsibilities with their tests and documentation.
- Instrument async work at the owning function or method when practical. Check for existing instrumentation before adding another span.
- Avoid unrelated style churn while making behavioral changes.

## Testing rules

- Behavior changes require tests at the layer that owns the behavior.
- Proxy retry, failover, streaming, header, and request-mutation changes require service-level integration coverage.
- Pool selection, cooldown, disabling, and refresh classification belong in `anthropic-pool` tests.
- PKCE, token classification, and credential persistence belong in `anthropic-auth` tests.
- Use local mock HTTP servers. Do not add tests that contact Anthropic, a live cluster, tailnet services, or operator infrastructure.
- Prefer asserting complete responses or state objects over many independent field assertions when the complete value is stable and meaningful.
- Do not add tests that only restate static constants. Test the behavior those constants produce.
- Avoid mutating process-wide environment in tests; inject configuration or dependencies where practical.
- When introducing a substantial new test module, prefer a sibling `*_tests.rs` file instead of further growing an inline implementation test module. Do not move existing tests solely for style conformity.
- Unit and integration tests can establish construction and behavior, but they cannot establish current vendor-wire parity.

## Compatibility surfaces

Before changing public behavior, search callers, tests, current specs, and runbooks for these contracts:

- HTTP request/response semantics, hop-by-hop header handling, body limits, retries, and SSE streaming.
- Configuration TOML and persisted credential formats.
- Admin API, `/health`, and Prometheus metric names and labels.
- OpenTelemetry span names, status, and attributes.
- Kubernetes resource names, ports, labels, PVC paths, ingress routing, and immutable image tags.
- Claude Code identity headers, beta flags, billing attribution, and system-prompt mutation.

Claude Code compatibility is empirical and perishable. Before changing `REQUIRED_BETA_FLAGS`, `USER_AGENT`, `X_APP`, `ANTHROPIC_BILLING_HEADER`, or system-prompt rewriting, read the provenance audits, inspect `provider_impl.rs` and its tests, and use `mise run headers:capture` when claiming parity with a current release.

Preserve streaming semantics: the initial upstream response has a timeout, while an established response stream uses an idle timeout. Do not replace this with a wall-clock timeout over the complete SSE response.

Preserve observability on every return path. Request counters, error counters, in-flight accounting, duration metrics, and span finalization must remain consistent across success, retry, failover, timeout, and early-error branches.

## Change-size and module guidance

Prefer focused, reviewable changes. For non-mechanical work, treat roughly 500 changed lines as a review trigger and 800 changed lines as a strong signal to split the work into coherent stages. Base staging on actual dependencies and affected call sites, not arbitrary file boundaries.

Several modules are already large. Do not add substantial new behavior to `main.rs`, `proxy.rs`, `admin.rs`, `config.rs`, `provider_impl.rs`, or `pool.rs` without first considering whether a cohesive module should own it. Do not split code merely to satisfy a line count; keep invariants, tests, and module documentation close to their implementation.

## Final review checklist

Before finalizing, verify:

- The change respects crate ownership and reuses existing abstractions.
- Credentials and request content cannot leak through logs, errors, metrics, traces, fixtures, or snapshots.
- Retry and failover counts have not combined into unintended multiplicative attempts.
- Streaming behavior and idle-timeout semantics remain correct.
- Public configuration, admin, health, metrics, telemetry, and Kubernetes contracts were considered.
- Tests are deterministic, local, and cover the owning layer.
- Current documentation was updated without treating historical material as authoritative.
- The narrowest sufficient validation was run, and any unverified live behavior is reported explicitly.
