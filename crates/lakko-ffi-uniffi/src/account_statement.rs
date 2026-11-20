// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for `lakko-attest`'s account statement (ADR-0074 section
//! 0.3).
//!
//! Mirror of `crates/lakko-ffi-wasm/src/account_statement.rs` for iOS +
//! Android. **Sign only**: a device that holds the account's identity key
//! signs a 138-byte request (remove a device, and the recall and
//! session-upgrade kinds of the same ADR); `services/auth` and
//! `services/discovery` verify it under the identity public key they hold for
//! the account. No client verifies one.
//!
//! The signing time is the system clock and the nonce is fresh randomness,
//! both taken here, so a client supplies only what it means: its account
//! identity's 32-byte Ed25519 seed (`Identity.ed25519Seed()`), the kind, its
//! 16-byte account id and the kind's canonical body. A statement is good for
//! two minutes either side of the server's clock; sign it right before the
//! request that carries it and never store one.

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use lakko_attest::{
    device_revoke_body, recall_body, recall_start_body, session_upgrade_body, AccountStatement,
    AccountStatementKind as InnerKind, ACCOUNT_STATEMENT_HEADER, ACCOUNT_STATEMENT_LEN,
    ACCOUNT_STATEMENT_NONCE_LEN,
};
use lakko_crypto::signing::SigningKey;

use crate::FfiError;

/// What an account statement asks for (ADR-0074 section 0.3).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountStatementKind {
    /// Read whether a recall is open for the signing key. Body: empty.
    RecallStatus,
    /// Take the account back after a reset. Body: `server_nonce(32) ‖
    /// new_signature_pubkey(32) ‖ BLAKE3(password_upload) ‖
    /// BLAKE3(recovery_upload)`.
    Recall,
    /// Remove one device of the account. Body: the 32-byte device id.
    DeviceRevoke,
    /// Exchange a refresh credential of an older key generation. Body:
    /// `BLAKE3(refresh_token)`.
    SessionUpgrade,
    /// Open one password attempt of a recall (`POST /account/recall/start`).
    /// Body: `anchor_seq(8, big-endian) ‖ BLAKE3(login_request)`. Appended in
    /// sprint T3; use [`sign_recall_start_statement`].
    RecallStart,
}

impl From<AccountStatementKind> for InnerKind {
    fn from(kind: AccountStatementKind) -> Self {
        match kind {
            AccountStatementKind::RecallStatus => Self::RecallStatus,
            AccountStatementKind::Recall => Self::Recall,
            AccountStatementKind::DeviceRevoke => Self::DeviceRevoke,
            AccountStatementKind::SessionUpgrade => Self::SessionUpgrade,
            AccountStatementKind::RecallStart => Self::RecallStart,
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn sign_at(
    account_seed: &[u8],
    kind: InnerKind,
    account_id: &[u8],
    body: &[u8],
    unix_ms: i64,
    nonce: [u8; ACCOUNT_STATEMENT_NONCE_LEN],
) -> Result<Vec<u8>, FfiError> {
    let seed: [u8; 32] = account_seed.try_into().map_err(|_| {
        FfiError::AccountStatement("bad-seed-length: account seed must be 32 bytes".into())
    })?;
    let account: [u8; 16] = account_id.try_into().map_err(|_| {
        FfiError::AccountStatement("bad-account-id-length: account id must be 16 bytes".into())
    })?;
    let signing = SigningKey::from_seed(&seed);
    Ok(AccountStatement::new(kind, account, unix_ms, nonce, body)
        .sign(&signing)
        .to_vec())
}

/// Sign an account statement of `kind` for `account_id` over `body` (the
/// kind's canonical body) with the account identity's 32-byte Ed25519 seed.
/// Returns the 138-byte wire form; a caller base64url-encodes it (no padding)
/// for a `statement_b64` body field or the header
/// [`account_statement_header_name`] names.
///
/// # Errors
///
/// [`FfiError::AccountStatement`], message `"<code>: <reason>"`, codes
/// `bad-seed-length` and `bad-account-id-length`.
#[uniffi::export]
pub fn sign_account_statement(
    account_seed: Vec<u8>,
    kind: AccountStatementKind,
    account_id: Vec<u8>,
    body: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let mut nonce = [0u8; ACCOUNT_STATEMENT_NONCE_LEN];
    lakko_crypto::random::fill(&mut nonce);
    sign_at(
        &account_seed,
        kind.into(),
        &account_id,
        &body,
        now_ms(),
        nonce,
    )
}

/// Sign the statement that removes one device of the account
/// (`POST /account/devices/revoke` at auth, and the
/// [`account_statement_header_name`] header of discovery's
/// `DELETE /discovery/keypackage/device/{id}`): kind `DeviceRevoke` over the
/// 32-byte `device_id`. The same wire form serves both requests while it is
/// inside the two-minute window.
///
/// # Errors
///
/// [`FfiError::AccountStatement`] with code `bad-device-id-length` when
/// `device_id` is not 32 bytes; otherwise as [`sign_account_statement`].
#[uniffi::export]
pub fn sign_device_revoke_statement(
    account_seed: Vec<u8>,
    account_id: Vec<u8>,
    device_id: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let device: [u8; 32] = device_id.as_slice().try_into().map_err(|_| {
        FfiError::AccountStatement("bad-device-id-length: device id must be 32 bytes".into())
    })?;
    sign_account_statement(
        account_seed,
        AccountStatementKind::DeviceRevoke,
        account_id,
        device_revoke_body(&device).to_vec(),
    )
}

/// Decode one standard-base64 OPAQUE message the way the auth routes do.
fn opaque_bytes(field: &str, b64: &str) -> Result<Vec<u8>, FfiError> {
    STANDARD.decode(b64).map_err(|_| {
        FfiError::AccountStatement(format!("bad-base64: {field} is not standard base64"))
    })
}

/// Sign the statement `POST /account/recall/start` requires (ADR-0074 "T3 as
/// built"): kind `RecallStart` over the anchor's `anchor_seq` (from
/// `POST /account/recall/status`) and the OPAQUE login request, given as the
/// same standard-base64 string the request body carries
/// (`OpaqueLogin.requestB64()`). `account_seed` is the OLD account identity's
/// 32-byte Ed25519 seed kept in the recall record.
///
/// # Errors
///
/// [`FfiError::AccountStatement`] with code `bad-base64`; otherwise as
/// [`sign_account_statement`].
#[uniffi::export]
pub fn sign_recall_start_statement(
    account_seed: Vec<u8>,
    account_id: Vec<u8>,
    anchor_seq: u64,
    login_request_b64: String,
) -> Result<Vec<u8>, FfiError> {
    let login_request = opaque_bytes("login request", &login_request_b64)?;
    sign_account_statement(
        account_seed,
        AccountStatementKind::RecallStart,
        account_id,
        recall_start_body(anchor_seq, &login_request).to_vec(),
    )
}

/// Sign the statement `POST /account/recall/finish` requires: kind `Recall`
/// over the 32-byte `server_nonce` the start returned, the NEW identity's
/// 32-byte signature public key, and the two OPAQUE registration uploads (the
/// new password's and the new recovery credential's), given as the same
/// standard-base64 strings the request body carries
/// (`OpaqueRegisterOutput.uploadB64()`). `account_seed` is the OLD account
/// identity's seed.
///
/// # Errors
///
/// [`FfiError::AccountStatement`] with codes `bad-server-nonce-length`,
/// `bad-pubkey-length`, `bad-base64`; otherwise as
/// [`sign_account_statement`].
#[uniffi::export]
pub fn sign_recall_statement(
    account_seed: Vec<u8>,
    account_id: Vec<u8>,
    server_nonce: Vec<u8>,
    new_signature_pubkey: Vec<u8>,
    password_upload_b64: String,
    recovery_upload_b64: String,
) -> Result<Vec<u8>, FfiError> {
    let nonce: [u8; 32] = server_nonce.as_slice().try_into().map_err(|_| {
        FfiError::AccountStatement("bad-server-nonce-length: server nonce must be 32 bytes".into())
    })?;
    let pubkey: [u8; 32] = new_signature_pubkey.as_slice().try_into().map_err(|_| {
        FfiError::AccountStatement(
            "bad-pubkey-length: new signature pubkey must be 32 bytes".into(),
        )
    })?;
    let password_upload = opaque_bytes("password upload", &password_upload_b64)?;
    let recovery_upload = opaque_bytes("recovery upload", &recovery_upload_b64)?;
    sign_account_statement(
        account_seed,
        AccountStatementKind::Recall,
        account_id,
        recall_body(&nonce, &pubkey, &password_upload, &recovery_upload).to_vec(),
    )
}

/// Sign the statement that upgrades a kept device's session after a key
/// rotation (ADR-0074 section 3.3): kind `SessionUpgrade` over
/// `BLAKE3(refresh_token)`. Sent as `key_statement_b64` (base64url, no
/// padding) beside `refresh_token` on `POST /auth/session/refresh`, once the
/// device has ADOPTED the new account seed. `account_seed` is the NEW account
/// identity's 32-byte Ed25519 seed; `refresh_token` is the very `r2`
/// credential the request carries, of the previous key generation.
///
/// A device that holds only the old account key cannot make a statement the
/// server accepts: it stays signed out (`key-generation-superseded`).
///
/// # Errors
///
/// As [`sign_account_statement`].
#[uniffi::export]
pub fn sign_session_upgrade_statement(
    account_seed: Vec<u8>,
    account_id: Vec<u8>,
    refresh_token: String,
) -> Result<Vec<u8>, FfiError> {
    sign_account_statement(
        account_seed,
        AccountStatementKind::SessionUpgrade,
        account_id,
        session_upgrade_body(&refresh_token).to_vec(),
    )
}

/// [`sign_account_statement`] with the signing time given by the caller.
///
/// For the one case the device's own clock is known to be wrong: a route
/// answered `401 {"error":"statement-stale","server_time_ms":T}`. That
/// answer is given only to a statement whose signature verified, so the
/// device holds the right key and only its time is off; it signs the same
/// claims again with `unix_ms = T` (plus the milliseconds that passed since
/// it read the answer) and repeats the request. `body` is the kind's
/// canonical body: for the kinds whose body hashes a request field, build it
/// with [`recall_start_statement_body`], [`recall_statement_body`] or
/// [`session_upgrade_statement_body`]; `RecallStatus` has an empty body and
/// `DeviceRevoke` the 32-byte device id.
///
/// # Errors
///
/// As [`sign_account_statement`].
#[uniffi::export]
pub fn sign_account_statement_at(
    account_seed: Vec<u8>,
    kind: AccountStatementKind,
    account_id: Vec<u8>,
    body: Vec<u8>,
    unix_ms: i64,
) -> Result<Vec<u8>, FfiError> {
    let mut nonce = [0u8; ACCOUNT_STATEMENT_NONCE_LEN];
    lakko_crypto::random::fill(&mut nonce);
    sign_at(
        &account_seed,
        kind.into(),
        &account_id,
        &body,
        unix_ms,
        nonce,
    )
}

/// The canonical body of a `RecallStart` statement, for
/// [`sign_account_statement_at`]: `anchor_seq(8 BE) ‖ BLAKE3(login_request)`.
///
/// # Errors
///
/// [`FfiError::AccountStatement`] with code `bad-base64`.
#[uniffi::export]
pub fn recall_start_statement_body(
    anchor_seq: u64,
    login_request_b64: String,
) -> Result<Vec<u8>, FfiError> {
    let login_request = opaque_bytes("login request", &login_request_b64)?;
    Ok(recall_start_body(anchor_seq, &login_request).to_vec())
}

/// The canonical body of a `Recall` statement, for
/// [`sign_account_statement_at`]: `server_nonce ‖ new_signature_pubkey ‖
/// BLAKE3(password_upload) ‖ BLAKE3(recovery_upload)`.
///
/// # Errors
///
/// [`FfiError::AccountStatement`] with codes `bad-server-nonce-length`,
/// `bad-pubkey-length`, `bad-base64`.
#[uniffi::export]
pub fn recall_statement_body(
    server_nonce: Vec<u8>,
    new_signature_pubkey: Vec<u8>,
    password_upload_b64: String,
    recovery_upload_b64: String,
) -> Result<Vec<u8>, FfiError> {
    let nonce: [u8; 32] = server_nonce.as_slice().try_into().map_err(|_| {
        FfiError::AccountStatement("bad-server-nonce-length: server nonce must be 32 bytes".into())
    })?;
    let pubkey: [u8; 32] = new_signature_pubkey.as_slice().try_into().map_err(|_| {
        FfiError::AccountStatement(
            "bad-pubkey-length: new signature pubkey must be 32 bytes".into(),
        )
    })?;
    let password_upload = opaque_bytes("password upload", &password_upload_b64)?;
    let recovery_upload = opaque_bytes("recovery upload", &recovery_upload_b64)?;
    Ok(recall_body(&nonce, &pubkey, &password_upload, &recovery_upload).to_vec())
}

/// The canonical body of a `SessionUpgrade` statement, for
/// [`sign_account_statement_at`]: `BLAKE3(refresh_token)`.
#[uniffi::export]
#[must_use]
pub fn session_upgrade_statement_body(refresh_token: String) -> Vec<u8> {
    session_upgrade_body(&refresh_token).to_vec()
}

/// The request header that carries a statement on a route with no JSON body:
/// `x-lakko-account-statement`, value base64url without padding.
#[uniffi::export]
#[must_use]
pub fn account_statement_header_name() -> String {
    ACCOUNT_STATEMENT_HEADER.to_string()
}

/// The length of a statement's wire form (138 bytes).
#[uniffi::export]
#[must_use]
pub fn account_statement_len() -> u32 {
    u32::try_from(ACCOUNT_STATEMENT_LEN).unwrap_or(u32::MAX)
}

/// Sign the identity proof auth requires before it records an identity key
/// for an account (ADR-0089 security fix F3): the identity key's signature
/// over `"lakko/auth/identity-proof/v1\0" ‖ account_id ‖ pubkey`, where
/// `pubkey` is `account_identity`'s own signature public key. Returns the
/// 64-byte proof; the caller sends it as `identity_proof_b64` (base64url
/// without padding) with the genesis mint, the key reset finish, the key
/// rotation and the recall finish, signed by the identity being recorded
/// (the NEW identity on a reset, rotation or recall).
///
/// # Errors
///
/// [`FfiError::AccountStatement`] with code `bad-account-id-length` when
/// `account_id` is not 16 bytes.
#[uniffi::export]
pub fn sign_identity_proof(
    account_identity: std::sync::Arc<crate::Identity>,
    account_id: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let account: [u8; 16] = account_id.as_slice().try_into().map_err(|_| {
        FfiError::AccountStatement("bad-account-id-length: account id must be 16 bytes".into())
    })?;
    Ok(
        lakko_attest::sign_identity_proof(&account_identity.inner.ed25519_seed_bytes(), &account)
            .to_vec(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_attest::{verify_account_statement, AccountStatementError};

    const SEED: [u8; 32] = [0x21; 32];
    const ACCOUNT: [u8; 16] = [0xA1; 16];
    const DEVICE: [u8; 32] = [0xD1; 32];

    fn verifying() -> lakko_crypto::signing::VerifyingKey {
        SigningKey::from_seed(&SEED).verifying_key()
    }

    #[test]
    fn a_signed_revoke_statement_verifies_under_the_account_key() {
        let wire =
            sign_device_revoke_statement(SEED.to_vec(), ACCOUNT.to_vec(), DEVICE.to_vec()).unwrap();
        assert_eq!(wire.len(), account_statement_len() as usize);
        let claims = verify_account_statement(
            &wire,
            &verifying(),
            InnerKind::DeviceRevoke,
            &ACCOUNT,
            &DEVICE,
            now_ms(),
        )
        .unwrap();
        assert_eq!(claims.account_id, ACCOUNT);
        // Another device, another account and another key are each refused.
        assert_eq!(
            verify_account_statement(
                &wire,
                &verifying(),
                InnerKind::DeviceRevoke,
                &ACCOUNT,
                &[0xD2; 32],
                now_ms()
            ),
            Err(AccountStatementError::BodyMismatch)
        );
        let other = SigningKey::from_seed(&[0x22; 32]).verifying_key();
        assert_eq!(
            verify_account_statement(
                &wire,
                &other,
                InnerKind::DeviceRevoke,
                &ACCOUNT,
                &DEVICE,
                now_ms()
            ),
            Err(AccountStatementError::BadSignature)
        );
    }

    #[test]
    fn every_kind_maps_to_its_wire_byte_and_nonces_differ() {
        for (kind, byte) in [
            (AccountStatementKind::RecallStatus, 1u8),
            (AccountStatementKind::Recall, 2),
            (AccountStatementKind::DeviceRevoke, 3),
            (AccountStatementKind::SessionUpgrade, 4),
            (AccountStatementKind::RecallStart, 5),
        ] {
            let a = sign_account_statement(SEED.to_vec(), kind, ACCOUNT.to_vec(), vec![1, 2, 3])
                .unwrap();
            let b = sign_account_statement(SEED.to_vec(), kind, ACCOUNT.to_vec(), vec![1, 2, 3])
                .unwrap();
            assert_eq!(a[1], byte);
            assert_ne!(a[26..42], b[26..42], "a fresh nonce per statement");
        }
    }

    #[test]
    fn the_recall_statements_cover_what_the_server_rebuilds() {
        let login = STANDARD.encode(b"login request");
        let wire = sign_recall_start_statement(SEED.to_vec(), ACCOUNT.to_vec(), 3, login).unwrap();
        verify_account_statement(
            &wire,
            &verifying(),
            InnerKind::RecallStart,
            &ACCOUNT,
            &recall_start_body(3, b"login request"),
            now_ms(),
        )
        .unwrap();
        assert_eq!(
            verify_account_statement(
                &wire,
                &verifying(),
                InnerKind::RecallStart,
                &ACCOUNT,
                &recall_start_body(3, b"another request"),
                now_ms()
            ),
            Err(AccountStatementError::BodyMismatch)
        );

        let pw = STANDARD.encode(b"password upload");
        let rc = STANDARD.encode(b"recovery upload");
        let wire = sign_recall_statement(
            SEED.to_vec(),
            ACCOUNT.to_vec(),
            vec![7; 32],
            vec![8; 32],
            pw.clone(),
            rc.clone(),
        )
        .unwrap();
        verify_account_statement(
            &wire,
            &verifying(),
            InnerKind::Recall,
            &ACCOUNT,
            &recall_body(&[7; 32], &[8; 32], b"password upload", b"recovery upload"),
            now_ms(),
        )
        .unwrap();

        let code = |r: Result<Vec<u8>, FfiError>| match r {
            Err(FfiError::AccountStatement(m)) => m.split(':').next().unwrap_or("").to_string(),
            other => panic!("expected AccountStatement, got {other:?}"),
        };
        assert_eq!(
            code(sign_recall_start_statement(
                SEED.to_vec(),
                ACCOUNT.to_vec(),
                3,
                "!!!".into()
            )),
            "bad-base64"
        );
        assert_eq!(
            code(sign_recall_statement(
                SEED.to_vec(),
                ACCOUNT.to_vec(),
                vec![7; 31],
                vec![8; 32],
                pw.clone(),
                rc.clone()
            )),
            "bad-server-nonce-length"
        );
        assert_eq!(
            code(sign_recall_statement(
                SEED.to_vec(),
                ACCOUNT.to_vec(),
                vec![7; 32],
                vec![8; 33],
                pw,
                rc
            )),
            "bad-pubkey-length"
        );
    }

    #[test]
    fn the_session_upgrade_statement_covers_the_refresh_credential() {
        let refresh = "r2.aaaa.2.bbbb.1.2.cccc";
        let wire = sign_session_upgrade_statement(SEED.to_vec(), ACCOUNT.to_vec(), refresh.into())
            .unwrap();
        verify_account_statement(
            &wire,
            &verifying(),
            InnerKind::SessionUpgrade,
            &ACCOUNT,
            &session_upgrade_body(refresh),
            now_ms(),
        )
        .unwrap();
        assert_eq!(
            verify_account_statement(
                &wire,
                &verifying(),
                InnerKind::SessionUpgrade,
                &ACCOUNT,
                &session_upgrade_body("r2.another"),
                now_ms()
            ),
            Err(AccountStatementError::BodyMismatch)
        );
        assert_eq!(
            session_upgrade_statement_body(refresh.into()),
            session_upgrade_body(refresh)
        );
    }

    #[test]
    fn a_statement_signed_at_a_given_time_carries_it_and_the_body_builders_match() {
        let server_time = 1_900_000_000_000;
        let login = STANDARD.encode(b"login request");
        let body = recall_start_statement_body(7, login.clone()).unwrap();
        assert_eq!(body, recall_start_body(7, b"login request"));
        let wire = sign_account_statement_at(
            SEED.to_vec(),
            AccountStatementKind::RecallStart,
            ACCOUNT.to_vec(),
            body.clone(),
            server_time,
        )
        .unwrap();
        // Verifies at the given time, not at the device's.
        verify_account_statement(
            &wire,
            &verifying(),
            InnerKind::RecallStart,
            &ACCOUNT,
            &body,
            server_time,
        )
        .unwrap();
        assert_eq!(
            verify_account_statement(
                &wire,
                &verifying(),
                InnerKind::RecallStart,
                &ACCOUNT,
                &body,
                now_ms()
            ),
            Err(AccountStatementError::OutsideWindow)
        );
        let pw = STANDARD.encode(b"password upload");
        let rc = STANDARD.encode(b"recovery upload");
        assert_eq!(
            recall_statement_body(vec![7; 32], vec![8; 32], pw, rc).unwrap(),
            recall_body(&[7; 32], &[8; 32], b"password upload", b"recovery upload")
        );
        assert!(recall_start_statement_body(1, "!!".into()).is_err());
        assert!(recall_statement_body(vec![7; 31], vec![8; 32], login.clone(), login).is_err());
    }

    #[test]
    fn wrong_lengths_are_refused_with_a_code() {
        let code = |r: Result<Vec<u8>, FfiError>| match r {
            Err(FfiError::AccountStatement(m)) => m.split(':').next().unwrap_or("").to_string(),
            other => panic!("expected AccountStatement, got {other:?}"),
        };
        assert_eq!(
            code(sign_device_revoke_statement(
                vec![0; 31],
                ACCOUNT.to_vec(),
                DEVICE.to_vec()
            )),
            "bad-seed-length"
        );
        assert_eq!(
            code(sign_device_revoke_statement(
                SEED.to_vec(),
                vec![0; 15],
                DEVICE.to_vec()
            )),
            "bad-account-id-length"
        );
        assert_eq!(
            code(sign_device_revoke_statement(
                SEED.to_vec(),
                ACCOUNT.to_vec(),
                vec![0; 33]
            )),
            "bad-device-id-length"
        );
        assert_eq!(account_statement_header_name(), "x-lakko-account-statement");
    }

    #[test]
    fn the_identity_proof_verifies_under_the_identity_key() {
        let identity = std::sync::Arc::new(crate::Identity {
            inner: lakko_mls::Identity::from_seed(&[5; 64]).unwrap(),
        });
        let pubkey: [u8; 32] = identity.inner.signature_public_key().try_into().unwrap();
        let proof = sign_identity_proof(identity.clone(), vec![3; 16]).unwrap();
        assert_eq!(
            lakko_attest::verify_identity_proof(&proof, &[3; 16], &pubkey),
            Ok(())
        );
        assert!(lakko_attest::verify_identity_proof(&proof, &[4; 16], &pubkey).is_err());
        assert!(matches!(
            sign_identity_proof(identity, vec![3; 15]),
            Err(FfiError::AccountStatement(m)) if m.starts_with("bad-account-id-length: ")
        ));
    }
}
