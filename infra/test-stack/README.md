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
| nginx | `/etc/nginx/sites-available/lakko-test` from `nginx/lakko-test.conf` |
| Ports | services on `127.0.0.1:8081-8086,8091`, SFU signaling `127.0.0.1:7880`, SFU media UDP `43210` public |

Logs: `journalctl -u lakko-test-<service>`.

Optional SFU switch, set by hand in `/etc/lakko-test/lakko.env` and followed by
`systemctl restart lakko-test-sfu` (`setup-server.sh` does not write it):

- `LAKKO_SFU_VIDEO_FEC=1` negotiates video `red` + `ulpfec` and makes the SFU rebuild a
  publisher's lost video packets from the publisher's own FEC (ADR-0067 step 4a). Unset or
  `0` is off. Read `services/sfu/README.md`, "Video FEC": not yet seen against a real
  browser. The SFU also sends its own FEC to each subscriber that negotiated the pair
  (step 4b).

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
   `/etc/nginx/snippets/lakko-test-proxy-headers.inc`, install `nginx/lakko-test.conf` as
   `/etc/nginx/sites-available/lakko-test` with the TLS server block commented out, enable
   it, issue the certificate with
   `certbot certonly --webroot -w /var/www/certbot -d test.lakko.me`, restore the TLS block,
   `nginx -t && systemctl reload nginx`.
4. `just deploy-test`.

## Server-specific notes (the current Hetzner host)

- Port 443 is an nginx `stream` SNI router: `edge.lakko.me` goes to coturn TLS, everything
  else to the HTTP server on `127.0.0.1:8443`. The router sends PROXY protocol, so every
  `:8443` listener carries `proxy_protocol` and `real_ip_header proxy_protocol`, and the
  coturn branch passes through a `127.0.0.1:5350` stream server that strips the header
  (coturn does not accept it). Without this every client appears as `127.0.0.1` and shares
  one discovery rate-limit bucket.
- nginx 1.18 knows no `.wasm` MIME type; the vhost sets `application/wasm` itself.
- coturn loads its certificate only at startup. `/etc/letsencrypt/renewal-hooks/deploy/lakko-reload.sh`
  restarts the matching coturn after a renewal and reloads nginx.

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
