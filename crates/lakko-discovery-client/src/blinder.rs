// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Client-side OPRF blinder.
//!
//! Two-step API mirroring the wire protocol:
//!
//!   1. [`OprfBlinder::blind`] — normalise the typed username,
//!      blind it via `voprf::OprfClient::blind`, hand back the
//!      base64-encoded `BlindedElement` for the
//!      `/discovery/oprf` request body.
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
use crate::normalize::normalize_username;
use crate::LakkoOprfCs;

/// VOPRF Ristretto255-SHA512 produces a 64-byte output (one
/// SHA-512 digest). Pinned here as a `const` so callers (and the
/// server side, which uses the same constant) stay in lock-step.
pub const OPRF_HASH_LEN: usize = 64;

/// In-flight OPRF blinding state. Single-use: `finalize` consumes
/// the value to enforce one blind / one finalize per username
/// query.
///
/// `Debug` formats the normalised-username length only — the
/// blinding scalar inside `state` is sensitive (recovers the
/// plaintext from the server's evaluation), so the derive
/// surfaces just enough to be useful in a `?` panic.
pub struct OprfBlinder {
    state: OprfClient<LakkoOprfCs>,
    /// The normalised username bytes. Cached so callers can't
    /// mismatch input between `blind` and `finalize` — a class
    /// of bug that produces silent wrong-hash output.
    normalised: Vec<u8>,
}

impl core::fmt::Debug for OprfBlinder {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Print the length only — the blinding state is
        // sensitive, and the normalised username is
        // partially-recoverable secret (lookup target).
        // `finish_non_exhaustive` documents that we are
        // intentionally hiding fields (clippy::missing_fields_in_debug).
        f.debug_struct("OprfBlinder")
            .field("normalised_len", &self.normalised.len())
            .finish_non_exhaustive()
    }
}

impl OprfBlinder {
    /// Normalise the typed username and produce a fresh blinded
    /// element. Returns `(blinder, blinded_b64)`.
    ///
    /// The base64 string drops straight into the `blinded` field
    /// of `/discovery/oprf`'s request body.
    ///
    /// # Errors
    ///
    /// * [`DiscoveryClientError::EmptyUsername`] — the input
    ///   reduces to an empty string after normalisation
    ///   (e.g. caller passed `"@"` or `"   "`).
    /// * [`DiscoveryClientError::Voprf`] — `voprf` rejected the
    ///   blind (extremely unlikely on a sane RNG; treat as fatal).
    pub fn blind(username: &str) -> Result<(Self, String), DiscoveryClientError> {
        let normalised = normalize_username(username);
        if normalised.is_empty() {
            return Err(DiscoveryClientError::EmptyUsername);
        }
        let normalised_bytes = normalised.into_bytes();

        let mut rng = OsRng;
        let blind_result = OprfClient::<LakkoOprfCs>::blind(&normalised_bytes, &mut rng)
            .map_err(|_| DiscoveryClientError::voprf())?;

        let blinded_b64 = B64.encode(blind_result.message.serialize());
        Ok((
            Self {
                state: blind_result.state,
                normalised: normalised_bytes,
            },
            blinded_b64,
        ))
    }

    /// The normalised username this blinder was built for —
    /// useful when the caller wants to display a confirmation
    /// like "looking up @alice" without re-running normalisation.
    #[must_use]
    pub fn normalised_username(&self) -> &str {
        // SAFETY: we constructed `normalised` from a `String` via
        // `into_bytes`, so it is valid UTF-8 by construction.
        // Using `from_utf8_unchecked` would be slightly faster
        // but adds an `unsafe` block we don't need.
        core::str::from_utf8(&self.normalised).expect("normalised is UTF-8 by construction")
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
            .finalize(&self.normalised, &evaluated)
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
        let err = OprfBlinder::blind("").expect_err("must fail");
        assert!(matches!(err, DiscoveryClientError::EmptyUsername));

        let err = OprfBlinder::blind("@").expect_err("must fail");
        assert!(matches!(err, DiscoveryClientError::EmptyUsername));

        let err = OprfBlinder::blind("   ").expect_err("must fail");
        assert!(matches!(err, DiscoveryClientError::EmptyUsername));
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
}
