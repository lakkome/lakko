// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0092 section 4: the client half of the channel-poll credential.
//!
//! Once per account: [`AnonCredentialSecret::registration`] goes to
//! `PUT /auth/anon-credential/commitment`. Once per period: fetch the
//! period's issuer key from the key route (never from a response), send
//! [`AnonCredentialSecret::issuance_request`], turn the answer into a stored
//! credential with [`AnonCredentialSecret::finalize_issuance`]. Per vote:
//! [`AnonCredentialSecret::present`] for the channel post. The issuer and
//! the verifier are services' and are not on this surface.
//!
//! Errors: [`FfiError::AnonCred`] `"<code>: <reason>"`
//! (`lakko_anon_cred::AnonCredError::code`, or `bad-length`).

use std::sync::Arc;

use lakko_anon_cred as core;

use crate::FfiError;

fn cred_err(e: core::AnonCredError) -> FfiError {
    FfiError::AnonCred(format!("{}: {e}", e.code()))
}

fn fixed<const N: usize>(v: &[u8], what: &str) -> Result<[u8; N], FfiError> {
    <[u8; N]>::try_from(v).map_err(|_| {
        FfiError::AnonCred(format!(
            "bad-length: {what} must be {N} bytes, got {}",
            v.len()
        ))
    })
}

/// A period's issuer public key, as fetched from the issuer's key route.
#[derive(uniffi::Object)]
pub struct AnonIssuerPublicKey {
    inner: core::IssuerPublicKey,
}

#[uniffi::export]
impl AnonIssuerPublicKey {
    /// Decode the 96-byte key of `period`.
    ///
    /// # Errors
    ///
    /// `malformed`, `non-canonical`, `identity-point`.
    #[uniffi::constructor]
    pub fn decode(period: u64, bytes: Vec<u8>) -> Result<Arc<Self>, FfiError> {
        Ok(Arc::new(Self {
            inner: core::IssuerPublicKey::decode(period, &bytes).map_err(cred_err)?,
        }))
    }

    /// The period.
    #[must_use]
    pub fn period(&self) -> u64 {
        self.inner.period()
    }

    /// The 96-byte key.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        self.inner.to_bytes().to_vec()
    }
}

/// A presentation for one vote.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct AnonPresentation {
    /// The wire bytes for `services/channels`.
    pub bytes: Vec<u8>,
    /// The per-poll tag (the same for every presentation of one account in
    /// one poll).
    pub tag: Vec<u8>,
    /// The credential period.
    pub period: u64,
}

/// The account's credential secret, derived from the lineage seed (the
/// oldest own account seed), so every device computes the same one. Secret;
/// wiped on drop.
#[derive(uniffi::Object)]
pub struct AnonCredentialSecret {
    inner: core::CredentialSecret,
}

#[uniffi::export]
impl AnonCredentialSecret {
    /// From the 32-byte lineage seed.
    ///
    /// # Errors
    ///
    /// `bad-length`, `zero-scalar`.
    #[uniffi::constructor]
    pub fn from_lineage_seed(lineage_seed: Vec<u8>) -> Result<Arc<Self>, FfiError> {
        let seed = fixed::<32>(&lineage_seed, "lineageSeed")?;
        Ok(Arc::new(Self {
            inner: core::CredentialSecret::from_lineage_seed(&seed).map_err(cred_err)?,
        }))
    }

    /// The 32-byte commitment auth stores.
    #[must_use]
    pub fn commitment(&self) -> Vec<u8> {
        self.inner.commitment().to_bytes().to_vec()
    }

    /// The commitment registration (97 bytes) for
    /// `PUT /auth/anon-credential/commitment`.
    ///
    /// # Errors
    ///
    /// `randomness-unavailable`.
    pub fn registration(&self) -> Result<Vec<u8>, FfiError> {
        Ok(self.inner.register().map_err(cred_err)?.to_bytes())
    }

    /// An issuance request for `key`'s period.
    ///
    /// # Errors
    ///
    /// `randomness-unavailable`.
    pub fn issuance_request(&self, key: Arc<AnonIssuerPublicKey>) -> Result<Vec<u8>, FfiError> {
        Ok(self
            .inner
            .issuance_request(&key.inner)
            .map_err(cred_err)?
            .to_bytes())
    }

    /// Check the issuer's response and return the credential to store
    /// (73 bytes).
    ///
    /// # Errors
    ///
    /// `malformed`, `wrong-period`, `bad-proof`.
    pub fn finalize_issuance(
        &self,
        key: Arc<AnonIssuerPublicKey>,
        response: Vec<u8>,
    ) -> Result<Vec<u8>, FfiError> {
        let response = core::IssuanceResponse::decode(&response).map_err(cred_err)?;
        Ok(self
            .inner
            .finalize_issuance(&key.inner, &response)
            .map_err(cred_err)?
            .to_bytes()
            .to_vec())
    }

    /// A fresh presentation of the stored `credential` for the poll posted at
    /// `post_cursor` in the channel `channel_id` (16 bytes), bound to
    /// `message` (the request content).
    ///
    /// # Errors
    ///
    /// `malformed`, `wrong-period`, `message-too-long`, `bad-length`,
    /// `randomness-unavailable`.
    pub fn present(
        &self,
        credential: Vec<u8>,
        key: Arc<AnonIssuerPublicKey>,
        channel_id: Vec<u8>,
        post_cursor: u64,
        message: Vec<u8>,
    ) -> Result<AnonPresentation, FfiError> {
        let credential = core::Credential::decode(&credential).map_err(cred_err)?;
        let scope =
            core::PollScope::channel_post(fixed::<16>(&channel_id, "channelId")?, post_cursor);
        let p = self
            .inner
            .present(&credential, &key.inner, &scope, &message)
            .map_err(cred_err)?;
        Ok(AnonPresentation {
            bytes: p.to_bytes(),
            tag: p.tag().to_vec(),
            period: p.period(),
        })
    }
}

/// The credential period at `unix_secs`.
#[uniffi::export]
#[must_use]
pub fn anon_cred_period_at(unix_secs: u64) -> u64 {
    core::period_at(unix_secs)
}

/// The period length, seconds (30 days).
#[uniffi::export]
#[must_use]
pub fn anon_cred_period_secs() -> u64 {
    core::PERIOD_SECS
}

/// The period of a stored credential.
///
/// # Errors
///
/// `malformed`, `unknown-version`, `non-canonical`.
#[uniffi::export]
pub fn anon_cred_credential_period(credential: Vec<u8>) -> Result<u64, FfiError> {
    Ok(core::Credential::decode(&credential)
        .map_err(cred_err)?
        .period())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_secret_is_deterministic_and_registration_has_its_length() {
        let a = AnonCredentialSecret::from_lineage_seed(vec![5; 32]).unwrap();
        let b = AnonCredentialSecret::from_lineage_seed(vec![5; 32]).unwrap();
        assert_eq!(a.commitment(), b.commitment());
        assert_eq!(a.registration().unwrap().len(), core::REGISTRATION_LEN);
        let Err(FfiError::AnonCred(m)) = AnonCredentialSecret::from_lineage_seed(vec![1; 3]) else {
            panic!("expected an anon-cred error");
        };
        assert!(m.starts_with("bad-length: "));
        assert_eq!(anon_cred_period_at(core::PERIOD_SECS * 3), 3);
    }

    #[test]
    fn issuance_and_presentation_round_trip_against_the_core_issuer() {
        let master = core::MasterSecret::from_bytes([7; 32]);
        let issuer = core::IssuerSecretKey::derive(&master, 5).unwrap();
        let key = AnonIssuerPublicKey::decode(5, issuer.public_key().to_bytes().to_vec()).unwrap();
        let secret = AnonCredentialSecret::from_lineage_seed(vec![9; 32]).unwrap();
        let stored = core::Commitment::decode(&secret.commitment()).unwrap();
        let request =
            core::IssuanceRequest::decode(&secret.issuance_request(key.clone()).unwrap()).unwrap();
        let response = issuer.issue(&request, &stored).unwrap();
        let credential = secret
            .finalize_issuance(key.clone(), response.to_bytes())
            .unwrap();
        assert_eq!(anon_cred_credential_period(credential.clone()).unwrap(), 5);
        let p = secret
            .present(credential, key, vec![3; 16], 42, b"vote".to_vec())
            .unwrap();
        let verifier = core::CredentialVerifier::new(master);
        let scope = core::PollScope::channel_post([3; 16], 42);
        assert!(verifier.verify(&p.bytes, &scope, b"vote", 5).is_ok());
    }
}
