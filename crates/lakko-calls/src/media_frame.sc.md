<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-calls/src/media_frame.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if media_frame.rs changes and this file does not. -->

# `media_frame` — sealed media frame layout: codec header in the clear, SFrame header at the end (spec)

**Module:** `crates/lakko-calls/src/media_frame.rs`
**One line:** Define where a sealed media frame keeps its clear codec header, its ciphertext, its SFrame header and its trailer, split such a frame without a key or a codec, and decide how many bytes a sender leaves clear per codec.

## Why this module exists

A receiver whose RTP stack does not carry the generic frame descriptor finds a key frame by reading the codec's own header at the start of the frame payload. Under the ADR-0013 layout the payload started with the SFrame header, so to such a receiver no frame ever looked like a key frame and its decoder never started. ADR-0071 moves the SFrame header to the end of the frame and leaves a short, codec-specific prefix of the encoded frame in the clear at the start.

That this fixes decoding in the browsers concerned is an inference from call logs. This module has only been run under host tests.

The keys, the nonce and the counter are untouched (ADR-0035, ADR-0069). The seal and the open are on `sframe::ParticipantKey` and `CallKeyRing`; this module owns the layout they run over.

## Wire layout

```
clear_prefix || ciphertext_and_tag || sframe_header || trailer
0..=16 B        >= 16 B               1..=17 B         3 B

trailer = clear_prefix_len (1 B) || sframe_header_len (1 B) || version (1 B) = 0x01
```

- `clear_prefix` — the first bytes of the encoded frame, unchanged.
- `ciphertext_and_tag` — ChaCha20-Poly1305 over the rest of the frame; the 16-byte tag is its end.
- `sframe_header` — the RFC 9605 section 4.3 header of `header.rs` (KID, CTR), byte for byte.
- `trailer` — fixed size, last in the frame, so a receiver reads it first.

AEAD additional data: `sframe_header || clear_prefix || trailer`, taken from the wire bytes. Plaintext: the frame after the clear prefix.

## Public contract

- `MediaFrameParts::split(wire)` — read the trailer and return the four regions as borrowed slices plus the decoded `SframeHeader`. Needs no key and no codec. Total on any input. Checks, in order: minimum length (20), version, clear length <= 16, header length in 1..=17, total length >= what the two lengths imply, that the header bytes decode, and that the header decodes to exactly the declared length. Every slice is taken only after the check that covers it. The four regions always add up to the whole input.
- `MediaFrameParts::aad()` / `media_frame_aad(header, clear, trailer)` — the additional data, in the one order both sides use.
- `media_frame_trailer(clear_len, header_len)` — build the trailer; refuses any length `split` would refuse, so a sender cannot emit a frame its peers reject.
- `MediaCodec { Other = 0, Vp8 = 1, Opus = 2 }` — what the sender states. `from_mime` maps `video/VP8` and `audio/opus` (any case), everything else is `Other`; `from_u8` maps the frozen FFI values, an unknown value is `Other`.
- `vp8_is_key_frame(frame)` — the low bit of the first byte is 0 for a key frame; `None` for an empty frame.
- `clear_prefix_len(codec, frame)` — VP8: 10 for a key frame (3-byte frame tag, 3-byte start code, 4 bytes of width and height), 3 for any other frame (the frame tag). Opus and `Other`: 0. Never more than `frame.len()`.
- Constants: `MEDIA_FRAME_VERSION` (0x01), `MEDIA_FRAME_TRAILER_LEN` (3), `MAX_CLEAR_PREFIX_LEN` (16; chosen as the power of two above VP8's 10, not measured), `MEDIA_FRAME_TAG_LEN` (16), `MIN_MEDIA_FRAME_LEN` (20), `MAX_MEDIA_FRAME_OVERHEAD` (36), `VP8_KEY_FRAME_CLEAR_LEN` (10), `VP8_DELTA_FRAME_CLEAR_LEN` (3).

## The sender's rule

- The receiver is codec-agnostic: only the sender chooses a prefix, and the trailer tells the receiver what was chosen.
- For VP8 the frame's own tag bit decides whether it is a key frame, not what the platform reported about the frame. The prefix exists for a receiver that reads these bytes, so the bytes are what it has to agree with: a frame the tag calls a key frame gets the 10 bytes its reader will parse, and a frame the tag calls a delta frame exposes 3 whatever else was claimed. A client may compare the platform's flag against the tag and report a disagreement; it does not change the prefix.
- An unknown or undetermined codec is `Other`: nothing is clear. Such a frame is as secret as under ADR-0013 and will not decode on a receiver without the frame descriptor. VP8 is never assumed.
- A frame shorter than its codec's prefix is left clear whole; its ciphertext is the tag over an empty plaintext.

## Invariants upheld

- **Every byte that is not ciphertext is authenticated.** The clear prefix, the SFrame header and the trailer are the additional data. Tests `any_single_bit_flip_is_refused` (every bit of three sealed frames), `each_region_is_authenticated`, `a_trailer_moved_onto_another_frames_body_is_refused`.
- **The clear prefix is the first bytes of the input and of the output.** Tests `vp8_key_frame_round_trips_with_ten_clear_bytes`, `vp8_delta_frame_round_trips_with_three_clear_bytes`, `a_sealed_vp8_frame_starts_with_the_vp8_header`, proptest `seal_then_open_is_the_identity`.
- **At most 16 bytes are ever clear**, enforced on seal (refused, counter unmoved) and on split.
- **The two layouts never open as each other.** The additional data of the header-first layout is an SFrame header alone; here it is a header followed by at least the 3-byte trailer, so the two are never equal. Tests `the_header_first_layout_is_refused_by_the_new_open` (2000 frames, so tag bytes that happen to pass the trailer checks are covered), `the_new_layout_is_refused_by_the_header_first_open`, `the_two_layouts_never_share_additional_data`.
- **C-5 / ADR-0035 / ADR-0069 unchanged:** one counter value per sealed frame, distinct keys per `(leaf, stream)`. Tests `counters_advance_once_per_sealed_frame`, `counter_exhaustion_seals_nothing`, `two_streams_of_one_leaf_seal_under_distinct_keys`.
- **ADR-0029 overlap unchanged.** Tests `the_ring_opens_old_and_new_epoch_frames_inside_the_window`, `the_ring_refuses_garbage_and_a_foreign_seed`.
- **Invariant #4:** a frame that does not split or does not authenticate yields an error and none of its bytes.
- **Pinned bytes.** `GOLDEN_MEDIA_FRAME_HEX` (seed `0x42` x 32, leaf 3, stream 1, counter 1, a VP8 key frame) is sealed here with the counter forced by the test-only setter and opened by the key and by the ring; the wasm surface and the C ABI open the same bytes.

## Failure modes

`MediaFrameError`, every variant a refusal:

- `TooShort { needed, had }` — under 20 bytes, or shorter than the trailer's lengths imply.
- `UnknownVersion(v)` — the last byte is not `0x01`. A header-first frame ends in a tag byte and usually stops here; when its last bytes happen to pass every check, the AEAD refuses it.
- `ClearPrefixTooLong { len, max }` — a trailer claiming, or a sender asking for, more than 16.
- `BadHeaderLength(n)` — header length 0 or above 17.
- `Header(HeaderError)` — the bytes at the header position do not decode.
- `HeaderLengthMismatch { declared, decoded }` — the header decodes to another length than declared, so the header region carries no slack bytes.

No panics: no `unwrap` / `expect` / indexing without a preceding bound in the module body; slices are taken with `get`. Lengths are one byte each plus small constants, so the sums cannot overflow.

## What this module does NOT do

- **No keys, no AEAD.** It never sees a key; `sframe.rs` runs the seal and the open over the regions defined here.
- **No codec parsing beyond one bit.** It reads the VP8 frame tag's low bit and nothing else. It does not validate the VP8 start code or dimensions, and it has no rule for H.264, VP9 or AV1: those are `Other`.
- **No decision for the receiver.** A receiver never states a codec and never trusts the clear prefix for anything before the open succeeds.
- **No replay protection**, like the rest of the crate: opening is stateless.

## What an SFU sees that it did not

For a VP8 frame: whether it is a key frame, the other fields of the 3-byte frame tag (version, show-frame flag, first-partition size), and for a key frame the width and height with their scale bits. It cannot change any of them unnoticed. Picture data, audio and every other codec stay sealed. Before ADR-0071 a sender that negotiated the generic frame descriptor already told the SFU the frame type and resolution through it; a sender that did not negotiate it told the SFU neither.

## Attacker exposure

`MediaFrameParts::split` runs on bytes from an untrusted SFU or peer before anything is authenticated: two attacker-chosen length bytes, a version byte, and the header bytes they point at. The bound is the checks listed under the public contract. Fuzz target `media_frame_open` (`crates/lakko-calls/fuzz/fuzz_targets/media_frame_open.rs`; compile-checked, not run) drives the split, the ring's open with and without a previous key, and the seal-then-open round trip on arbitrary input. Proptests `open_never_panics_and_never_accepts_arbitrary_bytes`, `open_never_panics_behind_a_plausible_trailer` and test `every_truncation_of_a_sealed_frame_is_refused` cover the same in `cargo test`.

The layout departs from RFC 9605's on-the-wire shape (header first, nothing clear). It is Lakko's own and part of what the G-39 external review has to read.

## References

- ADR-0071 (this layout), ADR-0013 (the header-first layout and the header-as-AAD rule it amends), ADR-0035 (per-`(leaf, stream)` KID), ADR-0069 (64-bit counter, random start), ADR-0029 (the overlap ring)
- RFC 9605 section 4.3 (the SFrame header), RFC 6386 section 9.1 (the VP8 frame header)
- `docs/security/calls.md` C-5, C-6, C-55; security invariants #2 and #4
- Sibling specs: `sframe.sc.md`, `call_key_ring.sc.md`, `header.sc.md`
