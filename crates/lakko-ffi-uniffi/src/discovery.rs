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
//!     the server's `evaluated` response to obtain an [`OprfResult`]:
//!     the OPRF output, which stays on the device, and its lookup key,
//!     which `/discovery/lookup` / `/discovery/register` consume as
//!     `oprf_hash`.
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
    discovery_lookup_key as inner_lookup_key, is_name_term as inner_is_name_term,
    login_handle as inner_login_handle, name_matches_query as inner_name_matches_query,
    name_query_term as inner_name_query_term, name_search_terms as inner_name_search_terms,
    normalize_name_for_search as inner_normalize_name_for_search,
    normalize_username as inner_normalize, validate_username as inner_validate,
    DiscoveryClientError, OprfBlinder as InnerBlinder, MAX_NAME_TERMS, NAME_INPUT_MAX_BYTES,
    NAME_SEARCH_MIN_CHARS, NAME_TERM_MAX_CHARS, NAME_TERM_MIN_CHARS, OPRF_HASH_LEN,
};

use crate::FfiError;

/// What one OPRF round yields ([ADR-0060] section 5).
///
/// Two values on purpose, so each call site names the one it uses:
/// `lookup_key` is the `oprf_hash` of every discovery request (register,
/// lookup, name register and lookup, channel handles) and what a username
/// certificate binds; `output` never leaves the device, because a public
/// record and an introduction record are sealed under keys derived from it.
///
/// [ADR-0060]: ../../../docs/adr/0060-scoped-discovery.md
#[derive(Debug, Clone, uniffi::Record)]
pub struct OprfResult {
    /// The 64-byte OPRF output. Never send it to the server.
    pub output: Vec<u8>,
    /// The 64-byte lookup key derived from `output`.
    pub lookup_key: Vec<u8>,
}

/// The lookup key of an OPRF output ([ADR-0060] section 5), for a caller
/// that kept an output from an earlier round.
///
/// # Errors
///
/// [`FfiError::Discovery`] when `output` is not 64 bytes.
///
/// [ADR-0060]: ../../../docs/adr/0060-scoped-discovery.md
#[uniffi::export]
pub fn discovery_lookup_key(output: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    let output = <[u8; OPRF_HASH_LEN]>::try_from(output.as_slice()).map_err(|_| {
        FfiError::Discovery(format!(
            "an OPRF output must be {OPRF_HASH_LEN} bytes, got {}",
            output.len()
        ))
    })?;
    Ok(inner_lookup_key(&output).to_vec())
}

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

/// The login handle for a typed username (ADR-0054 S5): normalise, apply
/// the username rule, then `BLAKE3-derive-key("lakko/auth/login-handle/v1",
/// canonical)`. 32 bytes; sent base64url (no padding) as `handle` to the
/// pre-session sign-in and recovery routes of `services/auth`, and bound to
/// the signed-in account with `PUT /auth/login-handle`.
///
/// # Errors
///
/// * [`FfiError::UsernameEmpty`] / [`FfiError::UsernameTooShort`] /
///   [`FfiError::UsernameTooLong`] / [`FfiError::UsernameInvalidCharacters`]
///   — the same reasons as [`validate_username`].
#[uniffi::export]
pub fn login_handle(username: String) -> Result<Vec<u8>, FfiError> {
    inner_login_handle(&username)
        .map(|h| h.to_vec())
        .map_err(Into::into)
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

// ============================================================================
// Name search — ADR-0060 §5 (word prefixes)
// ============================================================================

/// Normalise a display name or a typed query for search (NFKD, lowercase,
/// marks dropped, whitespace collapsed, trimmed). `None` when the input is
/// not searchable: over 1024 bytes, or under 2 characters normalised.
#[uniffi::export]
#[must_use]
pub fn normalize_name_for_search(input: String) -> Option<String> {
    inner_normalize_name_for_search(&input)
}

/// The search terms a display name publishes or seals under: every prefix
/// of 3 to 16 characters of every normalised word (a shorter word whole), at
/// most 64, in order. Blind each with [`OprfBlinder::blind_name_term`].
#[uniffi::export]
#[must_use]
pub fn name_search_terms(display_name: String) -> Vec<String> {
    inner_name_search_terms(&display_name)
}

/// The one term a typed query looks up: its longest normalised word, cut to
/// 16 characters. `None` when the query is not searchable.
#[uniffi::export]
#[must_use]
pub fn name_query_term(query: String) -> Option<String> {
    inner_name_query_term(&query)
}

/// The post-filter a lookup's candidates pass through: every word of the
/// query is a prefix of some word of the name.
#[uniffi::export]
#[must_use]
pub fn name_matches_query(display_name: String, query: String) -> bool {
    inner_name_matches_query(&display_name, &query)
}

/// Whether `term` is a name term: 1 to 16 characters, no whitespace, already
/// normalised.
#[uniffi::export]
#[must_use]
pub fn is_name_term(term: String) -> bool {
    inner_is_name_term(&term)
}

/// Fewest characters a normalised query needs to be searched: 2.
#[uniffi::export]
#[must_use]
pub fn name_search_min_chars() -> u32 {
    u32::try_from(NAME_SEARCH_MIN_CHARS).unwrap_or(u32::MAX)
}

/// Longest input the normaliser accepts, in bytes: 1024.
#[uniffi::export]
#[must_use]
pub fn name_input_max_bytes() -> u32 {
    u32::try_from(NAME_INPUT_MAX_BYTES).unwrap_or(u32::MAX)
}

/// Shortest prefix a word publishes as a term: 3.
#[uniffi::export]
#[must_use]
pub fn name_term_min_chars() -> u32 {
    u32::try_from(NAME_TERM_MIN_CHARS).unwrap_or(u32::MAX)
}

/// Longest term: 16.
#[uniffi::export]
#[must_use]
pub fn name_term_max_chars() -> u32 {
    u32::try_from(NAME_TERM_MAX_CHARS).unwrap_or(u32::MAX)
}

/// Most name terms one person publishes: 64.
#[uniffi::export]
#[must_use]
pub fn max_name_terms() -> u32 {
    u32::try_from(MAX_NAME_TERMS).unwrap_or(u32::MAX)
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
/// val done = blinder.finalize(response.evaluated)
/// // done.lookupKey → oprf_hash field on /discovery/lookup
/// // done.output stays on the device (records are sealed under it)
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

    /// Blind one name term (ADR-0060 §5) in the name OPRF namespace, apart
    /// from usernames and channel handles. A publisher blinds each of
    /// [`name_search_terms`]; the finalised hash is one search term of an
    /// introduction record or a public name row. `normalized_username`
    /// returns the term.
    ///
    /// # Errors
    ///
    /// [`FfiError::Discovery`] when `term` is not a name term
    /// ([`is_name_term`]), or on an OPRF blind failure.
    #[uniffi::constructor]
    pub fn blind_name_term(term: String) -> Result<Self, FfiError> {
        let (blinder, blinded_b64) = InnerBlinder::blind_name_term(&term)?;
        Ok(Self {
            inner: Mutex::new(Some(blinder)),
            blinded_b64,
        })
    }

    /// Blind the one term a typed name query looks up ([`name_query_term`]
    /// of `query`). Filter what the lookup returns with
    /// [`name_matches_query`].
    ///
    /// # Errors
    ///
    /// [`FfiError::Discovery`] when the query is not searchable, or on an
    /// OPRF blind failure.
    #[uniffi::constructor]
    pub fn blind_name(query: String) -> Result<Self, FfiError> {
        let (blinder, blinded_b64) = InnerBlinder::blind_name(&query)?;
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
    /// Returns the OPRF output and its lookup key ([`OprfResult`]); only
    /// the lookup key goes to the server. Single-use: a second call
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
    pub fn finalize(&self, evaluated_b64: String) -> Result<OprfResult, FfiError> {
        let mut guard = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let blinder = guard.take().ok_or(FfiError::DiscoveryAlreadyFinalized)?;
        let done = blinder.finalize(&evaluated_b64)?;
        Ok(OprfResult {
            output: done.output().to_vec(),
            lookup_key: done.lookup_key().to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_search_rules_cross_the_boundary() {
        assert_eq!(
            normalize_name_for_search("  Anna  W\u{e9}ber ".to_string()),
            Some("anna weber".to_string())
        );
        assert_eq!(normalize_name_for_search("a".to_string()), None);
        let terms = name_search_terms("Anna Weber".to_string());
        assert_eq!(
            terms,
            ["ann", "anna", "web", "webe", "weber"].map(String::from)
        );
        assert_eq!(
            name_query_term("anna weber".to_string()),
            Some("weber".to_string())
        );
        assert!(name_matches_query(
            "Anna Weber".to_string(),
            "we an".to_string()
        ));
        assert!(!name_matches_query(
            "Anna Weber".to_string(),
            "ber".to_string()
        ));
        assert!(is_name_term("ann".to_string()) && !is_name_term("Ann".to_string()));
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
    }

    #[test]
    fn name_blinders_normalise_and_refuse() {
        let b = OprfBlinder::blind_name("Anna  W\u{e9}ber".to_string()).unwrap();
        assert_eq!(b.normalized_username().unwrap(), "weber");
        let t = OprfBlinder::blind_name_term("ann".to_string()).unwrap();
        assert_eq!(t.normalized_username().unwrap(), "ann");
        assert!(matches!(
            OprfBlinder::blind_name_term("Anna".to_string()),
            Err(FfiError::Discovery(_))
        ));
        assert!(matches!(
            OprfBlinder::blind_name("a".to_string()),
            Err(FfiError::Discovery(_))
        ));
    }

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
    fn login_handle_is_the_pinned_derivation_and_refuses_typed() {
        let h = login_handle("@Alice".to_string()).unwrap();
        assert_eq!(
            h,
            lakko_contacts::username::login_handle("alice")
                .unwrap()
                .to_vec()
        );
        assert!(matches!(
            login_handle("al".to_string()),
            Err(FfiError::UsernameTooShort(_))
        ));
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
