// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! BIP39 12-word mnemonic for Lakko's account recovery.
//!
//! ## Why a wrapper crate
//!
//! `bip39` (the upstream Rust crate) supports 12 / 15 / 18 / 21 /
//! 24-word mnemonics across nine languages. Lakko fixes the shape
//! to **12 English words** (128 bits of entropy, ADR-0072; it was
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
//!     under a passkey's WebAuthn PRF output (ADR-0072 decision 6); and the
//!     email vault: the same entropy sealed under a key stretched from the
//!     login handle and the recovery email address (ADR-0075 section 2).
//!   * [`recovery_payload`] — the another-device recovery payload v3
//!     (ADR-0018 Path B, ADR-0074 "Joining after a rotation"): the grant,
//!     the phrase entropy and the account's own predecessor seeds a
//!     confirming device hands a joining one, plus the mailbox row tag.
//!   * [`recovery_email`] — the one rule for what a recovery email address
//!     is and how it is normalised (ADR-0075 section 4), shared with
//!     `services/auth`.
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
pub mod recovery_email;
pub mod recovery_payload;
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
    /// a passkey-vault blob opens to (ADR-0072), which whoever stored
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

    /// Drive the email-vault open path and the recovery-address rule on
    /// attacker-chosen bytes (ADR-0075).
    ///
    /// The sealed blob is served by the server in the answer to a mailed
    /// code, so every byte of it is chosen by whoever controls that answer.
    /// It is opened under a FIXED stretched key (Argon2id is skipped: its
    /// inputs are the person's, not the blob's), which exercises the length
    /// gate, the HKDF step, the AEAD refusal and the entropy rebuild. The
    /// same bytes, when UTF-8, go through `normalize_recovery_email`, and a
    /// normalised address must normalise to itself (the client and the
    /// server each normalise once).
    ///
    /// Returns `(opened, address_stable)`; the harness asserts
    /// `address_stable`.
    #[must_use]
    pub fn open_email_vault(bytes: &[u8]) -> (bool, bool) {
        let opened = crate::vault::fuzz_open_email_vault(bytes);
        let stable = core::str::from_utf8(bytes).map_or(true, |s| {
            crate::recovery_email::normalize_recovery_email(s).map_or(true, |once| {
                crate::recovery_email::normalize_recovery_email(&once).as_deref() == Ok(&once)
            })
        });
        (opened, stable)
    }

    /// Drive the recovery-payload v3 decoder and the row tag on
    /// attacker-chosen bytes (ADR-0074, "Joining after a rotation").
    ///
    /// The plaintext is what a joining device opens from the rendezvous
    /// mailbox, sealed by a device of the account; the row prefix is read
    /// before any key is involved, so whoever writes to that mailbox chooses
    /// it. Returns `None` when the payload was refused, `Some(same)` when it
    /// decoded, `same` telling whether it re-composed to the identical bytes;
    /// it never panics. The harness asserts `same`.
    #[must_use]
    pub fn decode_recovery_payload(bytes: &[u8]) -> Option<bool> {
        let _ = crate::recovery_payload::unwrap_recovery_row(bytes);
        let payload = crate::recovery_payload::decode_recovery_payload(bytes).ok()?;
        let again = crate::recovery_payload::compose_recovery_payload(
            &payload.grant,
            &payload.phrase_entropy,
            &payload.predecessors,
        )
        .ok()?;
        Some(again.as_slice() == bytes)
    }
}
