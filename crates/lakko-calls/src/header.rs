// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Lakko custom SFrame-style frame-header encoder + decoder.
//!
//! **Not RFC 9605 §4.3 wire-compatible.** This is a Lakko-internal
//! header layout that conveys the same `(KID, CTR)` pair an SFrame
//! header carries and is bound as the AEAD AAD per ADR-0013, but its
//! byte packing differs from RFC 9605 §4.3 (see "Divergence from
//! RFC 9605 §4.3" below). Every Lakko stack — the Rust core, the
//! `lakko-sframe-native` C ABI, the web wasm worker, iOS, Android —
//! pins the same golden vector and agrees byte-for-byte, so
//! Lakko-to-Lakko calls interoperate by proven equality. A non-Lakko
//! RFC 9605 stack (mlspp, Cisco libsframe, a browser-native SFrame
//! transform) would compute a DIFFERENT header, and because the header
//! IS the AEAD AAD it would fail to open a Lakko frame and vice versa.
//! External-stack interop is therefore broken by design — tracked as
//! **G-26 / GA-2** (see
//! [`docs/security/known-gaps.md`](../../../docs/security/known-gaps.md));
//! a true §4.3 rewrite is a deliberate future arc gated on an external
//! SFrame stack becoming a deployment target, not a current gap.
//!
//! ## Layout (Lakko custom)
//!
//! The header is variable-length. The first byte is laid
//! out in big-endian bit order (bit 7 = MSB, bit 0 = LSB):
//!
//! ```text
//!   Bit:   7 6 5 4 3 2 1 0
//!   Field: R L L L X K K K
//! ```
//!
//! - **R** (bit 7) — reserved, MUST be 0. The decoder
//!   rejects frames with R=1.
//! - **L** (bits 6-4) — counter length minus 1. Range
//!   `0..=7`, encoding counter widths `1..=8` bytes. There is
//!   no inline-CTR form: even `CTR=0` emits one trailing byte.
//! - **X** (bit 3) — extended-KID flag.
//! - **K** (bits 2-0) —
//!     * if X=0: the KID itself (range `0..=7`); the KID
//!       fits inline in the first byte.
//!     * if X=1: the KID length minus 1 (range `0..=7`,
//!       encoding 1..=8 trailing KID bytes).
//!
//! After the first byte:
//!
//! - if X=1, `KLEN+1` bytes of KID, big-endian, immediately
//!   follow.
//! - finally, `LEN+1` bytes of counter, big-endian.
//!
//! ## Divergence from RFC 9605 §4.3
//!
//! RFC 9605 §4.3 packs the first byte as `X(bit7) | K(bits6-4) |
//! Y(bit3) | C(bits2-0)`, where `Y` is an extended-CTR flag and a
//! `CTR<=7` is carried INLINE in `C` with no trailing CTR byte (and a
//! `KID<=7` inline in `K`). Lakko's layout instead puts the reserved
//! bit at the MSB, the CTR length in bits 6-4, has no inline-CTR form,
//! and the extended-KID flag at bit 3. The two produce different bytes
//! on every input — e.g. `(kid=0, ctr=0)` is `0x00 0x00` here versus
//! the single byte `0x00` under §4.3. The fields conveyed are the same
//! `(KID, CTR)`; only the wire packing differs.
//!
//! ## Why a u64 for both fields
//!
//! RFC 9605 caps the KID + CTR at `2^64 − 1` and Lakko keeps the same
//! 64-bit ceiling. The Lakko call-keying layer uses a `u32` per-leaf
//! counter ([`crate::sframe`]'s `ParticipantKey`), but the wire
//! format is intentionally wider — leaving the 64-bit ceiling intact
//! keeps the encoder usable by future schemes (channel sender-keys,
//! cross-call key rotations) without a wire-format break.
//!
//! ## What is NOT in this module
//!
//! - The header-as-AAD integration with [`crate::sframe`]
//!   has landed: [`crate::sframe::ParticipantKey::seal_framed`]
//!   / `open_framed` prepend this header on the wire and bind
//!   it as the AEAD AAD per ADR-0013, so the SFU can read KID
//!   bytes to route by participant without decrypting. The
//!   raw `(frame_counter, ciphertext)` `seal` / `open` pair
//!   stays available for callers that supply their own AAD.
//! - External interop with a non-Lakko RFC 9605 stack — broken by
//!   design (different first-byte layout, no inline CTR), see
//!   "Divergence from RFC 9605 §4.3" above + G-26 / GA-2. The
//!   self-consistency tests in this module pin Lakko's OWN wire shape
//!   (round-trip + bit-packing), which is what guarantees
//!   Lakko-to-Lakko byte-equality; they do NOT assert RFC 9605 §4.3
//!   conformance.

use thiserror::Error;

/// Failure modes of the header codec.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum HeaderError {
    /// Input buffer was too short to read the declared
    /// header.
    #[error("header truncated: needed {needed} bytes, had {had}")]
    Truncated {
        /// Bytes the declared layout required.
        needed: usize,
        /// Bytes actually available.
        had: usize,
    },

    /// Reserved bit (R) was set in the first byte. Lakko's
    /// custom header reserves bit 7; we treat any non-zero
    /// value as a version-drift signal and refuse to decode.
    /// (Note: RFC 9605 §4.3 has no reserved bit here — its
    /// bit 7 is the extended-KID flag; see the module docs.)
    #[error("reserved bit set in first header byte")]
    ReservedBitSet,
}

/// Maximum encoded header length: 1 byte first + 8 bytes
/// KID + 8 bytes CTR. Useful for callers that pre-allocate
/// a scratch buffer.
pub const MAX_HEADER_LEN: usize = 1 + 8 + 8;

/// Parsed SFrame header — the (KID, CTR) pair plus the
/// number of bytes the encoded form consumed. Returning the
/// consumed length lets the decoder pass through to the
/// ciphertext slice without a second length probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SframeHeader {
    /// Per-key identifier. Up to 64-bit per RFC 9605, even
    /// though Lakko's call-keying layer uses u32-per-leaf
    /// today.
    pub kid: u64,
    /// Per-frame monotone counter, up to 64-bit.
    pub ctr: u64,
}

impl SframeHeader {
    /// Number of bytes needed to encode `n` as a big-endian
    /// integer, minimum 1. Lakko's custom header always uses
    /// at least one CTR byte (even `CTR=0`); it has no inline
    /// form. (RFC 9605 §4.3, by contrast, carries `CTR<=7`
    /// inline in the config byte with zero trailing bytes.)
    fn bytes_needed(n: u64) -> usize {
        if n == 0 {
            return 1;
        }
        let bits = 64 - n.leading_zeros() as usize;
        bits.div_ceil(8)
    }

    /// Encode to the on-wire byte layout. Length is at most
    /// [`MAX_HEADER_LEN`].
    ///
    /// The casts below are exact: `bytes_needed` returns
    /// values in `1..=8`, and `self.kid <= 7` is checked
    /// before the inline branch — every `as u8` masks to
    /// values that fit. Clippy's `cast_possible_truncation`
    /// is silenced at the function level for that reason.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn encode(&self) -> Vec<u8> {
        let ctr_len = Self::bytes_needed(self.ctr);
        let kid_inline = self.kid <= 7;

        // ctr_len ∈ 1..=8 → LEN bits = ctr_len − 1 ∈ 0..=7.
        let len_bits = (ctr_len as u8 - 1) & 0x07;

        let first = if kid_inline {
            // R=0, LEN=len_bits, X=0, KID = self.kid in low 3 bits.
            (len_bits << 4) | (self.kid as u8 & 0x07)
        } else {
            // R=0, LEN=len_bits, X=1, KLEN bits = kid_len − 1.
            let kid_len = Self::bytes_needed(self.kid);
            let klen_bits = (kid_len as u8 - 1) & 0x07;
            (len_bits << 4) | 0b0000_1000 | klen_bits
        };

        let mut out = Vec::with_capacity(MAX_HEADER_LEN);
        out.push(first);

        if !kid_inline {
            let kid_len = Self::bytes_needed(self.kid);
            let kid_bytes = self.kid.to_be_bytes();
            out.extend_from_slice(&kid_bytes[8 - kid_len..]);
        }

        let ctr_bytes = self.ctr.to_be_bytes();
        out.extend_from_slice(&ctr_bytes[8 - ctr_len..]);

        out
    }

    /// Decode from the on-wire byte layout. Returns the
    /// parsed header plus the number of bytes consumed —
    /// the caller slices `bytes[consumed..]` to reach the
    /// ciphertext.
    ///
    /// # Errors
    ///
    /// - [`HeaderError::Truncated`] when the buffer is too
    ///   short for the declared layout.
    /// - [`HeaderError::ReservedBitSet`] when R=1 in the
    ///   first byte (would-be future schema marker; we
    ///   refuse so a forward-incompatible bump fails loud).
    pub fn decode(bytes: &[u8]) -> Result<(Self, usize), HeaderError> {
        if bytes.is_empty() {
            return Err(HeaderError::Truncated { needed: 1, had: 0 });
        }
        let first = bytes[0];

        if first & 0b1000_0000 != 0 {
            return Err(HeaderError::ReservedBitSet);
        }

        let ctr_len = (((first >> 4) & 0x07) + 1) as usize;
        let x = (first >> 3) & 0x01;
        let low3 = first & 0x07;

        let mut offset = 1;

        let kid: u64 = if x == 0 {
            u64::from(low3)
        } else {
            let kid_len = (low3 + 1) as usize;
            if bytes.len() < offset + kid_len {
                return Err(HeaderError::Truncated {
                    needed: offset + kid_len,
                    had: bytes.len(),
                });
            }
            let mut buf = [0u8; 8];
            buf[8 - kid_len..].copy_from_slice(&bytes[offset..offset + kid_len]);
            offset += kid_len;
            u64::from_be_bytes(buf)
        };

        if bytes.len() < offset + ctr_len {
            return Err(HeaderError::Truncated {
                needed: offset + ctr_len,
                had: bytes.len(),
            });
        }
        let mut buf = [0u8; 8];
        buf[8 - ctr_len..].copy_from_slice(&bytes[offset..offset + ctr_len]);
        offset += ctr_len;
        let ctr = u64::from_be_bytes(buf);

        Ok((Self { kid, ctr }, offset))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(kid: u64, ctr: u64) {
        let header = SframeHeader { kid, ctr };
        let bytes = header.encode();
        assert!(bytes.len() <= MAX_HEADER_LEN);
        let (parsed, consumed) = SframeHeader::decode(&bytes).unwrap();
        assert_eq!(parsed, header);
        assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn round_trip_zero_zero() {
        round_trip(0, 0);
    }

    #[test]
    fn round_trip_basic_kid_basic_ctr() {
        round_trip(0, 1);
        round_trip(0, 255);
        round_trip(7, 0);
        round_trip(7, 42);
    }

    #[test]
    fn round_trip_basic_kid_extended_ctr() {
        round_trip(3, 256);
        round_trip(3, 0xFFFF);
        round_trip(0, 0x0001_0000);
        round_trip(7, 0xFFFF_FFFF);
        round_trip(0, u64::MAX);
    }

    #[test]
    fn round_trip_extended_kid() {
        round_trip(8, 0);
        round_trip(255, 0);
        round_trip(256, 0);
        round_trip(0x1234, 0x5678);
        round_trip(u64::MAX, u64::MAX);
    }

    /// Concrete wire layouts — pin the bit-packing so a
    /// refactor cannot silently shift the byte boundaries.
    #[test]
    fn pinned_wire_layouts() {
        // (kid=0, ctr=0): first byte 0x00 (R=0, LEN=0, X=0,
        // KID=0), then 1 byte of CTR = 0x00. Total 2 bytes.
        assert_eq!(SframeHeader { kid: 0, ctr: 0 }.encode(), vec![0x00, 0x00]);

        // (kid=5, ctr=0x42): first 0x05 (LEN=0, X=0, KID=5),
        // CTR = 0x42.
        assert_eq!(
            SframeHeader { kid: 5, ctr: 0x42 }.encode(),
            vec![0x05, 0x42]
        );

        // (kid=0, ctr=0x0100): LEN=1 (ctr fits in 2 bytes),
        // X=0, KID=0 → first 0x10, CTR = 0x01 0x00.
        assert_eq!(
            SframeHeader {
                kid: 0,
                ctr: 0x0100
            }
            .encode(),
            vec![0x10, 0x01, 0x00]
        );

        // (kid=8, ctr=0): X=1, KLEN=0 (1 byte of KID),
        // first 0x08, KID byte = 0x08, CTR byte = 0x00.
        assert_eq!(
            SframeHeader { kid: 8, ctr: 0 }.encode(),
            vec![0x08, 0x08, 0x00]
        );

        // (kid=0x0100, ctr=0): X=1, KLEN=1 (2 bytes of KID),
        // first 0x09, KID = 0x01 0x00, CTR = 0x00.
        assert_eq!(
            SframeHeader {
                kid: 0x0100,
                ctr: 0
            }
            .encode(),
            vec![0x09, 0x01, 0x00, 0x00]
        );

        // (kid=u64::MAX, ctr=u64::MAX): X=1, KLEN=7 (8
        // bytes), LEN=7 (8 bytes). First byte:
        // LEN=7 (0x70) | X=1 (0x08) | KLEN=7 (0x07) = 0x7F.
        let header = SframeHeader {
            kid: u64::MAX,
            ctr: u64::MAX,
        };
        let bytes = header.encode();
        assert_eq!(bytes[0], 0x7F);
        assert_eq!(bytes.len(), 1 + 8 + 8);
        assert_eq!(&bytes[1..9], &[0xFF; 8]);
        assert_eq!(&bytes[9..17], &[0xFF; 8]);
    }

    #[test]
    fn decode_rejects_reserved_bit() {
        // R=1 means the encoder bumped to a schema version
        // we do not know — refuse.
        let err = SframeHeader::decode(&[0x80, 0x00]).unwrap_err();
        assert_eq!(err, HeaderError::ReservedBitSet);
    }

    #[test]
    fn decode_rejects_empty_buffer() {
        let err = SframeHeader::decode(&[]).unwrap_err();
        assert_eq!(err, HeaderError::Truncated { needed: 1, had: 0 });
    }

    #[test]
    fn decode_rejects_truncated_ctr() {
        // First byte declares LEN=3 (4 bytes of CTR) but no
        // CTR bytes follow.
        let err = SframeHeader::decode(&[0x30]).unwrap_err();
        assert_eq!(err, HeaderError::Truncated { needed: 5, had: 1 });
    }

    #[test]
    fn decode_rejects_truncated_extended_kid() {
        // First byte declares X=1, KLEN=3 (4 bytes of KID)
        // but only 2 follow.
        let err = SframeHeader::decode(&[0x0B, 0xAA, 0xBB]).unwrap_err();
        assert_eq!(err, HeaderError::Truncated { needed: 5, had: 3 });
    }

    #[test]
    fn bytes_needed_matches_expected_widths() {
        assert_eq!(SframeHeader::bytes_needed(0), 1);
        assert_eq!(SframeHeader::bytes_needed(1), 1);
        assert_eq!(SframeHeader::bytes_needed(0xFF), 1);
        assert_eq!(SframeHeader::bytes_needed(0x100), 2);
        assert_eq!(SframeHeader::bytes_needed(0xFFFF), 2);
        assert_eq!(SframeHeader::bytes_needed(0x1_0000), 3);
        assert_eq!(SframeHeader::bytes_needed(0xFFFF_FFFF), 4);
        assert_eq!(SframeHeader::bytes_needed(0x1_0000_0000), 5);
        assert_eq!(SframeHeader::bytes_needed(u64::MAX), 8);
    }

    /// Property: encoded length predictable from inputs.
    #[test]
    fn encoded_length_predictable() {
        for (kid, ctr) in [
            (0, 0),
            (7, 0),
            (8, 0),
            (255, 0),
            (256, 0),
            (0, 0xFF),
            (0, 0x100),
            (0xFFFF, 0xFFFF),
            (u64::MAX, u64::MAX),
        ] {
            let header = SframeHeader { kid, ctr };
            let bytes = header.encode();
            let kid_bytes = if kid <= 7 {
                0
            } else {
                SframeHeader::bytes_needed(kid)
            };
            let ctr_bytes = SframeHeader::bytes_needed(ctr);
            assert_eq!(bytes.len(), 1 + kid_bytes + ctr_bytes);
        }
    }

    /// All bit-positions in the first byte exercised once
    /// to lock the LEN / X / KID/KLEN packing.
    ///
    /// The `as u8` casts on `len` / `klen` are exact —
    /// both ranges are 0..=7 by construction.
    #[test]
    #[allow(clippy::cast_possible_truncation)]
    fn first_byte_bit_packing_pinned() {
        // LEN field varies 0..=7
        for len in 0..=7u64 {
            // pick a CTR that requires exactly len+1 bytes.
            let ctr = if len == 0 { 0 } else { 1u64 << (8 * len) };
            let header = SframeHeader { kid: 0, ctr };
            let bytes = header.encode();
            // Low 4 bits = X(=0) | KID(=0) → 0.
            // High 4 bits = R(=0) | LEN.
            assert_eq!(bytes[0] & 0xF0, (len as u8) << 4);
            assert_eq!(bytes[0] & 0x0F, 0);
        }

        // KID varies 0..=7 inline, X stays 0.
        for kid in 0..=7u64 {
            let header = SframeHeader { kid, ctr: 0 };
            let bytes = header.encode();
            assert_eq!(bytes[0] & 0x08, 0);
            assert_eq!(u64::from(bytes[0] & 0x07), kid);
        }

        // X=1 path — KLEN varies 0..=7.
        for klen in 0..=7u64 {
            let kid = 1u64 << (8 * (klen + 1) - 1); // forces kid_len = klen+1
            let header = SframeHeader { kid, ctr: 0 };
            let bytes = header.encode();
            assert_eq!(bytes[0] & 0x08, 0x08);
            assert_eq!(u64::from(bytes[0] & 0x07), klen);
        }
    }
}
