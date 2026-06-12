<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Lakko — Invariant index

Consolidated cross-reference for every numbered security
invariant in this audit pack. Reviewer cheat-sheet: open
this page, find the invariant ID, jump to the file +
section that describes it.

## Quick stats

| Class | Count | Range | Source file |
|---|---|---|---|
| **I-** | 8   | I-1..I-8   | [`mls-state-machine.md`](./mls-state-machine.md) |
| **C-** | 54  | C-1..C-54  | [`calls.md`](./calls.md) |
| **T-** | 15  | T-1..T-15  | [`transport.md`](./transport.md) |
| **G-** | 27  | G-1..G-27 (G-28 closed via the G3-sframe-native arc N-1..N-6) | [`known-gaps.md`](./known-gaps.md) — open / deferred |
| Total invariants  | 77  | (I + C + T) | live properties |
| Total open gaps   | 27  | (G)         | tracked + scoped |

## MLS state machine invariants (I-1..I-8)

Source: [`mls-state-machine.md`](./mls-state-machine.md)
— OpenMLS wrapper properties the state machine MUST
preserve. Each invariant is backed by a property test
in `crates/lakko-mls/`.

| ID  | Title | Topic |
|---|---|---|
| I-1 | A Welcome targeting a different `account_id` is rejected before any state advance | DeviceGroup join gate |
| I-2 | `remove_device_at(own_leaf)` is rejected | Self-removal guard |
| I-3 | `remove_device_at(unknown_leaf)` is rejected before OpenMLS sees it | Pre-flight check |
| I-4 | After a successful `add_device_from_wire` / `join_from_welcome` pair, both devices' exporter secrets agree | Cross-device key agreement |
| I-5 | Sealing a `HistoryBundle` larger than `MAX_BUNDLE_MESSAGES = 1000` is rejected | Bundle bound |
| I-6 | Opening a `SealedBundle` with a wrong key surfaces as an authentication failure, not a parse failure | Failure-mode discrimination |
| I-7 | Opening a `SealedBundle` with an unknown `algorithm` discriminator is rejected before AEAD-open is attempted | Forward-compat ratchet |
| I-8 | `create_devicegroup_message` round-trips through `parse_devicegroup_payload` losslessly for every typed body variant | Wire-format byte-stability |

## Call-side invariants (C-1..C-54)

Source: [`calls.md`](./calls.md). Five sub-sections —
media-key pipeline (C-1..C-14), call-admission JWT
(C-15..C-23), link-based invite + composer + dispatcher
(C-24..C-36), SFrame + RFC 9605 + SFU signaling
(C-37..C-39), channel-broadcast backend opacity
(C-40..C-44), then per-arc additions C-45..C-55
(SFrame native shim, TURN, PublisherGroup persistence,
host-side bridge validation, rotation policy, and the
SFU multi-party client SFrame coverage).

### C-1..C-14 — media-key pipeline (SFrame + overlap window)

| ID   | Title |
|---|---|
| C-1  | Two parties in the same MLS group at the same epoch derive byte-identical `CallKey` seeds |
| C-2  | Cross-party seal/open works end-to-end through the MLS exporter |
| C-3  | Per-participant keys differ across leaves |
| C-4  | Three-party full-mesh seal/open holds |
| C-5  | Frame counter is strictly monotonic per sender per epoch |
| C-6  | AEAD AAD-binding holds across the cross-party path |
| C-7  | Wrong frame counter on the receive path is rejected |
| C-8  | MLS epoch advance rotates the `CallKey` seed |
| C-9  | The 1-second overlap window absorbs in-flight frames and ONLY in-flight frames |
| C-10 | Only the most-recent previous epoch is retained in the overlap slot |
| C-11 | The 1×1 call signaling dispatcher does NOT cross-feed group-control bodies into the state machine |
| C-12 | Wrong-`call_id` signaling bodies are dropped silently |
| C-13 | `EndReason` parser folds unknown discriminator values to `HungUp` |
| C-14 | `call_id` length is strictly enforced at parse time |

### C-15..C-23 — call-admission JWT

| ID   | Title |
|---|---|
| C-15 | Call JWTs are signed by an Ed25519 key persisted across restarts |
| C-16 | A JWT with the wrong `iss` or `aud` is rejected |
| C-17 | A JWT past its `exp` is rejected |
| C-18 | A JWT signed by a different key is rejected, even if the `kid` matches |
| C-19 | A JWT with a tampered header / payload is rejected |
| C-20 | `kid` is a deterministic, stable function of the public key |
| C-21 | `POST /call/jwt` requires a valid session token |
| C-22 | `verify_with_jwks` resolves the correct key by `kid` across a multi-key JWKS doc |
| C-23 | `Jwk::to_verifying_key` rejects non-OKP / non-Ed25519 JWKs |

### C-24..C-36 — link-based invite + composer + dispatcher

| ID   | Title |
|---|---|
| C-24 | A `CallInviteCapability` with a tampered payload is rejected |
| C-25 | Splicing a different owner pubkey into a signed capability is rejected |
| C-26 | Schema-version + bounds enforced at parse time |
| C-27 | Cross-party `CallKey` seeds agree across the bootstrap path |
| C-28 | Owner fingerprint enables OOB anti-phishing verification |
| C-29 | Server-side call-invite ledger handler invariants |
| C-30 | `/c/<slug>` URL plumbing — discrimination contract |
| C-31 | Composer + acceptor pipelines — single-call surface contract |
| C-32 | Call-Welcome envelope reuses ContactRequest transport with domain separation |
| C-33 | Call KeyPackage-forward envelope for subsequent joiners |
| C-34 | Owner-side incoming-envelope dispatcher pins dispatch order |
| C-35 | End-to-end link-call wire chain holds across both 1×1 and multi-party paths |
| C-36 | Owner-handover preserves call-key chain across mid-call committer leave |

### C-37..C-39 — SFrame + RFC 9605 + SFU signaling

| ID   | Title |
|---|---|
| C-37 | SFrame keying math + AEAD pipeline pinned |
| C-38 | RFC 9605 §4.3 wire-header round-trip pinned |
| C-39 | SFU enforces zero-payload-inspection + cross-call isolation across BOTH the signaling AND the media (RTP) plane. Media-plane fanout (`fanout_media_data`) + downstream-track opening (`propagate_new_tracks_in`) are call-scoped on the server-authoritative JWT `call_id` (fail-closed; renegotiation transitively scoped). The media-plane enforcement was a security fix — before it the fanout had no `call_id` filter, leaking SFrame-encrypted media across calls (confidentiality held via SFrame, isolation/metadata invariant did not); `lakko-security` APPROVE |

### C-40..C-44 — channel-broadcast backend

| ID   | Title |
|---|---|
| C-40 | services/channels publisher path treats every byte as opaque |
| C-41 | services/channels subscribe handshake opacity holds |
| C-42 | Rotation is atomic — either every envelope lands or none does |
| C-43 | FFI subscriber-side surface preserves the in-Rust opacity contract |
| C-44 | Production channel seed comes from the MLS exporter; the demo path is grep-clean |

### C-45..C-50 — SFrame native-shim stack (G3-sframe-native arc, closes G-28)

| ID   | Title |
|---|---|
| C-45 | lakko-sframe-native C ABI is the only path into the AEAD seal from non-Rust callers |
| C-46 | Bad seed length is refused at every layer, no silent downgrade |
| C-47 | Boot-time selector probe enforces invariant #4 (no fallback to non-E2EE) |
| C-48 | SFrame attach happens between addLocalAudioTrack and SDP build |
| C-49 | Per-sender ParticipantKey cache is bounded by leaf count + thread-safe under multi-sender fan-in |
| C-50 | Refcount machinery balances every AddRef with exactly one Release, no leak no double-free |

### C-51 — TURN credential issuance (G3-{1,2}-turn)

| ID   | Title |
|---|---|
| C-51 | TURN credential follows the coturn `static-auth-secret` REST scheme; bound to a per-call expiry |

### C-52 — PublisherGroup persistence + rotation (G4-2-mls-persistence)

| ID   | Title |
|---|---|
| C-52 | PublisherGroup MLS state is persisted by deterministic channel-id, exporter output stable across reloads, rotation advances the epoch |

### C-53 — SFrame native-shim host-side validation (N-{4,5}-validate)

| ID   | Title |
|---|---|
| C-53 | Host-side bridge round-trip exercises every documented tampering mode of the SFrame native shim before either platform binding is loaded |

### C-54 — Channel rotation policy (G4-2-rotation-timer)

| ID   | Title |
|---|---|
| C-54 | Scheduled rotation cadence is governed by a pure policy, identical across all three platforms, with clock-skew-tolerant arithmetic |

### C-55 — SFU multi-party client SFrame coverage + cross-platform AAD (SFU-3 arc, ADR-0013)

| ID   | Title |
|---|---|
| C-55 | SFU multi-party media is SFrame-sealed end-to-end on every client (web / iOS / Android); the AEAD AAD is the cross-platform wire header, not host RTP metadata |

## Transport-layer invariants (T-1..T-11)

Source: [`transport.md`](./transport.md). Anti-censorship
+ rendezvous + Shadowsocks-2022 surfaces (Phase 5).

| ID   | Title |
|---|---|
| T-1  | Pluggable-kind wire identifiers are frozen at v1 |
| T-2  | Scaffold transports surface `NotImplemented` loudly |
| T-3  | Selector race preserves submission order |
| T-4  | Rendezvous verify is a 4-stage gate |
| T-5  | Canonical bytes = JSON with `signature_hex` zeroed |
| T-6  | TransportSpec kebab-case kind is part of the protocol surface |
| T-7  | Trust anchor is binary-pinned |
| T-8  | DoH fetcher preserves the rendezvous trust chain end-to-end |
| T-9  | Chunked AEAD stream primitive pins ChaCha20-Poly1305 + monotonic nonce |
| T-10 | Shadowsocks-2022 outer header layer pins BLAKE3 context + timestamp window + replay window |
| T-11 | SS-2022 TCP wiring preserves per-direction key separation + frame-size cap |
| T-12 | ECH config list parser is version-pinned (0xfe0d) + length-bounded (16 KiB outer / 16 inner configs / 4 KiB per config); wire-up via RustCrypto/hpke adapter on rustls 0.23 (HTTP + WebTransport landed, REALITY architecturally not applicable) |
| T-13 | REALITY authenticator is X25519 ECDH + HKDF-SHA-256 (label `lakko/reality/v1`) + HMAC-SHA-256 over (short_id || timestamp); tag verified constant-time before any downstream byte is trusted |
| T-14 | WebTransport client uses cert-hash pinning (SHA-256 of full DER cert, W3C `serverCertificateHashes` mode) + u32-BE length-prefixed framing with MAX_FRAME_BYTES enforcement |
| T-15 | MASQUE config + CONNECT-UDP request (RFC 9298 §3) + HTTP Datagram payload codec (RFC 9297 §3) are spec-aligned and length-bounded; CONNECT-UDP Context ID 0 pinned, others refused |

## Known gaps (G-1..G-26)

Source: [`known-gaps.md`](./known-gaps.md). Items the
maintainer knows are not audit-ready yet; reviewers
should not re-discover them.

### MLS state machine

| ID  | Title |
|---|---|
| G-1 | Channel two-tier flow (>50k subscribers) — **CLOSED** by Phase 4 backend + FFI (C-40..C-44) |
| G-2 | MLS-2 / quantum-hybrid suites pinned for later |
| G-3 | External-join proposals not wired into a UI surface |

### Multi-device

| ID  | Title |
|---|---|
| G-4 | AccountIdentity rotation on password change has no KT-log supersession proof yet |
| G-5 | Commit broadcast on `add_device_from_wire` to other pre-paired devices — **CLOSED** (all three orchestrators publish the commit to the DeviceGroup DS journal) |
| G-6 | History bundle `payload_bytes` use a JSON placeholder, not raw MLS application-payload bytes |
| G-7 | DeviceGroup snapshot persistence may briefly diverge across in-flight ratchet operations — **CLOSED** (persist-before-publish reorder on all three clients) |

### Web client

| ID  | Title |
|---|---|
| G-8 | Web has no Service Worker-side message-log mutation listener |
| G-9 | Web `payload_bytes` JSON placeholder strings are NOT byte-stable |

### Backup

| ID   | Title |
|---|---|
| G-10 | Backup envelope rotation on password change happens on the next baseline write, not the recover round-trip itself |
| G-11 | No tamper-evident chain across backup versions — **client-with-state case CLOSED** by `lakko-backup::chain` v2 envelope; fresh-install case still open (needs KT-log anchor) |

### Auth

| ID   | Title |
|---|---|
| G-12 | Session token validation is single-process in-memory — **CLOSED** (stale text; tokens are the stateless HMAC-SHA256 `lakko-auth-token` shape under a shared `LAKKO_SESSION_TOKEN_SECRET` — any instance validates without coordination, survives restarts) |
| G-13 | WebAuthn passkey lacks per-credential authentic-action attestation |

### Out-of-protocol

| ID   | Title |
|---|---|
| G-14 | Hostile-OS / hostile-browser threats |
| G-15 | Side-channel + physical attacks |
| G-16 | Quantum adversaries |

### Calls

| ID   | Title |
|---|---|
| G-17 | Frame-counter exhaustion path is test-exercised — **CLOSED** by 3-test trio in `crates/lakko-calls/src/sframe.rs` |
| G-18 | Overlap-window 1-second timer lives in platform code, not in the Rust core |
| G-19 | SFU-side JWT validation lands with 3b1c (LiveKit fork) — **superseded by ADR-0011 (str0m SFU); C-39 covers the new path** |
| G-20 | Call recording is out of scope (deferred per ROADMAP 3c8) |
| G-21 | Link-based call invites are single-joiner in V1 — **CLOSED** by C-33/C-35 KP-forward + multi-party tests |
| G-22 | Per-joiner dedup for call invites lands with multi-use orchestration — **CLOSED** by C-33/C-35 |
| G-26 | RFC 9605 §4.3 wire frame header — serializer + framed pipeline + cross-platform AAD **landed** (C-38 / C-55, ADR-0013); SFrame sealed end-to-end on all three SFU clients. **Effectively closed:** the cross-runtime byte interop is now a *proven equality* via a golden vector pinned + asserted (seal AND open) on all four stacks — Rust core / wasm-web / iOS-UniFFI / Android-UniFFI (`sframe.rs::golden_vector_*`, `sframe-golden.test.ts`, `SFrameGoldenTests.swift`, `SframeGoldenTest.kt`; Rust+web+iOS verified firsthand, Android in `g3-4-mobile.yml` CI), and the live-SFU verbatim forward is proven by `sframe_sealed_frame_round_trips_through_the_live_sfu`. Composed, the sealed-on-web → SFU-relay → opened-on-native path holds by correctness. Residual: a single monolithic real-Chrome ⊕ real-native ⊕ deployed-SFU smoke — redundant for correctness, a deploy/CI-execution concern |
| G-27 | Channel PublisherGroup — **web wired** (G4-2-mls-publisher); iOS / Android still using `newChannelSeedForTest`; persistence + rotation deferred to G4-2-mls-persistence |

### Dependency advisories

| ID   | Title |
|---|---|
| G-23 | `libcrux-chacha20poly1305 0.0.7` — RUSTSEC-2026-0124 (HIGH 8.2) — upstream-blocked |
| G-24 | `rsa 0.9.10` — RUSTSEC-2023-0071 (MEDIUM 5.9) — Marvin Attack — `sqlx-mysql` MariaDB-auth transitive (NOT TLS-stack — see known-gaps.md G-24 for reachability analysis) |
| G-25 | Three unmaintained-crate warnings |

## How to add a new invariant

Adding a new invariant means **all four** of the steps
below — drift between this index and the source files is
the audit-grep failure mode this page exists to prevent.

1. Decide which class — `I-` (MLS), `C-` (calls /
   channels — everything the SFU/channels server might
   see), `T-` (transport), `G-` (gap, not yet a real
   invariant).
2. Pick the next free integer. The class is namespaced —
   `C-45` and `T-12` are both legal next IDs at any time.
3. Add the invariant to its source file in this audit
   pack. Convention: `### {ID}: {Title}` followed by a
   short rationale + pinned tests / code references.
4. Add a row in **this** index — same ID, same title.
5. If the new invariant supersedes an old one, edit the
   old row to mention the supersession (don't delete —
   the row's removal would break grep + historical
   commit-message links).

## Counter sanity checks

If `wc -l docs/security/{calls,transport,mls-state-machine}.md`
goes up significantly without this index changing, an
invariant landed without being indexed. **CI gates on
this** via [`tools/audit-index-check`](../../tools/audit-index-check)
— a bash script that diffs the `### {class}-{n}:` headers
in the three source files against the per-class tables on
this page. Both directions are checked: a new invariant
in the source must have a matching index row, and a stale
index row whose source-file header is gone is also a fail.
Wired into `just ci` as the final docs gate. Run locally
via `just audit-index-check`.

## Cross-links

- [`README.md`](./README.md) — entry point for the audit
  pack.
- [`threat-model.md`](./threat-model.md) — adversaries +
  assumptions every invariant is defending against.
- [`crypto-overview.md`](./crypto-overview.md) — the
  primitive library each invariant rests on.
- [`../ROADMAP-PHASE3-CLOSEOUT.md`](../ROADMAP-PHASE3-CLOSEOUT.md)
  — sprint-level decomposition; every invariant in this
  page traces back to a sprint row there.
- [`../adr/`](../adr/) — Architecture Decision Records;
  the strategic rationale behind the protocols every
  invariant pins.
