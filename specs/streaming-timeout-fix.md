# Spec: Streaming Timeout Fix

**Status:** Active
**Created:** 2026-02-13

---

## Why

Claude Code sessions through the proxy work for small operations but hang and timeout on large operations (writing files, multi-tool sequences). Reverting to Anthropic's default OAuth flow eliminates the issue, confirming the proxy as the cause.

The Anthropic API streams responses via SSE. Large operations produce streaming responses that last several minutes. The proxy kills these mid-stream after 60 seconds.

---

## Root Cause

`proxy.rs:223` applies `.timeout(state.timeout)` on the per-request reqwest builder. The config sets `timeout_secs = 60` (`k8s/config.toml:3`). reqwest's `.timeout()` is a wall-clock limit on the entire request lifecycle — connection, sending, and receiving all included. Once the 60-second mark is reached, reqwest aborts the underlying connection regardless of whether data is still flowing.

For non-streaming or fast responses, this is invisible. For SSE streams that run longer than 60 seconds, the connection is killed mid-stream. The client sees a hang followed by a timeout error. Claude Code's retry logic cannot recover because the conversation state is already mid-operation.

---

## Three-Phase Timeout Model

A proxied request has three phases, each needing different timeout behavior:

| Phase | What happens | Current protection | Correct protection |
|-------|-------------|-------------------|-------------------|
| **1. Connection** | TCP handshake to upstream | `connect_timeout(5s)` on Client | Unchanged — `connect_timeout(5s)` |
| **2. Initial response** | Request sent, waiting for upstream to return status + headers | `.timeout(state.timeout)` (wall-clock) | `tokio::time::timeout` around `req.send().await` |
| **3. Body streaming** | SSE chunks flowing from upstream through proxy to client | `.timeout(state.timeout)` (wall-clock — **this is the bug**) | `IdleTimeoutStream` wrapper on `bytes_stream()` |

The current `.timeout()` covers phases 2 and 3 with a single wall-clock limit. This is wrong for phase 3 — a healthy stream sending bytes every few seconds gets killed at the wall-clock mark. The fix separates the phases so each gets the right timeout behavior.

---

## Requirements

**R1. Remove per-request wall-clock timeout**

Remove `.timeout(state.timeout)` from the reqwest request builder at `proxy.rs:223`. This is the single change that fixes the immediate bug — SSE streams will no longer be killed at 60 seconds.

**R2. Protect the initial response phase**

Wrap `req.send().await` in `tokio::time::timeout(state.timeout, ...)` inside the retry loop. This catches completely dead upstreams (accept connection but never send response headers) with the same timeout value and retry behavior as today. The timeout error from `tokio::time::timeout` must be mapped to the same code path that currently handles `e.is_timeout()` — the retry logic at `proxy.rs:349-375` must trigger identically.

**R3. Protect the streaming body phase with idle timeout**

Wrap the `upstream_response.bytes_stream()` in `build_streaming_response()` (`proxy.rs:458-459`) with an `IdleTimeoutStream` that resets a deadline on each chunk received. If no chunk arrives within the idle window, the stream terminates with an error. The idle timeout value is `state.timeout` (60s). The Anthropic API sends SSE heartbeat comments (`: ping`) during long streams, well within this window.

**R4. No behavioral change for non-streaming responses**

Non-streaming responses that complete within seconds must continue to work identically. Error classification, failover, and retry logic are unaffected. The `IdleTimeoutStream` wrapper is transparent for responses that complete quickly — the stream ends normally before any idle deadline fires.

**R5. Preserve retry semantics**

The retry loop (`proxy.rs:213-396`) continues to retry exactly 3 times on timeout with 100ms backoff. The only change is the error source: `tokio::time::timeout` elapsed error instead of reqwest `is_timeout()`. Map the elapsed error to the same retry/504 path.

---

## Changes

### `services/oauth-proxy/src/proxy.rs`

**Remove per-request timeout (line 223):**

```rust
let req = state
    .client
    .request(method.clone(), &upstream_url)
    .headers(headers.clone())
    .body(final_body.clone());
```

**Wrap send() with tokio timeout (inside the retry loop, around the send call):**

```rust
let send_result = tokio::time::timeout(state.timeout, req.send()).await;
match send_result {
    Ok(Ok(upstream_response)) => { /* existing success path */ }
    Ok(Err(e)) => { /* existing non-timeout reqwest error path (DNS failure, connection refused, etc.) */ }
    Err(_elapsed) => {
        // Timeout waiting for initial response — replaces the e.is_timeout() arms
        if attempt < MAX_UPSTREAM_ATTEMPTS - 1 {
            // Non-final attempt: log warning and continue to next retry iteration
            // (the retry loop's `if attempt > 0 { warn!(...); sleep(UPSTREAM_RETRY_DELAY); }` at the
            // top of the loop handles the delay and logging for the next iteration)
            continue;
        }
        // Final attempt exhausted — full 504 construction (replicate ALL of the existing
        // e.is_timeout() final-attempt path, including):
        //   - error!() log with request_id, method, upstream_url, timeout duration
        //   - state.errors_total.fetch_add(1, Ordering::Relaxed)
        //   - crate::metrics::record_upstream_error("timeout")  (if metrics exist)
        //   - construct JSON error response body with request_id and
        //     "upstream response timeout after {timeout}s ({MAX_UPSTREAM_ATTEMPTS} attempts)"
        //   - return Response with StatusCode::GATEWAY_TIMEOUT (504)
        // Do NOT use a skeleton — copy the complete final-attempt code from the current
        // Err(e) if e.is_timeout() arm and adapt it (the Elapsed error has different Display
        // formatting than reqwest timeout, so use the message string directly rather than
        // formatting the error).
    }
}
```

This replaces the current `match req.send().await { Ok(...), Err(e) if e.is_timeout() => ..., Err(e) => ... }` pattern. The `Err(_elapsed)` arm handles what the TWO `e.is_timeout()` arms used to handle (non-final and final attempt). The `Ok(Err(e))` arm handles non-timeout reqwest errors. The `proxy_does_not_retry_non_timeout_errors` test should pass unchanged — connection refused will hit `Ok(Err(e))` where `e.is_timeout()` is false, which routes to the existing non-timeout error path.

**Add IdleTimeoutStream wrapper in build_streaming_response (lines 458-459):**

```rust
let idle_stream = IdleTimeoutStream::new(
    upstream_response.bytes_stream(),
    timeout,
);
response
    .body(axum::body::Body::from_stream(idle_stream))
```

Pass `state.timeout` into `build_streaming_response()` as a new parameter.

**Dependency changes** — add these as direct dependencies (they are transitive deps but Rust requires explicit declaration):

```toml
# Add to services/oauth-proxy/Cargo.toml [dependencies]
futures-util = "0.3"
bytes = "1"
pin-project-lite = "0.2"
```

**Add IdleTimeoutStream implementation** (new code in proxy.rs or a submodule):

```rust
use std::pin::Pin;
use std::task::{Context, Poll};
use futures_util::Stream;
use tokio::time::{sleep, Sleep, Duration};
use bytes::Bytes;

pin_project_lite::pin_project! {
    pub struct IdleTimeoutStream<S> {
        #[pin]
        inner: S,
        #[pin]
        deadline: Sleep,
        timeout: Duration,
        timed_out: bool,
    }
}

impl<S> IdleTimeoutStream<S> {
    pub fn new(inner: S, timeout: Duration) -> Self {
        Self {
            inner,
            deadline: sleep(timeout),
            timeout,
            timed_out: false,
        }
    }
}

impl<S, E> Stream for IdleTimeoutStream<S>
where
    S: Stream<Item = Result<Bytes, E>>,
    E: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    type Item = Result<Bytes, Box<dyn std::error::Error + Send + Sync>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();

        // Once timed out, the stream is done — return None to terminate cleanly
        if *this.timed_out {
            return Poll::Ready(None);
        }

        // Check if the idle deadline has elapsed
        if this.deadline.poll(cx).is_ready() {
            *this.timed_out = true;
            tracing::warn!("upstream idle timeout after {}s", this.timeout.as_secs());
            return Poll::Ready(None); // Terminate stream cleanly — no error frame
        }

        // Poll the inner stream
        match this.inner.poll_next(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                // Data received — reset the idle deadline
                this.deadline.reset(tokio::time::Instant::now() + *this.timeout);
                Poll::Ready(Some(Ok(bytes)))
            }
            Poll::Ready(Some(Err(e))) => {
                Poll::Ready(Some(Err(e.into())))
            }
            Poll::Ready(None) => Poll::Ready(None), // Stream ended normally
            Poll::Pending => Poll::Pending,
        }
    }
}
```

Key differences from a naive implementation:
- `timed_out: bool` guard prevents infinite error loop — `Sleep` stays `Ready` after firing, so without this guard every subsequent `poll_next` would immediately fire the deadline again
- Item type is `Result<Bytes, _>` (bare `Bytes`, NOT `Frame<Bytes>`) — axum 0.8's `Body::from_stream()` requires `TryStream<Ok: Into<Bytes>>`, and axum's internal `StreamBody` handles the `Frame` wrapping automatically. The existing code passes reqwest's `bytes_stream()` (which yields bare `Bytes`) directly to `from_stream`, and this wrapper must match that contract.
- Timeout terminates the stream cleanly with `Poll::Ready(None)` instead of emitting an error frame — the client sees a closed stream, not a garbled SSE frame
- Uses `tokio::time::Instant` (not `std::time::Instant`) for the deadline reset — note that `proxy.rs` already imports `std::time::Instant` at line 8 for request timing, so qualify `tokio::time::Instant` explicitly to avoid name collision
- Inner stream errors use `e.into()` instead of `format!` wrapping, preserving the original error type chain
- No `http_body::Frame` import needed — axum handles the `Bytes` → `Frame<Bytes>` conversion internally

**Update build_streaming_response signature:**

```rust
fn build_streaming_response(
    status: StatusCode,
    resp_headers: &reqwest::header::HeaderMap,
    upstream_response: reqwest::Response,
    request_id: &str,
    idle_timeout: Duration,       // NEW parameter
) -> Response {
```

Update both call sites (`proxy.rs:318` passthrough errors and `proxy.rs:342` success path) to pass `state.timeout`.

**Update error message (proxy.rs:370-373):**

Change from:

```rust
"upstream timeout after {}s ({MAX_UPSTREAM_ATTEMPTS} attempts)"
```

To:

```rust
"upstream response timeout after {}s ({MAX_UPSTREAM_ATTEMPTS} attempts)"
```

This distinguishes the "waiting for initial response" timeout from the stream idle timeout. The `state.timeout.as_secs()` value is still correct here — it's the same Duration used for both `tokio::time::timeout` and `IdleTimeoutStream`.

### `services/oauth-proxy/src/main.rs`

No changes to the Client builder. The `connect_timeout(5s)` remains. No `read_timeout` needed — the `tokio::time::timeout` around `send()` and the `IdleTimeoutStream` cover everything.

### `k8s/config.toml`

No change. `timeout_secs = 60` is now the idle timeout value used for both initial response and stream idle detection. 60 seconds of zero activity is a reasonable dead-connection signal.

### Tests

**The following three timeout tests are affected** (verify each passes after the change):

1. `proxy_timeout_returns_504_gateway_timeout` (main.rs, searches for `fn proxy_timeout_returns_504`) — asserts 504 status and error message JSON
2. `proxy_retries_timeout_exactly_three_attempts` (main.rs, searches for `fn proxy_retries_timeout`) — asserts `attempt_count` equals 3
3. `proxy_resends_body_on_timeout_retry` (main.rs, searches for `fn proxy_resends_body`) — asserts request body is present on each retry attempt

Additionally, `proxy_does_not_retry_non_timeout_errors` must continue to pass unchanged — connection refused errors now hit `Ok(Err(e))` instead of `Err(e)`, but the non-retry behavior should be identical.

The tests currently rely on reqwest's `e.is_timeout()` check, which fires from the per-request `.timeout()`. After replacing with `tokio::time::timeout`, the error path changes.

The current error flow in tests is:

```text
mock server accepts but never responds
→ reqwest .timeout() fires
→ Err(e) where e.is_timeout() == true
→ retry loop continues/returns 504
```

The new error flow is:

```text
mock server accepts but never responds
→ tokio::time::timeout fires (Elapsed error)
→ Err(_elapsed) arm in match
→ retry loop continues/returns 504
```

The tests don't directly check the error variant — they check the HTTP response status (504) and the error message JSON. The retry count test checks `attempt_count.load()`. As long as the `Err(_elapsed)` arm triggers the same retry and 504 logic, the tests pass without changes to their assertions.

**However**, the test `ProxyState` construction at `main.rs:423` uses `reqwest::Client::new()` and `timeout: Duration::from_secs(5)`. The `state.timeout` value is used by the `tokio::time::timeout` call. Individual timeout tests override with `Duration::from_millis(50)`. Verify that:

1. `test_app_state()` at line 409 still sets `timeout: Duration::from_secs(5)` (used for non-timeout tests)
2. The three timeout tests override with `timeout: Duration::from_millis(50)` via their own `ProxyState` construction
3. The `reqwest::Client::new()` in tests no longer matters for timeout behavior — timeouts are now in the proxy code, not the reqwest client

**The test client (`reqwest::Client::new()`) is fine.** Previously, the per-request `.timeout()` was applied by the proxy's request builder using the reqwest Client. That's removed. Now the proxy uses `tokio::time::timeout()` with `state.timeout`, which is independent of the reqwest Client configuration. Tests set `state.timeout = Duration::from_millis(50)`, so the `tokio::time::timeout` uses 50ms. No changes needed to test Client construction.

**New test: stream idle timeout**

Add one test that verifies the `IdleTimeoutStream` terminates when the upstream stops sending mid-stream:

```text
1. Start mock server that sends HTTP 200 + 2 SSE chunks, then stops (no more data, connection held open)
2. Set state.timeout to 100ms
3. Send request through proxy
4. Verify: first 2 chunks arrive at the client
5. Verify: after ~100ms of silence, the stream terminates (client sees connection close or error)
```

This test exercises the `IdleTimeoutStream` directly, which no existing test covers.

---

## Success Criteria

- [ ] Claude Code large operations (file writes, multi-tool) complete through the proxy without timeout
- [ ] Dead upstream connections (zero bytes, never responds) still detected and return 504 within `timeout_secs`
- [ ] Dead upstream connections mid-stream (stops sending after some chunks) detected within `timeout_secs` — client sees a cleanly closed stream (not a 504 or error frame)
- [ ] Existing timeout retry behavior preserved (3 attempts on initial response timeout)
- [ ] New stream idle timeout test passes
- [ ] All existing tests pass (no changes to test assertions needed, only error path restructure in proxy code)
- [ ] `cargo clippy --workspace -- -D warnings` clean
- [ ] `state.timeout` field still used (by `tokio::time::timeout` and `IdleTimeoutStream`), no dead-field warning

---

## Non-Goals

- Changing `transfer-encoding` hop-by-hop stripping (axum/hyper re-applies chunked encoding for unknown-length bodies — not the primary issue)
- Changing request body buffering or `MAX_BODY_SIZE`
- Adding SSE-specific streaming logic (the `IdleTimeoutStream` is generic over any `Stream<Item = Result<Bytes, E>>`, not SSE-aware)
- reqwest `read_timeout` on the Client builder (the `tokio::time::timeout` around `send()` and `IdleTimeoutStream` cover all phases without relying on reqwest's internal timeout propagation semantics)
- Configurable per-phase timeouts (using `state.timeout` for both initial response and stream idle is correct — 60s of zero activity means dead in both cases)

---

## Implementation Notes

- The `IdleTimeoutStream` uses `pin_project_lite::pin_project!` for safe pin projection. `pin-project-lite` is declared as a direct dependency (see Dependency Changes section above). Do not use `unsafe` manual pin projection.
- The `IdleTimeoutStream::Item` type is `Result<Bytes, Box<dyn Error + Send + Sync>>` — bare `Bytes`, NOT `Frame<Bytes>`. Axum 0.8's `Body::from_stream()` signature is `fn from_stream<S>(stream: S) where S: TryStream, S::Ok: Into<Bytes>`. The internal `StreamBody` adapter wraps each chunk in `Frame::data(chunk.into())` automatically. Do NOT import `http_body::Frame` — it is not needed and `http-body` is not a direct dependency.
- **Instant name collision:** `proxy.rs` already imports `std::time::Instant` (line 8) for request timing. The `IdleTimeoutStream` needs `tokio::time::Instant` for `deadline.reset()`. Use fully qualified `tokio::time::Instant::now()` in the reset call, or add `use tokio::time::Instant as TokioInstant;`.
- **`Ok(Err(e))` arm content:** The `Ok(Err(e))` arm in the new match pattern must replicate the existing `Err(e)` (non-timeout) arm at `proxy.rs:377-394` exactly: increment `errors_total`, record metrics with `BAD_GATEWAY` status, log the error with `error!()`, and return `error_response` with 502 and the error message. Do not leave this as a comment placeholder.
- The `tokio::time::timeout` wrapping `send()` changes the match pattern from 2 arms (`Ok(response)`, `Err(e)`) to 3 arms (`Ok(Ok(response))`, `Ok(Err(e))`, `Err(elapsed)`). The `Err(elapsed)` arm replaces the `Err(e) if e.is_timeout()` arms. Keep the two existing `is_timeout()` arms' behavior (retry on non-final attempt, 504 on final attempt) in the `Err(elapsed)` arm.
- The `state.timeout` field on `ProxyState` is NOT dead after this change. It's passed to `tokio::time::timeout()` in the retry loop and to `build_streaming_response()` for the `IdleTimeoutStream`. The error message at `proxy.rs:370-373` also uses `state.timeout.as_secs()`. No clippy dead-field warning.
- `build_streaming_response` is called in two places: `proxy.rs:318` (passthrough error responses) and `proxy.rs:342` (success responses). Both need the `idle_timeout` parameter. For error passthrough responses, the idle timeout wrapper is still appropriate — the upstream error body might stream.
- **Clean termination ambiguity:** The `IdleTimeoutStream` returns `Poll::Ready(None)` on timeout, which is indistinguishable from a normal stream end at the HTTP level. Clients that need to distinguish "response complete" from "idle timeout" must check for `stop_reason` in the final SSE event. The proxy does not inject any termination signal — it is not SSE-aware by design (see Non-Goals).
- Line numbers reference the pre-implementation state of proxy.rs and main.rs. Use structural landmarks (function names, `MAX_UPSTREAM_ATTEMPTS`, `build_streaming_response`) after modifying the file.
