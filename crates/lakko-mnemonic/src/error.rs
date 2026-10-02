// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Error type for `lakko-mnemonic`.

use thiserror::Error;

/// Mnemonic parsing / construction failures.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MnemonicError {
    /// Phrase has the wrong number of words. Lakko fixes this at
    /// 12; anything else is rejected before the upstream BIP39
    /// parser sees it.
    #[error("expected 12 words, got {got}")]
    WrongWordCount {
        /// The actual count parsed from the input.
        got: usize,
    },

    /// One of the words is not in the BIP39 English wordlist, OR
    /// the BIP39 checksum failed. The variant intentionally
    /// collapses both cases — distinguishing them in error
    /// messages would only help an attacker probe the wordlist.
    #[error("invalid mnemonic phrase (unknown word or bad checksum)")]
    InvalidPhrase,
}
