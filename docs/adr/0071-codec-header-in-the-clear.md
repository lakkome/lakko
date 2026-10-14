<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0071: The codec's frame header stays in the clear; the SFrame header moves to the end of the frame

- **Status:** Proposed (2026-10-07; the maintainer decided to do this). Phase 1 is built: the Rust core, the wasm FFI, new entry points on the C ABI, and the web client. Host-tested only. Not run in any browser, not seen on a call. Phase 2 (the native bridges, iOS, Android) is built since 2026-10-11 (see the dated note below): host-tested, never run on a device, so that a web client and a native client open each other's media is not yet observed.
- **Date:** 2026-10-07
- **Context:** call media, Phase 3. Amends [ADR-0013](0013-sframe-cross-platform-aad.md) (the frame layout and what the AEAD additional data covers). Leaves [ADR-0035](0035-sframe-per-stream-kid-key.md) (one key per `(leaf, stream)` through the KID), [ADR-0069](0069-sframe-random-counter-start.md) (64-bit counter with a random start) and [ADR-0029](0029-mid-call-key-rotation.md) (the overlap key ring) unchanged in keying. Security invariants #2, #3 and #4; C-5, C-6, C-55.

## Context

Two live calls on the test stack (2026-10-06 and 2026-10-07). Observed:

- Chrome to Chrome video works.
- Firefox shows no remote video, and nobody sees Firefox's video.
- Firefox's console: `sframe receive opened kid=4: opened=1500 badHeader=0 openFailed=0`. The frames decrypt.
- Every video receiver in Firefox reports `pkts>0 decoded=0 pli=10` per two seconds. The decoder never starts and keeps asking for a key frame.
- At call start Firefox logs `SfuCallSession: 3 video transceiver(s) cannot negotiate the generic frame descriptor`.
- The SFU counted `key_frames=0` on every stream Firefox published.
- The negotiated video codec in every log line is VP8, the audio codec Opus.

The reading, which is an **inference from these logs and has not been seen fixed**: a receiver with no generic frame descriptor learns that a frame is a key frame by reading the codec's own header at the start of the frame payload. A sealed frame started with the RFC 9605 SFrame header, whose bytes since ADR-0069 are a KID and a random 62-bit counter. So to such a receiver no frame ever looks like a key frame, and a decoder that has seen no key frame decodes nothing. Chrome does not depend on those bytes because it negotiates the frame descriptor, which carries the frame type outside the payload. The same reading explains the SFU's `key_frames=0`: it counts key frames from the descriptor, and Firefox sends none.

Firefox and Safari have to work without depending on a header extension only one engine implements (the maintainer's rule that the client must not rest on one browser's behaviour).

## Decision

A sealed media frame keeps a short, codec-specific prefix of the encoded frame in the clear at its start, and carries the SFrame header at its end.

### Layout

```
clear_prefix || ciphertext_and_tag || sframe_header || trailer
0..=16 B        >= 16 B               1..=17 B         3 B

trailer = clear_prefix_len (1 B) || sframe_header_len (1 B) || version (1 B) = 0x01
```

- `clear_prefix`: the first bytes of the encoded frame exactly as the encoder produced them.
- `ciphertext_and_tag`: ChaCha20-Poly1305 over the rest of the frame. Same key, same nonce (`nonce_salt XOR counter`), same counter rules as before.
- `sframe_header`: the RFC 9605 section 4.3 header (KID, CTR), the unchanged codec of `lakko_calls::header`.
- `trailer`: fixed size and last, so a receiver reads it first and splits the frame without knowing the codec. The version byte is last.

**Additional data:** `sframe_header || clear_prefix || trailer`, taken from the wire bytes. Every byte that is not ciphertext is authenticated. **Plaintext:** the frame after the clear prefix.

The split of the additional data is unambiguous: its last three bytes are the trailer, which states the other two lengths. It can never equal the additional data of the header-first layout, which is an SFrame header alone and exactly as long as its own first byte says, where this one is at least three bytes longer than the header it starts with. A frame of one layout therefore never authenticates as the other, under the same key and counter.

### Clear prefix, chosen by the sender

| Frame | Clear bytes |
|---|---|
| VP8 key frame | 10: the 3-byte frame tag, the 3-byte start code, 4 bytes of width and height |
| VP8, any other frame | 3: the frame tag |
| Opus | 0 |
| any other or undetermined codec | 0 |

- Never more than the frame holds. A frame shorter than its prefix is left clear whole and its ciphertext is the tag over an empty plaintext.
- Never more than 16 bytes (`MAX_CLEAR_PREFIX_LEN`). A sender asking for more is refused before any state changes; a receiver refuses a trailer claiming more. The bound is the power of two above VP8's 10, chosen and not measured.
- **Opus keeps nothing clear.** Nothing found in this repository, and nothing in what is known here of the web platform, has a receiver read the Opus TOC byte before the transform returns the frame. This was not tested in a browser.
- **An unknown codec keeps nothing clear.** Such a frame is as secret as under ADR-0013, and it will not decode on a receiver without the frame descriptor. H.264, VP9 and AV1 have no rule; they are "other".

### Which frame is a key frame

For VP8 the frame's own tag decides: the low bit of the first byte is 0 for a key frame. The platform's word about the frame (`RTCEncodedVideoFrame.type` on the web) is compared against it, and a disagreement is logged; **the tag wins.** The prefix exists for a receiver that reads these bytes, so the bytes are what the prefix has to agree with. A frame the tag calls a key frame is one its reader will parse 10 bytes of, whatever the platform said. A frame the tag calls a delta frame is one its reader parses 3 bytes of, and exposing 7 more would serve nobody. It is also the only rule the native side can follow: libwebrtc's `FrameEncryptorInterface::Encrypt` is given a media type, an SSRC, additional data and the frame bytes, and no key-frame flag.

### The receiver

Reads the trailer; checks the minimum length, the version, both lengths against their bounds and against the frame length, that the header bytes decode, and that they decode to exactly the declared length, all before taking a slice; selects the key by the header's KID; opens; returns `clear_prefix || plaintext`, which is the encoded frame as the sender's encoder produced it. Anything inconsistent is a refusal that returns none of the input: no panic, no pass-through (invariant #4). The receiver never states a codec and trusts no clear byte before the open succeeds.

### One implementation

`lakko_calls::media_frame` owns the layout (`MediaFrameParts::split`, the trailer, the additional data) and the sender's rule (`MediaCodec`, `clear_prefix_len`, `vp8_is_key_frame`). `ParticipantKey::seal_media_frame` / `seal_media_frame_for_codec` / `open_media_frame` and `CallKeyRing::try_open_media_frame_with_overlap` seal and open; the ring splits once and tries its current and previous keys over the same parts. Exposed on wasm as `sealMediaFrame`, `sealMediaFrameWithPrefix`, `openMediaFrame`, `openMediaFrameWithOverlap`, `parseMediaFrame`, `MediaCodec`, `mediaCodecFromMime`, `mediaClearPrefixLen`, `vp8IsKeyFrame`, and on the C ABI as `lakko_sframe_seal_media_frame`, `lakko_sframe_call_key_ring_open_media_frame_with_overlap`, `lakko_sframe_media_clear_prefix_len`.

### The header-first functions stay, unused by the web

This is a flag day for call media and there is no deployment, so nothing dual-accepts on the receive path: the new open refuses a header-first frame and the old open refuses a new one. `seal_framed` / `open_framed` and their FFI and C ABI wrappers are kept, unchanged in behaviour, because the native bridges and the SFU's synthetic-client test still call them and neither is part of phase 1. The web client no longer calls them. They are removed when phase 2 lands.

### How the web sender learns the codec

In this order, per frame, in the transform worker (`apps/web/src/calls/sframe-codec.ts`):

1. the frame's own `getMetadata().mimeType`, where the browser gives one;
2. else `getMetadata().payloadType`, looked up in the payload type to MIME type table of `RTCRtpSender.getParameters().codecs`, which the main thread posts to the worker after the SFU's answer is applied, at once for a sender attached mid-call, and whenever the worker asks;
3. else unknown.

**Unknown is not guessed to be VP8.** The frame is sealed with nothing clear, one line is logged (then every 250th), and the worker asks the main thread for the table. The cost is that such frames do not decode on a receiver without the descriptor; the gain is that a frame of a codec with no rule never has 10 bytes of its content exposed by mistake.

What each browser gives, as far as the specifications and this code show, **none of them run**: `payloadType` is in the W3C encoded-frame metadata dictionary for audio and video; `mimeType` was added to that dictionary later and a given browser version may not have it; `getParameters().codecs` is filled after negotiation. Which of the three Chrome, Firefox and Safari populate is not known here. The resolver reads all three defensively and works from whichever is present.

## Security

Stated plainly.

**What an SFU learns that it did not before**, for VP8 video only:

- Whether each frame is a key frame, for a publisher that sends no frame descriptor. (A publisher that sends the descriptor already told the SFU this, and its resolution, through it.)
- The rest of the 3-byte frame tag of every frame: the VP8 version, the show-frame flag and the size of the first partition. The first-partition size is a number about the frame's content that was not visible before.
- The width and height of each key frame, with their scaling bits, for a publisher that sends no descriptor.

**What it already saw:** every frame's size and timing, the RTP headers and header extensions, the KID and the counter, and the frame descriptor where one is sent.

**What stays as it was:** the picture and the sound are encrypted and authenticated. Audio, and video of any codec other than VP8, expose nothing new. Keys, nonces and counters are those of ADR-0035 and ADR-0069; no counter is consumed differently.

**The clear bytes are authenticated.** They are in the additional data with the header and the trailer. An SFU that turns a key frame into a delta frame for a reader, changes the advertised size, moves the boundary between clear and sealed bytes in the trailer, or swaps a header and trailer onto another frame's body makes the open fail and the frame is dropped. A receiver's RTP stack does read the clear bytes before the open (that is their purpose), so an SFU can still make a receiver's depacketizer believe something false about a frame it then drops; it could already drop or corrupt any frame.

**New attack surface:** the trailer parser. Two length bytes and a version byte chosen by an untrusted SFU or peer are read before anything is authenticated and decide where slices are taken. The parser is bounded and total: the fuzz target `media_frame_open` (compile-checked, not run), three proptests and an every-truncation test cover it.

**A sender-side risk the layout adds:** a sender that states the wrong codec for a frame exposes up to 10 bytes of it. The 16-byte bound caps that, and an undetermined codec exposes nothing.

**It departs from RFC 9605.** RFC 9605 puts the SFrame header first and leaves nothing of the media in the clear. The header codec, the key schedule and the nonce are still the RFC's and Lakko's as before; the on-the-wire shape of a frame is now Lakko's own. The interoperability with other RFC 9605 stacks that the 2026-07 update of ADR-0013 noted no longer holds for frames in this layout.

**It grows the G-39 external-review surface:** a new parser on unauthenticated input, a new additional-data construction, and a deliberate plaintext leak whose bound a reviewer has to check.

## Alternatives

- **Leave the layout and require the frame descriptor.** That is the present state. Firefox logged that it cannot negotiate it (what Safari does was not observed); the result is the call described above.
- **Have the SFU write a frame descriptor for publishers that send none.** The SFU cannot tell a key frame from sealed bytes, so it would need the same clear header anyway, and receivers without the extension still could not read what it wrote.
- **Clear prefix with the SFrame header still first.** The codec header would not be at the start of the payload, which is where a depacketizer reads it.
- **SFrame header after the clear prefix, no trailer.** The receiver would have to know the codec, and whether the frame is a key frame, to find the header; a wrong guess reads sealed bytes as a header. The trailer costs 3 bytes a frame and removes the guess.
- **Encrypt per packet instead of per frame (RFC 9605's other mode).** A different integration with the RTP stack on every platform, and no browser API for it.
- **Guess VP8 when the codec is unknown.** Exposes 10 bytes of a frame of some other codec.

## Consequences

- **Positive:** a VP8 receiver that reads the codec header finds key frames in sealed frames. Firefox and Safari are expected to decode; not observed.
- **Positive:** an SFU can count and react to key frames of a publisher that sends no descriptor by reading the clear tag. Nothing in `services/sfu` does so yet; it was not changed.
- **Negative:** the SFU learns the items listed under Security.
- **Negative:** 3 more bytes per frame (the trailer). The header and tag are as before.
- **Negative:** until phase 2, web and native do not interoperate on media in either direction. The web client seals and opens only this layout; the native bridges seal and open only the header-first one. Each side drops the other's frames (they fail to parse or to authenticate); nothing is passed through unsealed.
- **Negative:** no interoperability with an external RFC 9605 stack for these frames.
- **Neutral:** the cross-stack golden vector of ADR-0013 still pins the header-first functions. A second golden vector pins this layout (seed `0x42` x 32, leaf 3, stream 1, counter 1, a VP8 key frame) in the core, the wasm FFI, the C ABI test and the web suite.
- **Neutral:** only VP8 has a rule. A call that negotiates H.264 or VP9 behaves as before this ADR.

## Phase 2: what the native side has to change

Not built. Listed so the work is sized from the code and not from this summary; the bridge sources were not read for this ADR beyond what `CLAUDE.md` and the C ABI say about them.

1. `apps/shared/sframe-bridge` `LakkoFrameEncryptor::Encrypt`: call `lakko_sframe_media_clear_prefix_len(codec, frame)` then `lakko_sframe_seal_media_frame` in place of `lakko_sframe_seal_framed`. `GetMaxCiphertextByteSize` must allow the frame plus 36 bytes (tag 16, header up to 17, trailer 3).
2. **Where the codec comes from.** `FrameEncryptorInterface::Encrypt` receives a `cricket::MediaType`, an SSRC, additional data and the frame. Audio is known from the media type (prefix 0). For video the interface names no codec, so the Swift / Kotlin side has to tell the encryptor which codec its sender negotiated (the first entry of the sender's parameters after negotiation), through a setter like the existing `Rekey`, and again when a renegotiation changes it. Until it is told, the encryptor seals as "other" (nothing clear).
3. **Where the key-frame flag comes from.** Nowhere in the interface; the core reads the VP8 frame tag, which is the rule on the web too.
4. `LakkoFrameDecryptor::Decrypt`: call `lakko_sframe_call_key_ring_open_media_frame_with_overlap` in place of `..._open_framed_with_overlap`. It needs no codec. The output is at most the input's length.
5. Rebuild the static libraries and the Android `.so` so the new symbols and the regenerated header exist; add the new golden vector to `test_bridge.cc` and the iOS / Android golden tests; then delete the header-first functions from the core, both FFI crates, the C ABI and the SFU's test.
6. UniFFI was not extended: per-frame media on native goes through the C ABI.

## Note, 2026-10-11: phase 2 as built, and what phase 1 has shown since

Written from the tree at `5b7c89a2`. The Status line above and the section before this one describe 2026-10-07 and are left as they were; whether this ADR is Accepted is the maintainer's call.

**Phase 1 on live calls.** The commit record of 2026-10-09 states that video between Chrome, Firefox and Safari was seen on live calls on the test stack. That answers the first open question below for those three browsers as far as a call shows it; this document holds no log or counter from those calls beyond the two calls in Context. The web client also gained a second way to reach the encoded frames, `createEncodedStreams()` where `RTCRtpScriptTransform` is absent (`apps/web/src/calls/sframe-transport.ts`); both end in the same seal and open (`sframe-worker-core.ts`), so the layout is the same on either.

**Phase 2, by the numbered list above:**

1. Done. `LakkoFrameEncryptor::Encrypt` calls `lakko_sframe_media_clear_prefix_len` and then `lakko_sframe_seal_media_frame`; `GetMaxCiphertextByteSize` returns the frame plus 36.
2. Done. `LakkoFrameEncryptor::SetCodec(uint8_t)` stores the codec in a `std::atomic<uint8_t>`, so it may be called from the signaling thread while the codec thread is inside `Encrypt`; a frame sealed during the call is sealed under one value or the other. A fresh encryptor holds `OTHER` (nothing clear). The C ABI gained `LAKKO_SFRAME_MEDIA_CODEC_OTHER` / `_VP8` / `_OPUS` and `LAKKO_SFRAME_MAX_CLEAR_PREFIX_LEN` as literals tied to the core by compile-time assertions; no function of the C ABI changed. iOS: `LakkoSFrameAttach setCodec:forSender:` / `setCodecName:forSender:` / `codecForName:`. Android: `SFrameMediaCodec`, `LakkoFrameEncryptor.setCodec` / `setCodecName`, JNI `nativeSetCodec` / `nativeCodecFromName`. Both clients call them from `syncSenderCodecs` on the peer connection wrapper, which reads the first entry of each sender's negotiated codecs, after every applied answer or renegotiation and after an encryptor is attached again.
3. As decided: nothing in the bridge states a key frame; the core reads the VP8 frame tag.
4. Done. `LakkoFrameDecryptor::Decrypt` calls `lakko_sframe_call_key_ring_open_media_frame_with_overlap` and takes no codec. An empty frame is answered as nothing to decrypt, not as a failed decryption.
5. **Half done.** The Android `libsframe_jni.so` for `arm64-v8a` and `x86_64` is rebuilt and committed. `test_bridge.cc` holds the ADR-0071 golden vector and fifteen tests, all in this layout, one of which asserts that the header-first golden vector and a freshly sealed header-first frame do not open on the media path. The iOS and Android golden tests were **not** given the new vector: they call UniFFI, which has no entry point for this layout, and now say in a comment that they pin the header-first functions only. **The header-first functions were not deleted**: `seal_framed` / `open_framed` are still in `lakko-calls`, both FFI crates and the C ABI, and `services/sfu/tests/synthetic_client_e2e.rs`, the golden tests and `crates/lakko-sframe-native/tests/golden_cross_stack.rs` still call them. No client's media path does.
6. Unchanged: UniFFI was not extended.

**What this changes in Consequences.** The line "until phase 2, web and native do not interoperate on media" describes the state before `5b7c89a2`. By construction a web client and a native client now seal and open the same layout through the same core. That has not been observed: no web-to-native call, and no native-to-native call, has been made since.

**Not run.** Nothing native was run on a device, an emulator or a call. Whether the bridge's host test binary, the iOS build and the Android build were run for the port is not recorded in this document; this note was written by reading the sources and ran none of them. The fuzz target `media_frame_open` is still compile-checked only.

**Still open from the list below, now with a native side:** the third question (a native receiver and a publisher that sends no frame descriptor, which is Firefox) is untested in both directions.

## Open questions

- **Does Firefox's receive path start decoding, and Safari's?** Not observable without a browser. The expectation rests on the inference in Context.
- **Does a depacketizer need anything else of the payload?** For VP8 the belief, from memory of libwebrtc's VP8 depacketizer and not checked against its source for this ADR, is that it reads the key-frame bit and, for a key frame, the width and height at bytes 6 to 9. Whether Firefox's and Safari's builds read anything further is unknown.
- **Native receivers and publishers that send no descriptor.** The repository's own comments say a native phone cannot find key frames without the frame descriptor. If libwebrtc's frame-decryptor path refuses frames that carry no descriptor at all (a recollection, not verified), then Firefox to native stays broken after phase 2 unless the SFU writes a descriptor for such publishers, which the clear frame tag would now let it do.
- **Does the Opus path need the TOC byte anywhere?** Believed not; untested.
- **H.264 and VP9.** No rule. A clear-prefix rule for H.264 has to follow NAL unit structure and is a different design.
- **The first key frame of a sender whose codec table has not reached the worker** is sealed with nothing clear when the browser's frame metadata names no codec either. A receiver without the descriptor then waits for the next key frame, which its own key-frame requests bring. Whether this happens in practice depends on what each browser's metadata carries.

## References

- [ADR-0013](0013-sframe-cross-platform-aad.md), [ADR-0029](0029-mid-call-key-rotation.md), [ADR-0035](0035-sframe-per-stream-kid-key.md), [ADR-0069](0069-sframe-random-counter-start.md), [ADR-0046](0046-external-crypto-review.md) (G-39)
- RFC 9605 (SFrame), section 4.3 (the header); RFC 6386 section 9.1 (the VP8 uncompressed data chunk: frame tag, start code, dimensions)
- `crates/lakko-calls/src/media_frame.rs` and its spec `media_frame.sc.md`; `docs/security/calls.md` C-5, C-6, C-55
- No precedent from another product is cited: none was read for this ADR.
