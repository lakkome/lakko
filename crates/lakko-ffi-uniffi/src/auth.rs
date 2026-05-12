// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI exposure of `lakko-auth-client`.
//!
//! Mirror of `lakko-ffi-wasm/src/auth.rs`. Two classes that wrap the
//! Rust [`RegistrationSession`] / [`LoginSession`] surface 1:1:
//!
//!   * [`OpaqueRegister`] — produces the registration request bytes
//!     for `/signup/start`, then consumes the server's response on
//!     `finish` to produce the upload envelope + the OPAQUE export
//!     key (seed for the device root key).
//!   * [`OpaqueLogin`] — same shape for the login dance, with a
//!     `session_key` field in the output the caller binds to a
//!     follow-up session token.
//!
//! ## "Consume on finish" model
//!
//! `RegistrationSession::finish` takes `self` by value in the
//! underlying Rust crate — finalizing the same OPAQUE state twice
//! is a protocol bug. UniFFI's `&self` impl methods cannot consume
//! the receiver, so the classes here hold the inner session in a
//! `Mutex<Option<…>>` and `.take()` it on the first `finish`. A
//! second call returns [`FfiError::AuthAlreadyFinalized`] instead
//! of corrupting state.
//!
//! ## What is NOT here
//!
//! * **HTTP client.** UniFFI exposes only the OPAQUE math; the
//!   `fetch`-driven JSON dance lives one layer up in Kotlin /
//!   Swift, exactly like the TS side in `apps/web`.
//! * **`account_id` (UUIDv7) generation.** Caller's job — Kotlin
//!   uses `java.util.UUID` + the standard UUIDv7 helper (1.15d2).

use std::sync::Mutex;

use lakko_auth_client::{AuthClientError, LoginSession, RegistrationSession};

use crate::FfiError;

impl From<AuthClientError> for FfiError {
    fn from(e: AuthClientError) -> Self {
        Self::Auth(e.to_string())
    }
}

// ============================================================================
// OpaqueRegister
// ============================================================================

/// Client-side OPAQUE registration session — Swift / Kotlin handle.
///
/// Kotlin usage:
///
/// ```kotlin
/// import uniffi.lakko_ffi_uniffi.OpaqueRegister
///
/// // Phase 1: produce the registration request.
/// val reg = OpaqueRegister.startPassword(passwordBytes)
/// val requestB64 = reg.requestB64()
///
/// // POST { account_id, password_request: requestB64, … pow }
/// // → server returns { password_response: "..." }.
///
/// // Phase 2: finalize once the response arrives.
/// val out = reg.finish(passwordBytes, serverResponseB64)
/// // out.uploadB64()    — goes into POST /signup/finish
/// // out.exportKey()    — ByteArray(64), seed for the device root key
/// ```
#[derive(uniffi::Object)]
pub struct OpaqueRegister {
    /// `Mutex<Option<…>>` so `finish` (which moves the inner
    /// session into `opaque-ke`) can `take()` the slot. UniFFI
    /// `Object`s are always `Arc<Self>` on the FFI side, so we
    /// need interior mutability regardless.
    inner: Mutex<Option<RegistrationSession>>,
    request_b64: String,
}

#[uniffi::export]
impl OpaqueRegister {
    /// Begin a password registration.
    ///
    /// `password` is the raw byte sequence the user typed (UTF-8-
    /// encoded by the platform caller). It is **not** sent to the
    /// server — OPAQUE blinds it through the OPRF.
    #[uniffi::constructor]
    pub fn start_password(password: Vec<u8>) -> Result<Self, FfiError> {
        let (session, request_b64) = RegistrationSession::start_password(&password)?;
        Ok(Self {
            inner: Mutex::new(Some(session)),
            request_b64,
        })
    }

    /// Begin a recovery registration. Driven by the 64-byte BIP39
    /// seed in the real flow; the FFI is byte-agnostic.
    #[uniffi::constructor]
    pub fn start_recovery(seed: Vec<u8>) -> Result<Self, FfiError> {
        let (session, request_b64) = RegistrationSession::start_recovery(&seed)?;
        Ok(Self {
            inner: Mutex::new(Some(session)),
            request_b64,
        })
    }

    /// Base64-encoded `RegistrationRequest`. Goes into
    /// `password_request` (or `recovery_request`) on
    /// `/signup/start`.
    pub fn request_b64(&self) -> String {
        self.request_b64.clone()
    }

    /// `true` iff this session was started via
    /// [`Self::start_password`].
    pub fn is_password(&self) -> Result<bool, FfiError> {
        let guard = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        guard
            .as_ref()
            .map(RegistrationSession::is_password)
            .ok_or(FfiError::AuthAlreadyFinalized)
    }

    /// `true` iff this session was started via
    /// [`Self::start_recovery`].
    pub fn is_recovery(&self) -> Result<bool, FfiError> {
        let guard = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        guard
            .as_ref()
            .map(RegistrationSession::is_recovery)
            .ok_or(FfiError::AuthAlreadyFinalized)
    }

    /// Finalize the registration with the server's
    /// `RegistrationResponse`.
    ///
    /// Single-use: a second call on the same instance returns
    /// [`FfiError::AuthAlreadyFinalized`] rather than producing a
    /// second envelope. `secret` MUST be byte-equal to the bytes
    /// passed into `start_password` / `start_recovery`.
    pub fn finish(
        &self,
        secret: Vec<u8>,
        server_response_b64: String,
    ) -> Result<OpaqueRegisterOutput, FfiError> {
        let mut guard = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let session = guard.take().ok_or(FfiError::AuthAlreadyFinalized)?;
        let out = session.finish(&secret, &server_response_b64)?;
        Ok(OpaqueRegisterOutput {
            upload_b64: out.upload_b64,
            export_key: out.export_key.as_ref().to_vec(),
        })
    }
}

/// Output of [`OpaqueRegister::finish`].
///
/// `export_key` is the OPAQUE export key — 64 bytes for the Lakko
/// cipher suite (Ristretto255-SHA512). Callers feed it into the
/// platform device-root-key store (Android Keystore / Secure
/// Enclave / non-extractable WebCrypto on web), never into plain
/// disk storage.
#[derive(uniffi::Object)]
pub struct OpaqueRegisterOutput {
    upload_b64: String,
    export_key: Vec<u8>,
}

#[uniffi::export]
impl OpaqueRegisterOutput {
    /// Base64-encoded `RegistrationUpload` for the
    /// `/signup/finish` body.
    pub fn upload_b64(&self) -> String {
        self.upload_b64.clone()
    }

    /// OPAQUE export key bytes. Each access returns a fresh copy
    /// — the caller is expected to consume / wipe the result
    /// promptly.
    pub fn export_key(&self) -> Vec<u8> {
        self.export_key.clone()
    }
}

// ============================================================================
// OpaqueLogin
// ============================================================================

/// Client-side OPAQUE login session — Swift / Kotlin handle.
/// Mirror of [`OpaqueRegister`].
///
/// On a successful `finish` call, the returned
/// [`OpaqueLoginOutput`] carries:
///
///   * `finalize_b64` — go-ahead bytes for `/login/finish`;
///   * `session_key` — symmetric channel key shared with the
///     server (bind a follow-up session token to it);
///   * `export_key` — deterministic seed for the device root key
///     (matches the export key produced at registration time for
///     the same secret).
#[derive(uniffi::Object)]
pub struct OpaqueLogin {
    inner: Mutex<Option<LoginSession>>,
    request_b64: String,
}

#[uniffi::export]
impl OpaqueLogin {
    /// Begin a password login.
    #[uniffi::constructor]
    pub fn start_password(password: Vec<u8>) -> Result<Self, FfiError> {
        let (session, request_b64) = LoginSession::start_password(&password)?;
        Ok(Self {
            inner: Mutex::new(Some(session)),
            request_b64,
        })
    }

    /// Begin a recovery login.
    #[uniffi::constructor]
    pub fn start_recovery(seed: Vec<u8>) -> Result<Self, FfiError> {
        let (session, request_b64) = LoginSession::start_recovery(&seed)?;
        Ok(Self {
            inner: Mutex::new(Some(session)),
            request_b64,
        })
    }

    /// Base64-encoded `CredentialRequest` for the `/login/start`
    /// body.
    pub fn request_b64(&self) -> String {
        self.request_b64.clone()
    }

    /// `true` iff this session was started via
    /// [`Self::start_password`].
    pub fn is_password(&self) -> Result<bool, FfiError> {
        let guard = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        guard
            .as_ref()
            .map(LoginSession::is_password)
            .ok_or(FfiError::AuthAlreadyFinalized)
    }

    /// `true` iff this session was started via
    /// [`Self::start_recovery`].
    pub fn is_recovery(&self) -> Result<bool, FfiError> {
        let guard = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        guard
            .as_ref()
            .map(LoginSession::is_recovery)
            .ok_or(FfiError::AuthAlreadyFinalized)
    }

    /// Finalize the login.
    ///
    /// Single-use, same rationale as [`OpaqueRegister::finish`]. A
    /// wrong-password attempt returns the same error as an
    /// unknown-account attempt — by design (OPAQUE indistinguishable
    /// failure).
    pub fn finish(
        &self,
        secret: Vec<u8>,
        server_response_b64: String,
    ) -> Result<OpaqueLoginOutput, FfiError> {
        let mut guard = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let session = guard.take().ok_or(FfiError::AuthAlreadyFinalized)?;
        let out = session.finish(&secret, &server_response_b64)?;
        Ok(OpaqueLoginOutput {
            finalize_b64: out.finalize_b64,
            session_key: out.session_key.as_ref().to_vec(),
            export_key: out.export_key.as_ref().to_vec(),
        })
    }
}

/// Output of [`OpaqueLogin::finish`].
#[derive(uniffi::Object)]
pub struct OpaqueLoginOutput {
    finalize_b64: String,
    session_key: Vec<u8>,
    export_key: Vec<u8>,
}

#[uniffi::export]
impl OpaqueLoginOutput {
    /// Base64-encoded `CredentialFinalization` for the
    /// `/login/finish` body.
    pub fn finalize_b64(&self) -> String {
        self.finalize_b64.clone()
    }

    /// OPAQUE session key bytes. Symmetric key shared with the
    /// server — use to bind a follow-up session token, **not** to
    /// encrypt user data.
    pub fn session_key(&self) -> Vec<u8> {
        self.session_key.clone()
    }

    /// OPAQUE export key bytes. Same provenance as
    /// [`OpaqueRegisterOutput::export_key`] — deterministic for a
    /// given password.
    pub fn export_key(&self) -> Vec<u8> {
        self.export_key.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_request_b64_is_stable_after_construction() {
        let reg = OpaqueRegister::start_password(b"x".to_vec()).expect("start");
        let a = reg.request_b64();
        let b = reg.request_b64();
        assert_eq!(a, b);
        assert!(!a.is_empty());
    }

    #[test]
    fn register_flags_password_vs_recovery() {
        let pw = OpaqueRegister::start_password(b"x".to_vec()).expect("start password");
        assert!(pw.is_password().unwrap());
        assert!(!pw.is_recovery().unwrap());

        let rc = OpaqueRegister::start_recovery(vec![0u8; 64]).expect("start recovery");
        assert!(!rc.is_password().unwrap());
        assert!(rc.is_recovery().unwrap());
    }

    #[test]
    fn login_flags_password_vs_recovery() {
        let pw = OpaqueLogin::start_password(b"x".to_vec()).expect("start password");
        assert!(pw.is_password().unwrap());
        assert!(!pw.is_recovery().unwrap());

        let rc = OpaqueLogin::start_recovery(vec![0u8; 64]).expect("start recovery");
        assert!(!rc.is_password().unwrap());
        assert!(rc.is_recovery().unwrap());
    }

    /// A second call to `finish` on the same handle MUST refuse
    /// rather than silently corrupt OPAQUE state. We can't easily
    /// drive a real `finish` through OPAQUE math without a server
    /// round-trip, but the "already finalized" path is reachable
    /// by manually draining the `Option<…>` inside.
    ///
    /// `Result<OpaqueRegisterOutput, FfiError>::expect_err` would
    /// need `Debug` on the `Object`-typed Ok variant — UniFFI does
    /// not auto-derive it. A plain `match` keeps the test working
    /// without adding boilerplate.
    #[test]
    fn finish_is_single_use_sentinel() {
        let reg = OpaqueRegister::start_password(b"x".to_vec()).expect("start");
        // Drain the inner session to simulate a prior successful finish.
        let _ = reg.inner.lock().expect("lock").take();
        match reg.finish(b"x".to_vec(), String::new()) {
            Err(FfiError::AuthAlreadyFinalized) => {},
            Err(other) => panic!("expected AuthAlreadyFinalized, got {other:?}"),
            Ok(_) => panic!("expected error on second finish"),
        }
    }
}
