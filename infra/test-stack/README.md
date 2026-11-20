<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Test stack (`test.lakko.me`)

A manual-testing deployment of the backend and the web client on one server. Nothing here
is production configuration: proof-of-work difficulty is 0 and every name carries `test`.

The server never builds and never pulls git. `just deploy-test` builds the **working
tree** on the developer machine and uploads the result, so uncommitted changes can be
tested.

## Redeploy

```bash
just deploy-test
```

Builds Linux x86_64 binaries in `Dockerfile.builder`, builds the web client with
`VITE_LAKKO_SFU_URL=wss://$LAKKO_TEST_DOMAIN/sfu`, uploads both, restarts
`lakko-test.target` and health-checks every route. `SKIP_WEB=1` or `SKIP_BACKEND=1` deploys
half. Environment: `LAKKO_TEST_DOMAIN` (default `test.lakko.me`), `LAKKO_TEST_SSH` (default
`root@$LAKKO_TEST_DOMAIN`), `LAKKO_TEST_SSH_KEY`.

Needs Docker, Node and SSH access as root. The first build fills the Docker volumes
`lakko-amd64-registry`, `lakko-amd64-git` and `lakko-amd64-target`; later builds are
incremental.

## Layout on the server

| What | Where |
|---|---|
| Binaries | `/opt/lakko-test/bin/lakko-svc-*` |
| Units | `lakko-test-<service>.service`, `lakko-test.target` (rendered from `infra/systemd` by `render-units.sh` on every deploy) |
| Environment and secrets | `/etc/lakko-test/lakko.env` (root:lakkotest 640, generated on the server) |
| Web Push VAPID keys | `/etc/lakko-test/vapid_private.pem` (PKCS#8, 640) and `vapid_public.b64url` (read by `deploy.sh` for the web build) |
| Databases | `lakko_test_{auth,ds,discovery,push_gw,channels}`, user `lakko_test` |
| Blobs | `/var/lib/lakko-test/blobs` |
| Web client | `/var/www/lakko-test-web` |
| nginx | `/etc/nginx/sites-available/lakko-test` from `nginx/lakko-test.conf`; `/etc/nginx/snippets/lakko-test-security-headers.inc` from `nginx/lakko-test-security-headers.inc` |
| Ports | services on `127.0.0.1:8081-8086,8091`, SFU signaling `127.0.0.1:7880`, SFU media UDP `43210` public |

Logs: `journalctl -u lakko-test-<service>`.

Optional SFU switches, set by hand in `/etc/lakko-test/lakko.env` and followed by
`systemctl restart lakko-test-sfu` (`setup-server.sh` does not write them):

- `LAKKO_SFU_VIDEO_FEC=1` negotiates video `red` + `ulpfec` and makes the SFU rebuild a
  publisher's lost video packets from the publisher's own FEC (ADR-0067 step 4a). Unset or
  `0` is off. Read `services/sfu/README.md`, "Video FEC": not yet seen against a real
  browser. The SFU also sends its own FEC to each subscriber that negotiated the pair
  (step 4b).
- `LAKKO_SFU_BITRATE_CONTROL=server` makes the SFU decide each publisher's bitrate
  (ADR-0068): it negotiates no transport-wide congestion control with any client (no
  `transport-cc`, no transport-wide sequence number; `abs-send-time` and `goog-remb` stay),
  estimates what each publisher's path carries and tells the publisher with REMB. Unset or
  `client` is the behaviour before ADR-0068; any other value stops the SFU at startup. The
  mode is logged once at startup. Two optional figures, in kbit/s:
  `LAKKO_SFU_BITRATE_START_KBPS` (default 1500, where an estimate starts) and
  `LAKKO_SFU_BITRATE_MAX_KBPS` (default 5000, its ceiling). With
  `LAKKO_SFU_MEDIA_DIAG_SECS` set, each publisher gets a line per interval:
  `media: publisher bitrate estimate over the last interval` with `estimate_kbps`,
  `arriving_kbps`, `state` and `last_change`. Clients must reconnect after the switch is
  changed (the negotiation differs). Read `services/sfu/README.md`, "Bitrate control": not
  yet seen against a real browser.

## Monitoring (ADR-0096 P4)

The monitoring VPS (`infra/README.md`, "Monitoring") scrapes this host under
`stack="test"`, through the same metrics server block as production, on
`test.lakko.me:9643` with the test stack's existing certificate. Each
service's `/metrics` listener (`LAKKO_<SVC>_METRICS_BIND`, 127.0.0.1:9641 and
9651-9660), node_exporter, mysqld_exporter and the nginx exporter listen on
127.0.0.1 only; the firewall opens 9643 to the monitoring VPS alone, and a
client certificate from the monitoring CA is required. The test coturn
instances (edge, edge2) export no metrics; edge's TURN-over-TLS on 443 is
probed instead. Test-stack alerts go to Telegram only, never to e-mail.

Setup, once the monitoring VPS exists:

```bash
just push-monitoring-ca-test
LAKKO_MONITORING_IP=<monitoring VPS address> just setup-metrics-test
just deploy-test        # restarts lakko-test.target, so the listeners open
```

`setup-metrics-test` copies `infra/` from the working tree to
`/root/lakko-infra/` on the server and runs `infra/scripts/setup-metrics.sh`
with `STACK=test`: the exporters, journald retention, the schema-size timer,
the metrics lines in `/etc/lakko-test/lakko.env`, the server block
(`/etc/nginx/sites-available/lakko-metrics.conf`) and the firewall rule. No
backup timers: the test stack's data is disposable.

## One-time setup

1. DNS: an `A` record for the domain, **not** proxied by Cloudflare (the SFU media port and
   TURN need the real address).
2. On the server, as root:
   ```bash
   PUBLIC_IP=<server ip> DOMAIN=test.lakko.me \
   TURN_URIS='turn:edge2.lakko.me:3478?transport=udp,turn:edge.lakko.me:3478?transport=tcp,turns:edge.lakko.me:443?transport=tcp' \
   bash setup-server.sh
   ```
   Omit `TURN_URIS` when the server has no coturn; auth then answers 503 on the TURN route.
3. nginx: copy `infra/nginx/sites-available/lakko-proxy-headers.inc` to
   `/etc/nginx/snippets/lakko-test-proxy-headers.inc` and
   `nginx/lakko-test-security-headers.inc` to
   `/etc/nginx/snippets/lakko-test-security-headers.inc`, install `nginx/lakko-test.conf` as
   `/etc/nginx/sites-available/lakko-test` with the TLS server block commented out, enable
   it, issue the certificate with
   `certbot certonly --webroot -w /var/www/certbot -d test.lakko.me`, restore the TLS block,
   `nginx -t && systemctl reload nginx`.
4. `just deploy-test`.

## Server-specific notes (the current Hetzner host)

- Port 443 is an nginx `stream` SNI router: `edge.lakko.me` goes to the TURN path below,
  everything else to the HTTP server on `127.0.0.1:8443`. The router sends PROXY protocol,
  so every `:8443` listener carries `proxy_protocol` and `real_ip_header proxy_protocol`.
  Without this every client appears as `127.0.0.1` and shares one discovery rate-limit
  bucket.
- Two coturn instances share the host: `coturn` (`/etc/turnserver.conf`, edge.lakko.me,
  TCP and TLS) and `coturn2` (`/etc/turnserver2.conf`, edge2.lakko.me, UDP only; it also
  listens for DTLS on UDP 5349 and loads that certificate at startup).
- Every TCP and TLS path to `coturn` goes through HAProxy (`/etc/haproxy/haproxy.cfg`):
  `127.0.0.1:5350` (443 via the router, PROXY v1 in), and the public `3478/tcp` and
  `5349/tcp` on IPv4 and IPv6. HAProxy terminates TLS for edge.lakko.me
  (`/etc/haproxy/certs/edge.lakko.me.pem`, fullchain plus key) and hands `coturn` plain
  TURN with the client address as PROXY v2 on `tcp-proxy-port=5351`, which ufw does not
  open. coturn 4.5.2 takes a client address only as binary PROXY v2, which nginx 1.18
  cannot send, its proxy port speaks plain TCP, not TLS, and with that port set it opens
  no other listener (measured 2026-10-24: TCP 3478 and TLS 5349 were gone until HAProxy
  took them). Before this, coturn saw every TLS client on 443 as `127.0.0.1` and reported
  that as the mapped address, and Firefox drops a TURN allocation whose mapped address is
  a loopback one: it gathered no relay candidate through `turns:edge.lakko.me:443` while
  Chromium and WebKit did.
- No connection log, for Lakko invariant #5: neither coturn has `verbose` or `syslog`, both
  write `log-file=/dev/null` (coturn writes only its start-up lines to
  `/var/tmp/turn_<pid>_<date>.log` before it reads the config), and HAProxy has no `log`
  in `defaults` or its proxies. Journal entries from before 2026-10-25 still hold client
  addresses.
- The IPv6 addresses of edge (`::ed6e`) and edge2 (`::ed62`), named by their AAAA records
  and by each config's `relay-ip` / `external-ip`, are added to `eth0` by
  `/etc/netplan/60-lakko-edge.yaml`. They were missing until 2026-10-24, so nothing
  answered on either AAAA record. STUN over IPv6 answers on edge2's address only (UDP
  there is `coturn2`'s); clients take STUN from the UDP TURN URI alone, which names edge2.
- nginx 1.18 knows no `.wasm` MIME type; the vhost sets `application/wasm` itself.
- `/etc/letsencrypt/renewal-hooks/deploy/lakko-reload.sh` rebuilds HAProxy's PEM and
  reloads HAProxy after an edge.lakko.me renewal, restarts `coturn2` after an
  edge2.lakko.me renewal, and reloads nginx.

## TURN on the test host (`turn/`)

The files in `turn/` are the TURN setup of the current host, one per file on the server
(each names its path in its header). Two coturn instances share the host (edge.lakko.me
TCP and TLS behind HAProxy, edge2.lakko.me UDP), behind the nginx SNI router on 443. To
build it on a fresh host, as root, after the main one-time setup:

1. DNS: `edge.lakko.me` and `edge2.lakko.me` with `A` = the host's IPv4 and `AAAA` =
   `…::ed6e` and `…::ed62` of its /64 (not proxied). Install `60-lakko-edge.yaml` as
   `/etc/netplan/60-lakko-edge.yaml` (mode 600) and add the two addresses now with
   `ip -6 addr add <addr>/64 dev eth0` (or `netplan apply`, which may briefly drop the
   network). Change the addresses in the two turnserver configs and the netplan file
   for another host.
2. `apt-get install coturn haproxy`; `systemctl disable --now coturn` before replacing the
   packaged unit. Certificates for both names: `certbot certonly --webroot -w /var/www/certbot
   -d edge.lakko.me` and the same for `edge2.lakko.me`.
3. A shared secret: `openssl rand -hex 32`, written over `@TURN_SHARED_SECRET@` in both
   `turnserver-edge.conf` (to `/etc/turnserver.conf`) and `turnserver-edge2.conf` (to
   `/etc/turnserver2.conf`), mode 640, group `turnserver`. Run `setup-server.sh` with
   `TURN_CONF=/etc/turnserver.conf` and `TURN_URIS` as in step 2 of the one-time setup, so
   auth mints credentials from the same secret.
4. `coturn.service` and `coturn2.service` to `/etc/systemd/system/`; `systemctl daemon-reload`;
   `systemctl enable --now coturn coturn2`.
5. HAProxy: `install -d -m 0750 -o root -g haproxy /etc/haproxy/certs`, then run
   `lakko-reload.sh` once with `RENEWED_DOMAINS=edge.lakko.me` to build
   `/etc/haproxy/certs/edge.lakko.me.pem`; `haproxy.cfg` to `/etc/haproxy/haproxy.cfg`;
   `haproxy -c -f /etc/haproxy/haproxy.cfg && systemctl enable --now haproxy`.
6. nginx: the `stream {}` block of `nginx-stream.conf` at the top level of
   `/etc/nginx/nginx.conf` (beside `http {}`); `nginx -t && systemctl reload nginx`.
7. `lakko-reload.sh` to `/etc/letsencrypt/renewal-hooks/deploy/lakko-reload.sh`, mode 755.
8. ufw: allow `3478/udp`, `3478/tcp`, `5349/tcp`, `5349/udp`, `443/tcp` and the relay ranges
   `49152:65535/udp`; leave `5351` closed (coturn's PROXY v2 port trusts the address in the
   header).
9. Check: `ss -ltnp` shows HAProxy on `127.0.0.1:5350`, `:3478` and `:5349` and coturn on
   `:5351` only; a STUN binding to UDP 3478 of edge2 over IPv4 and IPv6 answers; a call with
   only `turns:edge.lakko.me:443` connects in Firefox.

## App links (a tapped link opens the installed app)

The web build ships `/.well-known/assetlinks.json` (Android; it names the
package `me.lakko.dev` and the debug signing key's SHA-256, so it is per
developer machine until a release key exists) and nginx serves both
association files as JSON from the web root. Android verifies
`test.lakko.me` against it on install (`adb shell pm get-app-links
me.lakko.dev` shows `verified`), and the debug app mints its `/i/`, `/g/` and
`/c/` links under `https://test.lakko.me` (`LAKKO_WEB_ORIGIN`).

iOS needs `/.well-known/apple-app-site-association` naming
`<team id>.me.lakko.dev`. The team id is not in the repository: set
`LAKKO_APPLE_TEAM_ID=<team id>` in `/etc/lakko-test/lakko.env` and the next
web deploy writes the file; without it the deploy prints a WARN and an iPhone
opens the link in Safari. The app's entitlement already claims
`applinks:test.lakko.me`, but a simulator build signed without a team embeds
no entitlements at all, so this can only be checked on a device signed by that
team.

## Anti-censorship tunnel

`just deploy-test-obfs` (with `LAKKO_TEST_SSH_KEY`) builds `services/obfs-proxy`,
installs it as `lakko-test-obfs-proxy`, restarts only that unit and publishes a
signed config at `https://test.lakko.me/bootstrap.json`. It does not touch the
web client. Public ports: REALITY 8444/tcp, Shadowsocks-2022 8388/tcp. The relay
reaches the test vhost on `127.0.0.1:8180`, a plain listener nothing outside the
server can reach. Secrets are `LAKKO_OBFS_PROXY_*` in `/etc/lakko-test/lakko.env`;
the signing seed stays on the machine that ran the script, in
`~/.lakko/test-stack`, beside `trust-anchor`, which a client build pins. A config
lives 30 days; run the script again to sign a new one.
