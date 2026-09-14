// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen exposure of `lakko-discovery-client`.
//!
//! Two surfaces:
//!
//!   * [`OprfBlinder`] — the JS handle that drives the
//!     `/discovery/oprf` round-trip. Static `blind(username)`
//!     normalises the input and produces the base64 blinded
//!     element; `finalize(evaluatedB64)` consumes the server's
//!     evaluation and returns the 64-byte OPRF hash that goes
//!     into `/discovery/{register, lookup}` as `oprf_hash`.
//!
//!   * [`validateUsername`] — the ADR-0048 §4 username rule
//!     (normalise, then ASCII `[a-z0-9_]`, 3 to 32 characters). Throws
//!     an `Error` carrying a stable `usernameError` code property
//!     (`"empty"` / `"tooShort"` / `"tooLong"` / `"invalidCharacters"`).
//!     `OprfBlinder.blind` runs the same rule and throws the same way,
//!     so a non-conforming name can be neither registered nor looked up.
//!
//!   * [`normalizeUsername`] — pure helper exposed for
//!     UI-side use (display the canonical form back to the user
//!     before submission, run client-side dedup of the contact
//!     list, etc.). Single source of truth — JS never re-implements
//!     the normalisation rules.

use lakko_discovery_client::{
    normalize_username as inner_normalize, validate_username as inner_validate,
    DiscoveryClientError, OprfBlinder as InnerBlinder, UsernameError, OPRF_HASH_LEN,
};
use std::sync::Mutex;
use wasm_bindgen::prelude::*;

/// Map a `DiscoveryClientError` to a `JsError`. Same elision
/// policy as the underlying error: we keep the JS message terse
/// and avoid hinting at which structural failure occurred.
fn discovery_err(e: DiscoveryClientError) -> JsError {
    JsError::new(&format!("{e}"))
}

/// The value thrown for a username that fails the ADR-0048 §4 rule: a real
/// `Error` (so `instanceof Error` and `.message` behave as for every other
/// throw here) carrying the stable reason as an own `usernameError`
/// property. A client selects copy from `err.usernameError`, never from the
/// message; the message never contains the input.
pub(crate) fn username_error_value(reason: UsernameError) -> JsValue {
    let err = js_sys::Error::new(&reason.to_string());
    // Best-effort, as in `mls.rs`: a failed property write must not replace
    // the error being reported.
    let _ = js_sys::Reflect::set(
        err.as_ref(),
        &JsValue::from_str("usernameError"),
        &JsValue::from_str(reason.code()),
    );
    err.into()
}

/// Map a blinder failure to the thrown value: the typed username refusal
/// keeps its code, everything else is a plain error with the same message
/// style as before.
fn blind_err(e: DiscoveryClientError) -> JsValue {
    match e {
        DiscoveryClientError::InvalidUsername(reason) => username_error_value(reason),
        other => js_sys::Error::new(&other.to_string()).into(),
    }
}

fn already_finalized() -> JsError {
    JsError::new("OPRF blinder already finalized")
}

fn lock_poisoned() -> JsError {
    JsError::new("internal: OPRF blinder lock poisoned")
}

/// Canonical username form — same transformation the OPRF
/// blinder applies internally. Useful for UI confirmation
/// ("you're looking up @alice") and for client-side dedup of the
/// contact list.
///
/// Pure function; idempotent.
#[wasm_bindgen(js_name = normalizeUsername)]
#[must_use]
pub fn normalize_username(input: &str) -> String {
    inner_normalize(input)
}

/// Normalise `input` and require the ADR-0048 §4 username rule — ASCII
/// `[a-z0-9_]`, 3 to 32 characters. Returns the normalised form.
///
/// For signup and search fields as the user types: show the reason, and
/// never send a lookup for a name that fails.
///
/// # Errors
///
/// Throws an `Error` whose `usernameError` property is `"empty"`,
/// `"tooShort"`, `"tooLong"` or `"invalidCharacters"`.
#[wasm_bindgen(js_name = validateUsername)]
pub fn validate_username(input: &str) -> Result<String, JsValue> {
    inner_validate(input).map_err(username_error_value)
}

/// Client-side OPRF blinder — JS handle.
///
/// JS usage:
///
/// ```js
/// import { OprfBlinder } from "@lakko/ffi-wasm";
///
/// // 1. Blind the user-typed handle.
/// const blinder = OprfBlinder.blind("@Alice");
///
/// // 2. Round-trip through the server.
/// const { evaluated } = await fetch("/api/discovery/oprf", {
///   method: "POST",
///   headers: { "content-type": "application/json" },
///   body: JSON.stringify({ pow, blinded: blinder.blindedB64 }),
/// }).then((r) => r.json());
///
/// // 3. Finalize → 64-byte hash for /discovery/{register, lookup}.
/// const oprfHash = blinder.finalize(evaluated); // Uint8Array(64)
/// ```
#[wasm_bindgen]
pub struct OprfBlinder {
    /// Same `Mutex<Option<…>>` consume-on-finish pattern as the
    /// OPAQUE wrappers in `auth.rs`. The inner `OprfBlinder`'s
    /// `finalize` takes `self`; wasm-bindgen `&self` methods can
    /// only `take()` the slot.
    inner: Mutex<Option<InnerBlinder>>,
    blinded_b64: String,
}

#[wasm_bindgen]
impl OprfBlinder {
    /// Normalise the typed username and produce a fresh blinded
    /// element. The `blindedB64` getter then exposes the base64
    /// string for the `/discovery/oprf` request body.
    ///
    /// # Errors
    ///
    /// Throws an `Error` with a `usernameError` code property when the
    /// name fails the ADR-0048 §4 rule (empty after normalisation
    /// included), or a plain `Error` if the underlying OPRF blinding fails
    /// (extremely unlikely — would indicate a broken RNG).
    #[wasm_bindgen(js_name = blind)]
    pub fn blind(username: &str) -> Result<OprfBlinder, JsValue> {
        let (blinder, blinded_b64) = InnerBlinder::blind(username).map_err(blind_err)?;
        Ok(Self {
            inner: Mutex::new(Some(blinder)),
            blinded_b64,
        })
    }

    /// Domain-separated `@channel` variant of [`Self::blind`]. Blinds
    /// the channel namespace's OPRF input (`CHANNEL_OPRF_DOMAIN ‖
    /// normalise(handle)`), so a channel handle never produces the same
    /// OPRF hash as the identical string used as a `@username`
    /// (lakko-security channels review, Finding 2). Both the channel
    /// register and lookup paths MUST blind with this method so their
    /// hashes agree.
    ///
    /// # Errors
    ///
    /// Throws on an empty-after-normalisation handle or an OPRF blinding
    /// failure. The ADR-0048 §4 username rule is NOT applied: `@channel`
    /// handles are a separate namespace with their own policy.
    #[wasm_bindgen(js_name = blindChannel)]
    pub fn blind_channel(handle: &str) -> Result<OprfBlinder, JsError> {
        let (blinder, blinded_b64) = InnerBlinder::blind_channel(handle).map_err(discovery_err)?;
        Ok(Self {
            inner: Mutex::new(Some(blinder)),
            blinded_b64,
        })
    }

    /// Base64-encoded blinded element — the `blinded` field of
    /// `/discovery/oprf`'s request body.
    #[wasm_bindgen(getter, js_name = blindedB64)]
    pub fn blinded_b64(&self) -> String {
        self.blinded_b64.clone()
    }

    /// The normalised form of the typed username. Surface in the
    /// UI ("Looking up @alice…") so the user sees what's actually
    /// being queried.
    #[wasm_bindgen(getter, js_name = normalizedUsername)]
    pub fn normalized_username(&self) -> Result<String, JsError> {
        let guard = self.inner.lock().map_err(|_| lock_poisoned())?;
        match guard.as_ref() {
            Some(b) => Ok(b.normalised_username().to_string()),
            None => Err(already_finalized()),
        }
    }

    /// Finalise the OPRF dance with the server's evaluation.
    /// Returns the 64-byte OPRF hash as a `Uint8Array`.
    ///
    /// Single-use — a second call returns an error.
    pub fn finalize(&self, evaluated_b64: &str) -> Result<Vec<u8>, JsError> {
        let mut guard = self.inner.lock().map_err(|_| lock_poisoned())?;
        let blinder = guard.take().ok_or_else(already_finalized)?;
        let hash = blinder.finalize(evaluated_b64).map_err(discovery_err)?;
        debug_assert_eq!(hash.len(), OPRF_HASH_LEN);
        Ok(hash.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Host-side smoke. Error paths are covered by
    /// `lakko-discovery-client`'s own tests; here we only verify
    /// the wrapper's plumbing (`JsError::new` is a wasm-bindgen
    /// import stubbed off-wasm32 and panics if hit, same pattern
    /// noted in the rest of `lakko-ffi-wasm`).
    #[test]
    fn blinded_b64_is_stable_after_construction() {
        let blinder = OprfBlinder::blind("@Alice").expect("blind");
        let a = blinder.blinded_b64();
        let b = blinder.blinded_b64();
        assert_eq!(a, b);
        assert!(!a.is_empty());
    }

    #[test]
    fn normalized_username_is_canonical() {
        let blinder = OprfBlinder::blind("  @Alice  ").expect("blind");
        let normalised = blinder.normalized_username().expect("getter");
        assert_eq!(normalised, "alice");
    }

    /// Success path only: the throw path builds a `js_sys::Error`, which is
    /// a wasm-bindgen import that panics off wasm32. The reasons themselves
    /// are covered in `lakko-discovery-client` and `lakko-contacts`.
    #[test]
    fn validate_username_returns_the_normalised_form() {
        assert_eq!(validate_username("@Bob_42").expect("valid"), "bob_42");
    }

    #[test]
    fn normalize_username_helper_passes_through() {
        assert_eq!(normalize_username("@Alice"), "alice");
        assert_eq!(normalize_username("\t  bob  \n"), "bob");
    }
}
