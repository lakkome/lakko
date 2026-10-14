<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0013: Cross-platform SFrame AEAD AAD = the SFrame header (not host RTP metadata)

- **Status:** Accepted
- **Date:** 2026-06-02
- **Context:** SFU multi-party arc — making web (RTCRtpScriptTransform) and native (libwebrtc FrameEncryptorInterface) clients decrypt each other's SFrame media on the same SFU call.
- **Amended by:** [ADR-0071](0071-codec-header-in-the-clear.md) (Proposed, 2026-10-07): for call media the SFrame header moves to the end of the frame, a codec-header prefix stays in the clear at its start, and the AAD is the header plus that prefix plus a 3-byte trailer; still never host RTP metadata. Built on web first; the shared native bridge (iOS, Android) seals and opens that layout since 2026-10-11, unit- and host-tested, not run on a device or a call.

> **Update (2026-07) — the header is now RFC 9605 §4.3 wire-compatible.** An
> earlier revision of `SframeHeader::encode` used a Lakko-custom first-byte
> layout (`R|LEN|X|KID` with an always-trailing CTR) that was NOT RFC 9605 §4.3.
> That was corrected: `encode`/`decode` now pack the config byte as the §4.3
> `X | KKK | Y | CCC` (inline `KID<=7` / `CTR<=7`), proven byte-for-byte against
> RFC 9605 Appendix C.1's published vectors (`header.rs`
> `rfc_9605_appendix_c1_header_vectors`). The golden vector was re-pinned across
> all six stacks (Rust core, `lakko-sframe-native` C ABI, the C++ bridge, web
> wasm, iOS, Android). Because the header IS the AEAD AAD and it is now
> spec-conformant, a Lakko frame is interoperable with any other RFC 9605
> implementation that shares the key + nonce derivation — external interop is no
> longer broken by design; only a live cross-platform / external-stack RTP
> round-trip over a DEPLOYED SFU remains (G-26, a verification gate). This ADR's
> decision — bind the **SFrame header bytes** as the AEAD AAD, never the host
> RTP `additional_data` — is unchanged.

## Context

Lakko encrypts call media with **SFrame** (RFC 9605): each
sender seals every media frame under a per-sender key
([`lakko_calls::sframe::ParticipantKey`](../../crates/lakko-calls/src/sframe.rs))
derived from the per-call MLS exporter seed
(`Group.export_secret("lakko/call/v1", call_id, 32)`, audit-pack
C-1). The SFU forwards the SFrame ciphertext **without decrypting**
— that's how E2EE survives a star topology (security invariant #2:
the server only sees ciphertext; invariant #4: no non-E2EE
fallback).

For two clients to decrypt each other's media, **every input to the
AEAD must match byte-for-byte**: the key (✓ — both derive it from
the same MLS epoch), the nonce (✓ — derived from the per-frame
counter), and the **additional authenticated data (AAD)**.

The AAD is where the platforms currently diverge:

- The **`lakko_calls::sframe` core is AAD-agnostic** — `seal(aad,
  plaintext)` / `open(aad, ciphertext, counter)` feed whatever AAD
  the caller passes straight into ChaCha20-Poly1305. The convention
  lives in the *callers*, not the core.
- The **native shim** (`apps/shared/sframe-bridge/sframe-bridge.cc`)
  binds libwebrtc's RTP `additional_data` into the AAD (its comment:
  "with the RTP additional_data bound into the AAD"). `additional_data`
  is the generic frame-descriptor header libwebrtc computes — it is
  **platform-specific** and is **not reconstructible** from the
  `RTCEncodedAudioFrame` / `RTCEncodedVideoFrame` a browser
  `RTCRtpScriptTransform` sees.
  **(Past tense as of `b92a8a7`, 2026-06-02 — the same day this ADR
  landed. The shim was reconciled immediately; see the Reconciliation
  note under Consequences. This paragraph describes the divergence the
  ADR was written to remove, not a state that still exists.)**

Consequence: a web sender and a native receiver (or vice versa) would
feed **different AAD** into the same AEAD and every cross-platform
frame would fail to decrypt — silently. This is the unresolved half
of the long-tracked interop gap
[G-26](../security/known-gaps.md#g-26) (the SFrame wire-frame header serializer
is RFC 9605 §4.3-conformant; only a live cross-platform RTP round-trip over a
deployed SFU remains — see the Update above).

How the field solves this: every shipping E2EE-over-SFU client
(Google Meet client-side encryption, Jitsi e2ee, Matrix Element Call,
Signal group calls) makes the AEAD AAD **platform-independent** —
they authenticate the **SFrame header**, not the host WebRTC stack's
RTP descriptor (which they can't share across web ⇄ native anyway).

## Decision

**The SFrame AEAD AAD is the SFrame header bytes — the exact
output of [`SframeHeader::encode`](../../crates/lakko-calls/src/header.rs)
(the RFC 9605 §4.3 header layout; KID = sender leaf index,
CTR = per-frame counter) — and nothing else. No host RTP `additional_data`
is bound.**

> **The KID's meaning changed; this decision did not (2026-07,
> [ADR-0035](./0035-sframe-per-stream-kid-key.md)).** KID is no longer the bare
> sender leaf index — it is `(sender_leaf << 8) | stream_index`, so each of a
> participant's concurrent streams (audio, camera, screen-share) derives a
> disjoint key and nonce space. Read `KID = sender leaf index` below as
> `KID = the header's key identifier`, and the roster split as
> `sender_leaf = KID >> 8` (`SframeHeader::leaf_of` / `stream_of`). **Nothing
> about the AAD rule moves:** the AAD is still exactly the encoded header, the
> §4.3 codec is unchanged (a wider KID simply takes the extended form the RFC
> already defines), and the AAD is still computed from values both endpoints
> agree on with zero dependence on the host media stack — which is the whole
> point. One receiver-side detail did improve: because the *full* KID is the
> key input, the receiver derives the sender's key straight from the header and
> needs **no** local media/track-kind signal at all.

Concretely, on every platform, for each frame:

1. The sender takes the next frame counter `ctr` and its leaf `kid`.
2. It builds `header = SframeHeader { kid, ctr }.encode()`.
3. It seals: `ciphertext = AEAD(key, nonce(ctr), aad = header, plaintext)`.
4. It puts `header || ciphertext` on the wire (this is exactly what
   `wrap_sframe_frame` already produces).

The receiver runs the inverse: `unwrap_sframe_frame` parses the
header (recovering `kid` + `ctr`), selects the sender's
`ParticipantKey` by `kid`, and opens with `aad = header`.

This makes the AAD identical on web and native because the header is
computed from `(kid, ctr)` alone — values both sides already agree on
— with zero dependence on the host WebRTC media-stack internals.

### Why bind the header (and only the header)

- **The header MUST be authenticated.** `kid` selects which
  participant's key decrypts the frame; `ctr` derives the nonce. An
  on-path SFU that flipped `kid` or `ctr` without the header being in
  the AAD could, at minimum, cause targeted decrypt failures
  (availability). Binding the header makes any header tampering a
  clean AEAD authentication failure — which is RFC 9605's model
  (§4.4: the header is part of the authenticated data).
- **The RTP `additional_data` MUST NOT be bound** — it is the single
  thing that breaks web ⇄ native interop, and it carries no E2EE
  value the AEAD doesn't already provide: media confidentiality +
  integrity come from the ciphertext + tag regardless. Binding the
  RTP header was a libwebrtc-FrameEncryptor-convenient defense in
  depth, not a Lakko E2EE requirement.

## Consequences

- **Crypto core unchanged.** `lakko_calls::sframe::ParticipantKey`
  stays AAD-agnostic; the contract is enforced by the callers. The
  MLS state machine (invariant #3) is untouched.
- **API adjustment (callers).** Because the header carries `ctr`, the
  caller must know the counter *before* sealing so it can build the
  header to pass as AAD. The clean shape is a single
  `seal_framed(plaintext) -> wire_bytes` helper that, atomically:
  reads its own next counter, builds the header, seals with the
  header as AAD, and returns `header || ciphertext`. This wraps the
  existing `seal` + `wrap_sframe_frame` without changing the AEAD
  primitive. (Implemented as a follow-up to this ADR, with property
  tests + the audit-pack update the crypto-change rule requires.)
- **Native reconciliation — DONE (`b92a8a7`, 2026-06-02).**
  `apps/shared/sframe-bridge/sframe-bridge.cc` ~~stops passing~~ **no longer
  passes** libwebrtc `additional_data` and passes the SFrame header as AAD
  instead. Both entry points now take the parameter unnamed
  (`rtc::ArrayView<const uint8_t> /*additional_data*/`, `sframe-bridge.cc:80`
  and `:212`) with the reason stated in the code at each site — so the omission
  is deliberate and readable, not an oversight waiting to be "fixed" back.
  This **changed the bytes iOS/Android put on the wire**, and the host-side
  test was re-verified with it: `apps/shared/sframe-bridge/test_bridge.cc`
  seals and opens the canonical cross-stack golden vector **while passing a
  non-empty host `additional_data`**, which is the assertion that pins the
  decision — a regression that re-bound the RTP data would fail there rather
  than in a call. Indexed as **C-53**.
- **Closes G-26.** With a single platform-independent AAD + the
  already-landed `SframeHeader` wire codec, the RFC 9605 wire-frame
  interop gap is resolved in design; the closeout is the native
  reconciliation + the web `RTCRtpScriptTransform` worker built on
  this contract.
- **Web unblocked.** The web SFrame worker (next subsprint) seals /
  opens with `aad = SframeHeader::encode(kid, ctr)`, identical to
  native — so a web publisher's media decrypts on iOS/Android and
  vice versa.

## Alternatives considered

- **AAD = empty (`&[]`).** Simplest — native just drops
  `additional_data`, no header-before-seal API change. The counter is
  still bound (it derives the nonce) and `kid` still selects the key,
  so confidentiality + integrity hold. **Rejected** because it leaves
  the header unauthenticated: a flipped `kid`/`ctr` degrades to a
  decrypt failure rather than an explicit authentication failure, and
  it diverges from RFC 9605's authenticated-header model for no
  simplicity win once the `seal_framed` helper exists.
- **AAD = RTP `additional_data` (status quo on native).** Keeps the
  native defense-in-depth but is **not reconstructible on web** →
  permanently breaks web ⇄ native interop. Rejected — this is the
  divergence this ADR removes.
- **Per-platform AAD with a translation shim.** Bind RTP data on
  native, something else on web, translate at the SFU. Rejected — the
  SFU must stay oblivious to media (invariant #2); it cannot
  participate in the AEAD.

## References

- RFC 9605 — Secure Frame (SFrame).
- [`crates/lakko-calls/src/header.rs`](../../crates/lakko-calls/src/header.rs) — `SframeHeader::encode` / `decode` (the AAD bytes).
- [`crates/lakko-calls/src/sframe.rs`](../../crates/lakko-calls/src/sframe.rs) — `ParticipantKey::seal` / `open` (AAD-agnostic AEAD).
- [`apps/shared/sframe-bridge/sframe-bridge.cc`](../../apps/shared/sframe-bridge/sframe-bridge.cc) — native shim (~~to reconcile~~ **reconciled**, `b92a8a7`).
- [`docs/security/known-gaps.md` G-26](../security/known-gaps.md) — the interop gap this closes.
- [ADR-0011](./0011-rust-sfu.md) — the Rust SFU this media flows through.
