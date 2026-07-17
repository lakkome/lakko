<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0034: Per-(leaf, media-type) SFrame key derivation — close the cross-stream AEAD nonce reuse

- **Status:** Proposed
- **Date:** 2026-07-17
- **Context:** Phase 3 (calls / SFrame media encryption) — security fix

## Context

A `lakko-security` review of the mid-call camera-on arc (video-calls piece B,
ADR-0032) surfaced a **HIGH** finding that is pre-existing and system-wide, not
introduced by that arc. The arc's own control flow is correct and fail-closed;
the finding is in the SFrame key-derivation core.

### Root cause (verified in `crates/lakko-calls/src/sframe.rs`)

`CallKey::participant_key(sender_leaf: u32)` derives **both** the AEAD key and
the 12-byte nonce salt from `(seed, sender_leaf)` alone:

- key: `HKDF-SHA-256(seed, info = PARTICIPANT_KEY_INFO ‖ sender_leaf_be)`
- nonce salt: `HKDF-SHA-256(seed, info = FRAME_NONCE_INFO ‖ sender_leaf_be)`

The returned `ParticipantKey` starts `counter = 0` and increments once per
sealed frame; the per-frame nonce is `nonce_salt XOR (counter as u32 BE)`. The
SFrame header KID stamped on every frame is `sender_leaf` alone (the C++ bridge
`LakkoFrameEncryptor::Encrypt` receives but ignores `cricket::MediaType` /
`ssrc`, `apps/shared/sframe-bridge/sframe-bridge.cc:69`).

### The collision

Every `armSFrame*` call site passes the client's single `localLeaf` as the KID
— for the audio sender **and** the video sender. So a participant who sends
audio + video arms **two** `ParticipantKey` / `LakkoFrameEncryptor` instances
that share the **same key** and **same nonce salt**, each with an
**independent** counter starting at 0. Audio frame `N` and video frame `N`
therefore seal under the identical `(key, nonce)` pair on different plaintexts:

- ChaCha20 keystream reuse: `C_audio(n) XOR C_video(n) = P_audio(n) XOR
  P_video(n)` — leaks the XOR of the two plaintext streams.
- Poly1305 one-time-key reuse across two messages — enables tag forgery.

### Threat and severity

For an **SFU group video call**, the zero-trust SFU relays and therefore holds
both ciphertext streams — this is exactly the adversary invariant #2 targets.
It can recover plaintext structure from the keystream reuse and forge
authentication tags. This is a real break of media confidentiality and
integrity for any participant who sends audio + video simultaneously →
**HIGH**.

For a **1×1 native** call the SFrame layer sits behind P2P DTLS-SRTP (no server
in the media path), so the reuse is defence-in-depth only → low, but it should
still be fixed for consistency and because a future SFU-routed 1×1 would expose
it. The **web 1×1** path is DTLS-SRTP with no SFrame, so it is unaffected.

The collision is not caused by piece B's logic — it already exists for any
call started with video — but turning the camera on mid-call is precisely the
action that creates the second colliding stream, so the feature under review
directly triggers it. It also contradicts the documented **C-5** invariant
(`docs/security/calls.md`: "per-participant counter … not a silent nonce
reuse"), which was framed per single `ParticipantKey` and silently assumed one
stream per participant.

RFC 9605 §9 requires the `(key, CTR)` pair to be unique; the current derivation
violates it whenever a participant has more than one media stream under one KID.

## Decision

**Fold a media-type discriminator (Audio / Video) into the `participant_key`
derivation so a participant's audio and video streams get distinct keys AND
distinct nonce salts, while leaving the on-wire SFrame header KID unchanged
(= `sender_leaf`).** (Approach A.)

`participant_key(sender_leaf, media_type)` mixes `media_type` into **both** HKDF
`info` strings:

- key: `info = PARTICIPANT_KEY_INFO ‖ sender_leaf_be ‖ media_type_byte`
- nonce salt: `info = FRAME_NONCE_INFO ‖ sender_leaf_be ‖ media_type_byte`

where `media_type_byte` is a fixed 1-byte tag (`0x01` audio, `0x02` video; `0x00`
reserved / unspecified). Audio and video from the same leaf now derive
independent `(key, nonce_salt)` spaces, so their independent counters can never
collide.

`media_type` is unambiguous **local context** at every seal and open site — it
does not need to travel on the wire:

- **Native seal/open** — the libwebrtc `FrameEncryptorInterface::Encrypt` /
  `FrameDecryptorInterface::Decrypt` already receive `cricket::MediaType`
  (currently `/*ignored*/` at `sframe-bridge.cc:69` / `:193`). Un-ignore it and
  pass it through the C ABI to `participant_key`.
- **Web seal/open** — the SFrame worker already distinguishes
  `RTCEncodedAudioFrame | RTCEncodedVideoFrame`
  (`apps/web/src/calls/sframe-transform-worker.ts:44`); thread the frame kind
  into the key derivation.
- A `FrameDecryptor` is bound to exactly one audio-or-video receiver, so the
  receiver always knows which discriminator to use — the same KID + the local
  media context uniquely selects the key.

The KID stays `= sender_leaf`, so:

- the **SFrame wire header format is unchanged** (KID + CTR bytes identical) —
  no change to `SframeHeader` encode/decode, the `header_decode` fuzz target,
  or ADR-0013's cross-platform AAD (still the RFC 9605 header bytes);
- the **roster mapping is unchanged** — every `leaf == KID` assumption
  (peer-tag resolution, per-sender key cache keyed on `senderLeaf`) holds
  verbatim;
- **only the derived key + salt change**, so the ciphertext of a given
  plaintext changes → the golden test vectors must be regenerated on all
  stacks. This is a flag-day change, acceptable because Lakko is a closed
  ecosystem where all clients ship together (no interop with an external
  SFrame implementation, ADR-0011).

## Alternatives

### Approach B — per-stream KID (rejected)

Encode the media type into the KID itself, e.g. `KID = (leaf << 1) | media_bit`,
and keep `participant_key` keyed on the full composite KID. This is the
RFC-9605-idiomatic shape — the KID alone identifies the key, with no reliance on
out-of-band media context.

Rejected because it is materially more invasive for no security gain over A:

- The **on-wire header KID changes**, so the `SframeHeader` encode path, the
  golden vectors, AND the `header_decode` fuzz corpus all move.
- Every place that assumes `leaf == KID` must switch to `leaf = KID >> 1`:
  the per-sender key cache in the web worker + native shims (keyed on
  `senderLeaf` read from the header), the ADR-0021 peer-tag roster resolution,
  and any diagnostic that prints the leaf. Each is a ripple with its own
  regression surface.
- Under A the media context is *always* available at the decryptor (it is bound
  to one track kind), so A's only theoretical downside — "KID alone does not
  identify the key" — never bites in practice. B pays a real wire + roster cost
  to close a gap that A closes for free.

### Approach C — one shared counter across a participant's streams (rejected)

Keep one key + salt per leaf but share a single monotonic counter between the
audio and video encryptors, so no two frames ever reuse a counter value.

Rejected: it forces shared mutable counter state across two `LakkoFrameEncryptor`
instances that libwebrtc drives from **independent codec threads** (audio and
video encode on separate threads). Coordinating one atomic counter across them
reintroduces exactly the cross-thread contention the per-stream design avoids,
and a single missed increment silently reopens the reuse. It also breaks the
clean "one `ParticipantKey` per stream, counter local to it" model the whole
`CallKeyRing` / overlap-keyring (ADR-0029) is built on.

## Consequences

- **Positive**
  - Closes the HIGH cross-stream nonce reuse — audio and video from one leaf
    can never share `(key, nonce)`. Restores invariant #2 (the SFU can no
    longer recover plaintext from a group video call) and the cryptographic
    strength behind invariant #4.
  - No SFrame wire/header change, no roster ripple — the blast radius is the
    key-derivation function + a `media_type` argument threaded to each seal/open
    site, nothing on the wire.
  - Aligns with RFC 9605 §9 `(key, CTR)` uniqueness (via key uniqueness rather
    than CTR-space partitioning).

- **Negative**
  - **Flag-day golden/key change**: the derived key changes, so a client on the
    new derivation cannot decrypt a peer still on the old one. All five stacks
    (core, wasm, uniffi, native C-ABI, C++ bridge) must ship the new derivation
    together, and every golden vector (`GOLDEN_WIRE_HEX` in the core /
    web-wasm / uniffi / native C-ABI / C++ bridge golden tests) must be
    regenerated in one commit series. Acceptable for a closed ecosystem; there
    is no external SFrame peer to break.
  - The KID no longer uniquely identifies the key on its own — it is
    `KID + local media-type` that does. This is sound because a `FrameDecryptor`
    is always bound to a single track kind, but it is a documented deviation
    from the RFC's "KID identifies the key" wording and must be called out in
    `docs/security/calls.md`.

- **Neutral**
  - `media_type` becomes a required argument of the SFrame FFI (`uniffi` +
    `wasm`) `participant_key` / `sframe_derive_participant_key` surface and of
    the `lakko-sframe-native` C ABI. Existing single-argument call sites are
    updated in lock-step.
  - The `header_decode` fuzz target and the `SframeHeader` codec are untouched
    (the wire header does not change).

## Implementation sprint plan

Grounded in the actual surface. Each sprint ends green before the next starts;
the golden vectors are regenerated once, at the end of sprint 1, and threaded
through 2–4.

1. **Core (`lakko-calls`)** — `participant_key(sender_leaf, media_type)` (a
   `MediaType { Audio, Video }` enum or a 1-byte tag) mixing the discriminator
   into both HKDF `info` strings; `CallKeyRing::participant_key` /
   `try_open_framed_with_overlap` thread it through. **New property test**:
   audio and video from one leaf under one seed yield distinct keys AND distinct
   nonce salts, and sealing both to counter `N` produces no `(key, nonce)`
   collision. Regenerate the core `GOLDEN_WIRE_HEX`.
2. **FFI (`lakko-ffi-uniffi` + `lakko-ffi-wasm`)** — add the `media_type` param
   to `sframe_derive_participant_key` / the `CallKey.participantKey` /
   `CallKeyRing` surface on both bindings; `just regen-bindings` + rebuild wasm;
   update the uniffi + wasm golden tests.
3. **Native C ABI + C++ bridge + shims** — thread `media_type` through
   `lakko-sframe-native` (`lakko_sframe_*` participant-key / open-framed
   entry points); **un-ignore** `cricket::MediaType` in
   `LakkoFrameEncryptor::Encrypt` / `LakkoFrameDecryptor::Decrypt`
   (`sframe-bridge.cc`) and map it to the tag; the iOS Obj-C++ shim + Android
   JNI module pass it; regenerate `GOLDEN_WIRE_HEX` cross-stack golden
   (`tests/golden_cross_stack.rs` + `test_bridge.cc`). `just
   cross-build-sframe-native-ios` + `-android`.
4. **Web worker** — `apps/web/src/calls/sframe-transform-worker.ts` derives the
   per-sender key with the frame kind (`RTCEncodedAudioFrame` → audio,
   `RTCEncodedVideoFrame` → video), for both seal and the per-leaf open cache
   (cache key becomes `(senderLeaf, mediaType)`).
5. **All-client verify** — core property + golden tests; wasm; iOS `xcodebuild`;
   Android `:app:compileDebugKotlin`; the C++ `test_bridge` + the SFU-media
   round-trips.
6. **`lakko-security` re-review + docs reconcile** — confirm the reuse is gone;
   re-word **C-5** in `docs/security/calls.md` (per-`(leaf, media)` key, not
   per-leaf); mark the new **known-gap** entry CLOSED with this ADR; update the
   `lakko-calls` module-map row + the invariant index; add the ADR-0034 row to
   the CLAUDE.md table.

## Open questions

- **Screen-share / a third stream.** If a participant later publishes a second
  video stream (camera + screen-share) they would collide again under one video
  tag. V1 has one camera video stream per participant; a future multi-video
  design needs a per-track discriminator beyond audio/video (e.g. fold the
  track's `mid` or an SFrame CID). Out of scope here; noted so the `media_type`
  tag is defined as extensible (a byte, not a bit).
- **Counter-exhaustion rekey interaction.** The ADR-0029 overlap keyring rekeys
  on an MLS epoch change; the media-type discriminator is orthogonal (it rides
  inside `participant_key`, which the keyring already calls), so no interaction
  is expected — to be confirmed by the sprint-1 property test running against a
  `CallKeyRing` rekey.

## References

- Related ADRs: [0013](0013-sframe-cross-platform-aad.md) (SFrame AAD = the wire
  header bytes — unchanged by this ADR), [0029](0029-mid-call-key-rotation.md)
  (overlap keyring that calls `participant_key`),
  [0032](0032-multi-joiner-call-links.md) (the arc whose review surfaced this).
- RFC 9605 (SFrame) §4.4 (nonce construction), §9 (`(key, CTR)` uniqueness
  requirement).
- RFC 8439 (ChaCha20-Poly1305) — nonce-reuse consequences.
- `docs/security/calls.md` C-5 (to be re-worded).
