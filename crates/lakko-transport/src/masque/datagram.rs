// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! HTTP Datagram payload codec per RFC 9297 §3 + RFC 9298 §3.
//!
//! Wire layout inside one HTTP Datagram:
//!
//! ```text
//!     +--------+------------------+
//!     | ctx_id |   UDP payload    |
//!     +--------+------------------+
//!     ^   ^         ^
//!     |   |         +--- inner UDP datagram bytes the
//!     |   |              client sends to / receives from
//!     |   |              the proxy.
//!     |   +--- variable-length integer per RFC 9000 §16.
//!     |        For CONNECT-UDP, RFC 9298 §3 pins
//!     |        ctx_id = 0 (the "UDP forwarding" context).
//!     |        Future extensions may add other context IDs
//!     |        (e.g. ICMP forwarding); the codec
//!     |        decodes them but the client side surfaces
//!     |        a `UnsupportedContextId` error on anything
//!     |        other than 0 today.
//!     +--- single byte (when ctx_id < 64); the QUIC
//!          varint encoding lets it grow up to 8 bytes.
//!
//! ```
//!
//! The codec is symmetric: every encoded payload decodes
//! losslessly via [`decode_udp_payload`].

use thiserror::Error;

/// Context ID value for CONNECT-UDP per RFC 9298 §3.
pub const CONNECT_UDP_CONTEXT_ID: u64 = 0;

/// Hard cap on the encoded HTTP Datagram payload (Context
/// ID + UDP body) to keep arbitrary peer datagrams from
/// over-allocating on the receive path. 65 KiB matches the
/// typical UDP MTU ceiling (max UDP payload = 65 535) +
/// the leading varint context-id.
pub const MAX_DATAGRAM_PAYLOAD: usize = 65 * 1024;

/// Failure modes the datagram codec surfaces.
#[derive(Debug, Error)]
pub enum DatagramError {
    /// Encoded payload exceeded [`MAX_DATAGRAM_PAYLOAD`].
    #[error("datagram payload too large: {got} > {max}")]
    TooLarge {
        /// Encoded byte count we observed.
        got: usize,
        /// Hard cap.
        max: usize,
    },
    /// Wire bytes ran out mid-varint.
    #[error("truncated varint context id")]
    TruncatedVarint,
    /// Wire bytes were empty (no Context ID byte at all).
    #[error("empty datagram payload")]
    Empty,
    /// Context ID was not zero — CONNECT-UDP only defines
    /// ctx 0. Refuse loudly rather than silently treat the
    /// remaining bytes as UDP.
    #[error("unsupported context id {got}; CONNECT-UDP requires 0")]
    UnsupportedContextId {
        /// The decoded context id.
        got: u64,
    },
}

/// Convenience alias.
pub type DatagramResult<T> = core::result::Result<T, DatagramError>;

/// Encode an outbound UDP payload as an HTTP Datagram per
/// RFC 9297 + RFC 9298. Always prepends Context ID 0.
///
/// # Errors
///
/// [`DatagramError::TooLarge`] if the encoded length would
/// exceed [`MAX_DATAGRAM_PAYLOAD`].
pub fn encode_udp_payload(udp: &[u8]) -> DatagramResult<Vec<u8>> {
    // Context ID 0 encodes as a single 0x00 byte under the
    // QUIC varint scheme (RFC 9000 §16): the top two bits
    // encode the length class, and 0 fits in the 6-bit
    // "1-byte" class.
    let mut out = Vec::with_capacity(1 + udp.len());
    encode_varint(CONNECT_UDP_CONTEXT_ID, &mut out);
    out.extend_from_slice(udp);
    if out.len() > MAX_DATAGRAM_PAYLOAD {
        return Err(DatagramError::TooLarge {
            got: out.len(),
            max: MAX_DATAGRAM_PAYLOAD,
        });
    }
    Ok(out)
}

/// Decode an inbound HTTP Datagram into the carried UDP
/// payload. Refuses Context IDs other than 0
/// (CONNECT-UDP-defined) and empty / truncated wire.
///
/// # Errors
///
/// See [`DatagramError`].
pub fn decode_udp_payload(wire: &[u8]) -> DatagramResult<&[u8]> {
    if wire.is_empty() {
        return Err(DatagramError::Empty);
    }
    if wire.len() > MAX_DATAGRAM_PAYLOAD {
        return Err(DatagramError::TooLarge {
            got: wire.len(),
            max: MAX_DATAGRAM_PAYLOAD,
        });
    }
    let (ctx_id, consumed) = decode_varint(wire)?;
    if ctx_id != CONNECT_UDP_CONTEXT_ID {
        return Err(DatagramError::UnsupportedContextId { got: ctx_id });
    }
    Ok(&wire[consumed..])
}

/// Encode a u64 as a QUIC variable-length integer per
/// RFC 9000 §16. The top two bits of the first byte name
/// the length class:
///
/// | bits | length | range          |
/// |---|---|---|
/// | 00   | 1 byte | 0..=63         |
/// | 01   | 2 bytes| 0..=16383      |
/// | 10   | 4 bytes| 0..=2^30-1     |
/// | 11   | 8 bytes| 0..=2^62-1     |
fn encode_varint(value: u64, out: &mut Vec<u8>) {
    if value <= 63 {
        // Branch bound: value ≤ 63 fits in 6 bits, so the
        // u8 cast cannot lose information.
        out.push(u8::try_from(value).expect("value ≤ 63 fits u8"));
    } else if value <= 16_383 {
        let v = u16::try_from(value).expect("value ≤ 16_383 fits u16") | 0x4000;
        out.extend_from_slice(&v.to_be_bytes());
    } else if value < (1u64 << 30) {
        let v = u32::try_from(value).expect("value < 2^30 fits u32") | 0x8000_0000;
        out.extend_from_slice(&v.to_be_bytes());
    } else {
        let v = value | 0xC000_0000_0000_0000;
        out.extend_from_slice(&v.to_be_bytes());
    }
}

/// Decode a QUIC varint, returning `(value, bytes_consumed)`.
/// Returns [`DatagramError::TruncatedVarint`] when the wire
/// runs out mid-encoding.
fn decode_varint(wire: &[u8]) -> DatagramResult<(u64, usize)> {
    if wire.is_empty() {
        return Err(DatagramError::TruncatedVarint);
    }
    let first = wire[0];
    let len_class = first >> 6;
    let mask: u8 = 0x3F;
    match len_class {
        0 => Ok((u64::from(first & mask), 1)),
        1 => {
            if wire.len() < 2 {
                return Err(DatagramError::TruncatedVarint);
            }
            let v = ((u16::from(first & mask)) << 8) | u16::from(wire[1]);
            Ok((u64::from(v), 2))
        },
        2 => {
            if wire.len() < 4 {
                return Err(DatagramError::TruncatedVarint);
            }
            let mut buf = [0u8; 4];
            buf.copy_from_slice(&wire[..4]);
            buf[0] &= mask;
            Ok((u64::from(u32::from_be_bytes(buf)), 4))
        },
        3 => {
            if wire.len() < 8 {
                return Err(DatagramError::TruncatedVarint);
            }
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&wire[..8]);
            buf[0] &= mask;
            Ok((u64::from_be_bytes(buf), 8))
        },
        _ => unreachable!("len_class is bounded to two bits"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_round_trip() {
        let udp = b"hello, masque";
        let encoded = encode_udp_payload(udp).expect("encode");
        // Context ID 0 is a single 0x00 byte.
        assert_eq!(encoded[0], 0);
        let decoded = decode_udp_payload(&encoded).expect("decode");
        assert_eq!(decoded, udp);
    }

    #[test]
    fn encode_rejects_oversize_payload() {
        let huge = vec![0u8; MAX_DATAGRAM_PAYLOAD + 1];
        match encode_udp_payload(&huge) {
            Err(DatagramError::TooLarge { got, max }) => {
                assert!(got > max);
                assert_eq!(max, MAX_DATAGRAM_PAYLOAD);
            },
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_empty_wire() {
        assert!(matches!(decode_udp_payload(&[]), Err(DatagramError::Empty)));
    }

    #[test]
    fn decode_rejects_truncated_varint() {
        // 0x40 names the 2-byte varint class but only one
        // byte is present.
        assert!(matches!(
            decode_udp_payload(&[0x40]),
            Err(DatagramError::TruncatedVarint)
        ));
    }

    #[test]
    fn decode_rejects_unsupported_context_id() {
        // Encode context id 7 (single varint byte), then
        // arbitrary "udp" bytes — CONNECT-UDP only defines 0.
        let mut wire = Vec::new();
        encode_varint(7, &mut wire);
        wire.extend_from_slice(b"junk");
        match decode_udp_payload(&wire) {
            Err(DatagramError::UnsupportedContextId { got }) => assert_eq!(got, 7),
            other => panic!("expected UnsupportedContextId, got {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_oversize_wire() {
        let huge = vec![0u8; MAX_DATAGRAM_PAYLOAD + 1];
        assert!(matches!(
            decode_udp_payload(&huge),
            Err(DatagramError::TooLarge { .. })
        ));
    }

    #[test]
    fn varint_encoding_classes() {
        let mut buf = Vec::new();
        // 1-byte class: 0..=63.
        encode_varint(0, &mut buf);
        assert_eq!(buf, vec![0x00]);
        buf.clear();
        encode_varint(63, &mut buf);
        assert_eq!(buf, vec![0x3F]);

        // 2-byte class: top-2 bits = 01.
        buf.clear();
        encode_varint(64, &mut buf);
        assert_eq!(buf, vec![0x40, 0x40]);
        buf.clear();
        encode_varint(16_383, &mut buf);
        assert_eq!(buf, vec![0x7F, 0xFF]);

        // 4-byte class: top-2 bits = 10.
        buf.clear();
        encode_varint(16_384, &mut buf);
        assert_eq!(buf[0] & 0xC0, 0x80);

        // 8-byte class: top-2 bits = 11.
        buf.clear();
        encode_varint(1u64 << 31, &mut buf);
        assert_eq!(buf[0] & 0xC0, 0xC0);
    }

    #[test]
    fn varint_round_trip_across_classes() {
        for value in [0u64, 1, 63, 64, 16_383, 16_384, (1 << 30) - 1, 1 << 31] {
            let mut buf = Vec::new();
            encode_varint(value, &mut buf);
            let (back, consumed) = decode_varint(&buf).expect("decode");
            assert_eq!(back, value, "value {value} round-trip failed");
            assert_eq!(consumed, buf.len());
        }
    }
}
