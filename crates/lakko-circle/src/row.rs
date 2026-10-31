// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Sealed rows (ADR-0086 section 2).
//!
//! A row is one card, sealed under its own random key; the row key is wrapped
//! under the epoch key current when the row was written. A removal moves the
//! epoch and leaves every row where it is: a row keeps its epoch, and a
//! contact who joined later reaches that epoch through the chain.
//!
//! ```text
//! row    := format(1) = 0x01 || epoch(8) || wrapped_row_key(60) || nonce(12) || ciphertext
//! plain  := card_len(2) || card || zeros, ROW_PADDED_LEN bytes
//! aad    := "lakko/circle/row/v1\0" || directory_id || row_index(4) || epoch(8)
//! ```
//!
//! Every row has one length, so the server cannot tell a long name from a
//! short one. The associated data ties the row to its directory, its index
//! and its epoch, so the server cannot serve one row as another.
//!
//! The epoch key opens a row, and every contact of the owner holds it, so a
//! row alone hides a card from nobody in the circle. What a client stores here
//! is the card sealed under its search terms ([`crate::card`]), which the
//! person it describes sealed: walking the rows then yields only sealed cards.
//! The row keeps the card from people outside the circle and from anyone
//! removed before it was written.
//!
//! The card is opaque here; a reader decides what it means. `open_row` runs
//! on bytes the Delivery Service returns: lengths and the format byte are
//! checked first, the padding after authentication, and nothing panics
//! (fuzz target `row_open`).

use lakko_crypto::aead::{self, AeadKey, Algorithm, Nonce, NONCE_LEN, TAG_LEN};
use lakko_crypto::{random, SecretBytes};

use crate::card::SEALED_CARD_LEN;
use crate::codec::{pad, unpad, Reader, LEN_FIELD};
use crate::epoch::EpochKey;
use crate::error::CircleError;
use crate::filter::MAX_ROWS;
use crate::keys::{open_wrap, seal_wrap, DIRECTORY_ID_LEN, KEY_LEN, WRAP_LEN};

const FORMAT_V1: u8 = 0x01;
const AAD_DOMAIN: &[u8] = b"lakko/circle/row/v1\0";

/// Length of a row's padded plaintext: one sealed card and its length.
pub const ROW_PADDED_LEN: usize = SEALED_CARD_LEN + LEN_FIELD;
/// Most bytes a row carries: one sealed card. The longest plain card is
/// [`crate::card::CARD_MAX_LEN`].
pub const ROW_CARD_MAX_LEN: usize = ROW_PADDED_LEN - LEN_FIELD;
/// Length of every sealed row.
pub const ROW_LEN: usize = 1 + 8 + WRAP_LEN + NONCE_LEN + ROW_PADDED_LEN + TAG_LEN;

fn aad(directory_id: &[u8; DIRECTORY_ID_LEN], row: u32, epoch: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(AAD_DOMAIN.len() + DIRECTORY_ID_LEN + 12);
    out.extend_from_slice(AAD_DOMAIN);
    out.extend_from_slice(directory_id);
    out.extend_from_slice(&row.to_be_bytes());
    out.extend_from_slice(&epoch.to_be_bytes());
    out
}

/// Seal `card` as row `row` under the epoch key `key`.
///
/// # Errors
///
/// [`CircleError::CardTooLong`] past [`ROW_CARD_MAX_LEN`],
/// [`CircleError::RowOutOfRange`] past the row bound, [`CircleError::Seal`]
/// on a cipher failure.
pub fn seal_row(key: &EpochKey, row: u32, card: &[u8]) -> Result<Vec<u8>, CircleError> {
    if row >= MAX_ROWS {
        return Err(CircleError::RowOutOfRange);
    }
    let padded = pad(card, ROW_PADDED_LEN)?;

    let binding = aad(key.directory_id(), row, key.epoch());
    let row_key = random::secret_bytes::<KEY_LEN>();
    let wrapped = seal_wrap(&key.row_wrap_key(), &binding, &row_key)?;
    let mut nonce: Nonce = [0u8; NONCE_LEN];
    random::fill(&mut nonce);
    let aead_key: AeadKey = row_key;
    let sealed = aead::seal(
        Algorithm::ChaCha20Poly1305,
        &aead_key,
        &nonce,
        &binding,
        &padded,
    )
    .map_err(|_| CircleError::Seal)?;

    let mut out = Vec::with_capacity(ROW_LEN);
    out.push(FORMAT_V1);
    out.extend_from_slice(&key.epoch().to_be_bytes());
    out.extend_from_slice(&wrapped);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&sealed);
    if out.len() == ROW_LEN {
        Ok(out)
    } else {
        Err(CircleError::Seal)
    }
}

/// The epoch whose key opens this row. Read in the clear, so a reader knows
/// how far down the chain to walk.
///
/// # Errors
///
/// [`CircleError::Length`] or [`CircleError::UnknownFormat`].
pub fn row_epoch(bytes: &[u8]) -> Result<u64, CircleError> {
    if bytes.len() != ROW_LEN {
        return Err(CircleError::Length);
    }
    let mut r = Reader::new(bytes);
    if r.u8()? != FORMAT_V1 {
        return Err(CircleError::UnknownFormat);
    }
    r.u64()
}

/// Open row `row` under the key of the epoch it names.
///
/// # Errors
///
/// [`CircleError::WrongEpoch`] when `key` is another epoch's,
/// [`CircleError::Authentication`] when it does not open,
/// [`CircleError::Malformed`] on a padding that is not the canonical one, and
/// what [`row_epoch`] returns.
pub fn open_row(key: &EpochKey, row: u32, bytes: &[u8]) -> Result<Vec<u8>, CircleError> {
    let epoch = row_epoch(bytes)?;
    if epoch != key.epoch() {
        return Err(CircleError::WrongEpoch);
    }
    let mut r = Reader::new(bytes);
    r.take(1 + 8)?;
    let wrapped: [u8; WRAP_LEN] = r.array()?;
    let nonce: Nonce = r.array()?;
    let ciphertext = r.take(r.remaining())?;

    let binding = aad(key.directory_id(), row, epoch);
    let row_key: SecretBytes<KEY_LEN> = open_wrap(&key.row_wrap_key(), &binding, &wrapped)?;
    let aead_key: AeadKey = row_key;
    let padded = aead::open(
        Algorithm::ChaCha20Poly1305,
        &aead_key,
        &nonce,
        &binding,
        ciphertext,
    )
    .map_err(|_| CircleError::Authentication)?;
    parse_padded(&padded)
}

/// The card inside a padded plaintext.
pub(crate) fn parse_padded(padded: &[u8]) -> Result<Vec<u8>, CircleError> {
    unpad(padded, ROW_PADDED_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(epoch: u8) -> EpochKey {
        EpochKey::from_root(
            [1; 32],
            u64::from(epoch),
            &SecretBytes::new([epoch; KEY_LEN]),
        )
    }

    #[test]
    fn a_row_opens_to_its_card() {
        let k = key(2);
        let row = seal_row(&k, 5, b"a card").unwrap();
        assert_eq!(row.len(), ROW_LEN);
        assert_eq!(row_epoch(&row), Ok(2));
        assert_eq!(open_row(&k, 5, &row).unwrap(), b"a card");
    }

    #[test]
    fn every_row_has_one_length() {
        let k = key(2);
        assert_eq!(
            seal_row(&k, 0, b"").unwrap().len(),
            seal_row(&k, 0, &[7u8; ROW_CARD_MAX_LEN]).unwrap().len()
        );
    }

    #[test]
    fn a_row_is_bound_to_its_index_and_epoch_key() {
        let k = key(2);
        let row = seal_row(&k, 5, b"a card").unwrap();
        assert_eq!(open_row(&k, 6, &row), Err(CircleError::Authentication));
        assert_eq!(open_row(&key(3), 5, &row), Err(CircleError::WrongEpoch));
        let mut relabelled = row;
        relabelled[1..9].copy_from_slice(&3u64.to_be_bytes());
        assert_eq!(
            open_row(&key(3), 5, &relabelled),
            Err(CircleError::Authentication)
        );
    }

    #[test]
    fn a_card_past_the_bound_is_refused() {
        let k = key(2);
        assert_eq!(
            seal_row(&k, 0, &[0u8; ROW_CARD_MAX_LEN + 1]),
            Err(CircleError::CardTooLong)
        );
        assert_eq!(
            seal_row(&k, MAX_ROWS, b"x"),
            Err(CircleError::RowOutOfRange)
        );
    }

    #[test]
    fn padding_is_canonical() {
        let mut padded = vec![0u8; ROW_PADDED_LEN];
        padded[1] = 1;
        padded[2] = b'x';
        assert_eq!(parse_padded(&padded), Ok(b"x".to_vec()));
        padded[100] = 1;
        assert_eq!(parse_padded(&padded), Err(CircleError::Malformed));
        let mut too_long = vec![0u8; ROW_PADDED_LEN];
        too_long[0] = 0xFF;
        assert_eq!(parse_padded(&too_long), Err(CircleError::Malformed));
    }
}
