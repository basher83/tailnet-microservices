# Spec: Tailnet Integration Layer

**Status:** Draft
**Created:** 2026-02-06
**Author:** Astrogator + Brent

---

## Overview

Abstraction layer that connects a Rust service to a Tailscale tailnet. Provides the service with a tailnet identity (hostname, IP) and the ability to accept incoming connections from other tailnet nodes.

The tailnet integration is a prerequisite for the service state machine's `ConnectingTailnet` -> `Starting` transition and provides the `TailnetHandle` referenced throughout `specs/oauth-proxy.md`.

---

## Integration Strategy

Two viable approaches exist. The chosen approach determines the `TailnetHandle` implementation.

### Option A: libtailscale FFI (true single binary)

Embed Tailscale via `libtailscale-sys` / `libtailscale` Rust crates, which wrap the C API produced by `go build -buildmode=c-archive` from the Go `tsnet` package.

| Property | Value |
|----------|-------|
| Build dependency | Go 1.20+ |
| Runtime dependency | None (statically linked) |
| Binary size impact | +5-10MB (Go runtime) |
| Crate | `libtailscale` (messense, v0.2.0) or `tsnet` (badboy, v0.1.0) |
| Maturity | Experimental |

### Option B: tailscaled sidecar (proven pattern)

Run `tailscaled` as a separate process. Rust service listens on localhost; `tailscaled` forwards tailnet traffic. Use `tailscale-localapi` crate for identity queries.

| Property | Value |
|----------|-------|
| Build dependency | None |
| Runtime dependency | `tailscaled` binary |
| Binary size impact | None |
| Crate | `tailscale-localapi` (jtdowney, v0.4.2) |
| Maturity | Production-grade |

---

## Types

### `TailnetHandle`

Opaque handle representing an active tailnet connection.

```rust
pub struct TailnetHandle {
    /// Tailnet hostname assigned to this node
    pub hostname: String,
    /// Tailnet IPv4 address
    pub ip: std::net::IpAddr,
}
```

### `TailnetConfig`

Already defined in `services/oauth-proxy/src/config.rs` as `TailscaleConfig`.

---

## Lifecycle

1. Service reads `TailscaleConfig` from config file
2. Auth key loaded from `TS_AUTHKEY` env var or `auth_key_file`
3. Connect to tailnet (FFI call or verify tailscaled is running)
4. Obtain `TailnetHandle` with hostname and IP
5. On shutdown, disconnect cleanly

---

## Error Cases

| Error | Description | Retryable |
|-------|-------------|-----------|
| `TailnetAuthError` | Invalid or expired auth key | No |
| `TailnetConnectError` | Cannot reach coordination server | Yes (5 retries, exponential backoff) |
| `TailnetNotRunning` | tailscaled not available (Option B only) | No |

---

## Open Questions

1. Which option (A or B) to implement first? Option B is lower risk for initial deployment.
2. For Option B, should the Rust binary manage `tailscaled` as a child process, or expect it to be externally managed?
3. Should the tailnet module live in `crates/common/` (reusable across services) or `services/oauth-proxy/src/tailnet.rs` (service-specific)?
