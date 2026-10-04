# OAuth grant expiry and recovery — 2026-10-04

This dated record summarizes preserved pod-log timestamps, admin-pool observations, and headless Pi verification from the October 4 investigation. It contains no credentials or request/response content.

## Expiry evidence

Both accounts were provisioned on 2026-08-26. The captured rejections returned `invalid_grant` and permanently disabled the accounts.

| Account / lineage | Last good refresh (UTC) | First rejection (UTC) | Observed lifetime | `error_description` |
|---|---|---|---|---|
| `claude-max-local` / keychain shared with local Claude Code | Not established in the recorded evidence | 2026-09-23 07:26:55Z (inline refresh) | ~28 days | `Refresh token expired` |
| `claude-max-1787733199` / proxy-owned PKCE | 2026-09-24 11:59:37Z | 2026-09-24 19:44:37Z (background refresh) | ~29.5 days | `Refresh token expired` |

The similar ages and error descriptions suggest a ~30-day server-side grant lifetime. This is an inference, not a confirmed vendor TTL, and does not establish that PKCE grants last longer than keychain grants.

Complete pool unavailability ran from the final account's rejection at **2026-09-24 19:44:37Z** to the healthy admin-pool observation at **2026-10-04 05:38:31Z**: about 9 days 9 hours 54 minutes (~9.4 days). The initial October 4 client check reproduced `503 pool_exhausted`; `/health` returned HTTP 200 with body status `unhealthy`. This window measures pool unavailability, not the number of failed client requests.

## Recovery and verification

The operator completed browser consent for a new PKCE account, `claude-max-1791092167`. After confirming it was available, both disabled accounts were removed through the admin API. At 05:38:31Z, the pool was healthy with one available account and zero cooling or disabled accounts. No pod restart, deployment change, or credential-file copying was required.

At 05:45:08–05:45:12Z, headless Pi 0.87.1 passed through the tailnet route using provider `anthropic-proxy`:

| Model | Client exit | Proxy completion | Pool before and after |
|---|---:|---|---|
| `claude-haiku-4-5` | 0 | HTTP 200 | healthy; one available account |
| `claude-fable-5-1` | 0 | HTTP 200 | healthy; one available account |

A subsequent rerun with the operator's real Pi configuration, including extensions and tools, also passed both models with exit 0 and the expected output. The calls exercised normal streaming; no prolonged stream idle-timeout test or vendor-header parity capture was performed. These are dated checks, not a continuous health guarantee.

The pool now has one account and no account failover. Plan browser re-auth before ~2026-11-03, using the inferred lifetime only as an operational estimate. See [account management](../runbook/accounts.md#refresh-token-lifetime-and-re-auth) for the procedure.
