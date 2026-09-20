#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Build the working tree and deploy it to the test stack (`just deploy-test`).
# Runs on the developer machine; the server never builds and never pulls git,
# so uncommitted changes can be tested.
#
#   1. Linux x86_64 service binaries, built in Dockerfile.builder
#   2. The web client, with the SFU URL of the test domain
#   3. Upload, install, restart lakko-test.target, health-check every route
#
# Environment:
#   LAKKO_TEST_DOMAIN   default test.lakko.me
#   LAKKO_TEST_SSH      default root@$LAKKO_TEST_DOMAIN
#   LAKKO_TEST_SSH_KEY  optional private key for ssh/scp
#   SKIP_BACKEND=1      deploy the web client only
#   SKIP_WEB=1          deploy the services only
#   JOBS                cargo jobs inside the container (default 3)
set -euo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
HERE="$REPO/infra/test-stack"
DOMAIN="${LAKKO_TEST_DOMAIN:-test.lakko.me}"
SSH_TARGET="${LAKKO_TEST_SSH:-root@$DOMAIN}"
OUT="$REPO/target/test-stack"
SERVICES=(auth ds storage discovery push-gw gif-proxy channels sfu)

# accept-new records the host key on first contact and still refuses a changed one.
SSH_OPTS=(-o BatchMode=yes -o StrictHostKeyChecking=accept-new)
if [[ -n ${LAKKO_TEST_SSH_KEY:-} ]]; then SSH_OPTS+=(-i "$LAKKO_TEST_SSH_KEY"); fi
remote() { ssh "${SSH_OPTS[@]}" "$SSH_TARGET" "$@"; }

log() { printf '[deploy-test] %s\n' "$*"; }

if [[ ${SKIP_BACKEND:-0} != 1 ]]; then
  log "building service binaries (linux/amd64)"
  mkdir -p "$OUT/bin" "$OUT/units"
  docker build --platform linux/amd64 -q -t lakko-linux-builder -f "$HERE/Dockerfile.builder" "$HERE" >/dev/null
  pkgs=()
  for svc in "${SERVICES[@]}"; do pkgs+=(-p "lakko-svc-$svc"); done
  docker run --rm --platform linux/amd64 \
    -v "$REPO":/src \
    -v lakko-amd64-registry:/opt/cargo/registry \
    -v lakko-amd64-git:/opt/cargo/git \
    -v lakko-amd64-target:/target \
    -v "$OUT/bin":/out \
    -e CARGO_TARGET_DIR=/target \
    -e CARGO_BUILD_JOBS="${JOBS:-3}" \
    -w /src \
    lakko-linux-builder \
    bash -c "set -euo pipefail; cargo build --release --locked ${pkgs[*]}; for s in ${SERVICES[*]}; do cp /target/release/lakko-svc-\$s /out/; done"
  "$HERE/render-units.sh" "$OUT/units"
fi

if [[ ${SKIP_WEB:-0} != 1 ]]; then
  log "building web client"
  # The VAPID public key is generated on the server by setup-server.sh; an
  # absent key builds a client that simply does not offer Web Push.
  vapid_public="$(remote 'cat /etc/lakko-test/vapid_public.b64url 2>/dev/null || true' | tr -d '[:space:]')"
  [[ -n $vapid_public ]] || log "no VAPID public key on the server; Web Push stays off"
  (cd "$REPO/apps/web" && VITE_LAKKO_SFU_URL="wss://$DOMAIN/sfu" \
    VITE_LAKKO_WEBPUSH_VAPID_PUBLIC_KEY_B64URL="$vapid_public" npm run build)
fi

log "uploading to $SSH_TARGET"
remote 'rm -rf /tmp/lakko-test-deploy && install -d -m 700 /tmp/lakko-test-deploy'
if [[ ${SKIP_BACKEND:-0} != 1 ]]; then
  scp -q "${SSH_OPTS[@]}" "$OUT"/bin/lakko-svc-* "$OUT"/units/* "$SSH_TARGET":/tmp/lakko-test-deploy/
fi
if [[ ${SKIP_WEB:-0} != 1 ]]; then
  rsync -a --delete -e "ssh ${SSH_OPTS[*]}" "$REPO/apps/web/dist/" "$SSH_TARGET":/tmp/lakko-test-deploy/web/
fi

log "installing and restarting"
remote "SKIP_BACKEND=${SKIP_BACKEND:-0} SKIP_WEB=${SKIP_WEB:-0} bash -s" <<'REMOTE'
set -euo pipefail
cd /tmp/lakko-test-deploy
if [[ $SKIP_BACKEND != 1 ]]; then
  install -m 644 lakko-test-*.service lakko-test.target /etc/systemd/system/
  install -m 755 lakko-svc-* /opt/lakko-test/bin/
  systemctl daemon-reload
  systemctl enable lakko-test.target lakko-test-*.service >/dev/null 2>&1
  # The SFU fetches the JWKS from auth at startup, so auth goes first.
  systemctl restart lakko-test-auth
  systemctl restart lakko-test.target
fi
if [[ $SKIP_WEB != 1 ]]; then
  rsync -a --delete web/ /var/www/lakko-test-web/
  # iOS app-link association. Apple matches the app by "<team id>.<bundle id>",
  # and the team id is not in the repository, so the file is written here from
  # LAKKO_APPLE_TEAM_ID in the stack's env; without it there is no file and an
  # iPhone opens the link in Safari.
  team_id="$(sed -n 's/^LAKKO_APPLE_TEAM_ID=//p' /etc/lakko-test/lakko.env | tr -d '"'"'"' \r' | head -n1)"
  if [[ -n $team_id ]]; then
    install -d -m 755 /var/www/lakko-test-web/.well-known
    printf '{"applinks":{"details":[{"appIDs":["%s.me.lakko.dev"],"components":[{"/":"/i/*"},{"/":"/g/*"},{"/":"/c/*"}]}]}}\n' "$team_id" \
      > /var/www/lakko-test-web/.well-known/apple-app-site-association
  else
    echo "[deploy-test] WARN LAKKO_APPLE_TEAM_ID is not set in /etc/lakko-test/lakko.env; no apple-app-site-association written (iOS links open in Safari)"
  fi
  chown -R root:root /var/www/lakko-test-web
  find /var/www/lakko-test-web -type d -exec chmod 755 {} +
  find /var/www/lakko-test-web -type f -exec chmod 644 {} +
fi
rm -rf /tmp/lakko-test-deploy
REMOTE

log "health checks"
failed=0
for path in api/auth/health api/ds/health api/storage/health api/discovery/health api/push/health api/channels/health sfu/health; do
  code=000
  for _ in $(seq 1 15); do
    code=$(curl -s -m 5 -o /dev/null -w '%{http_code}' "https://$DOMAIN/$path" || true)
    [[ $code == 200 ]] && break
    sleep 2
  done
  printf '  %-22s %s\n' "$path" "$code"
  [[ $code == 200 ]] || failed=1
done
code=$(curl -s -m 5 -o /dev/null -w '%{http_code}' "https://$DOMAIN/" || true)
printf '  %-22s %s\n' "/" "$code"
[[ $code == 200 ]] || failed=1

if [[ $failed == 1 ]]; then
  log "FAILED: see 'journalctl -u lakko-test-<service>' on the server"
  exit 1
fi
log "OK: https://$DOMAIN"
