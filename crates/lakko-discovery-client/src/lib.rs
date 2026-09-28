// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! VOPRF client for `lakko-svc-discovery`.
//!
//! ## What this crate is
//!
//! The pure-Rust client side of the username-discovery flow that
//! `services/discovery` (`oprf.rs` + `registry.rs`) implements on
//! the server. The server stores `BLAKE3-keyed`-style hashes —
//! actually `OPRF(K, normalize(@username))` outputs — never
//! plaintext usernames. To register or look up a username the
//! client takes:
//!
//! ```text
//!   1. normalised = normalize_username(input)
//!   2. (state, blinded_b64) = blind(normalised)
//!   3. POST /discovery/oprf { pow, blinded: blinded_b64 }
//!         → { evaluated: evaluated_b64 }
//!   4. oprf_hash = state.finalize(normalised, evaluated_b64)
//!   5. POST /discovery/{register, lookup} { pow, account_id?, oprf_hash }
//! ```
//!
//! The server never sees `normalised` — only the blinded element.
//! Two different clients looking up the same username produce the
//! same `oprf_hash` because the server's static key `K` is the
//! same; two different clients looking up different usernames
//! produce uncorrelated 64-byte outputs.
//!
//! ## Cipher suite
//!
//! [`LakkoOprfCs`] is `voprf::Ristretto255` — Ristretto255-SHA512.
//! Same choice as the server (`services/discovery/src/oprf.rs`)
//! and the OPAQUE-PAKE in `services/auth`. Any change to the
//! cipher suite must land in the same commit on both sides.
//!
//! ## Username normalisation
//!
//! Plaintext-side normalisation is **purely client-side**. The
//! server is byte-agnostic — it stores whatever `oprf_hash` the
//! client computed. If the client and the server disagree on
//! "canonical form", lookups silently return 404 even when the
//! username is registered. We pin the canonical form here:
//!
//!   1. Strip a leading `@` if the user typed one.
//!   2. NFKC-normalise (Unicode equivalence; collapses
//!      compatibility-decomposed forms).
//!   3. Lowercase.
//!   4. Trim ASCII whitespace.
//!
//! Anything that the user types a slightly different way (e.g.
//! "Alice" vs "alice", `@bob` vs "bob", a stray space) maps to
//! the same hash. Username uniqueness is enforced at the
//! oprf-hash level, not the plaintext level — so two visually
//! confusable strings (e.g. `paypal` vs `paypaI`) are different
//! accounts — which is exactly why the character-set rule below exists.
//!
//! ## Username character set (ADR-0048 §4)
//!
//! [`validate_username`] requires the normalised form to be ASCII
//! `[a-z0-9_]`, 3 to 32 characters, so Latin / Cyrillic look-alikes
//! and dashes cannot mislead. The server cannot enforce it (it sees a
//! blinded element), so [`OprfBlinder::blind`] enforces it: a name
//! that fails is refused before any request is built, for register
//! and lookup alike. `@channel` handles ([`OprfBlinder::blind_channel`])
//! are a separate namespace with their own (unchanged) policy.
//!
//! ## Display-name search (ADR-0058 section 5)
//!
//! A name is found by word prefix, ignoring case, repeated spaces and
//! accents. [`normalize_name_for_search`] is the one normalised form,
//! [`name_search_terms`] the terms a name is published under,
//! [`name_query_term`] the one term a query looks up, and
//! [`name_matches_query`] the local filter over what comes back.
//! [`OprfBlinder::blind_name_term`] blinds a term in a third OPRF namespace,
//! domain-separated from usernames and `@channel` handles. It lives here
//! rather than in `lakko-contacts` (where the ADR places it) because it needs
//! the `unicode-normalization` tables, which the iOS notification extension
//! that links `lakko-contacts` must not carry; see the `name` module.
//!
//! ## Security notes
//!
//! * The OPRF blinds the username, not the request envelope. The
//!   server STILL learns "someone is asking about a username"
//!   and the timing thereof. PoW + per-IP rate limits at the
//!   nginx layer are the production defences against enumeration.
//! * The crate intentionally does NOT speak HTTP. The `apps/web`
//!   layer (1.16d2) composes the JSON request bodies, attaches a
//!   PoW proof, and posts via `fetch`.
//! * The crate intentionally does NOT solve the Equix PoW the
//!   server demands — that's `lakko-pow` (1.16c3a).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod blinder;
mod error;
mod name;
mod normalize;

pub use blinder::{OprfBlinder, OPRF_HASH_LEN};
pub use error::DiscoveryClientError;
pub use lakko_contacts::username::{UsernameError, USERNAME_MAX_LEN, USERNAME_MIN_LEN};
pub use name::{
    is_name_term, name_matches_query, name_query_term, name_search_terms,
    normalize_name_for_search, MAX_NAME_TERMS, NAME_INPUT_MAX_BYTES, NAME_SEARCH_MIN_CHARS,
    NAME_TERM_MAX_CHARS, NAME_TERM_MIN_CHARS,
};
pub use normalize::{login_handle, normalize_username, validate_username};

/// Lakko VOPRF cipher suite — Ristretto255-SHA512.
///
/// Re-exported as a type alias so callers (and tests) can talk
/// about the exact same suite the server uses without depending
/// on `voprf` directly. MUST equal
/// `services/discovery/src/oprf.rs::LakkoOprfCs`.
pub type LakkoOprfCs = voprf::Ristretto255;
