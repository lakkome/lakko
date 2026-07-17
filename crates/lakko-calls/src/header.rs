// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! RFC 9605 §4.3 SFrame frame-header encoder + decoder.
//!
//! **RFC 9605 §4.3 wire-compatible.** The header conveys the `(KID, CTR)`
//! pair an SFrame header carries and is bound as the AEAD AAD per ADR-0013,
//! and its byte packing is the RFC 9605 §4.3 layout — proven byte-for-byte
//! against the spec's own published vectors by
//! [`tests::rfc_9605_appendix_c1_header_vectors`]. Because the header IS the
//! AEAD AAD and it is now spec-conformant, a Lakko frame is interoperable with
//! any other RFC 9605 implementation (mlspp, Cisco libsframe, a browser-native
//! SFrame transform) that shares the key + nonce derivation. Every Lakko stack
//! — the Rust core, the `lakko-sframe-native` C ABI, the web wasm worker, iOS,
//! Android — pins the same golden vector and agrees byte-for-byte.
//!
//! ## Layout (RFC 9605 §4.3)
//!
//! The header is variable-length. The first byte is laid
//! out in big-endian bit order (bit 7 = MSB, bit 0 = LSB):
//!
//! ```text
//!   Bit:   7 6 5 4 3 2 1 0
//!   Field: X K K K Y C C C
//! ```
//!
//! - **X** (bit 7) — extended-KID flag.
//! - **K** (bits 6-4) —
//!     * if X=0: the KID itself (range `0..=7`), inline in the config byte;
//!     * if X=1: the KID length minus 1 (range `0..=7`, encoding 1..=8
//!       trailing KID bytes).
//! - **Y** (bit 3) — extended-CTR flag.
//! - **C** (bits 2-0) —
//!     * if Y=0: the CTR itself (range `0..=7`), inline in the config byte;
//!     * if Y=1: the CTR length minus 1 (range `0..=7`, encoding 1..=8
//!       trailing CTR bytes).
//!
//! After the config byte: `KID length` bytes of KID (only if X=1), then `CTR
//! length` bytes of CTR (only if Y=1), both big-endian. So `(kid=0, ctr=0)` is
//! the single byte `0x00`, and `(kid=0xff, ctr=0)` is `0x80 0xff`.
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
//! - Live cross-platform RTP interop over a DEPLOYED SFU (a
//!   browser-worker-sealed frame opened by a native shim, or interop with an
//!   external RFC 9605 stack on the wire) — deferred; needs live runtimes, not
//!   headless CI (G-26). The header is now §4.3-conformant, so this is a
//!   verification gate, not a design gap.

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

    /// A per-sender stream ordinal did not fit the 8-bit stream field of the
    /// composite KID ([`SframeHeader::kid_for`], ADR-0035).
    #[error("stream index {stream_index} exceeds the maximum {max}")]
    StreamIndexOverflow {
        /// The out-of-range stream index the caller supplied.
        stream_index: u32,
        /// The largest permitted stream index ([`MAX_STREAM_INDEX`]).
        max: u32,
    },
}

/// Maximum encoded header length: 1 byte first + 8 bytes
/// KID + 8 bytes CTR. Useful for callers that pre-allocate
/// a scratch buffer.
pub const MAX_HEADER_LEN: usize = 1 + 8 + 8;

/// Number of low bits of an SFrame KID reserved for the per-sender stream
/// ordinal (ADR-0035). The high bits carry the MLS sender leaf; the low
/// [`STREAM_INDEX_BITS`] bits carry a small per-sender stream index (0 for the
/// first stream, 1..N for additional streams a single leaf publishes — e.g.
/// camera + screen-share).
pub const STREAM_INDEX_BITS: u32 = 8;

/// Largest per-sender stream ordinal — the KID's low byte holds `0..=255`
/// (ADR-0035). 8 bits of stream space is ample for any realistic per-leaf
/// stream count; the sender assigns these ordinals in a stable local order.
pub const MAX_STREAM_INDEX: u32 = (1 << STREAM_INDEX_BITS) - 1;

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
    /// integer, minimum 1. Only called for the EXTENDED case
    /// (`n > 7`); a small `n` is carried inline in the config
    /// byte per RFC 9605 §4.3 and needs no trailing bytes.
    fn bytes_needed(n: u64) -> usize {
        if n == 0 {
            return 1;
        }
        let bits = 64 - n.leading_zeros() as usize;
        bits.div_ceil(8)
    }

    /// Encode to the on-wire byte layout (RFC 9605 §4.3). Length is at most
    /// [`MAX_HEADER_LEN`].
    ///
    /// Config byte: `X | KKK | Y | CCC` (bit 7 = X). `X=0` carries the KID
    /// inline in `KKK` (`0..=7`); `X=1` puts `KID length − 1` in `KKK` and the
    /// KID bytes follow. `Y`/`CCC` do the same for the CTR, whose bytes follow
    /// the KID bytes.
    ///
    /// The `as u8` casts are exact: `bytes_needed` returns `1..=8`, and the
    /// inline branches are gated on `<= 7`, so every masked value fits.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn encode(&self) -> Vec<u8> {
        let kid_inline = self.kid <= 7;
        let ctr_inline = self.ctr <= 7;

        let mut first = 0u8;
        if kid_inline {
            first |= (self.kid as u8 & 0x07) << 4;
        } else {
            first |= 0b1000_0000; // X = 1
            let kid_len = Self::bytes_needed(self.kid);
            first |= ((kid_len as u8 - 1) & 0x07) << 4;
        }
        if ctr_inline {
            first |= self.ctr as u8 & 0x07;
        } else {
            first |= 0b0000_1000; // Y = 1
            let ctr_len = Self::bytes_needed(self.ctr);
            first |= (ctr_len as u8 - 1) & 0x07;
        }

        let mut out = Vec::with_capacity(MAX_HEADER_LEN);
        out.push(first);
        if !kid_inline {
            let kid_len = Self::bytes_needed(self.kid);
            out.extend_from_slice(&self.kid.to_be_bytes()[8 - kid_len..]);
        }
        if !ctr_inline {
            let ctr_len = Self::bytes_needed(self.ctr);
            out.extend_from_slice(&self.ctr.to_be_bytes()[8 - ctr_len..]);
        }
        out
    }

    /// Decode from the on-wire byte layout (RFC 9605 §4.3). Returns the parsed
    /// header plus the number of bytes consumed — the caller slices
    /// `bytes[consumed..]` to reach the ciphertext.
    ///
    /// # Errors
    ///
    /// - [`HeaderError::Truncated`] when the buffer is too short for the
    ///   declared layout.
    pub fn decode(bytes: &[u8]) -> Result<(Self, usize), HeaderError> {
        if bytes.is_empty() {
            return Err(HeaderError::Truncated { needed: 1, had: 0 });
        }
        let first = bytes[0];
        // RFC 9605 §4.3: X | KKK | Y | CCC.
        let x = (first >> 7) & 0x01;
        let k = (first >> 4) & 0x07;
        let y = (first >> 3) & 0x01;
        let c = first & 0x07;

        let mut offset = 1;

        let kid: u64 = if x == 0 {
            u64::from(k)
        } else {
            let kid_len = (k + 1) as usize;
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

        let ctr: u64 = if y == 0 {
            u64::from(c)
        } else {
            let ctr_len = (c + 1) as usize;
            if bytes.len() < offset + ctr_len {
                return Err(HeaderError::Truncated {
                    needed: offset + ctr_len,
                    had: bytes.len(),
                });
            }
            let mut buf = [0u8; 8];
            buf[8 - ctr_len..].copy_from_slice(&bytes[offset..offset + ctr_len]);
            offset += ctr_len;
            u64::from_be_bytes(buf)
        };

        Ok((Self { kid, ctr }, offset))
    }

    /// Compose a per-`(sender_leaf, stream)` SFrame KID (ADR-0035):
    /// `(sender_leaf << STREAM_INDEX_BITS) | stream_index`.
    ///
    /// The KID is the ONLY per-stream discriminator that survives an SFU — the
    /// SFU forwards the RFC 9605 header verbatim (invariant #2), so a receiver
    /// re-derives the sender's key from the KID with no local media-type or
    /// transceiver-MID signal (a sender-side MID differs from the subscriber-side
    /// MID through an SFU, and audio/video track-kind cannot tell two same-kind
    /// streams apart). Folding the FULL KID into the key derivation
    /// ([`crate::sframe::CallKey::participant_key`]) gives every `(leaf, stream)`
    /// a disjoint `(key, nonce_salt)` space — so two video streams from one leaf
    /// (camera + screen-share) can never reuse a `(key, nonce)` pair, which the
    /// per-media-TYPE scheme of ADR-0034 could not guarantee.
    ///
    /// # Errors
    ///
    /// - [`HeaderError::StreamIndexOverflow`] when `stream_index >
    ///   MAX_STREAM_INDEX` (the ordinal would not fit the KID's 8-bit stream
    ///   field).
    pub fn kid_for(sender_leaf: u32, stream_index: u32) -> Result<u64, HeaderError> {
        if stream_index > MAX_STREAM_INDEX {
            return Err(HeaderError::StreamIndexOverflow {
                stream_index,
                max: MAX_STREAM_INDEX,
            });
        }
        Ok((u64::from(sender_leaf) << STREAM_INDEX_BITS) | u64::from(stream_index))
    }

    /// Recover the MLS sender leaf from a composite KID (ADR-0035): `kid >> 8`.
    ///
    /// A well-formed KID is `(u32 leaf << 8) | u8 stream`, so `kid >> 8` fits a
    /// `u32`. A malformed wire KID with bits set above the leaf range yields an
    /// advisory leaf whose derived key will not match the sender's — so the frame
    /// fails to open — meaning this split is roster/diagnostic-only and never a
    /// security gate (the key derivation binds the FULL KID, not the split leaf).
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub const fn leaf_of(kid: u64) -> u32 {
        (kid >> STREAM_INDEX_BITS) as u32
    }

    /// Recover the per-sender stream ordinal from a composite KID (ADR-0035):
    /// `kid & 0xFF`. Always in `0..=MAX_STREAM_INDEX`.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub const fn stream_of(kid: u64) -> u8 {
        (kid & MAX_STREAM_INDEX as u64) as u8
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

    /// Concrete RFC 9605 §4.3 wire layouts — pin the bit-packing (config byte
    /// `X | KKK | Y | CCC`, inline KID/CTR for values `<= 7`).
    #[test]
    fn pinned_wire_layouts() {
        // (kid=0, ctr=0): all inline → single config byte 0x00.
        assert_eq!(SframeHeader { kid: 0, ctr: 0 }.encode(), vec![0x00]);

        // (kid=5, ctr=0x42): kid inline (K=5 → 0x50), ctr extended (Y=1,
        // 1 byte → C=0 → 0x08) → 0x58, then CTR 0x42.
        assert_eq!(
            SframeHeader { kid: 5, ctr: 0x42 }.encode(),
            vec![0x58, 0x42]
        );

        // (kid=0, ctr=0x0100): ctr extended (Y=1, 2 bytes → C=1) → 0x09,
        // CTR = 0x01 0x00.
        assert_eq!(
            SframeHeader {
                kid: 0,
                ctr: 0x0100
            }
            .encode(),
            vec![0x09, 0x01, 0x00]
        );

        // (kid=8, ctr=0): kid extended (X=1, 1 byte → K=0) → 0x80, KID 0x08;
        // ctr inline (C=0) → NO trailing CTR byte. Total 2 bytes.
        assert_eq!(SframeHeader { kid: 8, ctr: 0 }.encode(), vec![0x80, 0x08]);

        // (kid=0x0100, ctr=0): kid extended (X=1, 2 bytes → K=1) → 0x90,
        // KID = 0x01 0x00; ctr inline → no CTR byte.
        assert_eq!(
            SframeHeader {
                kid: 0x0100,
                ctr: 0
            }
            .encode(),
            vec![0x90, 0x01, 0x00]
        );

        // (kid=u64::MAX, ctr=u64::MAX): X=1 K=7 (8 KID bytes) | Y=1 C=7 (8 CTR
        // bytes) → config 0xFF, then 8 + 8 bytes.
        let header = SframeHeader {
            kid: u64::MAX,
            ctr: u64::MAX,
        };
        let bytes = header.encode();
        assert_eq!(bytes[0], 0xFF);
        assert_eq!(bytes.len(), 1 + 8 + 8);
        assert_eq!(&bytes[1..9], &[0xFF; 8]);
        assert_eq!(&bytes[9..17], &[0xFF; 8]);
    }

    /// RFC 9605 Appendix C.1 published header test vectors — proves the encoder
    /// is byte-identical to the SPEC (not merely self-consistent), so any
    /// RFC-9605-conformant external stack (mlspp, Cisco libsframe, a
    /// browser-native SFrame transform) produces the same header bytes.
    #[test]
    fn rfc_9605_appendix_c1_header_vectors() {
        let cases: &[(u64, u64, &[u8])] = &[
            (0x0000_0000_0000_0000, 0x0000_0000_0000_0000, &[0x00]),
            (0x0000_0000_0000_0000, 0x0000_0000_0000_0001, &[0x01]),
            (0x0000_0000_0000_0001, 0x0000_0000_0000_0000, &[0x10]),
            (0x0000_0000_0000_0001, 0x0000_0000_0000_0001, &[0x11]),
            (0x0000_0000_0000_00ff, 0x0000_0000_0000_0000, &[0x80, 0xff]),
            (
                0x0000_0000_0000_0100,
                0x0000_0000_0000_0000,
                &[0x90, 0x01, 0x00],
            ),
            (
                0x0000_0000_0000_ffff,
                0x0000_0000_0000_0000,
                &[0x90, 0xff, 0xff],
            ),
            (
                0x0000_0001_0000_0000,
                0xffff_ffff_ffff_ffff,
                &[
                    0xcf, 0x01, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                    0xff,
                ],
            ),
        ];
        for (kid, ctr, wire) in cases {
            let encoded = SframeHeader {
                kid: *kid,
                ctr: *ctr,
            }
            .encode();
            assert_eq!(&encoded, wire, "RFC 9605 vector kid={kid:#x} ctr={ctr:#x}");
            let (parsed, consumed) = SframeHeader::decode(wire).unwrap();
            assert_eq!(
                parsed,
                SframeHeader {
                    kid: *kid,
                    ctr: *ctr
                }
            );
            assert_eq!(consumed, wire.len());
        }
    }

    #[test]
    fn decode_rejects_empty_buffer() {
        let err = SframeHeader::decode(&[]).unwrap_err();
        assert_eq!(err, HeaderError::Truncated { needed: 1, had: 0 });
    }

    #[test]
    fn decode_rejects_truncated_ctr() {
        // Config 0x0B = X=0 K=0 Y=1 C=3 → CTR is 4 bytes, but none follow.
        let err = SframeHeader::decode(&[0x0B]).unwrap_err();
        assert_eq!(err, HeaderError::Truncated { needed: 5, had: 1 });
    }

    #[test]
    fn decode_rejects_truncated_extended_kid() {
        // Config 0xB0 = X=1 K=3 → KID is 4 bytes, but only 2 follow.
        let err = SframeHeader::decode(&[0xB0, 0xAA, 0xBB]).unwrap_err();
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
            let ctr_bytes = if ctr <= 7 {
                0
            } else {
                SframeHeader::bytes_needed(ctr)
            };
            assert_eq!(bytes.len(), 1 + kid_bytes + ctr_bytes);
        }
    }

    /// All bit-positions in the RFC 9605 §4.3 first byte (`X | KKK | Y | CCC`)
    /// exercised once to lock the packing.
    #[test]
    fn first_byte_bit_packing_pinned() {
        // Inline KID varies 0..=7 (X=0, KID in bits 6-4), ctr=0.
        for kid in 0..=7u64 {
            let bytes = SframeHeader { kid, ctr: 0 }.encode();
            assert_eq!(bytes[0] & 0x80, 0, "X=0 for an inline kid");
            assert_eq!(u64::from((bytes[0] >> 4) & 0x07), kid);
        }
        // Inline CTR varies 0..=7 (Y=0, CTR in bits 2-0), kid=0.
        for ctr in 0..=7u64 {
            let bytes = SframeHeader { kid: 0, ctr }.encode();
            assert_eq!(bytes[0] & 0x08, 0, "Y=0 for an inline ctr");
            assert_eq!(u64::from(bytes[0] & 0x07), ctr);
        }
        // Extended KID: X=1, KKK = kid_len − 1 (0..=7).
        for klen in 0..=7u64 {
            let kid = 1u64 << (8 * (klen + 1) - 1); // forces kid_len = klen+1
            let bytes = SframeHeader { kid, ctr: 0 }.encode();
            assert_eq!(bytes[0] & 0x80, 0x80, "X=1 for an extended kid");
            assert_eq!(u64::from((bytes[0] >> 4) & 0x07), klen);
        }
        // Extended CTR: Y=1, CCC = ctr_len − 1 (0..=7).
        for clen in 0..=7u64 {
            let ctr = 1u64 << (8 * (clen + 1) - 1); // forces ctr_len = clen+1
            let bytes = SframeHeader { kid: 0, ctr }.encode();
            assert_eq!(bytes[0] & 0x08, 0x08, "Y=1 for an extended ctr");
            assert_eq!(u64::from(bytes[0] & 0x07), clen);
        }
    }

    // -- ADR-0035: KID = (sender_leaf << 8) | stream_index ------------------

    /// `kid_for` packs the leaf into the high bits and the stream ordinal into
    /// the low byte, and `leaf_of` / `stream_of` split it back out exactly.
    #[test]
    fn kid_compose_split_round_trips() {
        for (leaf, stream) in [
            (0u32, 0u32),
            (0, 1),
            (0, 255),
            (3, 0),
            (3, 1),
            (3, 2),
            (1, 255),
            (u32::MAX, 0),
            (u32::MAX, 255),
            (0x00AB_CDEF, 0x7F),
        ] {
            let kid = SframeHeader::kid_for(leaf, stream).expect("in range");
            assert_eq!(kid, (u64::from(leaf) << 8) | u64::from(stream));
            assert_eq!(SframeHeader::leaf_of(kid), leaf, "leaf round-trips");
            assert_eq!(
                u32::from(SframeHeader::stream_of(kid)),
                stream,
                "stream round-trips"
            );
        }
    }

    /// A leaf's distinct streams (0, 1, 2) map to distinct KIDs — the whole
    /// point of the discriminator (ADR-0035).
    #[test]
    fn distinct_streams_of_one_leaf_get_distinct_kids() {
        let a = SframeHeader::kid_for(4, 0).unwrap();
        let b = SframeHeader::kid_for(4, 1).unwrap();
        let c = SframeHeader::kid_for(4, 2).unwrap();
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
        // …and they all share the same leaf.
        assert_eq!(SframeHeader::leaf_of(a), 4);
        assert_eq!(SframeHeader::leaf_of(b), 4);
        assert_eq!(SframeHeader::leaf_of(c), 4);
    }

    /// `kid_for` rejects a stream ordinal that does not fit the 8-bit field
    /// (fail-closed, no silent truncation into another leaf's space).
    #[test]
    fn kid_for_rejects_stream_index_over_255() {
        assert_eq!(SframeHeader::kid_for(1, 255), Ok((1u64 << 8) | 255));
        assert_eq!(
            SframeHeader::kid_for(1, 256),
            Err(HeaderError::StreamIndexOverflow {
                stream_index: 256,
                max: MAX_STREAM_INDEX,
            })
        );
        assert_eq!(
            SframeHeader::kid_for(0, u32::MAX),
            Err(HeaderError::StreamIndexOverflow {
                stream_index: u32::MAX,
                max: MAX_STREAM_INDEX,
            })
        );
    }

    /// A composed KID survives an encode/decode wire round-trip unchanged — so
    /// the discriminator the sender folded into its key is exactly what the
    /// receiver reads back from the header.
    #[test]
    fn composed_kid_survives_wire_round_trip() {
        let kid = SframeHeader::kid_for(3, 2).unwrap(); // 0x0302
        let wire = SframeHeader { kid, ctr: 1 }.encode();
        let (parsed, _) = SframeHeader::decode(&wire).unwrap();
        assert_eq!(parsed.kid, kid);
        assert_eq!(SframeHeader::leaf_of(parsed.kid), 3);
        assert_eq!(SframeHeader::stream_of(parsed.kid), 2);
    }
}
