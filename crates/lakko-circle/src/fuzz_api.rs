// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Entry points for the fuzz harness. Not a supported API.
//!
//! Each function takes arbitrary bytes, never panics on them, and asserts
//! what its decoder promises. A failed assertion is the finding the fuzzer
//! reports.

use lakko_crypto::SecretBytes;

use crate::card::{self, open_card, seal_card, CARD_MAX_LEN, CARD_PADDED_LEN, SEALED_CARD_LEN};
use crate::epoch::EpochKey;
use crate::filter::{self, filter_header, open_filter};
use crate::head::{DirectoryHead, MemberGrant, HEAD_LEN};
use crate::keys::{self, KEY_LEN, OPRF_OUTPUT_LEN, WRAP_LEN};
use crate::row::{self, open_row, row_epoch, ROW_PADDED_LEN};
use crate::tree::{member_epoch_key, TreeOwner};
use crate::writer::DirectoryWriter;

const FUZZ_ACCOUNT: [u8; KEY_LEN] = [0x6B; KEY_LEN];
const FUZZ_TERM: [u8; OPRF_OUTPUT_LEN] = [0x7C; OPRF_OUTPUT_LEN];

fn fuzz_key() -> EpochKey {
    EpochKey::from_root([0x1D; 32], 3, &SecretBytes::new([0x2E; KEY_LEN]))
}

/// Decode the bytes as a head.
///
/// # Panics
///
/// When a decoded head does not re-encode to the identical bytes.
pub fn head_decode(data: &[u8]) {
    if let Ok(head) = DirectoryHead::decode(data) {
        assert_eq!(&head.encode()[..], data, "the head encoding is canonical");
    }
}

/// Decode the bytes as a grant.
///
/// # Panics
///
/// When a decoded grant does not re-encode to the identical bytes.
pub fn grant_decode(data: &[u8]) {
    if let Ok(grant) = MemberGrant::decode(data) {
        assert_eq!(&grant.encode()[..], data, "the grant encoding is canonical");
    }
}

/// Walk a path over hostile edges: a head, then edges of 64 bytes each
/// (`node(4) || wrap(60)`).
///
/// # Panics
///
/// When a walk over bytes the fuzzer cannot have sealed yields a key.
pub fn path_walk(data: &[u8]) {
    let grant = MemberGrant {
        directory_id: [0x1D; 32],
        leaf: 3,
        leaf_key: SecretBytes::new([0x2E; KEY_LEN]),
    };
    if data.len() < HEAD_LEN {
        return;
    }
    let (head_bytes, rest) = data.split_at(HEAD_LEN);
    let Ok(head) = DirectoryHead::decode(head_bytes) else {
        return;
    };
    let mut edges = Vec::new();
    for item in rest.chunks_exact(4 + WRAP_LEN).take(32) {
        let (node, wrap) = item.split_at(4);
        let (Ok(node), Ok(wrap)) = (<[u8; 4]>::try_from(node), <[u8; WRAP_LEN]>::try_from(wrap))
        else {
            continue;
        };
        edges.push((u32::from_be_bytes(node), wrap));
    }
    // The fuzzer can choose the read key in the head only by computing it,
    // so anything but a mismatch or a refused wrap is a finding unless the
    // walk used no edge at all.
    if let Ok(key) = member_epoch_key(&grant, &head, &edges) {
        assert_eq!(key.read_public_key(), head.read_pk);
    }
}

/// Parse the bytes as a sealed filter and as a filter's plaintext, and test a
/// fixed term against whatever parses.
///
/// # Panics
///
/// When an opened filter's values do not stay inside its row count.
pub fn filter_open(data: &[u8]) {
    let _ = filter_header(data);
    let _ = open_filter(&fuzz_key(), data);
    let hash_key = SecretBytes::new([0x3F; KEY_LEN]);
    if let Ok(opened) = filter::parse_plain(hash_key, 1, data) {
        for row in opened.lookup(&[0x51; 32]) {
            assert!(row < opened.row_count(), "a row past the row count");
        }
    }
}

/// Parse the bytes as a sealed row and as a row's padded plaintext.
///
/// # Panics
///
/// When a parsed card is longer than the padded plaintext.
pub fn row_open(data: &[u8]) {
    let _ = row_epoch(data);
    let _ = open_row(&fuzz_key(), 0, data);
    if let Ok(card) = row::parse_padded(data) {
        assert!(card.len() < ROW_PADDED_LEN, "a card longer than its row");
    }
}

/// Parse the bytes as a sealed card and as a card's padded plaintext, and seal
/// them as a card.
///
/// # Panics
///
/// When a parsed card is longer than the padded plaintext, or a card this
/// crate sealed does not open to itself at the one length.
pub fn card_open(data: &[u8]) {
    let _ = open_card(data, &FUZZ_TERM);
    if let Ok(parsed) = card::parse_padded(data) {
        assert!(
            parsed.len() < CARD_PADDED_LEN,
            "a card longer than its padding"
        );
    }
    if data.len() <= CARD_MAX_LEN {
        let Ok(sealed) = seal_card(data, &[FUZZ_TERM]) else {
            return;
        };
        assert_eq!(
            sealed.len(),
            SEALED_CARD_LEN,
            "a sealed card has one length"
        );
        assert_eq!(
            open_card(&sealed, &FUZZ_TERM).as_deref(),
            Ok(data),
            "a sealed card opens to itself"
        );
    }
}

/// Restore a writer from the bytes.
///
/// # Panics
///
/// When a restored state does not re-encode to the identical bytes.
pub fn state_decode(data: &[u8]) {
    if let Ok(writer) = DirectoryWriter::restore(&FUZZ_ACCOUNT, data) {
        assert_eq!(writer.encode_state(), data, "the state is canonical");
    }
    if let Ok(tree) = TreeOwner::decode_state(keys::owner_seed(&FUZZ_ACCOUNT), data) {
        assert_eq!(tree.encode_state(), data, "the tree state is canonical");
    }
}
