// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! MLS identity: signing keypair + credential + per-device storage provider.
//!
//! Phase 0 uses [`BasicCredential`] (an opaque user-chosen byte string).
//! X.509 credentials with Key Transparency log inclusion proofs land in
//! Phase 1+ per ADR-0001.
//!
//! ## X25519 envelope keypair (Phase 1.16d3b)
//!
//! Each Identity also owns a long-lived X25519 keypair used as the
//! HPKE init_key for `lakko-contacts` sealed envelopes
//! (ContactRequest intro messages, future invite-capability blobs).
//! It's separate from the X25519 keys MLS uses internally for
//! DHKEM — those are scoped to a specific MLS group and rotated
//! on every commit; the Identity-level X25519 has a much longer
//! lifetime and is published to the discovery service's
//! identity-bundle directory (1.16d3a).

use openmls::credentials::{BasicCredential, CredentialWithKey};
use openmls::prelude::Ciphersuite;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;
use openmls_traits::types::SignatureScheme;
use openmls_traits::OpenMlsProvider as _;

use lakko_crypto::{agreement, kdf, signing};

use crate::ciphersuite;
use crate::error::{Error, Result};

/// HKDF salt for deterministic Identity derivation. Pinned ASCII;
/// the value is a constant of the protocol and changing it is a
/// breaking change to every existing account.
const IDENTITY_HKDF_SALT: &[u8] = b"lakko-identity-v1";

/// HKDF info for the Ed25519 sub-secret. Distinct from the
/// X25519 info below so the two keys are domain-separated even
/// though they share the same IKM.
const IDENTITY_HKDF_INFO_ED25519: &[u8] = b"lakko/identity/v1/ed25519";

/// HKDF info for the X25519 sub-secret.
const IDENTITY_HKDF_INFO_X25519: &[u8] = b"lakko/identity/v1/x25519";

/// A user identity bound to a single device.
///
/// Owns:
/// - the device's MLS storage / crypto provider (in-memory for Phase 0)
/// - the long-term signing keypair
/// - the [`CredentialWithKey`] used as the MLS credential
///
/// Multi-device users have one [`Identity`] per device; the cross-device
/// linkage lives in the user-level DeviceGroup (Phase 2, see `lakko-sync`).
pub struct Identity {
    pub(crate) provider: OpenMlsRustCrypto,
    pub(crate) signature_keys: SignatureKeyPair,
    pub(crate) credential: CredentialWithKey,
    pub(crate) ciphersuite: Ciphersuite,
    /// Long-lived X25519 secret for HPKE-Base sealed envelopes
    /// targeted at this identity (Phase 1.16d3b). Distinct from
    /// the per-MLS-group DHKEM keys OpenMLS manages internally;
    /// see the file-level docstring.
    pub(crate) x25519_secret: agreement::SecretKey,
    /// Raw 32-byte Ed25519 signing seed (Phase 1.16d3c).
    ///
    /// Stored alongside `signature_keys` because OpenMLS's
    /// `SignatureKeyPair::private()` accessor is gated behind a
    /// `test-utils` feature flag — the upstream design
    /// intentionally hides the secret. We need the seed for
    /// `lakko-contacts::compose`, which builds its own
    /// `SigningKey::from_seed(...)`. Both `generate*()` and
    /// `from_seed()` produce the SignatureKeyPair via
    /// `from_raw(seed, pub)` so the seed is always available.
    pub(crate) ed25519_seed: [u8; 32],
}

impl Identity {
    /// Generate a fresh identity with the given user-visible name.
    ///
    /// The name is wrapped in a [`BasicCredential`] and is opaque to the
    /// protocol; treat it as a user-display label, not as authentication.
    ///
    /// Uses [`ciphersuite::DEFAULT`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Identity`] if the signing keypair could not be
    /// generated or persisted to the provider's storage.
    pub fn generate(name: &str) -> Result<Self> {
        Self::generate_with_ciphersuite(name, ciphersuite::DEFAULT)
    }

    /// Generate a fresh identity bound to a specific (Lakko-supported)
    /// ciphersuite.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Identity`] for unsupported ciphersuites or any
    /// keypair / storage failure.
    pub fn generate_with_ciphersuite(name: &str, cs: Ciphersuite) -> Result<Self> {
        if !ciphersuite::is_supported(cs) {
            return Err(Error::Identity(format!("unsupported ciphersuite: {cs:?}")));
        }

        let provider = OpenMlsRustCrypto::default();
        let scheme = signature_scheme(cs);

        // Generate the Ed25519 seed via lakko-crypto so we can
        // expose it through `ed25519_seed_bytes()` later. The
        // FIPS path (P-384) is blocked above by
        // `is_supported(cs)`, so in practice we always land on
        // ED25519 here; keep the `signature_scheme` match in
        // case that gate is ever loosened.
        let signing = signing::SigningKey::generate();
        let ed25519_seed = signing.to_seed();
        let ed25519_public = signing.verifying_key().to_bytes();

        let signature_keys = if scheme == SignatureScheme::ED25519 {
            SignatureKeyPair::from_raw(
                SignatureScheme::ED25519,
                ed25519_seed.to_vec(),
                ed25519_public.to_vec(),
            )
        } else {
            // Unreachable in Phase 1 (FIPS gated above) — but
            // keep the path so the FIPS lift is one-line.
            SignatureKeyPair::new(scheme)
                .map_err(|e| Error::Identity(format!("keypair generation: {e}")))?
        };
        signature_keys
            .store(provider.storage())
            .map_err(|e| Error::Identity(format!("keypair storage: {e:?}")))?;

        let basic = BasicCredential::new(name.as_bytes().to_vec());
        let credential = CredentialWithKey {
            credential: basic.into(),
            signature_key: signature_keys.public().into(),
        };

        // Long-lived X25519 keypair for HPKE-Base sealed envelopes.
        // Generated from the OS CSPRNG; the secret stays inside the
        // Identity — accessors below let downstream layers extract
        // raw bytes when persisting (encrypted by the device root key
        // — Phase 1.16c3c2 / 1.16d3b client wiring).
        let x25519_secret = agreement::SecretKey::generate();

        Ok(Self {
            provider,
            signature_keys,
            credential,
            ciphersuite: cs,
            x25519_secret,
            ed25519_seed,
        })
    }

    /// Generate a fresh identity **deterministically** from a
    /// caller-supplied seed.
    ///
    /// Both the Ed25519 signing key and the X25519 envelope key
    /// are derived via HKDF-SHA-256 with a fixed Lakko-pinned
    /// salt (`"lakko-identity-v1"`) and per-key `info` strings
    /// (`"lakko/identity/v1/ed25519"` and
    /// `"lakko/identity/v1/x25519"`). Same `seed` ⇒ byte-identical
    /// Identity (same Ed25519 pubkey, same X25519 pubkey).
    ///
    /// Intended caller: the `apps/web` auth layer, which seeds
    /// this with the OPAQUE password export key produced at
    /// signup / login. That makes the user's Identity a
    /// deterministic function of the password — no separate
    /// persistence layer required, and a successful login on a
    /// fresh device reproduces exactly the same on-the-wire
    /// public keys (and thus the same `identity_bundles` row on
    /// the discovery service, 1.16d3a).
    ///
    /// Currently fixed to the default Lakko ciphersuite
    /// ([`ciphersuite::DEFAULT`]); the FIPS path waits on
    /// OpenMLS X.509 credential support.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Identity`] if the underlying HKDF or the
    /// `SignatureKeyPair` construction / storage fails. HKDF
    /// failure is unreachable in practice (constant-size output);
    /// `SignatureKeyPair::from_raw` is infallible at the type
    /// level (`-> Self`) so the only fallible step is the
    /// per-provider keypair-storage write.
    pub fn from_seed(seed: &[u8]) -> Result<Self> {
        let cs = ciphersuite::DEFAULT;

        // 1. Derive the two 32-byte sub-secrets.
        let ed25519_seed: lakko_crypto::SecretBytes<32> =
            kdf::hkdf_sha256::<32>(seed, IDENTITY_HKDF_SALT, IDENTITY_HKDF_INFO_ED25519)
                .map_err(|e| Error::Identity(format!("HKDF for Ed25519 seed: {e}")))?;
        let x25519_seed: lakko_crypto::SecretBytes<32> =
            kdf::hkdf_sha256::<32>(seed, IDENTITY_HKDF_SALT, IDENTITY_HKDF_INFO_X25519)
                .map_err(|e| Error::Identity(format!("HKDF for X25519 seed: {e}")))?;

        // 2. Build the Ed25519 keypair via lakko-crypto's
        //    `SigningKey::from_seed`, then hand the raw bytes to
        //    OpenMLS via `SignatureKeyPair::from_raw`. The seed
        //    bytes are also stashed on `Identity::ed25519_seed`
        //    so the `ed25519_seed_bytes()` accessor can serve
        //    them without needing OpenMLS's `test-utils`-gated
        //    `private()` accessor.
        let signing_key = signing::SigningKey::from_seed(ed25519_seed.as_bytes());
        let ed25519_secret = signing_key.to_seed();
        let ed25519_public = signing_key.verifying_key().to_bytes();

        let provider = OpenMlsRustCrypto::default();
        let signature_keys = SignatureKeyPair::from_raw(
            SignatureScheme::ED25519,
            ed25519_secret.to_vec(),
            ed25519_public.to_vec(),
        );
        signature_keys
            .store(provider.storage())
            .map_err(|e| Error::Identity(format!("keypair storage: {e:?}")))?;

        // 3. Build the X25519 keypair from the same-shape seed.
        //    `agreement::SecretKey::from_bytes` is the
        //    deterministic counterpart of `SecretKey::generate`.
        let x25519_secret = agreement::SecretKey::from_bytes(*x25519_seed.as_bytes());

        // 4. Wrap the public Ed25519 key in a BasicCredential.
        //    The display name is empty for seed-derived
        //    identities — there is no human-typed label here.
        //    Future work: derive the credential bytes from the
        //    seed too, so credentials are also reproducible.
        let basic = BasicCredential::new(Vec::new());
        let credential = CredentialWithKey {
            credential: basic.into(),
            signature_key: signature_keys.public().into(),
        };

        Ok(Self {
            provider,
            signature_keys,
            credential,
            ciphersuite: cs,
            x25519_secret,
            // Store the [u8; 32] copy on the field — the
            // `SecretBytes<32>` HKDF wrapper drops at scope
            // exit; we keep the raw bytes so the accessor can
            // serve them.
            ed25519_seed: ed25519_secret,
        })
    }

    /// Public signature key as raw bytes (Ed25519 or ECDSA-P384 depending on
    /// the ciphersuite).
    #[must_use]
    pub fn signature_public_key(&self) -> &[u8] {
        self.signature_keys.public()
    }

    /// Raw 32-byte Ed25519 signing seed.
    ///
    /// Hands the secret over to higher layers that drive
    /// signing flows directly (e.g. `lakko-contacts::compose`,
    /// which builds its own `SigningKey::from_seed(...)` to sign
    /// the ContactRequest envelope).
    ///
    /// **Sensitive.** Same handling rules as
    /// [`x25519_secret_bytes`](Self::x25519_secret_bytes) — the
    /// returned bytes MUST NOT touch unencrypted storage. The
    /// in-Identity copy survives across calls; the returned
    /// array is a fresh copy each time.
    ///
    /// Returns a copy each call; the in-Identity seed is stable
    /// across reads.
    ///
    /// Currently only meaningful for Ed25519 ciphersuites. The
    /// FIPS path (P-384) is blocked at construction by
    /// `ciphersuite::is_supported`, so this is unreachable
    /// outside Ed25519 today.
    #[must_use]
    pub fn ed25519_seed_bytes(&self) -> [u8; 32] {
        self.ed25519_seed
    }

    /// Public X25519 key (32 bytes) — the HPKE init_key callers
    /// of `lakko-contacts::compose` target when sealing an
    /// envelope to this identity.
    ///
    /// This is the value the discovery service stores as
    /// `x25519_pubkey` in `identity_bundles` (1.16d3a).
    #[must_use]
    pub fn x25519_public_key(&self) -> [u8; 32] {
        self.x25519_secret.public_key().to_bytes()
    }

    /// Raw X25519 secret bytes (32 bytes). The caller is
    /// responsible for protecting them — wrap in WebCrypto via
    /// the device root key (1.16c3c2) before persisting.
    ///
    /// Returns a copy each call so the caller controls the
    /// lifetime; the in-Identity secret is unaffected.
    #[must_use]
    pub fn x25519_secret_bytes(&self) -> [u8; 32] {
        self.x25519_secret.to_bytes()
    }

    /// Ciphersuite this identity is bound to.
    #[must_use]
    pub const fn ciphersuite(&self) -> Ciphersuite {
        self.ciphersuite
    }

    /// Serialise the OpenMLS provider's storage to opaque bytes
    /// (Phase 1.16e6a).
    ///
    /// The provider holds every piece of MLS state OpenMLS has
    /// written for this identity — the signature keypair,
    /// ratchet trees, epoch keys, queued proposals, etc. The
    /// snapshot is a deterministic length-prefixed binary
    /// encoding of the underlying `HashMap<Vec<u8>, Vec<u8>>`:
    ///
    /// ```text
    ///   u32be       count of entries
    ///   for each:
    ///     u32be     key length
    ///     [u8]      key bytes
    ///     u32be     value length
    ///     [u8]      value bytes
    /// ```
    ///
    /// Phase-1 the bytes are opaque to the caller — the only
    /// supported operation is round-tripping back through
    /// [`Identity::restore_storage`]. Higher layers
    /// (Phase 1.16e6c) wrap the snapshot under the device root
    /// key (1.16c3c2) before stashing it in IndexedDB.
    ///
    /// # Errors
    ///
    /// Surfaces as [`Error::Identity`] only if the underlying
    /// `RwLock` is poisoned (a panic happened while another
    /// thread held the write guard) — unreachable in practice
    /// today because `lakko-mls` is single-threaded inside
    /// each `Identity`.
    pub fn snapshot_storage(&self) -> Result<Vec<u8>> {
        let map = self
            .provider
            .storage()
            .values
            .read()
            .map_err(|_| Error::Identity("storage RwLock poisoned".to_string()))?;
        // Pre-size the buffer: header + per-entry overhead +
        // total payload. Each row is `4 + k + 4 + v`.
        let count = u32::try_from(map.len())
            .map_err(|_| Error::Identity("storage map > u32::MAX entries".to_string()))?;
        let mut out = Vec::with_capacity(
            4 + map
                .iter()
                .map(|(k, v)| 8 + k.len() + v.len())
                .sum::<usize>(),
        );
        out.extend_from_slice(&count.to_be_bytes());
        for (k, v) in map.iter() {
            let kl = u32::try_from(k.len())
                .map_err(|_| Error::Identity("storage key > u32::MAX bytes".to_string()))?;
            let vl = u32::try_from(v.len())
                .map_err(|_| Error::Identity("storage value > u32::MAX bytes".to_string()))?;
            out.extend_from_slice(&kl.to_be_bytes());
            out.extend_from_slice(k);
            out.extend_from_slice(&vl.to_be_bytes());
            out.extend_from_slice(v);
        }
        Ok(out)
    }

    /// Replace this identity's MLS provider storage with the
    /// contents of a snapshot produced by
    /// [`Identity::snapshot_storage`] on a previous run.
    ///
    /// **Replaces** rather than merges — any state OpenMLS has
    /// written into this provider before the call (e.g. the
    /// signature keypair persisted at `from_seed` time) is
    /// discarded in favour of what's in the snapshot. The
    /// caller is expected to feed back a snapshot taken from
    /// the **same seed** so the signature key matches.
    ///
    /// # Errors
    ///
    /// * [`Error::Identity`] — snapshot is malformed (truncated
    ///   counter, length-prefix overflow, …).
    pub fn restore_storage(&self, snapshot: &[u8]) -> Result<()> {
        use std::collections::HashMap;

        // Local helper for the length-prefixed parser. Mirrors
        // the encoder in `snapshot_storage` and rejects any
        // truncation up front.
        fn take<'a>(cur: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
            if cur.len() < n {
                return Err(Error::Identity(format!(
                    "snapshot truncated: need {n} bytes, have {}",
                    cur.len()
                )));
            }
            let (head, rest) = cur.split_at(n);
            *cur = rest;
            Ok(head)
        }
        fn take_u32(cur: &mut &[u8]) -> Result<u32> {
            let bytes = take(cur, 4)?;
            // Length checked above — array conversion is total.
            Ok(u32::from_be_bytes(bytes.try_into().expect("4 bytes")))
        }

        let mut cur = snapshot;
        let count = take_u32(&mut cur)? as usize;
        let mut map: HashMap<Vec<u8>, Vec<u8>> = HashMap::with_capacity(count);
        for _ in 0..count {
            let kl = take_u32(&mut cur)? as usize;
            let k = take(&mut cur, kl)?.to_vec();
            let vl = take_u32(&mut cur)? as usize;
            let v = take(&mut cur, vl)?.to_vec();
            map.insert(k, v);
        }
        if !cur.is_empty() {
            return Err(Error::Identity(format!(
                "snapshot has {} trailing bytes after {count} entries",
                cur.len()
            )));
        }

        let mut guard = self
            .provider
            .storage()
            .values
            .write()
            .map_err(|_| Error::Identity("storage RwLock poisoned".to_string()))?;
        *guard = map;
        Ok(())
    }
}

/// Map a Lakko-supported ciphersuite to its MLS signature scheme.
///
/// Caller must already have validated `cs` via [`ciphersuite::is_supported`];
/// values outside the supported set fall through to the default Ed25519 path.
fn signature_scheme(cs: Ciphersuite) -> SignatureScheme {
    if cs == ciphersuite::FIPS {
        SignatureScheme::ECDSA_SECP384R1_SHA384
    } else {
        SignatureScheme::ED25519
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_default_identity() {
        let alice = Identity::generate("alice").unwrap();
        assert_eq!(alice.ciphersuite(), ciphersuite::DEFAULT);
        // Ed25519 public keys are 32 bytes.
        assert_eq!(alice.signature_public_key().len(), 32);
    }

    #[test]
    fn fips_path_not_yet_wired_in_phase_0() {
        // The FIPS suite (P-384 + ECDSA-P384) requires X.509 credentials and
        // ECDSA support in `openmls_basic_credential`, neither of which are
        // available in the 0.3 line. Phase 1+ will land both. For now we
        // verify the wrapper surfaces a clear error rather than panicking.
        let result = Identity::generate_with_ciphersuite("bob", ciphersuite::FIPS);
        assert!(matches!(result, Err(Error::Identity(_))));
    }

    #[test]
    fn unsupported_ciphersuite_rejected() {
        let result = Identity::generate_with_ciphersuite(
            "carol",
            Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519,
        );
        assert!(matches!(result, Err(Error::Identity(_))));
    }

    #[test]
    fn each_identity_has_distinct_keys() {
        let a = Identity::generate("alice").unwrap();
        let b = Identity::generate("alice").unwrap(); // same name, different keys
        assert_ne!(a.signature_public_key(), b.signature_public_key());
        assert_ne!(a.x25519_public_key(), b.x25519_public_key());
        assert_ne!(a.x25519_secret_bytes(), b.x25519_secret_bytes());
    }

    /// Phase 1.16d3b — every Identity exposes a 32-byte X25519
    /// public key intended for HPKE-Base sealed envelopes (the
    /// `lakko-contacts::compose` recipient_x25519_pub argument).
    #[test]
    fn x25519_public_key_is_32_bytes() {
        let alice = Identity::generate("alice").unwrap();
        let pk = alice.x25519_public_key();
        assert_eq!(pk.len(), 32);
        // Sanity: not all zero (would indicate the secret was
        // never generated).
        assert!(pk.iter().any(|&b| b != 0));
    }

    /// The X25519 secret round-trips through the
    /// `agreement::SecretKey::from_bytes` constructor — useful
    /// because the FFI layer hands the secret to JS as raw bytes
    /// for envelope decryption (`ContactRequest.openEnvelope`)
    /// and may eventually reconstruct an Identity from a
    /// persisted secret + signature key.
    #[test]
    fn x25519_secret_bytes_round_trip_to_public() {
        let alice = Identity::generate("alice").unwrap();
        let secret_bytes = alice.x25519_secret_bytes();
        let public_bytes = alice.x25519_public_key();

        // Reconstruct the secret from raw bytes and recompute the
        // public key — must match the one Identity returned.
        let restored = lakko_crypto::agreement::SecretKey::from_bytes(secret_bytes);
        assert_eq!(
            restored.public_key().to_bytes(),
            public_bytes,
            "x25519 secret round-trip must produce the same public key",
        );
    }

    /// Phase 1.16d3c — the Ed25519 seed accessor exposes the
    /// 32-byte signing secret needed by
    /// `lakko-contacts::compose`. For seed-derived Identities
    /// the bytes round-trip cleanly back through
    /// `signing::SigningKey::from_seed` to the same public key.
    #[test]
    fn ed25519_seed_round_trips_to_public_for_seed_derived_identity() {
        let seed = [0x99u8; 64];
        let alice = Identity::from_seed(&seed).unwrap();
        let signing_seed = alice.ed25519_seed_bytes();
        assert_eq!(signing_seed.len(), 32);
        let restored = lakko_crypto::signing::SigningKey::from_seed(&signing_seed);
        assert_eq!(
            restored.verifying_key().to_bytes().as_slice(),
            alice.signature_public_key(),
            "Ed25519 seed must reproduce the same public key",
        );
    }

    /// Calling `x25519_secret_bytes()` twice produces the same
    /// bytes — the accessor returns a copy, the in-Identity
    /// secret is stable across reads.
    #[test]
    fn x25519_secret_accessor_is_idempotent() {
        let alice = Identity::generate("alice").unwrap();
        let a = alice.x25519_secret_bytes();
        let b = alice.x25519_secret_bytes();
        assert_eq!(a, b);
    }

    /// Phase 1.16d3b2 — `from_seed` is deterministic. The same
    /// seed bytes ALWAYS produce byte-identical Ed25519 and
    /// X25519 keypairs. This is the property that lets the
    /// auth layer derive Identity from the OPAQUE export key on
    /// every login without persisting Identity bytes anywhere.
    #[test]
    fn from_seed_is_deterministic() {
        let seed = [0x11u8; 64];
        let a = Identity::from_seed(&seed).unwrap();
        let b = Identity::from_seed(&seed).unwrap();
        assert_eq!(a.signature_public_key(), b.signature_public_key());
        assert_eq!(a.x25519_public_key(), b.x25519_public_key());
        assert_eq!(a.x25519_secret_bytes(), b.x25519_secret_bytes());
    }

    /// Different seeds produce different keys for both flows
    /// (Ed25519 and X25519). The HKDF info-string domain
    /// separation also means the two halves don't accidentally
    /// collapse to the same key when the seed is short.
    #[test]
    fn from_seed_distinct_seeds_yield_distinct_keys() {
        let a = Identity::from_seed(&[0x11u8; 64]).unwrap();
        let b = Identity::from_seed(&[0x22u8; 64]).unwrap();
        assert_ne!(a.signature_public_key(), b.signature_public_key());
        assert_ne!(a.x25519_public_key(), b.x25519_public_key());
    }

    /// Within a single seed-derived Identity the Ed25519 and
    /// X25519 secrets are independent (different HKDF `info`
    /// strings keep them domain-separated). A fast sanity check
    /// is that they're not byte-equal — would catch a future
    /// refactor that accidentally folded them onto the same
    /// `info`.
    #[test]
    fn from_seed_ed25519_and_x25519_are_domain_separated() {
        let id = Identity::from_seed(&[0x33u8; 64]).unwrap();
        // Ed25519 secret is wrapped inside the SignatureKeyPair;
        // we compare the *public* keys plus the X25519 secret /
        // public to make sure none of the four bytestrings
        // happen to collide.
        assert_ne!(id.signature_public_key(), &id.x25519_public_key()[..]);
        assert_ne!(id.signature_public_key(), &id.x25519_secret_bytes()[..]);
        assert_ne!(id.x25519_public_key(), id.x25519_secret_bytes());
    }

    /// Seed length is intentionally not validated — HKDF
    /// accepts any IKM length. A 64-byte OPAQUE export key, a
    /// 32-byte raw seed, or anything else should produce some
    /// Identity. This test confirms a couple of length variants
    /// don't blow up.
    #[test]
    fn from_seed_accepts_arbitrary_length_ikm() {
        let _ = Identity::from_seed(&[0x44u8; 32]).unwrap();
        let _ = Identity::from_seed(&[0x44u8; 64]).unwrap();
        let _ = Identity::from_seed(&[0x44u8; 100]).unwrap();
    }
}
