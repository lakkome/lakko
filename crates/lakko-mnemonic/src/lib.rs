// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! BIP39 12-word mnemonic for Lakko's account recovery.
//!
//! ## Why a wrapper crate
//!
//! `bip39` (the upstream Rust crate) supports 12 / 15 / 18 / 21 /
//! 24-word mnemonics across nine languages. Lakko fixes the shape
//! to **12 English words** (128 bits of entropy, ADR-0064; it was
//! 24 until then), the only shape the product offers.
//! Locking the choice down at this layer means:
//!
//!   * `Mnemonic::generate()` cannot accidentally pick another
//!     length elsewhere in the codebase.
//!   * `Mnemonic::from_phrase()` rejects any non-12-word input
//!     before it reaches the BIP39 wordlist lookup.
//!   * The wasm-bindgen surface (`crates/lakko-ffi-wasm/src/mnemonic.rs`)
//!     can expose an opaque type rather than a generic enum.
//!
//! The recovery flow uses the **64-byte seed** the BIP39 PBKDF2
//! step produces — that is the byte stream we feed into OPAQUE-PAKE
//! as the secret in [`RegistrationSession::start_recovery`](
//! ../lakko_auth_client/struct.RegistrationSession.html#method.start_recovery).
//! Lakko intentionally calls `to_seed()` with an **empty BIP39
//! passphrase** — recovery must work with the words alone (the
//! whole point is the user has lost their typed password). Adding
//! a second secret would defeat that.
//!
//! ## What else lives here
//!
//!   * [`word_check`] — the local three-word check ("enter words 7, 3 and
//!     12"): a device that holds the phrase asks for three words at random
//!     positions and compares them itself. No server takes part and nothing
//!     about the words is stored or sent anywhere.
//!   * [`vault`] — the passkey vault: the 16-byte phrase entropy sealed
//!     under a passkey's WebAuthn PRF output (ADR-0064 decision 6).
//!
//! ## Security notes
//!
//! * Mnemonic phrases are **the keys to the account**. The crate
//!   wraps the phrase + seed in [`SecretMnemonic`] / `SecretSeed`
//!   types whose `Debug` impls elide their contents and which
//!   zeroize on drop.
//! * The phrase MUST be displayed to the user exactly once at
//!   signup. Never store it server-side; never persist it
//!   client-side without explicit user consent and at-rest
//!   protection.
//! * BIP39 includes a 8-bit checksum on the entropy, which guards
//!   against single-word typos at recovery time. We surface
//!   typos as [`MnemonicError::InvalidPhrase`].
//!
//! ## Example
//!
//! ```no_run
//! use lakko_mnemonic::Mnemonic;
//!
//! // Signup: generate a fresh 12-word mnemonic.
//! let m = Mnemonic::generate();
//! let phrase = m.phrase();
//! // Show `phrase` to the user — display once, never store on the server.
//!
//! // Derive the OPAQUE recovery seed.
//! let seed = m.to_seed();
//! // `seed.as_ref()` → &[u8; 64], hand to RegistrationSession::start_recovery.
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;
mod mnemonic;
mod secret;
pub mod vault;
pub mod word_check;

pub use error::MnemonicError;
pub use mnemonic::{Mnemonic, SecretEntropy, SecretSeed, ENTROPY_LEN, WORD_COUNT};
pub use secret::SecretMnemonic;

/// Fuzz-only entry point. Marked `#[doc(hidden)]` — the sole harness
/// surface for `crates/lakko-mnemonic/fuzz/`, **not** part of the
/// supported public API.
#[doc(hidden)]
pub mod fuzz_api {
    use crate::Mnemonic;

    /// Drive the BIP39 phrase + entropy parsers on attacker-influenced
    /// bytes.
    ///
    /// [`Mnemonic::from_phrase`] is the recovery-phrase parser (the
    /// phrase is user-typed or device-relayed during recovery);
    /// [`Mnemonic::from_entropy`] rebuilds the phrase from the entropy
    /// a passkey-vault blob opens to (ADR-0064), which whoever stored
    /// the blob chose. The harness runs both on the
    /// same input — the raw bytes through `from_entropy` (the 16-byte
    /// length gate → the BIP39 checksum reconstruction) and, when the
    /// bytes are valid UTF-8, the string through `from_phrase` (the
    /// 12-word count gate → the wordlist lookup → the checksum). Any
    /// panic / overflow here is a vulnerability.
    #[must_use]
    pub fn parse_mnemonic(bytes: &[u8]) -> bool {
        let entropy_ok = Mnemonic::from_entropy(bytes).is_ok();
        let phrase_ok = match core::str::from_utf8(bytes) {
            Ok(s) => Mnemonic::from_phrase(s).is_ok(),
            Err(_) => false,
        };
        entropy_ok || phrase_ok
    }
}
