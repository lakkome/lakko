// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Client-side OPRF blinder.
//!
//! Two-step API mirroring the wire protocol:
//!
//!   1. [`OprfBlinder::blind`] — normalise the typed username,
//!      refuse it unless it passes the ADR-0048 §4 character-set
//!      rule ([`crate::validate_username`]), blind it via
//!      `voprf::OprfClient::blind`, hand back the base64-encoded
//!      `BlindedElement` for the `/discovery/oprf` request body.
//!
//! ## The blinder is the username chokepoint
//!
//! Registration and lookup both need an OPRF hash, and the only way a
//! client obtains one is through [`OprfBlinder::blind`]. So refusing
//! here refuses both, by construction, without either call site
//! remembering to check — and the server, which only ever sees a
//! blinded element, could not check even if asked. A modified client
//! can still blind a non-conforming string with raw `voprf`; no
//! conforming client can then find or display it.
//!
//!   2. [`OprfBlinder::finalize`] — consume the server's
//!      `EvaluationElement`, run `OprfClient::finalize`, and
//!      return the 64-byte OPRF output that goes into
//!      `/discovery/{register, lookup}` as `oprf_hash`.
//!
//! [`OprfBlinder`] holds the normalised username internally so
//! callers cannot accidentally hand a different string to
//! `finalize` than the one they blinded — that miswiring would
//! produce nonsense output without any error signal.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use rand::rngs::OsRng;
use voprf::{EvaluationElement, OprfClient};

use crate::error::DiscoveryClientError;
use crate::name::{is_name_term, name_query_term};
use crate::normalize::{normalize_username, validate_username};
use crate::LakkoOprfCs;

/// VOPRF Ristretto255-SHA512 produces a 64-byte output (one
/// SHA-512 digest). Pinned here as a `const` so callers (and the
/// server side, which uses the same constant) stay in lock-step.
pub const OPRF_HASH_LEN: usize = 64;

/// Domain-separation prefix for the `@channel` OPRF namespace
/// (lakko-security channels review, Finding 2).
///
/// A `@channel` handle is blinded as
/// `CHANNEL_OPRF_DOMAIN ‖ normalize_username(handle)` instead of the
/// bare normalised bytes a `@username` blinds. Because the discovery
/// service applies the SAME OPRF key to both namespaces, two identical
/// handles would otherwise produce an identical OPRF hash across the
/// separate username / channel tables — an operator holding the key
/// (or diffing the tables) could observe that the same handle exists
/// in both namespaces.
///
/// The leading `0xFF` byte is load-bearing: it can NEVER begin a
/// valid-UTF-8 username input (`normalize_username` always returns a
/// `String`, so its bytes start in `0x00..=0x7F` or `0xC2..=0xF4`).
/// So no `@username` can forge a `@channel` input — the separation is
/// injective, not merely a string prefix a crafted username could
/// reproduce. The trailing `0x1F` (ASCII unit separator) delimits the
/// domain label from the handle.
const CHANNEL_OPRF_DOMAIN: &[u8] = b"\xfflakko/discovery/channel/v1\x1f";

/// Domain-separation prefix for the display-name OPRF namespace
/// ([ADR-0058] section 5), the same shape as [`CHANNEL_OPRF_DOMAIN`].
///
/// A name is blinded as `NAME_OPRF_DOMAIN ‖ normalize_name_for_search(name)`,
/// so the same string used as a username, a channel handle and a name gives
/// three unrelated OPRF outputs under the one server key. The leading `0xFF`
/// keeps it unforgeable by any username (whose input is valid UTF-8), and the
/// label differs from the channel domain's, so no channel handle can reproduce
/// a name input either.
///
/// [ADR-0058]: ../../../docs/adr/0058-scoped-discovery.md
const NAME_OPRF_DOMAIN: &[u8] = b"\xfflakko/discovery/name/v1\x1f";

/// In-flight OPRF blinding state. Single-use: `finalize` consumes
/// the value to enforce one blind / one finalize per query.
///
/// `Debug` formats the OPRF-input length only — the blinding scalar
/// inside `state` is sensitive (recovers the plaintext from the
/// server's evaluation), so the derive surfaces just enough to be
/// useful in a `?` panic.
pub struct OprfBlinder {
    state: OprfClient<LakkoOprfCs>,
    /// The exact OPRF input bytes — bare normalised bytes for a
    /// `@username`, or `CHANNEL_OPRF_DOMAIN ‖ normalised` for a
    /// `@channel`. Cached so callers can't mismatch input between
    /// `blind` and `finalize` — a class of bug that produces silent
    /// wrong-hash output. Not necessarily valid UTF-8 (the channel
    /// namespace prepends a `0xFF` byte), so display uses `display`.
    oprf_input: Vec<u8>,
    /// The display-normalised handle (always valid UTF-8), for
    /// `normalised_username()` — the human form without the OPRF
    /// domain prefix.
    display: String,
}

impl core::fmt::Debug for OprfBlinder {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Print the length only — the blinding state is
        // sensitive, and the normalised username is
        // partially-recoverable secret (lookup target).
        // `finish_non_exhaustive` documents that we are
        // intentionally hiding fields (clippy::missing_fields_in_debug).
        f.debug_struct("OprfBlinder")
            .field("oprf_input_len", &self.oprf_input.len())
            .finish_non_exhaustive()
    }
}

impl OprfBlinder {
    /// Normalise the typed username, require it to pass the
    /// ADR-0048 §4 username rule, and produce a fresh blinded element.
    /// Returns `(blinder, blinded_b64)`.
    ///
    /// The base64 string drops straight into the `blinded` field
    /// of `/discovery/oprf`'s request body. Both `/discovery/register`
    /// and `/discovery/lookup` consume the hash this produces, so a
    /// name refused here can be neither registered nor looked up.
    ///
    /// # Errors
    ///
    /// * [`DiscoveryClientError::InvalidUsername`] — the normalised
    ///   input is empty (e.g. `"@"`, `"   "`), shorter than 3 or longer
    ///   than 32 characters, or contains a character outside
    ///   `[a-z0-9_]`. Carries the reason code, never the input.
    /// * [`DiscoveryClientError::Voprf`] — `voprf` rejected the
    ///   blind (extremely unlikely on a sane RNG; treat as fatal).
    pub fn blind(username: &str) -> Result<(Self, String), DiscoveryClientError> {
        let normalised = validate_username(username)?;
        // @username namespace: the OPRF input is the bare normalised
        // bytes — unchanged wire, so existing registrations stay valid.
        let oprf_input = normalised.as_bytes().to_vec();
        Self::blind_input(normalised, oprf_input)
    }

    /// Normalise the typed channel handle and produce a fresh blinded
    /// element in the DOMAIN-SEPARATED `@channel` namespace — the OPRF
    /// input is `CHANNEL_OPRF_DOMAIN ‖ normalize_username(handle)`, so
    /// the resulting OPRF hash never collides with the same handle used
    /// as a `@username` (lakko-security channels review, Finding 2).
    /// Same `(blinder, blinded_b64)` shape + same `/discovery/oprf`
    /// round-trip as [`Self::blind`]; the caller finalises with
    /// [`Self::finalize`] and both register + lookup MUST use this
    /// method so their hashes match.
    ///
    /// # Errors
    ///
    /// [`DiscoveryClientError::EmptyUsername`] when the handle reduces to
    /// empty, or [`DiscoveryClientError::Voprf`] on a `voprf` blind failure.
    ///
    /// **The ADR-0048 §4 username character-set rule is NOT applied
    /// here.** A `@channel` handle is a separate namespace (its own
    /// OPRF domain, its own table) whose policy that ADR does not set;
    /// applying the username rule to it would be a silent policy change
    /// for channels. Handles keep the pre-ADR-0048 behaviour: normalise,
    /// refuse empty.
    pub fn blind_channel(handle: &str) -> Result<(Self, String), DiscoveryClientError> {
        let normalised = normalize_username(handle);
        if normalised.is_empty() {
            return Err(DiscoveryClientError::EmptyUsername);
        }
        let mut oprf_input = Vec::with_capacity(CHANNEL_OPRF_DOMAIN.len() + normalised.len());
        oprf_input.extend_from_slice(CHANNEL_OPRF_DOMAIN);
        oprf_input.extend_from_slice(normalised.as_bytes());
        Self::blind_input(normalised, oprf_input)
    }

    /// Blind one name term ([ADR-0058] section 5) in the DOMAIN-SEPARATED
    /// name namespace: the OPRF input is `NAME_OPRF_DOMAIN ‖ term`.
    ///
    /// A publisher blinds each of [`crate::name_search_terms`]; a searcher
    /// blinds [`crate::name_query_term`] (or calls [`Self::blind_name`]). Both
    /// derive the term from the same module, so a query term and the
    /// published term it equals give one OPRF input. Finalise with
    /// [`Self::finalize`]; [`Self::normalised_username`] returns the term.
    ///
    /// # Errors
    ///
    /// [`DiscoveryClientError::InvalidName`] when `term` is not a term
    /// ([`crate::is_name_term`]: 1 to 16 characters, no whitespace, already
    /// normalised), or [`DiscoveryClientError::Voprf`] on a `voprf` blind
    /// failure.
    ///
    /// [ADR-0058]: ../../../docs/adr/0058-scoped-discovery.md
    pub fn blind_name_term(term: &str) -> Result<(Self, String), DiscoveryClientError> {
        if !is_name_term(term) {
            return Err(DiscoveryClientError::InvalidName);
        }
        let mut oprf_input = Vec::with_capacity(NAME_OPRF_DOMAIN.len() + term.len());
        oprf_input.extend_from_slice(NAME_OPRF_DOMAIN);
        oprf_input.extend_from_slice(term.as_bytes());
        Self::blind_input(term.to_owned(), oprf_input)
    }

    /// Blind the one term a name query looks up: [`crate::name_query_term`]
    /// of `query`, through [`Self::blind_name_term`]. The candidates the
    /// lookup returns are then filtered locally with
    /// [`crate::name_matches_query`].
    ///
    /// # Errors
    ///
    /// [`DiscoveryClientError::InvalidName`] when the query is not
    /// searchable (over 1024 bytes, or under 2 characters normalised), or
    /// [`DiscoveryClientError::Voprf`] on a `voprf` blind failure.
    pub fn blind_name(query: &str) -> Result<(Self, String), DiscoveryClientError> {
        let term = name_query_term(query).ok_or(DiscoveryClientError::InvalidName)?;
        Self::blind_name_term(&term)
    }

    /// Shared blind step: blind the already-constructed OPRF input
    /// bytes and build the blinder. `display` is the human-normalised
    /// handle (valid UTF-8) surfaced by [`Self::normalised_username`];
    /// `oprf_input` is the exact bytes fed to `blind` + `finalize`
    /// (domain-prefixed for `@channel`, bare for `@username`).
    fn blind_input(
        display: String,
        oprf_input: Vec<u8>,
    ) -> Result<(Self, String), DiscoveryClientError> {
        let mut rng = OsRng;
        let blind_result = OprfClient::<LakkoOprfCs>::blind(&oprf_input, &mut rng)
            .map_err(|_| DiscoveryClientError::voprf())?;

        let blinded_b64 = B64.encode(blind_result.message.serialize());
        Ok((
            Self {
                state: blind_result.state,
                oprf_input,
                display,
            },
            blinded_b64,
        ))
    }

    /// The normalised username this blinder was built for —
    /// useful when the caller wants to display a confirmation
    /// like "looking up @alice" without re-running normalisation.
    #[must_use]
    pub fn normalised_username(&self) -> &str {
        // The display form is the human-normalised handle WITHOUT the
        // `@channel` OPRF domain prefix — always valid UTF-8, so no
        // fallible decode. The domained OPRF input lives in
        // `oprf_input` (which may start with a non-UTF-8 `0xFF` byte).
        &self.display
    }

    /// Finalise the OPRF dance with the server's response.
    ///
    /// Consumes `self` so the same blinder cannot be used twice
    /// (cuts a class of replay-mistake bugs at the API).
    ///
    /// # Errors
    ///
    /// * [`DiscoveryClientError::Base64`] — `evaluated_b64` does
    ///   not decode.
    /// * [`DiscoveryClientError::InvalidWire`] — bytes decode but
    ///   are not a valid `EvaluationElement`.
    /// * [`DiscoveryClientError::Voprf`] — `voprf` rejected the
    ///   finalisation (the server returned a structurally valid
    ///   element that doesn't satisfy the OPRF math; in
    ///   non-verifiable OPRF mode the only reachable case is
    ///   wire corruption between encode and decode).
    pub fn finalize(
        self,
        evaluated_b64: &str,
    ) -> Result<[u8; OPRF_HASH_LEN], DiscoveryClientError> {
        let evaluated_bytes = B64
            .decode(evaluated_b64)
            .map_err(DiscoveryClientError::base64)?;
        let evaluated = EvaluationElement::<LakkoOprfCs>::deserialize(&evaluated_bytes)
            .map_err(|_| DiscoveryClientError::invalid_wire())?;

        let output = self
            .state
            .finalize(&self.oprf_input, &evaluated)
            .map_err(|_| DiscoveryClientError::voprf())?;

        // `voprf` returns a `GenericArray<u8, U64>`. Copy into a
        // fixed-size array so callers don't need to know the
        // type-level length.
        let slice = output.as_slice();
        debug_assert_eq!(slice.len(), OPRF_HASH_LEN);
        let mut out = [0u8; OPRF_HASH_LEN];
        out.copy_from_slice(slice);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use voprf::{BlindedElement, OprfServer};

    /// End-to-end round trip in memory: same `OprfServer` shape
    /// the discovery service uses, no DB, no HTTP. Confirms the
    /// client crate negotiates byte-for-byte with the server-side
    /// `voprf` primitives.
    #[test]
    fn round_trip_with_oprf_server() {
        // Same constructor the server uses (see
        // services/discovery/src/oprf.rs::load_or_init_server_setup).
        let seed = [0xCDu8; 32];
        let server =
            OprfServer::<LakkoOprfCs>::new_from_seed(&seed, b"lakko/oprf/v1").expect("server seed");

        // Client: blind.
        let (blinder, blinded_b64) = OprfBlinder::blind("@Alice").expect("blind");

        // Server: blind_evaluate.
        let blinded_bytes = B64.decode(&blinded_b64).expect("decode blinded");
        let blinded_elem = BlindedElement::<LakkoOprfCs>::deserialize(&blinded_bytes)
            .expect("deserialize blinded");
        let evaluated = server.blind_evaluate(&blinded_elem);
        let evaluated_b64 = B64.encode(evaluated.serialize());

        // Client: finalize.
        let hash = blinder.finalize(&evaluated_b64).expect("finalize");
        assert_eq!(hash.len(), OPRF_HASH_LEN);
    }

    /// Determinism: two clients looking up the SAME normalised
    /// username against the SAME server key produce byte-equal
    /// OPRF output, even though the per-call blinding is random.
    /// This is the property that makes username lookups work at all.
    #[test]
    fn same_username_yields_same_oprf_hash() {
        let seed = [0u8; 32];
        let server =
            OprfServer::<LakkoOprfCs>::new_from_seed(&seed, b"lakko/oprf/v1").expect("server seed");

        let hash_a = oneshot(&server, "alice");
        let hash_b = oneshot(&server, "Alice");
        let hash_c = oneshot(&server, "@ALICE");
        let hash_d = oneshot(&server, "  alice  ");

        assert_eq!(hash_a, hash_b);
        assert_eq!(hash_a, hash_c);
        assert_eq!(hash_a, hash_d);
    }

    /// Different usernames → different hashes. (Collision
    /// probability for two distinct inputs through a random
    /// oracle of 64-byte output is 2^-256, well below any
    /// empirical test threshold.)
    #[test]
    fn different_usernames_yield_different_hashes() {
        let seed = [0xAA; 32];
        let server =
            OprfServer::<LakkoOprfCs>::new_from_seed(&seed, b"lakko/oprf/v1").expect("server seed");

        let alice = oneshot(&server, "alice");
        let bob = oneshot(&server, "bob");
        assert_ne!(alice, bob);
    }

    /// Different server keys → different hashes for the same
    /// username. Pins down the "key-bound" property — a
    /// stolen oprf_hash from server A doesn't authenticate the
    /// same username on server B.
    #[test]
    fn different_server_keys_yield_different_hashes() {
        let server_a = OprfServer::<LakkoOprfCs>::new_from_seed(&[1u8; 32], b"lakko/oprf/v1")
            .expect("server a");
        let server_b = OprfServer::<LakkoOprfCs>::new_from_seed(&[2u8; 32], b"lakko/oprf/v1")
            .expect("server b");

        let hash_a = oneshot(&server_a, "alice");
        let hash_b = oneshot(&server_b, "alice");
        assert_ne!(hash_a, hash_b);
    }

    #[test]
    fn blind_rejects_empty_username() {
        for empty in ["", "@", "   "] {
            let err = OprfBlinder::blind(empty).expect_err("must fail");
            assert!(
                matches!(
                    err,
                    DiscoveryClientError::InvalidUsername(crate::UsernameError::Empty)
                ),
                "{empty:?} -> {err:?}"
            );
        }
    }

    /// ADR-0048 §4: the blinder is the chokepoint. A name that fails the
    /// username rule never yields a blinded element — so it can reach
    /// neither `/discovery/register` nor `/discovery/lookup`, which both
    /// consume only what `blind` + `finalize` produce.
    #[test]
    fn blind_refuses_names_that_fail_the_username_rule() {
        use crate::UsernameError;
        for (bad, why) in [
            ("al", UsernameError::TooShort { min: 3 }),
            (
                "abcdefghijklmnopqrstuvwxyz0123456",
                UsernameError::TooLong { max: 32 },
            ),
            ("p\u{430}vel", UsernameError::InvalidCharacters),
            ("ali-ce", UsernameError::InvalidCharacters),
            ("ali.ce", UsernameError::InvalidCharacters),
            ("ali ce", UsernameError::InvalidCharacters),
            ("\u{1F600}alice", UsernameError::InvalidCharacters),
        ] {
            let err = OprfBlinder::blind(bad).expect_err("must refuse");
            assert!(
                matches!(err, DiscoveryClientError::InvalidUsername(r) if r == why),
                "{bad:?} -> {err:?}"
            );
        }
    }

    /// The register and lookup paths are the same call, so the refusal
    /// covers both; this pins that a CONFORMING name still round-trips to
    /// one hash whichever spelling was typed (fullwidth folds via NFKC).
    #[test]
    fn register_and_lookup_agree_on_a_conforming_name() {
        let server = OprfServer::<LakkoOprfCs>::new_from_seed(&[0x31u8; 32], b"lakko/oprf/v1")
            .expect("server seed");
        let registered = oneshot(&server, "Bob_42");
        let looked_up = oneshot(&server, "@bob_42");
        let fullwidth = oneshot(&server, "\u{ff42}\u{ff4f}\u{ff42}_42");
        assert_eq!(registered, looked_up);
        assert_eq!(registered, fullwidth);
    }

    /// `@channel` handles are a separate namespace and deliberately keep
    /// their pre-ADR-0048 policy — the username rule does not leak into
    /// `blind_channel`.
    #[test]
    fn channel_handles_are_not_subject_to_the_username_rule() {
        assert!(OprfBlinder::blind("my-channel").is_err());
        assert!(OprfBlinder::blind_channel("my-channel").is_ok());
        assert!(OprfBlinder::blind_channel("ab").is_ok());
    }

    #[test]
    fn finalize_rejects_invalid_base64() {
        let (blinder, _) = OprfBlinder::blind("alice").expect("blind");
        let err = blinder
            .finalize("this is not base64!!!")
            .expect_err("must fail");
        assert!(matches!(err, DiscoveryClientError::Base64(_)));
    }

    #[test]
    fn finalize_rejects_garbage_wire_bytes() {
        let (blinder, _) = OprfBlinder::blind("alice").expect("blind");
        let garbage = B64.encode([0u8; 32]);
        let err = blinder.finalize(&garbage).expect_err("must fail");
        // Garbage that doesn't deserialise as an EvaluationElement
        // → InvalidWire.
        assert!(matches!(err, DiscoveryClientError::InvalidWire));
    }

    #[test]
    fn normalised_username_accessor_returns_canonical_form() {
        let (blinder, _) = OprfBlinder::blind("@Alice").expect("blind");
        assert_eq!(blinder.normalised_username(), "alice");
    }

    /// Helper: full client + server round-trip in one call.
    fn oneshot(server: &OprfServer<LakkoOprfCs>, username: &str) -> [u8; OPRF_HASH_LEN] {
        let (blinder, blinded_b64) = OprfBlinder::blind(username).expect("blind");
        let blinded_bytes = B64.decode(&blinded_b64).expect("decode");
        let blinded_elem =
            BlindedElement::<LakkoOprfCs>::deserialize(&blinded_bytes).expect("deserialize");
        let evaluated = server.blind_evaluate(&blinded_elem);
        let evaluated_b64 = B64.encode(evaluated.serialize());
        blinder.finalize(&evaluated_b64).expect("finalize")
    }

    /// Helper: full client + server round-trip in the
    /// domain-separated `@channel` namespace (`blind_channel`).
    fn oneshot_channel(server: &OprfServer<LakkoOprfCs>, handle: &str) -> [u8; OPRF_HASH_LEN] {
        let (blinder, blinded_b64) = OprfBlinder::blind_channel(handle).expect("blind_channel");
        let blinded_bytes = B64.decode(&blinded_b64).expect("decode");
        let blinded_elem =
            BlindedElement::<LakkoOprfCs>::deserialize(&blinded_bytes).expect("deserialize");
        let evaluated = server.blind_evaluate(&blinded_elem);
        let evaluated_b64 = B64.encode(evaluated.serialize());
        blinder.finalize(&evaluated_b64).expect("finalize")
    }

    /// Finding 2 (lakko-security channels review): the `@channel`
    /// namespace is domain-separated from `@username`, so the SAME
    /// handle produces DIFFERENT OPRF hashes in the two namespaces
    /// under the SAME server key — an operator holding the key cannot
    /// observe that a username `alice` and a channel `alice` are the
    /// same string.
    #[test]
    fn channel_namespace_is_domain_separated_from_username() {
        let seed = [0x5Au8; 32];
        let server =
            OprfServer::<LakkoOprfCs>::new_from_seed(&seed, b"lakko/oprf/v1").expect("server seed");

        let as_user = oneshot(&server, "alice");
        let as_channel = oneshot_channel(&server, "alice");
        assert_ne!(
            as_user, as_channel,
            "same handle must hash differently across the username / channel namespaces"
        );
    }

    /// Determinism inside the `@channel` namespace: two lookups of the
    /// same channel handle (any equivalent normalised form) produce a
    /// byte-equal hash, so channel lookups resolve at all.
    #[test]
    fn same_channel_handle_yields_same_hash() {
        let seed = [0x5Au8; 32];
        let server =
            OprfServer::<LakkoOprfCs>::new_from_seed(&seed, b"lakko/oprf/v1").expect("server seed");

        let a = oneshot_channel(&server, "@Alice");
        let b = oneshot_channel(&server, "  alice ");
        assert_eq!(a, b, "equivalent channel handles must resolve to one hash");
    }

    /// The channel domain prefix is UNFORGEABLE by any username. A
    /// username's OPRF input is `normalize_username(x).as_bytes()` —
    /// always valid UTF-8, whose first byte is never `0xFF`. The
    /// channel domain begins with `0xFF`, so no crafted username can
    /// reproduce a channel input; the separation is injective, not a
    /// string prefix a username like
    /// `"lakko/discovery/channel/v1<US>alice"` could forge.
    #[test]
    fn channel_domain_is_injective_against_crafted_usernames() {
        assert_eq!(CHANNEL_OPRF_DOMAIN.first().copied(), Some(0xFF));

        let seed = [0x5Au8; 32];
        let server =
            OprfServer::<LakkoOprfCs>::new_from_seed(&seed, b"lakko/oprf/v1").expect("server seed");
        // A username crafted to mimic the readable part of the channel
        // domain is now refused outright by the ADR-0048 username rule
        // (`/` and U+001F are outside `[a-z0-9_]`) — a stronger outcome
        // than the hash separation, but not a substitute for it: the
        // separation must hold on its own, since the rule is client-side.
        assert!(OprfBlinder::blind("lakko/discovery/channel/v1\u{1f}alice").is_err());
        // So prove the separation at the OPRF-input level directly: a
        // username's input is valid UTF-8 and never starts with 0xFF.
        let (user, _) = OprfBlinder::blind("alice").expect("blind");
        let (channel, _) = OprfBlinder::blind_channel("alice").expect("blind_channel");
        assert_ne!(user.oprf_input.first().copied(), Some(0xFF));
        assert!(channel.oprf_input.starts_with(CHANNEL_OPRF_DOMAIN));
        let crafted_channel = oneshot_channel(&server, "alice");
        let real_user = oneshot(&server, "alice");
        assert_ne!(crafted_channel, real_user);
    }

    fn oneshot_name(server: &OprfServer<LakkoOprfCs>, name: &str) -> [u8; OPRF_HASH_LEN] {
        let (blinder, blinded_b64) = OprfBlinder::blind_name(name).expect("blind_name");
        let blinded_bytes = B64.decode(&blinded_b64).expect("decode");
        let blinded_elem =
            BlindedElement::<LakkoOprfCs>::deserialize(&blinded_bytes).expect("deserialize");
        let evaluated = server.blind_evaluate(&blinded_elem);
        let evaluated_b64 = B64.encode(evaluated.serialize());
        blinder.finalize(&evaluated_b64).expect("finalize")
    }

    /// ADR-0058 section 5: the same string as a username, a channel handle
    /// and a name gives three different OPRF inputs and three different
    /// hashes under one server key.
    #[test]
    fn name_namespace_is_separated_from_usernames_and_channels() {
        assert_eq!(NAME_OPRF_DOMAIN.first().copied(), Some(0xFF));
        assert_ne!(NAME_OPRF_DOMAIN, CHANNEL_OPRF_DOMAIN);
        let (user, _) = OprfBlinder::blind("alice").expect("blind");
        let (channel, _) = OprfBlinder::blind_channel("alice").expect("blind_channel");
        let (name, _) = OprfBlinder::blind_name("alice").expect("blind_name");
        assert_eq!(user.oprf_input, b"alice");
        assert!(channel.oprf_input.starts_with(CHANNEL_OPRF_DOMAIN));
        assert!(name.oprf_input.starts_with(NAME_OPRF_DOMAIN));
        assert_ne!(user.oprf_input, name.oprf_input);
        assert_ne!(channel.oprf_input, name.oprf_input);

        let server = OprfServer::<LakkoOprfCs>::new_from_seed(&[0x5Bu8; 32], b"lakko/oprf/v1")
            .expect("server seed");
        let as_user = oneshot(&server, "alice");
        let as_channel = oneshot_channel(&server, "alice");
        let as_name = oneshot_name(&server, "alice");
        assert_ne!(as_name, as_user);
        assert_ne!(as_name, as_channel);
    }

    /// Equivalent spellings of a query resolve to one hash; a query whose
    /// longest word differs does not.
    #[test]
    fn equivalent_queries_yield_one_hash() {
        let server = OprfServer::<LakkoOprfCs>::new_from_seed(&[0x5Cu8; 32], b"lakko/oprf/v1")
            .expect("server seed");
        let a = oneshot_name(&server, "Anna Weber");
        assert_eq!(a, oneshot_name(&server, " anna  WEBER "));
        assert_eq!(a, oneshot_name(&server, "Anna  W\u{e9}ber"));
        assert_ne!(a, oneshot_name(&server, "anna"));
        let (blinder, _) = OprfBlinder::blind_name("Anna  W\u{e9}ber").expect("blind_name");
        assert_eq!(blinder.normalised_username(), "weber");
    }

    /// The publish / query invariant: for every term `t` a name publishes, a
    /// query whose query term is `t` produces the same OPRF input, and so the
    /// same hash under one server key.
    #[test]
    fn a_query_and_the_published_term_it_names_agree() {
        let server = OprfServer::<LakkoOprfCs>::new_from_seed(&[0x5Du8; 32], b"lakko/oprf/v1")
            .expect("server seed");
        for name in [
            "Anna W\u{e9}ber",
            "\u{41b}\u{438} \u{412}\u{44d}\u{439}",
            "J Abcdefghijklmnopqrstuvwxyzabcd",
        ] {
            let terms = crate::name_search_terms(name);
            assert!(!terms.is_empty());
            for t in &terms {
                // `t` itself as a typed query, and the same term typed in
                // capitals with an accent-free spelling, both name `t`.
                for query in [t.clone(), t.to_uppercase()] {
                    if name_query_term(&query).as_deref() != Some(t.as_str()) {
                        continue;
                    }
                    let (published, _) = OprfBlinder::blind_name_term(t).expect("publish");
                    let (queried, _) = OprfBlinder::blind_name(&query).expect("query");
                    assert_eq!(published.oprf_input, queried.oprf_input, "{t:?}");
                }
                if t.chars().count() >= 2 {
                    let hash = |term: &str| {
                        let (b, blinded_b64) = OprfBlinder::blind_name_term(term).expect("term");
                        let blinded = BlindedElement::<LakkoOprfCs>::deserialize(
                            &B64.decode(&blinded_b64).expect("decode"),
                        )
                        .expect("deserialize");
                        let evaluated = B64.encode(server.blind_evaluate(&blinded).serialize());
                        b.finalize(&evaluated).expect("finalize")
                    };
                    assert_eq!(hash(t), oneshot_name(&server, t), "{t:?}");
                }
            }
        }
    }

    #[test]
    fn blind_name_term_refuses_non_terms() {
        for bad in ["", "anna weber", "Anna", "w\u{e9}ber", &"a".repeat(17)] {
            assert!(
                matches!(
                    OprfBlinder::blind_name_term(bad),
                    Err(DiscoveryClientError::InvalidName)
                ),
                "{bad:?}"
            );
        }
        assert!(OprfBlinder::blind_name_term("j").is_ok());
    }

    #[test]
    fn blind_name_refuses_unsearchable_queries() {
        for bad in ["", "a", "  a ", &"a".repeat(1025)] {
            assert!(
                matches!(
                    OprfBlinder::blind_name(bad),
                    Err(DiscoveryClientError::InvalidName)
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn blind_channel_rejects_empty_handle() {
        assert!(matches!(
            OprfBlinder::blind_channel("").expect_err("must fail"),
            DiscoveryClientError::EmptyUsername
        ));
        assert!(matches!(
            OprfBlinder::blind_channel("@").expect_err("must fail"),
            DiscoveryClientError::EmptyUsername
        ));
    }

    #[test]
    fn channel_blinder_display_has_no_domain_prefix() {
        let (blinder, _) = OprfBlinder::blind_channel("@Alice").expect("blind_channel");
        // The human-display form is the bare normalised handle — the
        // `0xFF` domain prefix lives only in the OPRF input.
        assert_eq!(blinder.normalised_username(), "alice");
    }
}
