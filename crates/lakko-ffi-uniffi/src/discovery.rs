// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI exposure of `lakko-discovery-client`.
//!
//! Mirror of `lakko-ffi-wasm/src/discovery.rs`. Two public items:
//!
//!   * [`normalize_username`] — a pure helper that turns whatever
//!     the user typed (`"  @Alice  "`, `"alice\n"`) into the
//!     canonical form (`"alice"`). Used in the Compose UI to
//!     surface "Looking up @alice…" while a request is in flight,
//!     and by the discovery server's own normalisation path on
//!     the wire (the strings have to agree byte-for-byte so the
//!     OPRF hash matches).
//!
//!   * [`OprfBlinder`] — the OPRF dance handle. Construct with
//!     `OprfBlinder::blind(username)`, read `blinded_b64` for the
//!     `/discovery/oprf` request body, then call `finalize` with
//!     the server's `evaluated` response to obtain the 64-byte
//!     OPRF hash (consumed by `/discovery/lookup` /
//!     `/discovery/register` as `oprf_hash`).
//!
//! ## "Consume on `finalize`" model
//!
//! Same posture as the OPAQUE wrappers in [`super::auth`].
//! `OprfClient::finalize` takes `self` by value — a second call
//! is a protocol bug that produces nonsense output without an
//! error signal. UniFFI `&self` methods cannot consume the
//! receiver, so the blinder is held in a `Mutex<Option<…>>` and
//! `.take()`d on the first `finalize`. A second call returns
//! [`FfiError::DiscoveryAlreadyFinalized`] instead of corrupting
//! math.

use std::sync::Mutex;

use lakko_discovery_client::{
    normalize_username as inner_normalize, validate_username as inner_validate,
    DiscoveryClientError, OprfBlinder as InnerBlinder,
};

use crate::FfiError;

impl From<DiscoveryClientError> for FfiError {
    fn from(e: DiscoveryClientError) -> Self {
        match e {
            // The typed ADR-0048 §4 reason becomes its own variant, so the
            // blinder's refusal is branchable exactly like `validate_username`'s.
            DiscoveryClientError::InvalidUsername(reason) => reason.into(),
            other => Self::Discovery(other.to_string()),
        }
    }
}

/// Normalise `input` and require the ADR-0048 §4 username rule — ASCII
/// `[a-z0-9_]`, 3 to 32 characters. Returns the normalised form.
///
/// What signup and search fields call as the user types, so the reason can
/// be shown and a lookup is never sent for a name that fails. The blinder
/// runs the same check, so a caller that skips this still cannot register
/// or look up a non-conforming name.
///
/// # Errors
///
/// * [`FfiError::UsernameEmpty`] / [`FfiError::UsernameTooShort`] /
///   [`FfiError::UsernameTooLong`] / [`FfiError::UsernameInvalidCharacters`]
///   — the reason is the variant; the message never contains the input.
#[uniffi::export]
pub fn validate_username(input: String) -> Result<String, FfiError> {
    inner_validate(&input).map_err(Into::into)
}

/// Normalise the typed username and return the canonical form.
///
/// Pure helper, exposed as a free function. The UI uses it for
/// "Looking up @alice…" feedback; production discovery flows
/// drive [`OprfBlinder::blind`] which calls this internally.
///
/// Trim → strip a leading `@` → trim again. The double-trim
/// handles `"\t@alice\n"` (whitespace outside the `@`) and
/// `"@ alice"` (a space between the `@` and the handle) — both
/// normal copy-paste artefacts.
#[uniffi::export]
pub fn normalize_username(input: String) -> String {
    inner_normalize(&input)
}

/// Client-side OPRF blinder handle — Swift / Kotlin exposure of
/// [`lakko_discovery_client::OprfBlinder`].
///
/// Kotlin usage:
///
/// ```kotlin
/// import uniffi.lakko_ffi_uniffi.OprfBlinder
///
/// val blinder = OprfBlinder.blind("alice")
/// val request = OprfChallengeRequest(blinded = blinder.blindedB64())
/// // POST /discovery/oprf → response.evaluated
/// val hash = blinder.finalize(response.evaluated)  // 64-byte ByteArray
/// // hash → oprf_hash field on /discovery/lookup
/// ```
#[derive(uniffi::Object)]
pub struct OprfBlinder {
    /// `Mutex<Option<…>>` so `finalize` (which moves the inner
    /// blinder into `voprf::OprfClient::finalize`) can `take()`
    /// the slot. UniFFI `Object`s are always `Arc<Self>` on the
    /// FFI side, so interior mutability is required regardless.
    inner: Mutex<Option<InnerBlinder>>,
    blinded_b64: String,
}

#[uniffi::export]
impl OprfBlinder {
    /// Normalise `username` and produce a fresh blinded element.
    ///
    /// `blinded_b64()` then exposes the base64 string for the
    /// `/discovery/oprf` request body.
    ///
    /// # Errors
    ///
    /// * [`FfiError::UsernameEmpty`] / [`FfiError::UsernameTooShort`] /
    ///   [`FfiError::UsernameTooLong`] /
    ///   [`FfiError::UsernameInvalidCharacters`] — the name fails the
    ///   ADR-0048 §4 rule, so it can be neither registered nor looked up.
    /// * [`FfiError::Discovery`] with a "voprf" message on a
    ///   blind failure — extremely unlikely on a sane RNG;
    ///   treat as fatal.
    #[uniffi::constructor]
    pub fn blind(username: String) -> Result<Self, FfiError> {
        let (blinder, blinded_b64) = InnerBlinder::blind(&username)?;
        Ok(Self {
            inner: Mutex::new(Some(blinder)),
            blinded_b64,
        })
    }

    /// Domain-separated `@channel` variant of [`Self::blind`]. Blinds
    /// the channel-namespace OPRF input (`CHANNEL_OPRF_DOMAIN ‖
    /// normalise(handle)`), so a channel handle never produces the same
    /// OPRF hash as the identical string used as a `@username`
    /// (lakko-security channels review, Finding 2). Both the channel
    /// register and lookup paths MUST use this constructor so their
    /// hashes agree.
    ///
    /// # Errors
    ///
    /// [`FfiError::Discovery`] for an empty-after-normalisation handle or
    /// an OPRF blind failure. The ADR-0048 §4 username rule is NOT applied
    /// to channel handles — a separate namespace with its own policy.
    #[uniffi::constructor]
    pub fn blind_channel(handle: String) -> Result<Self, FfiError> {
        let (blinder, blinded_b64) = InnerBlinder::blind_channel(&handle)?;
        Ok(Self {
            inner: Mutex::new(Some(blinder)),
            blinded_b64,
        })
    }

    /// Base64-encoded blinded element. Goes into the `blinded`
    /// field of the `/discovery/oprf` request body. Cheap to
    /// read; the cached string is cloned per call.
    pub fn blinded_b64(&self) -> String {
        self.blinded_b64.clone()
    }

    /// The normalised form of the typed username. Surface in
    /// the UI ("Looking up @alice…") so the user sees what's
    /// actually being queried.
    ///
    /// Errors with [`FfiError::DiscoveryAlreadyFinalized`] if
    /// the blinder has already been consumed by [`finalize`].
    pub fn normalized_username(&self) -> Result<String, FfiError> {
        let guard = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        guard
            .as_ref()
            .map(|b| b.normalised_username().to_string())
            .ok_or(FfiError::DiscoveryAlreadyFinalized)
    }

    /// Finalise the OPRF dance with the server's evaluation.
    ///
    /// Returns the 64-byte OPRF hash. Single-use: a second call
    /// on the same instance returns
    /// [`FfiError::DiscoveryAlreadyFinalized`] rather than
    /// producing a second hash.
    ///
    /// # Errors
    ///
    /// * [`FfiError::DiscoveryAlreadyFinalized`] — second call.
    /// * [`FfiError::Discovery`] — base64 decode failure, or
    ///   the bytes decode but are not a valid
    ///   `EvaluationElement`, or the underlying `voprf`
    ///   finalisation rejected the math (wire corruption in
    ///   the non-verifiable mode we use).
    pub fn finalize(&self, evaluated_b64: String) -> Result<Vec<u8>, FfiError> {
        let mut guard = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let blinder = guard.take().ok_or(FfiError::DiscoveryAlreadyFinalized)?;
        let hash = blinder.finalize(&evaluated_b64)?;
        Ok(hash.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_at_and_trims() {
        assert_eq!(normalize_username("  @Alice  ".to_string()), "alice");
        assert_eq!(normalize_username("@bob".to_string()), "bob");
        assert_eq!(normalize_username("carol".to_string()), "carol");
    }

    #[test]
    fn normalize_collapses_outer_whitespace() {
        assert_eq!(normalize_username("\t@Dave\n".to_string()), "dave");
    }

    #[test]
    fn blind_produces_non_empty_b64() {
        let blinder = OprfBlinder::blind("alice".to_string()).expect("blind");
        let b64 = blinder.blinded_b64();
        assert!(!b64.is_empty());
        // Cached — repeated reads must be stable.
        assert_eq!(b64, blinder.blinded_b64());
    }

    #[test]
    fn blind_rejects_empty_after_normalisation() {
        match OprfBlinder::blind("   @   ".to_string()) {
            Err(FfiError::UsernameEmpty(_)) => {},
            Err(other) => panic!("expected UsernameEmpty, got {other:?}"),
            Ok(_) => panic!("expected empty-username rejection"),
        }
    }

    #[test]
    fn validate_username_surfaces_each_reason_as_its_variant() {
        assert_eq!(validate_username("@Bob_42".to_string()).unwrap(), "bob_42");
        assert!(matches!(
            validate_username("@".to_string()),
            Err(FfiError::UsernameEmpty(_))
        ));
        assert!(matches!(
            validate_username("al".to_string()),
            Err(FfiError::UsernameTooShort(_))
        ));
        assert!(matches!(
            validate_username("a".repeat(33)),
            Err(FfiError::UsernameTooLong(_))
        ));
        assert!(matches!(
            validate_username("p\u{430}vel".to_string()),
            Err(FfiError::UsernameInvalidCharacters(_))
        ));
    }

    #[test]
    fn blind_surfaces_the_username_rule_refusal() {
        match OprfBlinder::blind("ali-ce".to_string()) {
            Err(FfiError::UsernameInvalidCharacters(msg)) => {
                assert!(!msg.contains("ali"), "the message must not echo the input");
            },
            Err(other) => panic!("expected UsernameInvalidCharacters, got {other:?}"),
            Ok(_) => panic!("expected a refusal"),
        }
        // Channel handles keep their own policy.
        assert!(OprfBlinder::blind_channel("my-channel".to_string()).is_ok());
    }

    #[test]
    fn normalized_username_surfaces_canonical_form() {
        let blinder = OprfBlinder::blind("@Eve".to_string()).expect("blind");
        assert_eq!(blinder.normalized_username().expect("getter"), "eve");
    }

    /// A second call to `finalize` (simulated by manually
    /// draining the inner `Option<…>`) MUST refuse rather than
    /// silently corrupt OPRF math. Driving a real `finalize`
    /// against the math needs an `OprfServer`, exercised in the
    /// underlying crate's tests.
    #[test]
    fn finalize_is_single_use_sentinel() {
        let blinder = OprfBlinder::blind("alice".to_string()).expect("blind");
        let _ = blinder.inner.lock().expect("lock").take();
        match blinder.finalize(String::new()) {
            Err(FfiError::DiscoveryAlreadyFinalized) => {},
            Err(other) => panic!("expected DiscoveryAlreadyFinalized, got {other:?}"),
            Ok(_) => panic!("expected error on second finalize"),
        }
        // The `normalized_username` getter also surfaces the
        // already-finalized sentinel — verify.
        match blinder.normalized_username() {
            Err(FfiError::DiscoveryAlreadyFinalized) => {},
            other => panic!("expected DiscoveryAlreadyFinalized, got {other:?}"),
        }
    }
}
