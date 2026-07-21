// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for `lakko-attest`'s release manifest (ADR-0038 S2).
//!
//! Mirror of `crates/lakko-ffi-uniffi/src/release.rs` for the web client.
//! Exposes the "an update is available" notice's **verify + status** surface —
//! never a sign. A release manifest is signed by a dedicated, offline **publisher
//! release key** (a later sprint's standalone tool); clients only ever verify it
//! (same posture as the backup-head FFI, ADR-0037: verify only, signing is
//! off-device).
//!
//! Two primitives:
//!
//! - [`verify_release_manifest`] (`verifyReleaseManifest`) — parse + Ed25519-
//!   verify a fetched manifest against the pinned publisher key + apply the
//!   freshness window, returning the authenticated release numbers. Throws on ANY
//!   failure.
//! - [`release_update_status`] (`releaseUpdateStatus`) — a PURE three-integer
//!   function returning the [`UpdateStatus`] verdict, so the web client decides
//!   "up to date / update available / update required" without passing a manifest
//!   object back across the boundary.
//!
//! **Error posture (differs from the backup-head FFI — simpler).** ADR-0038's UX
//! fails toward "up to date" on ANY verify failure: an unverified manifest must
//! never drive an update nag. So there is NO discriminable stale-vs-invalid code
//! split (unlike the backup-head `stale:` / `invalid:` convention) — every
//! [`ReleaseManifestError`] throws a single `JsError` whose message is prefixed
//! with the stable `release:` code, and the web client catches it and treats the
//! app as up-to-date.

use lakko_attest::ReleaseManifestError;
use lakko_crypto::signing::VerifyingKey;
use wasm_bindgen::prelude::*;

/// Stable code prefixed onto a thrown release-manifest verify error. Unlike the
/// backup-head convention (`stale:` / `invalid:`) there is only ONE code — the
/// client fails toward "up to date" on any failure, so it never needs to tell the
/// causes apart.
const RELEASE_CODE: &str = "release";

/// Build a `release:`-coded error message.
fn coded(msg: &str) -> String {
    format!("{RELEASE_CODE}: {msg}")
}

/// What a client on its current release should do about an update. The wasm
/// projection of [`lakko_attest::UpdateStatus`]. wasm-bindgen renders this as a
/// JS number-enum — `UpToDate = 0`, `UpdateAvailable = 1`, `UpdateRequired = 2`
/// (same convention as the [`crate::mls::Role`] enum) — so the web client
/// switches on the numeric value.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateStatus {
    /// The current release is at or above `latest_release` — nothing to do (`0`).
    UpToDate,
    /// A newer release exists (`current < latest`) but the current release is
    /// still supported — offer an optional update (`1`).
    UpdateAvailable,
    /// The current release is below `min_supported_release` — the client must
    /// update to keep working (`2`). Takes precedence over
    /// [`UpdateStatus::UpdateAvailable`].
    UpdateRequired,
}

impl From<lakko_attest::UpdateStatus> for UpdateStatus {
    fn from(s: lakko_attest::UpdateStatus) -> Self {
        match s {
            lakko_attest::UpdateStatus::UpToDate => Self::UpToDate,
            lakko_attest::UpdateStatus::UpdateAvailable => Self::UpdateAvailable,
            lakko_attest::UpdateStatus::UpdateRequired => Self::UpdateRequired,
        }
    }
}

/// The authenticated release numbers a verified manifest carries — the wasm
/// projection of [`lakko_attest::ReleaseManifest`]. Returned by
/// [`verify_release_manifest`] on success; the caller feeds `latestRelease` +
/// `minSupportedRelease` (with its own current release) to
/// [`release_update_status`].
#[wasm_bindgen]
pub struct VerifiedReleaseManifest {
    inner: lakko_attest::ReleaseManifest,
}

impl From<lakko_attest::ReleaseManifest> for VerifiedReleaseManifest {
    fn from(inner: lakko_attest::ReleaseManifest) -> Self {
        Self { inner }
    }
}

#[wasm_bindgen]
impl VerifiedReleaseManifest {
    /// The newest published release number (BigInt on the JS side).
    #[wasm_bindgen(getter, js_name = latestRelease)]
    pub fn latest_release(&self) -> u64 {
        self.inner.latest_release
    }

    /// The oldest release number the backend still accepts (BigInt on the JS
    /// side). A client below this must update; feed to [`release_update_status`].
    #[wasm_bindgen(getter, js_name = minSupportedRelease)]
    pub fn min_supported_release(&self) -> u64 {
        self.inner.min_supported_release
    }

    /// The mint timestamp, Unix **milliseconds** (BigInt on the JS side). The
    /// client applies its own staleness window against this (it is not an
    /// expiry).
    #[wasm_bindgen(getter, js_name = signedAtMillis)]
    pub fn signed_at_millis(&self) -> i64 {
        self.inner.signed_at_unix_millis
    }
}

/// Parse a release manifest's wire bytes and verify the Ed25519 signature against
/// `verifyingKey` (the client-pinned publisher release key) AND the
/// freshness window — the full check the web client applies before rendering an
/// update notice (ADR-0038). Returns the authenticated [`VerifiedReleaseManifest`].
///
/// `manifest` is the raw fetched manifest bytes. `verifyingKey` is the 32-byte
/// pinned publisher release verifying key. `nowMillis` and `maxStalenessMillis`
/// are Unix milliseconds (BigInt on the JS side); pass
/// `maxStalenessMillis === (2n ** 63n - 1n)` (`i64::MAX`) to disable the freshness
/// gate entirely (a stale manifest can only *suppress* an update notice, never
/// forge one — a LOW-threat residual, ADR-0038).
///
/// Throws a `release:`-coded `JsError` on ANY failure — a malformed / wrong-length
/// / wrong-version manifest, a signature that does not verify, a stale manifest,
/// or a bad `verifyingKey`. There is no discriminable stale-vs-invalid split
/// (ADR-0038): the caller catches the throw and treats the app as up-to-date, so
/// an unverified manifest never drives an update nag.
#[wasm_bindgen(js_name = verifyReleaseManifest)]
pub fn verify_release_manifest(
    manifest: &[u8],
    verifying_key: &[u8],
    now_millis: i64,
    max_staleness_millis: i64,
) -> Result<VerifiedReleaseManifest, JsError> {
    verify_release_manifest_inner(manifest, verifying_key, now_millis, max_staleness_millis)
        .map(VerifiedReleaseManifest::from)
        .map_err(|msg| JsError::new(&msg))
}

/// The `JsError`-free core of [`verify_release_manifest`]. Returns
/// `Err(coded_msg)` on any failure so the negative paths are exercisable on the
/// host target (`JsError::new` panics on a non-wasm target per wasm-bindgen-0.2's
/// "non-wasm targets" rule).
fn verify_release_manifest_inner(
    manifest: &[u8],
    verifying_key: &[u8],
    now_millis: i64,
    max_staleness_millis: i64,
) -> Result<lakko_attest::ReleaseManifest, String> {
    let verifying = VerifyingKey::from_bytes(verifying_key)
        .map_err(|_| coded("publisher key is not a valid Ed25519 key"))?;
    lakko_attest::verify_release_manifest(manifest, &verifying, now_millis, max_staleness_millis)
        .map_err(|e: ReleaseManifestError| coded(&e.to_string()))
}

/// Decide what a client on `currentRelease` should do, given the `latestRelease`
/// / `minSupportedRelease` numbers of a *verified* manifest. A PURE function — no
/// I/O, no clock, no allocation, no failure — so the web client need not pass a
/// manifest object back across the boundary: it reads the two numbers off a prior
/// [`verify_release_manifest`] and calls this with its own current release. All
/// three arguments are BigInt on the JS side.
///
/// `minSupportedRelease` takes precedence: a client below it gets
/// [`UpdateStatus::UpdateRequired`] even when it is also below `latestRelease`.
#[wasm_bindgen(js_name = releaseUpdateStatus)]
#[must_use]
pub fn release_update_status(
    current_release: u64,
    latest_release: u64,
    min_supported_release: u64,
) -> UpdateStatus {
    lakko_attest::release_status_for(current_release, latest_release, min_supported_release).into()
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
        lakko_attest::ReleaseManifest::new(latest, min_supported, signed_at_millis)
            .sign(signing)
            .to_vec()
    }

    #[test]
    fn verify_release_manifest_round_trips() {
        let signing = SigningKey::from_seed(&[0x11; 32]);
        let key = signing.verifying_key().to_bytes();
        let wire = signed_manifest(&signing, 42, 30, 1_000_000);
        let manifest =
            verify_release_manifest_inner(&wire, &key, 1_000_000, 10_000).expect("valid manifest");
        assert_eq!(manifest.latest_release, 42);
        // Also exercise the wasm struct projection getters on the host.
        let view = VerifiedReleaseManifest::from(manifest);
        assert_eq!(view.latest_release(), 42);
        assert_eq!(view.min_supported_release(), 30);
        assert_eq!(view.signed_at_millis(), 1_000_000);
    }

    #[test]
    fn disabled_freshness_gate_verifies_an_old_manifest() {
        // i64::MAX disables the staleness gate (ADR-0038 caller-controlled
        // relaxation), so a very old manifest still verifies.
        let signing = SigningKey::from_seed(&[0x11; 32]);
        let key = signing.verifying_key().to_bytes();
        let wire = signed_manifest(&signing, 42, 30, 1_000_000);
        let manifest = verify_release_manifest_inner(&wire, &key, 9_000_000_000, i64::MAX)
            .expect("i64::MAX disables the freshness gate");
        assert_eq!(manifest.latest_release, 42);
    }

    #[test]
    fn every_verify_failure_carries_the_release_code() {
        let signing = SigningKey::from_seed(&[0x11; 32]);
        let key = signing.verifying_key().to_bytes();
        let wire = signed_manifest(&signing, 42, 30, 1_000_000);

        // Wrong key → signature does not verify → release-coded.
        let other = SigningKey::from_seed(&[0x22; 32])
            .verifying_key()
            .to_bytes();
        let err = verify_release_manifest_inner(&wire, &other, 1_000_000, 10_000)
            .expect_err("a wrong key is rejected");
        assert!(err.starts_with("release:"), "got {err:?}");

        // Stale manifest → release-coded (NOT a distinct code — ADR-0038).
        let err = verify_release_manifest_inner(&wire, &key, 9_000_000, 10_000)
            .expect_err("a stale manifest is rejected");
        assert!(
            err.starts_with("release:"),
            "staleness must carry the single release code, got {err:?}"
        );

        // Malformed (truncated) manifest → release-coded.
        let err = verify_release_manifest_inner(&[0u8; 10], &key, 1_000_000, 10_000)
            .expect_err("a truncated manifest is rejected");
        assert!(err.starts_with("release:"), "got {err:?}");

        // Bad verifying key length → release-coded.
        let err = verify_release_manifest_inner(&wire, &[0u8; 8], 1_000_000, 10_000)
            .expect_err("a bad key length is rejected");
        assert!(err.starts_with("release:"), "got {err:?}");
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
