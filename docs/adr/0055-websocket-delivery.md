<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0055: WebSocket where a push beats a poll, HTTPS everywhere else

- **Status:** Proposed (2026-09-28)
- **Date:** 2026-09-28
- **Context:** Phase 3 (product hardening)
- **Related:** [ADR-0053](0053-ds-journal-authorisation.md) (DS proofs),
  [ADR-0054](0054-anti-abuse-without-client-work.md) (wake through the DS,
  anonymous tokens), [ADR-0024](0024-typing-read-receipts.md) (typing rows on
  the journal), [ADR-0026](0026-anti-censorship-wiring.md) (WebSocket over the
  obfs tunnel), [ADR-0011](0011-rust-sfu.md) (SFU signaling is already a
  WebSocket)

## Context

Clients talk to the backend over HTTPS with JSON bodies, plus one WebSocket:
SFU call signaling. New journal rows reach a foreground client by long
polling the Delivery Service: the web chat route long-polls the open chat with
a 25 s wait, Android long-polls with a 30 s wait, and a device mailbox is
long-polled for Welcomes. Typing indicators and read receipts are journal rows
(ADR-0024), which that ADR itself records as an anti-pattern: an ephemeral
signal persisted for 30 days.

The maintainer asked (2026-09-28) for a comparison of REST, WebSocket and gRPC
with development cost ignored. The conclusion recorded here:

- **gRPC** brings nothing the other two do not, and costs a gRPC-Web
  translating proxy with one-direction streaming in the browser, and HTTP/2
  inside the ADR-0026 tunnels whose client speaks HTTP/1.1.
- **HTTPS request/response** fits every call that is a request with an answer:
  per-request authorisation (the ADR-0053 proof, the ADR-0054 token), status
  codes and `Retry-After`, stateless servers, clear retries, cacheable GETs,
  and the mobile background model (a push wakes the app, one short request).
- **WebSocket** wins exactly where the server has something to say without
  being asked, while the app is in the foreground.

## Decision

**A foreground client holds one WebSocket to the Delivery Service for delivery
and ephemeral signals. Every request with an answer stays HTTPS. Background
delivery stays on APNs, FCM and Web Push.**

### 1. What moves to the WebSocket

1. **Delivery notices.** The client subscribes to the journals it is reading
   (the open chat, the chat list's groups, its device mailbox). The DS pushes
   `{journal, head}` when a row lands; the client then fetches the rows it
   lacks over the existing signed HTTPS GET. Rows themselves are not pushed:
   the GET keeps its proof, paging and cursor rules, and a lost notice costs
   latency, never a message.
2. **Ephemeral signals.** Typing, and later presence, ride the socket as
   MLS-sealed payloads relayed to the other subscribers of that journal and
   never stored. The journal copy of typing (ADR-0024) goes away with it. Read
   receipts stay on the journal: they are state a device that was away must
   catch up on.

### 2. What stays HTTPS

Auth, discovery, token issuance and spends, storage, push-gw, and publishing
to a journal. Publishing stays a POST because its proof binds method, path and
body, it is retried idempotently, and it carries the ADR-0054 wake header.

### 3. Subscription authorisation

A subscription to a claimed journal carries the same ADR-0053 proof a GET
would (a signed canonical request for `SUBSCRIBE <journal>`), verified at
subscribe time, re-verified when the journal's epoch key rotates (the server
drops subscriptions whose key left the two-epoch write tier), and refused with
the same uniform error. A mailbox subscription carries the mailbox proof. No
account identifier, session token or anonymous token rides the socket.

### 4. Failure and fallback

The socket is an accelerator. When it cannot open or drops (captive portals,
proxies that refuse the upgrade, a tunnel without the WebSocket path), the
client falls back to the long poll it has today and retries the socket with
backoff. The fallback is a transport choice, not a security one: both paths
read the same signed GET.

### 5. Liveness

The server pings every subscriber at the WebSocket protocol layer (the SFU's
5 s ping and 15 s idle cut are the precedent) and nginx's `proxy_read_timeout`
on the route is set above the idle cut. On iOS and Android the socket closes
when the app leaves the foreground; nothing tries to keep it alive in the
background.

## Capacity of one instance, current configuration

Measured and read on 2026-09-28. The only running deployment is the test
stack, so "current configuration" means that host.

| Item | Value | Source |
|---|---|---|
| CPU / RAM | 2 vCPU, 3810 MiB total, about 2.8 GiB available | `nproc`, `free -m` on the host |
| nginx | `worker_processes auto` (2), `worker_connections 768`, soft open files 1024, no `worker_rlimit_nofile` | `/etc/nginx/nginx.conf`, `/proc/<nginx>/limits` |
| DS process | soft open files 1024, hard 524288 | `/proc/<ds>/limits` |
| System | `fs.file-max` 51200, `nf_conntrack_max` 65536, ephemeral ports 10000-65000 | `sysctl` |
| Memory per idle socket | about 137 KiB | measured in Linux (Docker) with the workspace's axum 0.8.9 and the SFU's socket shape at 1000, 5000 and 10000 sockets; flat across sizes |

Each proxied WebSocket costs two nginx connections (client and upstream), one
DS socket, and about three file descriptors host-wide.

Ceiling today, in the order they bind:

1. **nginx: about 768 sockets** (2 workers x 768 connections / 2 per socket),
   shared with all other traffic.
2. **DS open files: about 1000** (soft limit 1024, less the database pool and
   listeners).
3. **Memory: about 20 000** at 137 KiB in 2.8 GiB, less whatever MariaDB and
   the other services take as they grow.
4. **Host file descriptors: about 17 000** (51200 / 3).
5. conntrack about 32 000 (two tracked flows per socket), ephemeral ports to
   one upstream 55 000.

So as configured the host holds **roughly 700 concurrent WebSockets**. Raising
`worker_connections` with `worker_rlimit_nofile`, and setting `LimitNOFILE` so
the services get a high soft limit, moves the ceiling to memory and file
descriptors, **about 17 000**. Lowering the per-socket read buffer
(`WebSocketUpgrade::read_buffer_size`; the default is what the 137 KiB is made
of) and raising `fs.file-max` would move it further, towards conntrack; that
per-socket figure is **not measured** and must be before anyone relies on it.
Measured sockets were idle after one round trip; active sockets add write
buffers, and CPU for pings and fan-out at these counts was not measured.

## Alternatives

- **gRPC for everything.** Rejected: see Context.
- **WebSocket for everything, including requests with answers.** Rejected:
  per-request proofs and tokens become per-frame protocol of our own, retries
  and idempotency move into application code, every service becomes stateful,
  and the mobile background model still needs the HTTPS path.
- **Server-Sent Events for delivery.** Viable for notices (server to client
  only, plain HTTP, reconnect built in) and simpler behind proxies, but it
  cannot carry typing from the client, and the SFU already has the WebSocket
  machinery. Kept as the fallback candidate if the upgrade proves unreliable
  through proxies in practice.
- **Push the rows over the socket.** Rejected: duplicates the GET's proof,
  paging and cursor rules on a second path, and a dropped frame would need its
  own recovery.

## Consequences

- **Positive:** new rows appear without waiting out a poll; typing stops
  filling the journal; fewer idle long-poll requests.
- **Negative:** the DS becomes stateful (sockets, subscription table), a
  second instance needs a fan-out between instances (not needed on one host);
  the host limits above must be raised before load, and nginx needs an upgrade
  location for the DS.
- **Privacy (invariant #5), stated:** one socket per device carries all its
  subscriptions, so the DS sees which journals one connection reads. That is
  what today's long polls over one reused HTTP/2 connection already show, so
  it adds no knowledge; it does not reduce it either. Subscriptions carry no
  account identifier.
- **Invariants:** #2 unchanged (notices are journal ids and heads; signals are
  MLS-sealed); #3 untouched; #4 unchanged (the socket carries no plaintext
  and no key).

## Open questions

1. Per-socket read buffer size and the memory it gives, measured.
2. Whether the chat list subscribes to every group or only to recent ones.
3. SSE instead of WebSocket for notices if proxies refuse upgrades in the
   field.
4. Presence (online / away) on the same socket: product question first.

## Plan

- **S1** host limits (nginx `worker_connections` + `worker_rlimit_nofile`,
  systemd `LimitNOFILE`, `fs.file-max`) and the read-buffer measurement.
- **S2** DS subscribe endpoint with proof verification, notices, epoch-driven
  re-verification, ping and idle cut; nginx upgrade location.
- **S3** clients: socket when foregrounded, long poll as fallback, typing moved
  off the journal.
- **S4** docs and a `lakko-security` pass.
