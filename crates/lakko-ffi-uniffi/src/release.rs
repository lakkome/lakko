// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for `lakko-attest`'s release manifest (ADR-0038 S2).
//!
//! Mirror of `crates/lakko-ffi-wasm/src/release.rs` for iOS + Android. Exposes
//! the "an update is available" notice's **verify + status** surface — never a
//! sign. A release manifest is signed by a dedicated, offline **publisher release
//! key** (a later sprint's standalone tool); clients only ever verify it (same
//! posture as the backup-head FFI, ADR-0037: verify only, signing is off-device).
//!
//! Two primitives:
//!
//! - [`verify_release_manifest`] — parse + Ed25519-verify a fetched manifest
//!   against the client-pinned publisher key + apply the freshness window,
//!   returning the authenticated release numbers. Throws on ANY failure.
//! - [`release_update_status`] — a PURE three-integer function returning the
//!   [`UpdateStatus`] verdict, so the client decides "up to date / update
//!   available / update required" without passing a manifest object back across
//!   the boundary.
//!
//! **Error posture (differs from the backup-head FFI — simpler).** ADR-0038's UX
//! fails toward "up to date" on ANY verify failure: an unverified manifest must
//! never drive an update nag. So there is no discriminable stale-vs-invalid split
//! — every [`ReleaseManifestError`] folds into the single [`FfiError::Release`]
//! variant, and the caller catches it and treats the app as up-to-date.

use lakko_attest::{
    release_status_for as inner_release_status_for,
    verify_release_manifest as inner_verify_release_manifest,
    ReleaseManifest as InnerReleaseManifest, ReleaseManifestError,
    UpdateStatus as InnerUpdateStatus,
};
use lakko_crypto::signing::VerifyingKey;

use crate::FfiError;

impl From<ReleaseManifestError> for FfiError {
    fn from(e: ReleaseManifestError) -> Self {
        // ADR-0038: no stale-vs-invalid split — the client fails toward
        // "up to date" on any verify failure, so every cause folds into the one
        // Release variant.
        FfiError::Release(e.to_string())
    }
}

/// What a client on its current release should do about an update. The UniFFI
/// projection of [`lakko_attest::UpdateStatus`]. The client renders an optional
/// or mandatory update prompt (or nothing) off this.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
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

impl From<InnerUpdateStatus> for UpdateStatus {
    fn from(s: InnerUpdateStatus) -> Self {
        match s {
            InnerUpdateStatus::UpToDate => Self::UpToDate,
            InnerUpdateStatus::UpdateAvailable => Self::UpdateAvailable,
            InnerUpdateStatus::UpdateRequired => Self::UpdateRequired,
        }
    }
}

/// The authenticated release numbers a verified manifest carries — the UniFFI
/// projection of [`lakko_attest::ReleaseManifest`]. Returned by
/// [`verify_release_manifest`] on success; the caller feeds `latest_release` +
/// `min_supported_release` (with its own current release) to
/// [`release_update_status`]. `Debug` is safe: every field is a public, signed
/// commitment (no secret / key material — a release manifest binds no account).
#[derive(uniffi::Record, Clone, Debug)]
pub struct VerifiedReleaseManifest {
    /// The newest published release number.
    pub latest_release: u64,
    /// The oldest release number the backend still accepts. A client below this
    /// must update; feed to [`release_update_status`].
    pub min_supported_release: u64,
    /// The mint timestamp, Unix **milliseconds**. The client applies its own
    /// staleness window against this (it is not an expiry).
    pub signed_at_millis: i64,
}

impl From<InnerReleaseManifest> for VerifiedReleaseManifest {
    fn from(m: InnerReleaseManifest) -> Self {
        Self {
            latest_release: m.latest_release,
            min_supported_release: m.min_supported_release,
            signed_at_millis: m.signed_at_unix_millis,
        }
    }
}

/// Parse a release manifest's wire bytes and verify the Ed25519 signature against
/// `verifying_key` (the client-pinned publisher release key) AND the freshness
/// window — the full check a client applies before rendering an update notice
/// (ADR-0038). Returns the authenticated [`VerifiedReleaseManifest`].
///
/// `manifest` is the raw fetched manifest bytes. `verifying_key` is the 32-byte
/// pinned publisher release verifying key. `now_millis` and `max_staleness_millis`
/// are Unix milliseconds; pass `max_staleness_millis == i64::MAX` to disable the
/// freshness gate entirely (a stale manifest can only *suppress* an update notice,
/// never forge one — a LOW-threat residual, ADR-0038).
///
/// # Errors
///
/// [`FfiError::Release`] on ANY failure — a malformed / wrong-length /
/// wrong-version manifest, a signature that does not verify, a stale manifest, or
/// a bad `verifying_key`. There is no discriminable stale-vs-invalid split
/// (ADR-0038): the caller catches this one variant and treats the app as
/// up-to-date, so an unverified manifest never drives an update nag.
#[uniffi::export]
pub fn verify_release_manifest(
    manifest: Vec<u8>,
    verifying_key: Vec<u8>,
    now_millis: i64,
    max_staleness_millis: i64,
) -> Result<VerifiedReleaseManifest, FfiError> {
    let verifying = VerifyingKey::from_bytes(&verifying_key)
        .map_err(|_| FfiError::Release("publisher key is not a valid Ed25519 key".into()))?;
    let manifest =
        inner_verify_release_manifest(&manifest, &verifying, now_millis, max_staleness_millis)?;
    Ok(manifest.into())
}

/// Decide what a client on `current_release` should do, given the
/// `latest_release` / `min_supported_release` numbers of a *verified* manifest.
/// A PURE function — no I/O, no clock, no allocation, no failure — so the client
/// need not pass a manifest object back across the FFI: it reads the two numbers
/// off a prior [`verify_release_manifest`] and calls this with its own current
/// release.
///
/// `min_supported_release` takes precedence: a client below it gets
/// [`UpdateStatus::UpdateRequired`] even when it is also below `latest_release`.
#[uniffi::export]
#[must_use]
pub fn release_update_status(
    current_release: u64,
    latest_release: u64,
    min_supported_release: u64,
) -> UpdateStatus {
    inner_release_status_for(current_release, latest_release, min_supported_release).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::signing::SigningKey;

    fn signed_manifest(
        signing: &SigningKey,
        latest: u64,
        min_supported: u64,
        signed_at_millis: i64,
    ) -> Vec<u8> {
        InnerReleaseManifest::new(latest, min_supported, signed_at_millis)
            .sign(signing)
            .to_vec()
    }

    #[test]
    fn verify_release_manifest_round_trips_through_ffi() {
        let signing = SigningKey::from_seed(&[0x11; 32]);
        let key = signing.verifying_key().to_bytes().to_vec();
        let wire = signed_manifest(&signing, 42, 30, 1_000_000);
        let got = verify_release_manifest(
            wire, key, 1_000_000, // now == signed_at → fresh
            10_000,
        )
        .expect("a fresh manifest verifies");
        assert_eq!(got.latest_release, 42);
        assert_eq!(got.min_supported_release, 30);
        assert_eq!(got.signed_at_millis, 1_000_000);
    }

    #[test]
    fn disabled_freshness_gate_verifies_an_old_manifest() {
        // i64::MAX disables the staleness gate (ADR-0038 caller-controlled
        // relaxation), so a very old manifest still verifies.
        let signing = SigningKey::from_seed(&[0x11; 32]);
        let key = signing.verifying_key().to_bytes().to_vec();
        let wire = signed_manifest(&signing, 42, 30, 1_000_000);
        let got = verify_release_manifest(wire, key, 9_000_000_000, i64::MAX)
            .expect("i64::MAX disables the freshness gate");
        assert_eq!(got.latest_release, 42);
    }

    #[test]
    fn every_verify_failure_folds_into_the_single_release_variant() {
        let signing = SigningKey::from_seed(&[0x11; 32]);
        let wire = signed_manifest(&signing, 42, 30, 1_000_000);

        // Wrong key → signature does not verify → Release.
        let other_key = SigningKey::from_seed(&[0x22; 32])
            .verifying_key()
            .to_bytes()
            .to_vec();
        let err = verify_release_manifest(wire.clone(), other_key, 1_000_000, 10_000)
            .expect_err("a wrong key is rejected");
        assert!(matches!(err, FfiError::Release(_)), "got {err:?}");

        // Stale manifest → Release (NOT a distinct variant — ADR-0038).
        let key = signing.verifying_key().to_bytes().to_vec();
        let err = verify_release_manifest(wire.clone(), key.clone(), 9_000_000, 10_000)
            .expect_err("a stale manifest is rejected");
        assert!(
            matches!(err, FfiError::Release(_)),
            "staleness must fold into the single Release variant, got {err:?}"
        );

        // Malformed (truncated) manifest → Release.
        let err = verify_release_manifest(vec![0u8; 10], key.clone(), 1_000_000, 10_000)
            .expect_err("a truncated manifest is rejected");
        assert!(matches!(err, FfiError::Release(_)), "got {err:?}");

        // Bad verifying key length → Release.
        let err = verify_release_manifest(wire, vec![0u8; 8], 1_000_000, 10_000)
            .expect_err("a bad key length is rejected");
        assert!(matches!(err, FfiError::Release(_)), "got {err:?}");
    }

    #[test]
    fn release_update_status_covers_every_band() {
        // current < min_supported → UpdateRequired.
        assert_eq!(
            release_update_status(29, 42, 30),
            UpdateStatus::UpdateRequired
        );
        // min_supported <= current < latest → UpdateAvailable.
        assert_eq!(
            release_update_status(30, 42, 30),
            UpdateStatus::UpdateAvailable
        );
        assert_eq!(
            release_update_status(41, 42, 30),
            UpdateStatus::UpdateAvailable
        );
        // current >= latest → UpToDate.
        assert_eq!(release_update_status(42, 42, 30), UpdateStatus::UpToDate);
        assert_eq!(release_update_status(100, 42, 30), UpdateStatus::UpToDate);
        // min-supported precedence: current < min < latest → UpdateRequired.
        assert_eq!(
            release_update_status(10, 50, 40),
            UpdateStatus::UpdateRequired
        );
    }
}
