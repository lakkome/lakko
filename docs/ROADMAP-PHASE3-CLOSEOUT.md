<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Phase 3 closeout — gap inventory + sprint plan

Living document. Source of truth for what is left to take
Phase 3 from "code + tests green in CI" to "official close
under the audit-pack standard". Tracks Phase 4 / Phase 5 /
audit gaps in the same table so that one document covers
"what is left in Lakko before public beta".

The strategic plan lives at
`~/.claude/plans/1-nested-wreath.md`; the per-phase
tactical breakdown is [`ROADMAP.md`](./ROADMAP.md). This
file zooms in on the **post-Phase-3** punch list and the
revised sprint shapes that follow from
[ADR-0011](./adr/0011-rust-sfu.md) (Rust SFU instead of
LiveKit fork).

## Reading guide

- Rows tagged **G3-\*** belong to Phase 3 closeout (calls).
- Rows tagged **G4-\*** belong to Phase 4 (channels).
- Rows tagged **G5-\*** belong to Phase 5 (anti-censorship).
- Rows tagged **GA-\*** belong to the audit pack / process.
- "Chat?" = can a Claude agent close this gap from the
  terminal without physical hardware? `✅` = yes; `⚠` =
  partial (code in chat, validation needs hardware /
  upstream / external party); `❌` = no.
- "Status" — `done` = fully closed; `partial` = some
  sub-sprints landed, see notes; `open` = not yet started.

## Summary table

| # | Gap | Group | Blocks | Chat? | Estimate | Status |
|---|---|---|---|---|---|---|
| **G3-1** | WebRTC pod iOS — `WebRTC.framework` + peer connection | Phase 3 closeout | 1×1 + group calls | ✅ + acceptance | 2 sprints | **partial** — `stasel/WebRTC` SPM dep pinned at `from: 125.0.0`; `apps/ios/Lakko/Calls/WebRTCPeerConnection.swift` wraps `RTCPeerConnectionFactory` (process-wide shared) + `RTCPeerConnection` with typed `Delegate` callbacks for local-candidate + connection-state, async `createOffer` / `createAnswer` / `applyRemoteAnswer` / `addRemoteCandidate`, mic-toggle, audio-track add. `apps/ios/Lakko/Calls/CallSession.swift` now drives a real RTCPeerConnection through dial / acceptCall / applyIncomingSignaling; local ICE candidates surface via `FfiCallSignalingBody.iceCandidate(...)`. iOS-app `xcodebuild -sdk iphonesimulator` succeeds. `NSMicrophoneUsageDescription` added to project.yml. **Deferred follow-ups**: G3-1-video (RTCCameraVideoCapturer + SwiftUI preview surface), G3-1-turn (rendezvous-bootstrap TURN-server config), G3-1-sframe (RTCFrameTransformer wired to MLS exporter — the call surface is currently SRTP-only, *not* E2EE-complete) |
| **G3-2** | WebRTC pod Android — `webrtc-sdk:android` + peer connection | Phase 3 closeout | 1×1 + group calls | ✅ + acceptance | 2 sprints | **partial** — `io.github.webrtc-sdk:android` pinned at `125.6422.07` in `libs.versions.toml`; `apps/android/app/src/main/kotlin/me/lakko/app/calls/WebRtcPeerConnection.kt` wraps `PeerConnectionFactory` (process-wide singleton via double-checked locking) + `PeerConnection` with typed `Delegate` data class for `onLocalCandidate` + `onConnectionState`, suspend `createOffer` / `createAnswer` / `applyRemoteAnswer` via `suspendCancellableCoroutine` over libwebrtc's `SdpObserver`, `addRemoteCandidate` sync, mic-toggle + audio-track add. `CallSession.kt` now drives a real PeerConnection through dial / acceptCall / applyIncomingSignaling; local ICE candidates surface via `FfiCallSignalingBody.IceCandidate(...)`. `ChatRepository` exposes `context` so `ChatViewModel.makeSession` can plumb the application context through to libwebrtc's native-library loader. `./gradlew :app:assembleDebug` succeeds. `RECORD_AUDIO` + `MODIFY_AUDIO_SETTINGS` permissions + `<uses-feature android.hardware.microphone required=false>` added to AndroidManifest.xml. **Deferred follow-ups**: G3-2-video (`Camera2Enumerator` + Compose preview), G3-2-turn (rendezvous-bootstrap TURN-server config), G3-2-sframe (libwebrtc `FrameDecryptor` / `FrameEncryptor` wired to MLS exporter — call surface currently SRTP-only, *not* E2EE-complete) |
| **G3-3** | Rust SFU on `str0m` (replaces LiveKit fork — see [ADR-0011](./adr/0011-rust-sfu.md)) | Phase 3 closeout | **group** calls only (1×1 needs no SFU) | ✅ | 7.5 sprints | **partial** — skeleton + spike + routing-signaling + tests + routing-media steps 1–5 (str0m =0.19.0 pinned, `services/sfu/src/media.rs` carries `MediaPeer` + `MediaRouter` + `MediaTaskHandle` + `spawn_media_task` with real UDP socket binding + tokio `select!` event loop draining `Rtc::poll_output` → UDP sends + dead-peer prune; `Config::media_bind` parses `LAKKO_SFU_MEDIA_BIND`, `AppState::new_with_media` + `build_app_with_media` wire the handle through `Router::with_media`, `main.rs` spawns the task when configured. Step 5: `drain_outputs` returns a `DrainSummary { earliest_deadline, pending: Vec<PendingFanout> }`, capturing each `Output::Event(Event::MediaData(_))` tagged with its origin peer id; `fanout_media_data(router, &pending)` walks every non-origin peer and calls `MediaPeer::forward_media_data` (a thin wrapper over `Rtc::writer(mid).match_params(params).write(pt, network_time, time, data.clone())`) that returns a typed `MediaForward { Sent / NoWriter / NoCodecMatch / WriteFailed }` outcome — SFrame ciphertext stays opaque (we forward bytes without inspection). 17 media-layer unit tests (incl. 4 new fanout tests: `forward_to_peer_without_writer_returns_no_writer`, `fanout_with_single_peer_is_noop`, `fanout_skips_origin_peer`, `fanout_empty_pending_is_noop`). **Deferred follow-up sub-sprint** — `TracksIn`/`TracksOut` mapping that triggers SDP renegotiation when a new peer joins so destination peers actually have a writer for the source `mid` (today the writer is found only when both sides happen to share a mid by accident, which is the case for echo-back tests but not for real multi-peer fanout); plus the full synthetic str0m-client UDP+DTLS round-trip integration test that proves end-to-end forwarding under real ICE. The structural pieces — capture, fanout dispatch, typed-error skip semantics — all land here |
| **G3-4** | End-to-end CI gate (iOS / Android ↔ Dockerised SFU) | Phase 3 closeout | acceptance Phase 3 | ✅ | 1 sprint | **partial** — backend half shipped: `services/sfu/Dockerfile` (multi-stage rust:1.84-bookworm → debian:bookworm-slim runtime with tini PID-1, non-root uid 10001, libssl3 + ca-certs only); `.dockerignore` keeps the build context lean; `ci/sfu-e2e/docker-compose.yml` brings up the SFU + nginx-test-jwks sidecar with healthcheck gating; fixture JWKS at `ci/sfu-e2e/test-jwks/jwks.json` generated from the fixed seed `[0x42; 32]` via `cargo run -p lakko-call-jwt --example gen_test_jwks` (the example landed in this sprint too); `services/sfu/tests/e2e_dockerized.rs` ships two `#[ignore]`-gated tokio tests (`live_sfu_welcome_round_trip` + `live_sfu_bad_jwt_returns_401`) that point at `$LAKKO_SFU_E2E_URL` and exercise the real container over real TCP; `.github/workflows/sfu-e2e.yml` wires it all on `workflow_dispatch` + `sfu-e2e` PR label + nightly 03:30 UTC cron, captures container logs on failure. **Deferred follow-ups**: the iOS / Android matrix half — wiring xcodebuild-test + Gradle connectedAndroidTest against the same compose stack (needs simulator / emulator runners) lands as G3-4-mobile |
| **G3-5** | Settings modal — audio / video / output device + toggles | UI Phase 3 | UX completeness | ✅ | 1 sprint (3 platforms parallel) | **done** |
| **G3-6** | In-call chat panel — MLS application messages in call group | UI Phase 3 | UX completeness | ✅ | 1 sprint | **done** |
| **G3-7** | Sound cues — resources + player on 3 platforms | UI Phase 3 | UX completeness | ✅ | 0.5 sprint | **done** |
| **G3-8** | Acceptance on hardware (mic, camera, BT, CallKit / ConnectionService) | Phase 3 closeout | official Phase 3 close | ❌ maintainer + devices | 1 sprint | open |
| **G3-9** | Cost analysis (3b6) — real measurements after a month in production | Phase 3 closeout | scale-out trigger calibration | ❌ needs deploy | out of scope of chat | open |
| **G4-1** | `services/channels` backend (Rust) | Phase 4a | publishing over the wire | ✅ | 5 sprints | **done** — in-memory MVP + SQL persistence (G4-1-sql) + production deployment config (G4-1-deploy) + posts retention (G4-1-retention) + stale-subscriber retention (G4-1-subretention). `ChannelStore` is now a trait with two impls: `InMemoryChannelStore` (test default) + `SqlChannelStore` (MariaDB). `LAKKO_CHANNELS_DATABASE_URL` toggles between them at boot; migrations under `services/channels/migrations/000{1,2}_*.sql` carry four ciphertext-only tables — `channels` / `channel_posts` / `channel_subscribers` (with `last_active_at` column) / `channel_envelopes` (FK ON DELETE CASCADE). `put` + `rotate` run under explicit `BEGIN`/`COMMIT` so a concurrent reader cannot observe a torn rotation. Production deploy: `infra/systemd/lakko-svc-channels.service` on 127.0.0.1:8086 (PartOf lakko.target, Requires mariadb), `lakko_channels` database row in `infra/etc-lakko/lakko.env.example`, nginx vhost upstream + `/api/channels/` + `/channels/` location blocks with 20 MiB body cap. Retention: background prune loop ticks both posts (default 30-day TTL) + subscribers (default 90-day TTL, FK cascade drops envelope) on the same cadence (default 1 h). Activity signal: every successful `GET .../envelope` bumps `last_active_at` — no separate heartbeat protocol. Toggles: `LAKKO_CHANNELS_RETENTION_SECS` / `LAKKO_CHANNELS_SUBSCRIBER_RETENTION_SECS` / `LAKKO_CHANNELS_PRUNE_INTERVAL_SECS` (any `=0` disables that step). Cursor monotonicity preserved across post prune so paginating subscribers never see cursor reuse |
| **G4-2** | Client integration of channels on 3 platforms | Phase 4a | UX channel publishing | ✅ | 4 sprints | **partial** — web + iOS + Android publisher + subscriber UI all shipped, plus auto-fanout polling on the publisher side (≤10 s latency without a manual click), plus MLS-derived channel seed FFI surface (G4-mls-seed: `channelSeedFromMlsExporterOutput` + `channelExporterLabel()` pinned to `"lakko/channel/v1"`; the demo path is the grep-clean `newChannelSeedForTest` from G4-2-crypto). Pinned in audit-pack invariant C-44. Remaining: client-side wiring of an actual MLS PublisherGroup (depends on a small `Group.createSolo` UniFFI surface on top of `lakko-mls`, mechanical) + xcodebuild / gradle acceptance on hardware |
| **G4-3** | Subscriber path — HPKE seed envelope + subscribe handshake | Phase 4b | real subscribers | ✅ | 5 sprints | **done** — crypto in `lakko-channels::subscribe` (G4-3a) + backend endpoints (G4-3b) + UniFFI / wasm-bindgen FFI surface (G4-3c) + subscriber UI on web (Subscribers panel + `/channels/:id/subscribe` route with 3-second poll) + subscriber UI on iOS (SubscribersPanelView + ChannelSubscribeScreen with `Task.sleep` poll) + subscriber UI on Android (Compose SubscribersPanel + ChannelSubscribeScreen with `delay(3000)` poll Job). All three client platforms reach the same Phase-4b end-to-end |
| **G4-4** | Rotation + fanout service | Phase 4c | scale-out 100k+ | ✅ | 2 sprints | **done** — atomic batch fanout + epoch tracking; per-receiver background fanout worker is a follow-up sub-sprint |
| **G4-5** | Channel discovery (`@channelname` via OPRF) | Phase 4b | UX "find a channel" | ✅ | 1 sprint | **done** — new `channel_names` table + `/discovery/channel/{register,lookup}` endpoints reuse the existing VOPRF Ristretto255-SHA512 infrastructure |
| **G-discovery-ratelimit** | Per-IP token-bucket on discovery enumeration endpoints | Phase 1.X | defense in depth | ✅ | 1 sprint | **done** — `services/discovery/src/rate_limit.rs` with `TokenBucket` + axum middleware. Applied to `/discovery/oprf`, `/discovery/lookup`, `/discovery/channel/lookup` (PoW remains the primary work proof; rate limiting caps per-IP server CPU). `LAKKO_DISCOVERY_RATE_LIMIT_PER_MINUTE` / `_BURST` env knobs. Idle-bucket pruner runs hourly. 8 unit tests on bucket math + isolation + pruner semantics |
| **G5-1** | REALITY pluggable transport | Phase 5 | RU / CN / IR block bypass | ✅ + lab | 4 sprints | **done — S-5-reality-rng-hook landed; both E2E integration tests green** — `crates/lakko-transport/src/reality/` ships the full REALITY v2 Path C client (driver + stream + transport + cert verifier) AND the full server module tree (handshake mirrors + server_driver + server_stream + listener). Foundation: HKDF + HMAC authenticator, strongly-typed `RealityTagSecret` / `RealityShortId` / `RealityConfig` with eager parse-time validation. TLS wire-format parsers per RFC 8446 §4.1.2 / §4.1.3 / §5.1: `patch_client_hello_session_id` + `extract_server_hello_random` (client-side) + `extract_client_hello_session_id` + `patch_server_hello_random` (server-side mirrors). G5-1-v2 Path C redesign: dropped ECDH, replaced with publisher-distributed pre-shared `tag_secret` per `(client, short_id)`. v2 wire is NOT compatible with upstream xtls/REALITY — Lakko-only ecosystem. **Client side** (S-5-reality-driver-v2): `build_reality_client_config` + `drive_handshake` async byte-intercept loop manually driving `rustls::ClientConnection`, splicing session_id patch on outgoing ClientHello + (optionally) verifying server-response on inbound ServerHello.Random via `RealityConfig::require_server_response`. Post-handshake `RealityConnection<S>` with u32-BE length-prefixed framing inside the TLS tunnel, 16 MiB cap, idempotent close. `RealityTransport::connect` wires it all together — TCP connect → rustls ClientConnection → drive_handshake → RealityConnection, no silent downgrade per security invariant #4. **Server side** (S-5-reality-server): `make_self_signed_cert(cover_sni)` + `build_reality_server_config` + `drive_server_handshake` (verifies inbound ClientHello.session_id against the publisher-distributed allow-list of `(short_id, tag_secret)` pairs, surfaces typed `AcceptedClient { short_id, client_timestamp }` on success). Post-handshake `RealityServerConnection<S>` mirrors `RealityConnection` for the server half. `RealityListener::accept` accept-loop over `tokio::net::TcpListener`. **S-5-reality-rng-hook** (sprint 4): custom `RealityRng` (`SecureRandom` impl) wired as `CryptoProvider::secure_random`; arms a per-thread one-shot inject slot with the REALITY authenticator before `ClientConnection::new` (client) and with `compute_server_response` before `process_new_packets` (server), so the TLS 1.3 transcript hash sees the REALITY bytes from the start. Post-loop ClientFinished drain in `drive_handshake` prevents TLS 1.3 deadlock. Pre-loop `pump_decrypt` in `read_until` defends against TCP coalescing on loopback. 62 unit tests + 2 integration tests (both green: `reality_v2_listener_refuses_active_prober` — active-prober rejection; `reality_v2_loopback_roundtrip` — full client/server handshake + application-data echo). Pinned in audit-pack invariant T-13. **Remaining**: a future `S-5-reality-wire-compat` sprint could re-align byte layout with upstream xtls/REALITY if multi-vendor interop becomes a goal. |
| **G5-2** | MASQUE / CONNECT-UDP | Phase 5 | QUIC under H3 | ✅ + lab | 3 sprints | **done — config + request + datagram codec + h3+quinn handshake** — `crates/lakko-transport/src/masque/` ships four independently-tested foundations: (1) `MasqueConfig` with eager parse (proxy must be `https://`, inner_target `host:port` form incl. bracketed IPv6, bearer non-empty if set), (2) `build_connect_udp_request` emits the RFC 9298 §3 pseudo-headers (`:method=CONNECT`, `:protocol=connect-udp`, `:path=/.well-known/masque/udp/{host}/{port}/` with RFC-3986 percent-encoded host, `capsule-protocol: ?1` per RFC 9297 §3.4, optional `Bearer` token), (3) HTTP Datagram codec per RFC 9297 §3 + RFC 9298 §3 with QUIC varint Context ID (CONNECT-UDP pins ctx=0, others refused with `UnsupportedContextId`), 65 KiB hard cap on encoded payload. **S-5-masque-handshake** (sprint 3): `MasqueTransport::connect` drives the full h3+quinn handshake — resolves the proxy via `std::net::ToSocketAddrs`, builds a rustls ClientConfig (TLS 1.3 + h3 ALPN + system CA roots via `rustls-native-certs`), opens a `quinn::Endpoint` on `[::]:0`, wraps the quinn connection in `h3_quinn::Connection`, then `h3::client::builder().enable_extended_connect(true).enable_datagram(true).build(..)` runs the SETTINGS exchange that negotiates both extensions. Builds the `ConnectUdpRequest` into an `http::Request<()>` carrying `:method = CONNECT` + `Protocol::CONNECT_UDP` extension + capsule-protocol + bearer headers, sends and waits for 2xx (non-2xx → `Error::Io`). Post-handshake, the cloned quinn::Connection lives in `MasqueConnection` which shuttles frames via `quinn::Connection::send_datagram` / `read_datagram` using our codec; the h3 driver + request-stream pump are spawned as tokio tasks aborted on `close()`/`Drop`. h3-datagram intentionally skipped (one Context ID = 0 per tunnel; our codec already produces RFC 9297-compliant wire bytes). 29 unit tests incl. http::Request translation + unreachable-proxy negative path. Pinned in audit-pack invariant T-15. **Remaining**: live MASQUE handshake test against an in-process proxy server (deferred — requires server-side h3+quinn stack with extended-CONNECT) |
| **G5-3** | Shadowsocks-2022 | Phase 5 | alternative obfs | ✅ | 4 sprints | **done** — chunked AEAD primitive (G5-aead-stream / T-9) + SS-2022 outer header layer (G5-3-header / T-10: BLAKE3 subkey derivation, salt + sealed prelude, replay window, response-binds-request-salt, timestamp tolerance) + TCP wiring (S-5-shadowsocks-tcp / T-11: `ShadowsocksTransport::connect` + `accept_inbound`, per-direction subkey separation, frame-size cap at 16 KiB) + `ShadowsocksListener` (G5-3-listener: symmetric `Listener` API around `accept_inbound` with per-listener replay window). 25 unit + integration tests, including sequential-distinct-clients via listener API |
| **G5-4** | WebTransport (web client) | Phase 5 | web client in censored regions | ✅ | 2 sprints | **done — client connect + framing + ECH + E2E server-side test** — `crates/lakko-transport/src/webtransport_pt.rs` real impl over `web-transport-quinn` 0.11 (rides on the same quinn 0.11 + rustls 0.23 stack the rest of the crate uses, crypto-provider-agnostic so we stay on `ring`). Cert-hash pinning (W3C `serverCertificateHashes` mode, SHA-256 of full DER cert) + system-roots fallback. `WebTransportConfig` field renamed `spki_pins_b64` → `cert_hashes_b64` for spec alignment. `Connection::send` / `recv` use u32-BE length-prefixed framing with `MAX_FRAME_BYTES` enforcement on both sides. `WebTransportPt::new` validates URL + cert hashes eagerly. ECH wire-up landed (G5-6-wireup, ADR-0012): `WebTransportConfig::ech_config_list_b64` bypasses `ClientBuilder` when armed and assembles quinn::ClientConfig manually with a Lakko-side `CertHashVerifier`; refuses ECH-without-cert-pins (`EchRequiresCertHashes`). 13 unit tests + 1 E2E integration test (`tests/webtransport_e2e.rs::webtransport_loopback_roundtrip` — G5-4-server-test): spins a `web_transport_quinn::Server` on `127.0.0.1:0` with rcgen-generated self-signed cert, `WebTransportPt` client connects with the cert's SHA-256 hash pinned, sends + recv-echo a frame end-to-end. URL pins `127.0.0.1` (not `localhost`) so the IPv4 server bind matches the client's destination — `localhost` would resolve to `::1` first on macOS while quinn binds `[::]:0`. Pinned in audit-pack invariant T-14 |
| **G5-5** | Transport rendezvous (signed bootstrap config) | Phase 5 | PT endpoint discovery | ✅ | 2 sprints | **done** — `BootstrapConfig` + Ed25519 sign / verify / expiry / clock-skew + JSON wire format in `crates/lakko-transport/src/rendezvous.rs`. Client-side `DohFetcher` (G5-5-doh) resolves bootstrap domains over four hardcoded JSON-DoH endpoints (Cloudflare / Google / Quad9 / AdGuard), then IP-pinned HTTPS GET + the existing 4-stage verify. Pinned in audit-pack invariant T-8 |
| **G5-6** | ECH (Encrypted Client Hello) | Phase 5 | SNI privacy | ✅ | 6 sprints | **done — parser + RustCrypto/hpke adapter + HTTP/WebTransport wire-up** — `crates/lakko-transport/src/ech/` ships the parser (G5-6) plus G5-6-wireup steps 1-5: `adapter.rs` exposes `LakkoHpkeSuiteAesGcm128 / AesGcm256 / ChaCha20Poly1305: rustls::crypto::hpke::Hpke` on top of `hpke 0.13` (RustCrypto, MIT/Apache), bundled as `ALL_SUPPORTED_SUITES` for `rustls::client::EchConfig::new`. `wireup.rs::build_ech_mode_from_bytes` runs a two-stage validation (our T-12 parser + rustls's own parser) before returning `EchMode::Enable(..)`. **HTTP transport** — `HttpTransportConfig::ech_config_list: Option<Vec<u8>>`; `with_config` routes through `ClientConfig::builder_with_provider(..).with_ech(mode)?`. **WebTransport** — `WebTransportConfig::ech_config_list_b64: Option<String>` (serde-default for backwards compat); when armed, `open_session` bypasses `web-transport-quinn::ClientBuilder` (which does not expose `with_ech`) and assembles the rustls + quinn::ClientConfig manually with a Lakko-side `CertHashVerifier` replicating the W3C `serverCertificateHashes` semantics. Refuses ECH-without-cert-pins at parse time per `EchRequiresCertHashes` (security invariant #4 — under ECH the outer cert is the cover-site's; system CA root fallback would let a compromised CA fronting the cover SNI MITM the transport). **REALITY is intentionally not wired**: REALITY and ECH are mutually exclusive (both want the outer ClientHello SNI / `legacy_session_id` slot for SNI hiding); REALITY already provides SNI privacy via its own mechanism — caller picks one transport. **rand_core caveat**: `rand_core` 0.9 split `OsRng` to `TryCryptoRng` only; `hpke` 0.13 still takes the infallible bound, wrapped with `UnwrapErr<OsRng>` (panics if the kernel CSPRNG fails — TLS itself would no longer be safe in that case). **rustls internal-API caveat**: rustls 0.23 surfaces `HpkeKem / HpkeKdf / HpkeAead / HpkeSymmetricCipherSuite` only via its `#[doc(hidden)] pub mod internal` path (the documented consumer surface for external crypto-provider adapters but explicitly excluded from rustls's stable interface; a major rustls bump may need re-pointing one `use` block in `adapter.rs`). **Tests**: 12 parser + 9 adapter + 4 wire-up + 2 HTTP + 5 WebTransport unit + integration tests, all green. **Fuzz**: `ech_decode` (parser, T-12 §1) + `ech_wireup_build_mode` (full parse → rustls assembly, T-12 §2). Pinned in audit-pack invariant T-12. See [ADR-0012](./adr/0012-ech-hpke-adapter.md). **Remaining**: live integration test against a real ECH-publishing endpoint (Cloudflare) — gated on the censorship-lab sprint (G5-7) |
| **G5-7** | Censorship lab (GFW / TSPU / IR emulation) | Phase 5 | CI gate for PT survival | ✅ | 2 sprints | **partial — first-wave SNI observer landed** — `ci/censorship-lab/sni-observer/` ships a ~370-line dependency-light Rust binary that listens for TLS on 8443, parses the inbound ClientHello (no rustls — raw wire view a passive on-path adversary sees), extracts the `server_name` extension, appends `{ts_ms, observed_sni, peer, first_bytes_hex}` JSON lines to `/var/log/sni-observer/observed.jsonl`. Multi-stage Dockerfile (rust:1.84-bookworm → debian:bookworm-slim runtime with tini + netcat-openbsd for the healthcheck + non-root uid 10001). `ci/censorship-lab/docker-compose.yml` brings it up with host bind-mount at `./logs/` so the test harness reads the file out-of-band. `crates/lakko-transport/tests/sni_observer_e2e.rs` (`#[ignore]`-gated, `tokio-rustls` 0.26 dev-dep + custom `AcceptAnyCert` server verifier) opens a TLS connection with a known SNI, observer logs it, test asserts the matching JSONL line. Verified end-to-end locally: stack comes up healthy in ~10 s, test passes, observed log shows `{"observed_sni":"lakko-censorship-test.example",..}`. `.github/workflows/censorship-lab.yml` ties it to `workflow_dispatch` + `censorship-lab` PR label + nightly 04:00 UTC cron with on-failure artefact upload. **Deferred follow-ups** (matrix grows by appending services + tests, not by forking): G5-7-fingerprint (JA3/JA4 passive observation), G5-7-reality-probe (active probing against REALITY endpoints), G5-7-gfw-string (packet-pattern blocking via iptables string-match), G5-7-throttle (IR-style speed caps), G5-7-transport-matrix (per-transport leak matrix — REALITY cover SNI ✓, Shadowsocks no-TLS ✓, HTTP transport leaks real SNI ✗) |
| **GA-0** | [ADR-0011](./adr/0011-rust-sfu.md) — supersedes ADR-0003 / ADR-0008 | Audit / Design | rationale for G3-3 rewrite | ✅ | landed | **done** |
| **GA-1** | G-23 libcrux — waits for openmls upgrade | Audit | upstream CVE | ⚠ depends on upstream | tracker | open (upstream-blocked) |
| **GA-2** | G-26 RFC 9605 wire-header interop | Audit | interop with mlspp | ✅ | 1 sprint | **partial** — serializer + 12 unit tests done; seal/open integration + external-stack interop pending G3-1/G3-2 |
| **GA-3** | External crypto audit (Phase 2 exit gate per plan) | Audit | public-beta launch | ❌ external auditor | out of scope of chat | open |
| **GA-4** | Reproducible builds verification — 2 runners byte-identical | Process | SLSA L3 | ✅ | 1 sprint | **partial** — `.github/workflows/repro.yml` ships same-runner double-build + cross-runner matrix verifier (both advisory `continue-on-error: true`); flip-to-gating waits on stable byte-identicality across the workspace |
| **GA-5** | TUF auto-update infrastructure | Process | secure updates | ✅ | 3 sprints | **partial — verifier crate feature-complete** — `crates/lakko-tuf` ships scaffold (GA-5-scaffold) + root-rotation walk (GA-5-rotation) + raw-JSON bootstrap path (GA-5-bootstrap-test) + target-bytes verifier (GA-5-target-verify) + defense-in-depth delegation rejection (GA-5-no-delegations) + URL conventions module with path-traversal defence (GA-5-urls) + end-to-end install-walk integration test (GA-5-e2e) + fuzz harnesses for the four role envelopes (GA-5-fuzz). Client-side metadata verifier: Root + Snapshot + Targets + Timestamp Ed25519-signed envelopes; `verify_chain` with version-rollback detection + per-role threshold-of-keys + expiry check; `verify_root_chain` walks the root version chain (each new root signed by the previous root's `root` role, anchored at binary-pinned trust anchor); `Signed::<T>::from_json` parses raw JSON bytes the caller pulled over its preferred transport; `verify_target_bytes` closes the last metadata-side gate — given a `TargetMeta` + actual fetched binary, confirms length + SHA-256 digest match; unknown hash algos skipped (forward-compat); refuses if no known algo got verified. 36 unit + 5 integration tests cover the full surface. **Remaining work moves outside the verifier crate**: network fetch glue **shipped** as `DohFetcher::fetch_pinned_bytes` (GA-5-fetch) — generic IP-pinned HTTPS `Vec<u8>` getter with body-size cap that downstream platform install-clients orchestrate together with `lakko_tuf::urls` + `Signed::from_json` + `verify_chain` + `verify_target_bytes`; pinned in audit-pack invariant T-8 §7. Still out-of-scope: delegated targets walk, mirror selection, platform-side install glue (idempotent atomic-swap binary replacement + post-install verification) |
| **GA-6** | Bug bounty programme | Process | ongoing post-launch | ❌ org work | out of scope of chat | open |

## Group A — Phase 3 closeout sprints

### S-3-A-iOS-pod (G3-1)

**Composition.**
- Add `WebRTC.framework` via SPM as a binary target
  (pinned version recorded in `apps/ios/Package.swift`).
- `apps/ios/Lakko/Calls/WebRtcPod.swift` —
  `RTCPeerConnectionFactory` wrapper, ICE / DTLS setup.
- `apps/ios/Lakko/Calls/SframeBridge.swift` —
  `RTCFrameEncryptor` / `RTCFrameDecryptor` calling our
  `crates/lakko-calls::sframe` via UniFFI.
- `apps/ios/Tests/WebRtc/` — peer-pair tests,
  ICE-gathering, codec-preference, SFrame round-trip.
- GitHub Actions workflow `ci/ios-webrtc-test.yml` with a
  macOS runner.

**Exit criteria.**
- [ ] All 9 unit / integration tests green locally and in CI.
- [ ] `RTCFrameEncryptor` pipeline: plaintext frame →
      ciphertext on the wire differs from plaintext;
      decryptor recovers bit-exact.
- [ ] Codec preferences: SDP contains AV1 before VP9
      before H264; Opus carries `useinbandfec=1`.
- [ ] Locally-spawned coturn fixture; ICE gathering
      completes.

**Out of scope.** Real app integration (a real call placed
through the app) — that is `S-3-A-acceptance`.

### S-3-A-Android-pod (G3-2)

Mirror of `S-3-A-iOS-pod`. `io.github.webrtc-sdk:android`,
JUnit + Robolectric, `apps/android/app/src/test/kotlin/me/lakko/app/calls/webrtc/`.
Workflow `ci/android-webrtc-test.yml` with a Linux runner.

**Exit criteria.** Identical to iOS, plus a Robolectric
shadow for `AudioManager` device enumeration.

### S-3-A-settings (G3-5)

3 platforms in parallel; same state shape (`CallSettings`)
across web / iOS / Android.

**Composition.**
- State: `selectedMic / selectedCamera / selectedSpeaker:
  DeviceId`, `availableDevices: list`,
  `noiseSuppression / echoCancellation / autoGain: bool`,
  `mirrorLocal: bool`, `lowDataMode: bool`.
- Device enumeration: `AVCaptureDevice.devices(for: .audio)`
  (iOS), `AudioManager.getDevices()` (Android),
  `navigator.mediaDevices.enumerateDevices()` (web).
- UI: gear button to the left of end-call → bottom-sheet
  (mobile) / modal (web / desktop).
- Persistence: encrypted under DeviceGroup; syncs between
  devices.
- Live apply: device switch during an active call does
  not drop the PC.

**Exit criteria.**
- [ ] UI snapshot tests green on all 3 platforms.
- [ ] Unit test: mid-call device switch keeps the PC alive.
- [ ] Settings persist across relaunch.

### S-3-A-chat (G3-6)

**Composition.**
- State in `CallUiState`: `chatMessages`, `chatOpen`,
  `composeText`, `unreadCount`.
- Transport: MLS application message in the **call's**
  MLS group. NOT `lakko-channels` — this is intra-call
  chat, not channel posts.
- UI: side panel on web (right rail, 320 px),
  bottom-sheet detent on mobile (~70 %).
- System events (Alice joined, hand raised, screen share
  started) — generated client-side from `CallUiState`
  deltas, printed inline as grey lines.
- Persistence: chat remains in the regular messaging
  store after `endCall()` (it's a normal MLS group —
  history bundle for a new device works "for free").

**Exit criteria.**
- [ ] UI snapshot tests green.
- [ ] Send / receive round-trip through a mock MLS group
      in unit test.
- [ ] Unread badge clears on panel open.
- [ ] Chat persists in storage after `endCall`.

### S-3-A-sounds (G3-7)

**Composition.**
- Synthesise sounds via `tools/audio/synth-sounds.sh`
  (SoX / FFmpeg — sine bursts + ADSR envelopes). All
  CC0, fully owned. Output: `apps/*/Resources/Sounds/*.m4a`.
  - `ringtone-incoming.m4a` (loop)
  - `ringback-outgoing.m4a` (loop)
  - `join.m4a`, `leave.m4a`, `error.m4a`
- iOS: `AVAudioPlayer` for one-shot, `AVAudioEngine` for
  loops, CallKit ringtone via
  `CXProviderConfiguration.ringtoneSound`.
- Android: `SoundPool` + `MediaPlayer`, ConnectionService
  ringtone.
- Web: `HTMLAudioElement` for one-shot, `AudioContext`
  for timing precision.

**Exit criteria.**
- [ ] Sounds reproducibly generated by the script
      (CI artifact comparison).
- [ ] Unit tests: phase transition triggers the correct
      sound on each platform.
- [ ] No audio plays when the device is in silent mode
      (platform politeness).

### S-3-A-e2e-gate (G3-4)

**Composition.**
- `docker-compose.e2e.yml` fixture: `auth` + `ds` +
  `discovery` + the Rust SFU + coturn.
- Test runner on macOS GitHub Actions runs an iOS XCTest
  that connects to the containerised SFU for real.
- Linux GitHub Actions runs an Android instrumented test
  via headless emulator against the same fixture.
- Scenario:
  1. Two clients pull JWT from auth.
  2. Connect to the SFU via WS.
  3. Publish encrypted media frames.
  4. SFU forwards.
  5. Receiver decrypts → byte-exact match.

**Exit criteria.**
- [ ] E2E scenario green in CI on both platforms.
- [ ] PR blocked when the scenario goes red.

### S-3-A-acceptance (G3-8) — the only "not in chat" sprint

Performed by the maintainer with hardware. Checklist:

- [ ] iPhone ↔ Pixel 1×1 call via P2P, clean audio,
      crisp video.
- [ ] Same scenario through the group SFU.
- [ ] BT headset routing (AirPods, Sony).
- [ ] CallKit behaviour on iOS lock screen.
- [ ] ConnectionService behaviour on Android lock screen.
- [ ] Battery drain: 10-minute call < 5 % drain on
      iPhone 13+.
- [ ] Subjective MOS rating ≥ 4.0 on 4G / 5G in the office.

## Group B — Rust SFU sprints (replaces the LiveKit-fork plan)

Per [ADR-0011](./adr/0011-rust-sfu.md). Lives in
`services/sfu/` as a regular workspace Rust crate
(`lakko-svc-sfu`).

| Sprint | Composition | Estimate |
|---|---|---|
| **S-3-B-spike** | Echo-test bake-off `webrtc-rs` vs `str0m`; throughput / RTT / memory per peer; deal-breaker API gaps. Decision recorded as an amendment to ADR-0011 §"Spike outcome" | 0.5 sprint |
| **S-3-B-skeleton** | `services/sfu/` Rust crate. axum WS signaling endpoint. Peer registry (`PeerId → PeerConnection`). Echo test (one peer publishes, sees itself receiving) | 1 sprint |
| **S-3-B-routing-signaling** | Lakko-native typed wire protocol (`peer-joined` / `peer-left` / `signal-relay`) + per-peer mpsc broadcast fanout. Unblocks P2P 1×1 calls — clients exchange SDP / ICE through the SFU as an ICE trampoline without real RTP routing | 0.5 sprint |
| **S-3-B-routing-media** | Physical `str0m` integration: per-peer `Rtc` instance, ICE / DTLS handshake driven from signaling, RTP track fanout publisher → subscribers, SFrame ciphertext opaque passthrough. **Requires a working WebRTC pod on at least one client** (iOS or Android) to drive integration tests | 0.5–1 sprint |
| **S-3-B-simulcast** | Simulcast layer extraction (RID + dependency descriptor). Per-receiver layer selection (low / mid / high) based on downlink bitrate | 1 sprint |
| **S-3-B-jwt** | `lakko-call-jwt` verification — **same crate** that `services/auth` mints from, direct `workspace.dependencies` link. Test: HS256 → reject, expired → reject, wrong audience → reject | 0.25 sprint |
| **S-3-B-twcc** | TWCC aggregation. Per-receiver bandwidth estimation. Layer-switching feedback (when to step down / up) | 1 sprint |
| **S-3-B-metrics** | Prometheus exporter via the `metrics` crate (same surface as other services). Every metric referenced by `infra/prometheus/rules/calls.yml` exposed | 0.5 sprint |
| **S-3-B-prod** | Config via env-vars (project convention). Graceful shutdown. `/health` + `/ready`. `tracing` structured logging (same shape as auth / ds / discovery) | 1 sprint |
| **S-3-B-tests** | Integration tests with mock peers through the chosen WebRTC stack's test harness. Includes SFrame passthrough (ciphertext byte-exact unmodified by SFU) | 1 sprint |
| **Total** | | **≈ 7.5 sprints** |

### Group B exit criteria (whole group)

- [ ] `services/sfu/` builds as a regular Rust crate
      (`cargo build -p lakko-svc-sfu`).
- [ ] `#![forbid(unsafe_code)]`, `clippy::pedantic` clean.
- [ ] Docker image ≤ 30 MB (`distroless/cc` base).
- [ ] Every metric referenced by
      `infra/prometheus/rules/calls.yml` is exposed.
- [ ] Integration test: 3 mock peers, A publish → B + C
      receive, SFrame ciphertext byte-exact passthrough.
- [ ] Simulcast: slow-link receiver gets low layer,
      fast-link gets high; switch under TWCC feedback.
- [ ] `lakko-call-jwt` is the only auth path — no
      alternative verifier exists.
- [ ] Memory per active call ≤ 20 MB.

## Wave plan

```
W1: UI parity, no WebRTC dependencies
    S-3-A-settings ∥ S-3-A-chat ∥ S-3-A-sounds       (~1.5 sprints)
    └─ closes: G3-5, G3-6, G3-7

W2.1: Client peer connections — 1×1 P2P calls work
    S-3-A-iOS-pod ∥ S-3-A-Android-pod                (~2 sprints, parallel toolchains)
    └─ closes: G3-1, G3-2
    └─ MILESTONE: 1×1 calls end-to-end through coturn
                  (STUN / TURN), with no SFU involved

W2.2: Rust SFU — group calls work
    S-3-B-spike → S-3-B-skeleton → S-3-B-routing →
      S-3-B-simulcast ∥ S-3-B-jwt ∥ S-3-B-twcc ∥ S-3-B-metrics →
      S-3-B-prod → S-3-B-tests                       (~7.5 sprints)
    └─ closes: G3-3, GA-0
    └─ MILESTONE: group calls up to ~50 participants

W3: End-to-end CI gate
    S-3-A-e2e-gate                                    (~1 sprint)
    └─ closes: G3-4

W4: Acceptance on hardware
    S-3-A-acceptance                                  (out of chat)
    └─ closes: G3-8 → official Phase 3 close

W5+: Phase 4 (channels) + Phase 5 (anti-censorship) + audit gaps
     (see ROADMAP.md §2c / §3 for the detailed sub-sprint breakdowns)
```

## Dependencies

```
W1 (UI parity) ─────────────────────────────────────┐
                                                     │
W2.1 (G3-1, G3-2) ──→ MILESTONE: 1×1 works          │
   │                                                 │
   ├──→ W3 (G3-4) ───────────────────────────────────┼──→ W4 (G3-8)
   │                                                 │
W2.2 (G3-3, GA-0) ──→ MILESTONE: groups work        │
   │                                                 │
   └──→ W3 (G3-4)                                    │
                                                     │
W5 (G4-*) — independent of W2 / W3; can start in parallel with W2.2
W6 (G5-*) — independent; can start in parallel
```

## What changed vs the previous plan

| Before (ADR-0003 / ADR-0008) | After (ADR-0011) |
|---|---|
| LiveKit Go fork: 2 sprints + ongoing rebase tax | Rust SFU: 7.5 sprints + ~0 maintenance |
| ~150 k LOC audit surface | ~3 k LOC audit surface |
| Go + Rust toolchains in CI | Rust only |
| Recording architecturally possible (patched out) | Recording **not in the code** |
| Cross-language FFI in the dep graph | None |
| Wire schemas duplicated (Go-prost + Rust-prost) | `lakko-proto.workspace = true` directly |
| Auth-JWT logic duplicated (Go lib + Rust) | `lakko-call-jwt` linked directly into SFU |
| 1×1 milestone not called out | 1×1 milestone explicit, lands after W2.1 |

## What is unchanged

- Client side stays on native `WebRTC.framework` (iOS) and
  `webrtc-sdk:android` (Android) — needed for hardware
  codecs + CallKit / ConnectionService.
- W1 (UI parity) — unchanged.
- coturn for STUN / TURN — unchanged; used by both 1×1
  and group flows for NAT traversal.
- Phase 4 / Phase 5 sprints — unchanged.
- Audit gaps — only GA-0 added (the ADR-0011 record).

## Cross-links

- [ADR-0011](./adr/0011-rust-sfu.md) — Rust SFU strategy
- [ADR-0003](./adr/0003-livekit-fork.md) — superseded
- [ADR-0008](./adr/0008-livekit-fork-strategy.md) — superseded
- [`ROADMAP.md`](./ROADMAP.md) — long-form per-phase plan
- [`services/sfu/README.md`](../services/sfu/README.md) — SFU crate entry point
- [`services/sfu/PLAYBOOK.md`](../services/sfu/PLAYBOOK.md) — build / run / test runbook
- [`infra/README-calls.md`](../infra/README-calls.md) — deployment topology
- [`docs/security/calls.md`](./security/calls.md) — call-side crypto invariants (unchanged)
- [`docs/security/known-gaps.md`](./security/known-gaps.md) — audit-pack gap register
