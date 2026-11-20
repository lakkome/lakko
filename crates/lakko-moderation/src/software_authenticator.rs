// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A software EdDSA "security key" (feature `software-authenticator`).
//!
//! For a test stack and for tests only: it holds an Ed25519 seed in memory
//! and answers a WebAuthn assertion request the way a hardware key with
//! user presence and user verification would, building
//!
//! - `authenticatorData` = `SHA-256(rp_id) ‖ flags(UP | UV) ‖ counter(4 BE)`,
//! - `clientDataJSON` = `{"type":"webauthn.get","challenge":<base64url>,
//!   "origin":<origin>,"crossOrigin":false}`,
//! - the signature over `authenticatorData ‖ SHA-256(clientDataJSON)`,
//!
//! exactly what [`crate::webauthn::verify_assertion`] checks. A deployment
//! never uses it: a real moderator signs with a security key in the panel.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_crypto::hash::sha256;
use lakko_crypto::signing::SigningKey;

use crate::moderators::{ModeratorKey, ModeratorList};
use crate::statement::{SignedStatement, Statement};
use crate::webauthn::Assertion;
use crate::ModerationError;

/// User present.
const FLAG_USER_PRESENT: u8 = 0x01;
/// User verified.
const FLAG_USER_VERIFIED: u8 = 0x04;

/// An Ed25519 key that answers assertions in software.
pub struct SoftwareAuthenticator {
    key: SigningKey,
    counter: u32,
}

impl SoftwareAuthenticator {
    /// The authenticator of `seed`, with signature counter 0.
    #[must_use]
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Self {
            key: SigningKey::from_seed(seed),
            counter: 0,
        }
    }

    /// The same key reporting `counter` in its authenticator data.
    #[must_use]
    pub const fn with_counter(mut self, counter: u32) -> Self {
        self.counter = counter;
        self
    }

    /// The credential public key, as a moderator list entry holds it.
    #[must_use]
    pub fn moderator_key(&self) -> ModeratorKey {
        ModeratorKey::Ed25519(self.key.verifying_key().to_bytes())
    }

    /// An assertion over `challenge` for the RP id and origin of `list`.
    #[must_use]
    pub fn assert(&self, list: &ModeratorList, challenge: &[u8; 32]) -> Assertion {
        let client_data_json = serde_json::json!({
            "type": "webauthn.get",
            "challenge": URL_SAFE_NO_PAD.encode(challenge),
            "origin": list.origin,
            "crossOrigin": false,
        })
        .to_string()
        .into_bytes();
        let mut authenticator_data = sha256(list.rp_id.as_bytes()).to_vec();
        authenticator_data.push(FLAG_USER_PRESENT | FLAG_USER_VERIFIED);
        authenticator_data.extend_from_slice(&self.counter.to_be_bytes());
        let mut message = authenticator_data.clone();
        message.extend_from_slice(&sha256(&client_data_json));
        Assertion {
            authenticator_data,
            client_data_json,
            signature: self.key.sign(&message).to_bytes().to_vec(),
        }
    }

    /// Sign `statement` for `list`: encode it (refusing what a decoder
    /// refuses) and assert over its challenge.
    ///
    /// # Errors
    ///
    /// [`ModerationError::Invalid`] for a statement that does not encode.
    pub fn sign_statement(
        &self,
        list: &ModeratorList,
        statement: &Statement,
    ) -> Result<SignedStatement, ModerationError> {
        let encoded = statement.encode_checked()?;
        Ok(SignedStatement {
            statement: encoded,
            assertion: self.assert(list, &statement.challenge()),
        })
    }
}

impl std::fmt::Debug for SoftwareAuthenticator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SoftwareAuthenticator")
            .field("counter", &self.counter)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::moderators::ModeratorEntry;
    use crate::reason::Reason;
    use crate::statement::{reason_commitment, Action};
    use crate::target::Target;
    use crate::webauthn::verify_assertion;

    fn list(auth: &SoftwareAuthenticator) -> ModeratorList {
        ModeratorList {
            list_version: 4,
            issued_at_ms: 1,
            rp_id: "moderation.example".into(),
            origin: "https://moderation.example".into(),
            entries: vec![ModeratorEntry {
                moderator_id: [5; 16],
                name: "dev".into(),
                key: auth.moderator_key(),
                credential_id: vec![1],
                x25519_public: [9; 32],
                added_at_ms: 0,
                revoked_at_ms: 0,
            }],
        }
    }

    #[test]
    fn assertions_verify_under_the_listed_key() {
        let auth = SoftwareAuthenticator::from_seed(&[3; 32]).with_counter(9);
        let l = list(&auth);
        let a = auth.assert(&l, &[7; 32]);
        assert_eq!(a.authenticator_data.len(), 37);
        assert_eq!(&a.authenticator_data[33..], &9u32.to_be_bytes());
        assert_eq!(
            verify_assertion(&l, &auth.moderator_key(), &[7; 32], &a),
            Ok(())
        );
        assert!(verify_assertion(&l, &auth.moderator_key(), &[8; 32], &a).is_err());
    }

    #[test]
    fn a_signed_statement_verifies_against_the_list() {
        let auth = SoftwareAuthenticator::from_seed(&[3; 32]);
        let l = list(&auth);
        let statement = Statement {
            statement_id: [1; 16],
            action: Action::Suspend,
            target: Target::account(&[7; 32]).unwrap(),
            reason_commitment: reason_commitment(Reason::Spam, &[2; 16]),
            until_ms: 0,
            issued_at_ms: 10,
            moderator_id: [5; 16],
            list_version: 4,
        };
        let signed = auth.sign_statement(&l, &statement).unwrap();
        let back = SignedStatement::decode(&signed.encode().unwrap()).unwrap();
        assert_eq!(back.verify(&l), Ok(statement));
        // Another key under the same moderator id does not verify.
        let other = SoftwareAuthenticator::from_seed(&[4; 32]);
        let forged = other.sign_statement(&l, &back.verify(&l).unwrap()).unwrap();
        assert_eq!(forged.verify(&l), Err(ModerationError::BadSignature));
    }
}
