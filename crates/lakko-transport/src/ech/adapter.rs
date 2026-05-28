// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `rustls::crypto::hpke::Hpke` adapter on top of the
//! RustCrypto [`hpke`] crate.
//!
//! ## Why this exists
//!
//! `rustls` 0.23 ships HPKE only under the `aws_lc_rs`
//! crypto provider; the `ring` provider — which the rest
//! of `lakko-transport` is pinned to — does NOT ship
//! HPKE. ECH client-side cannot be wired without an
//! `&[&'static dyn rustls::crypto::hpke::Hpke]` slice, so
//! we adapt the pure-Rust [`hpke`] crate (RustCrypto
//! family, RFC 9180) to rustls's trait surface here.
//!
//! ## Supported suites
//!
//! Three concrete `LakkoHpkeSuite*` types — one per AEAD
//! Cloudflare publishes in its ECH config — all on
//! DH-KEM-X25519-HKDF-SHA256 (the KEM Cloudflare uses):
//!
//! | Type | AEAD | rustls `HpkeSuite` |
//! |------|------|--------------------|
//! | [`LakkoHpkeSuiteAesGcm128`] | AES-128-GCM | DHKEM_X25519 + HKDF_SHA256 + AES_128_GCM |
//! | [`LakkoHpkeSuiteAesGcm256`] | AES-256-GCM | DHKEM_X25519 + HKDF_SHA256 + AES_256_GCM |
//! | [`LakkoHpkeSuiteChaCha20Poly1305`] | ChaCha20-Poly1305 | DHKEM_X25519 + HKDF_SHA256 + CHACHA20_POLY_1305 |
//!
//! The NIST-curve KEMs (P-256 / P-384 / P-521) are not
//! enabled — Cloudflare publishes X25519 only, and every
//! disabled feature shrinks the audit surface.
//!
//! ## Trust contract
//!
//! - The cryptographic guarantee comes from the `hpke`
//!   crate; this adapter only re-shapes its input/output
//!   types. Any bug in adapter logic (wrong type
//!   parameter, swapped sealer/opener) shows up as a
//!   round-trip test failure rather than a silent leak.
//! - Marshalling of `HpkePublicKey` / `HpkePrivateKey` /
//!   `EncapsulatedSecret` round-trips through hpke's
//!   `Serializable` / `Deserializable` traits, the same
//!   bytes RFC 9180 wire format mandates.
//! - The `OsRng` used here is `rand_core::OsRng` from
//!   `rand_core` 0.9. It is platform-backed
//!   (`getrandom` under the hood) and is the same RNG
//!   `quinn-proto` and `openmls_rust_crypto` are already
//!   feeding into the lockfile.
//!
//! ## Pinned audit invariants
//!
//! - **T-12 §3** — adapter is wire-compatible with RFC
//!   9180 single-shot Base mode (`OpModeS::Base` /
//!   `OpModeR::Base`); no auth-mode / PSK-mode entry
//!   points are exposed.
//! - **T-12 §4** — adapter never logs key material;
//!   `Debug` impls are opaque.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;

use hpke::aead::{AesGcm128, AesGcm256, ChaCha20Poly1305};
use hpke::kdf::HkdfSha256;
use hpke::kem::X25519HkdfSha256;
use hpke::{
    aead::{Aead as HpkeAead, AeadCtxR, AeadCtxS},
    kdf::Kdf as HpkeKdfTrait,
    setup_receiver, setup_sender, single_shot_open, single_shot_seal, Deserializable, HpkeError,
    Kem as KemTrait, OpModeR, OpModeS, Serializable,
};
use rand_core::{OsRng, UnwrapErr};

/// `rand_core` 0.9 split `RngCore` / `CryptoRng` into
/// infallible (`RngCore` / `CryptoRng`) and fallible
/// (`TryRngCore` / `TryCryptoRng`) traits, and made
/// `OsRng` fallible-only. `hpke` 0.13's API still takes
/// the infallible bound (`R: CryptoRng + RngCore`).
/// [`UnwrapErr`] is the standard adapter that wraps a
/// fallible RNG into the infallible trait, panicking on
/// the underlying RNG error.
///
/// In practice [`OsRng`] never fails on a non-broken
/// system; a panic here means the kernel CSPRNG is
/// itself broken, in which case TLS itself is no longer
/// safe to use. Surfacing the panic immediately is the
/// right hygiene — silent fallback to weak randomness
/// in an ECH context would be far worse.
type OsCryptoRng = UnwrapErr<OsRng>;

fn os_rng() -> OsCryptoRng {
    UnwrapErr(OsRng)
}
use rustls::crypto::hpke::{
    EncapsulatedSecret, Hpke, HpkeOpener, HpkePrivateKey, HpkePublicKey, HpkeSealer, HpkeSuite,
};
use rustls::Error as RustlsError;
use rustls::OtherError;
// rustls 0.23 exposes the IANA-aligned Hpke* enums only
// through its `internal` module (`#[doc(hidden)] pub
// mod internal { … }`). External crypto-provider
// adapters — exactly our use case — are the documented
// consumer; the rustls maintainers intentionally keep
// this path off the stable surface, so any major-version
// bump may require re-pointing these imports. We accept
// that maintenance cost in exchange for staying on
// `ring` for the rest of the transport stack.
use rustls::internal::msgs::enums::{
    HpkeAead as RustlsHpkeAead, HpkeKdf as RustlsHpkeKdf, HpkeKem,
};
use rustls::internal::msgs::handshake::HpkeSymmetricCipherSuite;

extern crate alloc;

/// Adapter error wrapper that carries an [`HpkeError`]
/// through [`OtherError`] into [`RustlsError::Other`].
///
/// Implements `Display` + `Debug` + `Error` so rustls's
/// internal logging can surface the underlying HPKE
/// failure when a `tracing` subscriber is attached.
#[derive(Debug)]
struct AdapterError(HpkeError);

impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "lakko-hpke-adapter: {:?}", self.0)
    }
}

impl std::error::Error for AdapterError {}

fn map_err(e: HpkeError) -> RustlsError {
    RustlsError::Other(OtherError(alloc::sync::Arc::new(AdapterError(e))))
}

// ============================================================================
// Boxed sealer / opener wrappers
// ============================================================================

/// Stateful sealer context — a single sender-side AEAD
/// stream initialised by [`Hpke::setup_sealer`]. Each call
/// to [`HpkeSealer::seal`] advances rustls's RFC 9180
/// nonce sequence by one.
struct LakkoSealer<A, K, Km>(AeadCtxS<A, K, Km>)
where
    A: HpkeAead,
    K: HpkeKdfTrait,
    Km: KemTrait;

impl<A, K, Km> fmt::Debug for LakkoSealer<A, K, Km>
where
    A: HpkeAead,
    K: HpkeKdfTrait,
    Km: KemTrait,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LakkoSealer<..>")
    }
}

impl<A, K, Km> HpkeSealer for LakkoSealer<A, K, Km>
where
    A: HpkeAead + Send + Sync + 'static,
    K: HpkeKdfTrait + Send + Sync + 'static,
    Km: KemTrait + Send + Sync + 'static,
{
    fn seal(&mut self, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, RustlsError> {
        self.0.seal(plaintext, aad).map_err(map_err)
    }
}

/// Stateful opener context — symmetric to
/// [`LakkoSealer`] for the receiver side. Built by
/// [`Hpke::setup_opener`] from an encapsulated key + the
/// recipient's private key.
struct LakkoOpener<A, K, Km>(AeadCtxR<A, K, Km>)
where
    A: HpkeAead,
    K: HpkeKdfTrait,
    Km: KemTrait;

impl<A, K, Km> fmt::Debug for LakkoOpener<A, K, Km>
where
    A: HpkeAead,
    K: HpkeKdfTrait,
    Km: KemTrait,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LakkoOpener<..>")
    }
}

impl<A, K, Km> HpkeOpener for LakkoOpener<A, K, Km>
where
    A: HpkeAead + Send + Sync + 'static,
    K: HpkeKdfTrait + Send + Sync + 'static,
    Km: KemTrait + Send + Sync + 'static,
{
    fn open(&mut self, aad: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, RustlsError> {
        self.0.open(ciphertext, aad).map_err(map_err)
    }
}

// ============================================================================
// Generic helpers — one body, three adapter types call through them
// ============================================================================

fn do_seal<A, K, Km>(
    info: &[u8],
    aad: &[u8],
    plaintext: &[u8],
    pub_key: &HpkePublicKey,
) -> Result<(EncapsulatedSecret, Vec<u8>), RustlsError>
where
    A: HpkeAead,
    K: HpkeKdfTrait,
    Km: KemTrait,
{
    let pk = <Km::PublicKey as Deserializable>::from_bytes(&pub_key.0).map_err(map_err)?;
    let mut rng = os_rng();
    let (enc, ciphertext) =
        single_shot_seal::<A, K, Km, _>(&OpModeS::Base, &pk, info, plaintext, aad, &mut rng)
            .map_err(map_err)?;
    Ok((EncapsulatedSecret(enc.to_bytes().to_vec()), ciphertext))
}

fn do_open<A, K, Km>(
    enc: &EncapsulatedSecret,
    info: &[u8],
    aad: &[u8],
    ciphertext: &[u8],
    secret_key: &HpkePrivateKey,
) -> Result<Vec<u8>, RustlsError>
where
    A: HpkeAead,
    K: HpkeKdfTrait,
    Km: KemTrait,
{
    let sk = <Km::PrivateKey as Deserializable>::from_bytes(secret_key.secret_bytes())
        .map_err(map_err)?;
    let encapped = <Km::EncappedKey as Deserializable>::from_bytes(&enc.0).map_err(map_err)?;
    single_shot_open::<A, K, Km>(&OpModeR::Base, &sk, &encapped, info, ciphertext, aad)
        .map_err(map_err)
}

fn do_setup_sealer<A, K, Km>(
    info: &[u8],
    pub_key: &HpkePublicKey,
) -> Result<(EncapsulatedSecret, Box<dyn HpkeSealer + 'static>), RustlsError>
where
    A: HpkeAead + Send + Sync + 'static,
    K: HpkeKdfTrait + Send + Sync + 'static,
    Km: KemTrait + Send + Sync + 'static,
{
    let pk = <Km::PublicKey as Deserializable>::from_bytes(&pub_key.0).map_err(map_err)?;
    let mut rng = os_rng();
    let (enc, ctx) =
        setup_sender::<A, K, Km, _>(&OpModeS::Base, &pk, info, &mut rng).map_err(map_err)?;
    Ok((
        EncapsulatedSecret(enc.to_bytes().to_vec()),
        Box::new(LakkoSealer::<A, K, Km>(ctx)),
    ))
}

fn do_setup_opener<A, K, Km>(
    enc: &EncapsulatedSecret,
    info: &[u8],
    secret_key: &HpkePrivateKey,
) -> Result<Box<dyn HpkeOpener + 'static>, RustlsError>
where
    A: HpkeAead + Send + Sync + 'static,
    K: HpkeKdfTrait + Send + Sync + 'static,
    Km: KemTrait + Send + Sync + 'static,
{
    let sk = <Km::PrivateKey as Deserializable>::from_bytes(secret_key.secret_bytes())
        .map_err(map_err)?;
    let encapped = <Km::EncappedKey as Deserializable>::from_bytes(&enc.0).map_err(map_err)?;
    let ctx = setup_receiver::<A, K, Km>(&OpModeR::Base, &sk, &encapped, info).map_err(map_err)?;
    Ok(Box::new(LakkoOpener::<A, K, Km>(ctx)))
}

fn do_generate_key_pair<Km: KemTrait>() -> (HpkePublicKey, HpkePrivateKey) {
    let mut rng = os_rng();
    let (sk, pk) = Km::gen_keypair(&mut rng);
    (
        HpkePublicKey(pk.to_bytes().to_vec()),
        HpkePrivateKey::from(sk.to_bytes().to_vec()),
    )
}

// ============================================================================
// Concrete adapter unit structs — one per AEAD
// ============================================================================

/// HPKE suite: DH-KEM-X25519-HKDF-SHA256 + HKDF-SHA256 +
/// AES-128-GCM. The lightweight Cloudflare ECH default.
#[derive(Debug)]
pub struct LakkoHpkeSuiteAesGcm128;

impl Hpke for LakkoHpkeSuiteAesGcm128 {
    fn seal(
        &self,
        info: &[u8],
        aad: &[u8],
        plaintext: &[u8],
        pub_key: &HpkePublicKey,
    ) -> Result<(EncapsulatedSecret, Vec<u8>), RustlsError> {
        do_seal::<AesGcm128, HkdfSha256, X25519HkdfSha256>(info, aad, plaintext, pub_key)
    }

    fn setup_sealer(
        &self,
        info: &[u8],
        pub_key: &HpkePublicKey,
    ) -> Result<(EncapsulatedSecret, Box<dyn HpkeSealer + 'static>), RustlsError> {
        do_setup_sealer::<AesGcm128, HkdfSha256, X25519HkdfSha256>(info, pub_key)
    }

    fn open(
        &self,
        enc: &EncapsulatedSecret,
        info: &[u8],
        aad: &[u8],
        ciphertext: &[u8],
        secret_key: &HpkePrivateKey,
    ) -> Result<Vec<u8>, RustlsError> {
        do_open::<AesGcm128, HkdfSha256, X25519HkdfSha256>(enc, info, aad, ciphertext, secret_key)
    }

    fn setup_opener(
        &self,
        enc: &EncapsulatedSecret,
        info: &[u8],
        secret_key: &HpkePrivateKey,
    ) -> Result<Box<dyn HpkeOpener + 'static>, RustlsError> {
        do_setup_opener::<AesGcm128, HkdfSha256, X25519HkdfSha256>(enc, info, secret_key)
    }

    fn generate_key_pair(&self) -> Result<(HpkePublicKey, HpkePrivateKey), RustlsError> {
        Ok(do_generate_key_pair::<X25519HkdfSha256>())
    }

    fn suite(&self) -> HpkeSuite {
        HpkeSuite {
            kem: HpkeKem::DHKEM_X25519_HKDF_SHA256,
            sym: HpkeSymmetricCipherSuite {
                kdf_id: RustlsHpkeKdf::HKDF_SHA256,
                aead_id: RustlsHpkeAead::AES_128_GCM,
            },
        }
    }
}

/// HPKE suite: DH-KEM-X25519-HKDF-SHA256 + HKDF-SHA256 +
/// AES-256-GCM.
#[derive(Debug)]
pub struct LakkoHpkeSuiteAesGcm256;

impl Hpke for LakkoHpkeSuiteAesGcm256 {
    fn seal(
        &self,
        info: &[u8],
        aad: &[u8],
        plaintext: &[u8],
        pub_key: &HpkePublicKey,
    ) -> Result<(EncapsulatedSecret, Vec<u8>), RustlsError> {
        do_seal::<AesGcm256, HkdfSha256, X25519HkdfSha256>(info, aad, plaintext, pub_key)
    }

    fn setup_sealer(
        &self,
        info: &[u8],
        pub_key: &HpkePublicKey,
    ) -> Result<(EncapsulatedSecret, Box<dyn HpkeSealer + 'static>), RustlsError> {
        do_setup_sealer::<AesGcm256, HkdfSha256, X25519HkdfSha256>(info, pub_key)
    }

    fn open(
        &self,
        enc: &EncapsulatedSecret,
        info: &[u8],
        aad: &[u8],
        ciphertext: &[u8],
        secret_key: &HpkePrivateKey,
    ) -> Result<Vec<u8>, RustlsError> {
        do_open::<AesGcm256, HkdfSha256, X25519HkdfSha256>(enc, info, aad, ciphertext, secret_key)
    }

    fn setup_opener(
        &self,
        enc: &EncapsulatedSecret,
        info: &[u8],
        secret_key: &HpkePrivateKey,
    ) -> Result<Box<dyn HpkeOpener + 'static>, RustlsError> {
        do_setup_opener::<AesGcm256, HkdfSha256, X25519HkdfSha256>(enc, info, secret_key)
    }

    fn generate_key_pair(&self) -> Result<(HpkePublicKey, HpkePrivateKey), RustlsError> {
        Ok(do_generate_key_pair::<X25519HkdfSha256>())
    }

    fn suite(&self) -> HpkeSuite {
        HpkeSuite {
            kem: HpkeKem::DHKEM_X25519_HKDF_SHA256,
            sym: HpkeSymmetricCipherSuite {
                kdf_id: RustlsHpkeKdf::HKDF_SHA256,
                aead_id: RustlsHpkeAead::AES_256_GCM,
            },
        }
    }
}

/// HPKE suite: DH-KEM-X25519-HKDF-SHA256 + HKDF-SHA256 +
/// ChaCha20-Poly1305. Software-only AEAD path —
/// preferable on mobile / ARM without AES hardware
/// acceleration, but Cloudflare typically advertises
/// AES-128-GCM first.
#[derive(Debug)]
pub struct LakkoHpkeSuiteChaCha20Poly1305;

impl Hpke for LakkoHpkeSuiteChaCha20Poly1305 {
    fn seal(
        &self,
        info: &[u8],
        aad: &[u8],
        plaintext: &[u8],
        pub_key: &HpkePublicKey,
    ) -> Result<(EncapsulatedSecret, Vec<u8>), RustlsError> {
        do_seal::<ChaCha20Poly1305, HkdfSha256, X25519HkdfSha256>(info, aad, plaintext, pub_key)
    }

    fn setup_sealer(
        &self,
        info: &[u8],
        pub_key: &HpkePublicKey,
    ) -> Result<(EncapsulatedSecret, Box<dyn HpkeSealer + 'static>), RustlsError> {
        do_setup_sealer::<ChaCha20Poly1305, HkdfSha256, X25519HkdfSha256>(info, pub_key)
    }

    fn open(
        &self,
        enc: &EncapsulatedSecret,
        info: &[u8],
        aad: &[u8],
        ciphertext: &[u8],
        secret_key: &HpkePrivateKey,
    ) -> Result<Vec<u8>, RustlsError> {
        do_open::<ChaCha20Poly1305, HkdfSha256, X25519HkdfSha256>(
            enc, info, aad, ciphertext, secret_key,
        )
    }

    fn setup_opener(
        &self,
        enc: &EncapsulatedSecret,
        info: &[u8],
        secret_key: &HpkePrivateKey,
    ) -> Result<Box<dyn HpkeOpener + 'static>, RustlsError> {
        do_setup_opener::<ChaCha20Poly1305, HkdfSha256, X25519HkdfSha256>(enc, info, secret_key)
    }

    fn generate_key_pair(&self) -> Result<(HpkePublicKey, HpkePrivateKey), RustlsError> {
        Ok(do_generate_key_pair::<X25519HkdfSha256>())
    }

    fn suite(&self) -> HpkeSuite {
        HpkeSuite {
            kem: HpkeKem::DHKEM_X25519_HKDF_SHA256,
            sym: HpkeSymmetricCipherSuite {
                kdf_id: RustlsHpkeKdf::HKDF_SHA256,
                aead_id: RustlsHpkeAead::CHACHA20_POLY_1305,
            },
        }
    }
}

// ============================================================================
// Suite registry
// ============================================================================

/// Singleton instances of all three supported suites.
/// `static` because `rustls::client::EchConfig::new`
/// takes `&[&'static dyn Hpke]`.
pub static SUITE_AES_GCM_128: LakkoHpkeSuiteAesGcm128 = LakkoHpkeSuiteAesGcm128;
/// Singleton — see [`SUITE_AES_GCM_128`].
pub static SUITE_AES_GCM_256: LakkoHpkeSuiteAesGcm256 = LakkoHpkeSuiteAesGcm256;
/// Singleton — see [`SUITE_AES_GCM_128`].
pub static SUITE_CHACHA20_POLY_1305: LakkoHpkeSuiteChaCha20Poly1305 =
    LakkoHpkeSuiteChaCha20Poly1305;

/// Every suite this adapter exposes, in the order rustls
/// will try them when matching a received ECH config.
/// rustls iterates this slice and selects the first
/// suite that matches a published `ECHConfig` entry; the
/// declared order therefore acts as a preference list.
///
/// Order rationale: AES-128-GCM first (Cloudflare's
/// announced preference), AES-256-GCM next (same family,
/// stronger), ChaCha20-Poly1305 last (software-only
/// path; appropriate when both endpoints lack AES-NI but
/// rare in the wild today).
pub static ALL_SUPPORTED_SUITES: &[&'static dyn Hpke] = &[
    &SUITE_AES_GCM_128,
    &SUITE_AES_GCM_256,
    &SUITE_CHACHA20_POLY_1305,
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip every suite: generate keypair, seal a
    /// fixed plaintext, open it back, compare. This is
    /// the cheapest end-to-end sanity check that the
    /// adapter wired hpke's generic parameters
    /// correctly.
    #[test]
    fn round_trip_aes_gcm_128() {
        round_trip(&SUITE_AES_GCM_128);
    }

    #[test]
    fn round_trip_aes_gcm_256() {
        round_trip(&SUITE_AES_GCM_256);
    }

    #[test]
    fn round_trip_chacha20_poly1305() {
        round_trip(&SUITE_CHACHA20_POLY_1305);
    }

    fn round_trip(suite: &dyn Hpke) {
        let (pk, sk) = suite.generate_key_pair().expect("keygen succeeds");
        let info = b"lakko/ech/test";
        let aad = b"lakko-aad";
        let plaintext = b"hello, hpke";

        let (enc, ciphertext) = suite
            .seal(info, aad, plaintext, &pk)
            .expect("seal succeeds");
        let recovered = suite
            .open(&enc, info, aad, &ciphertext, &sk)
            .expect("open succeeds");
        assert_eq!(recovered, plaintext);
    }

    /// Stateful seal/open round-trip: setup a sealer +
    /// opener once, then perform two AEAD operations on
    /// the same channel. Exercises that the rustls
    /// boxed-trait surface (`HpkeSealer` / `HpkeOpener`)
    /// preserves the per-stream nonce sequence rather
    /// than rolling back on each call.
    #[test]
    fn stateful_round_trip_aes_gcm_128() {
        stateful_round_trip(&SUITE_AES_GCM_128);
    }

    #[test]
    fn stateful_round_trip_aes_gcm_256() {
        stateful_round_trip(&SUITE_AES_GCM_256);
    }

    #[test]
    fn stateful_round_trip_chacha20_poly1305() {
        stateful_round_trip(&SUITE_CHACHA20_POLY_1305);
    }

    fn stateful_round_trip(suite: &dyn Hpke) {
        let (pk, sk) = suite.generate_key_pair().expect("keygen succeeds");
        let info = b"lakko/ech/stateful";

        let (enc, mut sealer) = suite
            .setup_sealer(info, &pk)
            .expect("setup_sealer succeeds");
        let mut opener = suite
            .setup_opener(&enc, info, &sk)
            .expect("setup_opener succeeds");

        let m1 = sealer.seal(b"aad-1", b"frame-1").expect("seal 1");
        let m2 = sealer.seal(b"aad-2", b"frame-2").expect("seal 2");

        let p1 = opener.open(b"aad-1", &m1).expect("open 1");
        let p2 = opener.open(b"aad-2", &m2).expect("open 2");

        assert_eq!(p1, b"frame-1");
        assert_eq!(p2, b"frame-2");
    }

    /// Wrong AAD must surface as `open` failure rather
    /// than silently producing garbage. Verifies the
    /// adapter does not strip / re-derive AAD.
    #[test]
    fn wrong_aad_fails_open() {
        let suite: &dyn Hpke = &SUITE_AES_GCM_128;
        let (pk, sk) = suite.generate_key_pair().expect("keygen");
        let (enc, ct) = suite
            .seal(b"info", b"correct-aad", b"plaintext", &pk)
            .expect("seal");
        let err = suite
            .open(&enc, b"info", b"wrong-aad", &ct, &sk)
            .expect_err("open with wrong AAD must fail");
        let _ = err; // any error variant is acceptable.
    }

    /// `suite()` reports the IANA-aligned `HpkeSuite`
    /// that rustls uses to match against published ECH
    /// configs. Drift here means rustls would never
    /// select us, even if the round-trip works.
    #[test]
    fn suite_ids_are_correct() {
        assert_eq!(
            SUITE_AES_GCM_128.suite(),
            HpkeSuite {
                kem: HpkeKem::DHKEM_X25519_HKDF_SHA256,
                sym: HpkeSymmetricCipherSuite {
                    kdf_id: RustlsHpkeKdf::HKDF_SHA256,
                    aead_id: RustlsHpkeAead::AES_128_GCM,
                },
            }
        );
        assert_eq!(
            SUITE_AES_GCM_256.suite(),
            HpkeSuite {
                kem: HpkeKem::DHKEM_X25519_HKDF_SHA256,
                sym: HpkeSymmetricCipherSuite {
                    kdf_id: RustlsHpkeKdf::HKDF_SHA256,
                    aead_id: RustlsHpkeAead::AES_256_GCM,
                },
            }
        );
        assert_eq!(
            SUITE_CHACHA20_POLY_1305.suite(),
            HpkeSuite {
                kem: HpkeKem::DHKEM_X25519_HKDF_SHA256,
                sym: HpkeSymmetricCipherSuite {
                    kdf_id: RustlsHpkeKdf::HKDF_SHA256,
                    aead_id: RustlsHpkeAead::CHACHA20_POLY_1305,
                },
            }
        );
    }

    /// The exported suite list must contain exactly the
    /// three concrete adapters, in the documented
    /// preference order. Order matters — rustls picks
    /// the first match.
    #[test]
    fn all_supported_suites_in_order() {
        assert_eq!(ALL_SUPPORTED_SUITES.len(), 3);
        assert_eq!(
            ALL_SUPPORTED_SUITES[0].suite().sym.aead_id,
            RustlsHpkeAead::AES_128_GCM
        );
        assert_eq!(
            ALL_SUPPORTED_SUITES[1].suite().sym.aead_id,
            RustlsHpkeAead::AES_256_GCM
        );
        assert_eq!(
            ALL_SUPPORTED_SUITES[2].suite().sym.aead_id,
            RustlsHpkeAead::CHACHA20_POLY_1305
        );
    }
}
