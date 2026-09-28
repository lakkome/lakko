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
    is_name_term as inner_is_name_term, login_handle as inner_login_handle,
    name_matches_query as inner_name_matches_query, name_query_term as inner_name_query_term,
    name_search_terms as inner_name_search_terms,
    normalize_name_for_search as inner_normalize_name_for_search,
    normalize_username as inner_normalize, validate_username as inner_validate,
    DiscoveryClientError, OprfBlinder as InnerBlinder, UsernameError, MAX_NAME_TERMS,
    NAME_INPUT_MAX_BYTES, NAME_SEARCH_MIN_CHARS, NAME_TERM_MAX_CHARS, NAME_TERM_MIN_CHARS,
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

/// The login handle for a typed username (ADR-0054 S5): normalise, apply
/// the username rule, then `BLAKE3-derive-key("lakko/auth/login-handle/v1",
/// canonical)`. 32 bytes; sent base64url (no padding) as `handle` to the
/// pre-session sign-in and recovery routes of `services/auth`, and bound to
/// the signed-in account with `PUT /auth/login-handle`.
///
/// # Errors
///
/// Throws the same `Error` as [`validate_username`], with its
/// `usernameError` code property.
#[wasm_bindgen(js_name = loginHandle)]
pub fn login_handle(username: &str) -> Result<Vec<u8>, JsValue> {
    inner_login_handle(username)
        .map(|h| h.to_vec())
        .map_err(username_error_value)
}

// ============================================================================
// Name search — ADR-0058 §5 (word prefixes)
// ============================================================================

/// Normalise a display name or a typed query for search (NFKD, lowercase,
/// marks dropped, whitespace collapsed, trimmed). `undefined` when the input
/// is not searchable: over 1024 bytes, or under 2 characters normalised.
#[wasm_bindgen(js_name = normalizeNameForSearch)]
#[must_use]
pub fn normalize_name_for_search(input: &str) -> Option<String> {
    inner_normalize_name_for_search(input)
}

/// The search terms a display name publishes or seals under: every prefix of
/// 3 to 16 characters of every normalised word (a shorter word whole), at
/// most 64, in order. Blind each with `OprfBlinder.blindNameTerm`.
#[wasm_bindgen(js_name = nameSearchTerms)]
#[must_use]
pub fn name_search_terms(display_name: &str) -> Vec<String> {
    inner_name_search_terms(display_name)
}

/// The one term a typed query looks up: its longest normalised word, cut to
/// 16 characters. `undefined` when the query is not searchable.
#[wasm_bindgen(js_name = nameQueryTerm)]
#[must_use]
pub fn name_query_term(query: &str) -> Option<String> {
    inner_name_query_term(query)
}

/// The post-filter a lookup's candidates pass through: every word of the
/// query is a prefix of some word of the name.
#[wasm_bindgen(js_name = nameMatchesQuery)]
#[must_use]
pub fn name_matches_query(display_name: &str, query: &str) -> bool {
    inner_name_matches_query(display_name, query)
}

/// Whether `term` is a name term: 1 to 16 characters, no whitespace, already
/// normalised.
#[wasm_bindgen(js_name = isNameTerm)]
#[must_use]
pub fn is_name_term(term: &str) -> bool {
    inner_is_name_term(term)
}

/// Fewest characters a normalised query needs to be searched: 2.
#[wasm_bindgen(js_name = nameSearchMinChars)]
#[must_use]
pub fn name_search_min_chars() -> usize {
    NAME_SEARCH_MIN_CHARS
}

/// Longest input the normaliser accepts, in bytes: 1024.
#[wasm_bindgen(js_name = nameInputMaxBytes)]
#[must_use]
pub fn name_input_max_bytes() -> usize {
    NAME_INPUT_MAX_BYTES
}

/// Shortest prefix a word publishes as a term: 3.
#[wasm_bindgen(js_name = nameTermMinChars)]
#[must_use]
pub fn name_term_min_chars() -> usize {
    NAME_TERM_MIN_CHARS
}

/// Longest term: 16.
#[wasm_bindgen(js_name = nameTermMaxChars)]
#[must_use]
pub fn name_term_max_chars() -> usize {
    NAME_TERM_MAX_CHARS
}

/// Most name terms one person publishes: 64.
#[wasm_bindgen(js_name = maxNameTerms)]
#[must_use]
pub fn max_name_terms() -> usize {
    MAX_NAME_TERMS
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

    /// Blind one name term (ADR-0058 §5) in the name OPRF namespace, apart
    /// from usernames and channel handles. A publisher blinds each of
    /// `nameSearchTerms`; `normalizedUsername` returns the term.
    ///
    /// # Errors
    ///
    /// Throws when `term` is not a name term (`isNameTerm`), or on an OPRF
    /// blinding failure.
    #[wasm_bindgen(js_name = blindNameTerm)]
    pub fn blind_name_term(term: &str) -> Result<OprfBlinder, JsError> {
        let (blinder, blinded_b64) = InnerBlinder::blind_name_term(term).map_err(discovery_err)?;
        Ok(Self {
            inner: Mutex::new(Some(blinder)),
            blinded_b64,
        })
    }

    /// Blind the one term a typed name query looks up (`nameQueryTerm` of
    /// `query`). Filter what the lookup returns with `nameMatchesQuery`.
    ///
    /// # Errors
    ///
    /// Throws when the query is not searchable, or on an OPRF blinding
    /// failure.
    #[wasm_bindgen(js_name = blindName)]
    pub fn blind_name(query: &str) -> Result<OprfBlinder, JsError> {
        let (blinder, blinded_b64) = InnerBlinder::blind_name(query).map_err(discovery_err)?;
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
    fn name_search_rules_cross_the_boundary() {
        assert_eq!(
            normalize_name_for_search("  Anna  W\u{e9}ber "),
            Some("anna weber".to_string())
        );
        assert_eq!(normalize_name_for_search("a"), None);
        assert_eq!(
            name_search_terms("Anna Weber"),
            ["ann", "anna", "web", "webe", "weber"].map(String::from)
        );
        assert_eq!(name_query_term("anna weber"), Some("weber".to_string()));
        assert!(name_matches_query("Anna Weber", "we an"));
        assert!(!name_matches_query("Anna Weber", "ber"));
        assert!(is_name_term("ann") && !is_name_term("Ann"));
        assert_eq!(
            (
                name_search_min_chars(),
                name_input_max_bytes(),
                name_term_min_chars(),
                name_term_max_chars(),
                max_name_terms()
            ),
            (2, 1024, 3, 16, 64)
        );
        // Success paths only: a throw builds a `JsError`, which panics off
        // wasm32; the refusals are `lakko-discovery-client`'s.
        let b = OprfBlinder::blind_name("Anna  W\u{e9}ber").expect("blind");
        assert_eq!(b.normalized_username().expect("getter"), "weber");
        let t = OprfBlinder::blind_name_term("ann").expect("blind term");
        assert_eq!(t.normalized_username().expect("getter"), "ann");
    }

    #[test]
    fn normalize_username_helper_passes_through() {
        assert_eq!(normalize_username("@Alice"), "alice");
        assert_eq!(normalize_username("\t  bob  \n"), "bob");
    }
}
