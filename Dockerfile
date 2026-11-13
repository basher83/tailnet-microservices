# Multi-stage build for the anthropic-oauth-proxy service.
#
# Builds natively on Linux (no cross-compilation) with the release profile
# from Cargo.toml (LTO, single codegen-unit, strip, panic=abort) for
# minimal binary size (~5 MB).
#
# Runtime: standalone container. Tailnet exposure is handled by the
# Tailscale Operator via Service annotations (not a sidecar).

# ---------- builder ----------
FROM rust:1-bookworm@sha256:503651ea31e66ecb74623beabde781059a5978df1595a9e8ed03974d5fec1bf0 AS builder

WORKDIR /src
COPY . .

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p oauth-proxy \
    && cp target/release/anthropic-oauth-proxy /anthropic-oauth-proxy

# ---------- runtime ----------
FROM debian:bookworm-slim@sha256:67b30a61dc87758f0caf819646104f29ecbda97d920aaf5edc834128ac8493d3

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -u 1000 -r -s /sbin/nologin appuser

COPY --from=builder /anthropic-oauth-proxy /usr/local/bin/anthropic-oauth-proxy

USER 1000

EXPOSE 8080 9090

ENV CONFIG_PATH=/etc/anthropic-oauth-proxy/config.toml

ENTRYPOINT ["anthropic-oauth-proxy"]
