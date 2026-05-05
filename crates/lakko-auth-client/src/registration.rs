// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Client-side OPAQUE registration session.
//!
//! Two halves:
//!
//!   * [`RegistrationSession::start_password`] /
//!     [`RegistrationSession::start_recovery`] — produce the
//!     `RegistrationRequest` blob the server consumes in
//!     `/signup/start` and stash an opaque [`ClientRegistration`]
//!     state for later.
//!
//!   * [`RegistrationSession::finish`] — consume the matching
//!     `RegistrationResponse` from the server, hand back a
//!     `RegistrationUpload` blob (which the server stores in
//!     `/signup/finish`) plus the OPAQUE `export_key` (which the
//!     client uses to derive its device root key).
//!
//! `start_password` and `start_recovery` are byte-identical at the
//! OPAQUE layer — the only difference is the input bytes (typed
//! password vs 64-byte BIP39 seed) and the wire field they fill
//! in. The split exists at the API level to make miswiring a
//! type error rather than a comment.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use opaque_ke::{
    ClientRegistration, ClientRegistrationFinishParameters, ClientRegistrationFinishResult,
    RegistrationResponse,
};
use rand::rngs::OsRng;

use crate::cipher_suite::LakkoCipherSuite;
use crate::error::{AuthClientError, Result};
use crate::secret::SecretBytes;

/// Private discriminator — see public `start_password` /
/// `start_recovery` for why this exists.
#[derive(Clone, Copy)]
enum Flow {
    Password,
    Recovery,
}

/// In-flight client-side registration. Holds the opaque-ke
/// `ClientRegistration` state between `start_*` and `finish`.
///
/// Cheap to construct; not `Clone` (the underlying state must be
/// finalized exactly once to avoid replay).
pub struct RegistrationSession {
    state: ClientRegistration<LakkoCipherSuite>,
    flow: Flow,
}

/// Output of [`RegistrationSession::finish`].
///
/// `Debug` is intentionally derived even though it carries an
/// [`export_key`](Self::export_key) — [`SecretBytes`] has a custom
/// `Debug` impl that prints only the length, not the contents, so
/// the derive is safe.
#[derive(Debug)]
pub struct RegistrationOutput {
    /// Base64-encoded `RegistrationUpload`. Goes into
    /// `password_upload` (or `recovery_upload`) on `/signup/finish`.
    pub upload_b64: String,
    /// OPAQUE export key. Used by the client to derive its device
    /// root key and any account-scoped secrets that survive a
    /// reboot. Length is [`crate::EXPORT_KEY_LEN`] bytes for
    /// [`LakkoCipherSuite`].
    pub export_key: SecretBytes,
}

impl RegistrationSession {
    /// Start a password registration.
    ///
    /// Returns the session (to be passed back into `finish` along
    /// with the server's `password_response`) and the base64-encoded
    /// `RegistrationRequest` that goes into the
    /// [`wire::SignupStartRequest`](crate::wire::SignupStartRequest)'s
    /// `password_request` field.
    ///
    /// # Errors
    ///
    /// Returns [`AuthClientError::Opaque`] if the underlying OPRF
    /// blinding fails — extremely unlikely in practice (it would
    /// mean a broken RNG).
    pub fn start_password(password: &[u8]) -> Result<(Self, String)> {
        Self::start_with_flow(password, Flow::Password)
    }

    /// Start a recovery registration. Identical math to
    /// [`Self::start_password`]; the wire field that consumes the
    /// returned base64 string is `recovery_request` instead.
    ///
    /// # Errors
    ///
    /// Same as [`Self::start_password`].
    pub fn start_recovery(seed: &[u8]) -> Result<(Self, String)> {
        Self::start_with_flow(seed, Flow::Recovery)
    }

    fn start_with_flow(secret: &[u8], flow: Flow) -> Result<(Self, String)> {
        let mut rng = OsRng;
        let started = ClientRegistration::<LakkoCipherSuite>::start(&mut rng, secret)
            .map_err(|_| AuthClientError::opaque())?;
        let request_b64 = B64.encode(started.message.serialize());
        Ok((
            Self {
                state: started.state,
                flow,
            },
            request_b64,
        ))
    }

    /// Whether this session was started for the password flow.
    /// Mirror of `flow == Flow::Password`; exposed publicly so
    /// callers (e.g. wire-struct builders in higher layers) can
    /// double-check they paired the right session with the right
    /// server response.
    #[must_use]
    pub fn is_password(&self) -> bool {
        matches!(self.flow, Flow::Password)
    }

    /// Whether this session was started for the recovery flow.
    #[must_use]
    pub fn is_recovery(&self) -> bool {
        matches!(self.flow, Flow::Recovery)
    }

    /// Finalize the registration with the server's
    /// `RegistrationResponse`.
    ///
    /// Consumes `self` so a session cannot be finalized twice.
    /// `secret` MUST be byte-identical to what was passed to the
    /// matching `start_*` call — passing a different password
    /// produces an envelope that will not unlock under any
    /// future login.
    ///
    /// # Errors
    ///
    /// * [`AuthClientError::Base64`] — `server_response_b64` does
    ///   not decode.
    /// * [`AuthClientError::InvalidWire`] — bytes decode but do
    ///   not deserialize as a `RegistrationResponse`.
    /// * [`AuthClientError::Opaque`] — opaque-ke rejected the
    ///   math (e.g. server proof is malformed).
    pub fn finish(self, secret: &[u8], server_response_b64: &str) -> Result<RegistrationOutput> {
        let response_bytes = B64
            .decode(server_response_b64)
            .map_err(AuthClientError::base64)?;
        let response = RegistrationResponse::<LakkoCipherSuite>::deserialize(&response_bytes)
            .map_err(|_| AuthClientError::invalid_wire())?;

        let mut rng = OsRng;
        let ClientRegistrationFinishResult {
            message,
            export_key,
            ..
        } = self
            .state
            .finish(
                &mut rng,
                secret,
                response,
                ClientRegistrationFinishParameters::default(),
            )
            .map_err(|_| AuthClientError::opaque())?;

        Ok(RegistrationOutput {
            upload_b64: B64.encode(message.serialize()),
            export_key: SecretBytes::new(export_key.to_vec()),
        })
    }
}
