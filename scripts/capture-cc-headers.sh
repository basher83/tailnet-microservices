#!/usr/bin/env bash
#
# capture-cc-headers.sh — Capture and audit Claude Code's Anthropic request headers.
#
# Re-runnable verification for the header-provenance audit
# (docs/audits/header-provenance.md). After a Claude Code upgrade, run this to
# refresh ground truth for the constants the OAuth proxy injects.
#
# It captures THREE things and diffs them against the proxy's hardcoded constants:
#   1. DEBUG attribution line   — what CC's `--debug-file` logs (x-anthropic-billing-header)
#   2. ON-WIRE headers          — what CC actually sends on POST /v1/messages (via mitmproxy)
#   3. Proxy constants          — what services/oauth-proxy/src/provider_impl.rs injects
#
# Key historical finding (2026-05-31): the billing header appears in the DEBUG
# line but NOT on the wire — genuine CC builds the attribution string yet does not
# attach it to /v1/messages. This script makes that easy to re-verify.
#
# Usage:
#   scripts/capture-cc-headers.sh [options]
#
# Options:
#   --debug-only      Only capture the --debug-file attribution line (no mitmproxy).
#   --port N          Proxy port for mitmproxy (default: auto-pick a free port).
#   --model M         Model to exercise (default: claude-haiku-4-5).
#   --keep            Keep the temp capture files instead of cleaning them up.
#   -h, --help        Show this help.
#
# Requirements:
#   - claude (Claude Code CLI) on PATH
#   - mitmproxy, provisioned via mise (mise.toml: "pipx:mitmproxy"). Not needed
#     with --debug-only.
#
# Exit codes: 0 ok, 1 environment/setup error, 2 capture produced no data.

set -uo pipefail

# ── Locate repo root (this script lives in <root>/scripts/) ───────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
PROVIDER_RS="$REPO_ROOT/services/oauth-proxy/src/provider_impl.rs"

# ── Defaults ──────────────────────────────────────────────────────────────────
DEBUG_ONLY=0
PORT=""
MODEL="claude-haiku-4-5"
KEEP=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --debug-only) DEBUG_ONLY=1; shift ;;
    --port) PORT="${2:?--port needs a value}"; shift 2 ;;
    --model) MODEL="${2:?--model needs a value}"; shift 2 ;;
    --keep) KEEP=1; shift ;;
    -h|--help) sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $1 (try --help)" >&2; exit 1 ;;
  esac
done

c_bold=$'\033[1m'; c_red=$'\033[31m'; c_grn=$'\033[32m'; c_yel=$'\033[33m'; c_dim=$'\033[2m'; c_off=$'\033[0m'
say()  { printf '%s\n' "$*"; }
hdr()  { printf '\n%s== %s ==%s\n' "$c_bold" "$*" "$c_off"; }
ok()   { printf '%s%s%s\n' "$c_grn" "$*" "$c_off"; }
warn() { printf '%s%s%s\n' "$c_yel" "$*" "$c_off"; }
err()  { printf '%s%s%s\n' "$c_red" "$*" "$c_off" >&2; }

command -v claude >/dev/null 2>&1 || { err "claude CLI not found on PATH"; exit 1; }

CC_VERSION="$(claude --version 2>/dev/null | awk '{print $1}')"
hdr "Claude Code version"
say "claude --version → ${CC_VERSION:-unknown}"

WORK="$(mktemp -d -t cc-headers.XXXXXX)"
cleanup() {
  [[ -n "${MITM_PID:-}" ]] && kill "$MITM_PID" 2>/dev/null
  pkill -f "mitmdump --listen-port ${PORT:-_none_}" 2>/dev/null
  if [[ "$KEEP" == 1 ]]; then say "${c_dim}(kept temp dir: $WORK)${c_off}"; else rm -rf "$WORK"; fi
}
trap cleanup EXIT INT TERM

# ── 1. DEBUG attribution line (always) ────────────────────────────────────────
hdr "1. DEBUG attribution line (claude --debug-file)"
DBG="$WORK/cc.debug"
claude -p --output-format json --no-session-persistence --model "$MODEL" \
  --debug-file "$DBG" 'Reply exactly: ok' > "$WORK/cc.out" 2> "$WORK/cc.err"
DEBUG_LINE="$(grep -i 'attribution header x-anthropic-billing-header' "$DBG" 2>/dev/null | tail -1)"
if [[ -n "$DEBUG_LINE" ]]; then
  ok "$(printf '%s' "$DEBUG_LINE" | sed 's/.*attribution header /  /')"
else
  warn "  no attribution-header debug line found (CC may have changed its debug output)"
fi

# ── 2. ON-WIRE headers via mitmproxy (unless --debug-only) ────────────────────
WIRE_BILLING="(not captured)"
if [[ "$DEBUG_ONLY" == 0 ]]; then
  hdr "2. On-wire headers (mitmproxy POST /v1/messages)"

  MITM_BIN="$(command -v mitmdump 2>/dev/null || true)"
  [[ -z "$MITM_BIN" ]] && MITM_BIN="$(mise which mitmdump 2>/dev/null || true)"
  if [[ -z "$MITM_BIN" ]]; then
    err "  mitmdump not found. Provision it: mise install (mise.toml has pipx:mitmproxy),"
    err "  or re-run with --debug-only."
    exit 1
  fi

  # Auto-pick a free port if not given.
  if [[ -z "$PORT" ]]; then
    for p in 8890 8891 8892 8893 8894 8895 8896; do
      if ! lsof -nP -iTCP:"$p" -sTCP:LISTEN >/dev/null 2>&1; then PORT="$p"; break; fi
    done
  fi
  [[ -z "$PORT" ]] && { err "  no free port found in 8890-8896"; exit 1; }

  CAP="$WORK/wire.jsonl"
  ADDON="$WORK/addon.py"
  cat > "$ADDON" <<PYEOF
import json
OUT = "$CAP"
def request(flow):
    h = flow.request.pretty_host
    if "anthropic.com" in h and "/v1/messages" in flow.request.path:
        with open(OUT, "a") as f:
            f.write(json.dumps({
                "path": flow.request.path,
                "headers": {k: v for k, v in flow.request.headers.items()},
                "names": sorted(k.lower() for k in flow.request.headers.keys()),
            }) + "\n")
PYEOF

  "$MITM_BIN" --listen-port "$PORT" -s "$ADDON" --set flow_detail=0 > "$WORK/mitm.log" 2>&1 &
  MITM_PID=$!

  CA="$HOME/.mitmproxy/mitmproxy-ca-cert.pem"
  for _ in $(seq 1 30); do
    [[ -f "$CA" ]] && lsof -nP -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1 && break
    sleep 1
  done
  if [[ ! -f "$CA" ]] || ! lsof -nP -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1; then
    err "  mitmdump failed to start on port $PORT"; tail -8 "$WORK/mitm.log" >&2; exit 1
  fi
  say "${c_dim}  mitmdump up on 127.0.0.1:$PORT (CA: $CA)${c_off}"

  HTTPS_PROXY="http://127.0.0.1:$PORT" HTTP_PROXY="http://127.0.0.1:$PORT" \
    NODE_EXTRA_CA_CERTS="$CA" SSL_CERT_FILE="$CA" REQUESTS_CA_BUNDLE="$CA" \
    CLAUDE_CODE_ENTRYPOINT=sdk-cli \
    timeout 90 claude -p --output-format json --no-session-persistence \
      --model "$MODEL" 'Reply exactly: ok' > "$WORK/cc-wire.out" 2> "$WORK/cc-wire.err"

  if [[ ! -s "$CAP" ]]; then
    err "  no /v1/messages flow captured. claude output:"
    head -c 300 "$WORK/cc-wire.out" >&2; echo >&2
    exit 2
  fi

  # Python prints the attribution-relevant headers to stdout and writes the
  # presence flag (True/False) to $FLAG, so the marker never pollutes output.
  FLAG="$WORK/billing_present"
  python3 - "$CAP" "$FLAG" <<'PY'
import json, sys
rows=[json.loads(l) for l in open(sys.argv[1]) if l.strip()]
r=rows[-1]
present="x-anthropic-billing-header" in r["names"]
want=("user-agent","x-app","anthropic-version","anthropic-beta",
      "anthropic-dangerous-direct-browser-access","x-anthropic-billing-header")
for k,v in r["headers"].items():
    if k.lower() in want:
        print("  %-42s %s" % (k+":", v[:100]))
open(sys.argv[2], "w").write("True" if present else "False")
PY
  if [[ "$(cat "$FLAG" 2>/dev/null)" == "True" ]]; then
    ok "  → x-anthropic-billing-header IS present on the wire"
  else
    warn "  → x-anthropic-billing-header is NOT on the wire (debug-only header)"
  fi
fi

# ── 3. Diff against proxy constants ───────────────────────────────────────────
hdr "3. Proxy constants (provider_impl.rs)"
if [[ -f "$PROVIDER_RS" ]]; then
  PXY_UA="$(grep -E 'const USER_AGENT' "$PROVIDER_RS" | sed -E 's/.*"(.*)".*/\1/')"
  PXY_BILL="$(grep -E 'const ANTHROPIC_BILLING_HEADER' "$PROVIDER_RS" | sed -E 's/.*"(.*)".*/\1/')"
  PXY_VER="$(grep -E 'const ANTHROPIC_VERSION' "$PROVIDER_RS" | sed -E 's/.*"(.*)".*/\1/')"
  say "  USER_AGENT              = $PXY_UA"
  say "  ANTHROPIC_VERSION       = $PXY_VER"
  say "  ANTHROPIC_BILLING_HEADER= $PXY_BILL"

  hdr "Summary"
  # Compare debug cc_version against proxy billing header version.
  DBG_CCVER="$(printf '%s' "$DEBUG_LINE" | grep -oE 'cc_version=[0-9.]+[a-f0-9.]*' | head -1)"
  PXY_CCVER="$(printf '%s' "$PXY_BILL" | grep -oE 'cc_version=[0-9.]+[a-f0-9.]*' | head -1)"
  if [[ -n "$DBG_CCVER" && "$DBG_CCVER" != "$PXY_CCVER" ]]; then
    warn "  cc_version drift: proxy has '$PXY_CCVER', live CC reports '$DBG_CCVER'"
    warn "  → update ANTHROPIC_BILLING_HEADER in provider_impl.rs and ~/.pi/agent/models.json if you want fidelity"
  else
    ok "  cc_version matches (or not determinable): proxy='$PXY_CCVER' live='$DBG_CCVER'"
  fi
else
  warn "  provider_impl.rs not found at $PROVIDER_RS (run from inside the repo)"
fi

hdr "Done"
say "See docs/audits/header-provenance.md for the full provenance analysis."
