---
name: sfu-media
description: Background knowledge for Lakko SFU + real-time media development — str0m, media routing, track negotiation, SFrame opacity, signaling wire protocol, and the call-side security invariants. Auto-applies when working on services/sfu/ or crates/lakko-calls/.
user-invocable: false
---

# Lakko SFU + media — domain knowledge

You are working on the Rust SFU (`services/sfu/`) and the media-key / SFrame layer (`crates/lakko-calls/`). This is the real-time media plane of an E2EE messenger. The SFU is a zero-trust relay: it forwards encrypted RTP without ever seeing plaintext media.

## Source map (`services/sfu/src/`)

| File | Owns |
|---|---|
| `protocol.rs` | Re-exports `lakko-sfu-proto` wire frames (ServerFrame / ClientFrame). The single wire definition shared with clients. |
| `signaling.rs` | WS signaling half — welcome / peer-joined / peer-left / signal-relay / ping-pong / error. JWT-admitted, per-peer mpsc fanout. |
| `media.rs` | Media half — `MediaPeer` (wraps `str0m::Rtc`), `MediaRouter`, `MediaCommand`, `MediaTaskHandle`, `spawn_media_task`, track routing, RTP fanout. |
| `peer.rs` | Per-peer state. |
| `router.rs` | App router wiring (`AppState`, `build_app_with_media`). |
| `jwks.rs` | Call-admission JWKS verification. |
| `config.rs` | `LAKKO_SFU_*` env config (incl. `LAKKO_SFU_MEDIA_BIND`). |
| `prom.rs` | Prometheus counters (`lakko_sfu_media_frames_forwarded_total`, `..._dropped_total{reason}`). |

## Hard rules — the SFU is opaque

- **The SFU NEVER inspects `payload_b64` / RTP media bytes.** It forwards `MediaForward` bytes verbatim. SFrame ciphertext is opaque (the opacity invariant, C-39).
- **No plaintext media path exists.** A client that cannot arm SFrame must refuse the call (security invariant #4). The SFU has no fallback to relay unencrypted.
- **Cross-call isolation** is mandatory — a peer in call A must never receive frames from call B.
- **Admission is JWT-gated** (`lakko-call-jwt`, EdDSA-Ed25519, JWKS). An unadmitted WS connection gets no frames.

## str0m specifics

- Pinned `str0m =0.19.0` with the `rust-crypto` feature (pure-Rust crypto provider — NO openssl / aws-lc-rs).
- The async/sync boundary is `MediaTaskHandle` ↔ `spawn_media_task`: commands cross via mpsc + `oneshot` reply channels, with `try_send` back-pressure (`Backpressure` / `TaskGone` / `Media(MediaError)` typed errors).
- `Rtc::poll_output` drives `Output::Transmit` → `UdpSocket::send_to`. Peers whose `Rtc::is_alive` flips after DTLS/ICE failure are pruned.
- SDP / ICE: `ClientFrame::SdpOffer` + `IceCandidate`, `ServerFrame::SdpAnswer`. Media peer lazy-promoted on first offer via `peer_or_insert`.
- Track routing: `TrackIn` / `TrackOutEntry` / `TrackOutState`, `Event::MediaAdded` capture, `propagate_new_tracks_in` opens a `ToOpen` on every non-origin peer, `drive_renegotiations` dispatches SFU-initiated `SdpOffer`s.
- Downstream MSID stream-id is pinned to the source peer id (`MediaPeerId == PeerId`) so clients map a remote track to a roster tile.

## SFrame / media-key invariants (check before touching `lakko-calls`)

- **C-1**: same MLS group + epoch → byte-identical `CallKey` seeds (deterministic exporter `group.export_secret("lakko/call/v1", call_id, 32)`).
- **C-5**: frame counter strictly monotonic per sender per epoch.
- **C-6 / ADR-0013**: AEAD AAD = the RFC 9605 §4.3 SFrame wire header bytes (KID+CTR) — NEVER host RTP `additional_data`. This is what lets a frame sealed on web open under the native shim and vice versa.
- **C-7**: wrong frame counter on receive is rejected.
- **C-8 / C-9 / C-10**: MLS epoch advance rotates the seed; a 1-second overlap window absorbs only in-flight frames; only the most-recent previous epoch is retained.

## Cross-platform SFrame stack (all must agree)

One core: `lakko_calls::sframe` `seal_framed` / `open_framed`. Three consumers:
- **Native** (iOS/Android): `lakko-sframe-native` C ABI → `apps/shared/sframe-bridge/` C++ → Obj-C++ shim / JNI module → libwebrtc `FrameEncryptorInterface` / `FrameDecryptorInterface`.
- **Web**: `lakko-ffi-wasm` → `apps/web/src/calls/sframe-transform.ts` + worker → `RTCRtpScriptTransform`.

A change to the SFrame core or `seal_framed` requires recross-compiling: `just cross-build-sframe-native-ios` + `just cross-build-sframe-native-android` + a fresh wasm build, or the platform shims reference stale symbols.

## Commands

```
cargo run -p lakko-svc-sfu            # or: just dev-sfu (set LAKKO_SFU_MEDIA_BIND for media)
just test                             # workspace tests incl. sfu unit + integration
cargo test -p lakko-svc-sfu           # sfu only
```

Integration tests of note: `services/sfu/tests/synthetic_client_e2e.rs` (full ICE+DTLS against a real `str0m::Rtc`), `two_clients_cross_fanout_forwards_media_verbatim` (publisher payload surfaces verbatim at a subscriber). The Dockerized e2e lives in `ci/sfu-e2e/` + `services/sfu/tests/e2e_dockerized.rs` (`#[ignore]`-gated).

## Fuzzing

`services/sfu/fuzz/` — `client_frame_decode` fuzzes the attacker-controlled WS-text-frame parser (admitted-but-malicious peer reaches it post-JWT, pre-routing). Extend it if you add a new client-frame parser surface.

## Deferred / known

- Simulcast-layer selection on the downstream offer (RID / dependency-descriptor extraction + per-receiver bandwidth-driven layer choice) — deferred, needs simulcast-capable test clients.
- Live cross-platform RTP verification over a deployed SFU (G-26) — the last open SFrame interop gate.

## Apply the rust-core rules too

All `crates/`/`services/` code-style rules apply (no unwrap in prod, `#![forbid(unsafe_code)]`, tracing not println, clippy pedantic+nursery, SPDX headers). See the rust-core skill.
