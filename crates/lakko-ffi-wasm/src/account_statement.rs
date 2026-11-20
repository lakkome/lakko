// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for `lakko-attest`'s account statement (ADR-0074
//! section 0.3).
//!
//! Mirror of `crates/lakko-ffi-uniffi/src/account_statement.rs` for the web
//! client. **Sign only**: a device that holds the account's identity key
//! signs a 138-byte request (remove a device, and the recall and
//! session-upgrade kinds of the same ADR); `services/auth` and
//! `services/discovery` verify it under the identity public key they hold for
//! the account.
//!
//! The clock is `Date.now()` on wasm32 and the system clock on the host
//! (where the unit tests run); the nonce is fresh randomness. A statement is
//! good for two minutes either side of the server's clock: sign it right
//! before the request that carries it and never store one.
//!
//! A refusal throws an `Error` whose message is `"<code>: <reason>"`, with
//! the stable codes `bad-seed-length`, `bad-account-id-length`,
//! `bad-device-id-length`, `unknown-kind`, and for the two recall helpers
//! `bad-base64`, `bad-server-nonce-length`, `bad-pubkey-length`.

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use lakko_attest::{
    device_revoke_body, recall_body, recall_start_body, session_upgrade_body, AccountStatement,
    AccountStatementKind, ACCOUNT_STATEMENT_HEADER, ACCOUNT_STATEMENT_LEN,
    ACCOUNT_STATEMENT_NONCE_LEN,
};
use lakko_crypto::signing::SigningKey;
use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
fn now_ms() -> i64 {
    // `Date.now()` is integral milliseconds carried in an f64; a non-finite
    // value (a broken clock) reads as 0 and the server refuses.
    let now = js_sys::Date::now();
    if now.is_finite() {
        #[allow(clippy::cast_possible_truncation)]
        let ms = now as i64;
        ms
    } else {
        0
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The `JsError`-free core of `signAccountStatement`.
fn sign_inner(
    account_seed: &[u8],
    kind: u8,
    account_id: &[u8],
    body: &[u8],
) -> Result<Vec<u8>, String> {
    sign_inner_at(account_seed, kind, account_id, body, now_ms())
}

/// The `JsError`-free core of `signAccountStatementAt`: as [`sign_inner`]
/// with the signing time given.
fn sign_inner_at(
    account_seed: &[u8],
    kind: u8,
    account_id: &[u8],
    body: &[u8],
    unix_ms: i64,
) -> Result<Vec<u8>, String> {
    let seed: [u8; 32] = account_seed
        .try_into()
        .map_err(|_| "bad-seed-length: account seed must be 32 bytes".to_string())?;
    let kind = AccountStatementKind::from_byte(kind)
        .ok_or_else(|| format!("unknown-kind: {kind} is not an account statement kind"))?;
    let account: [u8; 16] = account_id
        .try_into()
        .map_err(|_| "bad-account-id-length: account id must be 16 bytes".to_string())?;
    let mut nonce = [0u8; ACCOUNT_STATEMENT_NONCE_LEN];
    lakko_crypto::random::fill(&mut nonce);
    Ok(AccountStatement::new(kind, account, unix_ms, nonce, body)
        .sign(&SigningKey::from_seed(&seed))
        .to_vec())
}

/// The canonical body of a `recall` statement from the request's fields.
fn recall_body_inner(
    server_nonce: &[u8],
    new_signature_pubkey: &[u8],
    password_upload_b64: &str,
    recovery_upload_b64: &str,
) -> Result<Vec<u8>, String> {
    let nonce: [u8; 32] = server_nonce
        .try_into()
        .map_err(|_| "bad-server-nonce-length: server nonce must be 32 bytes".to_string())?;
    let pubkey: [u8; 32] = new_signature_pubkey
        .try_into()
        .map_err(|_| "bad-pubkey-length: new signature pubkey must be 32 bytes".to_string())?;
    let password_upload = opaque_bytes("password upload", password_upload_b64)?;
    let recovery_upload = opaque_bytes("recovery upload", recovery_upload_b64)?;
    Ok(recall_body(&nonce, &pubkey, &password_upload, &recovery_upload).to_vec())
}

/// The `JsError`-free core of `signDeviceRevokeStatement`.
fn sign_revoke_inner(
    account_seed: &[u8],
    account_id: &[u8],
    device_id: &[u8],
) -> Result<Vec<u8>, String> {
    let device: [u8; 32] = device_id
        .try_into()
        .map_err(|_| "bad-device-id-length: device id must be 32 bytes".to_string())?;
    sign_inner(
        account_seed,
        AccountStatementKind::DeviceRevoke.to_byte(),
        account_id,
        &device_revoke_body(&device),
    )
}

/// Decode one standard-base64 OPAQUE message the way the auth routes do.
fn opaque_bytes(field: &str, b64: &str) -> Result<Vec<u8>, String> {
    STANDARD
        .decode(b64)
        .map_err(|_| format!("bad-base64: {field} is not standard base64"))
}

/// The `JsError`-free core of `signRecallStartStatement`.
fn sign_recall_start_inner(
    account_seed: &[u8],
    account_id: &[u8],
    anchor_seq: u64,
    login_request_b64: &str,
) -> Result<Vec<u8>, String> {
    let login_request = opaque_bytes("login request", login_request_b64)?;
    sign_inner(
        account_seed,
        AccountStatementKind::RecallStart.to_byte(),
        account_id,
        &recall_start_body(anchor_seq, &login_request),
    )
}

/// The `JsError`-free core of `signRecallStatement`.
fn sign_recall_inner(
    account_seed: &[u8],
    account_id: &[u8],
    server_nonce: &[u8],
    new_signature_pubkey: &[u8],
    password_upload_b64: &str,
    recovery_upload_b64: &str,
) -> Result<Vec<u8>, String> {
    sign_inner(
        account_seed,
        AccountStatementKind::Recall.to_byte(),
        account_id,
        &recall_body_inner(
            server_nonce,
            new_signature_pubkey,
            password_upload_b64,
            recovery_upload_b64,
        )?,
    )
}

/// Sign an account statement of `kind` (1 recall-status, 2 recall,
/// 3 device-revoke, 4 session-upgrade, 5 recall-start) for the 16-byte `accountId` over
/// `body` (the kind's canonical body) with the account identity's 32-byte
/// Ed25519 seed (`identity.ed25519Seed`). Returns the 138-byte wire form; the
/// caller base64url-encodes it (no padding) for a `statement_b64` body field
/// or the header `accountStatementHeaderName()` names.
///
/// # Errors
///
/// Throws `"<code>: <reason>"`: `bad-seed-length`, `unknown-kind`,
/// `bad-account-id-length`.
#[wasm_bindgen(js_name = signAccountStatement)]
pub fn sign_account_statement(
    account_seed: &[u8],
    kind: u8,
    account_id: &[u8],
    body: &[u8],
) -> Result<Vec<u8>, JsError> {
    sign_inner(account_seed, kind, account_id, body).map_err(|msg| JsError::new(&msg))
}

/// Sign the statement that removes one device of the account
/// (`POST /account/devices/revoke` at auth, and the
/// `accountStatementHeaderName()` header of discovery's
/// `DELETE /discovery/keypackage/device/{id}`): kind 3 over the 32-byte
/// `deviceId`. The same wire form serves both requests while it is inside the
/// two-minute window.
///
/// # Errors
///
/// Throws `"<code>: <reason>"`: `bad-device-id-length`, or as
/// `signAccountStatement`.
#[wasm_bindgen(js_name = signDeviceRevokeStatement)]
pub fn sign_device_revoke_statement(
    account_seed: &[u8],
    account_id: &[u8],
    device_id: &[u8],
) -> Result<Vec<u8>, JsError> {
    sign_revoke_inner(account_seed, account_id, device_id).map_err(|msg| JsError::new(&msg))
}

/// Sign the statement `POST /account/recall/start` requires (ADR-0074 "T3 as
/// built"): kind 5 over the anchor's `anchorSeq` (from
/// `POST /account/recall/status`) and the OPAQUE login request, given as the
/// same standard-base64 string the request body carries
/// (`OpaqueLogin.requestB64`). `accountSeed` is the OLD account identity's
/// 32-byte Ed25519 seed kept in the recall record.
///
/// # Errors
///
/// Throws `"<code>: <reason>"`: `bad-base64`, or as `signAccountStatement`.
#[wasm_bindgen(js_name = signRecallStartStatement)]
pub fn sign_recall_start_statement(
    account_seed: &[u8],
    account_id: &[u8],
    anchor_seq: u64,
    login_request_b64: &str,
) -> Result<Vec<u8>, JsError> {
    sign_recall_start_inner(account_seed, account_id, anchor_seq, login_request_b64)
        .map_err(|msg| JsError::new(&msg))
}

/// Sign the statement `POST /account/recall/finish` requires: kind 2 over
/// the 32-byte `serverNonce` the start returned, the NEW identity's 32-byte
/// signature public key, and the two OPAQUE registration uploads (the new
/// password's and the new recovery credential's), given as the same
/// standard-base64 strings the request body carries
/// (`OpaqueRegisterOutput.uploadB64`). `accountSeed` is the OLD account
/// identity's seed.
///
/// # Errors
///
/// Throws `"<code>: <reason>"`: `bad-server-nonce-length`,
/// `bad-pubkey-length`, `bad-base64`, or as `signAccountStatement`.
#[wasm_bindgen(js_name = signRecallStatement)]
pub fn sign_recall_statement(
    account_seed: &[u8],
    account_id: &[u8],
    server_nonce: &[u8],
    new_signature_pubkey: &[u8],
    password_upload_b64: &str,
    recovery_upload_b64: &str,
) -> Result<Vec<u8>, JsError> {
    sign_recall_inner(
        account_seed,
        account_id,
        server_nonce,
        new_signature_pubkey,
        password_upload_b64,
        recovery_upload_b64,
    )
    .map_err(|msg| JsError::new(&msg))
}

/// Sign the statement that upgrades a kept device's session after a key
/// rotation (ADR-0074 section 3.3): kind 4 over `BLAKE3(refreshToken)`. Sent
/// as `key_statement_b64` (base64url, no padding) beside `refresh_token` on
/// `POST /auth/session/refresh`, once the device has ADOPTED the new account
/// seed. `accountSeed` is the NEW account identity's 32-byte Ed25519 seed;
/// `refreshToken` is the very `r2` credential the request carries, of the
/// previous key generation.
///
/// A device that holds only the old account key cannot make a statement the
/// server accepts: it stays signed out (`key-generation-superseded`).
///
/// # Errors
///
/// As `signAccountStatement`.
#[wasm_bindgen(js_name = signSessionUpgradeStatement)]
pub fn sign_session_upgrade_statement(
    account_seed: &[u8],
    account_id: &[u8],
    refresh_token: &str,
) -> Result<Vec<u8>, JsError> {
    sign_inner(
        account_seed,
        AccountStatementKind::SessionUpgrade.to_byte(),
        account_id,
        &session_upgrade_body(refresh_token),
    )
    .map_err(|msg| JsError::new(&msg))
}

/// `signAccountStatement` with the signing time (`unixMs`, a `bigint`) given
/// by the caller.
///
/// For the one case the device's own clock is known to be wrong: a route
/// answered `401 {"error":"statement-stale","server_time_ms":T}`. That
/// answer is given only to a statement whose signature verified, so the
/// device holds the right key and only its time is off; it signs the same
/// claims again with `unixMs = T` (plus the milliseconds that passed since it
/// read the answer) and repeats the request. `body` is the kind's canonical
/// body: for the kinds whose body hashes a request field, build it with
/// `recallStartStatementBody`, `recallStatementBody` or
/// `sessionUpgradeStatementBody`; kind 1 has an empty body and kind 3 the
/// 32-byte device id.
///
/// # Errors
///
/// As `signAccountStatement`.
#[wasm_bindgen(js_name = signAccountStatementAt)]
pub fn sign_account_statement_at(
    account_seed: &[u8],
    kind: u8,
    account_id: &[u8],
    body: &[u8],
    unix_ms: i64,
) -> Result<Vec<u8>, JsError> {
    sign_inner_at(account_seed, kind, account_id, body, unix_ms).map_err(|msg| JsError::new(&msg))
}

/// The canonical body of a kind-5 `recall-start` statement, for
/// `signAccountStatementAt`: `anchor_seq(8 BE) ‖ BLAKE3(login_request)`.
///
/// # Errors
///
/// Throws `bad-base64`.
#[wasm_bindgen(js_name = recallStartStatementBody)]
pub fn recall_start_statement_body(
    anchor_seq: u64,
    login_request_b64: &str,
) -> Result<Vec<u8>, JsError> {
    opaque_bytes("login request", login_request_b64)
        .map(|login_request| recall_start_body(anchor_seq, &login_request).to_vec())
        .map_err(|msg| JsError::new(&msg))
}

/// The canonical body of a kind-2 `recall` statement, for
/// `signAccountStatementAt`: `server_nonce ‖ new_signature_pubkey ‖
/// BLAKE3(password_upload) ‖ BLAKE3(recovery_upload)`.
///
/// # Errors
///
/// Throws `bad-server-nonce-length`, `bad-pubkey-length`, `bad-base64`.
#[wasm_bindgen(js_name = recallStatementBody)]
pub fn recall_statement_body(
    server_nonce: &[u8],
    new_signature_pubkey: &[u8],
    password_upload_b64: &str,
    recovery_upload_b64: &str,
) -> Result<Vec<u8>, JsError> {
    recall_body_inner(
        server_nonce,
        new_signature_pubkey,
        password_upload_b64,
        recovery_upload_b64,
    )
    .map_err(|msg| JsError::new(&msg))
}

/// The canonical body of a kind-4 `session-upgrade` statement, for
/// `signAccountStatementAt`: `BLAKE3(refreshToken)`.
#[wasm_bindgen(js_name = sessionUpgradeStatementBody)]
#[must_use]
pub fn session_upgrade_statement_body(refresh_token: &str) -> Vec<u8> {
    session_upgrade_body(refresh_token).to_vec()
}

/// The request header that carries a statement on a route with no JSON body:
/// `x-lakko-account-statement`, value base64url without padding.
#[wasm_bindgen(js_name = accountStatementHeaderName)]
#[must_use]
pub fn account_statement_header_name() -> String {
    ACCOUNT_STATEMENT_HEADER.to_string()
}

/// The length of a statement's wire form (138 bytes).
#[wasm_bindgen(js_name = accountStatementLen)]
#[must_use]
pub fn account_statement_len() -> u32 {
    u32::try_from(ACCOUNT_STATEMENT_LEN).unwrap_or(u32::MAX)
}

/// The `JsError`-free core of `signIdentityProof`.
fn identity_proof_inner(account_seed: &[u8; 32], account_id: &[u8]) -> Result<Vec<u8>, String> {
    let account: [u8; 16] = account_id
        .try_into()
        .map_err(|_| "bad-account-id-length: account id must be 16 bytes".to_string())?;
    Ok(lakko_attest::sign_identity_proof(account_seed, &account).to_vec())
}

/// Sign the identity proof auth requires before it records an identity key
/// for an account (ADR-0098 security fix F3): `accountIdentity`'s signature
/// over `"lakko/auth/identity-proof/v1\0" ‖ accountId ‖ pubkey`, `pubkey`
/// being its own signature public key. Returns the 64-byte proof; send it as
/// `identity_proof_b64` (base64url without padding) with the genesis mint,
/// the key reset finish, the key rotation and the recall finish, signed by
/// the identity being recorded (the NEW identity on a reset, rotation or
/// recall).
///
/// # Errors
///
/// Throws `"bad-account-id-length: ..."` when `accountId` is not 16 bytes.
#[wasm_bindgen(js_name = signIdentityProof)]
pub fn sign_identity_proof(
    account_identity: &crate::mls::Identity,
    account_id: &[u8],
) -> Result<Vec<u8>, JsError> {
    identity_proof_inner(
        &account_identity.mls_identity().ed25519_seed_bytes(),
        account_id,
    )
    .map_err(|msg| JsError::new(&msg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_attest::{verify_account_statement, AccountStatementError};

    const SEED: [u8; 32] = [0x21; 32];
    const ACCOUNT: [u8; 16] = [0xA1; 16];
    const DEVICE: [u8; 32] = [0xD1; 32];

    #[test]
    fn the_session_upgrade_statement_and_the_timed_signer_cover_what_the_server_rebuilds() {
        let vk = SigningKey::from_seed(&SEED).verifying_key();
        let refresh = "r2.aaaa.2.bbbb.1.2.cccc";
        let wire = sign_inner(
            &SEED,
            AccountStatementKind::SessionUpgrade.to_byte(),
            &ACCOUNT,
            &session_upgrade_body(refresh),
        )
        .unwrap();
        verify_account_statement(
            &wire,
            &vk,
            AccountStatementKind::SessionUpgrade,
            &ACCOUNT,
            &session_upgrade_statement_body(refresh),
            now_ms(),
        )
        .unwrap();
        assert_eq!(
            verify_account_statement(
                &wire,
                &vk,
                AccountStatementKind::SessionUpgrade,
                &ACCOUNT,
                &session_upgrade_statement_body("r2.another"),
                now_ms()
            ),
            Err(AccountStatementError::BodyMismatch)
        );

        // Signed at a time the caller gives: verifies there, not now.
        let server_time = 1_900_000_000_000;
        let body = recall_start_body(7, b"login request");
        let wire = sign_inner_at(
            &SEED,
            AccountStatementKind::RecallStart.to_byte(),
            &ACCOUNT,
            &body,
            server_time,
        )
        .unwrap();
        verify_account_statement(
            &wire,
            &vk,
            AccountStatementKind::RecallStart,
            &ACCOUNT,
            &body,
            server_time,
        )
        .unwrap();
        assert_eq!(
            verify_account_statement(
                &wire,
                &vk,
                AccountStatementKind::RecallStart,
                &ACCOUNT,
                &body,
                now_ms()
            ),
            Err(AccountStatementError::OutsideWindow)
        );
        let pw = STANDARD.encode(b"password upload");
        let rc = STANDARD.encode(b"recovery upload");
        assert_eq!(
            recall_body_inner(&[7; 32], &[8; 32], &pw, &rc).unwrap(),
            recall_body(&[7; 32], &[8; 32], b"password upload", b"recovery upload")
        );
        assert!(recall_body_inner(&[7; 31], &[8; 32], &pw, &rc)
            .unwrap_err()
            .starts_with("bad-server-nonce-length"));
        assert!(sign_inner_at(&SEED, 9, &ACCOUNT, &[], server_time)
            .unwrap_err()
            .starts_with("unknown-kind"));
    }

    #[test]
    fn a_signed_revoke_statement_verifies_under_the_account_key() {
        let wire = sign_revoke_inner(&SEED, &ACCOUNT, &DEVICE).unwrap();
        assert_eq!(wire.len(), account_statement_len() as usize);
        let vk = SigningKey::from_seed(&SEED).verifying_key();
        verify_account_statement(
            &wire,
            &vk,
            AccountStatementKind::DeviceRevoke,
            &ACCOUNT,
            &DEVICE,
            now_ms(),
        )
        .unwrap();
        assert_eq!(
            verify_account_statement(
                &wire,
                &vk,
                AccountStatementKind::DeviceRevoke,
                &ACCOUNT,
                &[0xD2; 32],
                now_ms()
            ),
            Err(AccountStatementError::BodyMismatch)
        );
    }

    #[test]
    fn every_kind_byte_signs_and_others_are_refused() {
        for kind in 1u8..=5 {
            let a = sign_inner(&SEED, kind, &ACCOUNT, b"body").unwrap();
            let b = sign_inner(&SEED, kind, &ACCOUNT, b"body").unwrap();
            assert_eq!(a[1], kind);
            assert_ne!(a[26..42], b[26..42], "a fresh nonce per statement");
        }
        for kind in [0u8, 6, 255] {
            assert!(sign_inner(&SEED, kind, &ACCOUNT, b"")
                .unwrap_err()
                .starts_with("unknown-kind: "));
        }
    }

    #[test]
    fn the_recall_statements_cover_what_the_server_rebuilds() {
        let vk = SigningKey::from_seed(&SEED).verifying_key();
        let login = STANDARD.encode(b"login request");
        let wire = sign_recall_start_inner(&SEED, &ACCOUNT, 3, &login).unwrap();
        verify_account_statement(
            &wire,
            &vk,
            AccountStatementKind::RecallStart,
            &ACCOUNT,
            &recall_start_body(3, b"login request"),
            now_ms(),
        )
        .unwrap();
        assert_eq!(
            verify_account_statement(
                &wire,
                &vk,
                AccountStatementKind::RecallStart,
                &ACCOUNT,
                &recall_start_body(4, b"login request"),
                now_ms()
            ),
            Err(AccountStatementError::BodyMismatch)
        );

        let pw = STANDARD.encode(b"password upload");
        let rc = STANDARD.encode(b"recovery upload");
        let wire = sign_recall_inner(&SEED, &ACCOUNT, &[7; 32], &[8; 32], &pw, &rc).unwrap();
        verify_account_statement(
            &wire,
            &vk,
            AccountStatementKind::Recall,
            &ACCOUNT,
            &recall_body(&[7; 32], &[8; 32], b"password upload", b"recovery upload"),
            now_ms(),
        )
        .unwrap();

        assert!(sign_recall_start_inner(&SEED, &ACCOUNT, 3, "!!!")
            .unwrap_err()
            .starts_with("bad-base64: "));
        assert!(
            sign_recall_inner(&SEED, &ACCOUNT, &[7; 31], &[8; 32], &pw, &rc)
                .unwrap_err()
                .starts_with("bad-server-nonce-length: ")
        );
        assert!(
            sign_recall_inner(&SEED, &ACCOUNT, &[7; 32], &[8; 33], &pw, &rc)
                .unwrap_err()
                .starts_with("bad-pubkey-length: ")
        );
    }

    #[test]
    fn wrong_lengths_are_refused_with_a_code() {
        assert!(sign_revoke_inner(&[0; 31], &ACCOUNT, &DEVICE)
            .unwrap_err()
            .starts_with("bad-seed-length: "));
        assert!(sign_revoke_inner(&SEED, &[0; 15], &DEVICE)
            .unwrap_err()
            .starts_with("bad-account-id-length: "));
        assert!(sign_revoke_inner(&SEED, &ACCOUNT, &[0; 33])
            .unwrap_err()
            .starts_with("bad-device-id-length: "));
        assert_eq!(account_statement_header_name(), "x-lakko-account-statement");
    }

    #[test]
    fn the_identity_proof_verifies_under_the_seed_key() {
        let pubkey = SigningKey::from_seed(&SEED).verifying_key().to_bytes();
        let proof = identity_proof_inner(&SEED, &ACCOUNT).unwrap();
        assert_eq!(
            lakko_attest::verify_identity_proof(&proof, &ACCOUNT, &pubkey),
            Ok(())
        );
        assert!(identity_proof_inner(&SEED, &[1; 15])
            .unwrap_err()
            .starts_with("bad-account-id-length: "));
    }
}
