// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen exposure of `lakko-auth-client`.
//!
//! Two classes that mirror the Rust surface 1:1:
//!
//!   * [`OpaqueRegister`] — wraps a [`RegistrationSession`].
//!     Construct it with the password (or BIP39 seed) the user
//!     just typed, read `requestB64` to fill the corresponding
//!     field in the `/signup/start` body, and call `finish` once
//!     the server's response arrives.
//!
//!   * [`OpaqueLogin`] — same shape for [`LoginSession`].
//!
//! ## Why a "consume on finish" model
//!
//! The Rust `RegistrationSession::finish` takes `self` by value:
//! finalizing twice with the same OPAQUE state is a protocol bug
//! that produces undefined behaviour on the math side. wasm-bindgen
//! `&self` / `&mut self` impl methods cannot consume the receiver,
//! so the classes here hold the inner session in a
//! `Mutex<Option<…>>` and `.take()` it on the first `finish` call.
//! A second call returns an error rather than corrupting state.
//!
//! ## What is NOT here (1.16c3)
//!
//! * **HTTP client.** The classes here only do OPAQUE math; the
//!   `fetch`-driven JSON dance + PoW solving live one layer up,
//!   in TypeScript inside `apps/web`. Keeping the FFI free of
//!   HTTP keeps the wasm bundle small and lets the TS side stay
//!   in charge of concerns like retries, AbortController, and
//!   anti-censorship transport selection.
//! * **`account_id` (UUIDv7) generation.** Caller's job — `apps/web`
//!   imports a JS UUIDv7 helper.
//! * **Equix PoW solver.** Separate sub-sprint; the solver shares
//!   code with the server-side verifier and lands as its own crate.

use std::sync::Mutex;

use lakko_auth_client::{AuthClientError, LoginSession, RegistrationSession};
use wasm_bindgen::prelude::*;

/// Map an `AuthClientError` to a `JsError`. Same elision policy as
/// the underlying error: we do **not** distinguish "wrong password"
/// from "unknown account" on the JS side, on purpose.
fn auth_err(e: AuthClientError) -> JsError {
    JsError::new(&format!("{e}"))
}

/// Sentinel returned when a session has already been finalized.
/// Calling `finish` twice on the same `OpaqueRegister` /
/// `OpaqueLogin` is a wiring bug — surface it loudly rather than
/// silently corrupt OPAQUE state.
fn already_finalized() -> JsError {
    JsError::new("OPAQUE session already finalized")
}

/// Lock-poison conversion. The wrappers below never panic while
/// holding the lock, so this branch is unreachable in practice;
/// returning a `JsError` is still preferable to `unwrap`.
fn lock_poisoned() -> JsError {
    JsError::new("internal: OPAQUE session lock poisoned")
}

// ============================================================================
// OpaqueRegister
// ============================================================================

/// Client-side OPAQUE registration session — JS handle.
///
/// JS usage:
///
/// ```js
/// import { OpaqueRegister } from "@lakko/ffi-wasm";
///
/// // Phase 1: produce the registration request.
/// const reg = OpaqueRegister.startPassword(passwordBytes);
/// const passwordRequestB64 = reg.requestB64;
///
/// // POST { account_id, password_request: passwordRequestB64, … pow }
/// // → server returns { password_response: "..." }.
///
/// // Phase 2: finalize once the response arrives.
/// const out = reg.finish(passwordBytes, serverResponseB64);
/// // out.uploadB64    — goes into POST /signup/finish
/// // out.exportKey    — Uint8Array(64), seed for the device root key
/// ```
#[wasm_bindgen]
pub struct OpaqueRegister {
    /// `Mutex<Option<…>>` so `finish` (which moves the inner
    /// session into `opaque-ke`) can `take()` the slot. We never
    /// expose `&mut self` to JS — the Mutex is purely for interior
    /// mutability.
    inner: Mutex<Option<RegistrationSession>>,
    request_b64: String,
}

#[wasm_bindgen]
impl OpaqueRegister {
    /// Begin a password registration. Equivalent to the Rust call
    /// [`RegistrationSession::start_password`].
    ///
    /// `password` is the raw byte sequence the user typed
    /// (UTF-8-encoded by the JS caller). It is **not** sent to
    /// the server — OPAQUE blinds it through the OPRF.
    #[wasm_bindgen(js_name = startPassword)]
    pub fn start_password(password: &[u8]) -> Result<OpaqueRegister, JsError> {
        let (session, request_b64) =
            RegistrationSession::start_password(password).map_err(auth_err)?;
        Ok(Self {
            inner: Mutex::new(Some(session)),
            request_b64,
        })
    }

    /// Begin a recovery registration. Driven by the 64-byte BIP39
    /// seed in the real flow; the FFI is byte-agnostic.
    #[wasm_bindgen(js_name = startRecovery)]
    pub fn start_recovery(seed: &[u8]) -> Result<OpaqueRegister, JsError> {
        let (session, request_b64) = RegistrationSession::start_recovery(seed).map_err(auth_err)?;
        Ok(Self {
            inner: Mutex::new(Some(session)),
            request_b64,
        })
    }

    /// Base64-encoded `RegistrationRequest`. Goes into
    /// `password_request` (or `recovery_request`) on
    /// `/signup/start`.
    ///
    /// Cheap to read — returns a copy of the cached string.
    #[wasm_bindgen(getter, js_name = requestB64)]
    pub fn request_b64(&self) -> String {
        self.request_b64.clone()
    }

    /// `true` iff this session was started via
    /// [`Self::start_password`].
    #[wasm_bindgen(getter, js_name = isPassword)]
    pub fn is_password(&self) -> Result<bool, JsError> {
        let guard = self.inner.lock().map_err(|_| lock_poisoned())?;
        match guard.as_ref() {
            Some(s) => Ok(s.is_password()),
            None => Err(already_finalized()),
        }
    }

    /// `true` iff this session was started via
    /// [`Self::start_recovery`].
    #[wasm_bindgen(getter, js_name = isRecovery)]
    pub fn is_recovery(&self) -> Result<bool, JsError> {
        let guard = self.inner.lock().map_err(|_| lock_poisoned())?;
        match guard.as_ref() {
            Some(s) => Ok(s.is_recovery()),
            None => Err(already_finalized()),
        }
    }

    /// Finalize the registration with the server's
    /// `RegistrationResponse`.
    ///
    /// Single-use: a second call to `finish` on the same instance
    /// returns an error rather than producing a second envelope.
    /// `secret` MUST be byte-equal to the bytes passed into
    /// `startPassword` / `startRecovery`.
    pub fn finish(
        &self,
        secret: &[u8],
        server_response_b64: &str,
    ) -> Result<OpaqueRegisterOutput, JsError> {
        let mut guard = self.inner.lock().map_err(|_| lock_poisoned())?;
        let session = guard.take().ok_or_else(already_finalized)?;
        let out = session
            .finish(secret, server_response_b64)
            .map_err(auth_err)?;
        Ok(OpaqueRegisterOutput {
            upload_b64: out.upload_b64,
            export_key: out.export_key.as_ref().to_vec(),
        })
    }
}

/// Output of [`OpaqueRegister::finish`].
///
/// `exportKey` is the OPAQUE export key. Apps store it (or, more
/// commonly, a key derived from it) in non-extractable WebCrypto
/// — never in plain `localStorage`.
#[wasm_bindgen]
pub struct OpaqueRegisterOutput {
    upload_b64: String,
    export_key: Vec<u8>,
}

#[wasm_bindgen]
impl OpaqueRegisterOutput {
    /// Base64-encoded `RegistrationUpload` for the `/signup/finish`
    /// body.
    #[wasm_bindgen(getter, js_name = uploadB64)]
    pub fn upload_b64(&self) -> String {
        self.upload_b64.clone()
    }

    /// OPAQUE export key bytes. 64 bytes for `LakkoCipherSuite`
    /// (Ristretto255-SHA512). Each access returns a fresh copy —
    /// the caller is expected to consume / wipe the result
    /// promptly.
    #[wasm_bindgen(getter, js_name = exportKey)]
    pub fn export_key(&self) -> Vec<u8> {
        self.export_key.clone()
    }
}

// ============================================================================
// OpaqueLogin
// ============================================================================

/// Client-side OPAQUE login session — JS handle. Mirror of
/// [`OpaqueRegister`].
///
/// On a successful `finish` call, the returned
/// [`OpaqueLoginOutput`] carries:
///
///   * `finalizeB64` — go-ahead bytes for `/login/finish`;
///   * `sessionKey` — symmetric channel key shared with the
///      server (use to bind a follow-up session token);
///   * `exportKey` — deterministic seed for the device root key
///      (matches the export key produced at registration time
///      for the same secret).
///
/// JS usage:
///
/// ```js
/// import { OpaqueLogin } from "@lakko/ffi-wasm";
///
/// const login = OpaqueLogin.startPassword(passwordBytes);
/// const requestB64 = login.requestB64;
/// // POST /login/start { account_id, request: requestB64, … pow }
/// // → server returns { session_id, response }.
/// const out = login.finish(passwordBytes, response);
/// // POST /login/finish { session_id, finalize: out.finalizeB64 }
/// ```
#[wasm_bindgen]
pub struct OpaqueLogin {
    inner: Mutex<Option<LoginSession>>,
    request_b64: String,
}

#[wasm_bindgen]
impl OpaqueLogin {
    /// Begin a password login.
    #[wasm_bindgen(js_name = startPassword)]
    pub fn start_password(password: &[u8]) -> Result<OpaqueLogin, JsError> {
        let (session, request_b64) = LoginSession::start_password(password).map_err(auth_err)?;
        Ok(Self {
            inner: Mutex::new(Some(session)),
            request_b64,
        })
    }

    /// Begin a recovery login.
    #[wasm_bindgen(js_name = startRecovery)]
    pub fn start_recovery(seed: &[u8]) -> Result<OpaqueLogin, JsError> {
        let (session, request_b64) = LoginSession::start_recovery(seed).map_err(auth_err)?;
        Ok(Self {
            inner: Mutex::new(Some(session)),
            request_b64,
        })
    }

    /// Base64-encoded `CredentialRequest` for the `/login/start`
    /// body.
    #[wasm_bindgen(getter, js_name = requestB64)]
    pub fn request_b64(&self) -> String {
        self.request_b64.clone()
    }

    /// `true` iff this session was started via
    /// [`Self::start_password`].
    #[wasm_bindgen(getter, js_name = isPassword)]
    pub fn is_password(&self) -> Result<bool, JsError> {
        let guard = self.inner.lock().map_err(|_| lock_poisoned())?;
        match guard.as_ref() {
            Some(s) => Ok(s.is_password()),
            None => Err(already_finalized()),
        }
    }

    /// `true` iff this session was started via
    /// [`Self::start_recovery`].
    #[wasm_bindgen(getter, js_name = isRecovery)]
    pub fn is_recovery(&self) -> Result<bool, JsError> {
        let guard = self.inner.lock().map_err(|_| lock_poisoned())?;
        match guard.as_ref() {
            Some(s) => Ok(s.is_recovery()),
            None => Err(already_finalized()),
        }
    }

    /// Finalize the login.
    ///
    /// Single-use, same rationale as
    /// [`OpaqueRegister::finish`]. A wrong-password attempt
    /// returns the same error as an unknown-account attempt — by
    /// design (OPAQUE indistinguishable failure).
    pub fn finish(
        &self,
        secret: &[u8],
        server_response_b64: &str,
    ) -> Result<OpaqueLoginOutput, JsError> {
        let mut guard = self.inner.lock().map_err(|_| lock_poisoned())?;
        let session = guard.take().ok_or_else(already_finalized)?;
        let out = session
            .finish(secret, server_response_b64)
            .map_err(auth_err)?;
        Ok(OpaqueLoginOutput {
            finalize_b64: out.finalize_b64,
            session_key: out.session_key.as_ref().to_vec(),
            export_key: out.export_key.as_ref().to_vec(),
        })
    }
}

/// Output of [`OpaqueLogin::finish`].
#[wasm_bindgen]
pub struct OpaqueLoginOutput {
    finalize_b64: String,
    session_key: Vec<u8>,
    export_key: Vec<u8>,
}

#[wasm_bindgen]
impl OpaqueLoginOutput {
    /// Base64-encoded `CredentialFinalization` for the
    /// `/login/finish` body.
    #[wasm_bindgen(getter, js_name = finalizeB64)]
    pub fn finalize_b64(&self) -> String {
        self.finalize_b64.clone()
    }

    /// OPAQUE session key bytes. 64 bytes for `LakkoCipherSuite`.
    /// Symmetric key shared with the server — use to bind a
    /// follow-up session token, **not** to encrypt user data.
    #[wasm_bindgen(getter, js_name = sessionKey)]
    pub fn session_key(&self) -> Vec<u8> {
        self.session_key.clone()
    }

    /// OPAQUE export key bytes. Same provenance as
    /// [`OpaqueRegisterOutput::export_key`] — deterministic for
    /// a given password.
    #[wasm_bindgen(getter, js_name = exportKey)]
    pub fn export_key(&self) -> Vec<u8> {
        self.export_key.clone()
    }
}

// ============================================================================
// Tests — host-side
// ============================================================================
//
// As with the rest of the FFI surface, host-side tests cover happy
// paths and the error paths that don't go through `JsError::new`
// (which is wasm-bindgen-imported and stubbed-out off-wasm32). The
// real OPAQUE math is tested in the underlying crate; tests here
// only confirm the wrapper plumbing.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_request_b64_is_stable_after_construction() {
        let reg = OpaqueRegister::start_password(b"x").expect("start");
        let a = reg.request_b64();
        let b = reg.request_b64();
        assert_eq!(a, b, "requestB64 getter must be idempotent");
        assert!(!a.is_empty());
    }

    #[test]
    fn register_flags_password_vs_recovery() {
        let pw = OpaqueRegister::start_password(b"x").expect("start password");
        assert!(pw.is_password().unwrap());
        assert!(!pw.is_recovery().unwrap());

        let rc = OpaqueRegister::start_recovery(&[0u8; 64]).expect("start recovery");
        assert!(!rc.is_password().unwrap());
        assert!(rc.is_recovery().unwrap());
    }

    #[test]
    fn login_flags_password_vs_recovery() {
        let pw = OpaqueLogin::start_password(b"x").expect("start password");
        assert!(pw.is_password().unwrap());
        assert!(!pw.is_recovery().unwrap());

        let rc = OpaqueLogin::start_recovery(&[0u8; 64]).expect("start recovery");
        assert!(!rc.is_password().unwrap());
        assert!(rc.is_recovery().unwrap());
    }
}
