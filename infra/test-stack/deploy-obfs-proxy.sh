#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Put the anti-censorship tunnel on the test stack (`just deploy-test-obfs`):
# build services/obfs-proxy, install and start it alone, and publish a signed
# bootstrap config at https://$DOMAIN/bootstrap.json. No other service restarts
# and the web client is not touched, so this does not replace what another
# session deployed. Safe to run again: secrets on the server and the signing key
# here are kept, and the config is signed afresh.
#
# On the server, once: secrets in /etc/lakko-test/lakko.env, a plain-HTTP
# listener for the relay on 127.0.0.1:8180 in the test vhost (nothing else
# reaches it), the static /bootstrap.json location, and ufw rules for the two
# public ports.
#
# Environment:
#   LAKKO_TEST_DOMAIN     default test.lakko.me
#   LAKKO_TEST_SSH        default root@$LAKKO_TEST_DOMAIN
#   LAKKO_TEST_SSH_KEY    optional private key for ssh/scp
#   LAKKO_BOOTSTRAP_DIR   where the signing seed and trust anchor stay on this
#                         machine, default ~/.lakko/test-stack
#   LAKKO_BOOTSTRAP_TTL_DAYS  config lifetime, default 30
#   JOBS                  cargo jobs inside the container (default 3)
set -euo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
HERE="$REPO/infra/test-stack"
DOMAIN="${LAKKO_TEST_DOMAIN:-test.lakko.me}"
SSH_TARGET="${LAKKO_TEST_SSH:-root@$DOMAIN}"
KEY_DIR="${LAKKO_BOOTSTRAP_DIR:-$HOME/.lakko/test-stack}"
TTL_DAYS="${LAKKO_BOOTSTRAP_TTL_DAYS:-30}"
OUT="$REPO/target/test-stack"
SS_PORT=8388
REALITY_PORT=8444
# What a REALITY handshake shows a network. Chosen, not measured.
COVER_SNI=www.microsoft.com

SSH_OPTS=(-o BatchMode=yes -o StrictHostKeyChecking=accept-new)
if [[ -n ${LAKKO_TEST_SSH_KEY:-} ]]; then SSH_OPTS+=(-i "$LAKKO_TEST_SSH_KEY"); fi
remote() { ssh "${SSH_OPTS[@]}" "$SSH_TARGET" "$@"; }
log() { printf '[deploy-obfs] %s\n' "$*"; }

log "building lakko-svc-obfs-proxy (linux/amd64)"
mkdir -p "$OUT/bin" "$OUT/units"
docker build --platform linux/amd64 -q -t lakko-linux-builder -f "$HERE/Dockerfile.builder" "$HERE" >/dev/null
target_volume="lakko-amd64-target-$(printf '%s' "$REPO" | shasum | cut -c1-12)"
docker run --rm --platform linux/amd64 \
  -v "$REPO":/src \
  -v lakko-amd64-registry:/opt/cargo/registry \
  -v lakko-amd64-git:/opt/cargo/git \
  -v "$target_volume":/target \
  -v "$OUT/bin":/out \
  -e CARGO_TARGET_DIR=/target \
  -e CARGO_BUILD_JOBS="${JOBS:-3}" \
  -w /src \
  lakko-linux-builder \
  bash -c "set -euo pipefail; cargo build --release --locked -p lakko-svc-obfs-proxy; cp /target/release/lakko-svc-obfs-proxy /out/"
"$HERE/render-units.sh" "$OUT/units"

log "uploading"
remote 'rm -rf /tmp/lakko-obfs-deploy && install -d -m 700 /tmp/lakko-obfs-deploy'
scp -q "${SSH_OPTS[@]}" "$OUT/bin/lakko-svc-obfs-proxy" "$OUT/units/lakko-test-obfs-proxy.service" \
  "$SSH_TARGET":/tmp/lakko-obfs-deploy/

log "installing on the server"
remote "SS_PORT=$SS_PORT REALITY_PORT=$REALITY_PORT COVER_SNI=$COVER_SNI bash -s" <<'REMOTE'
set -euo pipefail
ENV_FILE=/etc/lakko-test/lakko.env
VHOST=/etc/nginx/sites-available/lakko-test
add() { grep -q "^${1%%=*}=" "$ENV_FILE" || echo "$1" >> "$ENV_FILE"; }
add "LAKKO_OBFS_PROXY_TARGET=127.0.0.1:8180"
add "LAKKO_OBFS_PROXY_SS_BIND=0.0.0.0:$SS_PORT"
add "LAKKO_OBFS_PROXY_SS_PSK=$(openssl rand -base64 32)"
add "LAKKO_OBFS_PROXY_REALITY_BIND=0.0.0.0:$REALITY_PORT"
add "LAKKO_OBFS_PROXY_REALITY_SNI=$COVER_SNI"
add "LAKKO_OBFS_PROXY_REALITY_PAIRS=$(openssl rand -hex 8):$(openssl rand -base64 32)"

# The server's vhost may be behind the repository's, so the two lines go in
# where they belong and nothing else in the file changes.
grep -q 'listen 127.0.0.1:8180;' "$VHOST" || sed -i \
  '0,/listen \[::\]:8443 ssl http2 proxy_protocol;/s//&\n    # The obfs-proxy relay (ADR-0026). Loopback only.\n    listen 127.0.0.1:8180;/' "$VHOST"
grep -q 'location = /bootstrap.json' "$VHOST" || sed -i \
  '0,/^    root \/var\/www\/lakko-test-web;/s//    # The signed anti-censorship bootstrap config (ADR-0026).\n    location = \/bootstrap.json {\n        root \/var\/www\/lakko-test-bootstrap;\n        default_type application\/json;\n        add_header Cache-Control "no-store" always;\n    }\n\n&/' "$VHOST"
install -d -m 755 /var/www/lakko-test-bootstrap
nginx -t -q
systemctl reload nginx

ufw allow "$SS_PORT/tcp" comment 'Lakko test obfs Shadowsocks' >/dev/null
ufw allow "$REALITY_PORT/tcp" comment 'Lakko test obfs REALITY' >/dev/null

cd /tmp/lakko-obfs-deploy
install -m 644 lakko-test-obfs-proxy.service /etc/systemd/system/
install -m 755 lakko-svc-obfs-proxy /opt/lakko-test/bin/
systemctl daemon-reload
systemctl enable lakko-test-obfs-proxy >/dev/null 2>&1
systemctl restart lakko-test-obfs-proxy
rm -rf /tmp/lakko-obfs-deploy
REMOTE

log "signing the bootstrap config"
install -d -m 700 "$KEY_DIR"
tool="cargo run -q --manifest-path $REPO/Cargo.toml -p lakko-bootstrap-config-tool --"
if [[ ! -f $KEY_DIR/bootstrap-signing-key.seed ]]; then
  $tool keygen --out "$KEY_DIR/bootstrap-signing-key.seed" | sed -n 's/^public key: *//p' > "$KEY_DIR/trust-anchor"
fi
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
value() { remote "sed -n 's/^$1=//p' /etc/lakko-test/lakko.env" | head -n1; }
ip="$(value LAKKO_SFU_MEDIA_ADVERTISE_IP)"
psk="$(value LAKKO_OBFS_PROXY_SS_PSK)"
pair="$(value LAKKO_OBFS_PROXY_REALITY_PAIRS)"
cat > "$scratch/transports.json" <<JSON
[
  {"kind":"reality","sni":"$COVER_SNI","tag_secret_b64":"${pair#*:}","short_id_hex":"${pair%%:*}",
   "allowed_inner_servers":[],"remote":"$ip:$REALITY_PORT","require_server_response":true},
  {"kind":"shadowsocks-2022","remote":"$ip:$SS_PORT","method":"2022-blake3-chacha20-poly1305","psk_b64":"$psk"}
]
JSON
$tool sign --transports "$scratch/transports.json" --ttl-days "$TTL_DAYS" \
  --key "$KEY_DIR/bootstrap-signing-key.seed" --out "$scratch/bootstrap.json"
scp -q "${SSH_OPTS[@]}" "$scratch/bootstrap.json" "$SSH_TARGET":/var/www/lakko-test-bootstrap/bootstrap.json
remote 'chmod 644 /var/www/lakko-test-bootstrap/bootstrap.json'

log "checking"
code=$(curl -s -m 10 -o /dev/null -w '%{http_code}' "https://$DOMAIN/bootstrap.json" || true)
printf '  %-22s %s\n' bootstrap.json "$code"
remote 'systemctl is-active lakko-test-obfs-proxy; curl -s -m 5 -o /dev/null -w "  internal listener      %{http_code}\n" -H "Host: test.lakko.me" http://127.0.0.1:8180/api/auth/health'
[[ $code == 200 ]]
log "OK. Trust anchor for a client build: $KEY_DIR/trust-anchor"
