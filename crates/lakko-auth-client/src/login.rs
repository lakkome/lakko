// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Client-side OPAQUE login session.
//!
//! The shape mirrors [`crate::registration`]:
//!
//!   * [`LoginSession::start_password`] /
//!     [`LoginSession::start_recovery`] — produce the
//!     `CredentialRequest` blob (`request` field of
//!     [`wire::LoginStartRequest`](crate::wire::LoginStartRequest)).
//!
//!   * [`LoginSession::finish`] — consume the server's
//!     `CredentialResponse` and produce a `CredentialFinalization`
//!     (`finalize` field of
//!     [`wire::LoginFinishRequest`](crate::wire::LoginFinishRequest))
//!     plus the symmetric `session_key` and the deterministic
//!     `export_key`.
//!
//! Successful finalization implies the server proved knowledge of
//! the matching envelope — no separate "is login valid?" return is
//! needed.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use opaque_ke::{
    ClientLogin, ClientLoginFinishParameters, ClientLoginFinishResult, CredentialResponse,
};
use rand::rngs::OsRng;

use crate::cipher_suite::LakkoCipherSuite;
use crate::error::{AuthClientError, Result};
use crate::secret::SecretBytes;

#[derive(Clone, Copy)]
enum Flow {
    Password,
    Recovery,
}

/// In-flight client-side login. Holds the opaque-ke `ClientLogin`
/// state between `start_*` and `finish`. Single-use.
pub struct LoginSession {
    state: ClientLogin<LakkoCipherSuite>,
    flow: Flow,
}

/// Output of [`LoginSession::finish`].
///
/// Same `Debug` rationale as [`RegistrationOutput`](crate::RegistrationOutput):
/// the inner [`SecretBytes`] elide their contents, so the derive
/// is safe.
#[derive(Debug)]
pub struct LoginOutput {
    /// Base64-encoded `CredentialFinalization`. Goes into the
    /// `finalize` field of [`wire::LoginFinishRequest`](crate::wire::LoginFinishRequest).
    pub finalize_b64: String,
    /// Symmetric session key shared with the server. Length is
    /// [`crate::SESSION_KEY_LEN`] bytes. Used to bind a follow-up
    /// session token / channel; not the device root key.
    pub session_key: SecretBytes,
    /// OPAQUE export key. Identical bytes to the one produced at
    /// registration time for the same secret — this is what makes
    /// it usable as the seed for the device root key.
    pub export_key: SecretBytes,
}

impl LoginSession {
    /// Start a password login.
    ///
    /// Returns the session and the base64-encoded
    /// `CredentialRequest` ready for
    /// [`wire::LoginStartRequest`](crate::wire::LoginStartRequest).
    ///
    /// # Errors
    ///
    /// Returns [`AuthClientError::Opaque`] on a broken RNG. Treat
    /// as fatal.
    pub fn start_password(password: &[u8]) -> Result<(Self, String)> {
        Self::start_with_flow(password, Flow::Password)
    }

    /// Start a recovery login. Same math as
    /// [`Self::start_password`]; talks to `/recovery/login/start`
    /// rather than `/login/start`.
    ///
    /// # Errors
    ///
    /// Same as [`Self::start_password`].
    pub fn start_recovery(seed: &[u8]) -> Result<(Self, String)> {
        Self::start_with_flow(seed, Flow::Recovery)
    }

    fn start_with_flow(secret: &[u8], flow: Flow) -> Result<(Self, String)> {
        let mut rng = OsRng;
        let started = ClientLogin::<LakkoCipherSuite>::start(&mut rng, secret)
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

    /// `true` iff this session was created via [`Self::start_password`].
    #[must_use]
    pub fn is_password(&self) -> bool {
        matches!(self.flow, Flow::Password)
    }

    /// `true` iff this session was created via [`Self::start_recovery`].
    #[must_use]
    pub fn is_recovery(&self) -> bool {
        matches!(self.flow, Flow::Recovery)
    }

    /// Finalize the login.
    ///
    /// Consumes `self`. `secret` must equal the bytes passed to
    /// `start_*` — passing a different password (or a recovery
    /// seed in a session started for the password flow) returns
    /// [`AuthClientError::Opaque`], the same error any "wrong
    /// credentials" path produces, by design.
    ///
    /// # Errors
    ///
    /// * [`AuthClientError::Base64`] — `server_response_b64` does
    ///   not decode.
    /// * [`AuthClientError::InvalidWire`] — bytes decode but do
    ///   not deserialize as a `CredentialResponse`.
    /// * [`AuthClientError::Opaque`] — opaque-ke rejected the
    ///   math (wrong credentials, fake-credentials path, …).
    ///   The variant intentionally lacks finer granularity to
    ///   preserve OPAQUE's indistinguishable-failure property.
    pub fn finish(self, secret: &[u8], server_response_b64: &str) -> Result<LoginOutput> {
        let response_bytes = B64
            .decode(server_response_b64)
            .map_err(AuthClientError::base64)?;
        let response = CredentialResponse::<LakkoCipherSuite>::deserialize(&response_bytes)
            .map_err(|_| AuthClientError::invalid_wire())?;

        let ClientLoginFinishResult {
            message,
            session_key,
            export_key,
            ..
        } = self
            .state
            .finish(secret, response, ClientLoginFinishParameters::default())
            .map_err(|_| AuthClientError::opaque())?;

        Ok(LoginOutput {
            finalize_b64: B64.encode(message.serialize()),
            session_key: SecretBytes::new(session_key.to_vec()),
            export_key: SecretBytes::new(export_key.to_vec()),
        })
    }
}
