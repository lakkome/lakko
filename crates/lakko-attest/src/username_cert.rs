// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Username certificate ([ADR-0058] section 5).
//!
//! A *username certificate* is a compact, domain-separated, Ed25519-signed
//! statement by `services/discovery` that the OPRF output of a username was
//! registered to an account. Discovery signs it once, at username registration,
//! with a **dedicated** key (published at a well-known path and pinned in the
//! clients, the [ADR-0015] pattern). A viewer who is shown a claimed username
//! verifies it by evaluating the OPRF over the claimed username (one
//! evaluation, [ADR-0054] token-rationed) and checking this certificate against
//! the claimed account — **with no lookup**. That keeps [ADR-0048] username
//! verification working for people who do not answer lookups, and it means no
//! endpoint ever confirms a guessed username for a known account id.
//!
//! It is the fifth kind in this crate's fixed-layout attestation family:
//!
//! - **Discovery-signed**, not auth-signed and not publisher-signed. The key is
//!   its own; a certificate is only ever client-verified.
//! - **Permanent.** Usernames are immutable in Lakko, so a certificate carries
//!   an `issued_at` timestamp (informational) and no expiry, and
//!   [`verify_username_binding`] applies **no freshness check**.
//! - **Binds the OPRF output, never the username.** The plaintext username never
//!   reaches the server (invariant #1), so the certificate carries the 64-byte
//!   OPRF output the discovery table is keyed on.
//!
//! The format deliberately mirrors [`GenesisAttestation`](crate::GenesisAttestation):
//! a small fixed-layout binary blob (not a JWT), so the verifier stays trivial to
//! read, bounded, and fuzzable, and pulls no JWS / JWKS-fetch dependency into the
//! core. The trusted verifying key is supplied by the caller.
//!
//! ```text
//! wire           = version(1) ‖ oprf_hash(64) ‖ account_id(16)
//!                  ‖ issued_at_unix_millis(8, big-endian i64) ‖ Ed25519_sig(64)
//!                                                                  = 153 bytes
//! signing input  = DOMAIN ‖ version ‖ oprf_hash ‖ account_id ‖ issued_at
//! DOMAIN         = b"lakko/username-cert/v1"   (distinct from device-attest/v1,
//!                                               genesis-attest/v1, backup-head/v1
//!                                               and release-manifest/v1)
//! ```
//!
//! **Same length as a genesis attestation.** [`USERNAME_CERT_LEN`] and
//! [`GENESIS_ATTESTATION_LEN`](crate::GENESIS_ATTESTATION_LEN) are both 153
//! bytes, and both carry version byte `1`, so length alone does not tell the two
//! apart: a genesis blob parses as a username-certificate layout and vice versa.
//! What keeps them non-interchangeable is the domain-prefixed signing input
//! alone, which the five-way domain-crossing test in this module pins
//! explicitly (a whole genesis blob opened as a certificate, and the reverse,
//! each fail on the signature).
//!
//! **The profile carries it opaquely.** The ADR-0048 profile envelope (v3,
//! `lakko-contacts`) carries a certificate as opaque bytes and declares its own
//! length constant with the same value (153), because `lakko-contacts` does not
//! depend on this crate. A pin test asserting the two constants agree is to be
//! added where both crates are visible.
//!
//! **Units: Unix milliseconds** for `issued_at`, matching
//! [`BackupHead`](crate::BackupHead) and [`ReleaseManifest`](crate::ReleaseManifest).
//!
//! [ADR-0058]: ../../../docs/adr/0058-scoped-discovery.md
//! [ADR-0015]: ../../../docs/adr/0015-device-removal-authorization.md
//! [ADR-0048]: ../../../docs/adr/0048-profile-names.md
//! [ADR-0054]: ../../../docs/adr/0054-anti-abuse-without-client-work.md

use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, SIGNATURE_LEN};
use thiserror::Error;

/// Domain separator mixed into the signing input. Distinct from the four other
/// kinds' domains (`lakko/device-attest/v1`, `lakko/genesis-attest/v1`,
/// `lakko/backup-head/v1`, `lakko/release-manifest/v1`), so all five attestation
/// kinds are cryptographically non-interchangeable even under a shared signing
/// key. Load-bearing here in particular, since a username certificate and a
/// genesis attestation have the same total length. Bumping this string (and
/// [`VERSION`]) invalidates every previously-issued certificate.
const DOMAIN: &[u8] = b"lakko/username-cert/v1";

/// Wire-format version. The verifier refuses any other value.
const VERSION: u8 = 1;

/// Length of the OPRF output the certificate binds (the Ristretto255-SHA512
/// VOPRF output discovery keys its username table on).
pub const OPRF_HASH_LEN: usize = 64;

/// Length of the account id (raw UUIDv7 bytes).
const ACCOUNT_LEN: usize = 16;

/// Length of the big-endian `i64` `issued_at` timestamp.
const ISSUED_AT_LEN: usize = 8;

/// Total serialized username-certificate length:
/// `version + oprf_hash + account_id + issued_at + signature`.
///
/// The `lakko-contacts` profile envelope (ADR-0048 v3) carries a certificate
/// opaquely and declares its own constant of the same value; the two must stay
/// equal.
pub const USERNAME_CERT_LEN: usize =
    1 + OPRF_HASH_LEN + ACCOUNT_LEN + ISSUED_AT_LEN + SIGNATURE_LEN;

// Pin the wire length to the ADR-0058 layout so a field-width change cannot
// silently drift the format.
const _: () = assert!(USERNAME_CERT_LEN == 153);

/// Why a username certificate was rejected. Returned by
/// [`UsernameCertificate::open`] and [`verify_username_binding`]. There is no
/// `Expired` / `Stale` variant: a certificate is permanent.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum UsernameCertError {
    /// The byte slice is not exactly [`USERNAME_CERT_LEN`] long.
    #[error("username certificate has wrong length: {got} bytes")]
    BadLength {
        /// The actual length received.
        got: usize,
    },
    /// The version byte is not [`VERSION`].
    #[error("unsupported username certificate version: {0}")]
    BadVersion(u8),
    /// The Ed25519 signature did not verify (tampered field, wrong key, a blob
    /// of another attestation kind, or malformed signature bytes).
    #[error("username certificate signature is invalid")]
    BadSignature,
    /// The certified OPRF output does not match the OPRF of the claimed
    /// username.
    #[error("username certificate does not match the claimed username")]
    UsernameMismatch,
    /// The certified account id does not match the claimed account.
    #[error("username certificate account id does not match the claimed account")]
    AccountMismatch,
}

/// The authenticated claims carried by a username certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsernameCertificate {
    oprf_hash: [u8; OPRF_HASH_LEN],
    account_id: [u8; ACCOUNT_LEN],
    issued_at_unix_millis: i64,
}

impl UsernameCertificate {
    /// Construct claims to be signed.
    #[must_use]
    pub const fn new(
        oprf_hash: [u8; OPRF_HASH_LEN],
        account_id: [u8; ACCOUNT_LEN],
        issued_at_unix_millis: i64,
    ) -> Self {
        Self {
            oprf_hash,
            account_id,
            issued_at_unix_millis,
        }
    }

    /// The certified OPRF output of the username.
    #[must_use]
    pub const fn oprf_hash(&self) -> &[u8; OPRF_HASH_LEN] {
        &self.oprf_hash
    }

    /// The account the username is registered to (raw 16-byte id).
    #[must_use]
    pub const fn account_id(&self) -> &[u8; ACCOUNT_LEN] {
        &self.account_id
    }

    /// When discovery issued the certificate, Unix milliseconds.
    /// Informational only: nothing in this crate enforces it.
    #[must_use]
    pub const fn issued_at_unix_millis(&self) -> i64 {
        self.issued_at_unix_millis
    }

    /// The exact bytes the signature covers (domain-separated).
    fn signing_input(&self) -> Vec<u8> {
        let mut buf =
            Vec::with_capacity(DOMAIN.len() + 1 + OPRF_HASH_LEN + ACCOUNT_LEN + ISSUED_AT_LEN);
        buf.extend_from_slice(DOMAIN);
        buf.push(VERSION);
        buf.extend_from_slice(&self.oprf_hash);
        buf.extend_from_slice(&self.account_id);
        buf.extend_from_slice(&self.issued_at_unix_millis.to_be_bytes());
        buf
    }

    /// Serialize + sign these claims into the [`USERNAME_CERT_LEN`]-byte wire
    /// form. Called by `services/discovery` at username registration.
    #[must_use]
    pub fn sign(&self, signing: &SigningKey) -> [u8; USERNAME_CERT_LEN] {
        let sig = signing.sign(&self.signing_input());
        let mut out = [0u8; USERNAME_CERT_LEN];
        out[0] = VERSION;
        let mut off = 1;
        out[off..off + OPRF_HASH_LEN].copy_from_slice(&self.oprf_hash);
        off += OPRF_HASH_LEN;
        out[off..off + ACCOUNT_LEN].copy_from_slice(&self.account_id);
        off += ACCOUNT_LEN;
        out[off..off + ISSUED_AT_LEN].copy_from_slice(&self.issued_at_unix_millis.to_be_bytes());
        off += ISSUED_AT_LEN;
        out[off..off + SIGNATURE_LEN].copy_from_slice(&sig.to_bytes());
        out
    }

    /// Parse the wire form and verify the Ed25519 signature against `verifying`.
    /// Returns the authenticated claims.
    ///
    /// This does **not** check the username / account binding — callers should
    /// use [`verify_username_binding`].
    ///
    /// # Errors
    ///
    /// [`UsernameCertError::BadLength`] / [`UsernameCertError::BadVersion`] on a
    /// malformed envelope; [`UsernameCertError::BadSignature`] if the signature
    /// is malformed or does not verify.
    pub fn open(bytes: &[u8], verifying: &VerifyingKey) -> Result<Self, UsernameCertError> {
        if bytes.len() != USERNAME_CERT_LEN {
            return Err(UsernameCertError::BadLength { got: bytes.len() });
        }
        if bytes[0] != VERSION {
            return Err(UsernameCertError::BadVersion(bytes[0]));
        }
        let mut off = 1;
        let mut oprf_hash = [0u8; OPRF_HASH_LEN];
        oprf_hash.copy_from_slice(&bytes[off..off + OPRF_HASH_LEN]);
        off += OPRF_HASH_LEN;
        let mut account_id = [0u8; ACCOUNT_LEN];
        account_id.copy_from_slice(&bytes[off..off + ACCOUNT_LEN]);
        off += ACCOUNT_LEN;
        let mut issued = [0u8; ISSUED_AT_LEN];
        issued.copy_from_slice(&bytes[off..off + ISSUED_AT_LEN]);
        off += ISSUED_AT_LEN;
        let issued_at_unix_millis = i64::from_be_bytes(issued);

        let sig = Signature::from_bytes(&bytes[off..off + SIGNATURE_LEN])
            .map_err(|_| UsernameCertError::BadSignature)?;
        let claims = Self {
            oprf_hash,
            account_id,
            issued_at_unix_millis,
        };
        verifying
            .verify(&claims.signing_input(), &sig)
            .map_err(|_| UsernameCertError::BadSignature)?;
        Ok(claims)
    }
}

/// Verify a username certificate's signature and its binding to the claimed
/// username (as the OPRF output the viewer computed over it) and the claimed
/// account, with **no freshness check** — usernames are immutable, so a
/// certificate is a permanent artifact.
///
/// The binding fields are compared with plain equality: both are public data
/// (the certificate travels in the profile, the OPRF output is what anyone with
/// a token can compute for a guessed username), so a timing difference reveals
/// nothing the caller does not already hold.
///
/// # Errors
///
/// Propagates [`UsernameCertificate::open`] errors, then
/// [`UsernameCertError::UsernameMismatch`] /
/// [`UsernameCertError::AccountMismatch`] if the (now-authenticated) claims do
/// not match what the caller expects.
pub fn verify_username_binding(
    bytes: &[u8],
    verifying: &VerifyingKey,
    expected_oprf_hash: &[u8; OPRF_HASH_LEN],
    expected_account_id: &[u8; ACCOUNT_LEN],
) -> Result<UsernameCertificate, UsernameCertError> {
    let cert = UsernameCertificate::open(bytes, verifying)?;
    if &cert.oprf_hash != expected_oprf_hash {
        return Err(UsernameCertError::UsernameMismatch);
    }
    if &cert.account_id != expected_account_id {
        return Err(UsernameCertError::AccountMismatch);
    }
    Ok(cert)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ActionAttestation, AttestError, BackupHead, BackupHeadError, GenesisAttestError,
        GenesisAttestation, ReleaseManifest, ReleaseManifestError, GENESIS_ATTESTATION_LEN,
    };

    fn key() -> SigningKey {
        SigningKey::from_seed(&[0x11; 32])
    }

    fn sample() -> UsernameCertificate {
        UsernameCertificate::new([0xAB; 64], [0xCD; 16], 1_700_000_000_000)
    }

    #[test]
    fn sign_then_verify_binding_roundtrips() {
        let sk = key();
        let cert = sample();
        let wire = cert.sign(&sk);
        assert_eq!(wire.len(), USERNAME_CERT_LEN);
        assert_eq!(USERNAME_CERT_LEN, 153);
        let got = verify_username_binding(&wire, &sk.verifying_key(), &[0xAB; 64], &[0xCD; 16])
            .expect("valid certificate verifies");
        assert_eq!(got, cert);
        assert_eq!(got.oprf_hash(), &[0xAB; 64]);
        assert_eq!(got.account_id(), &[0xCD; 16]);
        assert_eq!(got.issued_at_unix_millis(), 1_700_000_000_000);
    }

    #[test]
    fn wire_layout_is_pinned() {
        let wire = sample().sign(&key());
        assert_eq!(wire[0], 1, "version byte");
        assert_eq!(&wire[1..65], &[0xAB; 64], "oprf_hash");
        assert_eq!(&wire[65..81], &[0xCD; 16], "account_id");
        assert_eq!(
            &wire[81..89],
            &1_700_000_000_000_i64.to_be_bytes(),
            "issued_at, big-endian"
        );
    }

    #[test]
    fn tampering_any_field_breaks_the_signature() {
        let sk = key();
        let wire = sample().sign(&sk);
        let vk = sk.verifying_key();
        // One byte in each region: oprf_hash, account_id, issued_at, sig.
        for idx in [1usize, 64, 70, 85, USERNAME_CERT_LEN - 1] {
            let mut bad = wire;
            bad[idx] ^= 0x01;
            assert_eq!(
                UsernameCertificate::open(&bad, &vk),
                Err(UsernameCertError::BadSignature),
                "tampering byte {idx} must invalidate the signature"
            );
        }
    }

    #[test]
    fn wrong_key_is_rejected() {
        let wire = sample().sign(&key());
        let other = SigningKey::from_seed(&[0x22; 32]).verifying_key();
        assert_eq!(
            UsernameCertificate::open(&wire, &other),
            Err(UsernameCertError::BadSignature)
        );
    }

    #[test]
    fn username_and_account_binding_is_enforced() {
        let sk = key();
        let wire = sample().sign(&sk);
        let vk = sk.verifying_key();
        assert_eq!(
            verify_username_binding(&wire, &vk, &[0x00; 64], &[0xCD; 16]),
            Err(UsernameCertError::UsernameMismatch)
        );
        assert_eq!(
            verify_username_binding(&wire, &vk, &[0xAB; 64], &[0x00; 16]),
            Err(UsernameCertError::AccountMismatch)
        );
    }

    #[test]
    fn issued_at_is_never_a_freshness_gate() {
        // A certificate issued at the epoch, or at a nonsensical far-future time,
        // still verifies: the binding check has no clock at all.
        let sk = key();
        let vk = sk.verifying_key();
        for issued in [0, i64::MIN, i64::MAX] {
            let wire = UsernameCertificate::new([0xAB; 64], [0xCD; 16], issued).sign(&sk);
            assert!(verify_username_binding(&wire, &vk, &[0xAB; 64], &[0xCD; 16]).is_ok());
        }
    }

    #[test]
    fn malformed_envelope_is_rejected() {
        let sk = key();
        let vk = sk.verifying_key();
        assert_eq!(
            UsernameCertificate::open(&[], &vk),
            Err(UsernameCertError::BadLength { got: 0 })
        );
        assert_eq!(
            UsernameCertificate::open(&[0u8; USERNAME_CERT_LEN - 1], &vk),
            Err(UsernameCertError::BadLength {
                got: USERNAME_CERT_LEN - 1
            })
        );
        assert_eq!(
            UsernameCertificate::open(&[0u8; USERNAME_CERT_LEN + 1], &vk),
            Err(UsernameCertError::BadLength {
                got: USERNAME_CERT_LEN + 1
            })
        );
        let mut wire = sample().sign(&sk);
        wire[0] = 2;
        assert_eq!(
            UsernameCertificate::open(&wire, &vk),
            Err(UsernameCertError::BadVersion(2))
        );
    }

    #[test]
    fn domains_do_not_cross_between_all_five_attestation_kinds() {
        // Domain separation is load-bearing: even under a SHARED signing key, a
        // `UsernameCertificate` and each of the four other kinds must be
        // cryptographically non-interchangeable. For three kinds the distinct
        // total length already refuses the foreign blob; the GENESIS kind has
        // the SAME length (153) and the same version byte, so there the domain
        // prefix is the only thing keeping them apart — asserted directly.
        let sk = key();
        let vk = sk.verifying_key();

        let cert_wire = sample().sign(&sk);
        let genesis_wire =
            GenesisAttestation::new([0xAA; 16], [0xBB; 32], [0xCC; 32], 1_000).sign(&sk);
        let backup_wire = BackupHead::new([0xAA; 16], 7, [0xBB; 32], 1_000_000).sign(&sk);
        let release_wire = ReleaseManifest::new(10, 5, 1_000_000).sign(&sk);
        let action_wire = ActionAttestation::new([0xAA; 16], [0xBB; 32], 1_000).sign(&sk);

        // (a) Length, for the three kinds whose length differs.
        for other in [&backup_wire[..], &release_wire[..], &action_wire[..]] {
            assert!(
                matches!(
                    UsernameCertificate::open(other, &vk),
                    Err(UsernameCertError::BadLength { .. })
                ),
                "a foreign blob must not parse as a username certificate"
            );
        }
        assert!(matches!(
            BackupHead::open(&cert_wire, &vk),
            Err(BackupHeadError::BadLength { .. })
        ));
        assert!(matches!(
            ReleaseManifest::open(&cert_wire, &vk),
            Err(ReleaseManifestError::BadLength { .. })
        ));
        assert!(matches!(
            ActionAttestation::open(&cert_wire, &vk),
            Err(AttestError::BadLength { .. })
        ));

        // (b) Same length as genesis: the WHOLE foreign blob parses layout-wise
        // in both directions and must fail on the signature alone.
        assert_eq!(GENESIS_ATTESTATION_LEN, USERNAME_CERT_LEN);
        assert_eq!(
            UsernameCertificate::open(&genesis_wire, &vk),
            Err(UsernameCertError::BadSignature),
            "a genesis attestation must not verify as a username certificate"
        );
        assert_eq!(
            GenesisAttestation::open(&cert_wire, &vk),
            Err(GenesisAttestError::BadSignature),
            "a username certificate must not verify as a genesis attestation"
        );

        // (c) Domain, for every other kind: splice its 64-byte signature into a
        // certificate-shaped buffer — it was produced over a different
        // domain-prefixed signing input, so it must not verify here.
        for (name, foreign) in [
            ("genesis", &genesis_wire[..]),
            ("backup-head", &backup_wire[..]),
            ("release-manifest", &release_wire[..]),
            ("action", &action_wire[..]),
        ] {
            let sig = &foreign[foreign.len() - SIGNATURE_LEN..];
            let mut spliced = sample().sign(&sk);
            spliced[USERNAME_CERT_LEN - SIGNATURE_LEN..].copy_from_slice(sig);
            assert_eq!(
                UsernameCertificate::open(&spliced, &vk),
                Err(UsernameCertError::BadSignature),
                "a {name}-domain signature must not verify as a username certificate"
            );
        }

        // And the reverse: the certificate's signature spliced into each other
        // kind's buffer must not verify under that kind's signing input.
        let cert_sig = &cert_wire[USERNAME_CERT_LEN - SIGNATURE_LEN..];
        let mut g = genesis_wire;
        let gl = g.len();
        g[gl - SIGNATURE_LEN..].copy_from_slice(cert_sig);
        assert_eq!(
            GenesisAttestation::open(&g, &vk),
            Err(GenesisAttestError::BadSignature)
        );
        let mut b = backup_wire;
        let bl = b.len();
        b[bl - SIGNATURE_LEN..].copy_from_slice(cert_sig);
        assert_eq!(
            BackupHead::open(&b, &vk),
            Err(BackupHeadError::BadSignature)
        );
        let mut r = release_wire;
        let rl = r.len();
        r[rl - SIGNATURE_LEN..].copy_from_slice(cert_sig);
        assert_eq!(
            ReleaseManifest::open(&r, &vk),
            Err(ReleaseManifestError::BadSignature)
        );
        let mut a = action_wire;
        let al = a.len();
        a[al - SIGNATURE_LEN..].copy_from_slice(cert_sig);
        assert_eq!(
            ActionAttestation::open(&a, &vk),
            Err(AttestError::BadSignature)
        );
    }
}
