#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Render the test-stack systemd units from the production units in
# infra/systemd, so a unit change in the repo reaches the test server on the
# next deploy instead of drifting in a hand-kept copy.
#
# Usage: render-units.sh <output-dir>
set -euo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="${1:?usage: render-units.sh <output-dir>}"
mkdir -p "$OUT"

SERVICES=(auth ds storage discovery push-gw gif-proxy channels sfu obfs-proxy moderation)

# The `Environment=LAKKO_*_KEY_PATH` lines dropped below point into
# /etc/lakko/secrets, which the test stack does not have. The test server
# supplies them from /etc/lakko-test/lakko.env instead, if at all:
#
#   LAKKO_APNS_KEY_PATH       - alert token: message wakes
#   LAKKO_APNS_VOIP_KEY_PATH  - call token: a closed iPhone rings only with
#                               this one, plus LAKKO_APNS_VOIP_{KEY_ID,
#                               TEAM_ID,TOPIC} (topic = <bundle id>.voip)
#   LAKKO_FCM_SA_PATH         - Android
#
# Absent, push-gw logs a warning per provider and keeps running with the
# log-only sender for it; see infra/systemd/lakko-svc-push-gw.service.
for svc in "${SERVICES[@]}"; do
  if [[ $svc == sfu ]]; then src=lakko-sfu.service; else src="lakko-svc-$svc.service"; fi
  {
  echo "# Rendered from infra/systemd/$src by infra/test-stack/render-units.sh."
  sed -E \
    -e '/^#/d' \
    -e 's/^Description=/Description=[test] /' \
    -e '/^Documentation=/d' \
    -e 's/^User=lakkome$/User=lakkotest/; s/^Group=lakkome$/Group=lakkotest/' \
    -e 's#^EnvironmentFile=/etc/lakko/lakko.env$#EnvironmentFile=/etc/lakko-test/lakko.env#' \
    -e 's#^ExecStart=/usr/local/bin/#ExecStart=/opt/lakko-test/bin/#' \
    -e 's/lakko\.target/lakko-test.target/g' \
    -e '/^Environment=RUST_LOG=/d' \
    -e '/^Environment=LAKKO_APNS_KEY_PATH=/d' \
    -e '/^Environment=LAKKO_APNS_VOIP_KEY_PATH=/d' \
    -e '/^Environment=LAKKO_FCM_SA_PATH=/d' \
    -e 's#^Environment=LAKKO_BLOB_DIR=.*#Environment=LAKKO_BLOB_DIR=/var/lib/lakko-test/blobs#' \
    -e 's#^ReadWritePaths=/var/lib/lakko/blobs$#ReadWritePaths=/var/lib/lakko-test/blobs#' \
    -e 's#^ReadOnlyPaths=/etc/lakko$#ReadOnlyPaths=/etc/lakko-test#' \
    -e 's#^LoadCredential=lakko-donations:/etc/lakko/credstore/#LoadCredential=lakko-donations:/etc/lakko-test/credstore/#' \
    "$REPO/infra/systemd/$src" | cat -s
  } > "$OUT/lakko-test-$svc.service"
done

{
  echo "[Unit]"
  echo "Description=Lakko test stack"
  echo "Wants=$(printf 'lakko-test-%s.service\n' "${SERVICES[@]}" | paste -sd ' ' -)"
  echo "After=network-online.target"
  echo
  echo "[Install]"
  echo "WantedBy=multi-user.target"
} > "$OUT/lakko-test.target"

# A unit that still points at the production layout would start against the
# wrong user, env file or binary; refuse rather than deploy it.
if grep -l -E '/etc/lakko/|/usr/local/bin/|User=lakkome|[^-]lakko\.target' "$OUT"/lakko-test-*.service; then
  echo "render-units: a production path survived the rewrite (files above)" >&2
  exit 1
fi
