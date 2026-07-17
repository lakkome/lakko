<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0035: Per-(leaf, stream) SFrame key derivation via the header KID — close the cross-stream AEAD nonce reuse for ANY number of streams

- **Status:** Proposed (the maintainer accepts). **Supersedes the discriminator of [ADR-0034](0034-sframe-per-media-stream-key.md)** — ADR-0034's Context/finding/threat analysis stand; its per-media-TYPE (Audio/Video) discriminator is replaced by the per-`(leaf, stream)` KID defined here.
- **Date:** 2026-07-17
- **Context:** Phase 3 (calls / SFrame media encryption) — security fix, closing the residual ADR-0034 left open

## Context

[ADR-0034](0034-sframe-per-media-stream-key.md) fixed a **HIGH** cross-stream AEAD nonce-reuse finding: a participant who publishes audio + video arms two `ParticipantKey` / `LakkoFrameEncryptor` instances that, under the original derivation, shared the SAME key and nonce salt (both keyed on `sender_leaf` alone), each with an independent counter — so audio frame `N` and video frame `N` sealed under the identical `(key, nonce)` on different plaintexts (ChaCha20 keystream reuse + Poly1305 tag forgery, recoverable by the zero-trust SFU that relays both streams — invariant #2). ADR-0034 folded a 1-byte **media-type tag** (`Audio = 0x01`, `Video = 0x02`) into both HKDF `info` strings so audio and video derive disjoint spaces.

ADR-0034 itself flagged the residual in its Open Questions ("Screen-share / a third stream"): the tag is per media **kind**, not per **stream**. That residual is a real, reachable break, not a hypothetical:

### The finding (media-type is per-kind, not per-stream)

A single participant can publish **two video streams at once** — the camera track **and** a screen-share track. Both are `Video`, so under ADR-0034 both map to the tag `0x02` and re-derive the **identical** `(key, nonce_salt)` space. Their two independent per-stream counters then reuse the exact `(key, nonce)` pair on different plaintexts — reopening the precise HIGH finding ADR-0034 set out to close, now between the two video streams. ADR-0034 closes the audio-vs-video pair but not the camera-vs-screen-share pair. The discriminator has to be per **stream**, not per **kind**.

### Why a local media-type or a transceiver MID cannot fix it

The receiver must reach the *same* discriminator the sender used, over an **SFU that forwards ciphertext it cannot read** (invariant #2). Two candidate "local" signals both fail:

- **Track kind (audio/video).** A `FrameDecryptor` is bound to one track kind, but track kind cannot tell two **same-kind** streams (camera vs screen-share) apart — exactly the pair that collides. It is strictly weaker than what is needed.
- **Transceiver MID.** A `mid` is negotiated per peer connection. Through an SFU the **sender-side MID and the subscriber-side MID differ** (the SFU re-negotiates its own transceivers downstream), so the two endpoints do not observe the same MID for one logical stream. A MID is not a shared discriminator across the SFU.

The **only** per-stream signal both endpoints agree on through the SFU is what rides *inside the SFrame header* — the SFU forwards the RFC 9605 header verbatim (that is what lets it route by KID without decrypting). So the discriminator must live in the header, and the header field that names the key is the **KID**.

## Decision

**Make the SFrame header KID a per-`(sender_leaf, stream)` key identifier, and derive the participant key from the FULL KID.**

```
KID = (sender_leaf << 8) | stream_index
```

- `stream_index` is a small per-sender ordinal (0 for the first stream, 1..N for additional streams) the **sender** assigns at arm time and keeps for the track's life. 8 bits of stream space (`0..=255`) is ample; the sender leaf sits in the high bits.
- `CallKey::participant_key(kid)` folds the FULL KID (i.e. `(sender_leaf, stream_index)`) into **both** HKDF `info` strings — replacing ADR-0034's `sender_leaf_be ‖ media_type_byte`:
  - key:  `"lakko/sframe/v1/key"   ‖ kid_be`
  - salt: `"lakko/sframe/v1/nonce" ‖ kid_be`
  Distinct KID ⇒ distinct `(key, nonce_salt)` space automatically. Two video streams `(leaf, 1)` and `(leaf, 2)` get disjoint spaces, so the reuse is closed for **any** number of concurrent streams per leaf — not just audio + one video.
- The **receiver** reads the KID from the header (forwarded verbatim by the SFU) and derives `participant_key(KID)` directly — it needs **NO** local media-type or track-kind signal, and no MID. The KID it sees is exactly the KID the sender keyed on.
- Roster / leaf consumers recover `sender_leaf = KID >> 8` (and `stream_index = KID & 0xFF`) with the pure `SframeHeader::leaf_of` / `stream_of` helpers.
- The ADR-0034 `MediaType` discriminator is **removed** from the core derivation (`stream_index` subsumes it). The HKDF domain-separation labels (`lakko/sframe/v1/key`, `.../nonce`) are unchanged.

The HKDF `info` widens from `sender_leaf_be(4) ‖ tag(1)` to `kid_be(8)`, so the derived key + salt — and therefore every ciphertext — change. The header KID also moves out of the inline `0..=7` range for most leaves (a leaf-3 stream-0 frame now carries `KID = 0x0300`, an extended 2-byte KID). Both are captured by regenerating the one canonical golden vector.

### The wire (unchanged codec, new value range)

The `SframeHeader` **codec is untouched** — RFC 9605 §4.3 already encodes an arbitrary `u64` KID with a variable-length extended form, so `(leaf << 8) | stream` fits with no format change beyond the value now commonly exceeding the inline `0..=7` range. `SframeHeader::decode`, its RFC-9605-Appendix-C.1 conformance vectors, and the `header_decode` fuzz target are all unchanged; the header is still the AEAD AAD (ADR-0013). Two pure helpers are added: `SframeHeader::kid_for(leaf, stream) -> Result` (compose, fail-closed on `stream > 255`) and `leaf_of` / `stream_of` (split).

## Invariant now held

For a fixed epoch (fixed `CallKey` seed), the map `(sender_leaf, stream_index) → (key, nonce_salt)` is injective (distinct KID ⇒ distinct HKDF `info` ⇒ distinct key + salt). Combined with the strictly-monotone per-`ParticipantKey` counter (C-5), the `(key, nonce)` pair is unique across **every concurrent stream of every participant**, for any stream count per leaf — restoring RFC 9605 §9 `(key, CTR)` uniqueness and the invariant #2 / #4 guarantees ADR-0034 targeted, now without the two-video-streams hole.

## Alternatives

### A — keep the ADR-0034 per-media-TYPE tag (rejected: the finding)

This is the status quo. Rejected because it is exactly what re-collides for two same-kind streams (camera + screen-share), as the finding shows.

### B — add a second local per-track tag beyond audio/video (rejected)

E.g. keep the KID = `sender_leaf` and fold `(media_type, track_ordinal)` into the derivation locally. Rejected: a purely-local track ordinal is not a signal both endpoints share through the SFU (same failure mode as the MID — the receiver cannot reconstruct which local ordinal the sender used for a given inbound stream). The discriminator has to travel on the wire, which is precisely what putting it in the KID does.

### C — a distinct KID sub-field on the wire separate from the leaf (rejected: needless)

Reserve wire bytes for a standalone stream id alongside the leaf. Rejected: the KID is already a full `u64` on the wire and already the routing/key identifier; packing `(leaf, stream)` into it needs **no** new wire field, only a value-range convention. Adding a parallel field is wire cost for no gain.

## Consequences

- **Positive**
  - Closes the cross-stream nonce reuse for **any** number of concurrent streams per leaf (the ADR-0034 residual), via key uniqueness. Audio + camera + screen-share (and beyond) each key into a disjoint space.
  - The receiver needs **no** out-of-band media context — the KID on the wire is self-sufficient, which is strictly more robust than ADR-0034's "KID + local media type" (it removes ADR-0034's documented deviation from RFC 9605's "the KID identifies the key" wording; here the KID *does* identify the key again).
  - Survives the SFU by construction (the SFU forwards the header verbatim, invariant #2 intact — the SFU still never derives a key).
  - No `SframeHeader` codec change, no `header_decode` fuzz-corpus change, no ADR-0013 AAD change.

- **Negative**
  - **Flag-day wire + golden change.** The header KID value range moves (leaf-`N` stream-`s` now `(N<<8)|s`, typically an extended-KID header) and the derived key changes, so a client on the new derivation cannot interop with a peer still on ADR-0034's. All five SFrame stacks (core, wasm, uniffi, native C-ABI, C++ bridge) plus iOS/Android/web must ship the KID convention + the regenerated golden together, in one commit series. Acceptable pre-GA for a closed ecosystem (ADR-0011) — there is no external SFrame peer to break, and no ADR-0034 build is deployed to real users.
  - Roster/leaf consumers that read the header KID must now split it (`kid >> 8`) instead of using it verbatim as the leaf. This is the one ripple ADR-0034's Approach A was chosen to avoid; it is small (a pure `leaf_of` helper) and is the price of a wire-carried per-stream discriminator, which the finding shows is required.

- **Neutral**
  - The SFrame FFI surface changes shape: `participant_key` takes a `kid: u64` (receiver) with a `participant_key_for(leaf, stream)` convenience (sender); `CallKeyRing` / `GroupCallSession` thread the KID / `stream_index`. Driven in the follow-up FFI + client sprints (see the plan).
  - `stream_index` is a byte, so a leaf can publish up to 256 concurrent streams before exhausting the space — far beyond any realistic call.

## Implementation sprint plan

Grounded in the actual surface. Each sprint ends green before the next; the golden is regenerated once (end of sprint 1) and threaded through 2–4.

1. **Core (`lakko-calls`) — landed in this change.** `SframeHeader::kid_for` / `leaf_of` / `stream_of` + `STREAM_INDEX_BITS` / `MAX_STREAM_INDEX` + `HeaderError::StreamIndexOverflow`; `CallKey::participant_key(kid)` folds the full KID, `participant_key_for(leaf, stream)` convenience; `ParticipantKey` holds the `kid` (accessors `kid()` / `sender_leaf()` / `stream_index()`); `seal_framed` stamps `self.kid`, `open_framed` checks `header.kid == self.kid`; `CallKeyRing` + `GroupCallSession` thread the KID / stream. `MediaType` removed. New property tests (three streams from one leaf — audio + two video — get pairwise-distinct keys/salts/ciphertext; KID compose/split round-trips + rejects `stream > 255`). Core `GOLDEN_WIRE_HEX` regenerated.
2. **FFI (`lakko-ffi-uniffi` + `lakko-ffi-wasm`).** Replace the ADR-0034 `FfiMediaType` / `media_type: u8` with the KID / `(leaf, stream)` surface on both bindings; `just regen-bindings` + rebuild wasm; update the uniffi + wasm golden tests to the new vector.
3. **Native C ABI + C++ bridge + shims.** Replace `media_type: u8` on the `lakko-sframe-native` participant-key / open-framed entry points with the KID; `LakkoFrameEncryptor::Create` / `LakkoFrameDecryptor::Create` take the KID (the sender assigns `stream_index`, the receiver reads it from the header); iOS Obj-C++ shim + Android JNI module thread it; regenerate the `golden_cross_stack.rs` + `test_bridge.cc` vector; `just cross-build-sframe-native-{ios,android}`.
4. **Web worker.** `sframe-transform.ts` / `-worker.ts` compose the KID from `(localLeaf, stream_index)` on seal and read the KID from the header on open (the per-leaf open cache key becomes the KID); the sender assigns a stable stream ordinal per local track.
5. **All-client verify** — core property + golden; wasm; iOS `xcodebuild`; Android `:app:compileDebugKotlin`; the C++ `test_bridge`; the SFU-media round-trips.
6. **`lakko-security` re-review + docs reconcile** — confirm the two-video-stream reuse is gone; re-word **C-5** in `docs/security/calls.md` (per-`(leaf, stream)` key); mark ADR-0034's residual CLOSED by this ADR; update the `lakko-calls` module-map row + the invariant index + the CLAUDE.md ADR table.

## Canonical golden vector (core)

The new cross-stack golden — seal on a fresh `ParticipantKey` (counter → 1) of:

- seed = `[0x42; 32]`
- `sender_leaf = 3`, `stream_index = 0` ⇒ `KID = 0x0300`
- plaintext = `b"lakko sframe golden vector v1"`

```
910300fe2c2af65799594a4c693ff4c9e0d49b0565756ddd7299b0945fc8adb1c9c12fced57c6937f372e6b94bdae40a
```

Header prefix `91 03 00` = extended 2-byte KID `0x0300` (X=1, K=1) + inline CTR `1` (Y=0, C=1); the remaining 45 bytes are the 29-byte plaintext + 16-byte Poly1305 tag. All five stacks pin this exact hex.

## Open questions

- **Simulcast RID interaction.** When a single logical video stream is sent as multiple simulcast layers (RIDs), each layer is a distinct RTP stream but the same *logical* stream. Whether each simulcast layer needs its own `stream_index` (so a layer switch does not reuse a counter across layers) or the layers share one `(leaf, stream)` key with the SFU's layer selection staying opaque to keying is deferred to the simulcast-layer-selection follow-up (already deferred in `services/sfu`). The 8-bit stream space leaves ample room to give each layer its own ordinal if that is the chosen answer.
- **`stream_index` assignment discipline on the sender.** The sender must assign a stable ordinal per local track for the track's life and never re-use a retired ordinal within an epoch on a *different* track without a rekey (re-using an ordinal on a fresh track at counter 0 under the same seed would reuse `(key, nonce)`). In practice each fresh `ParticipantKey` starts at counter 0, so a new track must take a new `stream_index` OR ride an epoch that rotated the seed. The FFI/client sprints own encoding this discipline (assign at arm time, keep for the track's life).
- **Leaf renumber across an MLS commit.** As today, a membership change can renumber leaves; the KID is re-derived per epoch from the current leaf, and the overlap keyring (ADR-0029) already handles in-flight frames across the boundary. No new interaction expected.

## References

- Related ADRs: [0034](0034-sframe-per-media-stream-key.md) (per-media-TYPE discriminator — this ADR supersedes its discriminator), [0013](0013-sframe-cross-platform-aad.md) (SFrame AAD = the header bytes — unchanged), [0029](0029-mid-call-key-rotation.md) (overlap keyring that calls `participant_key`), [0011](0011-rust-sfu.md) (closed ecosystem — flag-day acceptable).
- RFC 9605 (SFrame) §4.3 (header / KID wire format), §4.4 (nonce construction), §9 (`(key, CTR)` uniqueness requirement).
- RFC 8439 (ChaCha20-Poly1305) — nonce-reuse consequences.
- `docs/security/calls.md` C-5 (to be re-worded to per-`(leaf, stream)`).
