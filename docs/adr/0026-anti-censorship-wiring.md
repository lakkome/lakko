<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0026: Anti-censorship wiring — end-to-end architecture

- **Status:** Accepted
- **Date:** 2026-06-19
- **Context:** Phase 5 — anti-censorship operational wiring

## Context

`lakko-transport` ships censorship-resistant transports (REALITY, MASQUE,
Shadowsocks-2022, WebTransport) with working client `connect()` + server
listeners, a fail-closed `HappyEyeballs` selector, the signed `BootstrapConfig`
+ 4-stage `verify` + `DohFetcher`, and now (ADR-0025) the `BootstrapDialer` that
composes them into one fail-closed dial. **None of it is on any wire.** Three
firsthand facts define the remaining gap:

1. **The transports give a framed tunnel, not raw HTTP.** `Connection` is
   `async send(frame: Bytes)` / `recv() -> Bytes` with a `MAX_FRAME_BYTES` cap —
   a length-prefixed message channel layered over the transport's underlying
   TCP/QUIC byte stream.
2. **The Lakko services speak HTTP** (auth, discovery, ds, …) behind nginx; the
   clients dial them over their platform HTTP stacks (web `fetch`, iOS
   `URLSession`, Android Ktor/OkHttp). There is an impedance mismatch between the
   obfuscated framed tunnel and the HTTP services.
3. **Nothing terminates the obfs transports server-side.** The
   `RealityListener` / `ShadowsocksListener` / MASQUE + WebTransport server
   drivers exist but no service runs them; no component bridges an accepted
   obfs tunnel to the internal HTTP services.

So "doing anti-censorship completely" is a **system**, not a wiring tweak: a
server-side obfs-terminating proxy + a client transport→HTTP layer + FFI + the
bootstrap-config fetch + a deployment. It also runs into a hard platform limit.

### Hard constraint: the browser sandbox

A web page **cannot** open a raw TCP/UDP socket, so REALITY (raw TLS/TCP),
Shadowsocks (raw TCP), and MASQUE (QUIC) are **impossible from
web-in-browser**. Anti-censorship transport is therefore a **native feature**
(iOS, Android, and the desktop Tauri wrapper, whose Rust backend can open
sockets). Browser web stays on direct HTTPS (a censored network simply blocks
it — the browser build is not the censorship-resistant client). This is a
property of the platform, not a choice.

## Decision

**Carry the clients' plaintext HTTP over an encrypted obfuscated tunnel to a
new server-side obfs-terminating proxy that byte-relays to the internal HTTP
services; own the HTTP-over-tunnel path in Rust and expose it via FFI so the
native clients route their Lakko API calls through it when armed.** The obfs
transports provide the transport-layer confidentiality; the MLS payloads inside
stay E2EE regardless (invariant #2/#4 unaffected — the application layer never
relaxes).

### Components

> **Names + scope corrected 2026-09-02 (documentation audit).** Three items in
> this list drifted from what shipped; each is marked at its component.

1. **Server: `services/obfs-proxy`** (new `lakko-svc-obfs-proxy`).
   Runs the obfs server listeners (`RealityListener`, `ShadowsocksListener`,
   ~~MASQUE server,~~ WebTransport server) on dedicated public ports. On each
   accepted tunnel it obtains a framed `Box<dyn Connection>`, reassembles the
   byte stream, opens a TCP connection to the **internal** Lakko HTTP entry
   (nginx on localhost, or a service port), and relays bytes in both directions
   until either side closes. It is a **dumb byte-relay** — it never parses the
   inner HTTP, never sees plaintext application data (MLS-E2EE), and holds no
   account state (invariant #5). One confinement knob: the relay target is a
   fixed configured internal address, so a tunnel cannot be used to reach
   arbitrary hosts (no open relay).

   > **MASQUE is deliberately NOT one of the proxy's listeners** — and this is a
   > finding, not a backlog item. `services/obfs-proxy`'s config carries exactly
   > three listeners (`reality`, `shadowsocks`, `webtransport` —
   > [`config.rs`](../../services/obfs-proxy/src/config.rs)). A
   > `MasqueListener` **does exist** in `lakko-transport` and its handshake is
   > verified, but MASQUE `CONNECT-UDP` carries **unreliable, unordered QUIC
   > datagrams**, so relaying a *reliable* internal HTTP service over it does
   > not work: the response datagram is dropped on close, because QUIC datagrams
   > are never retransmitted nor flushed ahead of `CONNECTION_CLOSE`. The
   > MASQUE loopback e2e is `#[ignore]`d and documents this. MASQUE is the right
   > tool for tunnelling UDP, not TCP/HTTP; making it work here would mean
   > adding a reliability layer this proxy should not have. The other three
   > reliable-stream transports cover the use case.

2. **Bridge: `ByteStreamConnection`** (in `lakko-transport`).
   An adapter that presents the framed `Connection` as an `AsyncRead + AsyncWrite`
   byte stream (frame payloads concatenate; writes chunk into frames bounded by
   `MAX_FRAME_BYTES`). This is the seam that lets a byte-oriented HTTP client run
   over the message-oriented tunnel, on both the client and the proxy side.

3. **Client: `TunneledHttpClient`** (in `lakko-transport`).
   A minimal HTTP/1.1 request encoder + response parser that runs over a
   `ByteStreamConnection` obtained from `BootstrapDialer::dial`. The inner HTTP
   is plaintext (the obfs tunnel is the encryption); the request targets the
   same REST paths the platform clients already use. Fail-closed: if the dial
   fails, the request fails — there is no direct-HTTP fallback inside this path
   (the caller decides whether anti-censorship is required; see Consequences).

4. **FFI: ~~`tunneled_request`~~ `tunneled_http_request`** (UniFFI, in `lakko-ffi-uniffi`).
   `tunneled_http_request(config, anchor, now, endpoint, method, path, host, headers, body) -> FfiTunneledResponse`
   ([`transport.rs`](../../crates/lakko-ffi-uniffi/src/transport.rs)) so
   Swift/Kotlin route a Lakko API call through the dialer + tunnel without
   re-implementing any transport logic in the platform language. **The WS
   variant this called "later" landed too** — `open_tunneled_websocket` →
   `TunneledWsConnection` (`send_text` / `send_binary` / `recv` / `shutdown`,
   [`ws.rs`](../../crates/lakko-ffi-uniffi/src/ws.rs)) over
   `lakko_transport::ws_tunnel::tunneled_websocket`; both native
   `SfuSignalingClient`s use it. A third entry point the component list did not
   anticipate is `fetch_bootstrap_config`, which sources the config itself
   (component 6). No wasm surface — browser web is excluded by the sandbox
   constraint.

5. **Native client wiring.** When anti-censorship is **armed** (a user setting +
   a fetched, verified bootstrap config), iOS and Android route their Lakko REST
   (and later WS) calls through `tunneled_request` instead of `URLSession` /
   Ktor. When disarmed, they use the direct path as today. The arming gate is an
   explicit user/operator choice — not a silent automatic downgrade either way.

6. **Bootstrap-config fetch.** At app boot (when armed) the client runs
   `DohFetcher::fetch_bootstrap_config(domain, pinned_trust_anchor, now)` →
   `BootstrapDialer::from_signed_config`. The trust anchor is build-time pinned
   (the existing pinning pattern). A fetch/verify failure leaves
   anti-censorship unavailable (loud), never a silent direct fallback.

### Inner-HTTP confidentiality

The tunnel carries **plaintext** HTTP because the obfs transport already
encrypts + authenticates the channel (REALITY = TLS 1.3, Shadowsocks-2022 =
AEAD, MASQUE = QUIC/TLS), and the application payloads are MLS-E2EE end to end.
Double-wrapping the inner HTTP in TLS-to-nginx is avoidable overhead; the proxy
relays to the internal plaintext HTTP entry. (A deployment that prefers
TLS-in-tunnel can point the relay at nginx:443 and the client at an https URL —
the byte-relay is agnostic.)

## Phasing

- **Phase 1 — server obfs-proxy** (`services/obfs-proxy`): the listeners +
  byte-relay-to-internal-HTTP + a confinement target + tests. Headless-testable.
- **Phase 2 — client bridge + HTTP** (`lakko-transport`): `ByteStreamConnection`
  + `TunneledHttpClient` + a **loopback e2e** (client dials a local obfs listener
  → in-process proxy relay → a local HTTP echo, proving the full path). Fully
  headless-verifiable here.
- **Phase 3 — FFI** (`lakko-ffi-uniffi`): ~~`tunneled_request`~~
  `tunneled_http_request` + regen-bindings.
- **Phase 4 — native wiring**: iOS + Android route armed Lakko API calls through
  the FFI; the arming setting + bootstrap-config fetch at boot.
- **Phase 5 — deploy + live validation** (`infra/` + `ci/censorship-lab`): the
  obfs-proxy systemd unit + public ports + config publishing; a **live
  censored-network test** proving an end-to-end request survives a blocking
  middlebox. **Deferred — needs a deployed environment; cannot be verified in
  this repo.**

> **Phase status, re-verified 2026-09-02.** Phases 1–4 landed, including the
> WS-over-tunnel work this ADR originally scoped as a follow-up. **Phase 5 is
> half-open, and the two halves are worth separating** rather than carrying one
> "deferred":
>
> - **`ci/censorship-lab` exists** — the SNI observer, its JA3/JA4 fingerprint
>   parser, the blocklist detector, the throttle model, `docker-compose.yml`,
>   and `.github/workflows/censorship-lab.yml` driving the `#[ignore]`-gated
>   transport e2e tests. Note the workflow's automatic triggers are disabled for
>   CI-quota reasons, so it is manual-dispatch only.
> - **The `infra/` half has not landed.** There is **no obfs-proxy systemd
>   unit** (`infra/systemd/` carries coturn, sfu, and the six services — no
>   obfs-proxy), no nginx vhost, no `just` target, and no public-port opening.
>   So the proxy is built and loopback-proven but **has never been deployed**,
>   which means no user has ever routed traffic through it. Arming
>   anti-censorship on a device today points at a proxy that is not running.
>
> That distinction matters for reading the rest of this ADR: everything from
> the client side inward is real and tested; the operational half is not
> started.

Phases 1–3 + the Phase-2 loopback e2e are buildable and verifiable headless now.
Phase 4 builds + compiles here but cannot be run end-to-end without a deployed
proxy; Phase 5 is a deploy + live-network task flagged explicitly.

## Alternatives

1. **Route the platform HTTP stack (URLSession / OkHttp / fetch) through the
   tunnel via a custom URLProtocol / SocketFactory / Service-Worker.** Rejected
   as the primary path: the platform TLS layers are opaque and injecting a Rust
   `Box<dyn Connection>` beneath them is brittle and per-platform-bespoke. Owning
   the HTTP-over-tunnel in Rust + a thin FFI call is simpler, uniform across
   native platforms, and keeps the security-relevant code in the audited crate.
2. **A SOCKS5 proxy on-device that the platform HTTP points at.** Rejected: more
   moving parts (a local listener + the platform proxy config) for no gain over
   the direct FFI request; harder to make fail-closed.
3. **Tunnel a structured Lakko RPC instead of raw HTTP.** Rejected: it would
   re-implement every REST endpoint's request/response shape; relaying opaque
   HTTP bytes reuses the existing service contract verbatim.
4. **TLS-in-tunnel (client → nginx:443 over the tunnel).** Not chosen as the
   default (double encryption is wasted work when the obfs layer already
   authenticates + encrypts and the payloads are E2EE), but the byte-relay is
   agnostic, so a deployment may opt into it.
5. **Make web-in-browser a first-class anti-censorship client.** Rejected:
   impossible — the browser sandbox forbids raw sockets. The desktop Tauri build
   is the web codebase's censorship-resistant form.

## Consequences

- **Positive:** the built-but-dead transport stack finally reaches the wire on
  the platforms that can use it; the proxy stays a dumb, stateless,
  confined byte-relay (server opacity #2, no social graph #5); fail-closed
  arming (#4 — no silent downgrade in either direction); the HTTP-over-tunnel
  lives in the audited Rust crate, not three platform languages.
- **Negative (honest):** a new server component to run + deploy + monitor; the
  native clients gain a second API path (armed vs direct) that must stay in sync
  with the direct one; **web-in-browser gets no censorship resistance** (a hard
  platform limit, not a gap to close later); the end-to-end guarantee cannot be
  validated without a deployed proxy + a live censored-network test (Phase 5).
- **Neutral:** the inner HTTP is plaintext-inside-an-encrypted-tunnel; metadata
  privacy comes from the obfs layer, application confidentiality from MLS — the
  two are independent.

## Open questions

- **WebSocket-over-tunnel** (the DS long-poll + the SFU signaling are WS): Phase
  4+ needs a WS-over-`ByteStreamConnection` path beside the request/response HTTP
  one. Scoped as a follow-up within Phase 4.
- **Concurrent happy-eyeballs** (ADR-0025 open item) — the race is still
  sequential.
- **Bootstrap-config distribution** — who publishes the signed config, rotation
  cadence, and the DoH domain are an operational decision folded into Phase 5.

## References

- ADR-0025 (bootstrap transport dialer) — the `BootstrapDialer` this builds on.
- ADR-0004 (anti-censorship) — the transport-stack strategy this operationalises.
- `crates/lakko-transport/src/{transport,pluggable,selector,reality,shadowsocks,
  masque,webtransport_pt}.rs` — the transports + listeners + selector.
- `docs/security/transport.md` — T-1..T-15.
- Security invariants #2 (server opacity), #4 (fail-closed / no silent
  downgrade), #5 (social graph not materialised).

## Amendment (2026-10-30, found building the desktop host, ADR-0085 section 14)

- `dial(endpoint, ...)` no longer chooses where a tunnel goes. REALITY and
  Shadowsocks-2022 had ignored the signed `remote` and dialled the endpoint,
  which the clients pass as the API host: the blocked address. `BootstrapDialer`
  pins each to its `remote`; a spec whose `remote` is not `host:port` is skipped.
- `Connection::recv` must survive being dropped: the relay and
  `ByteStreamConnection` `select!` it against outbound data. Shadowsocks closed
  for good on that and WebTransport could lose bytes; both are fixed.
- A request or socket write is cut into `aead_stream::MAX_CHUNK` frames, and a
  chunked response is decoded.
- `tools/bootstrap-config` signs the config.
