// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `SecretBytes` — a `Vec<u8>` that zeroizes on drop.
//!
//! Used to wrap the OPAQUE `export_key` and `session_key` so that
//! call sites cannot accidentally leak them through `Debug`,
//! `Display`, or stale heap allocations.

use core::fmt;

use zeroize::{Zeroize, ZeroizeOnDrop};

/// Owned secret byte buffer that zeroizes on drop.
///
/// Implements `AsRef<[u8]>` for one-line passing into HKDF /
/// HMAC / etc. Does **not** implement `Display` or `Debug` payload
/// disclosure — `Debug` formats the length only.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct SecretBytes(Vec<u8>);

impl SecretBytes {
    /// Wrap an existing buffer. Pass-through; the caller is
    /// expected to have generated the bytes via OPAQUE.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Length of the wrapped buffer in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` iff the wrapped buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl AsRef<[u8]> for SecretBytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Length only — the bytes themselves never enter logs.
        f.debug_struct("SecretBytes")
            .field("len", &self.0.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_does_not_leak_payload() {
        let s = SecretBytes::new(vec![1, 2, 3, 4]);
        let printed = format!("{s:?}");
        assert!(printed.contains("len"));
        assert!(!printed.contains('1') && !printed.contains('2'));
    }

    #[test]
    fn len_and_empty_are_consistent() {
        let s = SecretBytes::new(vec![1, 2, 3]);
        assert_eq!(s.len(), 3);
        assert!(!s.is_empty());

        let empty = SecretBytes::new(Vec::new());
        assert_eq!(empty.len(), 0);
        assert!(empty.is_empty());
    }
}
