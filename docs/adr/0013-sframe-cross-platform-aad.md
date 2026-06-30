<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0013: Cross-platform SFrame AEAD AAD = the SFrame header (not host RTP metadata)

- **Status:** Accepted
- **Date:** 2026-06-02
- **Context:** SFU multi-party arc — making web (RTCRtpScriptTransform) and native (libwebrtc FrameEncryptorInterface) clients decrypt each other's SFrame media on the same SFU call.

> **Correction (2026-06-30, `lakko-security` APPROVE-WITH-NOTES).** This ADR's
> decision — bind the **SFrame header bytes** as the AEAD AAD, never the host
> WebRTC stack's RTP `additional_data` — is sound and unaffected. But the header
> that `SframeHeader::encode` produces is a **Lakko-custom layout, NOT RFC 9605
> §4.3 wire-compatible**: Lakko packs the first byte as `R|LEN|X|KID` with an
> always-trailing CTR, whereas RFC 9605 §4.3 packs `X|K|Y|C` with inline
> `KID<=7` / `CTR<=7`, so the bytes differ on every input. All Lakko stacks pin
> the same header (the golden vector), so the cross-platform-AAD guarantee this
> ADR establishes holds **within the Lakko ecosystem**. A non-Lakko RFC-9605
> stack would compute a different header → different AAD → fail to interop;
> that external-stack interop is broken by design and tracked under
> [G-26 / GA-2](../security/known-gaps.md#g-26) (a true §4.3 rewrite is a
> deferred follow-up gated on an external SFrame stack becoming a deployment
> target). Read every "RFC 9605 SFrame header" mention below as "the (Lakko-custom)
> SFrame header".

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

Consequence: a web sender and a native receiver (or vice versa) would
feed **different AAD** into the same AEAD and every cross-platform
frame would fail to decrypt — silently. This is the unresolved half
of the long-tracked interop gap
[G-26](../security/known-gaps.md#g-26) (the SFrame wire-frame header —
Lakko-custom, not RFC 9605 §4.3; Lakko-internal interop landed, external
interop broken by design — see the Correction above).

How the field solves this: every shipping E2EE-over-SFU client
(Google Meet client-side encryption, Jitsi e2ee, Matrix Element Call,
Signal group calls) makes the AEAD AAD **platform-independent** —
they authenticate the **SFrame header**, not the host WebRTC stack's
RTP descriptor (which they can't share across web ⇄ native anyway).

## Decision

**The SFrame AEAD AAD is the SFrame header bytes — the exact
output of [`SframeHeader::encode`](../../crates/lakko-calls/src/header.rs)
(a Lakko-custom layout, see the Correction above; KID = sender leaf index,
CTR = per-frame counter) — and nothing else. No host RTP `additional_data`
is bound.**

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
- **Native reconciliation.** `apps/shared/sframe-bridge/sframe-bridge.cc`
  stops passing libwebrtc `additional_data` and passes the SFrame
  header as AAD instead. This **changes the bytes iOS/Android already
  put on the wire**, so it requires a re-verified host-side test
  (`test_bridge.cc`) + a device smoke before shipping. Until both
  native and web are on this contract, cross-platform calls are not
  interop-safe (and per invariant #4 must refuse rather than fall
  back to a server-readable path).
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
- [`apps/shared/sframe-bridge/sframe-bridge.cc`](../../apps/shared/sframe-bridge/sframe-bridge.cc) — native shim (to reconcile).
- [`docs/security/known-gaps.md` G-26](../security/known-gaps.md) — the interop gap this closes.
- [ADR-0011](./0011-rust-sfu.md) — the Rust SFU this media flows through.
