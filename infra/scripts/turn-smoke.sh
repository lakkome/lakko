#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# G3-turn-deploy — local smoke probe for the coturn VPS.
#
# Runs from the dev machine (no ssh, no shell on the VPS).
# Verifies, in order:
#
#   1. DNS — `turn.lakko.me` resolves to at least one A or AAAA.
#   2. STUN port reachable — TCP 3478 accepts a connect.
#   3. TURN-over-TLS handshake — :443 negotiates TLS 1.2+ and
#      presents a cert whose CN / SAN matches `turn.lakko.me`.
#   4. mTLS metrics gate — :9642 refuses an unauthenticated
#      HTTPS handshake with `sslv3 alert handshake failure`.
#      (The full Prometheus scrape requires the client cert
#      from the monitoring CA, which lives only on the
#      monitoring VPS — testing the negative is what we can do
#      from any laptop.)
#
# What this does NOT cover (separate runbook step):
#   - End-to-end ICE relay test from a real client. That needs
#     the lakko app + a network with symmetric NAT to force the
#     fallback to TURN; a synthetic test harness for this lives
#     in the broader G3-4-mobile-call sub-sprint.
#
# Usage:
#
#     bash infra/scripts/turn-smoke.sh [host]
#
# Default host is `turn.lakko.me`; override via the first arg
# or by exporting `LAKKO_TURN_HOST`. Exit code 0 on success,
# non-zero on the first failed check.

set -euo pipefail

HOST="${1:-${LAKKO_TURN_HOST:-turn.lakko.me}}"

note() { printf '\n\033[1;36m==>\033[0m %s\n' "$*"; }
ok()   { printf '   \033[1;32mok\033[0m   %s\n' "$*"; }
fail() { printf '   \033[1;31mFAIL\033[0m %s\n' "$*"; exit 1; }

# --- 1. DNS ------------------------------------------------
note "1/4 DNS resolution for $HOST"
if ! addrs=$(getent hosts "$HOST" 2>/dev/null) && \
   ! addrs=$(host "$HOST" 2>/dev/null | grep -E 'has (IPv6 )?address') ; then
    fail "DNS lookup returned nothing — check the A / AAAA records"
fi
ok "resolved: $(echo "$addrs" | head -1 | tr -s ' ')"

# --- 2. STUN port reachable -------------------------------
note "2/4 STUN port 3478/TCP reachable"
if ! exec 3<>/dev/tcp/"$HOST"/3478 2>/dev/null; then
    fail "TCP connect to $HOST:3478 timed out — firewall? coturn down?"
fi
exec 3<&- 3>&-
ok "TCP :3478 accepts connects"

# --- 3. TURN-over-TLS on :443 -----------------------------
note "3/4 TURN-over-TLS handshake on :443"
# openssl s_client exits 0 on a successful TLS handshake even
# if there's no upper-layer protocol exchange — exactly what
# we want here. The `< /dev/null` closes stdin so s_client
# doesn't hang waiting for a STUN-over-TLS message.
tls_out=$(openssl s_client \
    -connect "$HOST:443" \
    -servername "$HOST" \
    -verify_return_error \
    < /dev/null 2>&1 \
    | head -40 || true)

if ! echo "$tls_out" | grep -qE 'Verify return code: 0 \(ok\)'; then
    echo "$tls_out" | tail -20
    fail "TLS verify_return_error failed — check the cert chain on $HOST:443"
fi

if ! echo "$tls_out" | grep -qE "subject=.*CN[ ]*=[ ]*$HOST"; then
    # SAN-only certs (Let's Encrypt issues these) don't put the
    # name in CN. Fall back to checking the SAN block.
    if ! echo "$tls_out" | grep -qE "DNS:$HOST(,|$)"; then
        fail "cert subject / SAN does not include $HOST"
    fi
fi
proto=$(echo "$tls_out" | grep -oE 'Protocol\s*:\s*TLSv1\.[23]' | head -1 || true)
if [[ -z "$proto" ]]; then
    fail "TLS protocol weaker than 1.2 — check turnserver.conf no-tlsv1*"
fi
ok "TLS handshake OK ($proto, cert chain verified)"

# --- 4. mTLS gate on :9642 --------------------------------
note "4/4 mTLS gate on :9642 refuses unauthenticated clients"
# Expect a `sslv3 alert handshake failure` (or similar) from
# the server, NOT a clean handshake. If the server accepts an
# unauthenticated TLS connection here, the metrics endpoint is
# exposed and we want the operator to notice.
mtls_out=$(openssl s_client \
    -connect "$HOST:9642" \
    -servername "$HOST" \
    < /dev/null 2>&1 \
    | head -40 || true)

# A successful refusal looks like one of these:
#   - "sslv3 alert handshake failure" (TLS 1.2 alert wire)
#   - "tlsv13 alert certificate required" (TLS 1.3 alert)
#   - "peer did not return a certificate"
# Any of those means the mTLS verify_client gate fired.
if echo "$mtls_out" | grep -qE 'alert handshake failure|alert certificate required|did not return a certificate'; then
    ok "mTLS verify_client gate is closed (unauthenticated client rejected)"
elif echo "$mtls_out" | grep -qE 'Verify return code: 0 \(ok\)'; then
    # If the handshake succeeded WITHOUT a client cert, the
    # nginx vhost is misconfigured.
    echo "$mtls_out" | tail -20
    fail "mTLS gate accepted an unauthenticated client — check nginx ssl_verify_client"
elif echo "$mtls_out" | grep -qE 'Connection refused|getaddrinfo|connect:errno'; then
    fail "could not reach $HOST:9642 — nginx down? firewall? port mismatch?"
else
    # Unknown shape — surface it for the operator instead of
    # silently passing.
    echo "$mtls_out" | tail -15
    fail "unexpected response from $HOST:9642 — see above"
fi

echo
echo "$(tput bold 2>/dev/null || true)all turn-smoke checks passed for $HOST$(tput sgr0 2>/dev/null || true)"
