// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A bounded reader and the length-prefix writers every codec here uses.

use crate::ModerationError;

/// Reads fields front to back; every read checks the remaining length first.
pub(crate) struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self { rest: bytes }
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], ModerationError> {
        if self.rest.len() < n {
            return Err(ModerationError::Malformed("truncated"));
        }
        let (head, tail) = self.rest.split_at(n);
        self.rest = tail;
        Ok(head)
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N], ModerationError> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, ModerationError> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16, ModerationError> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, ModerationError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    pub(crate) fn u64(&mut self) -> Result<u64, ModerationError> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    /// A one-byte length, then that many bytes, at most `max`.
    pub(crate) fn bytes8(&mut self, max: usize) -> Result<&'a [u8], ModerationError> {
        let n = usize::from(self.u8()?);
        if n > max {
            return Err(ModerationError::Malformed("length over bound"));
        }
        self.take(n)
    }

    /// A two-byte length, then that many bytes, at most `max`.
    pub(crate) fn bytes16(&mut self, max: usize) -> Result<&'a [u8], ModerationError> {
        let n = usize::from(self.u16()?);
        if n > max {
            return Err(ModerationError::Malformed("length over bound"));
        }
        self.take(n)
    }

    /// A four-byte length, then that many bytes, at most `max`.
    pub(crate) fn bytes32(&mut self, max: usize) -> Result<&'a [u8], ModerationError> {
        let n = usize::try_from(self.u32()?)
            .map_err(|_| ModerationError::Malformed("length over bound"))?;
        if n > max {
            return Err(ModerationError::Malformed("length over bound"));
        }
        self.take(n)
    }

    pub(crate) fn remaining(&self) -> &'a [u8] {
        self.rest
    }

    pub(crate) fn finish(self) -> Result<(), ModerationError> {
        if self.rest.is_empty() {
            Ok(())
        } else {
            Err(ModerationError::Malformed("trailing bytes"))
        }
    }
}

pub(crate) fn put8(out: &mut Vec<u8>, field: &[u8], max: usize) -> Result<(), ModerationError> {
    if field.len() > max {
        return Err(ModerationError::Invalid("length over bound"));
    }
    out.push(u8::try_from(field.len()).map_err(|_| ModerationError::Invalid("length over bound"))?);
    out.extend_from_slice(field);
    Ok(())
}

pub(crate) fn put16(out: &mut Vec<u8>, field: &[u8], max: usize) -> Result<(), ModerationError> {
    if field.len() > max {
        return Err(ModerationError::Invalid("length over bound"));
    }
    let n =
        u16::try_from(field.len()).map_err(|_| ModerationError::Invalid("length over bound"))?;
    out.extend_from_slice(&n.to_be_bytes());
    out.extend_from_slice(field);
    Ok(())
}

pub(crate) fn put32(out: &mut Vec<u8>, field: &[u8], max: usize) -> Result<(), ModerationError> {
    if field.len() > max {
        return Err(ModerationError::Invalid("length over bound"));
    }
    let n =
        u32::try_from(field.len()).map_err(|_| ModerationError::Invalid("length over bound"))?;
    out.extend_from_slice(&n.to_be_bytes());
    out.extend_from_slice(field);
    Ok(())
}

/// Strip a domain separator and a version byte.
pub(crate) fn header<'a>(
    bytes: &'a [u8],
    domain: &[u8],
    version: u8,
) -> Result<Reader<'a>, ModerationError> {
    let rest = bytes
        .strip_prefix(domain)
        .ok_or(ModerationError::WrongDomain)?;
    let mut r = Reader::new(rest);
    let v = r.u8()?;
    if v != version {
        return Err(ModerationError::UnknownVersion(v));
    }
    Ok(r)
}
