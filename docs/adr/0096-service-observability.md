<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0096: Service observability without a record of who did what

- **Status:** Accepted (2026-11-08, the maintainer). P1 and P2 built the
  same day (sections "P1 as built", "P2 as built"): host-tested, not
  deployed, nothing scraped yet. P3 written 2026-11-20 ("P3 as built"):
  configuration checked in containers, no monitoring VPS exists. P4 written
  the same day ("P4 as built"), likewise. P5 built the same day ("P5 as
  built"): all four steps passed against a local stack; not run against a
  deployed stack.
- **Date:** 2026-11-08 (renumbered from 0089 on 2026-11-20: main had taken
  0089 for camera background and C-90 to C-93, so this ADR's invariant is
  C-94)
- **Context:** operations of the single-VPS deployment
  ([`infra/README.md`](../../infra/README.md)) and the test stack
  ([`infra/test-stack/`](../../infra/test-stack/)); the servers-cannot-link-
  accounts posture of [ADR-0078](0078-servers-cannot-link-accounts.md),
  section 11 (logs) and section 12 (class (c)) in particular; security
  invariants #1, #2 and #5.

## Context

### What exists today (read from the tree on 2026-11-08)

- **One service exports metrics.** `services/sfu/src/prom.rs` installs a
  `metrics-exporter-prometheus` recorder and serves `/metrics` on a separate
  localhost listener (`LAKKO_SFU_METRICS_BIND`, default `127.0.0.1:9641`).
  nginx exposes it on `sfu.lakko.me/metrics` behind client-certificate TLS
  (`infra/nginx/sites-available/sfu.conf`, `ssl_client_certificate
  /etc/lakko/mtls/monitoring-ca.crt`). Its 22 metric families carry only
  closed-set labels (`direction`, `kind`, `outcome`, `reason`, `why`). No
  other service has a metrics recorder; every HTTP service answers
  `/health`, `/ready` and `/version`.
- **The Prometheus configuration is stale.** `infra/prometheus/prometheus.yml`
  describes a LiveKit SFU and a separate monitoring VPS scraping
  `node_exporter` on `sfu`, `turn`, `auth`, `ds` and `discovery.lakko.me`,
  which is not the single-VPS deployment. `rules/calls.yml` holds six alerts
  whose text still names LiveKit; `rules/node.yml`, which the configuration
  loads, does not exist.
- **No dashboard exists**, no exporter for the host, MariaDB or nginx is
  installed, and the test stack has no monitoring at all.
- **Background work is invisible.** Every service runs pruners and sweeps
  (anonymous-token spent sets, DS knocks and nonces, the ADR-0032
  unclaimed-account sweeps, the ADR-0065 blob sweep, the discovery KT
  reconciler). A sweep that stops prints nothing until its table grows.
  Backups are described in `infra/README.md` ("Backup / restore", a daily
  `mariadb-dump` and a weekly blob rsync to a Storage Box) but no unit or
  timer for them is in `infra/systemd/`, and nothing would notice one not
  running.
- **Logs** go to journald through `tracing`; ADR-0078 section 11 keeps
  identifiers out of every line at info and above, enforced by
  `lakko-log-hygiene` (route-template request spans, a source lint, runtime
  capture tests). No journald retention is configured.
- **Clients carry no telemetry SDK** (no crash reporter, no analytics) on
  web, iOS, Android or desktop.

### Why observability is a privacy question here

The servers see ciphertext and public artefacts. That makes the usual
operator questions (did a message get delivered, did a call connect) hard
to answer from the server side, and it makes the obvious way of answering
them dangerous: a metric labelled by group, room, journal, account or
`push_gid` is a social-graph table with timestamps, retained for weeks on a
machine designed to keep exactly that information out. ADR-0078 already
treats the server's logs as a dataset an operator, a backup or a seized
disk can be asked for; a metrics store is the same kind of dataset, and it
is kept longer and indexed by time.

Aggregates are not free either. At the user counts Lakko has before launch,
a counter of DS appends at 15-second resolution comes close to a record of
one person's activity times. The operator already sees each request live
(class (b)); what a metrics store adds is a retained, queryable copy of the
timing (class (a) for timing). That cost is recorded below rather than
argued away.

## Decision

Lakko gets aggregate, low-cardinality metrics from every service, a
synthetic end-to-end canary as the only "the product works" signal, a
separate monitoring host that stores and shows them, and nothing on the
clients; no metric carries an identifier, enforced by tests the same way
log hygiene is.

### 1. The metrics rule

1. **A label value comes from a closed set fixed in the source.** Allowed
   label kinds: `service`, `route` (the axum `MatchedPath` template, or
   `unmatched`), `method`, `status` (the class `2xx` .. `5xx`), `code` (one
   of the service's own stable error codes, else `other`), and named
   outcomes, reasons, kinds, providers and contexts that are Rust enums or
   `&'static str` constants. A label value is never built from request
   data.
2. **Never a label, never a metric name part:** account id, device id,
   login handle, username, group / journal / mailbox id, room or call id,
   channel id, `push_gid`, subscription id, blob id, peer id, token or nonce
   value, IP address, user agent, country or city, any prefix or hash of
   these. This is the denied list of ADR-0078 section 11, applied to
   metrics.
3. **No per-object gauges.** A gauge counts objects (active SFU rooms, open
   long-poll sockets); it never has one series per object.
4. **Exemplars and trace ids are off.** They attach a request to a sample.
5. **Enforcement.** A shared crate (section 2) builds every label from
   types that only accept the allowed kinds. Each service has a test that
   drives its router, renders `/metrics` and runs
   `lakko_log_hygiene::scan::find_identifiers` over the output, the way the
   discovery and DS capture tests do for logs, and asserts every label
   value belongs to the service's declared set. The `lakko-log-hygiene`
   source lint gains a rule refusing a `counter!` / `gauge!` / `histogram!`
   label whose value is not a literal, a constant or an enum's `as_str()`.

### 2. One shared crate and one listener per service

A new crate `lakko-metrics`, depended on by every service with an HTTP
surface (auth, ds, discovery, storage, gif-proxy, channels, push-gw both
listeners, transparency, witness, the obfs-proxy control plane, the SFU,
which moves onto it from `prom.rs`). The `edge` and `transport-rendezvous`
stubs are left out until they serve anything.

- `install()` sets up the Prometheus recorder once per process.
- `red_layer(service)`, a tower layer recording
  `lakko_http_requests_total{service,route,method,status,code}` and
  `lakko_http_request_duration_seconds{service,route,method}` (a histogram;
  buckets chosen, not measured: 5 ms to 30 s). The DS and SFU long polls
  and WebSockets are excluded from the duration histogram by route, since
  their duration is the design, and counted by an open-connections gauge
  instead.
- `job_heartbeat(job)`: sets `lakko_job_last_success_timestamp_seconds{task}`
  and increments `lakko_job_runs_total{task,outcome}`; every pruner, sweep,
  reconciler and the backup script call it. (Written `{job}` until P3, which
  found that Prometheus renames a scraped `job` label.)
- `serve(bind)`: the `/metrics` listener, on its own localhost port, off
  when `LAKKO_<SVC>_METRICS_BIND` is unset (the SFU's existing convention).
  `/metrics` is never served on a service's public listener.

nginx exposes all of them through **one** client-certificate-only server
block on the main host (`metrics.lakko.me`, a port of its own), one path
per service, with the existing `monitoring-ca.crt`. It also fronts
`node_exporter`, `mysqld_exporter`, nginx `stub_status` and the coturn
exporter, all bound to localhost. `sfu.lakko.me/metrics` and the
`turn.conf` metrics location move there.

### 3. What each service counts

All counters are totals, rated over a window by the dashboard. Nothing
below is labelled by anything outside section 1's allowed kinds.

| Service | Counters and gauges |
|---|---|
| all | RED (section 2); `lakko_build_info{service,version}`; DB pool in use and wait time; job heartbeats |
| auth | signup starts / finishes; logins by kind (password, passkey, recovery, TOTP, device grant) and outcome; refusals by limiter `scope`; `429`s by limiter category; anonymous tokens issued by `context`; key resets; `fresh-auth-required` refusals; session refreshes by outcome |
| ds | appends by queue kind (group, mailbox, knock, rendezvous); commit route answers `201` / `200` / `409`; proof refusals (`401`) by reason; wakes forwarded by outcome; open long-poll and WebSocket connections (gauge); journal rows pruned |
| discovery | OPRF evaluations (single, batch); identity uploads by outcome incl. `identity-change-needs-attestation`; KT append queue length and age of the oldest unappended change; anonymous-token spends by `context` and outcome |
| transparency / witness | log size; age of the newest root; witness co-signatures obtained per scrape and the number of configured witnesses that answered (by position, not by URL); consistency refusals |
| push-gw | wakes in, sealed-wake refusals by reason, nonce duplicates, sends by provider and outcome (APNs, FCM, UnifiedPush, Web Push), provider latency histogram |
| storage | bytes stored (gauge), uploads / downloads / keeps / deletes by outcome, passes issued by kind, blobs removed by the sweep |
| channels | posts, rotations staged / committed, envelope deliveries, publisher-proof refusals |
| gif-proxy | upstream calls by outcome, upstream latency, cache hit ratio |
| sfu | the existing families, plus active rooms (gauge) |
| obfs-proxy | tunnels accepted by transport kind, relay errors |

Totals that would be nearly per-person at small scale (signups, call
admissions) are kept anyway: they answer "is signup broken", and the
residual is section 8's.

### 4. Host and dependency signals

`node_exporter` (CPU, memory, disk use and I/O, network, the systemd
collector restricted to `lakko*` and `coturn` units), `mysqld_exporter`
(connections, slow queries, InnoDB, size per schema), nginx `stub_status`,
the coturn exporter, and `blackbox_exporter` run on the monitoring host:
HTTPS probes of every public `/health` and `/ready`, certificate expiry of
every public name, a UDP STUN probe of coturn, a TCP probe of the obfs
listeners.

### 5. A synthetic canary

A small client, `tools/canary` (`lakko-canary`), runs on the monitoring host
and every few minutes does what a person does, through the public
endpoints and the Rust core crates (`lakko-auth-client`, `lakko-mls`,
`lakko-ds-proof`, `lakko-anon-token`, `lakko-attachments`):

1. sign in two dedicated canary accounts (created once, credentials only on
   the monitoring host);
2. account A sends a message in their one-to-one; B receives and decrypts
   it; time from send to decrypted;
3. a short attachment upload and download;
4. A joins a call room, B joins, SFrame-sealed audio frames reach B.

It exports `lakko_canary_step_success{step}` and
`lakko_canary_step_seconds{step}`. It is the only signal that says a
message was delivered and opened, because no server can see that. The
canary accounts are ordinary accounts with no contacts outside each other;
they are visible to the servers as any account is.

### 6. Logs

journald stays the only log store: persistent, `SystemMaxUse=1G`,
`MaxRetentionSec=7day` (chosen, not measured). No log shipping, no Loki in
this ADR: one host does not need it, and a second copy of the logs is a
second dataset. The log rules of ADR-0078 section 11 are unchanged.

### 7. Where it is stored and shown

- **A separate small VPS** runs Prometheus, Alertmanager, Grafana,
  `blackbox_exporter` and the canary. It scrapes the main host and the test
  stack over the mTLS metrics endpoint, with a `stack` label
  (`prod`, `test`). It outlives the main host going down, which is when it
  is needed. VictoriaMetrics single-node may replace Prometheus if memory
  is short; the scrape and alert rules stay the same.
- **Retention:** 30 days for every series (chosen, not measured). The disk
  is encrypted, the host holds no other Lakko data, and only the maintainer
  can reach it.
- **Grafana is not public.** It binds to localhost on the monitoring host
  and is reached over an SSH port forward. No hosted metrics service
  (Grafana Cloud and similar): it would hand a third party the activity
  timeline.
- **Dashboards as code** in `infra/grafana/` (provisioning JSON), versioned
  with the rest:
  1. *Overview*: every unit up, error ratio and p95 per service, disk,
     certificates, canary steps.
  2. *Messaging*: DS appends and commit answers, wakes to push sends by
     provider, canary delivery time.
  3. *Calls*: SFU families, rooms and peers, coturn.
  4. *Identity and abuse*: signups, logins, limiter refusals, token
     issuance against spends, KT and witnesses.
  5. *Storage and database*: blob bytes, sweep, schema sizes, slow queries,
     age of the last backup.
  6. *Background jobs*: every heartbeat's age.
- **Alerts** go by Alertmanager to a Telegram bot and to e-mail, never
  through Lakko (an outage of Lakko would silence them). Alert text carries
  service names and numbers only. The first set: a unit down; `/ready`
  failing; 5xx ratio above a threshold; a canary step failing twice in a
  row; certificate expiry within 14 days; disk above 85%; a job heartbeat
  older than three of its periods; backup older than 26 hours; KT log root
  older than its expected interval; fewer witnesses answering than the
  clients' threshold K (clients then fail closed); push provider errors
  above a threshold. Thresholds start as guesses and are tuned on the test
  stack.

### 8. The residual this records

The monitoring host holds, for 30 days, aggregate counts per route and per
outcome at 15-second resolution. While users are few, such counts reveal
when someone was active and what kind of operation it was, though never
who or with whom. Whoever holds the monitoring host (the operator, its
backup, a seizure) learns that timing; the main host's operator already
sees it live. This is a class (a) timing residual in ADR-0078's terms; it
shrinks as users grow and it is accepted for operating the service. The
storage byte counters are sharper than request counts: at low traffic one
upload's ciphertext size can be read from a single increment.

Backups (P2) are a second at-rest copy of what the main host holds, not
derived from metrics but recorded here because this ADR adds them: dumps of
every `lakko_*` database, age-encrypted, kept `LAKKO_BACKUP_KEEP_DAYS` days
(default 7) on the main host and on a Storage Box, and a weekly mirror of
the client-encrypted blobs. A deleted account stays in those dumps until
they age out, and a deleted blob stays in the mirror until the next weekly
run. The age private key must be kept offline; on the main host or the
Storage Box it would undo the encryption. Restoring a dump rolls back state
that must only move forward (key generations, device floors, the spent-token
set, backup-head versions, journal epochs); `infra/README.md` ("Backup /
restore") treats a restore as an incident and rotates the session and
anonymous-token secrets before the services start again.

### 9. Clients

No client gets a telemetry or crash-reporting SDK. Crash and hang rates
come from the platforms' own aggregate reports (App Store Connect, Play
Console), which need no code in the app. A person who hits a problem can
send a diagnostic report by hand (the web call log already exists); that
flow needs its own UX spec and is not designed here.

### 10. Phasing

| Phase | Work |
|---|---|
| P1 | `lakko-metrics` crate; RED layer and `/metrics` listener in every service; the label test per service and the lint rule; the SFU moved onto the crate; `prometheus.yml` and the rules rewritten for the single-VPS deployment |
| P2 | Section 3 counters; job heartbeats in every pruner and sweep; the backup as a systemd timer that writes a heartbeat |
| P3 | The monitoring host; exporters; the nginx metrics server block; journald retention; dashboards and alerts in `infra/grafana/` and `infra/prometheus/` |
| P4 | The same for the test stack, under `stack="test"` |
| P5 | The canary |

## P1 as built (2026-11-08)

- **`crates/lakko-metrics`** (new): `install(service, version)`, idempotent
  per process (a test binary installs once and shares it), with the
  duration buckets of section 2 and `lakko_build_info`; `red_layer(service)`
  with `without_duration_for(&[routes])`; `job_succeeded` / `job_failed`
  (no caller yet, P2); `start_from_env(service, version, var)` and
  `serve(metrics, bind)`; `exposition::check` / `assert_clean`. No new
  third-party crate: `metrics`, `metrics-exporter-prometheus`, axum and
  tower were already in the graph through the SFU and the services.
- **Wired into every service with a router:** auth, ds, discovery, storage,
  push-gw (both listeners), channels (public and internal), transparency,
  witness, gif-proxy, the obfs-proxy control plane, the SFU. Listener
  variables `LAKKO_<SVC>_METRICS_BIND`, off when unset, suggested ports
  9651 to 9660 in `infra/etc-lakko/lakko.env.example`. The SFU keeps its
  own `LAKKO_SFU_METRICS_BIND` (default `127.0.0.1:9641`, as before);
  `services/sfu/src/prom.rs` now only names and describes the SFU's
  metrics, the recorder and listener are the crate's.
- **Differences from sections 1 and 2.** The `code` label (a service's own
  error code) is not built: it needs the code from the response body or a
  response extension each handler sets; it moves to P2. Instead of an
  open-connections gauge per long-lived route there is one
  `lakko_http_requests_in_flight{service}`, which counts long polls for as
  long as they wait; the DS long poll stays in the duration histogram
  (its long buckets show the wait), and only the SFU's WebSocket upgrade
  is excluded from it. A metrics listener bound to a non-loopback address
  starts with a warning rather than being refused, because the Docker e2e
  stacks bind `0.0.0.0`. Two `main` functions (push-gw, transparency)
  carry `#[allow(clippy::too_many_lines)]` for the added lines, as the
  SFU's already did.
- **Enforcement (section 1.5).** `lint_metric_labels` in
  `lakko-log-hygiene` and two tests over the tree in
  `tests/source_lint.rs` (no metric label names or builds an identifier;
  every service mounts the layer and a listener). The per-service
  exposition test exists for the SFU (`tests/metrics_exposition.rs`, no
  database) and as an added scan in the DS and discovery capture tests
  (database-gated, run against the local MariaDB on 2026-11-08). Other
  services have no router test that runs without their state, so only the
  shared layer test and the source tests cover them; their domain
  counters (P2) bring their own.
- **Prometheus configuration** rewritten for the single main VPS plus the
  coturn VPS, scraping `metrics.lakko.me:9643/<target>/metrics`, a server
  block P3 adds (until then only `sfu.lakko.me/metrics` exists); rules
  `services.yml`, `calls.yml` (rewritten on the `lakko_sfu_*` names; the
  LiveKit rules are gone) and `host.yml` (the missing `node.yml`).
  `promtool check config --syntax-only` and `check rules` (Prometheus
  2.53.0) pass. transparency, witness and obfs-proxy are not scraped:
  they have no systemd unit.
- **Tests run:** `lakko-metrics` 8, `lakko-log-hygiene` (lint unit tests and
  the four tree tests), and the full suites of all 13 touched packages.
  Two SFU `synthetic_client_e2e` timing tests fail intermittently under the
  parallel run; one of them also fails on the base commit (1 of 3 runs) and
  the other passes alone, so neither is caused by this change.

## P2 as built (2026-11-08)

**Shared.** `lakko-metrics` gained the `code` label (`ErrorCode`, a
response extension the RED layer reads), `job_registered` (a job's start
counts as a success, so one that never succeeds goes stale), the pool gauge
`lakko_db_pool_connections{state}` (`spawn_pool_gauge`, sampled every 15 s;
the time a request waits for a connection is not sampled, sqlx exposes no
such figure), a re-export of the `metrics` facade (no service adds a
dependency), one bucket set for every histogram (otherwise they render as
summaries), `record_locally` for tests, and `context` among the base label
names. `lakko-anon-token-server`'s gate counts every spend as
`lakko_anon_token_spends_total{context,outcome}` and sets the code on its
refusals; its two pruners and `lakko-pow-server`'s pruner report heartbeats.

**Per service** (each in a `src/metrics.rs` with a test that renders every
series and runs the exposition check):

| Service | Built |
|---|---|
| auth | `signups_total{step,outcome}`, `logins_total{kind,outcome}` (password, passkey, recovery, totp, device_grant; outcome from the status class of the handler's answer), `key_resets_total`, `session_refreshes_total{outcome}`, `limit_refusals_total{scope,outcome}`, `address_limit_refusals_total{category}`, `anon_tokens_issued_total{context}`, `anon_token_quota_exhausted_total{context}`, `fresh_auth_refusals_total`; codes on every JSON error; 8 heartbeats; pool gauge |
| ds | `appends_total{queue,outcome}`, `commits_total{outcome}` (appended, resent, conflict), `proof_refusals_total{reason}` (the answer stays a uniform 401), `wakes_total{outcome}`, `long_polls_open{queue}`, `pruned_rows_total{kind}`; codes on its two coded errors; heartbeat `ds_pruner`; pool gauge. No WebSocket gauge: the DS has no WebSocket route |
| discovery | `oprf_requests_total{kind}` and `oprf_evaluations_total{kind}` (single, batch), `identity_uploads_total{outcome}` (each change-rule refusal its own value), `kt_appends_total{kind,outcome}`, `kt_append_pending`, `kt_append_oldest_pending_age_seconds`; codes on every JSON refusal; 8 heartbeats; pool gauge |
| transparency | `appends_total{outcome}`, `log_size`, `cosigned_log_size`, `cosigned_timestamp_seconds`, `witnesses_configured`, `witnesses_answered`, `witness_cosigns_total{outcome}` (ok, not_consistent, refused, unreachable, bad_response); pool gauge. Two gauges instead of a per-position label |
| witness | `cosigns_total{outcome}`, `log_size`, `cosigned_timestamp_seconds`; pool gauge |
| push-gw | `wakes_total{outcome}` (incl. replayed and nonce_set_full, now told apart), `deliveries_total{provider,outcome}`, `provider_send_seconds{provider}`; pool gauge |
| storage | `uploads_total`, `downloads_total`, `keeps_total`, `deletes_total` (each `{outcome}`), `passes_issued_total{kind}`, `sweep_removed_total`, `bytes_added_total`, `bytes_removed_total`; codes on blob-route refusals; heartbeat `blob_sweep`; pool gauge when MariaDB backs the index |
| channels | `posts_total{outcome}`, `envelope_deliveries_total{outcome}`, `rotations_total{step,outcome}`, `proof_refusals_total{kind,reason}`; codes on every JSON error; heartbeat `channels_retention_prune`; pool gauge when MariaDB is configured |
| gif-proxy | `upstream_calls_total{kind,outcome}`, `upstream_duration_seconds{kind}`, `rendition_cache_lookups_total{outcome}`; codes on every JSON error |
| obfs-proxy | `tunnels_total{transport,outcome}`, `tunnels_active{transport}`, `relays_total{outcome}` |
| sfu | `rooms_active`; `calls_total`, which was declared and never incremented, now counts a room's first socket |

All names carry the `lakko_<service>_` prefix. **Differences from section 3:**
storage has no stored-bytes gauge. The only way to fill it was a COUNT/SUM over
the whole blob index at boot and after every sweep, which does not scale. Byte
counters give the growth rate, and node_exporter's filesystem metrics give the
absolute size. Where a service's errors are plain text (push-gw, transparency,
witness, most of the DS), there is no `code` label.

**Clients online.** The server has no notion of a person online (invariant #5),
but `sum(lakko_ds_long_polls_open{queue="mailbox"})` counts clients, read from
the client code on 2026-11-19 (not yet observed on a deployed stack). Each
signed-in client runs one loop that long-polls the account mailbox and re-arms
as soon as the poll returns; the DS counts a poll only while it waits. The
loops:
- web and desktop: `contacts/MailboxContext.tsx`, 25 s, one per open tab or
  window. It parks only when the tab is hidden and push wakes are reachable, so
  a hidden tab without push still counts;
- iOS: the session root in `LakkoApp.swift`, 30 s, for the whole session; it
  stops when iOS suspends the app;
- Android: `MainActivity.kt` under `repeatOnLifecycle(STARTED)`, 30 s, while
  the app is on screen.

The other mailbox reads (an older key's mailbox after a rotation, the
co-publisher Welcome) send `wait=0` and are never counted. The count runs low
for the moment between a return and the next poll (a poll that found rows
returns at once). It runs high for up to one wait (30 s) after a client went
away without closing its request. A person with three devices counts three.
Several DS instances are summed.

**Backup.** `infra/scripts/backup.sh` (installed as
`/usr/local/bin/lakko-backup`) and the units `lakko-backup-db.{service,timer}`
(daily) and `lakko-backup-blobs.{service,timer}` (weekly):
- The dumps are compressed with zstd and encrypted with age to a public key
  before they leave the host. The script refuses to run without the key unless
  `LAKKO_BACKUP_ALLOW_PLAINTEXT=1`; this refusal was not in the ADR and is my
  addition.
- The last 7 days are kept locally and mirrored to the target with
  `rsync --delete`.
- The heartbeat goes through node_exporter's textfile collector.
- The alerts are `LakkoBackupStale` (26 h for dumps, 8 d for blobs) and
  `LakkoBackupNeverRan`.
- Exercised locally against stand-in `mariadb` and `mariadb-dump` and local
  directories: the refusal, an encrypted dump that decrypts back, a failed run
  that leaves no heartbeat and no partial file, and the blob mirror. It has
  never run against MariaDB or a Storage Box.

**Alert rules** added for push provider failures, missing KT witnesses,
co-sign lag, the KT append backlog and DS proof refusals. They have not been
checked with promtool: Docker was unavailable when they were written.

**Database-gated tests.** They did not run on 2026-11-08 (Docker Desktop's
engine stopped answering). They ran on 2026-11-20 with P3, against a fresh
MariaDB 11 container, every `LAKKO_*_TEST_DATABASE_URL` set: all pass for
the 15 touched packages (auth 211). On a fresh database the auth and
transparency suites fail in a first parallel run because many tests run the
migrations at once (`migrations: Dirty(n)`, a duplicate key on the
migrations table); after one test has migrated the database they pass.
transparency's store tests each clear the one `log_entries` table, so four
of them fail when run in parallel and all 38 pass with `--test-threads=1`;
neither is caused by this ADR.

**Pre-existing test failures.**
- `discovery tests::locate_logs_neither_the_address_nor_the_answer` fails in
  most parallel runs, including on the base commit (2 of 3 runs). It is a
  race in tracing's callsite interest cache between parallel tests.
  Rebuilding the cache in the test did not fix it.

**Found and left.**
- discovery's KT reconciler scans `identity_bundles` in full every 300 s, and
  the new pending count does the same when the queue is not empty. Neither
  is index-backed.
- call-invite ledger rows have no pruner.

## P3 as built (2026-11-20)

Configuration only: nothing has been deployed, no monitoring VPS exists yet,
and no target has been scraped.

**A defect in P1/P2, fixed.** The heartbeat label was `job`. Prometheus
attaches its own `job` label (the scrape job) to every series it stores, and
a scraped label of the same name is renamed to `exported_job`. Every rule
that read `job="backup_db"` or `job!~"backup_.*"` would therefore never have
matched, and the stale-job and backup alerts would have stayed silent. The
label is now `task`: in `lakko-metrics` (`job.rs`, `BASE_LABELS`), in the
per-service tests, in `backup.sh`, in the rules. The same clash existed for
`service` (a target label and a metric label); the `lakko_services` scrape
job now sets `honor_labels: true`, so the metric's own `service` (same
value) is kept. A promtool unit test (`infra/prometheus/tests/jobs_test.yml`)
pins the rules against stored series carrying both labels.

**Main VPS** (`infra/scripts/setup-metrics.sh`, `just setup-metrics`):
- the metrics server block `infra/nginx/sites-available/metrics.conf`,
  `metrics.lakko.me:9643`, one path per target, `ssl_verify_client on` for
  the whole block, and a loopback `stub_status` server; the SFU's
  `/metrics` location on `sfu.lakko.me` is removed;
- node_exporter (textfile collector on
  `/var/lib/node_exporter/textfile_collector`, the systemd collector limited
  to Lakko, coturn, MariaDB and nginx units), mysqld_exporter,
  nginx-prometheus-exporter, all on 127.0.0.1;
- journald retention (`infra/journald/lakko.conf`), also installed on the
  coturn VPS and the monitoring VPS;
- the backup timers of P2 (enabled when `LAKKO_BACKUP_TARGET` is set) and a
  new hourly `lakko-schema-size` timer;
- the `LAKKO_<SVC>_METRICS_BIND` lines in `lakko.env`, added when missing;
- 9643/tcp opened to the monitoring VPS's address only.

**Schema sizes, differently from section 4.** mysqld_exporter's
`info_schema.tables` collector sees only the tables its account holds a
privilege on, and the grant that makes them visible reads the data too. The
exporter account gets `PROCESS` and `REPLICATION CLIENT` only. A root timer
(`infra/scripts/schema-size.sh`) writes
`lakko_mariadb_schema_bytes{schema}` from the size of each `lakko_*`
directory instead.

**coturn VPS** (`deploy-coturn.sh`, `turn.conf`): node_exporter listened on
0.0.0.0:9100 with the port open in the firewall and no authentication; it
now listens on loopback, is served as `/node/metrics` behind the coturn
vhost, and the 9100 rule is deleted. A second defect: `turn.conf` (and the
SFU's removed `/metrics` location) put `ssl_verify_client` inside a
`location`, where nginx refuses it, so `nginx -t` on that vhost could never
have passed. It is at server level now. With `MONITORING_IP` set
(`LAKKO_MONITORING_IP` through `just deploy-coturn`), port 9642 admits the
monitoring VPS only, as 9643 does on the main VPS (added after the security
review of 2026-11-20).

**Monitoring VPS** (`infra/monitoring/setup-monitoring.sh`,
`just deploy-monitoring`), written for Ubuntu 24.04:
- Prometheus (30 days), Alertmanager, blackbox_exporter, node_exporter and
  Grafana, all on 127.0.0.1; the firewall admits ssh only; Grafana through
  `just grafana` (an SSH port forward), from Grafana's own apt repository;
- the monitoring CA and Prometheus's client certificate are made on the
  first run; `just push-monitoring-ca` copies the CA certificate to the two
  Lakko hosts; the CA key stays on the monitoring VPS;
- Alertmanager rendered from `alertmanager.yml.tmpl` and
  `/etc/lakko-monitoring/secrets.env`: critical alerts to Telegram and
  e-mail, warnings to Telegram, two inhibit rules;
- blackbox: the public `/health` and `/ready` probes, and a TLS handshake
  to coturn on 443. blackbox_exporter has no STUN prober, so the STUN probe
  of section 4 is not built; nor is the TCP probe of the obfs listeners,
  which are not deployed;
- new scrape jobs for Alertmanager and the monitoring host's own
  node_exporter; new rules `LakkoBlackboxDown` and the monitoring host in
  `LakkoExporterDown`; `LakkoJobStale` uses three periods (three hours, three
  days for the daily `blob_sweep`) instead of one window for all.

**Dashboards** (`infra/grafana/dashboards/`, provisioned from
`infra/grafana/provisioning/`): the six of section 7, uids `lakko-overview`,
`lakko-messaging`, `lakko-calls`, `lakko-identity`, `lakko-storage`,
`lakko-jobs`. Overview carries "clients online"
(`sum(lakko_ds_long_polls_open{queue="mailbox"})`). Calls charts coturn
through `turn_total_allocations`, `turn_total_traffic_*` and
`turn_auth_credential_failures`, names read from coturn's master source;
the coturn version on the TURN VPS may predate some of them. The per-realm
`turn_traffic_*` counters are not used: with `prometheus_username_labels`
on they carry the TURN username.

**Checked on 2026-11-20, in containers:** `just monitoring-check`
(promtool check config and rules on Prometheus 2.53.2, the rule unit test,
amtool on the rendered template with Alertmanager 0.26.0, the blackbox
config with 0.24.0, the dashboards as JSON); `nginx -t` (nginx 1.24) on the
metrics, coturn and SFU vhosts; the metrics block's client-certificate gate
live (no certificate 400, another CA's certificate 400, the monitoring
certificate reaches the target); shellcheck at warning level on the five
scripts; `schema-size.sh` against a stand-in data directory; all six
dashboards provisioned into Grafana 11.3.0; all 123 dashboard queries
parsed by Prometheus 2.53.2; the Rust tests of the 15 packages touched by
the `task` rename, database-gated ones included (see "P2 as built"),
`cargo fmt --check`, clippy on the changed crates. Not checked: anything on Ubuntu itself (its
package versions and `/etc/default` files are as documented, not run), a
real scrape, a real alert delivered.

## P4 as built (2026-11-20)

Configuration only, as P3: nothing installed on the test host, nothing
scraped.

- `infra/scripts/setup-metrics.sh` takes `STACK=test`: the env file
  `/etc/lakko-test/lakko.env`, the server name `test.lakko.me` (the test
  stack's existing certificate, so no new DNS name), no backup timers (the
  test data is disposable). Everything else is the production path: the
  same exporters on loopback, journald retention, the schema-size timer, the
  metrics server block on 9643 (installed as `lakko-metrics.conf` on both
  hosts), 9643 open to the monitoring VPS only.
- `just setup-metrics-test` copies `infra/` from the working tree to the test
  host, which has no checkout, and runs it; `just push-monitoring-ca-test`
  copies the CA certificate there.
- `prometheus.yml`: every service the test stack runs (auth, ds, discovery,
  storage, push-gw, channels, gif-proxy, sfu, obfs-proxy), its three host
  exporters, its public probes and a TLS probe of `edge.lakko.me:443`, all
  under `stack="test"`. Its two coturn instances (4.5.2) are not configured
  with coturn's exporter and their configuration is not changed here.
- Alertmanager routes every `stack="test"` alert to Telegram only, with a
  12-hour repeat; production critical alerts keep e-mail.
- The storage dashboard's schema-size panel no longer filters
  `host="main"`, so it shows the test host under `stack="test"`; the other
  dashboards already select by stack.

**Checked on 2026-11-20:** `just monitoring-check` with the test targets;
`amtool config routes test` (a test critical alert goes to `telegram`, a
production critical alert to `telegram_and_email`, a production warning to
`telegram`); shellcheck on the changed script. Not checked: the script on
the test host (Ubuntu 22.04, nginx 1.18 there), a scrape.

## P5 as built (2026-11-20)

`tools/canary` (package `lakko-canary-tool`, binary `lakko-canary`), a
headless client over the Rust core crates; no new protocol and no server
change.

- **`lakko-canary init`** creates the two accounts of a stack once: signup
  with the proof of work, a password and a recovery credential, a BIP39
  phrase as each account identity seed (ADR-0017), the login handle bound at
  once (an account with neither a handle nor a passkey is deleted after a
  day), a device identity bound to each account (ADR-0050). Account A founds
  one `OneToOne` group with B's KeyPackage; the founding commit is not
  posted, B joins from the Welcome in memory, A claims the DS journal with
  its root statement (ADR-0057). Everything goes into one state file (mode
  0600, written atomically), including the service bases; init refuses an
  existing file. `--sfu` takes a base with a path prefix (the test stack's
  `wss://test.lakko.me/sfu`).
- **`lakko-canary run`** loops (`--interval-secs`, default 600) over four
  steps, in this order:
  1. `sign_in`: an OPAQUE login by handle for both accounts. Session tokens
     stay in memory and are never written. Auth allows 10 logins per handle
     in 900 s, so the interval must stay above about 90 s.
  2. `message`: A seals and publishes an application message with a DS
     proof; B long-polls after its cursor, decrypts every row in order and
     must find it within 60 s. The seconds are from A's publish to B's
     decrypt. A's MLS state is saved after sealing and before publishing,
     so a crash never reuses a ratchet generation. B's state and cursor are
     saved together after each batch. B must keep up: OpenMLS refuses to
     decrypt a message more than 1000 generations ahead.
  3. `attachment`: A encrypts about 1 KiB, uploads it with a one-day idle
     life and a delete key derived as the web client derives it; B
     downloads and decrypts it and compares; A deletes it. Storage passes
     are kept in the state and reused until 120 s before they expire, so
     the step spends about 24 upload and 24 read tokens a day, not one per
     run (the quotas are 96 a day each).
  4. `call`: every `--call-every` runs (default 3). Each run uses a new call
     id, because the SFU refuses a room that stood empty. Both accounts
     spend a `call-join` token. B publishes a receive-only audio offer, A a
     send-only one; B answers the SFU's renegotiation. A sends up to 50
     random 60-byte frames, each sealed in the ADR-0071 layout under the
     MLS-derived call seed; the step succeeds when 10 arrive at B and open.
     It is skipped (not failed) while either account is younger than 25
     hours, because a young account gets 8 `call-join` tokens a day.
     Default cadence: 48 calls a day against a quota of 100.
- **Metrics** (`LAKKO_CANARY_METRICS_BIND`, through `lakko-metrics`), labels
  `step` and `outcome` only:
  - `lakko_canary_step_success{step}` and `lakko_canary_step_seconds{step}`;
  - `lakko_canary_step_consecutive_failures{step}`;
  - `lakko_canary_step_runs_total{step,outcome}` (`ok`, `failed`,
    `skipped`);
  - `lakko_canary_last_run_timestamp_seconds`.

  The exposition is checked with `lakko_metrics::exposition::assert_clean`.
  A failed step logs one WARN line with the step and an error class.
- **On the monitoring VPS** (`infra/monitoring/setup-monitoring.sh` step 7):
  a `lakko-canary` system user and the unit
  `infra/monitoring/lakko-canary@.service`, one instance per stack (prod
  on 127.0.0.1:9661, test on :9662). The state is
  `/var/lib/lakko-canary/<stack>/state.json`, the settings
  `/etc/lakko-monitoring/canary-<stack>.env`. `just deploy-canary` builds
  on the main VPS from `main` and copies the binary; `just canary-init
  <stack>` runs init and starts the instance.
- **Prometheus and alerts:** scrape job `lakko_canary`, with rules in
  `infra/prometheus/rules/canary.yml` and promtool tests in
  `infra/prometheus/tests/canary_test.yml`:
  - `LakkoCanaryStepFailing`, critical: two failures in a row, the section
    7 rule;
  - `LakkoCanaryStale`, warning: no finished run for 30 minutes;
  - `LakkoCanaryDown`, warning;
  - `LakkoCanaryDeliverySlow`, warning: a message takes over 20 s, a
    guess.

  The Overview dashboard has the step outcomes and durations; Messaging has
  the delivery time.

Verified:
- Against a local stack (auth, ds, storage and the SFU from this tree on
  loopback, separate databases), init and two `--once` runs passed all four
  steps. The second run continued the ratchet and reused the passes, and no
  blob was left behind.
- Loop mode exported the metrics above.
- With the DS stopped, the message step failed, counted one consecutive
  failure and recovered after the restart; the other steps still passed.
- Without the test override, the call step was skipped for a young account
  and spent no token.
- An ignored test (`wss_tls_handshake_reaches_the_sfu`) passed the TLS
  handshake with the live test-stack SFU, which then refused the bogus token
  with 401.
- 18 unit tests pass, plus that network test; clippy and fmt are clean.

The canary is a test client run by the operator on the operator's host,
not a person's device: its account seeds sit in its state file, not in
hardware-backed storage, and invariant #7 does not apply to it. Its accounts
never publish an identity bundle or mint a genesis attestation, so they are
"an account that never minted genesis" (ADR-0074 residuals). Plain `http`
and `ws` bases are accepted only for a loopback host, so a mistyped init
cannot send sessions, tokens, passes or call JWTs in the clear.

Not verified:
- A call between the monitoring VPS and a deployed SFU. The canary's str0m
  client offers only a host candidate and has no TURN, so the call depends
  on the SFU's public candidate and peer-reflexive discovery.
- The `epoch-not-issuable` retry and the pass re-purchase on a 401 or 429:
  the code is there, but neither has been triggered.
- Nothing runs on a server yet.

## Alternatives

- **Logs only, no metrics.** Rates and percentiles would be computed by
  reading journald. Rejected: logs at the needed detail are larger and
  closer to identifying than counters, and nothing alerts on them without
  a log pipeline.
- **OpenTelemetry tracing with a collector.** Rejected for now: a trace
  ties a request's spans together across services, which is exactly the
  linkage ADR-0078 removes, and it needs a collector and a store for one
  host. Metrics answer the questions an operator has today.
- **A hosted service (Grafana Cloud, Datadog and similar).** Rejected: a
  third party would hold the activity timeline of section 8.
- **Monitoring on the main host.** Rejected: it fails with the host it is
  meant to watch, and it puts the timeline beside the data it could be
  correlated with.
- **Per-group or per-room metrics for debugging.** Rejected outright; it is
  the social graph with timestamps (invariant #5).
- **Client telemetry, opt-in.** Rejected for this ADR: even an opt-in
  stream from the app is a server-side record keyed by device, and the
  platform aggregates already show crash rates.

## Consequences

- **Positive:** outages, stalled background jobs, certificate expiry, a
  failing provider and broken delivery are seen before a person reports
  them; the stale LiveKit-era configuration goes; the label rule makes a
  privacy regression in metrics a failing test rather than a review
  finding.
- **Negative:** a second host to run and secure; a new crate in every
  service; the timing residual of section 8; the canary accounts are real
  accounts the servers count; a small CPU cost per request for the RED
  layer (not measured).
- **Neutral:** journald gains a retention it did not have; the SFU's
  metrics move from its own vhost to the shared metrics server block.

## Open questions

1. Whether activity counters should be stored at a coarser resolution than
   error counters (for example 5 minutes), which would narrow section 8;
   neither Prometheus nor VictoriaMetrics single-node keeps per-series
   retention, so this needs recording rules and dropped raw series.
2. A public status page (up / degraded per area) from the blackbox and
   canary results.
3. The manual diagnostic report on the clients (section 9) needs a UX spec.

## Documentation this ADR implies, to apply when accepted

- `docs/security/invariant-index.md`: a new C entry, "a metric label never
  carries an identifier", with the enforcement of section 1.5. Applied:
  C-94.
- `infra/README.md`: the monitoring host, the metrics server block, the
  canary accounts, journald retention, the backup timer.
- `CLAUDE.md` module map: the `lakko-metrics` crate and `tools/canary`.

## References

- [ADR-0078](0078-servers-cannot-link-accounts.md) sections 11 and 12
- [ADR-0011](0011-rust-sfu.md) (the SFU metrics this generalises)
- `services/sfu/src/prom.rs`, `crates/lakko-log-hygiene`
- Prometheus exposition format; the RED method (rate, errors, duration)
