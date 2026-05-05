// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! HTTP wire structs for `lakko-svc-auth`.
//!
//! Field-by-field mirror of the structs in
//! `services/auth/src/opaque.rs`. Kept in this crate (rather than
//! generated from a shared schema) for two reasons:
//!
//!   1. The server-side originals are `pub(crate)`. Promoting them
//!      to a shared crate would entangle the server's internal
//!      types with the public client surface.
//!   2. The client only ever speaks JSON to this endpoint and
//!      never needs the server's `axum::extract::*` machinery —
//!      a hand-mirrored set of `Serialize` / `Deserialize` structs
//!      is the smallest possible coupling.
//!
//! When a field is added or renamed on the server side, mirror
//! the change here in the same commit.
//!
//! ## What is NOT here yet
//!
//! * **`pow` field on every request.** The server demands a
//!   verified Equix proof on every endpoint that writes to the DB
//!   (`/signup/start`, `/signup/finish`, `/login/start`,
//!   `/recovery/login/start`). The PoW solver is its own crate
//!   (lands as 1.16c1+; the field is intentionally absent here so
//!   the auth-client crate stays free of an `equix` dependency).
//!   Higher-level callers will compose a final JSON body that
//!   merges these structs with a separately-built `pow` field.

use serde::{Deserialize, Serialize};

/// Request body for `POST /signup/start`. Excludes the `pow` and
/// `account_id` fields the server requires — the higher-level
/// HTTP wrapper adds those.
///
/// `recovery_request` is `None` for callers that haven't enrolled
/// a BIP39 mnemonic yet; the server uses
/// `#[serde(default, skip_serializing_if = "Option::is_none")]`
/// to make it backwards-compatible.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignupStartFields {
    /// Base64-encoded `RegistrationRequest` from the password flow.
    /// Returned by [`RegistrationSession::start_password`](crate::RegistrationSession::start_password).
    pub password_request: String,
    /// Base64-encoded `RegistrationRequest` from the recovery
    /// (BIP39) flow. Returned by
    /// [`RegistrationSession::start_recovery`](crate::RegistrationSession::start_recovery).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_request: Option<String>,
}

/// Response body for `POST /signup/start`. Server returns one
/// `RegistrationResponse` per `*_request` we sent.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignupStartResponseFields {
    /// Base64-encoded `RegistrationResponse` for the password flow.
    pub password_response: String,
    /// Base64-encoded `RegistrationResponse` for the recovery
    /// flow, present iff the request set `recovery_request`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_response: Option<String>,
}

/// Request body for `POST /signup/finish`. Same exclusions as
/// [`SignupStartFields`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignupFinishFields {
    /// Base64-encoded `RegistrationUpload` from the password flow.
    /// Set from [`RegistrationOutput::upload_b64`](crate::RegistrationOutput::upload_b64)
    /// returned by `RegistrationSession::finish` for a password
    /// session.
    pub password_upload: String,
    /// Base64-encoded `RegistrationUpload` from the recovery flow.
    /// MUST be present iff `recovery_request` was set in the
    /// matching `/signup/start`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_upload: Option<String>,
}

/// Response body for `POST /signup/finish`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignupFinishResponseFields {
    /// `true` iff both writes (account row + envelope row(s))
    /// committed.
    pub ok: bool,
}

/// Request body for `POST /login/start` and
/// `POST /recovery/login/start`. Same exclusions as
/// [`SignupStartFields`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoginStartFields {
    /// Base64-encoded `CredentialRequest`. Returned by
    /// [`LoginSession::start_password`](crate::LoginSession::start_password)
    /// or [`LoginSession::start_recovery`](crate::LoginSession::start_recovery).
    pub request: String,
}

/// Response body for `POST /login/start` and
/// `POST /recovery/login/start`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoginStartResponseFields {
    /// UUIDv7 the server assigned to this login attempt; the client
    /// passes it back in `/login/finish`.
    pub session_id: String,
    /// Base64-encoded `CredentialResponse`. Feed to
    /// [`LoginSession::finish`](crate::LoginSession::finish).
    pub response: String,
}

/// Request body for `POST /login/finish` and
/// `POST /recovery/login/finish`. No `pow` here — the server only
/// requires PoW on the start endpoints.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoginFinishFields {
    /// `session_id` echoed back from
    /// [`LoginStartResponseFields::session_id`].
    pub session_id: String,
    /// Base64-encoded `CredentialFinalization` from
    /// [`LoginOutput::finalize_b64`](crate::LoginOutput::finalize_b64).
    pub finalize: String,
}

/// Response body for `POST /login/finish` and
/// `POST /recovery/login/finish`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoginFinishResponseFields {
    /// `true` iff the OPAQUE finalization checked out.
    pub ok: bool,
    /// Base64-encoded raw 16-byte `account_id` (UUIDv7). The client
    /// pins this for follow-up requests until a real session-token
    /// surface lands.
    pub account_id: String,
}
