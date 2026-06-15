// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! QUIC Initial packet decryption + ClientHello extraction
//! (G5-7-transport-matrix-iter3).
//!
//! What this module does
//! =====================
//!
//! Given the first UDP datagram a passive observer sees on
//! a new QUIC connection (a long-header `Initial` packet),
//! recover the TLS ClientHello carried inside its
//! AEAD-protected `CRYPTO` frame. The math is pinned by
//! RFC 9001 §5.2 and the well-known initial salt — any
//! observer with the cleartext Destination Connection ID
//! (DCID) can decrypt the Initial; that is the on-the-wire
//! posture every QUIC v1 client publishes.
//!
//! The flow:
//!
//! ```text
//!   UDP datagram bytes
//!     ↓
//!   parse long header → (packet_type, version, DCID, SCID, length, pn_offset)
//!     ↓
//!   reject non-Initial / non-v1 packets
//!     ↓
//!   HKDF-Extract(initial_salt, DCID) → initial_secret
//!   HKDF-Expand-Label(initial_secret, "client in", 32) → cis
//!   HKDF-Expand-Label(cis, "quic key", 16) → key  (AES-128)
//!   HKDF-Expand-Label(cis, "quic iv",  12) → iv
//!   HKDF-Expand-Label(cis, "quic hp",  16) → hp_key
//!     ↓
//!   remove header protection (AES-ECB over sample → mask → XOR)
//!     ↓ → unprotect first byte (PNLEN) + packet number bytes
//!   AAD = header bytes (with first byte unprotected, pn bytes unprotected)
//!   nonce = iv XOR packet_number (right-aligned)
//!     ↓
//!   AEAD-decrypt payload+tag → plaintext frames
//!     ↓
//!   walk QUIC frames, accumulate CRYPTO offsets
//!     ↓
//!   feed reassembled handshake bytes to the TLS ClientHello
//!   parser (the same one the TCP path uses, via
//!   `parse_client_hello_from_handshake`)
//! ```
//!
//! Validated against the RFC 9001 §A.2 test vectors — the
//! exact byte sequence the spec publishes for a client
//! Initial. If our crypto math is wrong, the §A.2 test
//! fails immediately at module-load.

#![allow(clippy::missing_errors_doc)]

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockEncrypt, KeyInit};
use aes::Aes128;
use aes_gcm::aead::Aead;
use aes_gcm::{Aes128Gcm, Key, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;

/// RFC 9001 §5.2 — QUIC v1 initial salt.
pub const INITIAL_SALT_V1: [u8; 20] = [
    0x38, 0x76, 0x2c, 0xf7, 0xf5, 0x59, 0x34, 0xb3, 0x4d, 0x17, 0x9a, 0xe6, 0xa4, 0xc8, 0x0c, 0xad,
    0xcc, 0xbb, 0x7f, 0x0a,
];

/// QUIC v1 version number (RFC 9000 §15).
pub const QUIC_V1: u32 = 0x0000_0001;

/// Why we reject this packet. Coarse — the caller logs the
/// variant; we never surface internal state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuicInitialError {
    /// Packet is not a long-header form (high bit not set),
    /// or the long-header type bits don't say `Initial`
    /// (0b00).
    NotInitial,
    /// Packet version is something other than QUIC v1 (e.g.
    /// version negotiation 0x0000_0000, or a future version).
    UnsupportedVersion(u32),
    /// Datagram truncated mid-header / mid-payload.
    Truncated,
    /// AEAD decryption failed — wrong DCID, tampered packet,
    /// or genuinely-non-QUIC bytes that happened to pass the
    /// long-header parse.
    AeadFailed,
    /// QUIC frame parser saw a frame type we don't grok in
    /// an Initial packet (Initials allow CRYPTO + ACK +
    /// PADDING + PING + CONNECTION_CLOSE only per RFC 9000
    /// §17.2.2 — `BadFrameType` is the catch-all for
    /// anything else, including future extensions).
    BadFrameType(u8),
    /// CRYPTO frame reassembly hit a gap or overflow.
    CryptoReassembly,
}

/// One successful decryption. The caller takes the
/// `handshake_bytes` and feeds them to the existing
/// `parse_client_hello_from_handshake`.
#[derive(Debug, Clone)]
pub struct DecryptedInitial {
    /// Reassembled TLS handshake message bytes (header +
    /// body). Starts with `0x01` (ClientHello) for a fresh
    /// QUIC client; we do NOT validate that here so an
    /// observer logs even pathological starts.
    pub handshake_bytes: Vec<u8>,
}

/// Decrypt the first QUIC Initial packet in the datagram +
/// pull the handshake bytes out. Pure function; no I/O.
pub fn decrypt_initial(datagram: &[u8]) -> Result<DecryptedInitial, QuicInitialError> {
    // ----- 1. Parse the long header (cleartext) -----
    let header = parse_long_header(datagram)?;

    // Reject anything that isn't Initial v1.
    if header.packet_type != 0 {
        return Err(QuicInitialError::NotInitial);
    }
    if header.version != QUIC_V1 {
        return Err(QuicInitialError::UnsupportedVersion(header.version));
    }

    // ----- 2. Derive Initial keys from the client's DCID -----
    let (key, iv, hp_key) = derive_initial_keys(header.dcid);

    // ----- 3. Remove header protection -----
    // RFC 9001 §5.4 — sample is 16 bytes starting 4 bytes
    // PAST the start of the packet-number field. Since the
    // PN length is itself in the protected bits of the first
    // byte, sample-position is at `pn_offset + 4`.
    let sample_start = header.pn_offset + 4;
    let sample_end = sample_start + 16;
    if sample_end > datagram.len() {
        return Err(QuicInitialError::Truncated);
    }
    let sample = &datagram[sample_start..sample_end];

    let mask = compute_hp_mask(&hp_key, sample);

    // Unprotect the first byte (long header — low 4 bits
    // protected; PNLEN is in those low 4 bits' bottom 2).
    let first_byte = datagram[0] ^ (mask[0] & 0x0f);
    let pn_len = ((first_byte & 0x03) + 1) as usize; // 1..=4

    // Unprotect the packet number bytes.
    let pn_end = header.pn_offset + pn_len;
    if pn_end > datagram.len() {
        return Err(QuicInitialError::Truncated);
    }
    let mut pn_bytes = [0u8; 4];
    for i in 0..pn_len {
        pn_bytes[i] = datagram[header.pn_offset + i] ^ mask[1 + i];
    }
    // PN is right-aligned in pn_bytes[..pn_len] — pad MSBs.
    let mut pn_u64: u64 = 0;
    for &b in &pn_bytes[..pn_len] {
        pn_u64 = (pn_u64 << 8) | u64::from(b);
    }

    // ----- 4. Reconstruct the AAD + nonce -----
    // AAD is the full QUIC header AFTER header-protection
    // removal: protected first-byte cleared, PN bytes
    // cleared, but everything else kept as-is.
    let payload_start = pn_end;
    // The encoded `length` field on the long header is the
    // number of bytes covering the PN + payload + AEAD
    // tag. We need the END index of the Initial packet's
    // payload within the datagram; a coalesced datagram can
    // carry trailing Handshake / 0-RTT packets that we
    // ignore.
    let initial_end = header.pn_offset + header.length;
    if initial_end > datagram.len() {
        return Err(QuicInitialError::Truncated);
    }
    if payload_start >= initial_end {
        return Err(QuicInitialError::Truncated);
    }

    let mut aad = datagram[..pn_end].to_vec();
    aad[0] = first_byte;
    for i in 0..pn_len {
        aad[header.pn_offset + i] = pn_bytes[i];
    }

    // RFC 9001 §5.3 — nonce = iv XOR right-aligned PN.
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&iv);
    let pn_be = pn_u64.to_be_bytes(); // 8 bytes
    for (i, b) in pn_be.iter().enumerate() {
        nonce[4 + i] ^= b;
    }

    let ciphertext = &datagram[payload_start..initial_end];

    // ----- 5. AEAD decrypt -----
    let aead = Aes128Gcm::new(Key::<Aes128Gcm>::from_slice(&key));
    let plaintext = aead
        .decrypt(
            Nonce::from_slice(&nonce),
            aes_gcm::aead::Payload {
                msg: ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| QuicInitialError::AeadFailed)?;

    // ----- 6. Walk QUIC frames; reassemble CRYPTO bytes -----
    let handshake_bytes = reassemble_crypto(&plaintext)?;

    Ok(DecryptedInitial { handshake_bytes })
}

#[derive(Debug, Clone, Copy)]
struct LongHeader<'a> {
    /// 0=Initial, 1=0-RTT, 2=Handshake, 3=Retry. We only act
    /// on 0.
    packet_type: u8,
    version: u32,
    dcid: &'a [u8],
    /// Encoded length of (packet number + payload + AEAD tag).
    length: usize,
    /// Byte offset of the start of the (protected) packet
    /// number field.
    pn_offset: usize,
}

fn parse_long_header(datagram: &[u8]) -> Result<LongHeader<'_>, QuicInitialError> {
    if datagram.len() < 7 {
        return Err(QuicInitialError::Truncated);
    }
    let first = datagram[0];
    // Long-header form: high bit set; fixed bit (0x40) set.
    if first & 0xc0 != 0xc0 {
        return Err(QuicInitialError::NotInitial);
    }
    // Long-header type bits are positions 4-5: (first >> 4) & 0x03.
    let packet_type = (first >> 4) & 0x03;

    let version = u32::from_be_bytes([datagram[1], datagram[2], datagram[3], datagram[4]]);

    let dcid_len = datagram[5] as usize;
    if 6 + dcid_len > datagram.len() {
        return Err(QuicInitialError::Truncated);
    }
    let dcid = &datagram[6..6 + dcid_len];

    let scid_off = 6 + dcid_len;
    if scid_off >= datagram.len() {
        return Err(QuicInitialError::Truncated);
    }
    let scid_len = datagram[scid_off] as usize;
    let scid_end = scid_off + 1 + scid_len;
    if scid_end > datagram.len() {
        return Err(QuicInitialError::Truncated);
    }

    // RFC 9000 §17.2.2 — Initial packet only:
    //   Token Length (varint) + Token (token-len bytes)
    let mut cursor = scid_end;
    let (token_len, token_len_bytes) = read_varint(&datagram[cursor..])?;
    cursor += token_len_bytes;
    let token_len = token_len as usize;
    if cursor + token_len > datagram.len() {
        return Err(QuicInitialError::Truncated);
    }
    cursor += token_len;

    // Then the protected Length field (varint).
    let (length, length_bytes) = read_varint(&datagram[cursor..])?;
    cursor += length_bytes;
    let length = length as usize;

    // `cursor` now points at the start of the packet-number
    // field (which is variable-length 1..=4 bytes, with the
    // length encoded in the bottom 2 bits of `first` AFTER
    // header-protection removal).
    Ok(LongHeader {
        packet_type,
        version,
        dcid,
        length,
        pn_offset: cursor,
    })
}

/// RFC 9000 §16 — variable-length integer. Returns
/// `(value, bytes_consumed)`.
fn read_varint(buf: &[u8]) -> Result<(u64, usize), QuicInitialError> {
    if buf.is_empty() {
        return Err(QuicInitialError::Truncated);
    }
    let first = buf[0];
    let prefix = first >> 6;
    let len = 1usize << prefix;
    if buf.len() < len {
        return Err(QuicInitialError::Truncated);
    }
    // Mask off the two prefix bits in the first byte.
    let mut v: u64 = u64::from(first & 0x3f);
    for &b in &buf[1..len] {
        v = (v << 8) | u64::from(b);
    }
    Ok((v, len))
}

/// HKDF derivation per RFC 9001 §5.2. Returns
/// `(client_key, client_iv, client_hp_key)`.
fn derive_initial_keys(dcid: &[u8]) -> ([u8; 16], [u8; 12], [u8; 16]) {
    let (_prk, hk) = Hkdf::<Sha256>::extract(Some(&INITIAL_SALT_V1), dcid);
    let client_initial_secret = hkdf_expand_label(&hk, b"client in", 32);
    let client_hk = Hkdf::<Sha256>::from_prk(&client_initial_secret).expect("32-byte PRK");

    let mut key = [0u8; 16];
    key.copy_from_slice(&hkdf_expand_label(&client_hk, b"quic key", 16));
    let mut iv = [0u8; 12];
    iv.copy_from_slice(&hkdf_expand_label(&client_hk, b"quic iv", 12));
    let mut hp_key = [0u8; 16];
    hp_key.copy_from_slice(&hkdf_expand_label(&client_hk, b"quic hp", 16));

    (key, iv, hp_key)
}

/// TLS 1.3 HKDF-Expand-Label (RFC 8446 §7.1) using
/// "tls13 " prefix on the label.
fn hkdf_expand_label(hk: &Hkdf<Sha256>, label: &[u8], out_len: usize) -> Vec<u8> {
    // HkdfLabel struct = u16 length + u8 + "tls13 " + label
    //                  + u8 + context (empty here)
    let mut info = Vec::with_capacity(2 + 1 + 6 + label.len() + 1);
    info.extend_from_slice(&(out_len as u16).to_be_bytes());
    let prefix = b"tls13 ";
    let full_label_len = prefix.len() + label.len();
    info.push(full_label_len as u8);
    info.extend_from_slice(prefix);
    info.extend_from_slice(label);
    info.push(0); // empty context

    let mut out = vec![0u8; out_len];
    hk.expand(&info, &mut out)
        .expect("HKDF-Expand within range");
    out
}

/// RFC 9001 §5.4.2 — header-protection mask for AES-128.
/// Encrypt the 16-byte sample with AES-ECB-128(hp_key), the
/// first 5 bytes are the mask.
fn compute_hp_mask(hp_key: &[u8; 16], sample: &[u8]) -> [u8; 5] {
    let cipher = Aes128::new(GenericArray::from_slice(hp_key));
    let mut block = [0u8; 16];
    block.copy_from_slice(sample);
    let mut blk = GenericArray::from_mut_slice(&mut block);
    cipher.encrypt_block(&mut blk);
    let mut mask = [0u8; 5];
    mask.copy_from_slice(&block[..5]);
    mask
}

/// Walk the decrypted Initial-payload frame stream, collect
/// CRYPTO offsets, return the contiguous handshake bytes
/// starting at offset 0. Returns `Err(CryptoReassembly)` if
/// there is a gap or overlap.
fn reassemble_crypto(payload: &[u8]) -> Result<Vec<u8>, QuicInitialError> {
    // (offset, bytes) pairs, sorted by offset after the loop.
    let mut pieces: Vec<(u64, Vec<u8>)> = Vec::new();
    let mut cursor = 0usize;
    while cursor < payload.len() {
        let frame_type = payload[cursor];
        cursor += 1;
        match frame_type {
            // PADDING — single 0x00 byte; skip.
            0x00 => {},
            // PING — 0x01, no body.
            0x01 => {},
            // ACK — 0x02 / 0x03. Body: largest_ack(varint) +
            // ack_delay(varint) + ack_range_count(varint) +
            // first_ack_range(varint) + (ack_range_count) *
            // (gap + ack_range_length).
            0x02 | 0x03 => {
                let (_largest, n) = read_varint(&payload[cursor..])?;
                cursor += n;
                let (_delay, n) = read_varint(&payload[cursor..])?;
                cursor += n;
                let (range_count, n) = read_varint(&payload[cursor..])?;
                cursor += n;
                let (_first_range, n) = read_varint(&payload[cursor..])?;
                cursor += n;
                for _ in 0..range_count {
                    let (_gap, n) = read_varint(&payload[cursor..])?;
                    cursor += n;
                    let (_rlen, n) = read_varint(&payload[cursor..])?;
                    cursor += n;
                }
                // ACK_ECN (0x03) has 3 more varints — skip them.
                if frame_type == 0x03 {
                    for _ in 0..3 {
                        let (_v, n) = read_varint(&payload[cursor..])?;
                        cursor += n;
                    }
                }
            },
            // CRYPTO — what we actually want.
            0x06 => {
                let (offset, n) = read_varint(&payload[cursor..])?;
                cursor += n;
                let (length, n) = read_varint(&payload[cursor..])?;
                cursor += n;
                let length = length as usize;
                if cursor + length > payload.len() {
                    return Err(QuicInitialError::Truncated);
                }
                pieces.push((offset, payload[cursor..cursor + length].to_vec()));
                cursor += length;
            },
            // CONNECTION_CLOSE (Initial allows 0x1c). Body
            // we don't need — abort with a clean error.
            0x1c => return Err(QuicInitialError::CryptoReassembly),
            other => return Err(QuicInitialError::BadFrameType(other)),
        }
    }

    pieces.sort_by_key(|(off, _)| *off);
    let mut out: Vec<u8> = Vec::new();
    let mut expected: u64 = 0;
    for (off, bytes) in pieces {
        if off != expected {
            return Err(QuicInitialError::CryptoReassembly);
        }
        expected = off + bytes.len() as u64;
        out.extend_from_slice(&bytes);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 9001 §A.2 — client Initial packet test vector.
    /// The DCID, the plaintext frames, and the protected
    /// packet are all pinned in the spec. If our key
    /// derivation or AEAD math drifts, this vector fails
    /// immediately.
    ///
    /// DCID = 0x8394c8f03e515708
    /// Expected client_initial_secret =
    ///   0xc00cf151ca5be075ed0ebfb5c80323c42d6b7db67881289af4008f1f6c357aea
    /// Expected client key =
    ///   0x1f369613dd76d5467730efcbe3b1a22d
    /// Expected client iv =
    ///   0xfa044b2f42a3fd3b46fb255c
    /// Expected client hp =
    ///   0x9f50449e04a0e810283a1e9933adedd2
    #[test]
    fn rfc_9001_a2_derives_known_client_keys() {
        let dcid: [u8; 8] = [0x83, 0x94, 0xc8, 0xf0, 0x3e, 0x51, 0x57, 0x08];
        let (key, iv, hp) = derive_initial_keys(&dcid);

        assert_eq!(
            hex::encode(key),
            "1f369613dd76d5467730efcbe3b1a22d",
            "RFC 9001 §A.2 client quic key drifted",
        );
        assert_eq!(
            hex::encode(iv),
            "fa044b2f42a3fd3b46fb255c",
            "RFC 9001 §A.2 client quic iv drifted",
        );
        assert_eq!(
            hex::encode(hp),
            "9f50449e04a0e810283a1e9933adedd2",
            "RFC 9001 §A.2 client quic hp drifted",
        );
    }

    /// Smoke test for the long-header parser on a hand-
    /// rolled byte sequence (NOT the §A.2 wire packet —
    /// just enough to exercise the varint code).
    #[test]
    fn long_header_rejects_short_form() {
        // first byte 0x00 has the high bit clear → short
        // form → not Initial.
        let buf = [0x00u8; 32];
        let result = parse_long_header(&buf);
        assert!(matches!(result, Err(QuicInitialError::NotInitial)));
    }

    #[test]
    fn long_header_rejects_handshake_packet_type() {
        // type bits 0b10 = Handshake (2). High bit + fixed
        // bit + type=2 → first byte 0xe0.
        let mut buf = vec![0xe0, 0x00, 0x00, 0x00, 0x01]; // version v1
        buf.push(0); // DCID len = 0
        buf.push(0); // SCID len = 0
        buf.push(0); // token len varint = 0
        buf.push(0x40); // length varint = 0
        buf.extend_from_slice(&[0u8; 20]); // padding so varint reads don't truncate
        let header = parse_long_header(&buf).expect("parse ok");
        assert_eq!(header.packet_type, 2);
    }

    #[test]
    fn varint_decodes_all_four_lengths() {
        // RFC 9000 §16 — 1-byte: prefix 00, 6-bit payload.
        assert_eq!(read_varint(&[0x37]).unwrap(), (0x37, 1));
        // 2-byte: prefix 01, 14-bit payload.
        assert_eq!(read_varint(&[0x7b, 0xbd]).unwrap(), (0x3bbd, 2));
        // 4-byte: prefix 10.
        assert_eq!(
            read_varint(&[0x9d, 0x7f, 0x3e, 0x7d]).unwrap(),
            (0x1d7f3e7d, 4),
        );
        // 8-byte: prefix 11.
        assert_eq!(
            read_varint(&[0xc2, 0x19, 0x7c, 0x5e, 0xff, 0x14, 0xe8, 0x8c]).unwrap(),
            (0x2197c5eff14e88c, 8),
        );
    }

    #[test]
    fn hp_mask_is_first_five_aes_ecb_bytes() {
        // Trivial round-trip — encrypt zero block with zero
        // key, take first 5 bytes. The point is to lock in
        // that we are NOT accidentally taking some other
        // 5-byte window of the encrypted sample.
        let hp_key = [0u8; 16];
        let sample = [0u8; 16];
        let mask = compute_hp_mask(&hp_key, &sample);
        // AES-128-ECB(zero-key, zero-block) is a well-known
        // value: 66 e9 4b d4 ef 8a 2c 3b 88 4c fa 59 ca 34
        // 2b 2e. First 5 bytes:
        assert_eq!(hex::encode(mask), "66e94bd4ef");
    }

    #[test]
    fn reassemble_crypto_concatenates_in_order() {
        // Two CRYPTO frames, offsets 5 and 0 — must be
        // reassembled in the right order.
        let mut payload = Vec::new();
        // CRYPTO (0x06) offset=5 len=3 body="DEF"
        payload.push(0x06);
        payload.push(0x05); // varint 5
        payload.push(0x03); // varint 3
        payload.extend_from_slice(b"DEF");
        // CRYPTO (0x06) offset=0 len=5 body="ABCDE"
        payload.push(0x06);
        payload.push(0x00);
        payload.push(0x05);
        payload.extend_from_slice(b"ABCDE");

        let out = reassemble_crypto(&payload).unwrap();
        assert_eq!(out, b"ABCDEDEF");
    }

    #[test]
    fn reassemble_crypto_rejects_gaps() {
        let mut payload = Vec::new();
        // CRYPTO offset=10 len=3 — there is no offset=0
        // piece → caller cannot start parsing handshake.
        payload.push(0x06);
        payload.push(0x0a);
        payload.push(0x03);
        payload.extend_from_slice(b"XYZ");
        let result = reassemble_crypto(&payload);
        assert!(matches!(result, Err(QuicInitialError::CryptoReassembly)));
    }

    #[test]
    fn reassemble_crypto_skips_ping_and_padding() {
        let mut payload = Vec::new();
        payload.push(0x00); // PADDING
        payload.push(0x00); // PADDING
        payload.push(0x01); // PING
        payload.push(0x06); // CRYPTO offset=0 len=2 body="ok"
        payload.push(0x00);
        payload.push(0x02);
        payload.extend_from_slice(b"ok");
        payload.push(0x00); // trailing PADDING
        let out = reassemble_crypto(&payload).unwrap();
        assert_eq!(out, b"ok");
    }
}
