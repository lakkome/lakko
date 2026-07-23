// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Chunked AEAD stream primitive — building block for the
//! upcoming Shadowsocks-2022 (G5-3) and REALITY (G5-1)
//! pluggable-transport implementations.
//!
//! Both PT designs wrap the underlying TCP / QUIC stream
//! in **chunked ChaCha20-Poly1305**: each chunk is two
//! sealed AEAD frames sharing a deterministic nonce
//! counter — a length prefix and the payload itself. The
//! framing this module exposes is the SS-2022 / REALITY
//! intersection, with the SS-2022-specific timestamped
//! header layer left to the SS-2022 module on top.
//!
//! ## Wire format per chunk
//!
//! ```text
//! sealed_length  := AEAD_seal(key, nonce_n,   "", u16_be(plaintext_len))
//!                   == 2-byte ciphertext || 16-byte tag
//! sealed_payload := AEAD_seal(key, nonce_n+1, "", plaintext)
//!                   == plaintext.len()-byte ciphertext || 16-byte tag
//! wire_chunk     := sealed_length || sealed_payload
//! ```
//!
//! Two AEAD seals per chunk (length first, then payload)
//! sharing the per-stream nonce counter. The counter
//! advances after each seal — never reused across chunks
//! on the same key.
//!
//! ## Why two seals per chunk (not one)
//!
//! The length-prefix is sealed separately so the receiver
//! can authenticate the next-frame-size **before**
//! reserving a buffer for it. Reading the length unencrypted
//! would let an active attacker swap in a 64 KB length to
//! force a 64 KB buffer allocation; sealing it ties the
//! length to the per-stream key + nonce counter, and a
//! tampered length fails the AEAD authenticator on read.
//!
//! ## Nonce model
//!
//! 12-byte big-endian u64 counter (the low 8 bytes carry
//! the counter, the high 4 bytes stay zero — matches the
//! SS-2022 wire format and stays well below
//! ChaCha20-Poly1305's `2^96 - 1` ceiling). The writer's
//! counter is independent from the reader's; a duplex
//! stream uses two `AeadStreamWriter` / `AeadStreamReader`
//! pairs with different keys (one per direction) when
//! direction separation matters (it does for SS-2022).
//!
//! ## Max chunk size
//!
//! [`MAX_CHUNK`] = 16 KiB. Same as the SS-2022 / REALITY
//! pin; matches TLS 1.3 record size for cache-friendly
//! buffer reuse. Writers must split larger payloads
//! across multiple chunks; readers reject any sealed
//! length above this cap so a malformed stream can't
//! ask the receiver to allocate megabytes.

use std::io;

use lakko_crypto::aead::{self, AeadKey, Algorithm, Nonce, KEY_LEN, NONCE_LEN, TAG_LEN};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// AEAD ciphersuite — ChaCha20-Poly1305. Pinned at
/// module scope; SS-2022 + REALITY both default to it,
/// and AES-256-GCM stays a separate variant for the
/// rare FIPS-friendly call site.
pub const ALG: Algorithm = Algorithm::ChaCha20Poly1305;

/// Symmetric key length. Re-exported from
/// `lakko_crypto::aead` so call sites only depend on
/// this module.
pub const KEY_LEN_PUB: usize = KEY_LEN;

/// Nonce length. Re-exported from `lakko_crypto::aead`.
pub const NONCE_LEN_PUB: usize = NONCE_LEN;

/// Sealed AEAD tag length. Re-exported from
/// `lakko_crypto::aead`.
pub const TAG_LEN_PUB: usize = TAG_LEN;

/// Max plaintext bytes per chunk on the wire — 16 KiB.
/// Same pin SS-2022 and REALITY use. Writers must split
/// larger payloads across chunks; readers reject any
/// sealed length above this so a malformed stream can't
/// ask the receiver to allocate megabytes.
pub const MAX_CHUNK: usize = 16 * 1024;

/// Length-prefix encoding width — 2 bytes (big-endian
/// u16). Pinned by SS-2022; lifting to 4 bytes would be
/// a wire-format break.
pub const LENGTH_PREFIX_LEN: usize = 2;

/// Errors surfaced by the chunked AEAD stream wrappers.
#[derive(Debug, Error)]
pub enum AeadStreamError {
    /// I/O on the underlying stream failed (TCP reset,
    /// EOF mid-frame, write back-pressure surfacing as
    /// error).
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    /// AEAD authentication failed — wrong key, tampered
    /// ciphertext, or replay (counter desync between
    /// peers).
    #[error("AEAD authenticator rejected the frame")]
    AeadOpen,
    /// AEAD seal failed — essentially impossible with
    /// valid lengths under ChaCha20-Poly1305, surfaced
    /// for forward compatibility.
    #[error("AEAD seal failed")]
    AeadSeal,
    /// Plaintext chunk exceeds [`MAX_CHUNK`]. Writers
    /// must split larger payloads themselves.
    #[error("chunk plaintext ({0} bytes) exceeds MAX_CHUNK ({1})")]
    ChunkTooLarge(usize, usize),
    /// Sealed length-prefix decoded to a value above
    /// [`MAX_CHUNK`]. Indicates a malformed or
    /// tampered stream.
    #[error("sealed length-prefix decoded to {0} bytes — exceeds MAX_CHUNK")]
    DecodedLengthTooLarge(usize),
    /// Nonce counter exhausted. Will only happen at 2^64
    /// chunks on the same key — never in practice, but
    /// surfaced for symmetry.
    #[error("nonce counter wrapped to zero — rekey required")]
    NonceWrap,
}

/// Build a 12-byte AEAD nonce from a u64 counter. Low
/// 8 bytes are the counter, high 4 bytes are zero —
/// matches the SS-2022 wire layout.
fn nonce_from_counter(counter: u64) -> Nonce {
    let mut n = [0u8; NONCE_LEN];
    n[NONCE_LEN - 8..].copy_from_slice(&counter.to_be_bytes());
    n
}

// =========================================================
// Writer
// =========================================================

/// Chunked AEAD writer — wraps any `AsyncWrite` and
/// seals each call to [`Self::write_chunk`] under the
/// per-stream key + monotonic nonce counter.
///
/// Cheap to construct; the inner stream is held by move
/// so the writer owns the network-side flush. Call
/// [`Self::flush`] before drop if you need the buffer
/// drained.
pub struct AeadStreamWriter<W: AsyncWrite + Unpin> {
    inner: W,
    key: AeadKey,
    nonce_counter: u64,
}

impl<W: AsyncWrite + Unpin> AeadStreamWriter<W> {
    /// Construct from an underlying stream + the
    /// per-direction key. The nonce counter starts at 0.
    /// Equivalent to [`Self::new_with_counter`] with a
    /// `start_counter` of 0.
    pub fn new(inner: W, key: AeadKey) -> Self {
        Self::new_with_counter(inner, key, 0)
    }

    /// Construct from an underlying stream + the
    /// per-direction key, **resuming** the nonce counter at
    /// `start_counter` instead of 0.
    ///
    /// Use this when a handshake prelude sealed under the
    /// **same** key already consumed `start_counter` nonces,
    /// so the chunked body must continue past them. Restarting
    /// the body counter at 0 would reuse the prelude's
    /// `(key, nonce)` pairs on fresh plaintext — a
    /// catastrophic AEAD failure (keystream-XOR leak +
    /// authenticator forgery). SS-2022 seals its prelude fixed
    /// header at nonce 0 and its variable header at nonce 1, so
    /// the body resumes at 2 — see
    /// [`crate::shadowsocks::SS_PRELUDE_NONCE_COUNT`].
    ///
    /// The [`AeadStreamError::NonceWrap`] overflow guard is
    /// unchanged: seals continue to `checked_add` the counter
    /// from `start_counter`.
    pub fn new_with_counter(inner: W, key: AeadKey, start_counter: u64) -> Self {
        Self {
            inner,
            key,
            nonce_counter: start_counter,
        }
    }

    /// Seal `plaintext` as one chunk and write the
    /// sealed-length + sealed-payload pair to the
    /// underlying stream.
    ///
    /// # Errors
    ///
    /// - [`AeadStreamError::ChunkTooLarge`] when
    ///   `plaintext.len() > MAX_CHUNK`.
    /// - [`AeadStreamError::AeadSeal`] on AEAD failure
    ///   (effectively impossible).
    /// - [`AeadStreamError::Io`] on TCP write failure.
    /// - [`AeadStreamError::NonceWrap`] on the
    ///   astronomically-unlikely counter wraparound.
    pub async fn write_chunk(&mut self, plaintext: &[u8]) -> Result<(), AeadStreamError> {
        if plaintext.len() > MAX_CHUNK {
            return Err(AeadStreamError::ChunkTooLarge(plaintext.len(), MAX_CHUNK));
        }
        // First seal: the 2-byte length prefix. AAD is
        // empty — the nonce counter itself is the only
        // binding between length and payload.
        let length_bytes = u16::try_from(plaintext.len())
            .map_err(|_| AeadStreamError::ChunkTooLarge(plaintext.len(), MAX_CHUNK))?
            .to_be_bytes();
        let length_nonce = nonce_from_counter(self.nonce_counter);
        let sealed_length = aead::seal(ALG, &self.key, &length_nonce, &[], &length_bytes)
            .map_err(|_| AeadStreamError::AeadSeal)?;
        self.bump_counter()?;

        // Second seal: the payload itself.
        let payload_nonce = nonce_from_counter(self.nonce_counter);
        let sealed_payload = aead::seal(ALG, &self.key, &payload_nonce, &[], plaintext)
            .map_err(|_| AeadStreamError::AeadSeal)?;
        self.bump_counter()?;

        // Single combined write — TCP write coalescing
        // is the underlying layer's job.
        self.inner.write_all(&sealed_length).await?;
        self.inner.write_all(&sealed_payload).await?;
        Ok(())
    }

    /// Flush the underlying stream.
    pub async fn flush(&mut self) -> Result<(), AeadStreamError> {
        self.inner.flush().await?;
        Ok(())
    }

    /// Shut down the underlying stream gracefully (EOF).
    pub async fn shutdown(&mut self) -> Result<(), AeadStreamError> {
        self.inner.shutdown().await?;
        Ok(())
    }

    /// Borrow the inner stream — useful when the caller
    /// needs to thread it through a re-key dance.
    pub fn inner_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    /// Unwrap, returning the inner stream + the current
    /// nonce counter so a follow-up writer can resume
    /// without nonce reuse (e.g. after a re-key).
    pub fn into_inner(self) -> (W, u64) {
        (self.inner, self.nonce_counter)
    }

    fn bump_counter(&mut self) -> Result<(), AeadStreamError> {
        self.nonce_counter = self
            .nonce_counter
            .checked_add(1)
            .ok_or(AeadStreamError::NonceWrap)?;
        Ok(())
    }
}

// =========================================================
// Reader
// =========================================================

/// Chunked AEAD reader — wraps any `AsyncRead` and
/// decrypts one chunk per [`Self::read_chunk`] call.
/// Stateful: holds the per-stream nonce counter.
pub struct AeadStreamReader<R: AsyncRead + Unpin> {
    inner: R,
    key: AeadKey,
    nonce_counter: u64,
}

impl<R: AsyncRead + Unpin> AeadStreamReader<R> {
    /// Construct from an underlying stream + the
    /// per-direction key. The nonce counter starts at 0.
    /// Equivalent to [`Self::new_with_counter`] with a
    /// `start_counter` of 0.
    pub fn new(inner: R, key: AeadKey) -> Self {
        Self::new_with_counter(inner, key, 0)
    }

    /// Construct from an underlying stream + the
    /// per-direction key, **resuming** the nonce counter at
    /// `start_counter` instead of 0.
    ///
    /// The read-side mirror of
    /// [`AeadStreamWriter::new_with_counter`]: when a handshake
    /// prelude sealed under the **same** key already consumed
    /// `start_counter` nonces, the body reader must resume at
    /// the same offset so its counter tracks the peer's writer
    /// (SS-2022 resumes at 2 — see
    /// [`crate::shadowsocks::SS_PRELUDE_NONCE_COUNT`]).
    ///
    /// The [`AeadStreamError::NonceWrap`] overflow guard is
    /// unchanged.
    pub fn new_with_counter(inner: R, key: AeadKey, start_counter: u64) -> Self {
        Self {
            inner,
            key,
            nonce_counter: start_counter,
        }
    }

    /// Read + decrypt one chunk. Returns the plaintext.
    /// Returns `Ok(None)` on a clean EOF (peer closed
    /// before sending the next frame); returns an error
    /// on a half-read frame (peer closed mid-chunk).
    ///
    /// # Errors
    ///
    /// - [`AeadStreamError::Io`] on read failure or a
    ///   half-frame EOF.
    /// - [`AeadStreamError::AeadOpen`] on tampered or
    ///   wrong-key ciphertext.
    /// - [`AeadStreamError::DecodedLengthTooLarge`] if
    ///   the sealed length-prefix decoded above
    ///   [`MAX_CHUNK`] — malformed stream.
    /// - [`AeadStreamError::NonceWrap`] on counter
    ///   wraparound.
    pub async fn read_chunk(&mut self) -> Result<Option<Vec<u8>>, AeadStreamError> {
        // Read the sealed length frame: 2-byte
        // ciphertext + 16-byte tag = 18 bytes.
        let mut sealed_length = [0u8; LENGTH_PREFIX_LEN + TAG_LEN];
        match self.inner.read_exact(&mut sealed_length).await {
            Ok(_) => {},
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                // Clean EOF at frame boundary — peer
                // gracefully closed.
                return Ok(None);
            },
            Err(e) => return Err(e.into()),
        }
        let length_nonce = nonce_from_counter(self.nonce_counter);
        let length_plain = aead::open(ALG, &self.key, &length_nonce, &[], &sealed_length)
            .map_err(|_| AeadStreamError::AeadOpen)?;
        self.bump_counter()?;
        // Decoded length must be exactly 2 bytes by
        // construction; defensive check anyway.
        if length_plain.len() != LENGTH_PREFIX_LEN {
            return Err(AeadStreamError::AeadOpen);
        }
        let payload_len = u16::from_be_bytes([length_plain[0], length_plain[1]]) as usize;
        if payload_len > MAX_CHUNK {
            return Err(AeadStreamError::DecodedLengthTooLarge(payload_len));
        }

        // Read the sealed payload frame: payload_len
        // ciphertext + 16-byte tag.
        let mut sealed_payload = vec![0u8; payload_len + TAG_LEN];
        self.inner.read_exact(&mut sealed_payload).await?;
        let payload_nonce = nonce_from_counter(self.nonce_counter);
        let payload_plain = aead::open(ALG, &self.key, &payload_nonce, &[], &sealed_payload)
            .map_err(|_| AeadStreamError::AeadOpen)?;
        self.bump_counter()?;
        Ok(Some(payload_plain))
    }

    /// Borrow the inner stream.
    pub fn inner_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// Unwrap, returning the inner stream + the current
    /// nonce counter so a follow-up reader can resume
    /// after a re-key.
    pub fn into_inner(self) -> (R, u64) {
        (self.inner, self.nonce_counter)
    }

    fn bump_counter(&mut self) -> Result<(), AeadStreamError> {
        self.nonce_counter = self
            .nonce_counter
            .checked_add(1)
            .ok_or(AeadStreamError::NonceWrap)?;
        Ok(())
    }
}

// =========================================================
// Tests
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::secret::SecretBytes;
    use tokio::io::duplex;

    fn fresh_key(byte: u8) -> AeadKey {
        SecretBytes::new([byte; KEY_LEN])
    }

    #[test]
    fn nonce_counter_layout_pinned() {
        // Wire-contract pin — high 4 bytes zero, low 8
        // bytes big-endian counter.
        let n = nonce_from_counter(0x0123_4567_89AB_CDEF);
        assert_eq!(
            n,
            [0, 0, 0, 0, 0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF]
        );
    }

    #[test]
    fn nonce_counter_zero_is_all_zero() {
        assert_eq!(nonce_from_counter(0), [0u8; NONCE_LEN]);
    }

    #[test]
    fn pinned_constants() {
        assert_eq!(KEY_LEN_PUB, 32);
        assert_eq!(NONCE_LEN_PUB, 12);
        assert_eq!(TAG_LEN_PUB, 16);
        assert_eq!(LENGTH_PREFIX_LEN, 2);
        assert_eq!(MAX_CHUNK, 16 * 1024);
    }

    #[tokio::test]
    async fn round_trip_single_chunk() {
        let (a, b) = duplex(64 * 1024);
        let key = fresh_key(0x42);
        let mut writer = AeadStreamWriter::new(a, key.clone());
        let mut reader = AeadStreamReader::new(b, key);

        writer.write_chunk(b"hello aead").await.unwrap();
        writer.flush().await.unwrap();

        let got = reader.read_chunk().await.unwrap().unwrap();
        assert_eq!(got, b"hello aead");
    }

    #[tokio::test]
    async fn round_trip_many_chunks_nonce_ratchet() {
        // Many chunks in a row — proves the nonce
        // counter ratchets in lockstep on both sides.
        let (a, b) = duplex(64 * 1024);
        let key = fresh_key(0x77);
        let mut writer = AeadStreamWriter::new(a, key.clone());
        let mut reader = AeadStreamReader::new(b, key);

        let messages: Vec<Vec<u8>> = (0..16u32)
            .map(|i| vec![u8::try_from(i).unwrap(); 100 + usize::try_from(i).unwrap()])
            .collect();
        for m in &messages {
            writer.write_chunk(m).await.unwrap();
        }
        writer.flush().await.unwrap();

        for expected in &messages {
            let got = reader.read_chunk().await.unwrap().unwrap();
            assert_eq!(&got, expected);
        }
    }

    #[tokio::test]
    async fn new_with_counter_shifts_first_chunk_nonce() {
        // A writer resumed at counter 2 seals its first
        // chunk's length frame at nonce 2 (payload at 3),
        // NOT at 0/1. Prove it: a reader that restarts at 0
        // (the SS-2022 prelude/body reuse bug) cannot open
        // the frame, while a reader resuming at 2 can.
        let key = fresh_key(0x5A);
        let mut wire: Vec<u8> = Vec::new();
        {
            let mut writer = AeadStreamWriter::new_with_counter(&mut wire, key.clone(), 2);
            writer
                .write_chunk(b"body resumes at offset two")
                .await
                .unwrap();
            writer.flush().await.unwrap();
        }

        // Wrong offset (0) — AEAD authenticator rejects the
        // very first frame because its nonce is 2, not 0.
        {
            let (mut w, r) = duplex(4096);
            w.write_all(&wire).await.unwrap();
            drop(w);
            let mut wrong = AeadStreamReader::new(r, key.clone());
            assert!(matches!(
                wrong.read_chunk().await,
                Err(AeadStreamError::AeadOpen)
            ));
        }

        // Matching offset (2) opens the frame verbatim.
        {
            let (mut w, r) = duplex(4096);
            w.write_all(&wire).await.unwrap();
            drop(w);
            let mut right = AeadStreamReader::new_with_counter(r, key, 2);
            let got = right.read_chunk().await.unwrap().unwrap();
            assert_eq!(got, b"body resumes at offset two");
        }
    }

    #[tokio::test]
    async fn round_trip_max_chunk_size() {
        let (a, b) = duplex(64 * 1024);
        let key = fresh_key(0xAA);
        let mut writer = AeadStreamWriter::new(a, key.clone());
        let mut reader = AeadStreamReader::new(b, key);

        let big = vec![0xCDu8; MAX_CHUNK];
        writer.write_chunk(&big).await.unwrap();
        writer.flush().await.unwrap();
        let got = reader.read_chunk().await.unwrap().unwrap();
        assert_eq!(got.len(), MAX_CHUNK);
        assert_eq!(got, big);
    }

    #[tokio::test]
    async fn write_chunk_rejects_oversize() {
        let (a, _b) = duplex(64 * 1024);
        let key = fresh_key(0x01);
        let mut writer = AeadStreamWriter::new(a, key);
        let oversized = vec![0u8; MAX_CHUNK + 1];
        let err = writer.write_chunk(&oversized).await.unwrap_err();
        assert!(matches!(
            err,
            AeadStreamError::ChunkTooLarge(n, MAX_CHUNK) if n == MAX_CHUNK + 1
        ));
    }

    #[tokio::test]
    async fn read_chunk_rejects_wrong_key() {
        let (a, b) = duplex(64 * 1024);
        let mut writer = AeadStreamWriter::new(a, fresh_key(0x10));
        // Reader has a different key → AEAD authenticator
        // rejects the very first sealed-length frame.
        let mut reader = AeadStreamReader::new(b, fresh_key(0x20));

        writer.write_chunk(b"will not survive").await.unwrap();
        writer.flush().await.unwrap();
        let err = reader.read_chunk().await.unwrap_err();
        assert!(matches!(err, AeadStreamError::AeadOpen));
    }

    #[tokio::test]
    async fn read_chunk_clean_eof_returns_none() {
        let (a, b) = duplex(64 * 1024);
        let mut writer = AeadStreamWriter::new(a, fresh_key(0xF0));
        let mut reader = AeadStreamReader::new(b, fresh_key(0xF0));

        writer.write_chunk(b"first").await.unwrap();
        writer.flush().await.unwrap();
        writer.shutdown().await.unwrap();
        drop(writer); // close the underlying half of the duplex

        // First frame reads fine.
        let first = reader.read_chunk().await.unwrap().unwrap();
        assert_eq!(first, b"first");
        // Second read finds EOF cleanly at the frame
        // boundary — None, not error.
        assert!(reader.read_chunk().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn read_chunk_garbage_bytes_rejected_not_none() {
        // 18 zero bytes (a "sealed length" worth) is not
        // a clean EOF — it's a malformed frame the AEAD
        // authenticator must reject. Reader surfaces
        // this as AeadOpen, never as Ok(None).
        let (mut raw_w, raw_r) = duplex(1024);
        tokio::io::AsyncWriteExt::write_all(&mut raw_w, &[0u8; 18])
            .await
            .unwrap();
        drop(raw_w);
        let mut bad_reader = AeadStreamReader::new(raw_r, fresh_key(0xBB));
        let err = bad_reader.read_chunk().await.unwrap_err();
        assert!(matches!(err, AeadStreamError::AeadOpen));
    }

    #[tokio::test]
    async fn read_chunk_truncated_payload_is_io_error() {
        // Writer emits one full frame (sealed length +
        // sealed payload), then the peer closes. If the
        // reader is mid-payload, that surfaces as an Io
        // error (unexpected EOF), not Ok(None) — clean
        // EOF is only allowed at the frame boundary.
        let key = fresh_key(0xC0);

        // Build the sealed length + payload manually so
        // we can truncate the payload half by exactly N
        // bytes.
        let plaintext = b"some payload here";
        let len_nonce = nonce_from_counter(0);
        let payload_nonce = nonce_from_counter(1);
        let sealed_len = aead::seal(
            ALG,
            &key,
            &len_nonce,
            &[],
            &u16::try_from(plaintext.len()).unwrap().to_be_bytes(),
        )
        .unwrap();
        let sealed_payload = aead::seal(ALG, &key, &payload_nonce, &[], plaintext).unwrap();

        let (mut raw_w, raw_r) = duplex(1024);
        raw_w.write_all(&sealed_len).await.unwrap();
        // Write only the first half of the sealed payload
        // then drop — simulates peer crash mid-frame.
        raw_w
            .write_all(&sealed_payload[..sealed_payload.len() / 2])
            .await
            .unwrap();
        drop(raw_w);

        let mut bad_reader = AeadStreamReader::new(raw_r, key);
        let err = bad_reader.read_chunk().await.unwrap_err();
        assert!(matches!(err, AeadStreamError::Io(_)));
    }
}
