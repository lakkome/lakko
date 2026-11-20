// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Entry points for the fuzz harness. Not a supported API.
//!
//! Each function takes arbitrary bytes, never panics on them, and asserts
//! the property its decoder promises: whatever decodes re-encodes to the
//! identical bytes. A failed assertion is the finding the fuzzer reports.

use lakko_crypto::aead::AeadKey;

use crate::proof::{self, StorageProof, PROOF_WINDOW_MS};
use crate::record::{ConversationHead, DevicesHead, HistoryChunk, VaultIndex, MAX_RECORD_BYTES};
use crate::seal::{self, ObjectKind, MAX_OBJECT_BYTES, MAX_PADDED_BYTES};

const FUZZ_KEY: [u8; 32] = [0x6B; 32];
const FUZZ_VAULT_ID: [u8; 32] = [0x1D; 32];
const FUZZ_SEED: [u8; 32] = [0x5A; 32];

/// Decode the bytes as a conversation head and as a devices head.
///
/// # Panics
///
/// When a decoded head does not re-encode to the identical bytes.
pub fn head_decode(data: &[u8]) {
    if let Ok(head) = ConversationHead::decode(data) {
        assert!(data.len() <= MAX_RECORD_BYTES, "a decoded head is bounded");
        assert!(head.validate().is_ok(), "a decoded head validates");
        assert_eq!(
            head.encode().as_deref(),
            Ok(data),
            "the head encoding is canonical"
        );
        reentry_seeds_in_place(data, &head.reentry);
    }
    if let Ok(head) = DevicesHead::decode(data) {
        assert!(head.validate().is_ok(), "a decoded devices head validates");
        assert_eq!(
            head.encode().as_deref(),
            Ok(data),
            "the devices head encoding is canonical"
        );
        reentry_seeds_in_place(data, &head.reentry);
    }
}

/// Every decoded re-entry epoch's access seed is the 32 bytes that follow its
/// epoch number in the input, verbatim (ADR-0064 section 7 amendment): a
/// decoder that shifted, truncated or zeroed the seed would hand an entering
/// device a key that signs nothing the Delivery Service registered.
fn reentry_seeds_in_place(data: &[u8], reentry: &[crate::record::ReentryEpoch]) {
    for epoch in reentry {
        let mut needle = epoch.epoch.to_be_bytes().to_vec();
        needle.extend_from_slice(epoch.ds_access_seed.as_bytes());
        needle.extend_from_slice(
            &u32::try_from(epoch.group_info.len())
                .unwrap_or(0)
                .to_be_bytes(),
        );
        needle.extend_from_slice(&epoch.group_info);
        assert!(
            data.windows(needle.len()).any(|w| w == needle.as_slice()),
            "a re-entry epoch's access seed is read in place"
        );
    }
}

/// Decode the bytes as a history chunk.
///
/// # Panics
///
/// When a decoded chunk does not re-encode to the identical bytes.
pub fn chunk_decode(data: &[u8]) {
    let Ok(chunk) = HistoryChunk::decode(data) else {
        return;
    };
    assert!(data.len() <= MAX_RECORD_BYTES, "a decoded chunk is bounded");
    assert!(chunk.validate().is_ok(), "a decoded chunk validates");
    // ADR-0092: a content block and the audio tags are within their bounds.
    for message in &chunk.messages {
        if let Some(c) = &message.content {
            assert!(crate::record::CONTENT_KINDS.contains(&c.kind));
            assert!(
                !c.payload.is_empty()
                    && c.payload.len() <= crate::record::MAX_CONTENT_PAYLOAD_BYTES
            );
            assert!(c.state.len() <= crate::record::MAX_CONTENT_STATE_BYTES);
        }
        for a in &message.attachments {
            assert!(a.title.len() <= crate::record::MAX_AUDIO_TAG_BYTES);
            assert!(a.artist.len() <= crate::record::MAX_AUDIO_TAG_BYTES);
        }
    }
    assert_eq!(
        chunk.encode().as_deref(),
        Ok(data),
        "the chunk encoding is canonical"
    );
    // Merging a chunk with itself changes nothing but the order and the
    // repeats: every id survives exactly once.
    let merged = crate::merge::merge_messages(&chunk.messages, &chunk.messages);
    assert!(
        merged.windows(2).all(|w| w[0].message_id < w[1].message_id),
        "a merge is sorted and holds each id once"
    );
}

/// Decode the bytes as an index.
///
/// # Panics
///
/// When a decoded index does not re-encode to the identical bytes.
pub fn index_decode(data: &[u8]) {
    let Ok(index) = VaultIndex::decode(data) else {
        return;
    };
    assert!(data.len() <= MAX_RECORD_BYTES, "a decoded index is bounded");
    assert!(index.validate().is_ok(), "a decoded index validates");
    assert_eq!(
        index.encode().as_deref(),
        Ok(data),
        "the index encoding is canonical"
    );
}

/// Open the bytes as a sealed object under a fixed key, as a head and as a
/// chunk, and run the padding parser on them directly.
///
/// A fuzzer cannot forge the authentication tag, so the sealed path
/// exercises the size cap, the format byte, the bucket shape and the
/// refusal; the parser that runs after authentication is reached through
/// the second half.
///
/// # Panics
///
/// When an object that opens does not re-seal to the identical bytes, or a
/// frame that unpads does not re-pad to the identical bytes.
pub fn object_open(data: &[u8]) {
    let key = AeadKey::new(FUZZ_KEY);
    for (kind, chunk_no) in [(ObjectKind::Head, 0), (ObjectKind::Chunk, 3)] {
        if let Ok(plaintext) = seal::open_object(&key, &FUZZ_VAULT_ID, kind, chunk_no, 7, data) {
            assert!(
                data.len() <= MAX_OBJECT_BYTES,
                "an opened object is bounded"
            );
            let nonce: Option<[u8; 12]> = data.get(1..13).and_then(|n| n.try_into().ok());
            let resealed = nonce.map(|nonce| {
                seal::seal_object(&key, &FUZZ_VAULT_ID, kind, chunk_no, 7, &nonce, &plaintext)
            });
            assert!(
                matches!(resealed, Some(Ok(ref bytes)) if bytes == data),
                "an object that opens re-seals to the identical bytes"
            );
        }
    }
    if data.len() <= MAX_PADDED_BYTES {
        if let Ok(plaintext) = seal::unpad(data.to_vec()) {
            assert_eq!(
                seal::pad(&plaintext).as_deref(),
                Ok(data),
                "a frame that unpads re-pads to the identical bytes"
            );
        }
    }
}

/// Parse the bytes as a storage proof header and verify whatever parses.
///
/// # Panics
///
/// When a parsed proof does not re-encode to the identical text, or when a
/// proof this function did not sign verifies.
pub fn proof_parse(data: &[u8]) {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    let Ok(parsed) = StorageProof::parse(text) else {
        return;
    };
    assert_eq!(parsed.encode(), text, "the header encoding is canonical");
    let public_key = proof::public_key(&FUZZ_SEED);
    let genuine = proof::sign(&FUZZ_SEED, "GET", "/vault/00/head", b"", parsed.unix_ms);
    let outcome = parsed.verify(&public_key, "GET", "/vault/00/head", b"", parsed.unix_ms);
    assert_eq!(
        outcome.is_ok(),
        genuine.as_deref() == Ok(text),
        "only the genuine proof verifies"
    );
    // The far edges of the clock must not overflow.
    let _ = parsed.verify(&public_key, "GET", "/vault/00/head", b"", 0);
    let _ = parsed.verify(&public_key, "GET", "/vault/00/head", b"", u64::MAX);
    let _ = parsed.verify(
        &public_key,
        "GET",
        "/vault/00/head",
        b"",
        parsed.unix_ms.saturating_add(PROOF_WINDOW_MS),
    );
}
