// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Zeroize-on-drop secret byte buffer.
//!
//! Use [`SecretBytes`] to hold any short-lived key material that crosses
//! crate boundaries. The buffer is zeroed when the value is dropped, and
//! equality comparison is constant-time.

use core::fmt;

use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Fixed-length byte buffer that is zeroed on drop.
///
/// `SecretBytes<N>` is the canonical way to represent secret keys, derived
/// keys and other short-lived secrets in `lakko-crypto`. It implements
/// constant-time equality and refuses to expose its contents through `Debug`.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct SecretBytes<const N: usize>([u8; N]);

impl<const N: usize> SecretBytes<N> {
    /// Wrap a fixed-size array as a secret.
    #[must_use]
    pub const fn new(bytes: [u8; N]) -> Self {
        Self(bytes)
    }

    /// Borrow the underlying bytes. Avoid logging or otherwise leaking.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; N] {
        &self.0
    }

    /// Length of the secret in bytes — always `N`.
    #[must_use]
    pub const fn len(&self) -> usize {
        N
    }

    /// Always `false` — `N == 0` would be unusable; this is here for API parity.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        N == 0
    }
}

impl<const N: usize> From<[u8; N]> for SecretBytes<N> {
    fn from(bytes: [u8; N]) -> Self {
        Self(bytes)
    }
}

impl<const N: usize> PartialEq for SecretBytes<N> {
    /// Constant-time equality.
    fn eq(&self, other: &Self) -> bool {
        self.0.ct_eq(&other.0).into()
    }
}

impl<const N: usize> Eq for SecretBytes<N> {}

impl<const N: usize> fmt::Debug for SecretBytes<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretBytes<{N}>(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_does_not_leak() {
        let s = SecretBytes::<32>::new([0xAA; 32]);
        let dbg = format!("{s:?}");
        assert!(!dbg.contains("AA"), "debug must not leak bytes: {dbg}");
        assert!(dbg.contains("redacted"));
    }

    #[test]
    fn equality_is_value_based() {
        let a = SecretBytes::<16>::new([1; 16]);
        let b = SecretBytes::<16>::new([1; 16]);
        let c = SecretBytes::<16>::new([2; 16]);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn drop_zeroes_memory() {
        // Indirect — we cannot observe freed memory safely. Trust the
        // ZeroizeOnDrop derive macro contract; this test asserts the trait
        // is implemented.
        fn assert_zeroize<T: zeroize::ZeroizeOnDrop>() {}
        assert_zeroize::<SecretBytes<32>>();
    }
}
