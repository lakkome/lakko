// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Sealed vault objects (ADR-0064 section 1).
//!
//! A vault holds a head and numbered chunks. Each is sealed with
//! ChaCha20-Poly1305 under the vault's key and padded to a size bucket first,
//! so the storage service sees ciphertext of a few distinct sizes and nothing
//! else. The claim object is not sealed: it is the public write key.
//!
//! ## Wire
//!
//! ```text
//! sealed := format(1) = 0x01 || nonce(12) || ciphertext
//! ciphertext := ChaCha20-Poly1305(vault_key, nonce, aad, padded)   (padded + 16-byte tag)
//! padded := len(u32 BE) || plaintext || 0x00 ... up to the bucket
//! aad := "lakko/vault/object/v1\0" || vault_id(32) || kind(1)
//!        || chunk_no(u32 BE) || version(u64 BE)
//! ```
//!
//! ## What the associated data binds
//!
//! The server chooses which stored bytes answer a request. The associated
//! data names the vault, the object kind, the chunk number and the version,
//! so bytes sealed as one of those open as nothing else: a head does not open
//! as a chunk, chunk 3 does not open as chunk 4, version 7 does not open as
//! version 8, and an object of one vault does not open in another (which the
//! per-vault key already prevents; the id is bound as well so the property
//! does not rest on one mechanism). A head has no chunk number: its
//! `chunk_no` is zero, and both directions refuse anything else.
//!
//! What it does not prevent is the server answering with an **older** version
//! when the client does not know which version is current. ADR-0064 records
//! that as a residual; a device that has seen a version refuses a lower one.
//!
//! ## The nonce
//!
//! The caller passes 12 fresh random bytes for every seal; this crate reaches
//! for no random source so it is the same on every target. The nonce is not
//! derived from the version, because two devices racing for version `n + 1`
//! seal different plaintexts under the same `(kind, chunk_no, version)` and
//! one of them loses with `409` only after both have sealed.
//!
//! A random 96-bit nonce can repeat. After `n` seals under one key the
//! chance that any two share a nonce is about `n^2 / 2^97`: about `2^-33`
//! at `2^32` seals, which is one write a second for 136 years.
//!
//! ## Padding
//!
//! The framed plaintext is padded with zero bytes to the next bucket: a power
//! of two from [`MIN_BUCKET_BYTES`] (1 KiB) to [`BUCKET_STEP_BYTES`]
//! (64 KiB), then a multiple of 64 KiB. Opening checks the length field,
//! that the bucket is the one that length calls for, and that every padding
//! byte is zero, and refuses anything else, so one plaintext has exactly one
//! padded form.
//!
//! The buckets and the [`MAX_OBJECT_BYTES`] cap are **chosen, not measured**:
//! no traffic exists to measure. They trade a bounded overhead (up to 1 KiB
//! for the smallest objects, under 2x up to 64 KiB, under 64 KiB above) for
//! a small set of visible sizes: 69 in all.
//!
//! ## Hostile input
//!
//! `open_object` runs on bytes the storage service returns. The size cap, the
//! format byte and the bucket shape are checked before anything is allocated
//! or decrypted, and nothing panics (fuzz target `vault_object_open`).

use lakko_crypto::aead::{self, AeadKey, Algorithm, NONCE_LEN, TAG_LEN};
use thiserror::Error;

use crate::keys::VAULT_ID_LEN;

/// The one object format this crate reads and writes.
pub const OBJECT_FORMAT_V1: u8 = 0x01;

/// Domain prefix of the associated data. Frozen.
pub const OBJECT_AAD_DOMAIN: &[u8] = b"lakko/vault/object/v1\0";

/// Largest sealed object, in bytes (4 MiB). Chosen, not measured.
pub const MAX_OBJECT_BYTES: usize = 4 * 1024 * 1024;

/// Smallest padded size, in bytes (1 KiB). Chosen, not measured.
pub const MIN_BUCKET_BYTES: usize = 1024;

/// Up to this size (64 KiB) a bucket is a power of two; above it, a multiple
/// of this size. Chosen, not measured.
pub const BUCKET_STEP_BYTES: usize = 64 * 1024;

/// Bytes a sealed object adds to its padded plaintext: the format byte, the
/// nonce and the AEAD tag.
pub const OBJECT_OVERHEAD_BYTES: usize = 1 + NONCE_LEN + TAG_LEN;

/// Bytes the length field adds in front of the plaintext.
const FRAME_LEN: usize = 4;

/// Largest padded plaintext: the largest bucket whose sealed form fits
/// [`MAX_OBJECT_BYTES`].
pub const MAX_PADDED_BYTES: usize =
    (MAX_OBJECT_BYTES - OBJECT_OVERHEAD_BYTES) / BUCKET_STEP_BYTES * BUCKET_STEP_BYTES;

/// Largest plaintext one object can carry.
pub const MAX_PLAINTEXT_BYTES: usize = MAX_PADDED_BYTES - FRAME_LEN;

/// Smallest sealed object: the smallest bucket plus the overhead.
pub const MIN_OBJECT_BYTES: usize = MIN_BUCKET_BYTES + OBJECT_OVERHEAD_BYTES;

/// Which object of a vault. The claim is not listed: it is not sealed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    /// The head: small, rewritten often.
    Head,
    /// A numbered chunk of earlier messages.
    Chunk,
}

impl ObjectKind {
    /// The byte the associated data carries.
    #[must_use]
    pub const fn to_wire(self) -> u8 {
        match self {
            Self::Head => 1,
            Self::Chunk => 2,
        }
    }

    /// Read a kind byte; anything unknown is `None`.
    #[must_use]
    pub const fn from_wire(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Head),
            2 => Some(Self::Chunk),
            _ => None,
        }
    }
}

/// Why an object was not sealed or not opened. Reason codes only: no variant
/// carries input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SealError {
    /// The plaintext is longer than [`MAX_PLAINTEXT_BYTES`].
    #[error("vault object plaintext is too large")]
    PlaintextTooLarge,
    /// A head was given a chunk number other than zero.
    #[error("a vault head has no chunk number")]
    HeadChunkNumber,
    /// The cipher refused to seal.
    #[error("vault object could not be sealed")]
    Seal,
    /// The sealed bytes are longer than [`MAX_OBJECT_BYTES`].
    #[error("vault object is too large")]
    ObjectTooLarge,
    /// The sealed bytes are shorter than [`MIN_OBJECT_BYTES`].
    #[error("vault object is truncated")]
    Truncated,
    /// A format byte this crate does not read.
    #[error("unknown vault object format")]
    UnknownFormat,
    /// The sealed length is not that of any bucket.
    #[error("vault object size is not a padding bucket")]
    NotABucket,
    /// Authentication failed: the wrong key, vault, kind, chunk number or
    /// version, or altered bytes.
    #[error("vault object failed authentication")]
    Authentication,
    /// The opened bytes are not a canonical padded frame: a length that does
    /// not fit, the wrong bucket for that length, or non-zero padding.
    #[error("vault object padding is not canonical")]
    Padding,
}

impl SealError {
    /// Stable, language-neutral reason code for the FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::PlaintextTooLarge => "plaintextTooLarge",
            Self::HeadChunkNumber => "headChunkNumber",
            Self::Seal => "seal",
            Self::ObjectTooLarge => "objectTooLarge",
            Self::Truncated => "truncated",
            Self::UnknownFormat => "unknownFormat",
            Self::NotABucket => "notABucket",
            Self::Authentication => "authentication",
            Self::Padding => "padding",
        }
    }
}

/// The padded size for a frame of `framed_len` bytes (length field included),
/// or `None` when it exceeds [`MAX_PADDED_BYTES`].
const fn bucket_for(framed_len: usize) -> Option<usize> {
    if framed_len > MAX_PADDED_BYTES {
        return None;
    }
    let bucket = if framed_len <= MIN_BUCKET_BYTES {
        MIN_BUCKET_BYTES
    } else if framed_len <= BUCKET_STEP_BYTES {
        framed_len.next_power_of_two()
    } else {
        // `framed_len <= MAX_PADDED_BYTES`, itself a multiple of the step,
        // so rounding up cannot overflow or pass the maximum.
        framed_len.div_ceil(BUCKET_STEP_BYTES) * BUCKET_STEP_BYTES
    };
    Some(bucket)
}

/// Whether `len` is a size [`bucket_for`] can return.
const fn is_bucket(len: usize) -> bool {
    if len < MIN_BUCKET_BYTES || len > MAX_PADDED_BYTES {
        return false;
    }
    if len <= BUCKET_STEP_BYTES {
        len.is_power_of_two()
    } else {
        len % BUCKET_STEP_BYTES == 0
    }
}

/// The sealed size of an object carrying `plaintext_len` bytes, or `None`
/// when that is more than [`MAX_PLAINTEXT_BYTES`]. What the storage service
/// will see, for a client that budgets a vault's bytes.
#[must_use]
pub const fn sealed_len(plaintext_len: usize) -> Option<usize> {
    if plaintext_len > MAX_PLAINTEXT_BYTES {
        return None;
    }
    match bucket_for(plaintext_len + FRAME_LEN) {
        Some(bucket) => Some(bucket + OBJECT_OVERHEAD_BYTES),
        None => None,
    }
}

/// `len(u32 BE) || plaintext || zero padding`, up to the bucket.
pub(crate) fn pad(plaintext: &[u8]) -> Result<Vec<u8>, SealError> {
    if plaintext.len() > MAX_PLAINTEXT_BYTES {
        return Err(SealError::PlaintextTooLarge);
    }
    let len = u32::try_from(plaintext.len()).map_err(|_| SealError::PlaintextTooLarge)?;
    let bucket = bucket_for(plaintext.len() + FRAME_LEN).ok_or(SealError::PlaintextTooLarge)?;
    let mut padded = Vec::with_capacity(bucket);
    padded.extend_from_slice(&len.to_be_bytes());
    padded.extend_from_slice(plaintext);
    padded.resize(bucket, 0);
    Ok(padded)
}

/// The inverse of [`pad`], refusing every frame [`pad`] would not produce.
pub(crate) fn unpad(mut padded: Vec<u8>) -> Result<Vec<u8>, SealError> {
    let Some((len_bytes, body)) = padded.split_first_chunk::<FRAME_LEN>() else {
        return Err(SealError::Padding);
    };
    let len = usize::try_from(u32::from_be_bytes(*len_bytes)).map_err(|_| SealError::Padding)?;
    if len > body.len() {
        return Err(SealError::Padding);
    }
    if bucket_for(len + FRAME_LEN) != Some(padded.len()) {
        return Err(SealError::Padding);
    }
    let (_, padding) = body.split_at(len);
    if padding.iter().any(|byte| *byte != 0) {
        return Err(SealError::Padding);
    }
    padded.truncate(FRAME_LEN + len);
    padded.drain(..FRAME_LEN);
    Ok(padded)
}

/// The associated data of one object (see the module docs).
#[must_use]
pub fn object_aad(
    vault_id: &[u8; VAULT_ID_LEN],
    kind: ObjectKind,
    chunk_no: u32,
    version: u64,
) -> Vec<u8> {
    let mut aad = Vec::with_capacity(OBJECT_AAD_DOMAIN.len() + VAULT_ID_LEN + 1 + 4 + 8);
    aad.extend_from_slice(OBJECT_AAD_DOMAIN);
    aad.extend_from_slice(vault_id);
    aad.push(kind.to_wire());
    aad.extend_from_slice(&chunk_no.to_be_bytes());
    aad.extend_from_slice(&version.to_be_bytes());
    aad
}

const fn check_address(kind: ObjectKind, chunk_no: u32) -> Result<(), SealError> {
    match kind {
        ObjectKind::Head if chunk_no != 0 => Err(SealError::HeadChunkNumber),
        ObjectKind::Head | ObjectKind::Chunk => Ok(()),
    }
}

/// Seal `plaintext` as the object `(kind, chunk_no, version)` of the vault
/// `vault_id`, under `vault_key` and a fresh random `nonce`.
///
/// `chunk_no` is zero for a head. The nonce is 12 bytes from the platform's
/// secure random source, fresh for every call (see the module docs).
///
/// # Errors
///
/// [`SealError::HeadChunkNumber`], [`SealError::PlaintextTooLarge`], or
/// [`SealError::Seal`] if the cipher fails.
pub fn seal_object(
    vault_key: &AeadKey,
    vault_id: &[u8; VAULT_ID_LEN],
    kind: ObjectKind,
    chunk_no: u32,
    version: u64,
    nonce: &[u8; NONCE_LEN],
    plaintext: &[u8],
) -> Result<Vec<u8>, SealError> {
    check_address(kind, chunk_no)?;
    let padded = pad(plaintext)?;
    let aad = object_aad(vault_id, kind, chunk_no, version);
    let ciphertext = aead::seal(Algorithm::ChaCha20Poly1305, vault_key, nonce, &aad, &padded)
        .map_err(|_| SealError::Seal)?;
    let mut sealed = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
    sealed.push(OBJECT_FORMAT_V1);
    sealed.extend_from_slice(nonce);
    sealed.extend_from_slice(&ciphertext);
    Ok(sealed)
}

/// Open `sealed` as the object `(kind, chunk_no, version)` of the vault
/// `vault_id`. The caller names the object it asked for; bytes sealed as any
/// other object fail authentication.
///
/// # Errors
///
/// [`SealError::HeadChunkNumber`] for a head asked with a chunk number;
/// [`SealError::ObjectTooLarge`], [`SealError::Truncated`],
/// [`SealError::UnknownFormat`] or [`SealError::NotABucket`] before anything
/// is decrypted; [`SealError::Authentication`]; [`SealError::Padding`].
pub fn open_object(
    vault_key: &AeadKey,
    vault_id: &[u8; VAULT_ID_LEN],
    kind: ObjectKind,
    chunk_no: u32,
    version: u64,
    sealed: &[u8],
) -> Result<Vec<u8>, SealError> {
    check_address(kind, chunk_no)?;
    if sealed.len() > MAX_OBJECT_BYTES {
        return Err(SealError::ObjectTooLarge);
    }
    let Some((format, rest)) = sealed.split_first() else {
        return Err(SealError::Truncated);
    };
    if *format != OBJECT_FORMAT_V1 {
        return Err(SealError::UnknownFormat);
    }
    let Some((nonce, ciphertext)) = rest.split_first_chunk::<NONCE_LEN>() else {
        return Err(SealError::Truncated);
    };
    let Some(padded_len) = ciphertext.len().checked_sub(TAG_LEN) else {
        return Err(SealError::Truncated);
    };
    if padded_len < MIN_BUCKET_BYTES {
        return Err(SealError::Truncated);
    }
    if !is_bucket(padded_len) {
        return Err(SealError::NotABucket);
    }
    let aad = object_aad(vault_id, kind, chunk_no, version);
    let padded = aead::open(
        Algorithm::ChaCha20Poly1305,
        vault_key,
        nonce,
        &aad,
        ciphertext,
    )
    .map_err(|_| SealError::Authentication)?;
    unpad(padded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex;
    use lakko_crypto::hash;

    const VAULT_ID: [u8; 32] = [0x1D; 32];
    const NONCE: [u8; NONCE_LEN] = [0x4E; NONCE_LEN];

    fn key() -> AeadKey {
        AeadKey::new([0x6B; 32])
    }

    fn seal(kind: ObjectKind, chunk_no: u32, version: u64, plaintext: &[u8]) -> Vec<u8> {
        seal_object(
            &key(),
            &VAULT_ID,
            kind,
            chunk_no,
            version,
            &NONCE,
            plaintext,
        )
        .unwrap()
    }

    fn open(kind: ObjectKind, chunk_no: u32, version: u64, sealed: &[u8]) -> Vec<u8> {
        open_object(&key(), &VAULT_ID, kind, chunk_no, version, sealed).unwrap()
    }

    #[test]
    fn constants_are_the_documented_values() {
        assert_eq!(OBJECT_AAD_DOMAIN, b"lakko/vault/object/v1\0");
        assert_eq!(OBJECT_AAD_DOMAIN.len(), 22);
        assert_eq!(OBJECT_OVERHEAD_BYTES, 29);
        assert_eq!(MAX_OBJECT_BYTES, 4_194_304);
        assert_eq!(MAX_PADDED_BYTES, 63 * 65_536);
        assert_eq!(MAX_PLAINTEXT_BYTES, 63 * 65_536 - 4);
        assert_eq!(MIN_OBJECT_BYTES, 1024 + 29);
        // The largest bucket fits the cap and the next one would not.
        const { assert!(MAX_PADDED_BYTES + OBJECT_OVERHEAD_BYTES <= MAX_OBJECT_BYTES) };
        const {
            assert!(
                MAX_PADDED_BYTES + BUCKET_STEP_BYTES + OBJECT_OVERHEAD_BYTES > MAX_OBJECT_BYTES
            );
        };
        assert_eq!(ObjectKind::Head.to_wire(), 1);
        assert_eq!(ObjectKind::Chunk.to_wire(), 2);
        assert_eq!(ObjectKind::from_wire(1), Some(ObjectKind::Head));
        assert_eq!(ObjectKind::from_wire(2), Some(ObjectKind::Chunk));
        assert_eq!(ObjectKind::from_wire(0), None);
        assert_eq!(ObjectKind::from_wire(3), None);
    }

    #[test]
    fn round_trips_at_every_bucket_edge() {
        let edges = [
            0usize, 1, 1020, 1021, 2044, 2045, 32_764, 32_765, 65_532, 65_533, 131_068, 131_069,
        ];
        for len in edges {
            let plaintext: Vec<u8> = (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect();
            let sealed = seal(ObjectKind::Chunk, 3, 9, &plaintext);
            assert_eq!(Some(sealed.len()), sealed_len(len), "{len}");
            assert!(is_bucket(sealed.len() - OBJECT_OVERHEAD_BYTES), "{len}");
            assert_eq!(open(ObjectKind::Chunk, 3, 9, &sealed), plaintext, "{len}");
        }
    }

    #[test]
    fn buckets_are_the_documented_sizes() {
        assert_eq!(bucket_for(0), Some(1024));
        assert_eq!(bucket_for(4), Some(1024));
        assert_eq!(bucket_for(1024), Some(1024));
        assert_eq!(bucket_for(1025), Some(2048));
        assert_eq!(bucket_for(4096), Some(4096));
        assert_eq!(bucket_for(4097), Some(8192));
        assert_eq!(bucket_for(65_536), Some(65_536));
        assert_eq!(bucket_for(65_537), Some(131_072));
        assert_eq!(bucket_for(131_072), Some(131_072));
        assert_eq!(bucket_for(131_073), Some(196_608));
        assert_eq!(bucket_for(MAX_PADDED_BYTES), Some(MAX_PADDED_BYTES));
        assert_eq!(bucket_for(MAX_PADDED_BYTES + 1), None);
        assert_eq!(bucket_for(usize::MAX), None);
        // Every bucket is one `is_bucket` accepts, and maps to itself.
        let mut seen = Vec::new();
        for framed in (0..=MAX_PADDED_BYTES).step_by(509) {
            let bucket = bucket_for(framed).unwrap();
            assert!(bucket >= framed && is_bucket(bucket));
            assert_eq!(bucket_for(bucket), Some(bucket));
            if !seen.contains(&bucket) {
                seen.push(bucket);
            }
        }
        // 1 KiB .. 64 KiB is seven powers of two; then 2 .. 63 steps.
        assert_eq!(seen.len(), 7 + 62);
        assert!(!is_bucket(0));
        assert!(!is_bucket(1023));
        assert!(!is_bucket(1536));
        assert!(!is_bucket(65_536 + 1024));
        assert!(!is_bucket(MAX_PADDED_BYTES + BUCKET_STEP_BYTES));
    }

    #[test]
    fn the_largest_plaintext_seals_to_the_cap_and_one_more_is_refused() {
        let largest = vec![0xEEu8; MAX_PLAINTEXT_BYTES];
        let sealed = seal(ObjectKind::Chunk, 0, 1, &largest);
        assert_eq!(sealed.len(), MAX_PADDED_BYTES + OBJECT_OVERHEAD_BYTES);
        assert!(sealed.len() <= MAX_OBJECT_BYTES);
        assert_eq!(open(ObjectKind::Chunk, 0, 1, &sealed), largest);

        let one_more = vec![0u8; MAX_PLAINTEXT_BYTES + 1];
        assert_eq!(
            seal_object(
                &key(),
                &VAULT_ID,
                ObjectKind::Chunk,
                0,
                1,
                &NONCE,
                &one_more
            ),
            Err(SealError::PlaintextTooLarge)
        );
        assert_eq!(sealed_len(MAX_PLAINTEXT_BYTES + 1), None);
        assert_eq!(sealed_len(usize::MAX), None);
    }

    #[test]
    fn an_oversized_object_is_refused_before_anything_else() {
        // A wrong format byte and a size over the cap: the cap answers.
        let huge = vec![0xFFu8; MAX_OBJECT_BYTES + 1];
        assert_eq!(
            open_object(&key(), &VAULT_ID, ObjectKind::Head, 0, 1, &huge),
            Err(SealError::ObjectTooLarge)
        );
    }

    #[test]
    fn the_associated_data_binds_the_object() {
        let sealed = seal(ObjectKind::Head, 0, 7, b"head");
        assert_eq!(open(ObjectKind::Head, 0, 7, &sealed), b"head");
        let refused = |r: Result<Vec<u8>, SealError>| {
            assert_eq!(r, Err(SealError::Authentication));
        };
        // A head opened as a chunk.
        refused(open_object(
            &key(),
            &VAULT_ID,
            ObjectKind::Chunk,
            0,
            7,
            &sealed,
        ));
        // Another version.
        refused(open_object(
            &key(),
            &VAULT_ID,
            ObjectKind::Head,
            0,
            8,
            &sealed,
        ));
        // Another vault id.
        refused(open_object(
            &key(),
            &[0x1E; 32],
            ObjectKind::Head,
            0,
            7,
            &sealed,
        ));
        // Another key.
        refused(open_object(
            &AeadKey::new([0x6C; 32]),
            &VAULT_ID,
            ObjectKind::Head,
            0,
            7,
            &sealed,
        ));

        let chunk = seal(ObjectKind::Chunk, 3, 7, b"chunk");
        assert_eq!(open(ObjectKind::Chunk, 3, 7, &chunk), b"chunk");
        // Another chunk number.
        refused(open_object(
            &key(),
            &VAULT_ID,
            ObjectKind::Chunk,
            4,
            7,
            &chunk,
        ));
        // A chunk opened as a head.
        refused(open_object(
            &key(),
            &VAULT_ID,
            ObjectKind::Head,
            0,
            7,
            &chunk,
        ));
    }

    #[test]
    fn a_head_has_no_chunk_number() {
        assert_eq!(
            seal_object(&key(), &VAULT_ID, ObjectKind::Head, 1, 1, &NONCE, b""),
            Err(SealError::HeadChunkNumber)
        );
        let sealed = seal(ObjectKind::Head, 0, 1, b"");
        assert_eq!(
            open_object(&key(), &VAULT_ID, ObjectKind::Head, 1, 1, &sealed),
            Err(SealError::HeadChunkNumber)
        );
    }

    #[test]
    fn altered_bytes_are_refused() {
        let sealed = seal(ObjectKind::Chunk, 1, 2, b"some earlier messages");
        // Every position: the format byte, the nonce, the body, the tag.
        for i in [0, 1, 12, 13, 500, sealed.len() - 17, sealed.len() - 1] {
            let mut altered = sealed.clone();
            altered[i] ^= 0x01;
            let expected = if i == 0 {
                SealError::UnknownFormat
            } else {
                SealError::Authentication
            };
            assert_eq!(
                open_object(&key(), &VAULT_ID, ObjectKind::Chunk, 1, 2, &altered),
                Err(expected),
                "position {i}"
            );
        }
    }

    #[test]
    fn shapes_that_are_no_object_are_refused_before_decryption() {
        let open_err = |sealed: &[u8]| {
            open_object(&key(), &VAULT_ID, ObjectKind::Chunk, 0, 1, sealed).unwrap_err()
        };
        assert_eq!(open_err(&[]), SealError::Truncated);
        assert_eq!(open_err(&[0x02]), SealError::UnknownFormat);
        assert_eq!(open_err(&[0x00; 2000]), SealError::UnknownFormat);
        assert_eq!(open_err(&[OBJECT_FORMAT_V1]), SealError::Truncated);
        assert_eq!(open_err(&[OBJECT_FORMAT_V1; 13]), SealError::Truncated);
        assert_eq!(open_err(&[OBJECT_FORMAT_V1; 29]), SealError::Truncated);
        assert_eq!(
            open_err(&[OBJECT_FORMAT_V1; MIN_OBJECT_BYTES - 1]),
            SealError::Truncated
        );
        // The right smallest size, but not sealed under the key.
        assert_eq!(
            open_err(&[OBJECT_FORMAT_V1; MIN_OBJECT_BYTES]),
            SealError::Authentication
        );
        // One byte more or fewer than a bucket.
        assert_eq!(
            open_err(&[OBJECT_FORMAT_V1; MIN_OBJECT_BYTES + 1]),
            SealError::NotABucket
        );
        let sealed = seal(ObjectKind::Chunk, 0, 1, &[7u8; 3000]);
        assert_eq!(open_err(&sealed[..sealed.len() - 1]), SealError::NotABucket);
        let mut longer = sealed;
        longer.push(0);
        assert_eq!(open_err(&longer), SealError::NotABucket);
    }

    /// Seal an arbitrary padded frame directly, as a holder of the key who
    /// does not follow the padding rule would.
    fn seal_raw(padded: &[u8]) -> Vec<u8> {
        let aad = object_aad(&VAULT_ID, ObjectKind::Chunk, 0, 1);
        let ciphertext =
            aead::seal(Algorithm::ChaCha20Poly1305, &key(), &NONCE, &aad, padded).unwrap();
        let mut sealed = vec![OBJECT_FORMAT_V1];
        sealed.extend_from_slice(&NONCE);
        sealed.extend_from_slice(&ciphertext);
        sealed
    }

    fn open_raw(padded: &[u8]) -> Result<Vec<u8>, SealError> {
        open_object(
            &key(),
            &VAULT_ID,
            ObjectKind::Chunk,
            0,
            1,
            &seal_raw(padded),
        )
    }

    #[test]
    fn padding_that_is_not_canonical_is_refused() {
        let good = pad(b"hello").unwrap();
        assert_eq!(good.len(), 1024);
        assert_eq!(open_raw(&good).unwrap(), b"hello");

        // A non-zero padding byte, first and last.
        for i in [4 + 5, 1023] {
            let mut tampered = good.clone();
            tampered[i] = 1;
            assert_eq!(open_raw(&tampered), Err(SealError::Padding), "{i}");
        }

        // A length that runs past the frame.
        let mut too_long = good.clone();
        too_long[..4].copy_from_slice(&1021u32.to_be_bytes());
        assert_eq!(open_raw(&too_long), Err(SealError::Padding));
        let mut huge = good.clone();
        huge[..4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(open_raw(&huge), Err(SealError::Padding));

        // The right length in a larger bucket than it calls for.
        let mut wrong_bucket = good;
        wrong_bucket.resize(2048, 0);
        assert_eq!(open_raw(&wrong_bucket), Err(SealError::Padding));

        // The largest length the smallest bucket holds is accepted.
        let full = pad(&[9u8; 1020]).unwrap();
        assert_eq!(full.len(), 1024);
        assert_eq!(open_raw(&full).unwrap(), vec![9u8; 1020]);
    }

    #[test]
    fn unpad_refuses_frames_too_short_for_a_length() {
        for len in 0..FRAME_LEN {
            assert_eq!(unpad(vec![0u8; len]), Err(SealError::Padding));
        }
        assert_eq!(unpad(vec![0u8; 1024]), Ok(Vec::new()));
        assert_eq!(unpad(vec![0u8; 1023]), Err(SealError::Padding));
    }

    #[test]
    fn a_different_nonce_gives_a_different_object() {
        let a = seal(ObjectKind::Head, 0, 1, b"same");
        let b = seal_object(
            &key(),
            &VAULT_ID,
            ObjectKind::Head,
            0,
            1,
            &[0x4F; NONCE_LEN],
            b"same",
        )
        .unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), b.len());
        assert_eq!(open(ObjectKind::Head, 0, 1, &b), b"same");
    }

    const GOLDEN_AAD: &str =
        "6c616b6b6f2f7661756c742f6f626a6563742f7631001d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d02010203040a0b0c0d0e0f1011";
    const GOLDEN_SEALED_BLAKE3: &str =
        "5f58c3161b31123bb70b9e0bb4ae28e5df2fcc938107e485f4c746cec58670e1";

    #[test]
    fn the_wire_is_pinned() {
        let aad = object_aad(
            &VAULT_ID,
            ObjectKind::Chunk,
            0x0102_0304,
            0x0A0B_0C0D_0E0F_1011,
        );
        assert_eq!(aad.len(), 22 + 32 + 1 + 4 + 8);
        assert_eq!(hex(&aad), GOLDEN_AAD);

        let sealed = seal(
            ObjectKind::Chunk,
            0x0102_0304,
            0x0A0B_0C0D_0E0F_1011,
            b"saved history",
        );
        assert_eq!(sealed.len(), MIN_OBJECT_BYTES);
        assert_eq!(sealed[0], OBJECT_FORMAT_V1);
        assert_eq!(&sealed[1..13], &NONCE);
        assert_eq!(hex(&hash::blake3(&sealed)), GOLDEN_SEALED_BLAKE3);
    }
}
