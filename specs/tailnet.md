# Spec: Tailnet Integration Layer

**Status:** Draft
**Created:** 2026-02-06
**Author:** Astrogator + Brent

---

## Overview

Abstraction layer that connects a Rust service to a Tailscale tailnet. Provides the service with a tailnet identity (hostname, IP) and the ability to accept incoming connections from other tailnet nodes.

The tailnet integration is a prerequisite for the service state machine's `ConnectingTailnet` -> `Starting` transition and provides the `TailnetHandle` referenced throughout `specs/oauth-proxy.md`.

---

## Integration Strategy — DECIDED: Option B

Two approaches were evaluated. **Option B was chosen** for production maturity and zero Go build dependencies.

### Option A: libtailscale FFI (not chosen)

Embed Tailscale via `libtailscale-sys` / `libtailscale` Rust crates. Not chosen due to experimental maturity and Go build dependency requirement.

| Property | Value |
|----------|-------|
| Build dependency | Go 1.20+ |
| Runtime dependency | None (statically linked) |
| Binary size impact | +5-10MB (Go runtime) |
| Crate | `libtailscale` (messense, v0.2.0) or `tsnet` (badboy, v0.1.0) |
| Maturity | Experimental |

### Option B: tailscaled sidecar (chosen)

Run `tailscaled` as an externally managed separate process. Rust service queries it via `tailscale-localapi` for identity (hostname, IP). On macOS, falls back to `tailscale status --json` CLI for the App Store variant.

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

## Resolved Questions

1. **Which option?** Option B (tailscaled sidecar) was chosen for production maturity and zero Go build dependencies. See `IMPLEMENTATION_PLAN.md` for rationale.
2. **Child process vs external?** Externally managed. The Rust service queries an existing `tailscaled` daemon; it does not spawn or manage the process. This follows the standard sidecar pattern.
3. **Module location?** `services/oauth-proxy/src/tailnet.rs` (service-specific). The integration is tightly coupled to the oauth-proxy's error types and state machine. If a second service is added, the module can be extracted to `crates/common/` at that point.
