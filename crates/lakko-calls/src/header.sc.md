<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-calls/src/header.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if header.rs changes and this file does not. -->

# `header` — SFrame frame-header codec (spec)

**Module:** `crates/lakko-calls/src/header.rs`
**One line:** Encode / decode the RFC 9605 §4.3 SFrame header — the `(KID, CTR)` pair that prefixes every sealed media frame and doubles as the AEAD AAD.

## Why this module exists

Every SFrame-sealed media frame carries a short variable-length header naming which key (`KID`) and which frame counter (`CTR`) sealed it. This module is the sole encoder/decoder of that header. The header is load-bearing twice over: (1) the SFU reads the `KID` bytes to route a frame by participant **without decrypting it**, and (2) the header bytes ARE the AEAD AAD (ADR-0013), so any tampering with the header fails the frame's authentication downstream. The `KID` is also the **per-stream key discriminator** (ADR-0035): it is the only signal both endpoints share through the SFU (which forwards the header verbatim), so the `(sender_leaf, stream_index)` that names a stream's key rides packed inside it.

## Public contract

- `SframeHeader { kid: u64, ctr: u64 }` — the parsed header. Both fields are 64-bit on the wire per RFC 9605, even though today's call-keying layer (`sframe::ParticipantKey`) only uses a `u32` per-leaf counter. The extra width is deliberate headroom (and is what lets the composite `(leaf << 8) | stream` KID fit) and must not be narrowed — that would be a wire break.
- `encode(&self) -> Vec<u8>` — produces the on-wire bytes. Infallible. Length is at most `MAX_HEADER_LEN` (= 17 = 1 config byte + 8 KID + 8 CTR). Values `<= 7` are packed inline in the config byte; larger values set the extended flag and append big-endian bytes.
- `decode(bytes: &[u8]) -> Result<(SframeHeader, usize), HeaderError>` — parses the header and returns it plus the number of bytes consumed, so the caller slices `bytes[consumed..]` to reach the ciphertext without a second length probe.
- `MAX_HEADER_LEN` — the ceiling, for callers pre-allocating scratch.
- **KID composition (ADR-0035):** `kid_for(sender_leaf, stream_index) -> Result<u64, HeaderError>` packs `(sender_leaf << STREAM_INDEX_BITS) | stream_index` — the sole discriminator that survives an SFU. Fails closed with `StreamIndexOverflow` when `stream_index > MAX_STREAM_INDEX` (255), so an out-of-range ordinal can never silently truncate into another leaf's space. `leaf_of(kid) -> u32` (`kid >> 8`) and `stream_of(kid) -> u8` (`kid & 0xFF`) split it back for roster/diagnostic use. `STREAM_INDEX_BITS = 8` / `MAX_STREAM_INDEX = 255` pin the split. These are pure integer helpers — the *derivation* that binds the KID to a key lives in `sframe`; the *codec* here just carries the value.

## Wire layout (RFC 9605 §4.3)

Config byte, big-endian bit order: `X KKK Y CCC`.
- `X` (bit 7): extended-KID flag. `X=0` → `KKK` holds the KID inline (`0..=7`). `X=1` → `KKK` holds `KID_len − 1`, and that many big-endian KID bytes follow.
- `Y` (bit 3): extended-CTR flag, same scheme for `CCC` / the CTR, whose bytes follow the KID bytes.

So `(kid=0, ctr=0)` = `0x00`; `(kid=0xff, ctr=0)` = `0x80 0xff`.

## Invariants upheld

- **Encode↔decode round-trips** for every `(kid, ctr)` in the full `u64 × u64` space, and `consumed` on decode equals the encoded length.
- **RFC 9605 conformance is byte-exact**, not merely self-consistent: the encoder is pinned against RFC 9605 Appendix C.1's published vectors, so a header produced here is byte-identical to any conformant external stack (mlspp, Cisco libsframe, a browser-native transform) — the property that lets a Lakko frame cross stacks (C-55 / ADR-0013).
- **Cross-stack agreement:** the same golden vector is pinned by the Rust core, the `lakko-sframe-native` C ABI, the web wasm worker, iOS, and Android — all five agree byte-for-byte.
- **Bounded output:** an encoded header never exceeds `MAX_HEADER_LEN`; the `as u8` casts are provably exact (`bytes_needed` returns `1..=8`; inline branches are gated on `<= 7`).

## Failure modes

- `HeaderError::Truncated { needed, had }` — the decode error. Returned when the buffer is too short for the layout the config byte declares (empty buffer, or an extended KID/CTR whose trailing bytes are missing). Decode never panics and never over-reads: every extended-field read is length-checked before the slice copy.
- `HeaderError::StreamIndexOverflow { stream_index, max }` — `kid_for` refused a stream ordinal `> MAX_STREAM_INDEX` (ADR-0035). Fail-closed compose; never a truncated KID. Not a decode error — `decode` itself only ever returns `Truncated`.

## What this module explicitly does NOT do (do not add it here)

- **No cryptography.** It does not seal, open, derive keys, or authenticate. It only packs/unpacks integers. The AEAD binding of these bytes as AAD lives in `sframe::ParticipantKey::{seal_framed, open_framed}`.
- **No counter-monotonicity enforcement.** `CTR` monotonicity (invariant C-5) is the caller's job (`sframe` / `ParticipantKey`), not the codec's — `decode` will happily parse any counter value.
- **No allocation beyond `MAX_HEADER_LEN`.** There is no attacker-controlled length that drives an unbounded allocation.

## Attacker exposure

`decode` runs on bytes handed over by an **untrusted SFU or peer** — via the `lakko-sframe-native` C ABI on iOS/Android or the wasm worker on web — BEFORE the AEAD authenticates the frame. The config byte and the variable-length extended fields are therefore fully attacker-controlled. The bound is: parse must stay panic-free, allocation-bounded, and never over-read. Fuzzed by `crates/lakko-calls/fuzz/fuzz_targets/header_decode.rs` (asserts no-panic + `consumed <= buf.len()` + `<= MAX_HEADER_LEN` + round-trip stability).

## References

- ADR-0013 (cross-platform SFrame AEAD AAD = the header bytes), ADR-0035 (the KID is the per-`(leaf, stream)` key discriminator — `kid_for` / `leaf_of` / `stream_of`)
- Invariants C-5 (counter monotonicity — enforced elsewhere), C-55 (cross-platform AAD)
- Tests: `rfc_9605_appendix_c1_header_vectors`, `pinned_wire_layouts`, `first_byte_bit_packing_pinned`, the `decode_rejects_*` truncation cases, `kid_compose_split_round_trips`, `distinct_streams_of_one_leaf_get_distinct_kids`, `kid_for_rejects_stream_index_over_255`, `composed_kid_survives_wire_round_trip` (in-module `#[cfg(test)]`)
