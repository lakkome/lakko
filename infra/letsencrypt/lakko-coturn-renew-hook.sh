#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Let's Encrypt deploy-hook for the coturn VPS (G3-turn-deploy).
#
# certbot runs every file under `/etc/letsencrypt/renewal-hooks/deploy/`
# whenever a cert is successfully renewed (the default install on
# Ubuntu 24.04 schedules `certbot renew` twice daily via the
# `certbot.timer` systemd unit). Hooks run with full root + the
# renewed cert files already in place at
# `/etc/letsencrypt/live/<domain>/`.
#
# What this hook does:
#
#   1. Filter — only act when the renewed cert is the one coturn
#      uses. certbot exports `$RENEWED_DOMAINS` (space-separated
#      list of every domain covered by the cert that just
#      renewed); we grep for `turn.lakko.me`.
#
#   2. Reload coturn — `systemctl reload coturn` sends SIGUSR2 to
#      the running turnserver, which re-reads the cert files
#      WITHOUT dropping in-flight relay sessions. (`systemctl
#      restart` would; the difference matters in a call running
#      across the renewal window.)
#
#   3. Reload nginx — the `infra/nginx/sites-available/turn.conf`
#      vhost reads the same cert for the mTLS Prometheus front
#      on `:9642`. `systemctl reload nginx` is the documented
#      cert-pickup path; same SIGHUP-based reload.
#
# Install:
#
#     install -m 755 lakko-coturn-renew-hook.sh \
#         /etc/letsencrypt/renewal-hooks/deploy/lakko-coturn
#
# Verify (dry-run, doesn't actually renew):
#
#     certbot renew --dry-run
#
# Manually run for debugging (no actual renewal triggered):
#
#     RENEWED_DOMAINS='turn.lakko.me' \
#         /etc/letsencrypt/renewal-hooks/deploy/lakko-coturn

set -euo pipefail

# `$RENEWED_DOMAINS` is the canonical certbot env-var; not set
# means a manual invocation — fall through to the reload either
# way (manual invocations are how the operator forces a cert
# rotation after replacing the secret).
DOMAINS="${RENEWED_DOMAINS:-turn.lakko.me}"

# Match against turn.lakko.me as a whole word so a future
# subdomain like `lakko-coturn-staging.lakko.me` does not
# accidentally trigger this hook for the production VPS.
if ! [[ " $DOMAINS " =~ \ turn\.lakko\.me\  ]]; then
    # Not us — exit silently. certbot may be renewing certs for
    # other services that share this hook directory.
    exit 0
fi

logger -t lakko-coturn-renew "renewing TLS for $DOMAINS — reloading coturn + nginx"

# coturn's SIGUSR2-based reload re-reads the cert without
# dropping live relay sessions. systemctl translates `reload`
# into the right signal for us (see ExecReload= in
# /etc/systemd/system/coturn.service.d/lakko-override.conf).
systemctl reload coturn

# nginx's SIGHUP reload is a zero-downtime worker swap — the
# existing workers finish their in-flight requests, the new
# workers come up with the new cert.
systemctl reload nginx
