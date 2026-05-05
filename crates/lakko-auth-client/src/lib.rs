// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! OPAQUE-PAKE client for `lakko-svc-auth`.
//!
//! ## What this crate is
//!
//! The pure-Rust client side of the OPAQUE-PAKE flow that
//! `services/auth/src/opaque.rs` implements on the server. Every
//! `lakko-svc-auth` HTTP endpoint that talks crypto bytes
//! (`/signup/start`, `/signup/finish`, `/login/start`,
//! `/login/finish`, `/recovery/login/{start,finish}`) has a
//! counterpart here.
//!
//! Two registrations live in parallel per account:
//!
//!   * **Password** — `RegistrationSession::start_password` /
//!     `LoginSession::start_password`.
//!   * **Recovery** — `RegistrationSession::start_recovery` /
//!     `LoginSession::start_recovery`. Driven by a 64-byte BIP39
//!     mnemonic seed in the real flow; the crate itself is agnostic
//!     to the input bytes' provenance.
//!
//! This crate intentionally does **not**:
//!
//!   * solve the Equix PoW the server demands on every endpoint
//!     (separate sub-sprint — the PoW solver shares code with the
//!     server-side verifier and lands as its own crate);
//!   * speak HTTP (the wasm `fetch` glue lands in
//!     `lakko-ffi-wasm` and a future `lakko-transport` HTTP client);
//!   * pick `account_id` (UUIDv7 generation is the caller's job —
//!     a fresh signup picks one, a returning login already has one).
//!
//! ## Cipher suite
//!
//! [`LakkoCipherSuite`] is a verbatim copy of the server's type
//! (see `services/auth/src/opaque.rs`). Any change to the
//! server-side cipher suite must land in the same commit as the
//! matching change here — the OPAQUE math is bound to the suite
//! and a mismatch produces "invalid credentials" with no
//! recoverable diagnostic.
//!
//! ## Wire format
//!
//! Every byte string crossing the FFI boundary is **base64-encoded
//! (standard alphabet, with padding)** to match the server. The
//! [`wire`] module mirrors the JSON request / response structs
//! exactly; callers that prefer to assemble their own payloads can
//! ignore it and use the raw `*_b64` strings returned by the
//! session APIs.
//!
//! ## Security notes
//!
//! * The client **password is never sent** — OPAQUE-PAKE blinds it
//!   through the OPRF. An attacker who has compromised the server
//!   cannot brute-force the password offline.
//! * The OPAQUE [`export_key`](LoginOutput::export_key) returned on
//!   successful login is the seed from which the device root key
//!   gets derived. It MUST be stored in hardware-backed storage
//!   (Secure Enclave / StrongBox / non-extractable WebCrypto) and
//!   MUST NOT touch disk.
//! * [`SecretBytes`] zeroizes its contents on drop. Hold sessions
//!   in scopes that drop them promptly.
//!
//! ## Example
//!
//! ```no_run
//! use lakko_auth_client::{LoginSession, RegistrationSession};
//!
//! # fn drive_signup_against(_url: &str, _b64: &str) -> String { String::new() }
//! # fn account_id() -> [u8; 16] { [0; 16] }
//! let password = b"correct horse battery staple";
//!
//! // Phase 1: registration.
//! let (session, password_request_b64) =
//!     RegistrationSession::start_password(password)
//!         .expect("opaque registration start");
//! // POST { account_id, password_request: password_request_b64, … pow }
//! // → server returns `password_response`.
//! let password_response_b64 = drive_signup_against("/signup/start", &password_request_b64);
//! let reg_out = session
//!     .finish(password, &password_response_b64)
//!     .expect("opaque registration finish");
//! // POST { account_id, password_upload: reg_out.upload_b64, … pow }
//! // → server stores the envelope.
//!
//! // Phase 2: login.
//! let (session, credential_request_b64) =
//!     LoginSession::start_password(password).expect("opaque login start");
//! let credential_response_b64 = drive_signup_against("/login/start", &credential_request_b64);
//! let login_out = session
//!     .finish(password, &credential_response_b64)
//!     .expect("opaque login finish");
//! // POST { session_id, finalize: login_out.finalize_b64 }
//! // login_out.session_key — symmetric channel-bound key
//! // login_out.export_key — seed for the device root key
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod cipher_suite;
mod error;
mod login;
mod registration;
mod secret;
pub mod wire;

pub use cipher_suite::{LakkoCipherSuite, EXPORT_KEY_LEN, SESSION_KEY_LEN};
pub use error::AuthClientError;
pub use login::{LoginOutput, LoginSession};
pub use registration::{RegistrationOutput, RegistrationSession};
pub use secret::SecretBytes;
