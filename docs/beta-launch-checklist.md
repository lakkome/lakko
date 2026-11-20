<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Public beta (1000 accounts) — what is in it and what must happen first

**Draft, 2026-11-18.** Built from [`PRODUCT-READINESS.md`](PRODUCT-READINESS.md) (last
edited 2026-11-10; several of its rows are older than the fixes since) and from reading
`infra/` and `services/`. Where the readiness document and the code disagreed, the code
was checked and the row says so. Nothing here was walked on a device for this list.
The beta itself is allowed by the [ADR-0046 amendment](adr/0046-external-crypto-review.md).

## 1. Before the first user signs up

| Item | State today | Evidence |
|---|---|---|
| Server-enforced cap of 1000 accounts | Built 2026-11-19 on the server (DB-tested) and on web, desktop, iOS and Android (unit-tested); not walked. The beta (lakko.me, production layout, not yet deployed) takes `LAKKO_AUTH_MAX_ACCOUNTS=1000` from the env template; the test stack (test.lakko.me) has no cap | `services/auth/src/account_cap.rs`, migration `0037`, `LAKKO_AUTH_MAX_ACCOUNTS=1000` in `infra/etc-lakko/lakko.env.example`; UX `docs/ux/beta-account-cap.md`; section 4 |
| "Beta, not audited" notice where a person signs up, on the site and in the store beta text | Built 2026-11-19 on web, desktop, iOS and Android (unit-tested, not walked); store text drafted | `docs/ux/beta-notice.md`, `docs/beta-store-text.md`; the two strings are security-critical, so other languages show English until a person reviews them |
| `SECURITY.md` with an address | Written 2026-11-19 with `security@lakko.me` and `/.well-known/security.txt`; the mailbox exists (maintainer, 2026-11-20); still to do: switch on GitHub private vulnerability reporting, optionally publish an encryption key | `SECURITY.md`, `apps/web/public/.well-known/security.txt` |
| Terms of service and privacy policy, shown at sign-up | Drafts 2026-11-19 (`docs/legal/terms.md`, `docs/legal/privacy.md`, written from a code inventory); links at sign-up and in Settings > About built on the three clients (unit-tested, not walked); the pages render a "being prepared" stub until every TODO is filled. Before publishing: VVG's KvK number and address, a privacy contact address, the mail provider's name, whether Cloudflare proxies traffic, a legal read, and the [FIX BEFORE PUBLISHING] items below | UX `docs/ux/legal-pages.md` |
| Fixes the privacy policy depends on | Account deletion: DONE 2026-11-19 (backup-head rows cascade, migration 0038; auth queues the discovery and storage purge in the delete transaction and a worker retries it, migration 0039 + `services/auth/src/purge.rs`, needs `LAKKO_INTERNAL_PURGE_SECRET` and the internal binds in `lakko.env`; the three clients unsubscribe push and delete the backup after the auth delete, buying the push tokens first); host-tested only. Log retention: 7 days, `infra/scripts/log-retention.sh` + `just log-retention` (coturn on the host keeps no connection log; built 2026-11-19, not yet run on the servers). Still open: offer the existing `GET /account/export` in a client | data inventory 2026-11-19: `services/auth/src/webauthn.rs:419-510`, `apps/web/src/auth/AccountSections.tsx:294-299`, `infra/coturn/turnserver.conf:93` |
| Database and blob backups | Built and tested in Docker 2026-11-19; not set up on the server (needs a Storage Box and `just backup-setup`); no alert on a failed run. Blob bytes can move to Hetzner Object Storage (built, tested against SeaweedFS, not against Hetzner) and then leave the restic backup | `infra/scripts/backup.sh`, `infra/README.md` "Backup / restore" |
| Dependency advisories | Done 2026-11-19: `cargo deny check advisories` passes. 24 fixed by compatible updates (rustls, rustls-webpki, webpki roots, quinn-proto, quinn-udp, lettre, tokio, openssl, ctutils, yanked chacha20) and SQLite by rusqlite 0.41; 3 libcrux entries ignored as unreachable (ML-KEM/X-Wing only); OPEN: crypto-bigint 0.5 timing (needs elliptic-curve 0.14 upstream: SFU DTLS, opaque-ke, hpke-rs); chrono unmaintained | `deny.toml` `[advisories] ignore`, each with its reason |
| CI on merge | Off: every workflow is `workflow_dispatch` | `.github/workflows/ci.yml:44-49` |
| Production secrets chosen: anon-token master secret, vault, TOTP, handle and email peppers, discovery attest key, VAPID | Template only | `infra/etc-lakko/lakko.env.example`; `LAKKO_DISCOVERY_ATTEST_KEY` commented out at line 126, without it a key reset cannot publish |
| Production anti-abuse values (signup proof-of-work effort, anonymous-token quotas, auth and discovery limits) | The beta on lakko.me (not yet deployed) takes the defaults in the code and `infra/etc-lakko/lakko.env.example`; the relaxed values (effort 0, quotas of 1,000,000, limits of 100,000) belong to the test stack on test.lakko.me only. Check the beta's `/etc/lakko/lakko.env` at deploy | `infra/etc-lakko/lakko.env.example`; `infra/test-stack/setup-server.sh:71`, `:170-182` |
| Deploying lakko.me on the machine that runs the test stack | Not done. By the repository's configuration the two use the same loopback ports (8081-8086, 8091, 8092, 8094, 8095, the SFU 7880 and its metrics 9641) and one nginx; the production deploy has never run beside the test stack | `infra/test-stack/setup-server.sh`, `infra/etc-lakko/lakko.env.example`, `infra/scripts/deploy.sh` |
| SMTP for recovery email (ADR-0075) | No letter ever sent through a real relay | PRODUCT-READINESS:71 |
| Block and report a user | Absent on every client | PRODUCT-READINESS:110 |
| A per-account storage quota (today an account may upload up to 2 GiB per hourly pass, about 96 passes a day, and files live 365 days after their last access, so one account can fill the server disk) | No per-account quota. Since 2026-11-19 uploads stop at 10 GiB free disk (`507 storage-full`, `services/storage/src/disk.rs`), so the disk cannot fill, but one account can still use up the space for everyone | `services/storage/src/pass.rs` `PassLimits`, auth `blob-upload` quota 96 a day |
| Delete account removes backup, blobs and journals | Backup, directory and push: yes since 2026-11-19 (host-tested). Messages in the DS expire in 30 days and attachment files by their lifetime, as the privacy draft states | PRODUCT-READINESS:85, :165 |
| Base scenarios walked on every client | 35 scenarios listed, no results recorded | `docs/qa/base-scenarios.md` |

## 2. Before iOS and Android testers can install

| Item | State today | Evidence |
|---|---|---|
| Production bundle id / application id (not `me.lakko.dev`) | Dev id only | `apps/ios/project.yml:115-117`, `apps/android/app/build.gradle.kts:41` |
| iOS team and signing; Android release signing | None configured | no `DEVELOPMENT_TEAM`, no `signingConfig` |
| Release endpoints in the build | Empty strings | `project.yml:69-72`, `build.gradle.kts` release block |
| APNs production entitlement, FCM `google-services.json`, push credentials on the server | `aps-environment: development`, no FCM file | `project.yml:395`; native push is NO in PRODUCT-READINESS:119 |
| `apple-app-site-association` for links | Missing (only `assetlinks.json`, for the dev id) | `apps/web/public/.well-known/` |
| App icon | Missing | PRODUCT-READINESS:150 |

Without native push a phone gets messages only while the app is open, and calls do not
ring in the background (PRODUCT-READINESS:119, :128). Either fix that first or say it in
the beta description.

## 3. What the beta would contain (per PRODUCT-READINESS, to be confirmed by a walk)

- **Works:** one-to-one text, attachments, forward and multi-select, edit / delete /
  reactions, backup that follows the current state, web push, web installed as a PWA.
- **Partly works:** sign-up and sign-in on iOS and Android, recovery paths, second device
  (walked on web only), groups and `/g/` links, contacts, voice and video messages,
  one-to-one calls with both apps open, group calls, channels, the feed (web).
- **Not working or absent:** change password, in-chat search, two tabs on one account
  (web), the offline indicator, screen share and in-call chat (marked NO there, built
  since on web; not walked), store installation.
- **Server pieces not run anywhere:** key transparency log and witness (no systemd unit),
  so the safety-number check against the log is off. `obfs-proxy` runs on the test stack
  only. GIF search answers 503 until a GIPHY key is set.

Decide per item whether it ships in the beta, is hidden, or is shipped with a note.

## 4. The 1000-account cap — as built (server, 2026-11-19)

Count accounts that **have a username**, at the moment the first username is bound
(`services/auth/src/login_handle.rs`, the INSERT branch at about line 413, inside the
existing transaction that already locks the account row `FOR UPDATE`):

- serialise on a one-row lock table, count `login_handles`, refuse at the limit with
  `403 {"error":"registration-closed"}`; a rename or a re-bind is never refused;
- `LAKKO_AUTH_MAX_ACCOUNTS`, `0` = no cap;
- an early answer before the proof of work, `GET /signup/open` -> `{"open": bool}`
  (not on `/pow/challenge`: guests from call links solve that proof too and must still
  get in), so nobody solves the proof only to be refused;
- guests from call links have no username, so they never take a place and can still
  join calls; an account refused at the bind has no name and is removed by the existing
  one-day sweep;
- deleting an account deletes its `login_handles` row (foreign key), which frees a place;
- the count adds no personal data (invariant #1).

Why not cap at account creation: guests and abandoned sign-ups would hold places for up
to a day, and a burst of call-link guests could close sign-up and then block guests from
joining calls. Open question: whether any sign-up path creates an account with a passkey
and no username; such accounts would not count.

Client work on web, desktop, iOS and Android: handle `registration-closed` on the sign-up
and guest-claim screens (today it would show the generic "Could not create the account"),
new catalog keys in `l10n/catalog/en.json`, and a UX spec for the closed state, with a
waitlist or not.
