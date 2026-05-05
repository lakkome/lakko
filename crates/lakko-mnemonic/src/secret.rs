// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Owned secret types for the mnemonic and seed.

use core::fmt;

use zeroize::{Zeroize, ZeroizeOnDrop};

/// Owned mnemonic phrase string that zeroizes on drop. Holds the
/// 24 English words separated by single spaces, exactly as the
/// BIP39 spec writes them.
///
/// `Debug` elides the phrase content and prints only the word
/// count — display the actual phrase via [`SecretMnemonic::as_str`]
/// or by writing it directly into the UI.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct SecretMnemonic(String);

impl SecretMnemonic {
    /// Wrap an existing string. Pass-through; the caller has
    /// already obtained the bytes via BIP39.
    #[must_use]
    pub fn new(phrase: String) -> Self {
        Self(phrase)
    }

    /// View as `&str`. The borrow lives as long as the wrapper.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Number of bytes in the underlying UTF-8 string. Useful
    /// when the caller wants a length without exposing the
    /// content.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` iff the wrapped phrase string is empty (only true
    /// for the moved-from / temporary state).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SecretMnemonic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Only the word count surfaces — never the words themselves.
        let words = self.0.split_whitespace().count();
        f.debug_struct("SecretMnemonic")
            .field("words", &words)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_does_not_leak_words() {
        let s = SecretMnemonic::new("alpha bravo charlie".to_string());
        let printed = format!("{s:?}");
        assert!(printed.contains("words"));
        assert!(!printed.contains("alpha"));
        assert!(!printed.contains("bravo"));
    }

    #[test]
    fn len_and_empty_consistent() {
        let m = SecretMnemonic::new("abc def".to_string());
        assert_eq!(m.len(), 7);
        assert!(!m.is_empty());

        let empty = SecretMnemonic::new(String::new());
        assert_eq!(empty.len(), 0);
        assert!(empty.is_empty());
    }
}
