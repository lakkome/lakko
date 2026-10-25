#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# /etc/letsencrypt/renewal-hooks/deploy/lakko-reload.sh on the test host.
# Reload the consumers of a renewed certificate. certbot sets RENEWED_DOMAINS.
# edge.lakko.me: HAProxy terminates TLS for the edge coturn (443 via the nginx
# SNI router, and 5349) from one combined PEM, rebuilt here; coturn itself no
# longer serves TLS. edge2.lakko.me: coturn2 loads its certificate only at
# startup, so it needs a restart.
for d in $RENEWED_DOMAINS; do
  case "$d" in
    edge.lakko.me)
      umask 027
      cat /etc/letsencrypt/live/edge.lakko.me/fullchain.pem \
          /etc/letsencrypt/live/edge.lakko.me/privkey.pem \
          > /etc/haproxy/certs/edge.lakko.me.pem.new &&
        chown root:haproxy /etc/haproxy/certs/edge.lakko.me.pem.new &&
        mv /etc/haproxy/certs/edge.lakko.me.pem.new /etc/haproxy/certs/edge.lakko.me.pem &&
        systemctl reload haproxy
      ;;
    edge2.lakko.me) systemctl restart coturn2 ;;
  esac
done
systemctl reload nginx
