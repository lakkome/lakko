// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The camera-less add-device confirmation ([ADR-0049] section 2, amended
//! 2026-09-28).
//!
//! A fresh device that cannot be scanned asks auth to route a request to the
//! account's signed-in devices. One of them (the **old** device) shows a
//! six-digit code it chose; the person types that code on the **new** device;
//! the two devices run SPAKE2 with the code as the password and the new device
//! seals its ADR-0056 pairing payload under the agreed key. The old device
//! pairs only if the new device's key confirmation verifies.
//!
//! # Why a PAKE
//!
//! The code this replaces was SHA-256 of the pairing payload, compared on both
//! screens. The payload carries a free 32-byte rendezvous pseudonym, so a
//! relay could grind its own payload until its code matched (about a million
//! hashes) and have its device added. Here the code is chosen by the old
//! device and never leaves it except through the person; SPAKE2 gives an
//! active attacker one guess per exchange, and a passive one nothing.
//!
//! # Roles and order
//!
//! SPAKE2 side A is the new device, side B the old device. B moves first,
//! because it is the side that knows the code first:
//!
//! 1. old: [`Responder::start`] -> the code to show and `pake_b`;
//! 2. new: the person types the code; [`initiate`] -> `pake_a`, the sealed
//!    payload and `confirm_a`, plus a [`ConfirmationCheck`];
//! 3. old: [`Responder::accept`] -> the pairing payload and `confirm_b`, or
//!    [`LinkError::WrongCode`];
//! 4. new: [`ConfirmationCheck::verify`] on `confirm_b`.
//!
//! [`Responder::accept`] consumes the responder: one guess per request.
//!
//! # Binding
//!
//! Both SPAKE2 identities and the transcript carry the [`LinkContext`] (the
//! auth request id and the login handle), so an exchange cannot be moved to
//! another request or another account's name; a handle the server routed
//! wrongly simply yields a different key.
//!
//! ```text
//! ctx     = "lakko/device-link/v1" 0x00 || request_id(16) || login_handle(32)
//! id_A    = "lakko/device-link/new" 0x00 || ctx
//! id_B    = "lakko/device-link/old" 0x00 || ctx
//! K       = SPAKE2-Ed25519(password = the six ASCII digits, id_A, id_B)
//! T       = SHA-256("lakko/device-link/transcript/v1" 0x00 || ctx || pake_a || pake_b)
//! k_pay   = HKDF-SHA-256(ikm K, salt T, "lakko/device-link/v1/payload-key")   32 bytes
//! nonce   = HKDF-SHA-256(ikm K, salt T, "lakko/device-link/v1/payload-nonce") 12 bytes
//! k_new   = HKDF-SHA-256(ikm K, salt T, "lakko/device-link/v1/confirm-new")   32 bytes
//! k_old   = HKDF-SHA-256(ikm K, salt T, "lakko/device-link/v1/confirm-old")   32 bytes
//! sealed  = ChaCha20-Poly1305(k_pay, nonce, aad = "lakko/device-link/payload/v1" 0x00 || T, payload)
//! confirm_a = HMAC-SHA-256(k_new, "lakko/device-link/confirm/new" 0x00 || T || SHA-256(sealed))
//! confirm_b = HMAC-SHA-256(k_old, "lakko/device-link/confirm/old" 0x00 || T)
//! ```
//!
//! The payload key is used for exactly one message, so a derived nonce is
//! safe.
//!
//! # What this crate does not zeroize
//!
//! The derived keys and the code this crate holds are zeroized on drop. The
//! `spake2` crate keeps its own copy of the password and its ephemeral scalar
//! and does not zeroize them; they are freed, not wiped, when the state is
//! dropped. The password is a six-digit code the person has already seen on a
//! screen; the scalar is live only while one exchange is in flight.
//!
//! [ADR-0049]: ../../../docs/adr/0049-identity-recovery-word-vault.md

#![forbid(unsafe_code)]

use lakko_crypto::aead::{self, Algorithm, NONCE_LEN, TAG_LEN};
use lakko_crypto::hash::{hmac_sha256, sha256};
use lakko_crypto::kdf::hkdf_sha256;
use lakko_crypto::secret::SecretBytes;
use rand_core::{CryptoRng, RngCore};
use spake2::{Ed25519Group, Identity, Password, Spake2};
/// Re-exported so the FFI surfaces hold the typed code and the displayed
/// code in the same zeroizing wrapper without a dependency of their own.
pub use zeroize::Zeroizing;

/// Number of digits in a code.
pub const CODE_DIGITS: usize = 6;

/// Length of the auth request id the exchange is bound to.
pub const REQUEST_ID_LEN: usize = 16;

/// Length of the login handle the exchange is bound to (ADR-0054 S5).
pub const LOGIN_HANDLE_LEN: usize = 32;

/// Length of a SPAKE2 message: a side byte and a compressed Ed25519 point.
pub const PAKE_MESSAGE_LEN: usize = 33;

/// Length of a key-confirmation MAC.
pub const CONFIRM_LEN: usize = 32;

/// Largest pairing payload the new device may seal. The same cap auth has
/// always put on a pairing payload.
pub const MAX_PAYLOAD_LEN: usize = 2048;

/// Largest sealed payload: the payload plus the AEAD tag.
pub const MAX_SEALED_LEN: usize = MAX_PAYLOAD_LEN + TAG_LEN;

/// The count of codes: `10^CODE_DIGITS`.
const CODE_SPACE: u32 = 1_000_000;

/// The largest multiple of [`CODE_SPACE`] that fits a `u32` sample. A sample
/// at or above it is redrawn, so the `%` below carries no bias.
const CODE_SAMPLE_BOUND: u32 = (u32::MAX / CODE_SPACE) * CODE_SPACE;

const CTX_DOMAIN: &[u8] = b"lakko/device-link/v1\0";
const ID_NEW: &[u8] = b"lakko/device-link/new\0";
const ID_OLD: &[u8] = b"lakko/device-link/old\0";
const TRANSCRIPT_DOMAIN: &[u8] = b"lakko/device-link/transcript/v1\0";
const INFO_PAYLOAD_KEY: &[u8] = b"lakko/device-link/v1/payload-key";
const INFO_PAYLOAD_NONCE: &[u8] = b"lakko/device-link/v1/payload-nonce";
const INFO_CONFIRM_NEW: &[u8] = b"lakko/device-link/v1/confirm-new";
const INFO_CONFIRM_OLD: &[u8] = b"lakko/device-link/v1/confirm-old";
const AAD_DOMAIN: &[u8] = b"lakko/device-link/payload/v1\0";
const CONFIRM_NEW_DOMAIN: &[u8] = b"lakko/device-link/confirm/new\0";
const CONFIRM_OLD_DOMAIN: &[u8] = b"lakko/device-link/confirm/old\0";

/// Why an exchange was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    /// The key confirmation did not verify: the code typed on the new device
    /// is not the code this old device showed, or a message was changed in
    /// transit. The request is spent either way.
    #[error("the code did not match")]
    WrongCode,
    /// A message has an impossible shape (length, side byte, not a curve
    /// point, or a sealed payload that does not open under a key whose
    /// confirmation verified).
    #[error("malformed device-link message: {0}")]
    Malformed(&'static str),
    /// What was typed is not six ASCII digits.
    #[error("the code must be six digits")]
    InvalidCode,
    /// A caller-supplied value has the wrong length (request id, handle, an
    /// empty or oversized payload).
    #[error("bad device-link input: {0}")]
    BadInput(&'static str),
    /// A primitive failed where it cannot for these inputs.
    #[error("device-link internal error")]
    Internal,
}

impl LinkError {
    /// A stable code for the FFI surfaces: `wrong-code`, `malformed`,
    /// `invalid-code`, `bad-input`, `internal`.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::WrongCode => "wrong-code",
            Self::Malformed(_) => "malformed",
            Self::InvalidCode => "invalid-code",
            Self::BadInput(_) => "bad-input",
            Self::Internal => "internal",
        }
    }
}

/// What an exchange is bound to: the auth request id and the login handle of
/// the account's name. Public values; both devices know them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkContext {
    request_id: [u8; REQUEST_ID_LEN],
    login_handle: [u8; LOGIN_HANDLE_LEN],
}

impl LinkContext {
    /// Build a context from the 16-byte request id and the 32-byte handle.
    ///
    /// # Errors
    ///
    /// [`LinkError::BadInput`] on a wrong length.
    pub fn new(request_id: &[u8], login_handle: &[u8]) -> Result<Self, LinkError> {
        Ok(Self {
            request_id: request_id
                .try_into()
                .map_err(|_| LinkError::BadInput("request_id must be 16 bytes"))?,
            login_handle: login_handle
                .try_into()
                .map_err(|_| LinkError::BadInput("login_handle must be 32 bytes"))?,
        })
    }

    fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(CTX_DOMAIN.len() + REQUEST_ID_LEN + LOGIN_HANDLE_LEN);
        out.extend_from_slice(CTX_DOMAIN);
        out.extend_from_slice(&self.request_id);
        out.extend_from_slice(&self.login_handle);
        out
    }

    fn identities(&self) -> (Identity, Identity) {
        let ctx = self.bytes();
        let mut a = ID_NEW.to_vec();
        a.extend_from_slice(&ctx);
        let mut b = ID_OLD.to_vec();
        b.extend_from_slice(&ctx);
        (Identity::new(&a), Identity::new(&b))
    }
}

/// A six-digit code, held as its ASCII digits and zeroized on drop.
pub struct LinkCode(Zeroizing<[u8; CODE_DIGITS]>);

impl core::fmt::Debug for LinkCode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("LinkCode(<redacted>)")
    }
}

impl LinkCode {
    /// Draw a uniformly random code. Rejection sampling: a `u32` sample at or
    /// above the largest multiple of a million is redrawn, so every code has
    /// the same probability.
    pub fn generate<R: RngCore + CryptoRng>(rng: &mut R) -> Self {
        Self::from_value(sample_code_value(rng))
    }

    /// Parse what the person typed. ASCII digits only; spaces and `-` between
    /// them are ignored (the code is shown in three pairs); anything else, or
    /// a count other than six, is refused.
    ///
    /// # Errors
    ///
    /// [`LinkError::InvalidCode`].
    pub fn parse(input: &str) -> Result<Self, LinkError> {
        let mut digits = Zeroizing::new([0u8; CODE_DIGITS]);
        let mut n = 0usize;
        for b in input.bytes() {
            match b {
                b'0'..=b'9' => {
                    let slot = digits.get_mut(n).ok_or(LinkError::InvalidCode)?;
                    *slot = b;
                    n += 1;
                },
                b' ' | b'-' => {},
                _ => return Err(LinkError::InvalidCode),
            }
        }
        if n != CODE_DIGITS {
            return Err(LinkError::InvalidCode);
        }
        Ok(Self(digits))
    }

    fn from_value(mut value: u32) -> Self {
        let mut digits = Zeroizing::new([b'0'; CODE_DIGITS]);
        for slot in digits.iter_mut().rev() {
            // `value % 10` is below 10, so the cast cannot truncate.
            #[allow(clippy::cast_possible_truncation)]
            let d = (value % 10) as u8;
            *slot = b'0' + d;
            value /= 10;
        }
        Self(digits)
    }

    /// The six digits, e.g. `"471903"`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        // Only ASCII digits are ever stored.
        core::str::from_utf8(self.0.as_slice()).unwrap_or("")
    }

    /// The code as it is shown: three pairs, `"47 19 03"`.
    #[must_use]
    pub fn grouped(&self) -> String {
        let s = self.as_str();
        let mut out = String::with_capacity(CODE_DIGITS + 2);
        for (i, c) in s.chars().enumerate() {
            if i > 0 && i % 2 == 0 {
                out.push(' ');
            }
            out.push(c);
        }
        out
    }

    fn password(&self) -> Password {
        Password::new(self.0.as_slice())
    }
}

/// Draw a value in `0..1_000_000` without modulo bias.
fn sample_code_value<R: RngCore>(rng: &mut R) -> u32 {
    loop {
        let sample = rng.next_u32();
        if sample < CODE_SAMPLE_BOUND {
            return sample % CODE_SPACE;
        }
    }
}

/// The old device's side (SPAKE2 side B). Created when the request is shown;
/// consumed by [`Responder::accept`].
pub struct Responder {
    state: Spake2<Ed25519Group>,
    context: LinkContext,
    pake_b: Vec<u8>,
}

impl core::fmt::Debug for Responder {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Responder")
            .field("context", &self.context)
            .finish_non_exhaustive()
    }
}

/// What the old device gets from a verified exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    /// The ADR-0056 pairing payload the new device sealed; decode it with
    /// `lakko_mls::pairing::decode` against the account's own handle.
    pub payload: Vec<u8>,
    /// The old device's key confirmation, for the new device.
    pub confirm_b: [u8; CONFIRM_LEN],
}

impl Responder {
    /// Choose a code and start the exchange. Returns the responder, the code
    /// to show, and `pake_b` (read it with [`Responder::pake_message`]).
    pub fn start<R: RngCore + CryptoRng>(context: LinkContext, rng: &mut R) -> (Self, LinkCode) {
        let code = LinkCode::generate(rng);
        let (id_a, id_b) = context.identities();
        let (state, pake_b) =
            Spake2::<Ed25519Group>::start_b_with_rng(&code.password(), &id_a, &id_b, rng);
        (
            Self {
                state,
                context,
                pake_b,
            },
            code,
        )
    }

    /// The message to hand the new device (`pake_b`, 33 bytes).
    #[must_use]
    pub fn pake_message(&self) -> &[u8] {
        &self.pake_b
    }

    /// Finish the exchange with the new device's messages. Consumes the
    /// responder, so a request gets exactly one guess.
    ///
    /// # Errors
    ///
    /// [`LinkError::Malformed`] for a message of impossible shape,
    /// [`LinkError::WrongCode`] when the new device's confirmation does not
    /// verify. Either way nothing may be paired.
    pub fn accept(
        self,
        pake_a: &[u8],
        sealed: &[u8],
        confirm_a: &[u8],
    ) -> Result<Accepted, LinkError> {
        if pake_a.len() != PAKE_MESSAGE_LEN {
            return Err(LinkError::Malformed("pake_a length"));
        }
        if confirm_a.len() != CONFIRM_LEN {
            return Err(LinkError::Malformed("confirm_a length"));
        }
        if sealed.len() <= TAG_LEN || sealed.len() > MAX_SEALED_LEN {
            return Err(LinkError::Malformed("sealed payload length"));
        }
        let key = Zeroizing::new(
            self.state
                .finish(pake_a)
                .map_err(|_| LinkError::Malformed("pake_a is not a side-A message"))?,
        );
        let schedule = KeySchedule::derive(&key, &self.context, pake_a, &self.pake_b)?;

        let expected = SecretBytes::new(schedule.confirm_a(sealed));
        let presented: [u8; CONFIRM_LEN] = confirm_a
            .try_into()
            .map_err(|_| LinkError::Malformed("confirm_a length"))?;
        if expected != SecretBytes::new(presented) {
            return Err(LinkError::WrongCode);
        }
        let payload = aead::open(
            Algorithm::ChaCha20Poly1305,
            &schedule.payload_key,
            &schedule.nonce,
            &schedule.aad(),
            sealed,
        )
        .map_err(|_| LinkError::Malformed("sealed payload does not open"))?;
        if payload.is_empty() {
            return Err(LinkError::Malformed("empty payload"));
        }
        Ok(Accepted {
            payload,
            confirm_b: schedule.confirm_b(),
        })
    }
}

/// What the new device sends after the person typed the code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    /// SPAKE2 side-A message (33 bytes).
    pub pake_a: Vec<u8>,
    /// The pairing payload under the agreed key.
    pub sealed_payload: Vec<u8>,
    /// The new device's key confirmation (32 bytes).
    pub confirm_a: [u8; CONFIRM_LEN],
}

/// The new device's check of the old device's key confirmation. Holds the
/// expected MAC; zeroized on drop.
pub struct ConfirmationCheck {
    expected_confirm_b: SecretBytes<CONFIRM_LEN>,
}

impl core::fmt::Debug for ConfirmationCheck {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("ConfirmationCheck(<redacted>)")
    }
}

impl ConfirmationCheck {
    /// Verify the old device's `confirm_b` (constant time).
    ///
    /// # Errors
    ///
    /// [`LinkError::Malformed`] on a wrong length, [`LinkError::WrongCode`]
    /// on a mismatch: whoever answered did not hold the code the person
    /// typed.
    pub fn verify(&self, confirm_b: &[u8]) -> Result<(), LinkError> {
        let presented: [u8; CONFIRM_LEN] = confirm_b
            .try_into()
            .map_err(|_| LinkError::Malformed("confirm_b length"))?;
        if self.expected_confirm_b == SecretBytes::new(presented) {
            Ok(())
        } else {
            Err(LinkError::WrongCode)
        }
    }
}

/// The new device's side (SPAKE2 side A), in one step: it starts only once the
/// person has typed the code, and by then it already holds `pake_b`.
///
/// # Errors
///
/// [`LinkError::BadInput`] for an empty or oversized payload,
/// [`LinkError::Malformed`] for a `pake_b` of impossible shape.
pub fn initiate<R: RngCore + CryptoRng>(
    context: &LinkContext,
    code: &LinkCode,
    pake_b: &[u8],
    payload: &[u8],
    rng: &mut R,
) -> Result<(Sealed, ConfirmationCheck), LinkError> {
    if payload.is_empty() || payload.len() > MAX_PAYLOAD_LEN {
        return Err(LinkError::BadInput("payload must be 1..=2048 bytes"));
    }
    if pake_b.len() != PAKE_MESSAGE_LEN {
        return Err(LinkError::Malformed("pake_b length"));
    }
    let (id_a, id_b) = context.identities();
    let (state, pake_a) =
        Spake2::<Ed25519Group>::start_a_with_rng(&code.password(), &id_a, &id_b, rng);
    let key = Zeroizing::new(
        state
            .finish(pake_b)
            .map_err(|_| LinkError::Malformed("pake_b is not a side-B message"))?,
    );
    let schedule = KeySchedule::derive(&key, context, &pake_a, pake_b)?;
    let sealed_payload = aead::seal(
        Algorithm::ChaCha20Poly1305,
        &schedule.payload_key,
        &schedule.nonce,
        &schedule.aad(),
        payload,
    )
    .map_err(|_| LinkError::Internal)?;
    let confirm_a = schedule.confirm_a(&sealed_payload);
    let check = ConfirmationCheck {
        expected_confirm_b: SecretBytes::new(schedule.confirm_b()),
    };
    Ok((
        Sealed {
            pake_a,
            sealed_payload,
            confirm_a,
        },
        check,
    ))
}

/// Everything derived from the SPAKE2 key. The keys are `SecretBytes`,
/// zeroized on drop.
struct KeySchedule {
    transcript: [u8; 32],
    payload_key: SecretBytes<32>,
    nonce: [u8; NONCE_LEN],
    confirm_new_key: SecretBytes<32>,
    confirm_old_key: SecretBytes<32>,
}

impl KeySchedule {
    fn derive(
        key: &[u8],
        context: &LinkContext,
        pake_a: &[u8],
        pake_b: &[u8],
    ) -> Result<Self, LinkError> {
        let mut t = Vec::with_capacity(TRANSCRIPT_DOMAIN.len() + 96 + 2 * PAKE_MESSAGE_LEN);
        t.extend_from_slice(TRANSCRIPT_DOMAIN);
        t.extend_from_slice(&context.bytes());
        t.extend_from_slice(pake_a);
        t.extend_from_slice(pake_b);
        let transcript = sha256(&t);
        let payload_key = hkdf_sha256::<32>(key, &transcript, INFO_PAYLOAD_KEY)
            .map_err(|_| LinkError::Internal)?;
        let nonce = *hkdf_sha256::<NONCE_LEN>(key, &transcript, INFO_PAYLOAD_NONCE)
            .map_err(|_| LinkError::Internal)?
            .as_bytes();
        let confirm_new_key = hkdf_sha256::<32>(key, &transcript, INFO_CONFIRM_NEW)
            .map_err(|_| LinkError::Internal)?;
        let confirm_old_key = hkdf_sha256::<32>(key, &transcript, INFO_CONFIRM_OLD)
            .map_err(|_| LinkError::Internal)?;
        Ok(Self {
            transcript,
            payload_key,
            nonce,
            confirm_new_key,
            confirm_old_key,
        })
    }

    fn aad(&self) -> Vec<u8> {
        let mut aad = AAD_DOMAIN.to_vec();
        aad.extend_from_slice(&self.transcript);
        aad
    }

    fn confirm_a(&self, sealed: &[u8]) -> [u8; CONFIRM_LEN] {
        let mut msg = CONFIRM_NEW_DOMAIN.to_vec();
        msg.extend_from_slice(&self.transcript);
        msg.extend_from_slice(&sha256(sealed));
        hmac_sha256(self.confirm_new_key.as_bytes(), &msg)
    }

    fn confirm_b(&self) -> [u8; CONFIRM_LEN] {
        let mut msg = CONFIRM_OLD_DOMAIN.to_vec();
        msg.extend_from_slice(&self.transcript);
        hmac_sha256(self.confirm_old_key.as_bytes(), &msg)
    }
}

/// Fuzzing entry points. Not a supported API.
#[doc(hidden)]
pub mod fuzz_api {
    use super::{LinkContext, Responder};

    /// A deterministic RNG so the fuzzer sees a fixed transcript.
    struct FixedRng(u64);

    impl rand_core::RngCore for FixedRng {
        fn next_u32(&mut self) -> u32 {
            // Truncation to the low half is the point.
            #[allow(clippy::cast_possible_truncation)]
            let v = self.next_u64() as u32;
            v
        }
        fn next_u64(&mut self) -> u64 {
            // SplitMix64: fixed, well mixed, no dependency.
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            for chunk in dest.chunks_mut(8) {
                let v = self.next_u64().to_le_bytes();
                chunk.copy_from_slice(&v[..chunk.len()]);
            }
        }
        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }

    impl rand_core::CryptoRng for FixedRng {}

    /// Run the old device's accept path on arbitrary bytes under a fixed
    /// responder. The input is split as `pake_a(33) || confirm_a(32) ||
    /// sealed(rest)` when long enough, else fed whole as `pake_a`. Returns
    /// whether the exchange was accepted (it never should be).
    #[must_use]
    pub fn accept_arbitrary(data: &[u8]) -> bool {
        let Ok(context) = LinkContext::new(&[0x11; 16], &[0x22; 32]) else {
            return false;
        };
        let (responder, _code) = Responder::start(context, &mut FixedRng(7));
        if data.len() >= 65 {
            let (pake_a, rest) = data.split_at(33);
            let (confirm_a, sealed) = rest.split_at(32);
            responder.accept(pake_a, sealed, confirm_a).is_ok()
        } else {
            responder.accept(data, &[], &[]).is_ok()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rand_chacha::rand_core::SeedableRng as _;
    use rand_chacha::ChaCha20Rng;

    fn ctx() -> LinkContext {
        LinkContext::new(&[0xA1; 16], &[0xB2; 32]).unwrap()
    }

    fn rng(seed: u64) -> ChaCha20Rng {
        ChaCha20Rng::seed_from_u64(seed)
    }

    fn run(
        seed: u64,
        typed: Option<&str>,
        payload: &[u8],
    ) -> (Result<Accepted, LinkError>, ConfirmationCheck) {
        let mut r = rng(seed);
        let (responder, code) = Responder::start(ctx(), &mut r);
        let typed = typed.map_or_else(
            || LinkCode::parse(code.as_str()).unwrap(),
            |t| LinkCode::parse(t).unwrap(),
        );
        let (sealed, check) =
            initiate(&ctx(), &typed, responder.pake_message(), payload, &mut r).unwrap();
        (
            responder.accept(&sealed.pake_a, &sealed.sealed_payload, &sealed.confirm_a),
            check,
        )
    }

    #[test]
    fn right_code_round_trips_and_confirms_both_ways() {
        let (accepted, check) = run(1, None, b"pairing payload");
        let accepted = accepted.unwrap();
        assert_eq!(accepted.payload, b"pairing payload");
        check.verify(&accepted.confirm_b).unwrap();
    }

    #[test]
    fn wrong_code_is_refused() {
        let mut r = rng(2);
        let (responder, code) = Responder::start(ctx(), &mut r);
        let other = if code.as_str() == "000000" {
            "000001"
        } else {
            "000000"
        };
        let typed = LinkCode::parse(other).unwrap();
        let (sealed, check) =
            initiate(&ctx(), &typed, responder.pake_message(), b"p", &mut r).unwrap();
        assert_eq!(
            responder.accept(&sealed.pake_a, &sealed.sealed_payload, &sealed.confirm_a),
            Err(LinkError::WrongCode)
        );
        // And a confirm_b from a real old device under another code does not
        // verify on the new device either.
        assert_eq!(check.verify(&[0u8; 32]), Err(LinkError::WrongCode));
    }

    #[test]
    fn a_relay_that_changes_any_message_fails() {
        let mut r = rng(3);
        let (responder, code) = Responder::start(ctx(), &mut r);
        let (sealed, check) =
            initiate(&ctx(), &code, responder.pake_message(), b"payload", &mut r).unwrap();

        // Swap pake_a for the relay's own side-A message under a guessed code.
        let guess = LinkCode::parse("123456").unwrap();
        let mut r2 = rng(99);
        let (relay_resp, _) = Responder::start(ctx(), &mut r2);
        let (relay_sealed, _) =
            initiate(&ctx(), &guess, relay_resp.pake_message(), b"x", &mut r2).unwrap();
        let (fresh, _) = Responder::start(ctx(), &mut rng(3));
        assert!(fresh
            .accept(
                &relay_sealed.pake_a,
                &sealed.sealed_payload,
                &sealed.confirm_a
            )
            .is_err());

        // Flip one byte of the sealed payload.
        let mut bad = sealed.sealed_payload.clone();
        bad[0] ^= 1;
        let (fresh, _) = Responder::start(ctx(), &mut rng(3));
        assert_eq!(
            fresh.accept(&sealed.pake_a, &bad, &sealed.confirm_a),
            Err(LinkError::WrongCode)
        );

        // Flip one byte of confirm_a.
        let mut bad = sealed.confirm_a;
        bad[5] ^= 0x80;
        let (fresh, _) = Responder::start(ctx(), &mut rng(3));
        assert_eq!(
            fresh.accept(&sealed.pake_a, &sealed.sealed_payload, &bad),
            Err(LinkError::WrongCode)
        );

        // A pake_b the relay substituted (its own side-B message): the new
        // device's confirmation then fails at the real old device, and the
        // relay's confirm_b fails at the new device.
        let mut r4 = rng(4);
        let (relay_b, _) = Responder::start(ctx(), &mut r4);
        let (sealed2, check2) =
            initiate(&ctx(), &code, relay_b.pake_message(), b"payload", &mut r4).unwrap();
        assert_eq!(
            responder.accept(&sealed2.pake_a, &sealed2.sealed_payload, &sealed2.confirm_a),
            Err(LinkError::WrongCode)
        );
        let relay_accept =
            relay_b.accept(&sealed2.pake_a, &sealed2.sealed_payload, &sealed2.confirm_a);
        assert_eq!(relay_accept, Err(LinkError::WrongCode));
        assert_eq!(check2.verify(&[7u8; 32]), Err(LinkError::WrongCode));
        drop(check);
    }

    #[test]
    fn a_sealed_payload_moved_to_another_request_fails() {
        let mut r = rng(5);
        let (responder, code) = Responder::start(ctx(), &mut r);
        // The new device is told a different request id (or a relay moves
        // the messages to another request): the keys differ.
        let other = LinkContext::new(&[0xA2; 16], &[0xB2; 32]).unwrap();
        let (sealed, _) =
            initiate(&other, &code, responder.pake_message(), b"payload", &mut r).unwrap();
        assert_eq!(
            responder.accept(&sealed.pake_a, &sealed.sealed_payload, &sealed.confirm_a),
            Err(LinkError::WrongCode)
        );
        // Same for another account's handle.
        let (responder, code) = Responder::start(ctx(), &mut r);
        let other = LinkContext::new(&[0xA1; 16], &[0xB3; 32]).unwrap();
        let (sealed, _) =
            initiate(&other, &code, responder.pake_message(), b"payload", &mut r).unwrap();
        assert_eq!(
            responder.accept(&sealed.pake_a, &sealed.sealed_payload, &sealed.confirm_a),
            Err(LinkError::WrongCode)
        );
    }

    #[test]
    fn side_confusion_and_bad_shapes_are_malformed() {
        let mut r = rng(6);
        let (responder, _) = Responder::start(ctx(), &mut r);
        let pake_b = responder.pake_message().to_vec();
        // A side-B message offered as pake_a.
        assert!(matches!(
            responder.accept(&pake_b, &[0u8; 40], &[0u8; 32]),
            Err(LinkError::Malformed(_))
        ));
        let (responder, code) = Responder::start(ctx(), &mut r);
        assert!(matches!(
            responder.accept(&[0x41; 32], &[0u8; 40], &[0u8; 32]),
            Err(LinkError::Malformed(_))
        ));
        let (responder, _) = Responder::start(ctx(), &mut r);
        let pb = responder.pake_message().to_vec();
        assert!(matches!(
            initiate(&ctx(), &code, &pb, &[], &mut r),
            Err(LinkError::BadInput(_))
        ));
        assert!(matches!(
            initiate(&ctx(), &code, &pb, &vec![1u8; MAX_PAYLOAD_LEN + 1], &mut r),
            Err(LinkError::BadInput(_))
        ));
        assert!(matches!(
            initiate(&ctx(), &code, &pb[..32], b"p", &mut r),
            Err(LinkError::Malformed(_))
        ));
        // A side-A message offered as pake_b.
        let (sealed, _) = initiate(&ctx(), &code, &pb, b"p", &mut r).unwrap();
        assert!(matches!(
            initiate(&ctx(), &code, &sealed.pake_a, b"p", &mut r),
            Err(LinkError::Malformed(_))
        ));
        assert!(LinkContext::new(&[0; 15], &[0; 32]).is_err());
        assert!(LinkContext::new(&[0; 16], &[0; 31]).is_err());
    }

    #[test]
    fn the_largest_payload_round_trips() {
        let payload = vec![0x5A; MAX_PAYLOAD_LEN];
        let (accepted, _) = run(8, None, &payload);
        assert_eq!(accepted.unwrap().payload, payload);
    }

    #[test]
    fn code_parse_and_format() {
        let c = LinkCode::parse("47 19 03").unwrap();
        assert_eq!(c.as_str(), "471903");
        assert_eq!(c.grouped(), "47 19 03");
        assert_eq!(LinkCode::parse("47-19-03").unwrap().as_str(), "471903");
        assert_eq!(LinkCode::parse("000000").unwrap().grouped(), "00 00 00");
        for bad in [
            "",
            "12345",
            "1234567",
            "12a456",
            "１２３４５６",
            "12.3456",
            "\u{0663}23456",
        ] {
            assert_eq!(
                LinkCode::parse(bad).err(),
                Some(LinkError::InvalidCode),
                "{bad}"
            );
        }
        assert_eq!(format!("{c:?}"), "LinkCode(<redacted>)");
    }

    #[test]
    fn code_generation_rejects_at_the_bound() {
        struct Scripted(Vec<u32>);
        impl rand_core::RngCore for Scripted {
            fn next_u32(&mut self) -> u32 {
                self.0.remove(0)
            }
            fn next_u64(&mut self) -> u64 {
                u64::from(self.next_u32())
            }
            fn fill_bytes(&mut self, _: &mut [u8]) {}
            fn try_fill_bytes(&mut self, _: &mut [u8]) -> Result<(), rand_core::Error> {
                Ok(())
            }
        }
        assert_eq!(CODE_SAMPLE_BOUND, 4_294_000_000);
        let mut s = Scripted(vec![u32::MAX, CODE_SAMPLE_BOUND, CODE_SAMPLE_BOUND - 1]);
        assert_eq!(sample_code_value(&mut s), 999_999);
        let mut s = Scripted(vec![1_000_042]);
        assert_eq!(sample_code_value(&mut s), 42);
        assert_eq!(LinkCode::from_value(42).as_str(), "000042");
    }

    #[test]
    fn code_generation_is_unbiased_over_a_large_sample() {
        // Statistical sanity, loose bounds: every digit position is close to
        // uniform, and the top decile of codes is not favoured over the rest.
        const N: usize = 200_000;
        let mut r = rng(42);
        let mut counts = [[0u32; 10]; CODE_DIGITS];
        let mut high = 0u32;
        for _ in 0..N {
            let c = LinkCode::generate(&mut r);
            for (pos, d) in c.as_str().bytes().enumerate() {
                counts[pos][usize::from(d - b'0')] += 1;
            }
            if c.as_str().as_bytes()[0] == b'9' {
                high += 1;
            }
        }
        // Expected 20_000 per cell; sd about 134. Allow 6 sd.
        for pos in counts {
            for n in pos {
                assert!((19_200..=20_800).contains(&n), "cell count {n}");
            }
            // Chi-square with 9 dof: 99.99th percentile is about 33.7.
            let chi: f64 = pos
                .iter()
                .map(|&n| {
                    let d = f64::from(n) - 20_000.0;
                    d * d / 20_000.0
                })
                .sum();
            assert!(chi < 40.0, "chi-square {chi}");
        }
        assert!((19_200..=20_800).contains(&high));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        #[test]
        fn any_payload_round_trips_under_the_right_code(
            seed in any::<u64>(),
            payload in proptest::collection::vec(any::<u8>(), 1..512),
        ) {
            let (accepted, check) = run(seed, None, &payload);
            let accepted = accepted.unwrap();
            prop_assert_eq!(&accepted.payload, &payload);
            prop_assert!(check.verify(&accepted.confirm_b).is_ok());
        }

        #[test]
        fn any_other_code_is_refused(seed in any::<u64>(), guess in 0u32..1_000_000) {
            let mut r = rng(seed);
            let (responder, code) = Responder::start(ctx(), &mut r);
            let typed = LinkCode::from_value(guess);
            prop_assume!(typed.as_str() != code.as_str());
            let (sealed, _) =
                initiate(&ctx(), &typed, responder.pake_message(), b"p", &mut r).unwrap();
            prop_assert_eq!(
                responder.accept(&sealed.pake_a, &sealed.sealed_payload, &sealed.confirm_a),
                Err(LinkError::WrongCode)
            );
        }

        #[test]
        fn accept_never_panics_on_arbitrary_input(data in proptest::collection::vec(any::<u8>(), 0..200)) {
            prop_assert!(!fuzz_api::accept_arbitrary(&data));
        }
    }
}
