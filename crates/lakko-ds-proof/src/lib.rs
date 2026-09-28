// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The Delivery Service request proof ([ADR-0053]).
//!
//! A claimed journal on `services/ds` answers only a request signed with a
//! key the journal was claimed under: for a group a per-epoch Ed25519 key
//! every member derives from the MLS exporter (`ACCESS_KEY_LABEL`), for a
//! mailbox the identity key its pseudonym is the hash of. This crate is the
//! one definition of what is signed and how the proof travels, shared by the
//! server and both FFI surfaces so no client re-derives the bytes.
//!
//! # Wire format
//!
//! One request header:
//!
//! ```text
//! X-Lakko-DS-Proof: v1.<epoch>.<unix_ms>.<nonce_b64url>.<sig_b64url>
//! X-Lakko-DS-Proof: v1.<epoch>.<unix_ms>.<nonce_b64url>.<sig_b64url>.<reach_pk_b64url>
//! ```
//!
//! `epoch` and `unix_ms` are ASCII decimals, the nonce is 16 bytes and the
//! signature 64, both base64url without padding. The sixth field is present
//! exactly when `epoch` is [`REACH_EPOCH`] (a reach proof, [ADR-0058]) and
//! carries the signer's 32-byte Ed25519 public key; a reach proof without it,
//! or any other proof with it, does not parse.
//!
//! # Canonical bytes
//!
//! ```text
//! "lakko/ds/proof/v1" || 0x00 || method || 0x00 || canonical_path || 0x00
//!   || query || 0x00 || BLAKE3(body) || epoch(8 BE) || unix_ms(8 BE) || nonce(16)
//!   [ || reach_pk(32) ]   -- a reach proof only
//! ```
//!
//! **Why a reach proof names its key.** A closed mailbox may hold thousands
//! of reach keys; without a hint the server would try each one. The key is
//! inside the signed bytes, so it cannot be swapped for another registered
//! key, and [`verify`] refuses a reach proof whose named key is not the key it
//! is checked under. The server looks up exactly that key and verifies once.
//! The field was put in the one proof header rather than a second header so a
//! reach proof stays one self-contained value that parses or does not; a
//! group or identity-key proof is byte-for-byte what it was before.
//!
//! `canonical_path` is the service path (`/ds/group/<hex>/messages`), never
//! an nginx prefix, so web and native sign the same bytes. The NUL separators
//! keep `method || path` unambiguous, and the epoch is bound so a proof under
//! one epoch's key cannot be presented under another label. The body enters
//! as its BLAKE3 digest, so a signature is the same size for a 1 MB publish
//! as for an empty long-poll.
//!
//! # What the verifier checks, in order
//!
//! 1. the header parses (bounded, fail-closed);
//! 2. `unix_ms` lies within [`PROOF_WINDOW_MS`] of the server's clock;
//! 3. the signature verifies under the key the caller resolved for `epoch`.
//!
//! Key resolution and the nonce replay cache are the server's, since both
//! are state; this crate is pure.
//!
//! # Reach proofs and delegations ([ADR-0058] section 2)
//!
//! A write to a claimed, closed identity mailbox is signed not by the
//! identity key but by a **reach key** the owner registered
//! ([`DsProofSigner::for_reach`]). Its proofs carry [`REACH_EPOCH`]
//! (`u64::MAX`) in the epoch field, a value no group reaches and distinct
//! from the identity-key mailbox proof's [`MAILBOX_EPOCH`] (0), so the server
//! can never confuse which key table a proof must be checked against. A reach
//! key holder may also hand one write to another key through a
//! [`Delegation`] (module [`delegation`], which states the server contract).
//!
//! # Ownership statements ([ADR-0057])
//!
//! Who may register a journal's epoch keys is a second, separate surface:
//! the [`statement`] module - the root and registrar seed derivations and
//! the signed statements (claim, root reset, register epoch, registrar
//! management, root transfer, push binding) the DS routes take as bodies.
//!
//! [ADR-0057]: ../../../docs/adr/0057-journal-ownership.md
//!
//! [ADR-0053]: ../../../docs/adr/0053-ds-journal-authorisation.md
//! [ADR-0058]: ../../../docs/adr/0058-scoped-discovery.md

#![forbid(unsafe_code)]

pub mod statement;

pub use statement::{
    journal_registrar_seed, journal_root_seed, push_binding_key, sign_statement,
    statement_signing_key, Rank, RegistrarChange, RegistrarEntry, SignedStatement, StatementBody,
    StatementError, StatementKind, StatementSigner, JOURNAL_ID_LEN, MAX_LIST_ENTRIES,
    MAX_STATEMENT_LEN, ROOT_RANK_BYTE, STATEMENT_DOMAIN,
};

use base64::engine::general_purpose::{STANDARD as B64_STD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use lakko_contacts::IdentityPseudonym;
use lakko_crypto::hash;

pub use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey};

pub mod delegation;

pub use delegation::{
    sign_delegation, verify_delegated_proof, verify_delegation, Delegation, DelegationError,
    DELEGATION_HEADER_NAME, DELEGATION_LEN, MAX_DELEGATION_TTL_MS,
};

/// The request header the proof travels in (lowercase, as HTTP/2 carries it).
pub const PROOF_HEADER_NAME: &str = "x-lakko-ds-proof";
/// Domain separator that opens the canonical bytes.
pub const CANONICAL_DOMAIN: &[u8] = b"lakko/ds/proof/v1";
/// The MLS exporter label whose 32-byte output seeds a group's access key.
pub const ACCESS_KEY_LABEL: &str = "lakko/ds/access/v1";
/// Nonce length in bytes.
pub const NONCE_LEN: usize = 16;
/// Ed25519 signature length in bytes.
pub const SIGNATURE_LEN: usize = 64;
/// How far from the server's clock a proof's `unix_ms` may lie, either way.
pub const PROOF_WINDOW_MS: u64 = 120_000;
/// The epoch label a mailbox proof carries (a mailbox has no epochs).
pub const MAILBOX_EPOCH: u64 = 0;
/// The epoch label a reach-key proof carries ([ADR-0058] section 2).
///
/// A delegate's proof carries it too. `u64::MAX`: never a group epoch, never
/// [`MAILBOX_EPOCH`], so a server cannot check a reach proof against the
/// wrong key table.
///
/// [ADR-0058]: ../../../docs/adr/0058-scoped-discovery.md
pub const REACH_EPOCH: u64 = u64::MAX;
/// Upper bound on the header's length; anything longer is refused unparsed.
pub const MAX_HEADER_LEN: usize = 256;

const VERSION: &str = "v1";
const SEP: u8 = 0;

/// The parts of a request the proof binds.
#[derive(Clone, Copy, Debug)]
pub struct RequestParts<'a> {
    /// HTTP method, upper case (`GET`, `POST`, `PUT`).
    pub method: &'a str,
    /// The service path, without any reverse-proxy prefix.
    pub canonical_path: &'a str,
    /// The raw query string without the `?`, or empty.
    pub query: &'a str,
    /// The request body, or empty.
    pub body: &'a [u8],
}

/// Length of the reach public key a reach proof names.
pub const REACH_KEY_LEN: usize = 32;

/// A parsed proof header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proof {
    /// The epoch whose key signed this (`MAILBOX_EPOCH` for a mailbox).
    pub epoch: u64,
    /// The signer's clock at signing, unix milliseconds.
    pub unix_ms: u64,
    /// Fresh per request; the server refuses a repeat within the window.
    pub nonce: [u8; NONCE_LEN],
    /// Ed25519 over the canonical bytes.
    pub signature: [u8; SIGNATURE_LEN],
    /// The signer's public key, present exactly when `epoch` is
    /// [`REACH_EPOCH`]: which reach key (or, for a delegated write, which
    /// delegate key) the server must check the proof under. Bound by the
    /// signature.
    pub reach_key: Option<[u8; REACH_KEY_LEN]>,
}

/// Why a proof was refused. Every variant is a refusal; the server maps all of
/// them to one response so a caller learns nothing from which one it hit.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProofError {
    /// The header did not parse (wrong shape, wrong lengths, bad base64).
    #[error("proof header malformed")]
    Malformed,
    /// A version this crate does not speak.
    #[error("proof header version unknown")]
    UnknownVersion,
    /// `unix_ms` is outside the window around the verifier's clock.
    #[error("proof outside the time window")]
    OutsideWindow,
    /// The signature does not verify under the resolved key.
    #[error("proof signature invalid")]
    BadSignature,
}

/// The bytes a proof signs. Total and allocation-bounded by the inputs.
///
/// `reach_key` is the key a reach proof names; it is appended after the nonce
/// when present. [`sign`] passes it exactly for [`REACH_EPOCH`], and
/// [`verify`] refuses a proof that does not.
#[must_use]
pub fn canonical_bytes(
    req: &RequestParts<'_>,
    epoch: u64,
    unix_ms: u64,
    nonce: &[u8; NONCE_LEN],
    reach_key: Option<&[u8; REACH_KEY_LEN]>,
) -> Vec<u8> {
    let digest = hash::blake3(req.body);
    let mut out = Vec::with_capacity(
        CANONICAL_DOMAIN.len()
            + req.method.len()
            + req.canonical_path.len()
            + req.query.len()
            + digest.len()
            + 4
            + 8
            + 8
            + NONCE_LEN
            + REACH_KEY_LEN,
    );
    out.extend_from_slice(CANONICAL_DOMAIN);
    out.push(SEP);
    out.extend_from_slice(req.method.as_bytes());
    out.push(SEP);
    out.extend_from_slice(req.canonical_path.as_bytes());
    out.push(SEP);
    out.extend_from_slice(req.query.as_bytes());
    out.push(SEP);
    out.extend_from_slice(&digest);
    out.extend_from_slice(&epoch.to_be_bytes());
    out.extend_from_slice(&unix_ms.to_be_bytes());
    out.extend_from_slice(nonce);
    if let Some(key) = reach_key {
        out.extend_from_slice(key);
    }
    out
}

/// Sign a request under `key` for `epoch`. A reach proof ([`REACH_EPOCH`])
/// names `key`'s public half, inside the signed bytes.
#[must_use]
pub fn sign(
    key: &SigningKey,
    req: &RequestParts<'_>,
    epoch: u64,
    unix_ms: u64,
    nonce: [u8; NONCE_LEN],
) -> Proof {
    let reach_key = (epoch == REACH_EPOCH).then(|| key.verifying_key().to_bytes());
    let bytes = canonical_bytes(req, epoch, unix_ms, &nonce, reach_key.as_ref());
    Proof {
        epoch,
        unix_ms,
        nonce,
        signature: key.sign(&bytes).to_bytes(),
        reach_key,
    }
}

/// Verify `proof` over `req` under `key`, with `now_ms` the verifier's clock.
///
/// Checks the window before the signature so a stale proof costs no
/// signature verification. Resolving `key` from `proof.epoch` (or, for a
/// reach proof, from `proof.reach_key`) and refusing a replayed nonce are the
/// caller's. A reach proof must name exactly `key`; any other proof must name
/// none.
///
/// # Errors
///
/// [`ProofError::OutsideWindow`] or [`ProofError::BadSignature`].
pub fn verify(
    proof: &Proof,
    req: &RequestParts<'_>,
    key: &VerifyingKey,
    now_ms: u64,
) -> Result<(), ProofError> {
    if now_ms.abs_diff(proof.unix_ms) > PROOF_WINDOW_MS {
        return Err(ProofError::OutsideWindow);
    }
    let named_key_matches = match (proof.epoch == REACH_EPOCH, proof.reach_key) {
        (true, Some(named)) => named == key.to_bytes(),
        (false, None) => true,
        _ => false,
    };
    if !named_key_matches {
        return Err(ProofError::BadSignature);
    }
    let bytes = canonical_bytes(
        req,
        proof.epoch,
        proof.unix_ms,
        &proof.nonce,
        proof.reach_key.as_ref(),
    );
    let signature =
        Signature::from_bytes(&proof.signature).map_err(|_| ProofError::BadSignature)?;
    key.verify(&bytes, &signature)
        .map_err(|_| ProofError::BadSignature)
}

impl Proof {
    /// The header value.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut out = format!(
            "{VERSION}.{}.{}.{}.{}",
            self.epoch,
            self.unix_ms,
            URL_SAFE_NO_PAD.encode(self.nonce),
            URL_SAFE_NO_PAD.encode(self.signature),
        );
        if let Some(key) = self.reach_key {
            out.push('.');
            out.push_str(&URL_SAFE_NO_PAD.encode(key));
        }
        out
    }

    /// Parse a header value. Bounded (`MAX_HEADER_LEN`) and fail-closed:
    /// five dot-separated fields, decimal epoch and time, a 16-byte nonce and
    /// a 64-byte signature in base64url without padding - plus, exactly when
    /// the epoch is [`REACH_EPOCH`], a sixth: the 32-byte reach key. Nothing
    /// else.
    ///
    /// # Errors
    ///
    /// [`ProofError::UnknownVersion`] on a version this crate does not speak,
    /// [`ProofError::Malformed`] on anything else that does not parse.
    pub fn parse(value: &str) -> Result<Self, ProofError> {
        if value.len() > MAX_HEADER_LEN {
            return Err(ProofError::Malformed);
        }
        let mut parts = value.split('.');
        let version = parts.next().ok_or(ProofError::Malformed)?;
        let epoch = parts.next().ok_or(ProofError::Malformed)?;
        let unix_ms = parts.next().ok_or(ProofError::Malformed)?;
        let nonce = parts.next().ok_or(ProofError::Malformed)?;
        let signature = parts.next().ok_or(ProofError::Malformed)?;
        let reach_key = parts.next();
        if parts.next().is_some() {
            return Err(ProofError::Malformed);
        }
        if version != VERSION {
            return Err(ProofError::UnknownVersion);
        }
        let epoch = parse_decimal(epoch)?;
        let reach_key = match (epoch == REACH_EPOCH, reach_key) {
            (true, Some(field)) => {
                let bytes = URL_SAFE_NO_PAD
                    .decode(field)
                    .map_err(|_| ProofError::Malformed)?;
                let key: [u8; REACH_KEY_LEN] = bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| ProofError::Malformed)?;
                Some(key)
            },
            (false, None) => None,
            _ => return Err(ProofError::Malformed),
        };
        let unix_ms = parse_decimal(unix_ms)?;
        let nonce_bytes = URL_SAFE_NO_PAD
            .decode(nonce)
            .map_err(|_| ProofError::Malformed)?;
        let signature_bytes = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| ProofError::Malformed)?;
        let nonce: [u8; NONCE_LEN] = nonce_bytes
            .as_slice()
            .try_into()
            .map_err(|_| ProofError::Malformed)?;
        let signature: [u8; SIGNATURE_LEN] = signature_bytes
            .as_slice()
            .try_into()
            .map_err(|_| ProofError::Malformed)?;
        Ok(Self {
            epoch,
            unix_ms,
            nonce,
            signature,
            reach_key,
        })
    }
}

/// A strict decimal: 1..=20 ASCII digits, no sign, no leading `+`, fits a u64.
fn parse_decimal(field: &str) -> Result<u64, ProofError> {
    if field.is_empty() || field.len() > 20 || !field.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ProofError::Malformed);
    }
    field.parse::<u64>().map_err(|_| ProofError::Malformed)
}

/// The group access signing key for one epoch, from the 32 exporter bytes of
/// `Group::export_secret(ACCESS_KEY_LABEL, group_id, 32)`.
#[must_use]
pub fn access_signing_key(exporter_seed: &[u8; 32]) -> SigningKey {
    SigningKey::from_seed(exporter_seed)
}

/// Whether `identity_pk` is the key a mailbox pseudonym is named after
/// (`IdentityPseudonym::from_identity`, the ADR-0005 derivation). The server
/// runs this on a mailbox claim; the derivation is defined once in
/// `lakko-contacts` and used here rather than copied.
#[must_use]
pub fn mailbox_owner_matches(pseudonym: &[u8; 32], identity_pk: &VerifyingKey) -> bool {
    mailbox_pseudonym(identity_pk) == *pseudonym
}

/// The pseudonym of the mailbox `identity_pk` owns - the same derivation
/// [`mailbox_owner_matches`] checks, for a client naming its own mailbox.
#[must_use]
pub fn mailbox_pseudonym(identity_pk: &VerifyingKey) -> [u8; 32] {
    *IdentityPseudonym::from_identity(identity_pk).as_bytes()
}

/// The client half: one signer per journal per epoch, held by the client and
/// re-derived when the epoch moves.
///
/// Time and randomness are the caller's - [`Self::sign_at`] takes the clock
/// reading and the nonce - so this type is the same on the host, in the
/// browser (where `SystemTime` does not exist) and in an extension process;
/// each FFI surface supplies its own clock and CSPRNG.
#[derive(Clone)]
pub struct DsProofSigner {
    epoch: u64,
    key: SigningKey,
}

impl core::fmt::Debug for DsProofSigner {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DsProofSigner")
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

impl DsProofSigner {
    /// The group access signer for `epoch`, from the 32 exporter bytes of
    /// `Group::export_secret(ACCESS_KEY_LABEL, group_id, 32)` at that epoch.
    #[must_use]
    pub fn for_group(epoch: u64, exporter_seed: &[u8; 32]) -> Self {
        Self {
            epoch,
            key: access_signing_key(exporter_seed),
        }
    }

    /// The mailbox signer: the identity key itself, at [`MAILBOX_EPOCH`].
    #[must_use]
    pub fn for_mailbox(identity_seed: &[u8; 32]) -> Self {
        Self {
            epoch: MAILBOX_EPOCH,
            key: SigningKey::from_seed(identity_seed),
        }
    }

    /// A reach-key signer ([ADR-0058] section 2), at [`REACH_EPOCH`]: signs a
    /// write to a closed identity mailbox with a reach key the owner
    /// registered. A [`Delegation`]'s delegate signs with this too, over its
    /// own delegate seed. Its proofs name the signing key (the reach key, or
    /// the delegate key) automatically.
    ///
    /// [ADR-0058]: ../../../docs/adr/0058-scoped-discovery.md
    #[must_use]
    pub fn for_reach(reach_seed: &[u8; 32]) -> Self {
        Self {
            epoch: REACH_EPOCH,
            key: SigningKey::from_seed(reach_seed),
        }
    }

    /// The epoch this signer proves membership at.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The verifying key the server must hold for this signer's proofs to
    /// pass - what a claim registers.
    #[must_use]
    pub fn verifying_key(&self) -> VerifyingKey {
        self.key.verifying_key()
    }

    /// Sign `req` at `now_ms` with `nonce`, ready for [`Proof::encode`].
    #[must_use]
    pub fn sign_at(&self, req: &RequestParts<'_>, now_ms: u64, nonce: [u8; NONCE_LEN]) -> Proof {
        sign(&self.key, req, self.epoch, now_ms, nonce)
    }

    /// The JSON body of `PUT /ds/group/{hex}/access` registering this signer's
    /// key: `{"epoch":<n>,"access_pk_b64":"<standard base64>"}`. Defined here
    /// so no client spells the field names itself.
    #[must_use]
    pub fn group_claim_body_json(&self) -> String {
        format!(
            "{{\"epoch\":{},\"access_pk_b64\":\"{}\"}}",
            self.epoch,
            B64_STD.encode(self.verifying_key().to_bytes())
        )
    }

    /// The JSON body of `PUT /ds/mailbox/{hex}/access` claiming the mailbox
    /// this signer's identity key names: `{"identity_pk_b64":"<standard
    /// base64>"}`.
    #[must_use]
    pub fn mailbox_claim_body_json(&self) -> String {
        format!(
            "{{\"identity_pk_b64\":\"{}\"}}",
            B64_STD.encode(self.verifying_key().to_bytes())
        )
    }

    /// The pseudonym of the mailbox this signer's key names (meaningful for
    /// a [`Self::for_mailbox`] signer).
    #[must_use]
    pub fn mailbox_pseudonym(&self) -> [u8; 32] {
        mailbox_pseudonym(&self.verifying_key())
    }
}

/// Harness entry points for the fuzz crate. Not part of the supported API.
#[doc(hidden)]
pub mod fuzz_api {
    use super::{Proof, RequestParts, SigningKey};

    /// Parse the bytes as a header and, when they parse, verify the result
    /// against a fixed key and request, and (for a reach proof) against the
    /// key the proof names. Asserts that a parsed proof re-encodes to a value
    /// that parses back to itself, and that a reach proof names a key exactly
    /// when its epoch is the reach epoch. `true` when the parse succeeds.
    ///
    /// # Panics
    ///
    /// When either assertion fails - the fuzzer's finding.
    #[must_use]
    pub fn parse_and_verify(data: &[u8]) -> bool {
        let Ok(text) = core::str::from_utf8(data) else {
            return false;
        };
        let Ok(proof) = Proof::parse(text) else {
            return false;
        };
        assert!(
            Proof::parse(&proof.encode()).as_ref() == Ok(&proof),
            "encode/parse not stable"
        );
        assert!(
            (proof.epoch == super::REACH_EPOCH) == proof.reach_key.is_some(),
            "a reach key on a non-reach proof, or a reach proof without one"
        );
        let key = SigningKey::from_seed(&[0x5Au8; 32]).verifying_key();
        let req = RequestParts {
            method: "GET",
            canonical_path: "/ds/group/00/messages",
            query: "after=0",
            body: &[],
        };
        let _ = super::verify(&proof, &req, &key, proof.unix_ms);
        if let Some(named) = proof.reach_key {
            if let Ok(named) = super::VerifyingKey::from_bytes(&named) {
                let _ = super::verify(&proof, &req, &named, proof.unix_ms);
            }
        }
        true
    }

    /// Parse the bytes as a delegation and verify whatever parses.
    ///
    /// Raw, and as a header value when they are UTF-8, against a fixed
    /// mailbox and clock. Asserts round-trip stability: a delegation that decodes
    /// re-encodes to the identical bytes and header text. `true` when either
    /// form parses.
    ///
    /// # Panics
    ///
    /// When the round trip is not stable - the fuzzer's finding.
    #[must_use]
    pub fn delegation_parse(data: &[u8]) -> bool {
        const MAILBOX: [u8; 32] = [0x10; 32];
        const NOW_MS: u64 = 1_700_000_000_000;
        let mut parsed = false;
        if let Ok(d) = super::Delegation::decode(data) {
            parsed = true;
            assert!(d.encode().as_slice() == data, "decode/encode not stable");
            let _ = super::verify_delegation(&d, &MAILBOX, NOW_MS);
            let _ = super::verify_delegation(&d, &d.mailbox, d.expires_at_ms.saturating_sub(1));
            // Step 4 against a fixed delegate's proof: it verifies only when
            // the fuzzed delegation names that delegate.
            let delegate = super::DsProofSigner::for_reach(&[0x5Bu8; 32]);
            let req = RequestParts {
                method: "POST",
                canonical_path: "/ds/mailbox/00/messages",
                query: "",
                body: b"welcome",
            };
            let proof = delegate.sign_at(&req, NOW_MS, [0x5Cu8; super::NONCE_LEN]);
            let outcome = super::verify_delegated_proof(&d, &proof, &req, NOW_MS);
            assert!(
                outcome.is_ok() == (d.delegate_pk == delegate.verifying_key().to_bytes()),
                "a delegated proof verified for a delegate it does not name"
            );
        }
        if let Ok(text) = core::str::from_utf8(data) {
            if let Ok(d) = super::Delegation::parse_header_value(text) {
                parsed = true;
                assert!(d.to_header_value() == text, "header round trip not stable");
                let _ = super::verify_delegation(&d, &MAILBOX, NOW_MS);
            }
        }
        parsed
    }

    /// Decode the bytes as an ADR-0057 ownership statement and, when they
    /// parse, re-encode (the encoding is canonical, so the bytes must come
    /// back unchanged) and verify at the statement's own time. `true` when
    /// the decode succeeds. Panics only on a broken canonical-form property.
    #[must_use]
    pub fn decode_statement(data: &[u8]) -> bool {
        let Ok(statement) = super::SignedStatement::decode(data) else {
            return false;
        };
        assert_eq!(
            statement.encode(),
            data,
            "statement encoding is not canonical"
        );
        let _ = statement.verify(statement.unix_ms);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SigningKey {
        SigningKey::from_seed(&[0x11u8; 32])
    }

    fn req(body: &[u8]) -> RequestParts<'_> {
        RequestParts {
            method: "POST",
            canonical_path: "/ds/group/ab/messages",
            query: "",
            body,
        }
    }

    #[test]
    fn round_trips_through_the_header() {
        let k = key();
        let r = req(b"hello");
        let proof = sign(&k, &r, 7, 1_000_000, [9u8; NONCE_LEN]);
        let header = proof.encode();
        assert!(header.starts_with("v1.7.1000000."));
        let parsed = Proof::parse(&header).expect("parse");
        assert_eq!(parsed, proof);
        verify(&parsed, &r, &k.verifying_key(), 1_000_500).expect("verify");
    }

    #[test]
    fn every_bound_field_is_bound() {
        let k = key();
        let r = req(b"hello");
        let proof = sign(&k, &r, 7, 1_000_000, [9u8; NONCE_LEN]);
        let vk = k.verifying_key();
        let now = 1_000_000;
        let variants: [RequestParts<'_>; 4] = [
            RequestParts { method: "GET", ..r },
            RequestParts {
                canonical_path: "/ds/group/ab/knocks",
                ..r
            },
            RequestParts {
                query: "after=1",
                ..r
            },
            RequestParts {
                body: b"hellp",
                ..r
            },
        ];
        for v in &variants {
            assert_eq!(verify(&proof, v, &vk, now), Err(ProofError::BadSignature));
        }
        let mut other_epoch = proof.clone();
        other_epoch.epoch = 8;
        assert_eq!(
            verify(&other_epoch, &r, &vk, now),
            Err(ProofError::BadSignature)
        );
        let mut other_nonce = proof.clone();
        other_nonce.nonce[0] ^= 1;
        assert_eq!(
            verify(&other_nonce, &r, &vk, now),
            Err(ProofError::BadSignature)
        );
        let other_key = SigningKey::from_seed(&[0x22u8; 32]).verifying_key();
        assert_eq!(
            verify(&proof, &r, &other_key, now),
            Err(ProofError::BadSignature)
        );
    }

    #[test]
    fn the_window_is_checked_before_the_signature() {
        let k = key();
        let r = req(b"");
        let proof = sign(&k, &r, 0, 1_000_000, [0u8; NONCE_LEN]);
        let vk = k.verifying_key();
        verify(&proof, &r, &vk, 1_000_000 + PROOF_WINDOW_MS).expect("edge inside");
        verify(&proof, &r, &vk, 1_000_000 - PROOF_WINDOW_MS).expect("edge inside");
        assert_eq!(
            verify(&proof, &r, &vk, 1_000_000 + PROOF_WINDOW_MS + 1),
            Err(ProofError::OutsideWindow)
        );
        assert_eq!(
            verify(&proof, &r, &vk, 1_000_000 - PROOF_WINDOW_MS - 1),
            Err(ProofError::OutsideWindow)
        );
    }

    #[test]
    fn parse_is_bounded_and_fail_closed() {
        let good = sign(&key(), &req(b""), 1, 2, [3u8; NONCE_LEN]).encode();
        assert!(Proof::parse(&good).is_ok());
        assert_eq!(Proof::parse(""), Err(ProofError::Malformed));
        assert_eq!(
            Proof::parse("v2.1.2.AAAA.AAAA"),
            Err(ProofError::UnknownVersion)
        );
        assert_eq!(
            Proof::parse(&format!("{good}.extra")),
            Err(ProofError::Malformed)
        );
        assert_eq!(
            Proof::parse(&good.replacen("v1.1.", "v1.+1.", 1)),
            Err(ProofError::Malformed)
        );
        assert_eq!(
            Proof::parse(&good.replacen("v1.1.2.", "v1.1.99999999999999999999999.", 1)),
            Err(ProofError::Malformed)
        );
        let long = "v1.1.2.".to_string() + &"A".repeat(MAX_HEADER_LEN);
        assert_eq!(Proof::parse(&long), Err(ProofError::Malformed));
        // A nonce of the wrong length, a signature of the wrong length.
        assert_eq!(Proof::parse("v1.1.2.AAAA.AAAA"), Err(ProofError::Malformed));
    }

    const GOLDEN_CANONICAL_BLAKE3: &str =
        "881c504b2a652f3f768cc78f2904f780cb9a3409f76afc6d354daba91e32ab48";

    #[test]
    fn canonical_bytes_are_pinned() {
        let r = RequestParts {
            method: "GET",
            canonical_path: "/ds/group/00/messages",
            query: "after=0&wait=25",
            body: b"",
        };
        let bytes = canonical_bytes(&r, 3, 1_700_000_000_000, &[0xABu8; NONCE_LEN], None);
        // domain, NUL, method, NUL, path, NUL, query, NUL, blake3(""), epoch, ms, nonce
        let expected_len = CANONICAL_DOMAIN.len()
            + 1
            + 3
            + 1
            + r.canonical_path.len()
            + 1
            + r.query.len()
            + 1
            + 32
            + 8
            + 8
            + NONCE_LEN;
        assert_eq!(bytes.len(), expected_len);
        assert!(
            bytes.starts_with(b"lakko/ds/proof/v1\0GET\0/ds/group/00/messages\0after=0&wait=25\0")
        );
        assert_eq!(&bytes[bytes.len() - NONCE_LEN..], &[0xABu8; NONCE_LEN]);
        // The pinned golden: a change to the canonical layout must fail here,
        // not on a live server refusing every client.
        assert_eq!(hex(&hash::blake3(&bytes)), GOLDEN_CANONICAL_BLAKE3);
    }

    #[test]
    fn a_mailbox_is_named_after_its_identity_key() {
        let k = key();
        let vk = k.verifying_key();
        let pseudonym = *IdentityPseudonym::from_identity(&vk).as_bytes();
        assert!(mailbox_owner_matches(&pseudonym, &vk));
        let other = SigningKey::from_seed(&[0x33u8; 32]).verifying_key();
        assert!(!mailbox_owner_matches(&pseudonym, &other));
    }

    #[test]
    fn the_access_key_is_a_pure_function_of_the_exporter_seed() {
        let a = access_signing_key(&[0x44u8; 32]);
        let b = access_signing_key(&[0x44u8; 32]);
        assert_eq!(a.verifying_key().to_bytes(), b.verifying_key().to_bytes());
        let c = access_signing_key(&[0x45u8; 32]);
        assert_ne!(a.verifying_key().to_bytes(), c.verifying_key().to_bytes());
    }

    #[test]
    fn the_client_signer_produces_what_the_server_verifies() {
        let seed = [0x61u8; 32];
        let signer = DsProofSigner::for_group(7, &seed);
        let r = req(b"payload");
        let proof = signer.sign_at(&r, 1_000_000, [0x11u8; NONCE_LEN]);
        assert_eq!(proof.epoch, 7);
        let header = proof.encode();
        let parsed = Proof::parse(&header).unwrap();
        assert!(verify(&parsed, &r, &signer.verifying_key(), 1_000_500).is_ok());
        // The verifying key is the one `access_signing_key` derives.
        assert_eq!(
            signer.verifying_key().to_bytes(),
            access_signing_key(&seed).verifying_key().to_bytes()
        );
    }

    #[test]
    fn the_mailbox_signer_names_its_own_mailbox() {
        let identity_seed = [0x62u8; 32];
        let signer = DsProofSigner::for_mailbox(&identity_seed);
        assert_eq!(signer.epoch(), MAILBOX_EPOCH);
        let vk = SigningKey::from_seed(&identity_seed).verifying_key();
        assert_eq!(signer.mailbox_pseudonym(), mailbox_pseudonym(&vk));
        assert!(mailbox_owner_matches(
            &signer.mailbox_pseudonym(),
            &signer.verifying_key()
        ));
    }

    #[test]
    fn a_reach_proof_carries_the_reach_epoch() {
        let signer = DsProofSigner::for_reach(&[0x65u8; 32]);
        assert_eq!(signer.epoch(), REACH_EPOCH);
        assert_ne!(REACH_EPOCH, MAILBOX_EPOCH);
        let r = req(b"contact request");
        let proof = signer.sign_at(&r, 1_000_000, [0x12u8; NONCE_LEN]);
        assert_eq!(proof.epoch, u64::MAX);
        let parsed = Proof::parse(&proof.encode()).unwrap();
        assert_eq!(parsed.epoch, REACH_EPOCH);
        verify(&parsed, &r, &signer.verifying_key(), 1_000_000).unwrap();
        // The same key's proof under the mailbox label does not verify as a
        // reach proof: the epoch is bound.
        let mut relabelled = parsed;
        relabelled.epoch = MAILBOX_EPOCH;
        assert_eq!(
            verify(&relabelled, &r, &signer.verifying_key(), 1_000_000),
            Err(ProofError::BadSignature)
        );
    }

    const GOLDEN_REACH_CANONICAL_BLAKE3: &str =
        "c529bd512e98d4e95d4bbad64fd43ec183e4dbb93589ae58c473e813af091a0b";

    #[test]
    fn a_reach_proof_names_its_key_inside_the_signed_bytes() {
        let r = RequestParts {
            method: "POST",
            canonical_path: "/ds/mailbox/00/messages",
            query: "",
            body: b"",
        };
        let key = [0xCDu8; REACH_KEY_LEN];
        let bytes = canonical_bytes(
            &r,
            REACH_EPOCH,
            1_700_000_000_000,
            &[0xABu8; NONCE_LEN],
            Some(&key),
        );
        let without = canonical_bytes(
            &r,
            REACH_EPOCH,
            1_700_000_000_000,
            &[0xABu8; NONCE_LEN],
            None,
        );
        assert_eq!(bytes.len(), without.len() + REACH_KEY_LEN);
        assert_eq!(&bytes[bytes.len() - REACH_KEY_LEN..], &key);
        assert_eq!(hex(&hash::blake3(&bytes)), GOLDEN_REACH_CANONICAL_BLAKE3);

        // The header carries the key as a sixth field.
        let signer = DsProofSigner::for_reach(&[0x66u8; 32]);
        let proof = signer.sign_at(&r, 1_000_000, [0x13u8; NONCE_LEN]);
        let vk = signer.verifying_key();
        assert_eq!(proof.reach_key, Some(vk.to_bytes()));
        let header = proof.encode();
        assert_eq!(header.split('.').count(), 6);
        assert!(header.ends_with(&URL_SAFE_NO_PAD.encode(vk.to_bytes())));
        assert!(header.len() <= MAX_HEADER_LEN);
        let parsed = Proof::parse(&header).unwrap();
        assert_eq!(parsed, proof);
        verify(&parsed, &r, &vk, 1_000_000).unwrap();

        // Swapping the named key for another (even a registered one) breaks
        // the proof: it names a key it is not checked under, or a key whose
        // signature it does not carry.
        let other = SigningKey::from_seed(&[0x67u8; 32]).verifying_key();
        let mut swapped = parsed.clone();
        swapped.reach_key = Some(other.to_bytes());
        assert_eq!(
            verify(&swapped, &r, &other, 1_000_000),
            Err(ProofError::BadSignature)
        );
        assert_eq!(
            verify(&swapped, &r, &vk, 1_000_000),
            Err(ProofError::BadSignature)
        );
        // Checked under a key it does not name.
        assert_eq!(
            verify(&parsed, &r, &other, 1_000_000),
            Err(ProofError::BadSignature)
        );
        // A reach proof with the key stripped.
        let mut stripped = parsed;
        stripped.reach_key = None;
        assert_eq!(
            verify(&stripped, &r, &vk, 1_000_000),
            Err(ProofError::BadSignature)
        );
    }

    #[test]
    fn the_reach_key_field_is_present_exactly_on_reach_proofs() {
        let r = req(b"x");
        let reach = DsProofSigner::for_reach(&[0x68u8; 32]).sign_at(&r, 5, [1u8; NONCE_LEN]);
        let header = reach.encode();
        let (five, _) = header.rsplit_once('.').unwrap();
        // A reach proof without the key.
        assert_eq!(Proof::parse(five), Err(ProofError::Malformed));
        // A group proof with one.
        let group = sign(&key(), &r, 7, 5, [2u8; NONCE_LEN]).encode();
        let key_field = URL_SAFE_NO_PAD.encode([0u8; REACH_KEY_LEN]);
        assert_eq!(
            Proof::parse(&format!("{group}.{key_field}")),
            Err(ProofError::Malformed)
        );
        // A key field of the wrong length, or a seventh field.
        assert_eq!(
            Proof::parse(&format!("{five}.AAAA")),
            Err(ProofError::Malformed)
        );
        assert_eq!(
            Proof::parse(&format!("{header}.{key_field}")),
            Err(ProofError::Malformed)
        );
        // A group proof's header is unchanged: five fields.
        assert_eq!(group.split('.').count(), 5);
    }

    #[test]
    fn a_delegate_writes_with_a_reach_signer_over_its_own_seed() {
        let mailbox = mailbox_pseudonym(&SigningKey::from_seed(&[0x70u8; 32]).verifying_key());
        let reach_seed = [0x71u8; 32];
        let delegate = DsProofSigner::for_reach(&[0x72u8; 32]);
        let d = sign_delegation(
            &reach_seed,
            &mailbox,
            &delegate.verifying_key().to_bytes(),
            1_000_000 + 60_000,
            [0x73u8; NONCE_LEN],
        );
        verify_delegation(&d, &mailbox, 1_000_000).unwrap();
        let r = req(b"welcome");
        let proof = delegate.sign_at(&r, 1_000_000, [0x74u8; NONCE_LEN]);
        let delegate_vk = VerifyingKey::from_bytes(&d.delegate_pk).unwrap();
        verify(&proof, &r, &delegate_vk, 1_000_000).unwrap();
        // The delegate's proof names the delegate key.
        assert_eq!(proof.reach_key, Some(d.delegate_pk));
        verify_delegated_proof(&d, &proof, &r, 1_000_000).unwrap();
        // A proof by the issuer itself is not the delegate's.
        let issuer_proof =
            DsProofSigner::for_reach(&reach_seed).sign_at(&r, 1_000_000, [0x75u8; NONCE_LEN]);
        assert_eq!(
            verify_delegated_proof(&d, &issuer_proof, &r, 1_000_000),
            Err(DelegationError::NotTheDelegate)
        );
        assert_eq!(
            d.issuer_pk,
            DsProofSigner::for_reach(&reach_seed)
                .verifying_key()
                .to_bytes()
        );
    }

    #[test]
    fn claim_bodies_carry_the_fields_the_ds_reads() {
        let signer = DsProofSigner::for_group(3, &[0x63u8; 32]);
        let body = signer.group_claim_body_json();
        let pk = B64_STD.encode(signer.verifying_key().to_bytes());
        assert_eq!(body, format!("{{\"epoch\":3,\"access_pk_b64\":\"{pk}\"}}"));
        let mailbox = DsProofSigner::for_mailbox(&[0x64u8; 32]);
        let pk = B64_STD.encode(mailbox.verifying_key().to_bytes());
        assert_eq!(
            mailbox.mailbox_claim_body_json(),
            format!("{{\"identity_pk_b64\":\"{pk}\"}}")
        );
        // Standard base64 with padding is what the server decodes.
        assert!(pk.ends_with('=') || pk.len() % 4 == 0);
    }

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
    }
}
