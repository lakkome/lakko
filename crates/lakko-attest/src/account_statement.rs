// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Account-key statement ([ADR-0074] section 0.3).
//!
//! An *account statement* is a compact, domain-separated, Ed25519-signed
//! request by a **client** that holds the account's identity key. It is the
//! first kind in this crate that a client signs and a service verifies: auth
//! and discovery each hold the account's identity public key, so a request
//! can prove "the caller holds the account key" beside (or without) a
//! session.
//!
//! ```text
//! wire   = version(1)=0x01 ‖ kind(1) ‖ account_id(16) ‖ unix_ms(8, big-endian i64)
//!          ‖ nonce(16) ‖ body_hash(32) ‖ Ed25519_sig(64)            = 138 bytes
//! signed = "lakko/account-statement/v1" ‖ 0x00 ‖ wire[0..74]
//! ```
//!
//! `body_hash` is BLAKE3 over the kind's canonical body, which is not on the
//! wire: the verifier rebuilds it from what the request names (a device id, a
//! refresh credential, ...) and compares.
//!
//! | Kind | Name | Canonical body | Verified by |
//! |---|---|---|---|
//! | 1 | `recall-status` | empty | auth, under a recall anchor's key |
//! | 2 | `recall` | `server_nonce(32) ‖ new_signature_pubkey(32) ‖ BLAKE3(password_upload) ‖ BLAKE3(recovery_upload)` | auth, under a recall anchor's key |
//! | 3 | `device-revoke` | `device_id(32)` | auth and discovery, under the current key |
//! | 4 | `session-upgrade` | `BLAKE3(refresh_token)` | auth, under the current key |
//! | 5 | `recall-start` | `anchor_seq(8, big-endian u64) ‖ BLAKE3(login_request)` | auth, under a recall anchor's key |
//!
//! [`verify_account_statement`] is the one verify entry point and checks, in
//! this order: layout, kind, account id, `unix_ms` within
//! [`ACCOUNT_STATEMENT_WINDOW_MS`] of the verifier's clock (either side),
//! signature, body hash. Every refusal is an error; nothing here is
//! best-effort.
//!
//! A statement is not a session and carries no secret. Kinds 2 to 5 are bound
//! by their body to a server nonce or to something the caller must also
//! present (a bearer and a fresh-auth grant, a refresh credential, the OPAQUE
//! login request whose blind only the caller holds), so a captured statement
//! is useless alone; kind 1 authorises a read and may be replayed inside the
//! window.
//!
//! [ADR-0074]: ../../../docs/adr/0074-account-takeover-resistance.md

use lakko_crypto::hash;
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN, SIGNATURE_LEN};
use thiserror::Error;

/// Domain separator mixed into the signing input, followed by one `0x00`
/// byte. Distinct from every other `lakko-attest` kind. An account identity
/// key signs other things too (device bindings, supersession proofs, DS
/// proofs); none of them starts with this string.
const DOMAIN: &[u8] = b"lakko/account-statement/v1";

/// Wire-format version. The verifier refuses any other value.
const VERSION: u8 = 1;

/// Length of a raw account id (UUIDv7 bytes).
pub const ACCOUNT_ID_LEN: usize = 16;

/// Length of the per-statement random nonce.
pub const ACCOUNT_STATEMENT_NONCE_LEN: usize = 16;

/// Length of `body_hash` (a BLAKE3 output).
pub const BODY_HASH_LEN: usize = 32;

/// Length of a device id: the device's conversation leaf signature key.
pub const DEVICE_ID_LEN: usize = PUBLIC_KEY_LEN;

/// Length of the big-endian `i64` signing time.
const UNIX_MS_LEN: usize = 8;

/// Length of the signed prefix of the wire form: every field but the
/// signature.
const SIGNED_PREFIX_LEN: usize =
    1 + 1 + ACCOUNT_ID_LEN + UNIX_MS_LEN + ACCOUNT_STATEMENT_NONCE_LEN + BODY_HASH_LEN;

/// Total serialized account-statement length.
pub const ACCOUNT_STATEMENT_LEN: usize = SIGNED_PREFIX_LEN + SIGNATURE_LEN;

/// How far `unix_ms` may lie from the verifier's clock, either side. The
/// `lakko-ds-proof` window.
pub const ACCOUNT_STATEMENT_WINDOW_MS: i64 = 120_000;

/// Request header that carries a statement on a route with no JSON body
/// (discovery's `DELETE /discovery/keypackage/device/{id}`). The value is
/// base64url without padding of the [`ACCOUNT_STATEMENT_LEN`] wire bytes.
pub const ACCOUNT_STATEMENT_HEADER: &str = "x-lakko-account-statement";

/// Length of the canonical body of a [`AccountStatementKind::Recall`]
/// statement.
pub const RECALL_BODY_LEN: usize = 32 + PUBLIC_KEY_LEN + 32 + 32;

/// What a statement asks for. The wire byte is the discriminant; an unknown
/// byte is refused at decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum AccountStatementKind {
    /// Read whether a recall is open for the signing key. Body: empty.
    RecallStatus = 1,
    /// Take the account back after a reset. Body: [`recall_body`].
    Recall = 2,
    /// Remove one device of the account. Body: the 32-byte device id.
    DeviceRevoke = 3,
    /// Exchange a refresh credential of an older key generation for a pair at
    /// the current one. Body: [`session_upgrade_body`].
    SessionUpgrade = 4,
    /// Open one password attempt of a recall: the OPAQUE login start against
    /// the anchored password. Body: [`recall_start_body`]. Added in sprint T3
    /// (ADR-0074 "T3 as built"): the login start is the step that answers a
    /// password guess, so it is the step that must prove the anchor key.
    RecallStart = 5,
}

impl AccountStatementKind {
    /// The wire byte.
    #[must_use]
    pub const fn to_byte(self) -> u8 {
        self as u8
    }

    /// Parse the wire byte; `None` for any value that is not a kind.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::RecallStatus),
            2 => Some(Self::Recall),
            3 => Some(Self::DeviceRevoke),
            4 => Some(Self::SessionUpgrade),
            5 => Some(Self::RecallStart),
            _ => None,
        }
    }
}

/// Why an account statement was rejected.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AccountStatementError {
    /// The byte slice is not exactly [`ACCOUNT_STATEMENT_LEN`] long.
    #[error("account statement has wrong length: {got} bytes")]
    BadLength {
        /// The actual length received.
        got: usize,
    },
    /// The version byte is not the supported one.
    #[error("unsupported account statement version: {0}")]
    BadVersion(u8),
    /// The kind byte names no kind.
    #[error("unknown account statement kind: {0}")]
    UnknownKind(u8),
    /// The statement is of another kind than the route accepts.
    #[error("account statement is of another kind than expected")]
    KindMismatch,
    /// The statement names another account than expected.
    #[error("account statement names another account")]
    AccountMismatch,
    /// `unix_ms` lies outside the window around the verifier's clock.
    #[error("account statement is outside the time window")]
    OutsideWindow,
    /// The Ed25519 signature did not verify (tampered field, wrong key, or
    /// malformed signature bytes).
    #[error("account statement signature is invalid")]
    BadSignature,
    /// The signed `body_hash` is not the hash of the expected body.
    #[error("account statement does not cover the expected body")]
    BodyMismatch,
}

/// The claims an account statement carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountStatement {
    /// What the statement asks for.
    pub kind: AccountStatementKind,
    /// The account the signer speaks for.
    pub account_id: [u8; ACCOUNT_ID_LEN],
    /// Signing time, Unix milliseconds.
    pub unix_ms: i64,
    /// Random per statement.
    pub nonce: [u8; ACCOUNT_STATEMENT_NONCE_LEN],
    /// BLAKE3 over the kind's canonical body.
    pub body_hash: [u8; BODY_HASH_LEN],
}

impl AccountStatement {
    /// Construct claims over `body`, the kind's canonical body.
    #[must_use]
    pub fn new(
        kind: AccountStatementKind,
        account_id: [u8; ACCOUNT_ID_LEN],
        unix_ms: i64,
        nonce: [u8; ACCOUNT_STATEMENT_NONCE_LEN],
        body: &[u8],
    ) -> Self {
        Self {
            kind,
            account_id,
            unix_ms,
            nonce,
            body_hash: hash::blake3(body),
        }
    }

    /// The signed prefix of the wire form.
    fn prefix(&self) -> [u8; SIGNED_PREFIX_LEN] {
        let mut out = [0u8; SIGNED_PREFIX_LEN];
        out[0] = VERSION;
        out[1] = self.kind.to_byte();
        let mut off = 2;
        out[off..off + ACCOUNT_ID_LEN].copy_from_slice(&self.account_id);
        off += ACCOUNT_ID_LEN;
        out[off..off + UNIX_MS_LEN].copy_from_slice(&self.unix_ms.to_be_bytes());
        off += UNIX_MS_LEN;
        out[off..off + ACCOUNT_STATEMENT_NONCE_LEN].copy_from_slice(&self.nonce);
        off += ACCOUNT_STATEMENT_NONCE_LEN;
        out[off..off + BODY_HASH_LEN].copy_from_slice(&self.body_hash);
        out
    }

    /// The exact bytes the signature covers: the domain, one zero byte, the
    /// signed prefix.
    fn signing_input(prefix: &[u8; SIGNED_PREFIX_LEN]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(DOMAIN.len() + 1 + SIGNED_PREFIX_LEN);
        buf.extend_from_slice(DOMAIN);
        buf.push(0);
        buf.extend_from_slice(prefix);
        buf
    }

    /// Serialize and sign these claims into the [`ACCOUNT_STATEMENT_LEN`]-byte
    /// wire form. `signing` is the account's identity signing key.
    #[must_use]
    pub fn sign(&self, signing: &SigningKey) -> [u8; ACCOUNT_STATEMENT_LEN] {
        let prefix = self.prefix();
        let sig = signing.sign(&Self::signing_input(&prefix));
        let mut out = [0u8; ACCOUNT_STATEMENT_LEN];
        out[..SIGNED_PREFIX_LEN].copy_from_slice(&prefix);
        out[SIGNED_PREFIX_LEN..].copy_from_slice(&sig.to_bytes());
        out
    }

    /// Parse the wire layout WITHOUT verifying the signature. For a verifier
    /// that must read the account id or the kind to find the key to verify
    /// under; nothing the result says is authenticated until
    /// [`verify_account_statement`] has accepted the same bytes.
    ///
    /// # Errors
    ///
    /// [`AccountStatementError::BadLength`], [`AccountStatementError::BadVersion`],
    /// [`AccountStatementError::UnknownKind`].
    pub fn decode_unverified(bytes: &[u8]) -> Result<Self, AccountStatementError> {
        let wire: &[u8; ACCOUNT_STATEMENT_LEN] = bytes
            .try_into()
            .map_err(|_| AccountStatementError::BadLength { got: bytes.len() })?;
        if wire[0] != VERSION {
            return Err(AccountStatementError::BadVersion(wire[0]));
        }
        let kind = AccountStatementKind::from_byte(wire[1])
            .ok_or(AccountStatementError::UnknownKind(wire[1]))?;
        let mut off = 2;
        let mut account_id = [0u8; ACCOUNT_ID_LEN];
        account_id.copy_from_slice(&wire[off..off + ACCOUNT_ID_LEN]);
        off += ACCOUNT_ID_LEN;
        let mut unix_ms = [0u8; UNIX_MS_LEN];
        unix_ms.copy_from_slice(&wire[off..off + UNIX_MS_LEN]);
        off += UNIX_MS_LEN;
        let mut nonce = [0u8; ACCOUNT_STATEMENT_NONCE_LEN];
        nonce.copy_from_slice(&wire[off..off + ACCOUNT_STATEMENT_NONCE_LEN]);
        off += ACCOUNT_STATEMENT_NONCE_LEN;
        let mut body_hash = [0u8; BODY_HASH_LEN];
        body_hash.copy_from_slice(&wire[off..off + BODY_HASH_LEN]);
        Ok(Self {
            kind,
            account_id,
            unix_ms: i64::from_be_bytes(unix_ms),
            nonce,
            body_hash,
        })
    }

    /// Parse the wire form and verify the Ed25519 signature against
    /// `verifying`. Checks no kind, account, time or body; callers use
    /// [`verify_account_statement`].
    ///
    /// # Errors
    ///
    /// The layout errors of [`Self::decode_unverified`];
    /// [`AccountStatementError::BadSignature`] if the signature is malformed
    /// or does not verify.
    pub fn open(bytes: &[u8], verifying: &VerifyingKey) -> Result<Self, AccountStatementError> {
        let claims = Self::decode_unverified(bytes)?;
        verify_signature(bytes, verifying)?;
        Ok(claims)
    }

    /// `true` when `unix_ms` lies within [`ACCOUNT_STATEMENT_WINDOW_MS`] of
    /// `now_unix_ms`, either side. Saturating, so extreme values cannot
    /// overflow.
    #[must_use]
    pub const fn within_window(&self, now_unix_ms: i64) -> bool {
        let lower = now_unix_ms.saturating_sub(ACCOUNT_STATEMENT_WINDOW_MS);
        let upper = now_unix_ms.saturating_add(ACCOUNT_STATEMENT_WINDOW_MS);
        self.unix_ms >= lower && self.unix_ms <= upper
    }
}

/// Verify the signature over the signed prefix of a wire form whose layout
/// has already been accepted.
fn verify_signature(bytes: &[u8], verifying: &VerifyingKey) -> Result<(), AccountStatementError> {
    let wire: &[u8; ACCOUNT_STATEMENT_LEN] = bytes
        .try_into()
        .map_err(|_| AccountStatementError::BadLength { got: bytes.len() })?;
    let mut prefix = [0u8; SIGNED_PREFIX_LEN];
    prefix.copy_from_slice(&wire[..SIGNED_PREFIX_LEN]);
    let sig = Signature::from_bytes(&wire[SIGNED_PREFIX_LEN..])
        .map_err(|_| AccountStatementError::BadSignature)?;
    // A small-order key verifies forged signatures; no account's key is one,
    // so a statement "under" such a key is refused outright.
    if verifying.is_weak() {
        return Err(AccountStatementError::BadSignature);
    }
    verifying
        .verify(&AccountStatement::signing_input(&prefix), &sig)
        .map_err(|_| AccountStatementError::BadSignature)
}

/// Verify an account statement: layout, `expected_kind`,
/// `expected_account_id`, the time window around `now_unix_ms`, the signature
/// under `verifying`, and that it covers `expected_body` (the kind's
/// canonical body, rebuilt by the verifier from the request). Returns the
/// claims on success.
///
/// # Errors
///
/// One [`AccountStatementError`] per failed check, in the order above. A
/// service answers all of them with one status so the caller learns nothing
/// about which check failed.
pub fn verify_account_statement(
    bytes: &[u8],
    verifying: &VerifyingKey,
    expected_kind: AccountStatementKind,
    expected_account_id: &[u8; ACCOUNT_ID_LEN],
    expected_body: &[u8],
    now_unix_ms: i64,
) -> Result<AccountStatement, AccountStatementError> {
    let claims = AccountStatement::decode_unverified(bytes)?;
    if claims.kind != expected_kind {
        return Err(AccountStatementError::KindMismatch);
    }
    if &claims.account_id != expected_account_id {
        return Err(AccountStatementError::AccountMismatch);
    }
    if !claims.within_window(now_unix_ms) {
        return Err(AccountStatementError::OutsideWindow);
    }
    verify_signature(bytes, verifying)?;
    if claims.body_hash != hash::blake3(expected_body) {
        return Err(AccountStatementError::BodyMismatch);
    }
    Ok(claims)
}

/// `true` when the statement is refused by [`verify_account_statement`] for
/// its TIME ALONE: every other check (layout, kind, account, signature under
/// `verifying`, body) passes and only `unix_ms` lies outside the window.
///
/// A service uses this to tell a device whose clock is wrong from a caller
/// who does not hold the key (ADR-0074 "T5 as built"): to the first it may
/// answer its own time so the device signs again, and that answer goes only
/// to a caller whose signature verified, so a stranger learns nothing from
/// it. A statement that is inside the window, or fails any other check,
/// yields `false`.
#[must_use]
pub fn account_statement_is_stale(
    bytes: &[u8],
    verifying: &VerifyingKey,
    expected_kind: AccountStatementKind,
    expected_account_id: &[u8; ACCOUNT_ID_LEN],
    expected_body: &[u8],
    now_unix_ms: i64,
) -> bool {
    let Ok(claims) = AccountStatement::decode_unverified(bytes) else {
        return false;
    };
    claims.kind == expected_kind
        && &claims.account_id == expected_account_id
        && !claims.within_window(now_unix_ms)
        && verify_signature(bytes, verifying).is_ok()
        && claims.body_hash == hash::blake3(expected_body)
}

/// Canonical body of a [`AccountStatementKind::DeviceRevoke`] statement: the
/// device id itself.
#[must_use]
pub const fn device_revoke_body(device_id: &[u8; DEVICE_ID_LEN]) -> [u8; DEVICE_ID_LEN] {
    *device_id
}

/// Canonical body of a [`AccountStatementKind::SessionUpgrade`] statement:
/// `BLAKE3(refresh_token)` over the credential's ASCII bytes.
#[must_use]
pub fn session_upgrade_body(refresh_token: &str) -> [u8; 32] {
    hash::blake3(refresh_token.as_bytes())
}

/// Canonical body of a [`AccountStatementKind::Recall`] statement:
/// `server_nonce(32) ‖ new_signature_pubkey(32) ‖ BLAKE3(password_upload)
/// ‖ BLAKE3(recovery_upload)`, the uploads as the raw OPAQUE bytes.
#[must_use]
pub fn recall_body(
    server_nonce: &[u8; 32],
    new_signature_pubkey: &[u8; PUBLIC_KEY_LEN],
    password_upload: &[u8],
    recovery_upload: &[u8],
) -> [u8; RECALL_BODY_LEN] {
    let mut out = [0u8; RECALL_BODY_LEN];
    out[..32].copy_from_slice(server_nonce);
    out[32..64].copy_from_slice(new_signature_pubkey);
    out[64..96].copy_from_slice(&hash::blake3(password_upload));
    out[96..].copy_from_slice(&hash::blake3(recovery_upload));
    out
}

/// Length of the canonical body of a [`AccountStatementKind::RecallStart`]
/// statement.
pub const RECALL_START_BODY_LEN: usize = 8 + 32;

/// Canonical body of a [`AccountStatementKind::RecallStart`] statement:
/// `anchor_seq(8, big-endian u64) ‖ BLAKE3(login_request)`, the login request
/// as the raw OPAQUE `CredentialRequest` bytes. Binding the request means a
/// captured statement can only re-ask the server about a blinded element
/// whose blind the capturer does not hold.
#[must_use]
pub fn recall_start_body(anchor_seq: u64, login_request: &[u8]) -> [u8; RECALL_START_BODY_LEN] {
    let mut out = [0u8; RECALL_START_BODY_LEN];
    out[..8].copy_from_slice(&anchor_seq.to_be_bytes());
    out[8..].copy_from_slice(&hash::blake3(login_request));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ActionAttestation, AttestError, BackupHead, BackupHeadError, GenesisAttestError,
        GenesisAttestation, KeyResetAttestError, KeyResetAttestation, ReleaseManifest,
        ReleaseManifestError, UsernameCertError, UsernameCertificate,
    };

    const ACCOUNT: [u8; ACCOUNT_ID_LEN] = [0xA1; ACCOUNT_ID_LEN];
    const DEVICE: [u8; DEVICE_ID_LEN] = [0xD1; DEVICE_ID_LEN];
    const NONCE: [u8; ACCOUNT_STATEMENT_NONCE_LEN] = [0x4E; ACCOUNT_STATEMENT_NONCE_LEN];
    const NOW: i64 = 1_800_000_000_000;

    fn key() -> SigningKey {
        SigningKey::from_seed(&[0x33; 32])
    }

    fn revoke() -> AccountStatement {
        AccountStatement::new(
            AccountStatementKind::DeviceRevoke,
            ACCOUNT,
            NOW,
            NONCE,
            &device_revoke_body(&DEVICE),
        )
    }

    fn verify_revoke(wire: &[u8], now: i64) -> Result<AccountStatement, AccountStatementError> {
        verify_account_statement(
            wire,
            &key().verifying_key(),
            AccountStatementKind::DeviceRevoke,
            &ACCOUNT,
            &DEVICE,
            now,
        )
    }

    #[test]
    fn stale_is_true_only_for_a_good_statement_outside_the_window() {
        let wire = revoke().sign(&key());
        let vk = key().verifying_key();
        let stale = |wire: &[u8], vk: &VerifyingKey, body: &[u8], now: i64| {
            account_statement_is_stale(
                wire,
                vk,
                AccountStatementKind::DeviceRevoke,
                &ACCOUNT,
                body,
                now,
            )
        };
        let late = NOW + ACCOUNT_STATEMENT_WINDOW_MS + 1;
        let early = NOW - ACCOUNT_STATEMENT_WINDOW_MS - 1;
        // Inside the window it is simply valid, not stale.
        assert!(!stale(&wire, &vk, &DEVICE, NOW));
        assert!(!stale(
            &wire,
            &vk,
            &DEVICE,
            NOW + ACCOUNT_STATEMENT_WINDOW_MS
        ));
        // Outside it, either side, with everything else right: stale.
        assert!(stale(&wire, &vk, &DEVICE, late));
        assert!(stale(&wire, &vk, &DEVICE, early));
        // Outside it and wrong in any other way: not stale, just refused.
        let other = SigningKey::from_seed(&[0x34; 32]).verifying_key();
        assert!(!stale(&wire, &other, &DEVICE, late));
        assert!(!stale(&wire, &vk, &[0xD2; DEVICE_ID_LEN], late));
        let mut tampered = wire;
        tampered[20] ^= 1;
        assert!(!stale(&tampered, &vk, &DEVICE, late));
        assert!(!account_statement_is_stale(
            &wire,
            &vk,
            AccountStatementKind::SessionUpgrade,
            &ACCOUNT,
            &DEVICE,
            late
        ));
        assert!(!account_statement_is_stale(
            &wire,
            &vk,
            AccountStatementKind::DeviceRevoke,
            &[0xA2; ACCOUNT_ID_LEN],
            &DEVICE,
            late
        ));
        assert!(!stale(&wire[..100], &vk, &DEVICE, late));
    }

    #[test]
    fn the_wire_form_is_138_bytes_in_the_documented_layout() {
        assert_eq!(ACCOUNT_STATEMENT_LEN, 138);
        assert_eq!(SIGNED_PREFIX_LEN, 74);
        let wire = revoke().sign(&key());
        assert_eq!(wire[0], 1);
        assert_eq!(wire[1], 3);
        assert_eq!(&wire[2..18], &ACCOUNT);
        assert_eq!(&wire[18..26], &NOW.to_be_bytes());
        assert_eq!(&wire[26..42], &NONCE);
        assert_eq!(&wire[42..74], &lakko_crypto::hash::blake3(&DEVICE));
        // The signature covers the domain, one zero byte and the prefix.
        let mut signed = b"lakko/account-statement/v1".to_vec();
        signed.push(0);
        signed.extend_from_slice(&wire[..74]);
        let sig = Signature::from_bytes(&wire[74..]).unwrap();
        key().verifying_key().verify(&signed, &sig).unwrap();
    }

    #[test]
    fn sign_then_verify_round_trips_every_kind() {
        let sk = key();
        let vk = sk.verifying_key();
        let recall = recall_body(&[1; 32], &[2; 32], b"password upload", b"recovery upload");
        let upgrade = session_upgrade_body("r2.aaa");
        let start = recall_start_body(7, b"login request");
        assert_eq!(&start[..8], &7u64.to_be_bytes());
        assert_eq!(&start[8..], &lakko_crypto::hash::blake3(b"login request"));
        let cases: [(AccountStatementKind, &[u8]); 5] = [
            (AccountStatementKind::RecallStatus, &[]),
            (AccountStatementKind::Recall, &recall),
            (AccountStatementKind::DeviceRevoke, &DEVICE),
            (AccountStatementKind::SessionUpgrade, &upgrade),
            (AccountStatementKind::RecallStart, &start),
        ];
        for (kind, body) in cases {
            let claims = AccountStatement::new(kind, ACCOUNT, NOW, NONCE, body);
            let wire = claims.sign(&sk);
            let got = verify_account_statement(&wire, &vk, kind, &ACCOUNT, body, NOW).unwrap();
            assert_eq!(got, claims);
            assert_eq!(AccountStatement::open(&wire, &vk).unwrap(), claims);
            assert_eq!(AccountStatement::decode_unverified(&wire).unwrap(), claims);
            // Each kind is refused where another is expected.
            for (other, _) in cases {
                if other != kind {
                    assert_eq!(
                        verify_account_statement(&wire, &vk, other, &ACCOUNT, body, NOW),
                        Err(AccountStatementError::KindMismatch)
                    );
                }
            }
        }
    }

    #[test]
    fn kind_bytes_are_frozen() {
        assert_eq!(AccountStatementKind::RecallStatus.to_byte(), 1);
        assert_eq!(AccountStatementKind::Recall.to_byte(), 2);
        assert_eq!(AccountStatementKind::DeviceRevoke.to_byte(), 3);
        assert_eq!(AccountStatementKind::SessionUpgrade.to_byte(), 4);
        assert_eq!(AccountStatementKind::RecallStart.to_byte(), 5);
        for byte in 0..=u8::MAX {
            match AccountStatementKind::from_byte(byte) {
                Some(kind) => assert_eq!(kind.to_byte(), byte),
                None => assert!(!(1..=4).contains(&byte)),
            }
        }
    }

    #[test]
    fn layout_refusals() {
        let vk = key().verifying_key();
        let wire = revoke().sign(&key());
        for len in [0, 1, 73, 74, 137, 139, 145] {
            let bytes = vec![1u8; len];
            assert_eq!(
                AccountStatement::decode_unverified(&bytes),
                Err(AccountStatementError::BadLength { got: len })
            );
            assert_eq!(
                verify_revoke(&bytes, NOW),
                Err(AccountStatementError::BadLength { got: len })
            );
        }
        let mut bad_version = wire;
        bad_version[0] = 2;
        assert_eq!(
            AccountStatement::open(&bad_version, &vk),
            Err(AccountStatementError::BadVersion(2))
        );
        for kind in [0u8, 6, 0xFF] {
            let mut bad_kind = wire;
            bad_kind[1] = kind;
            assert_eq!(
                verify_revoke(&bad_kind, NOW),
                Err(AccountStatementError::UnknownKind(kind))
            );
        }
    }

    #[test]
    fn every_signed_byte_is_covered() {
        let wire = revoke().sign(&key());
        let vk = key().verifying_key();
        // Version and kind are caught by the layout gate; every later byte,
        // the signature included, by the signature.
        for index in 2..ACCOUNT_STATEMENT_LEN {
            let mut tampered = wire;
            tampered[index] ^= 0x01;
            assert_eq!(
                AccountStatement::open(&tampered, &vk),
                Err(AccountStatementError::BadSignature),
                "byte {index}"
            );
        }
    }

    #[test]
    fn wrong_key_account_body_and_time_are_refused() {
        let sk = key();
        let wire = revoke().sign(&sk);

        // Another key (the remote-wipe case: a statement by a key that is not
        // the account's).
        let other = SigningKey::from_seed(&[0x44; 32]).verifying_key();
        assert_eq!(
            verify_account_statement(
                &wire,
                &other,
                AccountStatementKind::DeviceRevoke,
                &ACCOUNT,
                &DEVICE,
                NOW
            ),
            Err(AccountStatementError::BadSignature)
        );

        // Another account.
        assert_eq!(
            verify_account_statement(
                &wire,
                &sk.verifying_key(),
                AccountStatementKind::DeviceRevoke,
                &[0xA2; ACCOUNT_ID_LEN],
                &DEVICE,
                NOW
            ),
            Err(AccountStatementError::AccountMismatch)
        );

        // Another device than the one signed for.
        assert_eq!(
            verify_account_statement(
                &wire,
                &sk.verifying_key(),
                AccountStatementKind::DeviceRevoke,
                &ACCOUNT,
                &[0xD2; DEVICE_ID_LEN],
                NOW
            ),
            Err(AccountStatementError::BodyMismatch)
        );

        // The window, both edges inclusive, both sides.
        assert!(verify_revoke(&wire, NOW + ACCOUNT_STATEMENT_WINDOW_MS).is_ok());
        assert!(verify_revoke(&wire, NOW - ACCOUNT_STATEMENT_WINDOW_MS).is_ok());
        assert_eq!(
            verify_revoke(&wire, NOW + ACCOUNT_STATEMENT_WINDOW_MS + 1),
            Err(AccountStatementError::OutsideWindow)
        );
        assert_eq!(
            verify_revoke(&wire, NOW - ACCOUNT_STATEMENT_WINDOW_MS - 1),
            Err(AccountStatementError::OutsideWindow)
        );
        // Extreme clocks saturate instead of overflowing.
        assert_eq!(
            verify_revoke(&wire, i64::MAX),
            Err(AccountStatementError::OutsideWindow)
        );
        assert_eq!(
            verify_revoke(&wire, i64::MIN),
            Err(AccountStatementError::OutsideWindow)
        );
    }

    #[test]
    fn a_small_order_key_verifies_nothing() {
        // The identity point: a signature of all-zero `R` and `S` verifies
        // under it for any message with a permissive verifier.
        let mut identity = [0u8; 32];
        identity[0] = 1;
        let weak = VerifyingKey::from_bytes(&identity).unwrap();
        let mut wire = revoke().sign(&key());
        wire[SIGNED_PREFIX_LEN..].fill(0);
        wire[SIGNED_PREFIX_LEN] = 1;
        assert_eq!(
            AccountStatement::open(&wire, &weak),
            Err(AccountStatementError::BadSignature)
        );
    }

    #[test]
    fn canonical_bodies_are_the_documented_bytes() {
        assert_eq!(device_revoke_body(&DEVICE), DEVICE);
        assert_eq!(
            session_upgrade_body("r2.token"),
            lakko_crypto::hash::blake3(b"r2.token")
        );
        let body = recall_body(&[1; 32], &[2; 32], b"pw", b"rec");
        assert_eq!(body.len(), 128);
        assert_eq!(&body[..32], &[1; 32]);
        assert_eq!(&body[32..64], &[2; 32]);
        assert_eq!(&body[64..96], &lakko_crypto::hash::blake3(b"pw"));
        assert_eq!(&body[96..], &lakko_crypto::hash::blake3(b"rec"));
    }

    #[test]
    fn golden_pins_the_signed_bytes() {
        // A drift in the domain, the separator byte or the field order
        // changes this signature and fails here, not at a service.
        let wire = revoke().sign(&key());
        let again = revoke().sign(&key());
        assert_eq!(wire, again, "Ed25519 signing is deterministic");
        assert_eq!(wire[74..82], GOLDEN_SIG_PREFIX);
    }

    /// The first eight signature bytes of [`revoke`] under [`key`].
    const GOLDEN_SIG_PREFIX: [u8; 8] = [0xa8, 0xa2, 0x77, 0x92, 0x39, 0x66, 0x18, 0xe8];

    #[test]
    fn domains_do_not_cross_between_a_statement_and_every_attestation_kind() {
        // The statement is signed by an ACCOUNT key, the attestations by a
        // service key, so in deployment the keys differ too. The domain must
        // hold even if one key signed all of them: (a) the length (138) is
        // shared with no other kind, so each parser refuses the other's
        // bytes; (b) a signature spliced across kinds never verifies.
        let sk = key();
        let vk = sk.verifying_key();

        let statement = revoke().sign(&sk);
        let action = ActionAttestation::new([0xAA; 16], [0xBB; 32], 1_000).sign(&sk);
        let genesis = GenesisAttestation::new([0xAA; 16], [0xBB; 32], [0xCC; 32], 1_000).sign(&sk);
        let backup = BackupHead::new([0xAA; 16], 7, [0xBB; 32], 1_000_000).sign(&sk);
        let release = ReleaseManifest::new(10, 5, 1_000_000).sign(&sk);
        let cert = UsernameCertificate::new([0xDD; 64], [0xAA; 16], 1_000_000).sign(&sk);
        let reset = KeyResetAttestation::for_account(&[0xAA; 16], 2, [0xCC; 32], 1_000).sign(&sk);

        let others: [(&str, &[u8]); 6] = [
            ("action", &action),
            ("genesis", &genesis),
            ("backup-head", &backup),
            ("release-manifest", &release),
            ("username-cert", &cert),
            ("key-reset", &reset),
        ];

        // (a) Length, both ways.
        for (name, other) in others {
            assert_ne!(other.len(), ACCOUNT_STATEMENT_LEN, "{name}");
            assert!(
                matches!(
                    AccountStatement::open(other, &vk),
                    Err(AccountStatementError::BadLength { .. })
                ),
                "a {name} blob must not parse as an account statement"
            );
        }
        assert!(matches!(
            ActionAttestation::open(&statement, &vk),
            Err(AttestError::BadLength { .. })
        ));
        assert!(matches!(
            GenesisAttestation::open(&statement, &vk),
            Err(GenesisAttestError::BadLength { .. })
        ));
        assert!(matches!(
            BackupHead::open(&statement, &vk),
            Err(BackupHeadError::BadLength { .. })
        ));
        assert!(matches!(
            ReleaseManifest::open(&statement, &vk),
            Err(ReleaseManifestError::BadLength { .. })
        ));
        assert!(matches!(
            UsernameCertificate::open(&statement, &vk),
            Err(UsernameCertError::BadLength { .. })
        ));
        assert!(matches!(
            KeyResetAttestation::open(&statement, &vk),
            Err(KeyResetAttestError::BadLength { .. })
        ));

        // (b) Domain: a foreign signature in a statement, and the statement's
        // signature in every foreign blob.
        for (name, foreign) in others {
            let mut spliced = statement;
            spliced[SIGNED_PREFIX_LEN..].copy_from_slice(&foreign[foreign.len() - SIGNATURE_LEN..]);
            assert_eq!(
                AccountStatement::open(&spliced, &vk),
                Err(AccountStatementError::BadSignature),
                "a {name}-domain signature must not verify as an account statement"
            );
        }
        let splice = |wire: &[u8]| {
            let mut out = wire.to_vec();
            let len = out.len();
            out[len - SIGNATURE_LEN..].copy_from_slice(&statement[SIGNED_PREFIX_LEN..]);
            out
        };
        assert_eq!(
            ActionAttestation::open(&splice(&action), &vk),
            Err(AttestError::BadSignature)
        );
        assert_eq!(
            GenesisAttestation::open(&splice(&genesis), &vk),
            Err(GenesisAttestError::BadSignature)
        );
        assert_eq!(
            BackupHead::open(&splice(&backup), &vk),
            Err(BackupHeadError::BadSignature)
        );
        assert_eq!(
            ReleaseManifest::open(&splice(&release), &vk),
            Err(ReleaseManifestError::BadSignature)
        );
        assert_eq!(
            UsernameCertificate::open(&splice(&cert), &vk),
            Err(UsernameCertError::BadSignature)
        );
        assert_eq!(
            KeyResetAttestation::open(&splice(&reset), &vk),
            Err(KeyResetAttestError::BadSignature)
        );

        // (c) The same claims re-signed under each foreign domain's framing
        // cannot be built from this module, so the remaining crossing is one
        // statement kind standing in for another: covered by the kind byte
        // being signed.
        let mut relabelled = statement;
        relabelled[1] = AccountStatementKind::SessionUpgrade.to_byte();
        assert_eq!(
            AccountStatement::open(&relabelled, &vk),
            Err(AccountStatementError::BadSignature)
        );
    }
}
