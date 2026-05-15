#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Post-deploy end-to-end smoke test. Runs from any machine that can
# reach the VPS over HTTPS — no ssh needed; everything is exercised
# via the public HTTPS surface.
#
# What it checks:
#
#   1. nginx layer is up on both vhosts:
#      * https://lakko.me/health        → "lakko.me ok"
#      * https://api.lakko.me/health    → "api.lakko.me ok"
#
#   2. Each backend service replies on its `api.lakko.me` route:
#      * /auth/health        → 200 "ok"
#      * /ds/health          → 200 "ok"
#      * /storage/health     → 200 "ok"
#      * /discovery/health   → 200 "ok"
#      * /push-gw/health     → 200 "ok"
#
#   3. The same-origin `/api/*` proxies on lakko.me work (used by
#      the web client's relative-URL convention):
#      * https://lakko.me/api/auth/health
#      * https://lakko.me/api/ds/health
#      * https://lakko.me/api/storage/health
#      * https://lakko.me/api/discovery/health
#      * https://lakko.me/api/push/health
#
#   4. /version on each service returns JSON with the expected
#      `service` name — confirms the right binary is bound to the
#      right port (cheap regression check after a deploy).
#
# Usage:
#   ./infra/scripts/staging-smoke.sh
#   ./infra/scripts/staging-smoke.sh https://api.lakko.me https://lakko.me
#
# Exit codes:
#   0   — every check passed
#   1   — at least one check failed (the failed line is printed)
#   2   — invalid args / curl missing

set -euo pipefail

if ! command -v curl >/dev/null 2>&1; then
    echo "ERROR: curl is required" >&2
    exit 2
fi

WEB_BASE="${1:-https://lakko.me}"
API_BASE="${2:-https://api.lakko.me}"

# Counters for the summary line.
PASS=0
FAIL=0

# Probe a single URL + assert the status code + an optional body
# substring. Prints "ok URL" on pass, "FAIL URL reason" on fail.
probe() {
    local url="$1"
    local want_status="$2"
    local want_substring="${3:-}"
    local response
    if ! response=$(curl -sS -o /tmp/lakko-smoke-body -w '%{http_code}' --max-time 10 "$url"); then
        echo "FAIL  $url  (curl error)"
        FAIL=$((FAIL + 1))
        return
    fi
    if [[ "$response" != "$want_status" ]]; then
        echo "FAIL  $url  (got HTTP $response, want $want_status)"
        FAIL=$((FAIL + 1))
        return
    fi
    if [[ -n "$want_substring" ]]; then
        if ! grep -q "$want_substring" /tmp/lakko-smoke-body; then
            echo "FAIL  $url  (body missing substring '$want_substring')"
            FAIL=$((FAIL + 1))
            return
        fi
    fi
    echo "ok    $url"
    PASS=$((PASS + 1))
}

echo "==> nginx layer"
probe "$WEB_BASE/health" 200 "lakko.me ok"
probe "$API_BASE/health" 200 "api.lakko.me ok"

echo "==> backend services via api.lakko.me"
probe "$API_BASE/auth/health"        200 ok
probe "$API_BASE/ds/health"          200 ok
probe "$API_BASE/storage/health"     200 ok
probe "$API_BASE/discovery/health"   200 ok
probe "$API_BASE/push-gw/health"     200 ok

echo "==> same-origin /api/* proxies on lakko.me"
probe "$WEB_BASE/api/auth/health"        200 ok
probe "$WEB_BASE/api/ds/health"          200 ok
probe "$WEB_BASE/api/storage/health"     200 ok
probe "$WEB_BASE/api/discovery/health"   200 ok
probe "$WEB_BASE/api/push/health"        200 ok

echo "==> /version sanity check (right binary on the right port)"
probe "$API_BASE/auth/version"      200 lakko-svc-auth
probe "$API_BASE/ds/version"        200 lakko-svc-ds
probe "$API_BASE/storage/version"   200 lakko-svc-storage
probe "$API_BASE/discovery/version" 200 lakko-svc-discovery
probe "$API_BASE/push-gw/version"   200 lakko-svc-push-gw

rm -f /tmp/lakko-smoke-body

echo
echo "==> summary: $PASS passed, $FAIL failed"
if [[ "$FAIL" -gt 0 ]]; then
    exit 1
fi
