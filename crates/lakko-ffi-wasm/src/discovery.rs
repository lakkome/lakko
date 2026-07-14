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
//!   * [`normalizeUsername`] — pure helper exposed for
//!     UI-side use (display the canonical form back to the user
//!     before submission, run client-side dedup of the contact
//!     list, etc.). Single source of truth — JS never re-implements
//!     the normalisation rules.

use lakko_discovery_client::{
    normalize_username as inner_normalize, DiscoveryClientError, OprfBlinder as InnerBlinder,
    OPRF_HASH_LEN,
};
use std::sync::Mutex;
use wasm_bindgen::prelude::*;

/// Map a `DiscoveryClientError` to a `JsError`. Same elision
/// policy as the underlying error: we keep the JS message terse
/// and avoid hinting at which structural failure occurred.
fn discovery_err(e: DiscoveryClientError) -> JsError {
    JsError::new(&format!("{e}"))
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
    /// Throws if the input reduces to an empty string after
    /// normalisation, or if the underlying OPRF blinding fails
    /// (extremely unlikely — would indicate a broken RNG).
    #[wasm_bindgen(js_name = blind)]
    pub fn blind(username: &str) -> Result<OprfBlinder, JsError> {
        let (blinder, blinded_b64) = InnerBlinder::blind(username).map_err(discovery_err)?;
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
    /// Same as [`Self::blind`] — throws on an empty-after-normalisation
    /// handle or an OPRF blinding failure.
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

    #[test]
    fn normalize_username_helper_passes_through() {
        assert_eq!(normalize_username("@Alice"), "alice");
        assert_eq!(normalize_username("\t  bob  \n"), "bob");
    }
}
