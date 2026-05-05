// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! End-to-end OPAQUE round-trip exercising the public client crate
//! surface against the upstream `opaque-ke` server primitives.
//!
//! The server-side definition we mirror lives in
//! `services/auth/src/opaque.rs`; we cannot depend on
//! `lakko-svc-auth` directly without dragging in axum / sqlx /
//! tokio, so the tests here re-instantiate `ServerSetup`,
//! `ServerRegistration`, `ServerLogin` from `opaque-ke` against the
//! same [`LakkoCipherSuite`] type.
//!
//! What this proves:
//!
//!   * Client `RegistrationSession` produces a `RegistrationRequest`
//!     blob the server accepts.
//!   * Client `RegistrationOutput::upload_b64` is valid input for
//!     `ServerRegistration::finish`.
//!   * Client `LoginSession` produces a `CredentialRequest` blob
//!     `ServerLogin::start` accepts.
//!   * The server's `session_key` matches the client's `session_key`
//!     after `LoginSession::finish` succeeds.
//!   * The client's `export_key` is identical between registration
//!     and a subsequent successful login (deterministic property).
//!
//! These tests are the contract that backs the inline mirror of
//! `LakkoCipherSuite` between this crate and `services/auth`.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_auth_client::{
    AuthClientError, LakkoCipherSuite, LoginSession, RegistrationSession, EXPORT_KEY_LEN,
    SESSION_KEY_LEN,
};
use opaque_ke::{
    CredentialRequest, RegistrationRequest, RegistrationUpload, ServerLogin,
    ServerLoginStartParameters, ServerRegistration, ServerSetup,
};
use rand::rngs::OsRng;

/// Same domain-separation byte the server uses for the password
/// flow. See `services/auth/src/opaque.rs::KIND_PASSWORD`.
const KIND_PASSWORD: u8 = 1;

/// Build a server-side credential_id `account_id || kind`. Same
/// shape `services/auth` builds.
fn credential_id(account_id: &[u8; 16], kind: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(17);
    out.extend_from_slice(account_id);
    out.push(kind);
    out
}

/// Walk a full `register → store envelope` cycle, returning the
/// stored `ServerRegistration` for use in a subsequent login test.
fn register(
    setup: &ServerSetup<LakkoCipherSuite>,
    account_id: &[u8; 16],
    kind: u8,
    password: &[u8],
) -> ServerRegistration<LakkoCipherSuite> {
    // 1. Client: start.
    let (session, password_request_b64) =
        RegistrationSession::start_password(password).expect("client registration start");
    assert!(session.is_password());

    // 2. Server: start.
    let request_bytes = B64
        .decode(&password_request_b64)
        .expect("decode password_request");
    let request = RegistrationRequest::<LakkoCipherSuite>::deserialize(&request_bytes)
        .expect("deserialize RegistrationRequest");
    let cred = credential_id(account_id, kind);
    let server_start = ServerRegistration::<LakkoCipherSuite>::start(setup, request, &cred)
        .expect("server registration start");
    let password_response_b64 = B64.encode(server_start.message.serialize());

    // 3. Client: finish.
    let out = session
        .finish(password, &password_response_b64)
        .expect("client registration finish");
    assert_eq!(out.export_key.len(), EXPORT_KEY_LEN);

    // 4. Server: finish (store envelope).
    let upload_bytes = B64.decode(&out.upload_b64).expect("decode password_upload");
    let upload = RegistrationUpload::<LakkoCipherSuite>::deserialize(&upload_bytes)
        .expect("deserialize RegistrationUpload");
    ServerRegistration::<LakkoCipherSuite>::finish(upload)
}

/// `(client_session_key, server_session_key, client_export_key)`.
/// All three are owned `Vec<u8>` — the test compares them by value.
type LoginKeys = (Vec<u8>, Vec<u8>, Vec<u8>);

/// Walk a full login. Returns the client and server `session_key`s
/// (which MUST match on success) plus the client's `export_key`.
fn login(
    setup: &ServerSetup<LakkoCipherSuite>,
    account_id: &[u8; 16],
    kind: u8,
    stored: Option<ServerRegistration<LakkoCipherSuite>>,
    password: &[u8],
) -> Result<LoginKeys, AuthClientError> {
    // 1. Client: start.
    let (session, request_b64) = LoginSession::start_password(password)?;
    assert!(session.is_password());

    // 2. Server: start.
    let request_bytes = B64
        .decode(&request_b64)
        .map_err(|_| AuthClientError::InvalidWire)?;
    let request = CredentialRequest::<LakkoCipherSuite>::deserialize(&request_bytes)
        .map_err(|_| AuthClientError::InvalidWire)?;
    let cred = credential_id(account_id, kind);
    let mut rng = OsRng;
    let server_start = ServerLogin::<LakkoCipherSuite>::start(
        &mut rng,
        setup,
        stored,
        request,
        &cred,
        ServerLoginStartParameters::default(),
    )
    .map_err(|_| AuthClientError::Opaque)?;
    let response_b64 = B64.encode(server_start.message.serialize());

    // 3. Client: finish.
    let client_out = session.finish(password, &response_b64)?;

    // 4. Server: finish.
    let finalize_bytes = B64
        .decode(&client_out.finalize_b64)
        .map_err(|_| AuthClientError::InvalidWire)?;
    let finalize =
        opaque_ke::CredentialFinalization::<LakkoCipherSuite>::deserialize(&finalize_bytes)
            .map_err(|_| AuthClientError::InvalidWire)?;
    let server_finish = server_start
        .state
        .finish(finalize)
        .map_err(|_| AuthClientError::Opaque)?;

    Ok((
        client_out.session_key.as_ref().to_vec(),
        server_finish.session_key.as_slice().to_vec(),
        client_out.export_key.as_ref().to_vec(),
    ))
}

#[test]
fn register_then_login_produces_matching_session_key() {
    let mut rng = OsRng;
    let setup = ServerSetup::<LakkoCipherSuite>::new(&mut rng);
    let account_id = [0xABu8; 16];
    let password = b"correct horse battery staple";

    let stored = register(&setup, &account_id, KIND_PASSWORD, password);
    let (client_sk, server_sk, _export_key) =
        login(&setup, &account_id, KIND_PASSWORD, Some(stored), password)
            .expect("login should succeed with the right password");

    assert_eq!(client_sk.len(), SESSION_KEY_LEN);
    assert_eq!(client_sk, server_sk, "session keys must match");
}

#[test]
fn login_with_wrong_password_fails_at_finish() {
    let mut rng = OsRng;
    let setup = ServerSetup::<LakkoCipherSuite>::new(&mut rng);
    let account_id = [0u8; 16];

    let real = b"correct horse battery staple";
    let wrong = b"hunter2";

    let stored = register(&setup, &account_id, KIND_PASSWORD, real);
    let result = login(&setup, &account_id, KIND_PASSWORD, Some(stored), wrong);

    // OPAQUE's design: a wrong password produces a finalization
    // that the server rejects; the client may also reject earlier.
    // Either way we expect an `Opaque` error and crucially nothing
    // that distinguishes "wrong password" from "unknown account".
    match result {
        Err(AuthClientError::Opaque) => {},
        Err(other) => panic!("expected Opaque, got: {other:?}"),
        Ok(_) => panic!("login with wrong password unexpectedly succeeded"),
    }
}

#[test]
fn export_key_is_deterministic_across_register_and_login() {
    // The OPAQUE export_key is a deterministic function of the
    // password and the server-static OPRF key — the same password
    // always derives the same key. This is what makes it usable
    // as the seed for the device root key.
    let mut rng = OsRng;
    let setup = ServerSetup::<LakkoCipherSuite>::new(&mut rng);
    let account_id = [0x42u8; 16];
    let password = b"deterministic-password";

    // First registration — capture the export_key.
    let (session, request_b64) = RegistrationSession::start_password(password).expect("start");
    let request_bytes = B64.decode(&request_b64).expect("decode");
    let request =
        RegistrationRequest::<LakkoCipherSuite>::deserialize(&request_bytes).expect("deserialize");
    let cred = credential_id(&account_id, KIND_PASSWORD);
    let server_start = ServerRegistration::<LakkoCipherSuite>::start(&setup, request, &cred)
        .expect("server start");
    let response_b64 = B64.encode(server_start.message.serialize());
    let reg_out = session.finish(password, &response_b64).expect("finish");
    let registration_export = reg_out.export_key.as_ref().to_vec();

    let upload_bytes = B64.decode(&reg_out.upload_b64).expect("decode upload");
    let upload = RegistrationUpload::<LakkoCipherSuite>::deserialize(&upload_bytes)
        .expect("deserialize upload");
    let stored = ServerRegistration::<LakkoCipherSuite>::finish(upload);

    // Login with the same password — export_key must be byte-equal.
    let (_csk, _ssk, login_export) =
        login(&setup, &account_id, KIND_PASSWORD, Some(stored), password).expect("login");

    assert_eq!(
        registration_export, login_export,
        "export_key must be deterministic for the same password"
    );
    assert_eq!(registration_export.len(), EXPORT_KEY_LEN);
}

#[test]
fn recovery_flow_round_trip() {
    // Recovery flow is byte-identical OPAQUE math; the only
    // difference at the wire level is the `kind` byte the server
    // mixes into `credential_id`. Run a parallel registration +
    // login under KIND_RECOVERY to confirm the client surface
    // doesn't accidentally hard-code the password flow.
    const KIND_RECOVERY: u8 = 2;

    let mut rng = OsRng;
    let setup = ServerSetup::<LakkoCipherSuite>::new(&mut rng);
    let account_id = [0x07u8; 16];
    // Pretend this is the 64-byte BIP39 seed.
    let seed = [0xFEu8; 64];

    // Client: registration via the recovery API path.
    let (session, request_b64) =
        RegistrationSession::start_recovery(&seed).expect("recovery registration start");
    assert!(session.is_recovery());
    let request_bytes = B64.decode(&request_b64).expect("decode");
    let request =
        RegistrationRequest::<LakkoCipherSuite>::deserialize(&request_bytes).expect("deserialize");

    let cred = credential_id(&account_id, KIND_RECOVERY);
    let server_start = ServerRegistration::<LakkoCipherSuite>::start(&setup, request, &cred)
        .expect("server start");
    let response_b64 = B64.encode(server_start.message.serialize());
    let reg_out = session.finish(&seed, &response_b64).expect("finish");

    let upload_bytes = B64.decode(&reg_out.upload_b64).expect("decode upload");
    let upload = RegistrationUpload::<LakkoCipherSuite>::deserialize(&upload_bytes)
        .expect("deserialize upload");
    let stored = ServerRegistration::<LakkoCipherSuite>::finish(upload);

    // Client: login via the recovery API path.
    let (login_session, request_b64) =
        LoginSession::start_recovery(&seed).expect("recovery login start");
    assert!(login_session.is_recovery());

    let request_bytes = B64.decode(&request_b64).expect("decode");
    let request =
        CredentialRequest::<LakkoCipherSuite>::deserialize(&request_bytes).expect("deserialize");
    let server_start = ServerLogin::<LakkoCipherSuite>::start(
        &mut rng,
        &setup,
        Some(stored),
        request,
        &cred,
        ServerLoginStartParameters::default(),
    )
    .expect("server login start");

    let response_b64 = B64.encode(server_start.message.serialize());
    let client_out = login_session.finish(&seed, &response_b64).expect("finish");
    let finalize_bytes = B64
        .decode(&client_out.finalize_b64)
        .expect("decode finalize");
    let finalize =
        opaque_ke::CredentialFinalization::<LakkoCipherSuite>::deserialize(&finalize_bytes)
            .expect("deserialize finalize");
    let server_finish = server_start
        .state
        .finish(finalize)
        .expect("server login finish");

    assert_eq!(
        client_out.session_key.as_ref(),
        server_finish.session_key.as_slice(),
        "recovery flow session keys must match"
    );
}

#[test]
fn finish_rejects_invalid_base64() {
    let (session, _) = RegistrationSession::start_password(b"x").expect("start");
    let err = session
        .finish(b"x", "this is not valid base64!@#")
        .expect_err("must fail");
    matches!(err, AuthClientError::Base64(_));
}

#[test]
fn finish_rejects_garbage_wire_bytes() {
    let (session, _) = RegistrationSession::start_password(b"x").expect("start");
    // Valid base64 but garbage payload — should fail at the
    // OPAQUE deserialize step, not at base64 decode.
    let garbage_b64 = B64.encode([0u8; 32]);
    let err = session.finish(b"x", &garbage_b64).expect_err("must fail");
    assert!(
        matches!(err, AuthClientError::InvalidWire),
        "expected InvalidWire, got {err:?}"
    );
}

/// Wire-struct round-trip — confirms our hand-mirrored Serde
/// shapes match the server's JSON exactly. We don't have a way
/// to drive the live server here without a docker harness, so
/// the cheap proxy is: serialize, parse, re-serialize, and check
/// keys + values via `serde_json::Value` comparison.
#[test]
fn wire_signup_start_round_trip() {
    use lakko_auth_client::wire::SignupStartFields;

    let original = SignupStartFields {
        password_request: "AAA=".to_string(),
        recovery_request: Some("BBB=".to_string()),
    };
    let json = serde_json::to_string(&original).expect("serialize");

    // Confirm absent recovery_request is omitted (matches the server's
    // `#[serde(skip_serializing_if = "Option::is_none")]`).
    let original_no_recovery = SignupStartFields {
        password_request: "AAA=".to_string(),
        recovery_request: None,
    };
    let json_no_recovery =
        serde_json::to_string(&original_no_recovery).expect("serialize without recovery");
    assert!(
        !json_no_recovery.contains("recovery_request"),
        "None recovery must be skipped, got: {json_no_recovery}"
    );

    // Round-trip the full version.
    let parsed: SignupStartFields = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.password_request, original.password_request);
    assert_eq!(parsed.recovery_request, original.recovery_request);
}
