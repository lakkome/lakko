// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Contacts of contacts: introduction records and the envelopes that relay
//! them ([ADR-0060](../../../docs/adr/0060-scoped-discovery.md) §4).
//!
//! ## The flow
//!
//! When "Contacts of my contacts" is on, the owner (Alice) builds ONE
//! **introduction record** and sends it to each contact in their 1:1
//! conversation as an [introduction offer](compose_introduction_offer). Each
//! contact (Carol) keeps the newest record per contact who offered one and
//! sends her own contacts (Bob) an [introductions bundle](compose_bundle):
//! every record she holds, padded to a size bucket. Bob, searching `anna`,
//! computes the server OPRF of what he typed once, derives the tag, finds it
//! in a bundle, opens the record and learns Alice's identity key, account
//! id, name, username certificate and `fof` reach grant.
//!
//! ## Search terms, and why the OPRF
//!
//! A record is findable by a list of **search terms**, each supplied as its
//! 64-byte server OPRF output: the username's, and one per name search term
//! (every prefix of 3 to 16 characters of every word of the normalised name,
//! a shorter word whole). The terms are computed by `lakko-discovery-client`;
//! **this module never sees a plaintext username or name for tagging**.
//!
//! Usernames and names are low-entropy. Keyed on the plain string, a bundle
//! would let Bob recover Carol's contacts offline from a dictionary. Keyed on
//! the server's OPRF output, each guess costs an online, token-rationed
//! evaluation (ADR-0054).
//!
//! ```text
//! tag = BLAKE3-derive-key("lakko/fof/tag/v1", oprf_output)
//! kek = BLAKE3-derive-key("lakko/fof/key/v1", oprf_output)
//! ```
//!
//! ## Record wire (v1)
//!
//! The payload is sealed ONCE under a random content key, and the content
//! key is wrapped once per term, so a record with many terms does not carry
//! the payload many times.
//!
//! ```text
//! record         = version(1)=0x01 || issued_at_ms(8, BE)
//!                  || entry_count(1: 1..=MAX_INTRODUCTION_TERMS)
//!                  || entry_count x [ tag(32) || wrapped_key(60) ]
//!                  || sealed_payload_len(2, BE) || sealed_payload
//! wrapped_key    = nonce(12) || ChaCha20-Poly1305(kek, aad = tag, content_key(32))
//! sealed_payload = nonce(12) || ChaCha20-Poly1305(content_key, aad = header, payload)
//! header         = version(1) || issued_at_ms(8, BE) || entry_count(1) || every tag
//! ```
//!
//! `lakko_crypto::aead::seal_with_key` takes no associated data, so this
//! module seals with [`lakko_crypto::aead::seal`] and a fresh random nonce,
//! producing the same `nonce || ciphertext` shape (a wrapped key is exactly
//! what `seal_with_key` would output for a 32-byte plaintext: 60 bytes) and
//! binding:
//!
//! * each wrapped key to its own tag, so a wrapped key moved under another
//!   tag fails to unwrap;
//! * the payload to the version, `issued_at_ms` and the whole tag list, so a
//!   relaying contact cannot re-date a record or add, drop or reorder its
//!   terms without the content key.
//!
//! Tags within one record are distinct ([`compose_introduction`] refuses a
//! duplicate OPRF output rather than silently dropping it, since a duplicate
//! means the caller built its term list wrong). Tags across records need not
//! be: two people named Anna share the `ann` tag, and a searcher opens every
//! matching record.
//!
//! [`MAX_INTRODUCTION_TERMS`] is 65 (64 name terms plus the username):
//! chosen, not measured.
//!
//! ## Payload wire (v2)
//!
//! ```text
//! payload = version(1)=0x02 || identity_pk(32) || account_id(16)
//!           || name_len(1) || name || username_len(1) || username
//!           || cert_len(1: 0 | 153) || cert
//!           || fof_len(1: 0 | 37) || fof_grant
//! ```
//!
//! Field rules are the profile's ([`crate::profile`]): the display-name
//! rule, the canonical-username rule, the certificate length and "a
//! certificate needs a username"; a grant, when present, must be of class
//! [`ReachClass::Fof`]. Version 1 (grant always present) is refused: a flag
//! day, nothing is deployed.
//!
//! ## Two uses of one record: contacts of contacts, and Everyone
//!
//! The same record shape serves two uses, and the grant is what tells them
//! apart. The rule is enforced on both sides of each use, never left to the
//! caller:
//!
//! | Use | Compose | Open | Grant |
//! |---|---|---|---|
//! | Contacts of contacts (section 4) | [`compose_introduction`] | [`open_introduction`] | required |
//! | Everyone's public record (section 5) | [`compose_public_record`] | [`open_public_record`] | refused |
//!
//! A public record carries no `fof` grant: the mailbox of a person in
//! Everyone is open, so none is needed, and a grant handed to the world
//! would outlive leaving Everyone. A contacts-of-contacts record without a
//! grant is useless to its reader (the mailbox is closed to them), so both
//! the composer and the opener refuse one.
//!
//! ## Bundles and padding
//!
//! [`compose_bundle`] pads the record count up to the next of
//! [`BUNDLE_SIZE_BUCKETS`] with dummy records that are structurally valid:
//! each copies the entry count and the sealed-payload length of a randomly
//! chosen real record in the bundle, carries random tags, random wrapped
//! keys and random sealed bytes, and an `issued_at_ms` drawn between the
//! oldest and the newest real record; then the whole list is shuffled.
//! Without an OPRF output that matches, a dummy and a real record differ
//! only in bytes that are indistinguishable from random. A dummy never
//! opens: its tags match no OPRF output except with probability 2^-256.
//!
//! **Residual, recorded:** `issued_at_ms` and the entry count are in the
//! clear (the record layout the ADR plan fixed), so a receiver sees their
//! spread across the real records, and with exactly one real record no dummy
//! is needed because the bucket is 1. The entry count also says roughly how
//! long the owner's name is. Both are left for the S6 security pass.
//!
//! [`MAX_BUNDLE_RECORDS`] (1024) is chosen, not measured; with the longest
//! records it bounds a bundle at about 6.7 MB.

use core::result::Result as CoreResult;

use lakko_crypto::aead::{self, Algorithm, NONCE_LEN, TAG_LEN};
use lakko_crypto::hash::blake3_derive_key_from_material;
use lakko_crypto::{random, SecretBytes};
use thiserror::Error;

use crate::profile::{check_display_name, DisplayNameError, ACCOUNT_ID_LEN, USERNAME_CERT_LEN};
use crate::reach::{ReachClass, ReachError, ReachGrant, REACH_GRANT_LEN};
use crate::username::{check_canonical_username, UsernameError};

/// BLAKE3 derive-key context for a record tag.
pub const FOF_TAG_DOMAIN: &str = "lakko/fof/tag/v1";

/// BLAKE3 derive-key context for the key-encryption key a record's content
/// key is wrapped under, per term.
pub const FOF_KEY_DOMAIN: &str = "lakko/fof/key/v1";

/// Length of a server OPRF output (RFC 9497, Ristretto255-SHA512).
pub const OPRF_OUTPUT_LEN: usize = 64;

/// Length of a record tag.
pub const FOF_TAG_LEN: usize = 32;

/// Length of a record's content key.
pub const CONTENT_KEY_LEN: usize = 32;

/// Length of a wrapped content key: nonce, the 32-byte key, the AEAD tag.
pub const WRAPPED_KEY_LEN: usize = NONCE_LEN + CONTENT_KEY_LEN + TAG_LEN;

/// Length of an identity public key in a payload.
pub const IDENTITY_PK_LEN: usize = 32;

/// Payload schema version (v2: the `fof` grant became optional).
pub const INTRODUCTION_PAYLOAD_VERSION_V2: u8 = 0x02;

/// Record schema version.
pub const INTRODUCTION_RECORD_VERSION_V1: u8 = 0x01;

/// Most search terms (entries) one record carries: 64 name terms plus the
/// username. Chosen, not measured.
pub const MAX_INTRODUCTION_TERMS: usize = 65;

/// Shortest payload: no name, no username, no certificate, no grant (a
/// public record's shape).
pub const INTRODUCTION_PAYLOAD_MIN_LEN: usize =
    1 + IDENTITY_PK_LEN + ACCOUNT_ID_LEN + 1 + 1 + 1 + 1;

/// Longest payload: a maximal name, username, a certificate and a grant.
pub const INTRODUCTION_PAYLOAD_MAX_LEN: usize = INTRODUCTION_PAYLOAD_MIN_LEN
    + crate::profile::DISPLAY_NAME_MAX_BYTES
    + crate::username::USERNAME_MAX_LEN
    + USERNAME_CERT_LEN
    + REACH_GRANT_LEN;

/// Shortest sealed payload: nonce, AEAD tag, the shortest payload.
pub const SEALED_PAYLOAD_MIN_LEN: usize = NONCE_LEN + TAG_LEN + INTRODUCTION_PAYLOAD_MIN_LEN;

/// Longest sealed payload: nonce, AEAD tag, the longest payload.
pub const SEALED_PAYLOAD_MAX_LEN: usize = NONCE_LEN + TAG_LEN + INTRODUCTION_PAYLOAD_MAX_LEN;

/// Record header: version, `issued_at_ms`, `entry_count`.
const RECORD_HEADER_LEN: usize = 1 + 8 + 1;

/// One entry: tag and wrapped key.
const ENTRY_LEN: usize = FOF_TAG_LEN + WRAPPED_KEY_LEN;

/// Shortest record: one entry and the shortest sealed payload.
pub const INTRODUCTION_RECORD_MIN_LEN: usize =
    RECORD_HEADER_LEN + ENTRY_LEN + 2 + SEALED_PAYLOAD_MIN_LEN;

/// Longest record: every term and the longest sealed payload.
pub const INTRODUCTION_RECORD_MAX_LEN: usize =
    RECORD_HEADER_LEN + MAX_INTRODUCTION_TERMS * ENTRY_LEN + 2 + SEALED_PAYLOAD_MAX_LEN;

/// Domain separator of the introduction offer (owner to contact: "relay
/// this"). Frozen: [`crate::incoming_envelope`] dispatches on it.
pub const INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/introduction-offer/v1\x00";

/// Offer schema version.
pub const INTRODUCTION_OFFER_VERSION_V1: u8 = 0x01;

/// Domain separator of the introductions bundle (contact to contacts).
/// Frozen: [`crate::incoming_envelope`] dispatches on it.
pub const INTRODUCTIONS_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/introductions/v1\x00";

/// Bundle schema version.
pub const INTRODUCTIONS_VERSION_V1: u8 = 0x01;

/// Most records one bundle carries, padding included. Chosen, not
/// measured.
pub const MAX_BUNDLE_RECORDS: usize = 1024;

/// The record counts a bundle may carry. A bundle is padded up to the next
/// bucket, and a decoded bundle whose count is not a bucket is refused.
pub const BUNDLE_SIZE_BUCKETS: [usize; 6] = [1, 4, 16, 64, 256, MAX_BUNDLE_RECORDS];

/// What an introduction record reveals to whoever holds a matching OPRF
/// output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntroductionPayload {
    /// The owner's identity (account) public key.
    pub identity_pk: [u8; IDENTITY_PK_LEN],
    /// The owner's account id.
    pub account_id: [u8; ACCOUNT_ID_LEN],
    /// The owner's display name, per [`check_display_name`].
    pub display_name: Option<String>,
    /// The owner's canonical username.
    pub username: Option<String>,
    /// The discovery-signed username certificate, exactly
    /// [`USERNAME_CERT_LEN`] bytes, opaque here. Requires `username`.
    pub username_certificate: Option<Vec<u8>>,
    /// The `fof` reach grant, class [`ReachClass::Fof`]. Required in a
    /// contacts-of-contacts record, refused in a public record (module
    /// docs, "Two uses of one record").
    pub fof: Option<ReachGrant>,
}

impl IntroductionPayload {
    /// Check every bound. The rule [`IntroductionPayload::encode`] and
    /// [`IntroductionPayload::decode`] share.
    ///
    /// # Errors
    ///
    /// The field errors of [`IntroductionError`].
    pub fn validate(&self) -> CoreResult<(), IntroductionError> {
        if let Some(name) = &self.display_name {
            check_display_name(name).map_err(IntroductionError::DisplayName)?;
        }
        if let Some(username) = &self.username {
            check_canonical_username(username).map_err(IntroductionError::Username)?;
        }
        if let Some(cert) = &self.username_certificate {
            if cert.len() != USERNAME_CERT_LEN {
                return Err(IntroductionError::CertificateLength { got: cert.len() });
            }
            if self.username.is_none() {
                return Err(IntroductionError::CertificateWithoutUsername);
            }
        }
        if let Some(fof) = &self.fof {
            if fof.class != ReachClass::Fof {
                return Err(IntroductionError::NotFofGrant(fof.class.to_byte()));
            }
        }
        Ok(())
    }

    /// Encode the payload plaintext.
    ///
    /// # Errors
    ///
    /// Exactly what [`IntroductionPayload::validate`] returns.
    pub fn encode(&self) -> CoreResult<Vec<u8>, IntroductionError> {
        self.validate()?;
        let name = self.display_name.as_deref().unwrap_or("").as_bytes();
        let username = self.username.as_deref().unwrap_or("").as_bytes();
        let cert = self.username_certificate.as_deref().unwrap_or(&[]);
        let mut out = Vec::with_capacity(INTRODUCTION_PAYLOAD_MAX_LEN);
        out.push(INTRODUCTION_PAYLOAD_VERSION_V2);
        out.extend_from_slice(&self.identity_pk);
        out.extend_from_slice(&self.account_id);
        push_len_u8(&mut out, name.len())?;
        out.extend_from_slice(name);
        push_len_u8(&mut out, username.len())?;
        out.extend_from_slice(username);
        push_len_u8(&mut out, cert.len())?;
        out.extend_from_slice(cert);
        match &self.fof {
            Some(fof) => {
                push_len_u8(&mut out, REACH_GRANT_LEN)?;
                out.extend_from_slice(&fof.encode());
            },
            None => out.push(0),
        }
        Ok(out)
    }

    /// Decode a payload plaintext, fail-closed.
    ///
    /// # Errors
    ///
    /// Framing errors ([`IntroductionError::Truncated`],
    /// [`IntroductionError::TrailingBytes`],
    /// [`IntroductionError::UnknownVersion`]) and every field error.
    pub fn decode(bytes: &[u8]) -> CoreResult<Self, IntroductionError> {
        let mut cur = bytes;
        let version = take_u8(&mut cur)?;
        if version != INTRODUCTION_PAYLOAD_VERSION_V2 {
            return Err(IntroductionError::UnknownVersion(version));
        }
        let identity_pk = take_array::<IDENTITY_PK_LEN>(&mut cur)?;
        let account_id = take_array::<ACCOUNT_ID_LEN>(&mut cur)?;
        let name_len = usize::from(take_u8(&mut cur)?);
        let name_bytes = take(&mut cur, name_len)?;
        let username_len = usize::from(take_u8(&mut cur)?);
        let username_bytes = take(&mut cur, username_len)?;
        let cert_len = usize::from(take_u8(&mut cur)?);
        let username_certificate = match cert_len {
            0 => None,
            USERNAME_CERT_LEN => Some(take(&mut cur, USERNAME_CERT_LEN)?.to_vec()),
            got => return Err(IntroductionError::CertificateLength { got }),
        };
        let fof = match usize::from(take_u8(&mut cur)?) {
            0 => None,
            REACH_GRANT_LEN => Some(
                ReachGrant::decode(take(&mut cur, REACH_GRANT_LEN)?)
                    .map_err(IntroductionError::Grant)?,
            ),
            got => return Err(IntroductionError::GrantLength { got }),
        };
        if !cur.is_empty() {
            return Err(IntroductionError::TrailingBytes { count: cur.len() });
        }
        let display_name = if name_bytes.is_empty() {
            None
        } else {
            Some(
                core::str::from_utf8(name_bytes)
                    .map_err(|_| IntroductionError::DisplayName(DisplayNameError::NotUtf8))?
                    .to_string(),
            )
        };
        let username = if username_bytes.is_empty() {
            None
        } else {
            Some(
                core::str::from_utf8(username_bytes)
                    .map_err(|_| IntroductionError::Username(UsernameError::InvalidCharacters))?
                    .to_string(),
            )
        };
        let payload = Self {
            identity_pk,
            account_id,
            display_name,
            username,
            username_certificate,
            fof,
        };
        payload.validate()?;
        Ok(payload)
    }
}

/// One search term of a record: the tag a searcher matches, and the content
/// key wrapped under the key-encryption key derived from the same OPRF
/// output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntroductionEntry {
    /// `BLAKE3-derive-key("lakko/fof/tag/v1", oprf_output)`.
    pub tag: [u8; FOF_TAG_LEN],
    /// `nonce || ChaCha20-Poly1305(kek, aad = tag, content_key)`.
    pub wrapped_key: [u8; WRAPPED_KEY_LEN],
}

/// A parsed introduction record. Parsing checks the structure only; the
/// payload stays sealed until [`IntroductionRecord::open`] is given a
/// matching OPRF output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntroductionRecord {
    /// When the owner built the record (owner clock, UNIX ms). Bound into
    /// the payload's AAD.
    pub issued_at_ms: u64,
    /// 1 to [`MAX_INTRODUCTION_TERMS`] entries with distinct tags.
    pub entries: Vec<IntroductionEntry>,
    /// `nonce || ChaCha20-Poly1305(content_key, aad = header, payload)`,
    /// [`SEALED_PAYLOAD_MIN_LEN`] to [`SEALED_PAYLOAD_MAX_LEN`] bytes.
    pub sealed_payload: Vec<u8>,
}

impl IntroductionRecord {
    /// Check the structure: 1 to [`MAX_INTRODUCTION_TERMS`] entries,
    /// distinct tags, the sealed-payload length in bounds.
    ///
    /// # Errors
    ///
    /// [`IntroductionError::BadEntryCount`], [`IntroductionError::DuplicateTag`],
    /// [`IntroductionError::SealedLength`].
    pub fn validate(&self) -> CoreResult<(), IntroductionError> {
        check_entry_count(self.entries.len())?;
        check_sealed_len(self.sealed_payload.len())?;
        check_distinct_tags(self.entries.iter().map(|e| &e.tag))?;
        Ok(())
    }

    /// Encode the record wire.
    ///
    /// # Errors
    ///
    /// Exactly what [`IntroductionRecord::validate`] returns.
    pub fn encode(&self) -> CoreResult<Vec<u8>, IntroductionError> {
        self.validate()?;
        let mut out = Vec::with_capacity(
            RECORD_HEADER_LEN + self.entries.len() * ENTRY_LEN + 2 + self.sealed_payload.len(),
        );
        out.push(INTRODUCTION_RECORD_VERSION_V1);
        out.extend_from_slice(&self.issued_at_ms.to_be_bytes());
        out.push(entry_count_byte(self.entries.len())?);
        for entry in &self.entries {
            out.extend_from_slice(&entry.tag);
            out.extend_from_slice(&entry.wrapped_key);
        }
        let len = u16::try_from(self.sealed_payload.len()).map_err(|_| {
            IntroductionError::SealedLength {
                got: self.sealed_payload.len(),
            }
        })?;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&self.sealed_payload);
        Ok(out)
    }

    /// Decode a record wire, fail-closed. Structure only; nothing is opened.
    ///
    /// # Errors
    ///
    /// Framing errors and every error of [`IntroductionRecord::validate`].
    pub fn decode(bytes: &[u8]) -> CoreResult<Self, IntroductionError> {
        let mut cur = bytes;
        let record = read_record(&mut cur)?;
        if !cur.is_empty() {
            return Err(IntroductionError::TrailingBytes { count: cur.len() });
        }
        Ok(record)
    }

    /// The tags a searcher matches against.
    #[must_use]
    pub fn tags(&self) -> Vec<[u8; FOF_TAG_LEN]> {
        self.entries.iter().map(|e| e.tag).collect()
    }

    /// Open the record with the entry whose tag `oprf_output` derives.
    ///
    /// * `Ok(None)` — no entry matches: this record is not the person
    ///   searched for (or it is a padding dummy).
    /// * `Ok(Some(payload))` — the key unwrapped, the payload opened, and it
    ///   is valid.
    ///
    /// # Errors
    ///
    /// * [`IntroductionError::Open`] — a tag matched but the wrapped key or
    ///   the payload failed to authenticate. A matching tag is a claim the
    ///   record makes about the OPRF output; failing to back it is an error,
    ///   never a miss.
    /// * Every payload error of [`IntroductionPayload::decode`].
    pub fn open(
        &self,
        oprf_output: &[u8; OPRF_OUTPUT_LEN],
    ) -> CoreResult<Option<IntroductionPayload>, IntroductionError> {
        let tag = fof_tag(oprf_output);
        let Some(entry) = self.entries.iter().find(|e| e.tag == tag) else {
            return Ok(None);
        };
        let content_key = open_sealed(&fof_kek(oprf_output), &tag, &entry.wrapped_key)?;
        let content_key: [u8; CONTENT_KEY_LEN] = content_key
            .as_slice()
            .try_into()
            .map_err(|_| IntroductionError::Open)?;
        let content_key = SecretBytes::new(content_key);
        let header = payload_aad(self.issued_at_ms, &self.tags())?;
        let plaintext = open_sealed(&content_key, &header, &self.sealed_payload)?;
        IntroductionPayload::decode(&plaintext).map(Some)
    }
}

/// The tag a record entry is found by.
#[must_use]
pub fn fof_tag(oprf_output: &[u8; OPRF_OUTPUT_LEN]) -> [u8; FOF_TAG_LEN] {
    blake3_derive_key_from_material(FOF_TAG_DOMAIN, oprf_output)
}

fn fof_kek(oprf_output: &[u8; OPRF_OUTPUT_LEN]) -> SecretBytes<32> {
    SecretBytes::new(blake3_derive_key_from_material(FOF_KEY_DOMAIN, oprf_output))
}

/// `version || issued_at_ms || entry_count || every tag`: what the payload
/// is bound to.
fn payload_aad(
    issued_at_ms: u64,
    tags: &[[u8; FOF_TAG_LEN]],
) -> CoreResult<Vec<u8>, IntroductionError> {
    let mut aad = Vec::with_capacity(RECORD_HEADER_LEN + tags.len() * FOF_TAG_LEN);
    aad.push(INTRODUCTION_RECORD_VERSION_V1);
    aad.extend_from_slice(&issued_at_ms.to_be_bytes());
    aad.push(entry_count_byte(tags.len())?);
    for tag in tags {
        aad.extend_from_slice(tag);
    }
    Ok(aad)
}

/// `nonce || ChaCha20-Poly1305(key, aad, plaintext)` with a fresh nonce.
fn seal_bound(
    key: &SecretBytes<32>,
    aad: &[u8],
    plaintext: &[u8],
) -> CoreResult<Vec<u8>, IntroductionError> {
    let mut nonce = [0u8; NONCE_LEN];
    random::fill(&mut nonce);
    let ciphertext = aead::seal(Algorithm::ChaCha20Poly1305, key, &nonce, aad, plaintext)
        .map_err(|_| IntroductionError::Seal)?;
    let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Inverse of [`seal_bound`]; any failure is [`IntroductionError::Open`].
fn open_sealed(
    key: &SecretBytes<32>,
    aad: &[u8],
    sealed: &[u8],
) -> CoreResult<Vec<u8>, IntroductionError> {
    if sealed.len() < NONCE_LEN + TAG_LEN {
        return Err(IntroductionError::Open);
    }
    let (nonce_bytes, ciphertext) = sealed.split_at(NONCE_LEN);
    let nonce: [u8; NONCE_LEN] = nonce_bytes
        .try_into()
        .map_err(|_| IntroductionError::Open)?;
    aead::open(Algorithm::ChaCha20Poly1305, key, &nonce, aad, ciphertext)
        .map_err(|_| IntroductionError::Open)
}

/// Build a contacts-of-contacts introduction record findable by each search
/// term supplied. The payload must carry its `fof` grant.
///
/// `term_oprfs` is the username's OPRF output (if the owner has one) plus
/// one per name search term, computed by `lakko-discovery-client`. The order
/// is kept.
///
/// # Errors
///
/// * [`IntroductionError::MissingFofGrant`] — the payload carries no grant.
/// * [`IntroductionError::NoTerms`] — the list is empty.
/// * [`IntroductionError::TooManyTerms`] — more than
///   [`MAX_INTRODUCTION_TERMS`].
/// * [`IntroductionError::DuplicateTag`] — two equal OPRF outputs. Refused
///   rather than deduplicated: equal outputs mean the caller's term list is
///   wrong, and silently dropping one would hide that.
/// * Every error of [`IntroductionPayload::encode`].
/// * [`IntroductionError::Seal`] — the AEAD refused (not expected).
pub fn compose_introduction(
    payload: &IntroductionPayload,
    term_oprfs: &[[u8; OPRF_OUTPUT_LEN]],
    issued_at_ms: u64,
) -> CoreResult<Vec<u8>, IntroductionError> {
    if payload.fof.is_none() {
        return Err(IntroductionError::MissingFofGrant);
    }
    compose_record(payload, term_oprfs, issued_at_ms)
}

/// Build the public record a person in Everyone publishes in discovery
/// (ADR-0060 section 5), findable by each search term supplied: the
/// username's OPRF output plus one per name search term. The payload must
/// carry NO `fof` grant.
///
/// # Errors
///
/// * [`IntroductionError::UnexpectedFofGrant`] — the payload carries a grant.
/// * Every other error of [`compose_introduction`].
pub fn compose_public_record(
    payload: &IntroductionPayload,
    term_oprfs: &[[u8; OPRF_OUTPUT_LEN]],
    issued_at_ms: u64,
) -> CoreResult<Vec<u8>, IntroductionError> {
    if payload.fof.is_some() {
        return Err(IntroductionError::UnexpectedFofGrant);
    }
    compose_record(payload, term_oprfs, issued_at_ms)
}

fn compose_record(
    payload: &IntroductionPayload,
    term_oprfs: &[[u8; OPRF_OUTPUT_LEN]],
    issued_at_ms: u64,
) -> CoreResult<Vec<u8>, IntroductionError> {
    if term_oprfs.is_empty() {
        return Err(IntroductionError::NoTerms);
    }
    if term_oprfs.len() > MAX_INTRODUCTION_TERMS {
        return Err(IntroductionError::TooManyTerms {
            count: term_oprfs.len(),
        });
    }
    let tags: Vec<[u8; FOF_TAG_LEN]> = term_oprfs.iter().map(fof_tag).collect();
    check_distinct_tags(tags.iter())?;
    let plaintext = payload.encode()?;
    let content_key = random::secret_bytes::<CONTENT_KEY_LEN>();
    let mut entries = Vec::with_capacity(term_oprfs.len());
    for (oprf, tag) in term_oprfs.iter().zip(&tags) {
        let wrapped = seal_bound(&fof_kek(oprf), tag, content_key.as_bytes())?;
        let wrapped_key: [u8; WRAPPED_KEY_LEN] = wrapped
            .as_slice()
            .try_into()
            .map_err(|_| IntroductionError::Seal)?;
        entries.push(IntroductionEntry {
            tag: *tag,
            wrapped_key,
        });
    }
    let sealed_payload = seal_bound(&content_key, &payload_aad(issued_at_ms, &tags)?, &plaintext)?;
    IntroductionRecord {
        issued_at_ms,
        entries,
        sealed_payload,
    }
    .encode()
}

/// The tags of a record wire, for matching without opening.
///
/// # Errors
///
/// Every error of [`IntroductionRecord::decode`].
pub fn introduction_tags(record: &[u8]) -> CoreResult<Vec<[u8; FOF_TAG_LEN]>, IntroductionError> {
    Ok(IntroductionRecord::decode(record)?.tags())
}

/// Open a contacts-of-contacts record wire with one OPRF output. See
/// [`IntroductionRecord::open`]; a matched payload always carries its `fof`
/// grant.
///
/// # Errors
///
/// * [`IntroductionError::MissingFofGrant`] — the record opened but carries
///   no grant (a public record relayed as an introduction).
/// * Every error of [`IntroductionRecord::decode`] and
///   [`IntroductionRecord::open`].
pub fn open_introduction(
    record: &[u8],
    oprf_output: &[u8; OPRF_OUTPUT_LEN],
) -> CoreResult<Option<IntroductionPayload>, IntroductionError> {
    match IntroductionRecord::decode(record)?.open(oprf_output)? {
        Some(payload) if payload.fof.is_none() => Err(IntroductionError::MissingFofGrant),
        other => Ok(other),
    }
}

/// Open a public record (served by discovery beside an account id) with the
/// OPRF output of the searched term. See [`IntroductionRecord::open`]; a
/// matched payload never carries a grant.
///
/// # Errors
///
/// * [`IntroductionError::UnexpectedFofGrant`] — the record opened but
///   carries a grant.
/// * Every error of [`IntroductionRecord::decode`] and
///   [`IntroductionRecord::open`].
pub fn open_public_record(
    record: &[u8],
    oprf_output: &[u8; OPRF_OUTPUT_LEN],
) -> CoreResult<Option<IntroductionPayload>, IntroductionError> {
    match IntroductionRecord::decode(record)?.open(oprf_output)? {
        Some(payload) if payload.fof.is_some() => Err(IntroductionError::UnexpectedFofGrant),
        other => Ok(other),
    }
}

// =============================================================================
// Offer envelope
// =============================================================================

/// Wrap one record as an introduction offer (owner to a contact, sent in
/// their 1:1 conversation).
///
/// # Errors
///
/// Every error of [`IntroductionRecord::decode`]: an offer never carries a
/// record a receiver would refuse.
pub fn compose_introduction_offer(record: &[u8]) -> CoreResult<Vec<u8>, IntroductionError> {
    IntroductionRecord::decode(record)?;
    let mut out =
        Vec::with_capacity(INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1.len() + 1 + record.len());
    out.extend_from_slice(INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1);
    out.push(INTRODUCTION_OFFER_VERSION_V1);
    out.extend_from_slice(record);
    Ok(out)
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher.
///
/// * `Ok(Some(record))` — the separator matched and the record is
///   structurally valid.
/// * `Ok(None)` — the prefix does not match; fall through.
/// * `Err(_)` — the prefix matched but the payload is malformed; the caller
///   drops it.
///
/// # Errors
///
/// [`IntroductionError::UnknownVersion`] and every error of
/// [`IntroductionRecord::decode`].
pub fn try_unwrap_introduction_offer(
    plaintext: &[u8],
) -> CoreResult<Option<IntroductionRecord>, IntroductionError> {
    let Some(payload) = plaintext.strip_prefix(INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let mut cur = payload;
    let version = take_u8(&mut cur)?;
    if version != INTRODUCTION_OFFER_VERSION_V1 {
        return Err(IntroductionError::UnknownVersion(version));
    }
    IntroductionRecord::decode(cur).map(Some)
}

/// Strict standalone decoder for an offer.
///
/// # Errors
///
/// [`IntroductionError::WrongDomain`] and every error of
/// [`try_unwrap_introduction_offer`].
pub fn decode_introduction_offer(
    bytes: &[u8],
) -> CoreResult<IntroductionRecord, IntroductionError> {
    try_unwrap_introduction_offer(bytes)?.ok_or(IntroductionError::WrongDomain)
}

// =============================================================================
// Bundle envelope
// =============================================================================

/// A contact's bundle of introduction records, padding included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntroductionsBundle {
    /// Sender-clock UNIX ms at which the bundle was built. Orders bundles
    /// from the same contact via [`bundle_is_newer`].
    pub updated_at_ms: u64,
    /// The records, a [`BUNDLE_SIZE_BUCKETS`] count of them.
    pub records: Vec<IntroductionRecord>,
}

impl IntroductionsBundle {
    /// Encode as-is (no padding, no shuffle): the count must already be a
    /// bucket. [`compose_bundle`] is what a sender calls.
    ///
    /// # Errors
    ///
    /// [`IntroductionError::BadRecordCount`] and every record error.
    pub fn encode(&self) -> CoreResult<Vec<u8>, IntroductionError> {
        check_bucket(self.records.len())?;
        let count =
            u16::try_from(self.records.len()).map_err(|_| IntroductionError::BadRecordCount {
                count: self.records.len(),
            })?;
        let mut out = Vec::new();
        out.extend_from_slice(INTRODUCTIONS_ENVELOPE_DOMAIN_V1);
        out.push(INTRODUCTIONS_VERSION_V1);
        out.extend_from_slice(&self.updated_at_ms.to_be_bytes());
        out.extend_from_slice(&count.to_be_bytes());
        for record in &self.records {
            let wire = record.encode()?;
            let len = u16::try_from(wire.len())
                .map_err(|_| IntroductionError::RecordLength { got: wire.len() })?;
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(&wire);
        }
        Ok(out)
    }
}

/// Build a bundle from the real records a contact holds: pad to the next
/// bucket with dummies, shuffle, encode. See the module docs.
///
/// # Errors
///
/// * [`IntroductionError::TooManyRecords`] — more than
///   [`MAX_BUNDLE_RECORDS`] real records.
/// * Every record error.
pub fn compose_bundle(
    records: &[IntroductionRecord],
    updated_at_ms: u64,
) -> CoreResult<Vec<u8>, IntroductionError> {
    if records.len() > MAX_BUNDLE_RECORDS {
        return Err(IntroductionError::TooManyRecords {
            count: records.len(),
        });
    }
    for record in records {
        record.validate()?;
    }
    let target = BUNDLE_SIZE_BUCKETS
        .iter()
        .copied()
        .find(|&b| b >= records.len().max(1))
        .unwrap_or(MAX_BUNDLE_RECORDS);
    let mut all: Vec<IntroductionRecord> = records.to_vec();
    while all.len() < target {
        all.push(dummy_record(records, updated_at_ms));
    }
    shuffle(&mut all);
    IntroductionsBundle {
        updated_at_ms,
        records: all,
    }
    .encode()
}

/// A structurally valid record that opens under no OPRF output. Its entry
/// count and sealed-payload length copy a randomly chosen real record.
fn dummy_record(real: &[IntroductionRecord], fallback_issued_at_ms: u64) -> IntroductionRecord {
    let (issued_at_ms, entry_count, payload_len) = if real.is_empty() {
        let span = SEALED_PAYLOAD_MAX_LEN - SEALED_PAYLOAD_MIN_LEN + 1;
        (
            fallback_issued_at_ms,
            1 + random_below(MAX_INTRODUCTION_TERMS),
            SEALED_PAYLOAD_MIN_LEN + random_below(span),
        )
    } else {
        let template = &real[random_below(real.len())];
        let oldest = real.iter().map(|r| r.issued_at_ms).min().unwrap_or(0);
        let newest = real.iter().map(|r| r.issued_at_ms).max().unwrap_or(0);
        (
            oldest.saturating_add(random_u64_at_most(newest.saturating_sub(oldest))),
            template.entries.len(),
            template.sealed_payload.len(),
        )
    };
    let entries = (0..entry_count)
        .map(|_| {
            let mut tag = [0u8; FOF_TAG_LEN];
            random::fill(&mut tag);
            let mut wrapped_key = [0u8; WRAPPED_KEY_LEN];
            random::fill(&mut wrapped_key);
            IntroductionEntry { tag, wrapped_key }
        })
        .collect();
    let mut sealed_payload = vec![0u8; payload_len];
    random::fill(&mut sealed_payload);
    IntroductionRecord {
        issued_at_ms,
        entries,
        sealed_payload,
    }
}

/// Uniform in `0..n` (`n > 0`), by rejection sampling so no index is favoured.
fn random_below(n: usize) -> usize {
    let n64 = n.max(1) as u64;
    let zone = u64::MAX - (u64::MAX % n64);
    loop {
        let mut b = [0u8; 8];
        random::fill(&mut b);
        let v = u64::from_be_bytes(b);
        if v < zone {
            // `v % n64 < n64 <= usize::MAX`, so the conversion cannot fail.
            return usize::try_from(v % n64).unwrap_or(0);
        }
    }
}

/// Uniform in `0..=max`.
fn random_u64_at_most(max: u64) -> u64 {
    let Some(n) = max.checked_add(1) else {
        let mut b = [0u8; 8];
        random::fill(&mut b);
        return u64::from_be_bytes(b);
    };
    let zone = u64::MAX - (u64::MAX % n);
    loop {
        let mut b = [0u8; 8];
        random::fill(&mut b);
        let v = u64::from_be_bytes(b);
        if v < zone {
            return v % n;
        }
    }
}

/// Fisher-Yates with an unbiased index.
fn shuffle<T>(items: &mut [T]) {
    for i in (1..items.len()).rev() {
        items.swap(i, random_below(i + 1));
    }
}

/// `true` when a bundle stamped `incoming_updated_at_ms` must replace one
/// stamped `stored_updated_at_ms` from the **same contact**. Strictly greater
/// wins; equal is not newer.
#[must_use]
pub const fn bundle_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    incoming_updated_at_ms > stored_updated_at_ms
}

/// Discriminated unwrap of a bundle for the [`crate::incoming_envelope`]
/// dispatcher. Same contract as [`try_unwrap_introduction_offer`].
///
/// # Errors
///
/// Framing errors, [`IntroductionError::BadRecordCount`],
/// [`IntroductionError::RecordLength`] and every record error.
pub fn try_unwrap_introductions(
    plaintext: &[u8],
) -> CoreResult<Option<IntroductionsBundle>, IntroductionError> {
    let Some(payload) = plaintext.strip_prefix(INTRODUCTIONS_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let mut cur = payload;
    let version = take_u8(&mut cur)?;
    if version != INTRODUCTIONS_VERSION_V1 {
        return Err(IntroductionError::UnknownVersion(version));
    }
    let updated_at_ms = u64::from_be_bytes(take_array::<8>(&mut cur)?);
    let count = usize::from(u16::from_be_bytes(take_array::<2>(&mut cur)?));
    // The count is checked before anything is allocated for it.
    check_bucket(count)?;
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let len = usize::from(u16::from_be_bytes(take_array::<2>(&mut cur)?));
        if !(INTRODUCTION_RECORD_MIN_LEN..=INTRODUCTION_RECORD_MAX_LEN).contains(&len) {
            return Err(IntroductionError::RecordLength { got: len });
        }
        records.push(IntroductionRecord::decode(take(&mut cur, len)?)?);
    }
    if !cur.is_empty() {
        return Err(IntroductionError::TrailingBytes { count: cur.len() });
    }
    Ok(Some(IntroductionsBundle {
        updated_at_ms,
        records,
    }))
}

/// Strict standalone decoder for a bundle.
///
/// # Errors
///
/// [`IntroductionError::WrongDomain`] and every error of
/// [`try_unwrap_introductions`].
pub fn decode_introductions(bytes: &[u8]) -> CoreResult<IntroductionsBundle, IntroductionError> {
    try_unwrap_introductions(bytes)?.ok_or(IntroductionError::WrongDomain)
}

// =============================================================================
// Errors and cursor helpers
// =============================================================================

/// Why an introduction record, offer or bundle was refused. A reason code;
/// nothing from the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum IntroductionError {
    /// The plaintext does not carry the expected domain separator (strict
    /// decoders only).
    #[error("plaintext does not carry the introduction domain separator")]
    WrongDomain,
    /// A version byte is not the one accepted.
    #[error("introduction carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// The bytes end before a declared field.
    #[error("introduction is truncated")]
    Truncated,
    /// Bytes remain after the last field.
    #[error("introduction has {count} trailing bytes")]
    TrailingBytes {
        /// How many.
        count: usize,
    },
    /// A record carries no entry or more than [`MAX_INTRODUCTION_TERMS`].
    #[error("introduction record carries {0} entries, expected 1 to {MAX_INTRODUCTION_TERMS}")]
    BadEntryCount(u8),
    /// Two entries of one record share a tag.
    #[error("introduction record carries a tag twice")]
    DuplicateTag,
    /// The sealed payload is outside its length bounds.
    #[error("sealed introduction payload is {got} bytes, outside the allowed range")]
    SealedLength {
        /// The length as read.
        got: usize,
    },
    /// A record in a bundle is outside its length bounds.
    #[error("introduction record is {got} bytes, outside the allowed range")]
    RecordLength {
        /// The length as read.
        got: usize,
    },
    /// No search term was supplied to [`compose_introduction`].
    #[error("an introduction record needs at least one search term")]
    NoTerms,
    /// More search terms than [`MAX_INTRODUCTION_TERMS`].
    #[error("{count} search terms, at most {MAX_INTRODUCTION_TERMS} allowed")]
    TooManyTerms {
        /// The count supplied.
        count: usize,
    },
    /// More real records than a bundle can carry.
    #[error("{count} introduction records, at most {MAX_BUNDLE_RECORDS} allowed")]
    TooManyRecords {
        /// The count supplied.
        count: usize,
    },
    /// A bundle's record count is not one of [`BUNDLE_SIZE_BUCKETS`].
    #[error("introductions bundle carries {count} records, not a size bucket")]
    BadRecordCount {
        /// The count as read.
        count: usize,
    },
    /// The display name violates a bound.
    #[error("invalid display name: {0}")]
    DisplayName(DisplayNameError),
    /// The username is not canonical.
    #[error("invalid username: {0}")]
    Username(UsernameError),
    /// The certificate is neither absent nor exactly [`USERNAME_CERT_LEN`].
    #[error("introduction username certificate is {got} bytes")]
    CertificateLength {
        /// The length as read.
        got: usize,
    },
    /// A certificate without a username.
    #[error("introduction carries a username certificate but no username")]
    CertificateWithoutUsername,
    /// The grant is not of class `fof` (the class byte).
    #[error("introduction grant is of class {0:#04x}, expected fof")]
    NotFofGrant(u8),
    /// The grant length byte is neither 0 nor the grant length.
    #[error("introduction grant is {got} bytes")]
    GrantLength {
        /// The length as read.
        got: usize,
    },
    /// A contacts-of-contacts record without its `fof` grant.
    #[error("a contacts-of-contacts introduction must carry its fof grant")]
    MissingFofGrant,
    /// A public record carrying a `fof` grant.
    #[error("a public record must not carry a fof grant")]
    UnexpectedFofGrant,
    /// The grant failed to decode.
    #[error("invalid reach grant: {0}")]
    Grant(ReachError),
    /// The AEAD refused to seal (not expected for valid inputs).
    #[error("introduction could not be sealed")]
    Seal,
    /// A tag matched but the wrapped key or the payload failed to
    /// authenticate.
    #[error("introduction failed to open under its matching tag")]
    Open,
}

impl IntroductionError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub fn code(self) -> String {
        match self {
            Self::WrongDomain => "wrongDomain".to_string(),
            Self::UnknownVersion(_) => "unknownVersion".to_string(),
            Self::Truncated => "truncated".to_string(),
            Self::TrailingBytes { .. } => "trailingBytes".to_string(),
            Self::BadEntryCount(_) => "badEntryCount".to_string(),
            Self::DuplicateTag => "duplicateTag".to_string(),
            Self::SealedLength { .. } => "sealedLength".to_string(),
            Self::RecordLength { .. } => "recordLength".to_string(),
            Self::NoTerms => "noTerms".to_string(),
            Self::TooManyTerms { .. } => "tooManyTerms".to_string(),
            Self::TooManyRecords { .. } => "tooManyRecords".to_string(),
            Self::BadRecordCount { .. } => "badRecordCount".to_string(),
            Self::DisplayName(e) => format!("displayName.{}", e.code()),
            Self::Username(e) => format!("username.{}", e.code()),
            Self::CertificateLength { .. } => "certificate.length".to_string(),
            Self::CertificateWithoutUsername => "certificate.withoutUsername".to_string(),
            Self::NotFofGrant(_) => "grant.notFof".to_string(),
            Self::GrantLength { .. } => "grant.length".to_string(),
            Self::MissingFofGrant => "grant.missing".to_string(),
            Self::UnexpectedFofGrant => "grant.unexpected".to_string(),
            Self::Grant(e) => format!("grant.{}", e.code()),
            Self::Seal => "seal".to_string(),
            Self::Open => "open".to_string(),
        }
    }
}

fn check_sealed_len(len: usize) -> CoreResult<(), IntroductionError> {
    if (SEALED_PAYLOAD_MIN_LEN..=SEALED_PAYLOAD_MAX_LEN).contains(&len) {
        Ok(())
    } else {
        Err(IntroductionError::SealedLength { got: len })
    }
}

fn check_entry_count(count: usize) -> CoreResult<(), IntroductionError> {
    if (1..=MAX_INTRODUCTION_TERMS).contains(&count) {
        Ok(())
    } else {
        Err(IntroductionError::BadEntryCount(
            u8::try_from(count).unwrap_or(u8::MAX),
        ))
    }
}

fn entry_count_byte(count: usize) -> CoreResult<u8, IntroductionError> {
    check_entry_count(count)?;
    u8::try_from(count).map_err(|_| IntroductionError::BadEntryCount(u8::MAX))
}

fn check_distinct_tags<'a>(
    tags: impl Iterator<Item = &'a [u8; FOF_TAG_LEN]> + Clone,
) -> CoreResult<(), IntroductionError> {
    for (i, tag) in tags.clone().enumerate() {
        if tags.clone().take(i).any(|t| t == tag) {
            return Err(IntroductionError::DuplicateTag);
        }
    }
    Ok(())
}

fn check_bucket(count: usize) -> CoreResult<(), IntroductionError> {
    if BUNDLE_SIZE_BUCKETS.contains(&count) {
        Ok(())
    } else {
        Err(IntroductionError::BadRecordCount { count })
    }
}

fn read_record(cur: &mut &[u8]) -> CoreResult<IntroductionRecord, IntroductionError> {
    let version = take_u8(cur)?;
    if version != INTRODUCTION_RECORD_VERSION_V1 {
        return Err(IntroductionError::UnknownVersion(version));
    }
    let issued_at_ms = u64::from_be_bytes(take_array::<8>(cur)?);
    let entry_count = take_u8(cur)?;
    // Checked before anything is allocated for it.
    check_entry_count(usize::from(entry_count))?;
    let mut entries = Vec::with_capacity(usize::from(entry_count));
    for _ in 0..entry_count {
        let tag = take_array::<FOF_TAG_LEN>(cur)?;
        let wrapped_key = take_array::<WRAPPED_KEY_LEN>(cur)?;
        entries.push(IntroductionEntry { tag, wrapped_key });
    }
    let sealed_len = usize::from(u16::from_be_bytes(take_array::<2>(cur)?));
    check_sealed_len(sealed_len)?;
    let sealed_payload = take(cur, sealed_len)?.to_vec();
    let record = IntroductionRecord {
        issued_at_ms,
        entries,
        sealed_payload,
    };
    record.validate()?;
    Ok(record)
}

fn push_len_u8(out: &mut Vec<u8>, len: usize) -> CoreResult<(), IntroductionError> {
    out.push(u8::try_from(len).map_err(|_| IntroductionError::Truncated)?);
    Ok(())
}

fn take_u8(cur: &mut &[u8]) -> CoreResult<u8, IntroductionError> {
    let (&first, rest) = cur.split_first().ok_or(IntroductionError::Truncated)?;
    *cur = rest;
    Ok(first)
}

fn take<'a>(cur: &mut &'a [u8], n: usize) -> CoreResult<&'a [u8], IntroductionError> {
    if cur.len() < n {
        return Err(IntroductionError::Truncated);
    }
    let (head, rest) = cur.split_at(n);
    *cur = rest;
    Ok(head)
}

fn take_array<const N: usize>(cur: &mut &[u8]) -> CoreResult<[u8; N], IntroductionError> {
    take(cur, N)?
        .try_into()
        .map_err(|_| IntroductionError::Truncated)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const U_OPRF: [u8; OPRF_OUTPUT_LEN] = [0x11; OPRF_OUTPUT_LEN];
    const N_OPRF: [u8; OPRF_OUTPUT_LEN] = [0x22; OPRF_OUTPUT_LEN];
    const OTHER_OPRF: [u8; OPRF_OUTPUT_LEN] = [0x33; OPRF_OUTPUT_LEN];

    fn term(i: usize) -> [u8; OPRF_OUTPUT_LEN] {
        let mut o = [0xEE; OPRF_OUTPUT_LEN];
        o[..8].copy_from_slice(&(i as u64).to_be_bytes());
        o
    }

    fn fof(byte: u8) -> ReachGrant {
        ReachGrant {
            class: ReachClass::Fof,
            version: 7,
            seed: SecretBytes::new([byte; 32]),
        }
    }

    fn payload() -> IntroductionPayload {
        IntroductionPayload {
            identity_pk: [0xA1; 32],
            account_id: [0xB2; 16],
            display_name: Some("Anna Weber".to_string()),
            username: Some("anna".to_string()),
            username_certificate: Some(vec![0xC3; USERNAME_CERT_LEN]),
            fof: Some(fof(0xD4)),
        }
    }

    /// The Everyone shape: the same fields, no grant.
    fn public_payload() -> IntroductionPayload {
        IntroductionPayload {
            fof: None,
            ..payload()
        }
    }

    fn minimal_payload() -> IntroductionPayload {
        IntroductionPayload {
            identity_pk: [1; 32],
            account_id: [2; 16],
            display_name: None,
            username: None,
            username_certificate: None,
            fof: None,
        }
    }

    fn maximal_payload() -> IntroductionPayload {
        let mut max = payload();
        max.display_name = Some(format!("{}\u{20AC}", "\u{1F600}".repeat(63)));
        max.username = Some("a".repeat(32));
        max
    }

    // --- payload -------------------------------------------------------------

    #[test]
    fn payload_round_trips_and_its_bounds_are_the_documented_construction() {
        assert_eq!(INTRODUCTION_PAYLOAD_MIN_LEN, 53);
        assert_eq!(INTRODUCTION_PAYLOAD_MAX_LEN, 53 + 255 + 32 + 153 + 37);
        for p in [
            payload(),
            public_payload(),
            minimal_payload(),
            maximal_payload(),
        ] {
            let wire = p.encode().unwrap();
            assert_eq!(IntroductionPayload::decode(&wire).unwrap(), p);
        }
        assert_eq!(
            minimal_payload().encode().unwrap().len(),
            INTRODUCTION_PAYLOAD_MIN_LEN
        );
        assert_eq!(
            maximal_payload().encode().unwrap().len(),
            INTRODUCTION_PAYLOAD_MAX_LEN
        );
    }

    #[test]
    fn payload_field_bounds_are_refused_by_encode_and_decode_alike() {
        let mut bad_name = payload();
        bad_name.display_name = Some("Anna\u{202E}".to_string());
        let mut bad_username = payload();
        bad_username.username = Some("Anna".to_string());
        let mut no_username = payload();
        no_username.username = None;
        let mut not_fof = payload();
        if let Some(grant) = not_fof.fof.as_mut() {
            grant.class = ReachClass::Contact;
        }
        let mut short_cert = payload();
        short_cert.username_certificate = Some(vec![0; 10]);
        for (p, why) in [
            (
                bad_name,
                IntroductionError::DisplayName(DisplayNameError::ControlOrFormatCharacter),
            ),
            (
                bad_username,
                IntroductionError::Username(UsernameError::InvalidCharacters),
            ),
            (no_username, IntroductionError::CertificateWithoutUsername),
            (not_fof, IntroductionError::NotFofGrant(1)),
            (short_cert, IntroductionError::CertificateLength { got: 10 }),
        ] {
            assert_eq!(p.encode(), Err(why));
            let raw = raw_payload(&p);
            assert_eq!(IntroductionPayload::decode(&raw), Err(why));
        }
    }

    /// Encode past `validate`, so decode's gates are exercised.
    fn raw_payload(p: &IntroductionPayload) -> Vec<u8> {
        let name = p.display_name.as_deref().unwrap_or("").as_bytes();
        let username = p.username.as_deref().unwrap_or("").as_bytes();
        let cert = p.username_certificate.as_deref().unwrap_or(&[]);
        let mut out = vec![INTRODUCTION_PAYLOAD_VERSION_V2];
        out.extend_from_slice(&p.identity_pk);
        out.extend_from_slice(&p.account_id);
        out.push(u8::try_from(name.len()).unwrap());
        out.extend_from_slice(name);
        out.push(u8::try_from(username.len()).unwrap());
        out.extend_from_slice(username);
        out.push(u8::try_from(cert.len()).unwrap());
        out.extend_from_slice(cert);
        match &p.fof {
            Some(grant) => {
                out.push(u8::try_from(REACH_GRANT_LEN).unwrap());
                out.extend_from_slice(&grant.encode());
            },
            None => out.push(0),
        }
        out
    }

    #[test]
    fn payload_framing_is_fail_closed() {
        let wire = payload().encode().unwrap();
        for cut in 0..wire.len() {
            assert_eq!(
                IntroductionPayload::decode(&wire[..cut]),
                Err(IntroductionError::Truncated),
                "cut at {cut}"
            );
        }
        let mut long = wire.clone();
        long.push(0);
        assert_eq!(
            IntroductionPayload::decode(&long),
            Err(IntroductionError::TrailingBytes { count: 1 })
        );
        let mut v1 = wire.clone();
        v1[0] = 1;
        assert_eq!(
            IntroductionPayload::decode(&v1),
            Err(IntroductionError::UnknownVersion(1)),
            "v1 (grant always present) is refused: a flag day"
        );
        // The grant length byte is 0 or the grant length, nothing else.
        let grant_len_at = wire.len() - REACH_GRANT_LEN - 1;
        let mut odd = wire;
        odd[grant_len_at] = 36;
        assert_eq!(
            IntroductionPayload::decode(&odd),
            Err(IntroductionError::GrantLength { got: 36 })
        );
    }

    #[test]
    fn each_use_requires_or_refuses_the_grant_on_compose_and_open() {
        // Contacts of contacts: required on compose ...
        assert_eq!(
            compose_introduction(&public_payload(), &[U_OPRF], 1),
            Err(IntroductionError::MissingFofGrant)
        );
        // ... and Everyone: refused on compose.
        assert_eq!(
            compose_public_record(&payload(), &[U_OPRF], 1),
            Err(IntroductionError::UnexpectedFofGrant)
        );
        let public = compose_public_record(&public_payload(), &[U_OPRF, N_OPRF], 5).unwrap();
        assert_eq!(
            open_public_record(&public, &N_OPRF).unwrap(),
            Some(public_payload())
        );
        assert_eq!(open_public_record(&public, &OTHER_OPRF).unwrap(), None);
        // A public record relayed as an introduction opens to an error, never
        // to a payload the reader could not reach.
        assert_eq!(
            open_introduction(&public, &U_OPRF),
            Err(IntroductionError::MissingFofGrant)
        );
        let fof_record = compose_introduction(&payload(), &[U_OPRF], 5).unwrap();
        assert_eq!(
            open_public_record(&fof_record, &U_OPRF),
            Err(IntroductionError::UnexpectedFofGrant)
        );
        // A miss is a miss under either opener, whatever the record's use.
        assert_eq!(open_introduction(&public, &OTHER_OPRF).unwrap(), None);
        assert_eq!(open_public_record(&fof_record, &OTHER_OPRF).unwrap(), None);
    }

    // --- record --------------------------------------------------------------

    #[test]
    fn a_record_opens_under_every_term_and_not_under_another() {
        let terms: Vec<_> = (0..10).map(term).chain([U_OPRF, N_OPRF]).collect();
        let record = compose_introduction(&payload(), &terms, 99).unwrap();
        assert_eq!(
            introduction_tags(&record).unwrap(),
            terms.iter().map(fof_tag).collect::<Vec<_>>(),
            "one tag per term, in order"
        );
        for t in &terms {
            assert_eq!(open_introduction(&record, t).unwrap(), Some(payload()));
        }
        assert_eq!(open_introduction(&record, &OTHER_OPRF).unwrap(), None);
        let parsed = IntroductionRecord::decode(&record).unwrap();
        assert_eq!(parsed.issued_at_ms, 99);
        assert_eq!(parsed.entries.len(), terms.len());
    }

    #[test]
    fn the_term_list_is_bounded_and_duplicates_are_refused() {
        assert_eq!(
            compose_introduction(&payload(), &[], 1),
            Err(IntroductionError::NoTerms)
        );
        let max: Vec<_> = (0..MAX_INTRODUCTION_TERMS).map(term).collect();
        let record = compose_introduction(&payload(), &max, 1).unwrap();
        assert_eq!(
            introduction_tags(&record).unwrap().len(),
            MAX_INTRODUCTION_TERMS
        );
        let over: Vec<_> = (0..=MAX_INTRODUCTION_TERMS).map(term).collect();
        assert_eq!(
            compose_introduction(&payload(), &over, 1),
            Err(IntroductionError::TooManyTerms {
                count: MAX_INTRODUCTION_TERMS + 1
            })
        );
        assert_eq!(
            compose_introduction(&payload(), &[U_OPRF, N_OPRF, U_OPRF], 1),
            Err(IntroductionError::DuplicateTag)
        );
    }

    #[test]
    fn record_length_bounds_are_the_documented_construction() {
        assert_eq!(WRAPPED_KEY_LEN, 60);
        assert_eq!(SEALED_PAYLOAD_MIN_LEN, 12 + 16 + 53);
        assert_eq!(INTRODUCTION_RECORD_MIN_LEN, 10 + 92 + 2 + 81);
        assert_eq!(
            INTRODUCTION_RECORD_MAX_LEN,
            10 + 65 * 92 + 2 + 12 + 16 + 530
        );
        let min = compose_public_record(&minimal_payload(), &[U_OPRF], 1).unwrap();
        assert_eq!(min.len(), INTRODUCTION_RECORD_MIN_LEN);
        let terms: Vec<_> = (0..MAX_INTRODUCTION_TERMS).map(term).collect();
        let max = compose_introduction(&maximal_payload(), &terms, 1).unwrap();
        assert_eq!(max.len(), INTRODUCTION_RECORD_MAX_LEN);
        assert!(u16::try_from(INTRODUCTION_RECORD_MAX_LEN).is_ok());
    }

    #[test]
    fn the_wrapped_key_is_what_seal_with_key_would_produce_for_a_key() {
        // Shape parity with `lakko_crypto::aead::seal_with_key`: nonce ||
        // ciphertext || tag for a 32-byte plaintext.
        let wrapped = lakko_crypto::aead::seal_with_key(&[1; 32], &[2; 32]).unwrap();
        assert_eq!(wrapped.len(), WRAPPED_KEY_LEN);
    }

    #[test]
    fn a_tampered_record_fails_to_open_under_its_matching_tag() {
        let wire = compose_introduction(&payload(), &[U_OPRF, N_OPRF], 5).unwrap();
        let parsed = IntroductionRecord::decode(&wire).unwrap();

        // Re-dated: issued_at is bound into the payload AAD.
        let mut redated = parsed.clone();
        redated.issued_at_ms += 1;
        assert_eq!(redated.open(&U_OPRF), Err(IntroductionError::Open));

        // A term dropped: the tag list is bound into the payload AAD.
        let mut dropped = parsed.clone();
        dropped.entries.pop();
        assert_eq!(dropped.open(&U_OPRF), Err(IntroductionError::Open));

        // A term added (a random entry): same.
        let mut added = parsed.clone();
        added.entries.push(IntroductionEntry {
            tag: [9; 32],
            wrapped_key: [9; WRAPPED_KEY_LEN],
        });
        assert_eq!(added.open(&U_OPRF), Err(IntroductionError::Open));

        // Reordered: same.
        let mut reordered = parsed.clone();
        reordered.entries.reverse();
        assert_eq!(reordered.open(&U_OPRF), Err(IntroductionError::Open));

        // A wrapped key moved under another tag: bound to its own tag.
        let mut moved = parsed.clone();
        moved.entries[0].tag = fof_tag(&OTHER_OPRF);
        assert_eq!(moved.open(&OTHER_OPRF), Err(IntroductionError::Open));

        // A flipped payload byte.
        let mut flipped = parsed.clone();
        let last = flipped.sealed_payload.len() - 1;
        flipped.sealed_payload[last] ^= 1;
        assert_eq!(flipped.open(&N_OPRF), Err(IntroductionError::Open));

        // A flipped wrapped-key byte.
        let mut bad_wrap = parsed;
        bad_wrap.entries[1].wrapped_key[20] ^= 1;
        assert_eq!(bad_wrap.open(&N_OPRF), Err(IntroductionError::Open));
    }

    #[test]
    fn record_framing_is_fail_closed() {
        let wire = compose_introduction(&payload(), &[U_OPRF, N_OPRF], 5).unwrap();
        for cut in 0..wire.len() {
            assert!(
                IntroductionRecord::decode(&wire[..cut]).is_err(),
                "cut at {cut}"
            );
        }
        let mut long = wire.clone();
        long.push(0);
        assert_eq!(
            IntroductionRecord::decode(&long),
            Err(IntroductionError::TrailingBytes { count: 1 })
        );
        for count in [0u8, 66, 0xFF] {
            let mut bad = wire.clone();
            bad[9] = count;
            assert_eq!(
                IntroductionRecord::decode(&bad),
                Err(IntroductionError::BadEntryCount(count))
            );
        }
        let mut version = wire.clone();
        version[0] = 2;
        assert_eq!(
            IntroductionRecord::decode(&version),
            Err(IntroductionError::UnknownVersion(2))
        );
        // A duplicate tag, on encode and on decode.
        let mut dup = IntroductionRecord::decode(&wire).unwrap();
        dup.entries[1].tag = dup.entries[0].tag;
        assert_eq!(dup.encode(), Err(IntroductionError::DuplicateTag));
        let mut dup_wire = wire;
        dup_wire.copy_within(
            RECORD_HEADER_LEN..RECORD_HEADER_LEN + FOF_TAG_LEN,
            RECORD_HEADER_LEN + ENTRY_LEN,
        );
        assert_eq!(
            IntroductionRecord::decode(&dup_wire),
            Err(IntroductionError::DuplicateTag)
        );
    }

    #[test]
    fn sealed_payload_lengths_out_of_bounds_are_refused() {
        for len in [
            0,
            SEALED_PAYLOAD_MIN_LEN - 1,
            SEALED_PAYLOAD_MAX_LEN + 1,
            usize::from(u16::MAX),
        ] {
            let record = IntroductionRecord {
                issued_at_ms: 1,
                entries: vec![IntroductionEntry {
                    tag: [0; 32],
                    wrapped_key: [0; WRAPPED_KEY_LEN],
                }],
                sealed_payload: vec![0; len],
            };
            assert_eq!(
                record.encode(),
                Err(IntroductionError::SealedLength { got: len })
            );
            let mut wire = vec![INTRODUCTION_RECORD_VERSION_V1];
            wire.extend_from_slice(&1u64.to_be_bytes());
            wire.push(1);
            wire.extend_from_slice(&[0; ENTRY_LEN]);
            wire.extend_from_slice(&u16::try_from(len).unwrap().to_be_bytes());
            wire.extend_from_slice(&vec![0; len]);
            assert_eq!(
                IntroductionRecord::decode(&wire),
                Err(IntroductionError::SealedLength { got: len })
            );
        }
    }

    /// Every single-byte substitution of a valid record, and of a valid
    /// payload: decode never panics, whatever still decodes re-encodes to
    /// the identical bytes, and an open never panics.
    #[test]
    fn every_single_byte_mutation_decodes_canonically_or_is_refused() {
        let record = compose_introduction(&payload(), &[U_OPRF, N_OPRF], 5).unwrap();
        for at in 0..record.len() {
            for byte in [0u8, 1, 2, 0x41, 0x42, 0x7F, 0x80, 0xFF, record[at] ^ 1] {
                let mut m = record.clone();
                m[at] = byte;
                if let Ok(r) = IntroductionRecord::decode(&m) {
                    assert_eq!(r.encode().unwrap(), m);
                    let _ = r.open(&U_OPRF);
                    let _ = r.open(&N_OPRF);
                }
            }
        }
        let wire = payload().encode().unwrap();
        for at in 0..wire.len() {
            for byte in 0..=u8::MAX {
                let mut m = wire.clone();
                m[at] = byte;
                if let Ok(p) = IntroductionPayload::decode(&m) {
                    assert_eq!(p.encode().unwrap(), m);
                }
            }
        }
        let bundle = compose_bundle(&real_records(1), 3).unwrap();
        for at in 0..bundle.len() {
            let mut m = bundle.clone();
            m[at] ^= 0xFF;
            if let Ok(b) = decode_introductions(&m) {
                assert_eq!(b.encode().unwrap(), m);
            }
        }
    }

    // --- offer ---------------------------------------------------------------

    #[test]
    fn offer_round_trips_and_is_fail_closed() {
        let record = compose_introduction(&payload(), &[U_OPRF], 3).unwrap();
        let offer = compose_introduction_offer(&record).unwrap();
        assert!(offer.starts_with(b"lakko/introduction-offer/v1\x00"));
        let parsed = decode_introduction_offer(&offer).unwrap();
        assert_eq!(parsed.encode().unwrap(), record);
        assert_eq!(try_unwrap_introduction_offer(b"hello"), Ok(None));
        assert_eq!(
            decode_introduction_offer(b"hello"),
            Err(IntroductionError::WrongDomain)
        );
        assert_eq!(
            try_unwrap_introduction_offer(INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1),
            Err(IntroductionError::Truncated)
        );
        let mut v2 = offer.clone();
        v2[INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1.len()] = 2;
        assert_eq!(
            decode_introduction_offer(&v2),
            Err(IntroductionError::UnknownVersion(2))
        );
        let mut long = offer;
        long.push(0);
        assert!(decode_introduction_offer(&long).is_err());
        assert!(compose_introduction_offer(b"not a record").is_err());
    }

    // --- bundle --------------------------------------------------------------

    /// Real records of varied shape: the term count and the name length
    /// differ per record.
    fn real_records(n: usize) -> Vec<IntroductionRecord> {
        (0..n)
            .map(|i| {
                let mut p = payload();
                p.display_name = Some(format!("Person {i}{}", "x".repeat(i % 7)));
                let terms: Vec<_> = (0..=(i % 5))
                    .map(|k| term(i * 100 + k))
                    .chain([N_OPRF])
                    .collect();
                let wire = compose_introduction(&p, &terms, 100 + i as u64).unwrap();
                IntroductionRecord::decode(&wire).unwrap()
            })
            .collect()
    }

    #[test]
    fn bundles_pad_to_the_next_bucket_with_dummies_shaped_like_real_records() {
        for (real, bucket) in [
            (0, 1),
            (1, 1),
            (2, 4),
            (4, 4),
            (5, 16),
            (17, 64),
            (65, 256),
            (257, 1024),
        ] {
            let records = real_records(real);
            let wire = compose_bundle(&records, 1_000).unwrap();
            let bundle = decode_introductions(&wire).unwrap();
            assert_eq!(bundle.records.len(), bucket, "{real} real records");
            assert_eq!(bundle.updated_at_ms, 1_000);
            for r in &records {
                assert!(bundle.records.contains(r));
            }
            let dummies: Vec<_> = bundle
                .records
                .iter()
                .filter(|r| !records.contains(r))
                .collect();
            assert_eq!(dummies.len(), bucket - real);
            for d in dummies {
                d.validate().unwrap();
                assert_eq!(d.open(&N_OPRF), Ok(None), "a dummy never opens");
                if real > 0 {
                    let oldest = records.iter().map(|r| r.issued_at_ms).min().unwrap();
                    let newest = records.iter().map(|r| r.issued_at_ms).max().unwrap();
                    assert!((oldest..=newest).contains(&d.issued_at_ms));
                    assert!(
                        records.iter().any(|r| r.entries.len() == d.entries.len()
                            && r.sealed_payload.len() == d.sealed_payload.len()),
                        "a dummy copies a real record's entry count and length"
                    );
                }
            }
            assert_eq!(bundle.encode().unwrap(), wire, "decode/encode is stable");
        }
    }

    #[test]
    fn a_bundle_of_too_many_records_is_refused() {
        let one = real_records(1).pop().unwrap();
        let many = vec![one; MAX_BUNDLE_RECORDS + 1];
        assert_eq!(
            compose_bundle(&many, 1),
            Err(IntroductionError::TooManyRecords {
                count: MAX_BUNDLE_RECORDS + 1
            })
        );
    }

    #[test]
    fn a_bundle_count_that_is_not_a_bucket_is_refused() {
        for count in [0usize, 2, 3, 5, 1023, 1025, 0xFFFF] {
            let mut wire = INTRODUCTIONS_ENVELOPE_DOMAIN_V1.to_vec();
            wire.push(INTRODUCTIONS_VERSION_V1);
            wire.extend_from_slice(&1u64.to_be_bytes());
            wire.extend_from_slice(&u16::try_from(count).unwrap().to_be_bytes());
            assert_eq!(
                decode_introductions(&wire),
                Err(IntroductionError::BadRecordCount { count })
            );
        }
        let bundle = IntroductionsBundle {
            updated_at_ms: 1,
            records: real_records(2),
        };
        assert_eq!(
            bundle.encode(),
            Err(IntroductionError::BadRecordCount { count: 2 })
        );
    }

    #[test]
    fn bundle_framing_is_fail_closed() {
        let wire = compose_bundle(&real_records(3), 9).unwrap();
        let domain = INTRODUCTIONS_ENVELOPE_DOMAIN_V1.len();
        for cut in domain..wire.len() {
            assert!(decode_introductions(&wire[..cut]).is_err(), "cut at {cut}");
        }
        let mut long = wire.clone();
        long.push(0);
        assert!(matches!(
            decode_introductions(&long),
            Err(IntroductionError::TrailingBytes { count: 1 })
        ));
        let mut v2 = wire.clone();
        v2[domain] = 2;
        assert_eq!(
            decode_introductions(&v2),
            Err(IntroductionError::UnknownVersion(2))
        );
        let mut bad_len = wire;
        let first_len_at = domain + 1 + 8 + 2;
        bad_len[first_len_at..first_len_at + 2].copy_from_slice(&5u16.to_be_bytes());
        assert_eq!(
            decode_introductions(&bad_len),
            Err(IntroductionError::RecordLength { got: 5 })
        );
        assert_eq!(try_unwrap_introductions(b"hello"), Ok(None));
        assert_eq!(
            decode_introductions(b"hello"),
            Err(IntroductionError::WrongDomain)
        );
    }

    #[test]
    fn the_shuffle_moves_real_records() {
        let records = real_records(2);
        let mut positions = [false; 4];
        for _ in 0..64 {
            let bundle = decode_introductions(&compose_bundle(&records, 1).unwrap()).unwrap();
            let at = bundle
                .records
                .iter()
                .position(|r| *r == records[0])
                .unwrap();
            positions[at] = true;
        }
        assert!(positions.iter().filter(|p| **p).count() > 1);
    }

    #[test]
    fn random_helpers_stay_in_range() {
        for n in [1usize, 2, 3, 7, 1024] {
            for _ in 0..100 {
                assert!(random_below(n) < n);
            }
        }
        for max in [0u64, 1, 5, u64::MAX - 1, u64::MAX] {
            for _ in 0..20 {
                assert!(random_u64_at_most(max) <= max);
            }
        }
    }

    #[test]
    fn bundle_order_is_strict() {
        assert!(bundle_is_newer(2, 1));
        assert!(!bundle_is_newer(1, 1));
        assert!(!bundle_is_newer(0, 1));
    }

    #[test]
    fn error_codes_are_frozen() {
        assert_eq!(IntroductionError::Open.code(), "open");
        assert_eq!(IntroductionError::NoTerms.code(), "noTerms");
        assert_eq!(
            IntroductionError::TooManyTerms { count: 66 }.code(),
            "tooManyTerms"
        );
        assert_eq!(IntroductionError::BadEntryCount(0).code(), "badEntryCount");
        assert_eq!(IntroductionError::NotFofGrant(1).code(), "grant.notFof");
        assert_eq!(
            IntroductionError::Grant(ReachError::UnknownClass(9)).code(),
            "grant.unknownClass"
        );
        assert_eq!(
            IntroductionError::BadRecordCount { count: 2 }.code(),
            "badRecordCount"
        );
    }

    #[test]
    fn domains_are_distinct_from_every_sibling() {
        let other_domains: &[&[u8]] = &[
            crate::call_welcome::CALL_WELCOME_ENVELOPE_DOMAIN_V1,
            crate::call_kp_forward::CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1,
            crate::media_state::MEDIA_STATE_ENVELOPE_DOMAIN_V1,
            crate::push_routing::PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1,
            crate::push_routing::PUSH_GID_DERIVE_DOMAIN_V1,
            crate::committer_beacon::COMMITTER_BEACON_ENVELOPE_DOMAIN_V1,
            crate::profile::PROFILE_ENVELOPE_DOMAIN_V1,
            crate::profile_request::PROFILE_REQUEST_ENVELOPE_DOMAIN_V1,
            crate::group_name::GROUP_NAME_ENVELOPE_DOMAIN_V1,
            crate::pin::PIN_ENVELOPE_DOMAIN_V1,
            crate::visibility::VISIBILITY_ENVELOPE_DOMAIN_V1,
            crate::visibility::REACH_STATE_ENVELOPE_DOMAIN_V1,
            crate::committer_delegation::COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1,
            crate::invite::REVOKE_DOMAIN_V1,
            crate::invite::OWNER_LIST_DOMAIN_V1,
            crate::invite::JOINER_TAG_SALT_DOMAIN_V1,
            crate::call_invite::CALL_INVITE_REVOKE_DOMAIN_V1,
            crate::group_invite::GROUP_INVITE_REVOKE_DOMAIN_V1,
            b"lakko/group-join-attribution/v1",
        ];
        for mine in [
            INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1,
            INTRODUCTIONS_ENVELOPE_DOMAIN_V1,
        ] {
            for d in other_domains {
                assert!(
                    !d.starts_with(mine) && !mine.starts_with(d),
                    "neither separator may prefix the other: {d:?}",
                );
            }
        }
        assert!(
            !INTRODUCTIONS_ENVELOPE_DOMAIN_V1.starts_with(INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1)
        );
        assert!(
            !INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1.starts_with(INTRODUCTIONS_ENVELOPE_DOMAIN_V1)
        );
    }
}
