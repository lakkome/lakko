// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Release manifest — signed "update available" notice ([ADR-0038]).
//!
//! A *release manifest* is a compact, domain-separated, Ed25519-signed
//! statement of the current and minimum-supported app release numbers. A client
//! fetches it, verifies the signature against a pinned publisher key, and
//! renders an in-app "an update is available" (or "you must update") notice —
//! without trusting the host it fetched the manifest from to be honest about
//! versions. It is the fourth kind in this crate's fixed-layout attestation
//! family, but the odd one out in two respects:
//!
//! - **Global, not account-bound.** [`ActionAttestation`](crate::ActionAttestation),
//!   [`GenesisAttestation`](crate::GenesisAttestation), and
//!   [`BackupHead`](crate::BackupHead) each bind an `account_id`; a release
//!   manifest is app-wide, so it binds no account — the same manifest is served
//!   to and verified by every client.
//! - **Not signed by auth.** The other three are minted by `services/auth` (the
//!   shared attest key). A release manifest is signed by a **dedicated, offline
//!   publisher release key** and only ever **client-verified** — auth is not in
//!   the loop, and neither the host serving the manifest nor the zero-trust
//!   backend can forge one.
//!
//! The format deliberately mirrors [`BackupHead`](crate::BackupHead): a small
//! fixed-layout binary blob (not a JWT), so the verifier stays trivial to read,
//! bounded, and fuzzable, and pulls no JWS / JWKS-fetch dependency into the
//! core. The trusted verifying key is supplied by the caller (the client pins
//! the publisher release key from bootstrap trust config).
//!
//! ```text
//! wire           = format_version(1) ‖ latest_release(8, big-endian u64)
//!                  ‖ min_supported_release(8, big-endian u64)
//!                  ‖ signed_at_unix_millis(8, big-endian i64)
//!                  ‖ Ed25519_sig(64)                                = 89 bytes
//! signing input  = DOMAIN ‖ format_version ‖ latest_release
//!                  ‖ min_supported_release ‖ signed_at
//! DOMAIN         = b"lakko/release-manifest/v1"   (NEW — distinct from
//!                                                  backup-head/v1, genesis-attest/v1,
//!                                                  and device-attest/v1)
//! ```
//!
//! `format_version` is the blob-layout version (`= 1`). `latest_release` is the
//! newest published release number; `min_supported_release` is the oldest
//! release the server still accepts — a client below it must update to keep
//! working (see [`release_status`]). `signed_at_unix_millis` is the mint
//! timestamp; the staleness window is a **client policy**, not a server-fixed
//! TTL, so the manifest carries the raw mint time and the client applies its own
//! window.
//!
//! **Units: Unix milliseconds.** `signed_at`, `now`, and the staleness / skew
//! windows are all milliseconds (the clients — web / iOS / Android — work in
//! milliseconds natively), matching [`BackupHead`](crate::BackupHead)'s
//! `signed_at_unix_millis`. The field and parameter names carry `_millis` so the
//! unit is unambiguous.
//!
//! **Freshness is caller-controlled and relaxable (ADR-0038).** A stale
//! manifest is a LOW-threat, suppression-only residual (an attacker who pins an
//! old-but-valid manifest can at most *hide* an update notice, never fake one or
//! downgrade the app), so [`verify_release_manifest`] lets the caller choose how
//! strict to be: passing `max_staleness_millis == i64::MAX` **disables** the
//! staleness gate entirely (the manifest can never be [`Stale`](ReleaseManifestError::Stale)).
//! Clients will pass a generous or disabled value. A small forward clock-skew
//! tolerance ([`MAX_CLOCK_SKEW_MILLIS`]) matches [`BackupHead`](crate::BackupHead).
//!
//! [ADR-0038]: ../../../docs/adr/0038-update-available-notice.md

use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, SIGNATURE_LEN};
use thiserror::Error;

/// Domain separator mixed into the signing input. Distinct from
/// [`BackupHead`](crate::BackupHead)'s `lakko/backup-head/v1`,
/// [`GenesisAttestation`](crate::GenesisAttestation)'s `lakko/genesis-attest/v1`,
/// and [`ActionAttestation`](crate::ActionAttestation)'s `lakko/device-attest/v1`,
/// so all four attestation kinds are cryptographically non-interchangeable even
/// under a shared signing key. Bumping this string (and [`FORMAT_VERSION`])
/// invalidates every previously-issued release manifest.
const DOMAIN: &[u8] = b"lakko/release-manifest/v1";

/// Wire-format (blob-layout) version. The verifier refuses any other value.
const FORMAT_VERSION: u8 = 1;

/// Length of the big-endian `u64` latest-release number.
const LATEST_RELEASE_LEN: usize = 8;

/// Length of the big-endian `u64` minimum-supported-release number.
const MIN_SUPPORTED_RELEASE_LEN: usize = 8;

/// Length of the big-endian `i64` `signed_at` mint timestamp.
const SIGNED_AT_LEN: usize = 8;

/// Forward clock-skew tolerance (milliseconds) the freshness check allows: a
/// `signed_at` up to this far in the future of `now` is still accepted, so a
/// mildly fast publisher clock relative to a verifying client does not flag a
/// genuinely-current manifest as stale. A `signed_at` beyond this future bound
/// is treated as stale (a nonsensical future timestamp is not a fresh manifest).
const MAX_CLOCK_SKEW_MILLIS: i64 = 5 * 60 * 1_000;

/// Total serialized release-manifest length:
/// `format_version + latest_release + min_supported_release + signed_at + signature`.
pub const RELEASE_MANIFEST_LEN: usize =
    1 + LATEST_RELEASE_LEN + MIN_SUPPORTED_RELEASE_LEN + SIGNED_AT_LEN + SIGNATURE_LEN;

// Pin the wire length to the ADR-0038 layout so a field-width change cannot
// silently drift the format.
const _: () = assert!(RELEASE_MANIFEST_LEN == 89);

/// Why a release manifest was rejected. Returned by [`ReleaseManifest::open`]
/// and [`verify_release_manifest`]. Unlike the account-bound attestations this
/// primitive binds neither an account nor a hash, so there are no
/// `AccountMismatch` / `EnvelopeHashMismatch` variants.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ReleaseManifestError {
    /// The byte slice is not exactly [`RELEASE_MANIFEST_LEN`] long.
    #[error("release manifest has wrong length: {got} bytes")]
    BadLength {
        /// The actual length received.
        got: usize,
    },
    /// The format-version byte is not [`FORMAT_VERSION`].
    #[error("unsupported release manifest format version: {0}")]
    BadVersion(u8),
    /// The Ed25519 signature did not verify (tampered field, wrong key, or
    /// malformed signature bytes).
    #[error("release manifest signature is invalid")]
    BadSignature,
    /// The manifest's `signed_at` falls outside the freshness window (older than
    /// `now - max_staleness`, or further in the future than the clock-skew
    /// tolerance). Never returned when the caller disables the gate with
    /// `max_staleness_millis == i64::MAX`.
    #[error("release manifest is stale (signed_at outside the freshness window)")]
    Stale,
}

/// What a client should do about its current release, given a verified manifest.
/// Returned by the pure [`release_status`] function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateStatus {
    /// The current release is at or above `latest_release` — nothing to do.
    UpToDate,
    /// A newer release exists (`current < latest`) but the current release is
    /// still supported — offer an optional update.
    UpdateAvailable,
    /// The current release is below `min_supported_release` — the client must
    /// update to keep working. Takes precedence over [`UpdateStatus::UpdateAvailable`].
    UpdateRequired,
}

/// The authenticated statement carried by a release manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseManifest {
    /// The newest published release number.
    pub latest_release: u64,
    /// The oldest release number the backend still accepts. A client below this
    /// must update; see [`release_status`].
    pub min_supported_release: u64,
    /// The mint timestamp, Unix milliseconds. The client applies its own
    /// staleness window against this (it is not an expiry).
    pub signed_at_unix_millis: i64,
}

impl ReleaseManifest {
    /// Construct a manifest to be signed.
    #[must_use]
    pub const fn new(
        latest_release: u64,
        min_supported_release: u64,
        signed_at_unix_millis: i64,
    ) -> Self {
        Self {
            latest_release,
            min_supported_release,
            signed_at_unix_millis,
        }
    }

    /// The exact bytes the signature covers (domain-separated).
    fn signing_input(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(
            DOMAIN.len() + 1 + LATEST_RELEASE_LEN + MIN_SUPPORTED_RELEASE_LEN + SIGNED_AT_LEN,
        );
        buf.extend_from_slice(DOMAIN);
        buf.push(FORMAT_VERSION);
        buf.extend_from_slice(&self.latest_release.to_be_bytes());
        buf.extend_from_slice(&self.min_supported_release.to_be_bytes());
        buf.extend_from_slice(&self.signed_at_unix_millis.to_be_bytes());
        buf
    }

    /// Serialize + sign this manifest into the [`RELEASE_MANIFEST_LEN`]-byte wire
    /// form. Called by the offline publisher release-signing tool — never by
    /// `services/auth` (this key is a dedicated publisher key, not the auth
    /// attest key).
    #[must_use]
    pub fn sign(&self, signing: &SigningKey) -> [u8; RELEASE_MANIFEST_LEN] {
        let sig = signing.sign(&self.signing_input());
        let mut out = [0u8; RELEASE_MANIFEST_LEN];
        out[0] = FORMAT_VERSION;
        let mut off = 1;
        out[off..off + LATEST_RELEASE_LEN].copy_from_slice(&self.latest_release.to_be_bytes());
        off += LATEST_RELEASE_LEN;
        out[off..off + MIN_SUPPORTED_RELEASE_LEN]
            .copy_from_slice(&self.min_supported_release.to_be_bytes());
        off += MIN_SUPPORTED_RELEASE_LEN;
        out[off..off + SIGNED_AT_LEN].copy_from_slice(&self.signed_at_unix_millis.to_be_bytes());
        off += SIGNED_AT_LEN;
        out[off..off + SIGNATURE_LEN].copy_from_slice(&sig.to_bytes());
        out
    }

    /// Parse the wire form and verify the Ed25519 signature against `verifying`.
    /// Returns the authenticated manifest.
    ///
    /// This does **not** check freshness — callers should use
    /// [`verify_release_manifest`] (the full client entry point) for that.
    ///
    /// # Errors
    ///
    /// [`ReleaseManifestError::BadLength`] / [`ReleaseManifestError::BadVersion`]
    /// on a malformed envelope; [`ReleaseManifestError::BadSignature`] if the
    /// signature is malformed or does not verify.
    pub fn open(bytes: &[u8], verifying: &VerifyingKey) -> Result<Self, ReleaseManifestError> {
        if bytes.len() != RELEASE_MANIFEST_LEN {
            return Err(ReleaseManifestError::BadLength { got: bytes.len() });
        }
        if bytes[0] != FORMAT_VERSION {
            return Err(ReleaseManifestError::BadVersion(bytes[0]));
        }
        let mut off = 1;
        let mut latest = [0u8; LATEST_RELEASE_LEN];
        latest.copy_from_slice(&bytes[off..off + LATEST_RELEASE_LEN]);
        off += LATEST_RELEASE_LEN;
        let latest_release = u64::from_be_bytes(latest);
        let mut min_supported = [0u8; MIN_SUPPORTED_RELEASE_LEN];
        min_supported.copy_from_slice(&bytes[off..off + MIN_SUPPORTED_RELEASE_LEN]);
        off += MIN_SUPPORTED_RELEASE_LEN;
        let min_supported_release = u64::from_be_bytes(min_supported);
        let mut signed_at = [0u8; SIGNED_AT_LEN];
        signed_at.copy_from_slice(&bytes[off..off + SIGNED_AT_LEN]);
        off += SIGNED_AT_LEN;
        let signed_at_unix_millis = i64::from_be_bytes(signed_at);

        let sig = Signature::from_bytes(&bytes[off..off + SIGNATURE_LEN])
            .map_err(|_| ReleaseManifestError::BadSignature)?;
        let manifest = Self {
            latest_release,
            min_supported_release,
            signed_at_unix_millis,
        };
        verifying
            .verify(&manifest.signing_input(), &sig)
            .map_err(|_| ReleaseManifestError::BadSignature)?;
        Ok(manifest)
    }

    /// `true` when `signed_at` lies within the freshness window: at or after
    /// `now - max_staleness`, and no further in the future than the clock-skew
    /// tolerance ([`MAX_CLOCK_SKEW_MILLIS`]). All arguments are Unix
    /// milliseconds. Uses saturating arithmetic so extreme `now` /
    /// `max_staleness` values cannot overflow.
    ///
    /// Passing `max_staleness_millis == i64::MAX` **disables** the check
    /// entirely (always `true`) — the caller-controlled relaxation the ADR
    /// mandates (a stale manifest is a suppression-only residual).
    #[must_use]
    pub fn is_fresh(&self, now_unix_millis: i64, max_staleness_millis: i64) -> bool {
        if max_staleness_millis == i64::MAX {
            // Freshness gate disabled by the caller — never stale.
            return true;
        }
        let lower = now_unix_millis.saturating_sub(max_staleness_millis);
        let upper = now_unix_millis.saturating_add(MAX_CLOCK_SKEW_MILLIS);
        self.signed_at_unix_millis >= lower && self.signed_at_unix_millis <= upper
    }
}

/// Verify a release manifest's signature and freshness — the full check the
/// **client** applies before rendering an update notice. Returns the parsed
/// [`ReleaseManifest`] so the caller reads `latest_release` /
/// `min_supported_release` (feed them to [`release_status`] against its own
/// current release).
///
/// `now_unix_millis` and `max_staleness_millis` are Unix milliseconds. The
/// staleness window is caller-controlled and **relaxable**: pass
/// `max_staleness_millis == i64::MAX` to disable the freshness gate entirely (a
/// stale manifest can only *suppress* an update notice, never forge one — a
/// LOW-threat residual, ADR-0038).
///
/// # Errors
///
/// Propagates [`ReleaseManifest::open`] errors, then
/// [`ReleaseManifestError::Stale`] if `signed_at` is outside the freshness
/// window (unless the gate is disabled).
pub fn verify_release_manifest(
    bytes: &[u8],
    verifying: &VerifyingKey,
    now_unix_millis: i64,
    max_staleness_millis: i64,
) -> Result<ReleaseManifest, ReleaseManifestError> {
    let manifest = ReleaseManifest::open(bytes, verifying)?;
    if !manifest.is_fresh(now_unix_millis, max_staleness_millis) {
        return Err(ReleaseManifestError::Stale);
    }
    Ok(manifest)
}

/// Decide what a client on `current_release` should do, given a verified
/// `manifest`. A **pure** function — no I/O, no clock, no allocation. A thin
/// convenience over [`release_status_for`] that reads the two release numbers off
/// the manifest.
///
/// `min_supported_release` takes precedence: a client below it gets
/// [`UpdateStatus::UpdateRequired`] even when it is also below `latest_release`.
#[must_use]
pub fn release_status(current_release: u64, manifest: &ReleaseManifest) -> UpdateStatus {
    release_status_for(
        current_release,
        manifest.latest_release,
        manifest.min_supported_release,
    )
}

/// Decide what a client on `current_release` should do, given the raw
/// `latest_release` / `min_supported_release` numbers directly (rather than a
/// [`ReleaseManifest`]). A **pure** function — no I/O, no clock, no allocation.
///
/// This is the shared decision core [`release_status`] delegates to; it exists so
/// a caller that already holds the two release numbers — notably the FFI boundary
/// ([`crate`] consumers pass three integers, not a manifest object, back across
/// the wire) — can reach the verdict without reconstructing a manifest.
///
/// `min_supported_release` takes precedence: a client below it gets
/// [`UpdateStatus::UpdateRequired`] even when it is also below `latest_release`.
#[must_use]
pub const fn release_status_for(
    current_release: u64,
    latest_release: u64,
    min_supported_release: u64,
) -> UpdateStatus {
    if current_release < min_supported_release {
        UpdateStatus::UpdateRequired
    } else if current_release < latest_release {
        UpdateStatus::UpdateAvailable
    } else {
        UpdateStatus::UpToDate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActionAttestation, BackupHead, GenesisAttestation};

    fn key() -> SigningKey {
        SigningKey::from_seed(&[0x11; 32])
    }

    fn sample() -> ReleaseManifest {
        ReleaseManifest::new(42, 30, 1_000_000)
    }

    #[test]
    fn sign_then_open_roundtrips() {
        let sk = key();
        let manifest = sample();
        let wire = manifest.sign(&sk);
        assert_eq!(wire.len(), RELEASE_MANIFEST_LEN);
        assert_eq!(RELEASE_MANIFEST_LEN, 89);
        let got = ReleaseManifest::open(&wire, &sk.verifying_key()).expect("valid manifest opens");
        assert_eq!(got, manifest);
    }

    #[test]
    fn sign_then_verify_roundtrips_fresh() {
        let sk = key();
        let wire = sample().sign(&sk); // signed_at = 1_000_000
        let got = verify_release_manifest(
            &wire,
            &sk.verifying_key(),
            1_000_000, // now == signed_at → fresh
            10_000,
        )
        .expect("a fresh manifest verifies");
        assert_eq!(got.latest_release, 42);
        assert_eq!(got.min_supported_release, 30);
    }

    #[test]
    fn wrong_key_is_rejected() {
        let wire = sample().sign(&key());
        let other = SigningKey::from_seed(&[0x22; 32]).verifying_key();
        assert_eq!(
            ReleaseManifest::open(&wire, &other),
            Err(ReleaseManifestError::BadSignature)
        );
    }

    #[test]
    fn tampering_any_field_breaks_the_signature() {
        let sk = key();
        let wire = sample().sign(&sk);
        let vk = sk.verifying_key();
        // Flip one byte in each region (version, latest_release,
        // min_supported_release, signed_at, sig) and confirm the signature no
        // longer verifies.
        for idx in [0usize, 4, 12, 20, RELEASE_MANIFEST_LEN - 1] {
            let mut bad = wire;
            bad[idx] ^= 0x01;
            // A flipped version byte is caught as BadVersion before the
            // signature check; every other region is BadSignature.
            let expected = if idx == 0 {
                ReleaseManifestError::BadVersion(FORMAT_VERSION ^ 0x01)
            } else {
                ReleaseManifestError::BadSignature
            };
            assert_eq!(
                ReleaseManifest::open(&bad, &vk),
                Err(expected),
                "tampering byte {idx} must be rejected"
            );
        }
    }

    #[test]
    fn malformed_envelope_is_rejected() {
        let sk = key();
        let vk = sk.verifying_key();
        assert_eq!(
            ReleaseManifest::open(&[0u8; 10], &vk),
            Err(ReleaseManifestError::BadLength { got: 10 })
        );
        // Truncated by one byte.
        assert_eq!(
            ReleaseManifest::open(&[0u8; RELEASE_MANIFEST_LEN - 1], &vk),
            Err(ReleaseManifestError::BadLength {
                got: RELEASE_MANIFEST_LEN - 1
            })
        );
        // One byte too long.
        assert_eq!(
            ReleaseManifest::open(&[0u8; RELEASE_MANIFEST_LEN + 1], &vk),
            Err(ReleaseManifestError::BadLength {
                got: RELEASE_MANIFEST_LEN + 1
            })
        );
        let mut wire = sample().sign(&sk);
        wire[0] = 9; // bad version
        assert_eq!(
            ReleaseManifest::open(&wire, &vk),
            Err(ReleaseManifestError::BadVersion(9))
        );
    }

    #[test]
    fn stale_manifest_is_rejected() {
        let sk = key();
        let wire = sample().sign(&sk); // signed_at = 1_000_000
        let vk = sk.verifying_key();
        // now far past signed_at + window → below the lower bound → Stale.
        assert_eq!(
            verify_release_manifest(&wire, &vk, 2_000_000, 10_000),
            Err(ReleaseManifestError::Stale)
        );
    }

    #[test]
    fn disabled_freshness_gate_never_stale() {
        // The SAME manifest that is Stale under a finite window verifies OK when
        // the caller disables the gate with max_staleness_millis == i64::MAX
        // (the ADR-0038 caller-controlled relaxation).
        let sk = key();
        let wire = sample().sign(&sk); // signed_at = 1_000_000
        let vk = sk.verifying_key();
        assert_eq!(
            verify_release_manifest(&wire, &vk, 9_000_000_000, 10_000),
            Err(ReleaseManifestError::Stale),
            "a finite window rejects a very old manifest"
        );
        let got = verify_release_manifest(&wire, &vk, 9_000_000_000, i64::MAX)
            .expect("i64::MAX disables the freshness gate");
        assert_eq!(got.latest_release, 42);
    }

    #[test]
    fn fresh_within_window_and_forward_skew() {
        let sk = key();
        let wire = sample().sign(&sk); // signed_at = 1_000_000
        let vk = sk.verifying_key();
        // Exactly at the lower window bound (signed_at == now - max_staleness).
        assert!(
            verify_release_manifest(&wire, &vk, 1_010_000, 10_000).is_ok(),
            "the lower window bound is inclusive"
        );
        // signed_at slightly in the future of now, within MAX_CLOCK_SKEW_MILLIS
        // (5 min = 300_000 ms) → accepted (tolerates a fast publisher clock).
        assert!(
            verify_release_manifest(&wire, &vk, 999_000, 10_000).is_ok(),
            "a manifest within the forward clock-skew tolerance verifies"
        );
        // signed_at far in the future of now, beyond the skew tolerance →
        // upper = now + 300_000 = 900_000 < 1_000_000 → Stale.
        assert_eq!(
            verify_release_manifest(&wire, &vk, 600_000, 10_000),
            Err(ReleaseManifestError::Stale),
            "a manifest beyond the forward skew tolerance is stale"
        );
    }

    #[test]
    fn release_status_covers_every_band() {
        let manifest = ReleaseManifest::new(42, 30, 1_000_000);
        // current < min_supported → UpdateRequired.
        assert_eq!(release_status(29, &manifest), UpdateStatus::UpdateRequired);
        // min_supported <= current < latest → UpdateAvailable.
        assert_eq!(release_status(30, &manifest), UpdateStatus::UpdateAvailable);
        assert_eq!(release_status(41, &manifest), UpdateStatus::UpdateAvailable);
        // current >= latest → UpToDate.
        assert_eq!(release_status(42, &manifest), UpdateStatus::UpToDate);
        assert_eq!(release_status(100, &manifest), UpdateStatus::UpToDate);
    }

    #[test]
    fn release_status_min_supported_takes_precedence() {
        // When current < min < latest, the required-update verdict wins over the
        // merely-available one.
        let manifest = ReleaseManifest::new(50, 40, 1_000_000);
        assert_eq!(release_status(10, &manifest), UpdateStatus::UpdateRequired);
    }

    #[test]
    fn release_status_for_matches_release_status_across_bands() {
        // The three-integer core agrees with the manifest-taking convenience for
        // every band, incl. the min-supported-precedence corner — it is the same
        // decision, so the FFI three-integer entry point never diverges from the
        // manifest path.
        let manifest = ReleaseManifest::new(50, 40, 1_000_000);
        for current in [0u64, 39, 40, 41, 49, 50, 100] {
            assert_eq!(
                release_status_for(current, 50, 40),
                release_status(current, &manifest),
                "release_status_for must match release_status at current={current}"
            );
        }
        // And the three bands directly.
        assert_eq!(
            release_status_for(10, 50, 40),
            UpdateStatus::UpdateRequired,
            "current < min_supported → UpdateRequired"
        );
        assert_eq!(
            release_status_for(45, 50, 40),
            UpdateStatus::UpdateAvailable,
            "min_supported <= current < latest → UpdateAvailable"
        );
        assert_eq!(
            release_status_for(50, 50, 40),
            UpdateStatus::UpToDate,
            "current >= latest → UpToDate"
        );
    }

    #[test]
    fn domains_do_not_cross_between_all_four_attestation_kinds() {
        // Domain separation is load-bearing: even under a SHARED signing key, a
        // `ReleaseManifest`, a `BackupHead`, a `GenesisAttestation`, and an
        // `ActionAttestation` must be cryptographically non-interchangeable. Two
        // mechanisms enforce it — (a) distinct total lengths, and (b) distinct
        // domain-prefixed signing inputs — and this proves BOTH, four-way.
        let sk = key();
        let vk = sk.verifying_key();

        let release_wire = sample().sign(&sk); // 89 bytes
        let backup_wire = BackupHead::new([0xAA; 16], 7, [0xBB; 32], 1_000_000).sign(&sk); // 129 bytes
        let genesis_wire =
            GenesisAttestation::new([0xAA; 16], [0xBB; 32], [0xCC; 32], 1_000).sign(&sk); // 153 bytes
        let action_wire = ActionAttestation::new([0xAA; 16], [0xBB; 32], 1_000).sign(&sk); // 121 bytes

        // (a) Length: the release parser accepts none of the other three kinds'
        // wire blobs, and none of the other three parsers accept the release
        // blob.
        for other in [&backup_wire[..], &genesis_wire[..], &action_wire[..]] {
            assert!(
                matches!(
                    ReleaseManifest::open(other, &vk),
                    Err(ReleaseManifestError::BadLength { .. })
                ),
                "a foreign blob must not parse as a release manifest"
            );
        }
        assert!(
            matches!(
                BackupHead::open(&release_wire, &vk),
                Err(crate::BackupHeadError::BadLength { .. })
            ),
            "a release blob must not parse as a backup head"
        );
        assert!(
            matches!(
                GenesisAttestation::open(&release_wire, &vk),
                Err(crate::GenesisAttestError::BadLength { .. })
            ),
            "a release blob must not parse as a genesis attestation"
        );
        assert!(
            matches!(
                ActionAttestation::open(&release_wire, &vk),
                Err(crate::AttestError::BadLength { .. })
            ),
            "a release blob must not parse as an action attestation"
        );

        // (b) Domain: splice a backup-head signature (same 64-byte length) into a
        // release-shaped buffer. The backup signature was produced over a
        // different domain-prefixed signing input, so it must NOT verify under
        // the release (`lakko/release-manifest/v1`) signing input — proving the
        // domain prefix, not merely the length, keeps the signatures apart.
        let backup_sig = &backup_wire[backup_wire.len() - SIGNATURE_LEN..];
        let mut spliced = sample().sign(&sk);
        spliced[RELEASE_MANIFEST_LEN - SIGNATURE_LEN..].copy_from_slice(backup_sig);
        assert_eq!(
            ReleaseManifest::open(&spliced, &vk),
            Err(ReleaseManifestError::BadSignature),
            "a backup-domain signature must not verify as a release signature"
        );

        // And the reverse: a release-manifest signature spliced into a
        // backup-shaped buffer must not verify under the backup signing input.
        let release_sig = &release_wire[release_wire.len() - SIGNATURE_LEN..];
        let mut backup_spliced = BackupHead::new([0xAA; 16], 7, [0xBB; 32], 1_000_000).sign(&sk);
        let backup_len = backup_spliced.len();
        backup_spliced[backup_len - SIGNATURE_LEN..].copy_from_slice(release_sig);
        assert_eq!(
            BackupHead::open(&backup_spliced, &vk),
            Err(crate::BackupHeadError::BadSignature),
            "a release-domain signature must not verify as a backup-head signature"
        );
    }
}
