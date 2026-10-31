// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A bounded reader over untrusted bytes. Every take checks the remaining
//! length first, so no decoder in this crate indexes past its input.

use crate::error::CircleError;

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
