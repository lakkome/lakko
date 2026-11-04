// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A bounded reader over untrusted bytes. Every take checks the remaining
//! length first, so no decoder in this crate indexes past its input.

use crate::error::CircleError;

/// Length of the length field of a padded plaintext.
pub(crate) const LEN_FIELD: usize = 2;

/// `card_len(2) || card || zeros`, `padded_len` bytes. The caller has checked
/// that the card fits.
pub(crate) fn pad(card: &[u8], padded_len: usize) -> Result<Vec<u8>, CircleError> {
    let card_len = u16::try_from(card.len()).map_err(|_| CircleError::CardTooLong)?;
    if card.len() + LEN_FIELD > padded_len {
        return Err(CircleError::CardTooLong);
    }
    let mut padded = Vec::with_capacity(padded_len);
    padded.extend_from_slice(&card_len.to_be_bytes());
    padded.extend_from_slice(card);
    padded.resize(padded_len, 0);
    Ok(padded)
}

/// The card inside a padded plaintext. The padding must be the canonical one,
/// all zeros.
pub(crate) fn unpad(padded: &[u8], padded_len: usize) -> Result<Vec<u8>, CircleError> {
    if padded.len() != padded_len {
        return Err(CircleError::Length);
    }
    let mut r = Reader::new(padded);
    let len = usize::from(u16::from_be_bytes(r.array()?));
    if len + LEN_FIELD > padded_len {
        return Err(CircleError::Malformed);
    }
    let card = r.take(len)?.to_vec();
    let padding = r.take(r.remaining())?;
    if padding.iter().any(|b| *b != 0) {
        return Err(CircleError::Malformed);
    }
    Ok(card)
}

/// Bytes still to be read.
pub(crate) struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self { rest: bytes }
    }

    pub(crate) fn take(&mut self, len: usize) -> Result<&'a [u8], CircleError> {
        if self.rest.len() < len {
            return Err(CircleError::Length);
        }
        let (head, tail) = self.rest.split_at(len);
        self.rest = tail;
        Ok(head)
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N], CircleError> {
        self.take(N)?.try_into().map_err(|_| CircleError::Length)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, CircleError> {
        Ok(self.array::<1>()?[0])
    }

    pub(crate) fn u32(&mut self) -> Result<u32, CircleError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    pub(crate) fn u64(&mut self) -> Result<u64, CircleError> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    pub(crate) const fn remaining(&self) -> usize {
        self.rest.len()
    }

    pub(crate) const fn finish(&self) -> Result<(), CircleError> {
        if self.rest.is_empty() {
            Ok(())
        } else {
            Err(CircleError::Length)
        }
    }
}
