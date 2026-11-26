# OpenWiki brief: Tailnet Microservices

Write an engineer-facing wiki for maintaining `anthropic-oauth-proxy`, the
Rust service in `services/oauth-proxy/`. Help readers build and test it, trace
a request, find the right code to change, and diagnose a deployment.

- **Quickstart and source map.** Start with `cargo build --workspace` and
  `cargo test --workspace`. Explain the boundaries between the service and
  `crates/common`, `crates/provider`, `crates/anthropic-auth`, and
  `crates/anthropic-pool`. Point readers to representative tests and
  distinguish automated checks from smoke tests that need a live deployment.
- **Architecture and behavior.** Trace the request path from Tailscale Operator
  Ingress through the Kubernetes Service to the proxy and Anthropic. Keep the
  proxy and admin surfaces distinct. Explain passthrough versus OAuth pool
  mode, then cover the behaviors maintainers are most likely to debug:
  PKCE enrollment, credential persistence and refresh, account selection,
  quota failover, and streaming timeouts.
- **Operations.** Use `k8s/` to verify what is deployed and
  `docs/runbook/README.md` for operator procedures. Prioritize health and pool
  status, metrics, logs and traces, account re-authentication, and rollback.
  Link to detailed procedures rather than duplicating them. Use placeholders
  for credentials and deployment-specific hostnames.
- **Source of truth.** Verify shipped behavior against source, tests, and
  manifests. Use `specs/README.md` to distinguish current design from drafts
  and historical specs, especially around Tailscale exposure and the
  passthrough-to-OAuth evolution.

