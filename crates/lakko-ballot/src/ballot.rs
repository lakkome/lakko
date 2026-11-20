// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The anonymous ballot and its journal row (ADR-0092 section 3.6).
//!
//! ```text
//! row      = "lakko/poll-ballot/v1\0" || 0x01 || nonce(24)
//!            || XChaCha20-Poly1305(seal_key, aad, pad(ballot))
//! aad      = "lakko/poll-ballot/v1\0" || 0x01
//! seal_key = HKDF-SHA256(ikm = secret, salt = empty,
//!                        info = "lakko/poll/ballot-seal/v1" || id)
//! pad(b)   = len(b) u16 BE || b || zero bytes to a multiple of 256
//! ballot   = id(16) || ring_hash_k(32) || set || choices || tag(32) || proof
//! set      = count(u16 BE) || index(u32 BE)*   strictly ascending, each < L_k,
//!            count == min(256, L_k)
//! choices  = count(u8, 0..=10) || index(u8)*   strictly ascending, each < 10
//! ```
//!
//! **The anonymity set.** The proof covers at most [`ANONYMITY_SET`] keys of
//! ring version `k`: the whole version when it has at most 256 keys, else the
//! voter's own key and 255 others. The voter draws the others
//! deterministically ([`draw_set`]) with a seed derived from its secret
//! scalar, the group, the poll and the version: the account's first vote
//! draws from the newest version the device holds, and every later vote, from
//! any device of the account, draws from that recorded version again, so it
//! gets the same set and repeated votes cannot be intersected. The verifier cannot
//! recompute the draw (it needs the secret), so it accepts any well-formed
//! set of the right size over usable keys. That does not let anyone vote
//! twice: the tag does not depend on the set.
//!
//! The tag is `x^-1 U` with `U` the point of `"lakko/poll/tag-base/v1"` over
//! `len(group_id) u8 || group_id || id`.
//!
//! The proof's context, bound into its transcript, is
//! `"lakko/poll/ballot-context/v1\0" || len(group_id) u8 || group_id || id ||
//! ring_hash_k || set || choices`; the proof's own transcript hashes the
//! selected keys. A proof is valid for exactly one group, poll, ring version,
//! anonymity set and set of choices.

use std::collections::BTreeSet;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use curve25519_dalek::edwards::EdwardsPoint;
use lakko_crypto::SecretBytes;
use zeroize::Zeroize;

use crate::error::{BallotError, Result};
use crate::point::{decode_nonidentity_point, hash_to_point, VoterKey, POINT_LEN};
use crate::proof::{self, capacity, encoded_len, Proof, RingRef, MAX_DIGITS};
use crate::ring::Ring;

/// The domain prefix of a ballot row, also how a receiver recognises one.
pub const BALLOT_ROW_DOMAIN: &[u8] = b"lakko/poll-ballot/v1\0";
/// The row version byte.
pub const ROW_VERSION: u8 = 0x01;
/// Length of a poll id.
pub const POLL_ID_LEN: usize = 16;
/// Length of a poll secret.
pub const POLL_SECRET_LEN: usize = 32;
/// Length of the row nonce.
pub const NONCE_LEN: usize = 24;
/// The padding block of the sealed plaintext. Chosen, not measured: it hides
/// how many options a multiple-choice ballot names.
pub const PAD_BLOCK: usize = 256;
/// The largest number of options a poll has (ADR-0092 section 3.1).
pub const MAX_OPTIONS: usize = 10;
/// The longest group id accepted.
pub const MAX_GROUP_ID_LEN: usize = 255;
/// The largest anonymity set a ballot proves over: 256 keys. Chosen, not
/// measured (maintainer, 2026-11-15): it bounds proving and verifying cost
/// whatever the group size.
pub const ANONYMITY_SET: usize = capacity(MAX_DIGITS);

const TAG_LEN: usize = 16;
const HASH_LEN: usize = 32;
const TAG_BASE_DOMAIN: &str = "lakko/poll/tag-base/v1";
const SEAL_INFO: &[u8] = b"lakko/poll/ballot-seal/v1";
const CONTEXT_DOMAIN: &[u8] = b"lakko/poll/ballot-context/v1\0";
const DRAW_DOMAIN: &str = "lakko/poll/anonymity-set/v1";
/// Bound on the rejection-sampling loop of [`draw_set`]. With a ring version
/// of `L > 256` keys, all valid, filling the set needs about
/// `L * (H_L - H_{L-255})` draws in the worst case `L = 257` (about 1,500);
/// the bound only stops a version whose keys are mostly not valid points.
const MAX_DRAW_ATTEMPTS: usize = 1 << 20;

/// The longest ballot: the largest set, ten choices, the largest proof.
pub const MAX_BALLOT_LEN: usize = POLL_ID_LEN
    + HASH_LEN
    + 2
    + 4 * ANONYMITY_SET
    + 1
    + MAX_OPTIONS
    + POINT_LEN
    + encoded_len(MAX_DIGITS);
/// The longest padded plaintext.
pub const MAX_PLAINTEXT_LEN: usize = (2 + MAX_BALLOT_LEN).div_ceil(PAD_BLOCK) * PAD_BLOCK;
/// The longest row.
pub const MAX_ROW_LEN: usize =
    BALLOT_ROW_DOMAIN.len() + 1 + NONCE_LEN + MAX_PLAINTEXT_LEN + TAG_LEN;

/// Whether `row` starts with the ballot row domain (the receiver's dispatch
/// test: such a row is never fed to MLS processing).
#[must_use]
pub fn is_ballot_row(row: &[u8]) -> bool {
    row.starts_with(BALLOT_ROW_DOMAIN)
}

/// The chosen option indices: strictly ascending, each below
/// [`MAX_OPTIONS`]. Empty means a retracted vote. Whether the count and the
/// indices fit a given poll is the tally's question ([`crate::tally`]).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Choices(Vec<u8>);

impl Choices {
    /// Validate and wrap.
    pub fn new(indices: &[u8]) -> Result<Self> {
        if indices.len() > MAX_OPTIONS
            || indices.iter().any(|&i| usize::from(i) >= MAX_OPTIONS)
            || indices.windows(2).any(|w| w[0] >= w[1])
        {
            return Err(BallotError::InvalidChoices);
        }
        Ok(Self(indices.to_vec()))
    }

    /// The indices.
    #[must_use]
    pub fn indices(&self) -> &[u8] {
        &self.0
    }

    /// `count || index*`.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + self.0.len());
        // At most MAX_OPTIONS, which fits a byte.
        out.push(u8::try_from(self.0.len()).unwrap_or(u8::MAX));
        out.extend_from_slice(&self.0);
        out
    }
}

/// The anonymity set: indices into one ring version, strictly ascending.
/// Whether it fits a given version is checked by [`AnonymitySet::check`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AnonymitySet(Vec<u32>);

impl AnonymitySet {
    /// Wrap indices that are strictly ascending and at most
    /// [`ANONYMITY_SET`] long. Fit to a version is a separate check.
    pub fn new(indices: &[u32]) -> Result<Self> {
        if indices.is_empty()
            || indices.len() > ANONYMITY_SET
            || indices.windows(2).any(|w| w[0] >= w[1])
        {
            return Err(BallotError::InvalidSet);
        }
        Ok(Self(indices.to_vec()))
    }

    /// The indices.
    #[must_use]
    pub fn indices(&self) -> &[u32] {
        &self.0
    }

    /// `count(u16 BE) || index(u32 BE)*`.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(2 + 4 * self.0.len());
        // At most ANONYMITY_SET (256), which fits a u16.
        out.extend_from_slice(
            &u16::try_from(self.0.len())
                .unwrap_or(u16::MAX)
                .to_be_bytes(),
        );
        for index in &self.0 {
            out.extend_from_slice(&index.to_be_bytes());
        }
        out
    }

    /// The set fits a version of `version_len` keys: exactly
    /// `min(256, version_len)` indices, each below `version_len` (ascending
    /// order is the constructor's guarantee).
    pub fn check(&self, version_len: usize) -> Result<()> {
        let expected = version_len.min(ANONYMITY_SET);
        let fits = self.0.len() == expected
            && self
                .0
                .last()
                .is_some_and(|&last| (last as usize) < version_len);
        if fits {
            Ok(())
        } else {
            Err(BallotError::InvalidSet)
        }
    }
}

/// Everything a member needs to cast and read the ballots of one anonymous
/// poll: the group id, the poll id, the seal key derived from the poll's
/// secret and the poll's tag base.
pub struct PollKeys {
    group_id: Vec<u8>,
    poll_id: [u8; POLL_ID_LEN],
    seal_key: SecretBytes<32>,
    tag_base: EdwardsPoint,
}

impl core::fmt::Debug for PollKeys {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PollKeys")
            .field("poll_id", &self.poll_id)
            .finish_non_exhaustive()
    }
}

impl PollKeys {
    /// Derive the keys of the poll `poll_id` in group `group_id` from the
    /// 32-byte secret in its body.
    pub fn new(
        group_id: &[u8],
        poll_id: &[u8; POLL_ID_LEN],
        secret: &[u8; POLL_SECRET_LEN],
    ) -> Result<Self> {
        if group_id.is_empty() || group_id.len() > MAX_GROUP_ID_LEN {
            return Err(BallotError::InvalidGroupId);
        }
        let mut info = Vec::with_capacity(SEAL_INFO.len() + POLL_ID_LEN);
        info.extend_from_slice(SEAL_INFO);
        info.extend_from_slice(poll_id);
        let seal_key = lakko_crypto::kdf::hkdf_sha256::<32>(secret, &[], &info)
            .map_err(|_| BallotError::Kdf)?;
        let tag_base = hash_to_point(TAG_BASE_DOMAIN, &group_and_poll(group_id, poll_id))?;
        Ok(Self {
            group_id: group_id.to_vec(),
            poll_id: *poll_id,
            seal_key,
            tag_base,
        })
    }

    /// The poll id.
    #[must_use]
    pub const fn poll_id(&self) -> &[u8; POLL_ID_LEN] {
        &self.poll_id
    }

    /// The tag `voter` produces in this poll.
    #[must_use]
    pub fn tag_of(&self, voter: &VoterKey) -> [u8; POINT_LEN] {
        (self.tag_base * voter.scalar().invert())
            .compress()
            .to_bytes()
    }

    fn context(
        &self,
        ring_hash: &[u8; HASH_LEN],
        set: &AnonymitySet,
        choices: &Choices,
    ) -> Vec<u8> {
        let set_bytes = set.encode();
        let mut out = Vec::with_capacity(
            CONTEXT_DOMAIN.len()
                + 1
                + self.group_id.len()
                + POLL_ID_LEN
                + HASH_LEN
                + set_bytes.len()
                + 11,
        );
        out.extend_from_slice(CONTEXT_DOMAIN);
        out.extend_from_slice(&group_and_poll(&self.group_id, &self.poll_id));
        out.extend_from_slice(ring_hash);
        out.extend_from_slice(&set_bytes);
        out.extend_from_slice(&choices.encode());
        out
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new(self.seal_key.as_bytes().into())
    }
}

fn group_and_poll(group_id: &[u8], poll_id: &[u8; POLL_ID_LEN]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + group_id.len() + POLL_ID_LEN);
    // The caller has checked group_id.len() <= 255.
    out.push(u8::try_from(group_id.len()).unwrap_or(u8::MAX));
    out.extend_from_slice(group_id);
    out.extend_from_slice(poll_id);
    out
}

/// A ballot as decoded from an opened row. Nothing in it is verified yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ballot {
    /// The poll it is for.
    pub poll_id: [u8; POLL_ID_LEN],
    /// The ring version it was proven against.
    pub ring_hash: [u8; HASH_LEN],
    /// The anonymity set, as indices into that version.
    pub set: AnonymitySet,
    /// The choices.
    pub choices: Choices,
    /// The tag.
    pub tag: [u8; POINT_LEN],
    /// The proof.
    pub proof: Proof,
}

impl Ballot {
    /// `id || ring_hash || set || choices || tag || proof`.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(MAX_BALLOT_LEN);
        out.extend_from_slice(&self.poll_id);
        out.extend_from_slice(&self.ring_hash);
        out.extend_from_slice(&self.set.encode());
        out.extend_from_slice(&self.choices.encode());
        out.extend_from_slice(&self.tag);
        out.extend_from_slice(&self.proof.encode());
        out
    }

    /// Strict decode: exact length, a well-formed set, valid choices, a
    /// well-formed proof.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_BALLOT_LEN {
            return Err(BallotError::Malformed);
        }
        let (poll_id, rest) = take::<POLL_ID_LEN>(bytes)?;
        let (ring_hash, rest) = take::<HASH_LEN>(rest)?;
        let (set_count, rest) = take::<2>(rest)?;
        let set_count = usize::from(u16::from_be_bytes(set_count));
        if set_count == 0 || set_count > ANONYMITY_SET || rest.len() < 4 * set_count {
            return Err(BallotError::Malformed);
        }
        let (set_bytes, rest) = rest.split_at(4 * set_count);
        let indices: Vec<u32> = set_bytes
            .chunks_exact(4)
            .map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let set = AnonymitySet::new(&indices)?;
        let (&count, rest) = rest.split_first().ok_or(BallotError::Malformed)?;
        let count = usize::from(count);
        if count > MAX_OPTIONS || rest.len() < count {
            return Err(BallotError::Malformed);
        }
        let (choice_bytes, rest) = rest.split_at(count);
        let choices = Choices::new(choice_bytes)?;
        let (tag, rest) = take::<POINT_LEN>(rest)?;
        let proof = Proof::decode(rest)?;
        Ok(Self {
            poll_id,
            ring_hash,
            set,
            choices,
            tag,
            proof,
        })
    }
}

fn take<const N: usize>(bytes: &[u8]) -> Result<([u8; N], &[u8])> {
    if bytes.len() < N {
        return Err(BallotError::Malformed);
    }
    let (head, rest) = bytes.split_at(N);
    let mut out = [0u8; N];
    out.copy_from_slice(head);
    Ok((out, rest))
}

/// A ballot that opened, names this poll and verified against a ring
/// version this device holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedBallot {
    /// The voter's tag in this poll.
    pub tag: [u8; POINT_LEN],
    /// The choices (empty = retracted).
    pub choices: Choices,
    /// The ring version it was proven against.
    pub version: usize,
}

/// The keys and points a set selects from `ring`. Refuses a set whose keys
/// do not all decode.
fn selected(ring: &Ring, set: &AnonymitySet) -> Result<(Vec<[u8; POINT_LEN]>, Vec<EdwardsPoint>)> {
    let mut keys = Vec::with_capacity(set.0.len());
    let mut points = Vec::with_capacity(set.0.len());
    for &index in &set.0 {
        let index = index as usize;
        let key = ring.keys().get(index).ok_or(BallotError::InvalidSet)?;
        keys.push(*key);
        points.push(ring.point(index)?);
    }
    Ok((keys, points))
}

/// The voter's anonymity set in this poll (ADR-0092 section 3.6, as amended
/// 2026-11-15), drawn from ring version `set_version`, or from the newest
/// version this device holds when `set_version` is `None` (the account's
/// first vote in the poll).
///
/// - A version of at most [`ANONYMITY_SET`] keys: every index of it.
/// - A larger one: the voter's position and 255 other distinct positions,
///   drawn by rejection sampling over a BLAKE3 XOF keyed by
///   `BLAKE3-derive-key("lakko/poll/anonymity-set/v1", x || len(group_id) ||
///   group_id || id || version u32 BE)`, `x` the voter's secret scalar.
///   Indices whose keys are not valid points are skipped. Sorted ascending.
///
/// Deterministic in `(x, group, poll, version)`: every device of the account
/// that draws at the same version draws the same set. The client records the
/// version of the first vote in the account's own poll-choice register
/// (`lakko_sync::synced::PollChoiceEntry::set_version`) and passes it on
/// every revote, so a revote never draws a new set. Returns the version and
/// the set.
///
/// [`BallotError::UnknownRingVersion`] when `set_version` is a version this
/// device does not hold yet (retry after the next commit);
/// [`BallotError::NotInRing`] when the voter's key is not in the version.
pub fn draw_set(
    keys: &PollKeys,
    ring: &Ring,
    voter: &VoterKey,
    set_version: Option<u32>,
) -> Result<(usize, AnonymitySet)> {
    let version = match set_version {
        Some(v) => {
            let v = v as usize;
            if v >= ring.version_count() {
                return Err(BallotError::UnknownRingVersion);
            }
            v
        },
        None => ring
            .version_count()
            .checked_sub(1)
            .ok_or(BallotError::RingVersionUnusable)?,
    };
    let len = ring
        .version_len(version)
        .ok_or(BallotError::RingVersionUnusable)?;
    let own = ring
        .position(voter.public_key())
        .filter(|&p| p < len)
        .ok_or(BallotError::NotInRing)?;
    if len <= ANONYMITY_SET {
        let all: Vec<u32> = (0..len)
            .map(|i| u32::try_from(i).unwrap_or(u32::MAX))
            .collect();
        return Ok((version, AnonymitySet::new(&all)?));
    }

    let version_u32 = u32::try_from(version).map_err(|_| BallotError::RingTooLarge)?;
    let mut scalar_bytes = voter.scalar().to_bytes();
    let mut seeder = blake3::Hasher::new_derive_key(DRAW_DOMAIN);
    seeder.update(&scalar_bytes);
    seeder.update(&group_and_poll(&keys.group_id, &keys.poll_id));
    seeder.update(&version_u32.to_be_bytes());
    scalar_bytes.zeroize();
    let mut seed = *seeder.finalize().as_bytes();
    let mut reader = blake3::Hasher::new_keyed(&seed).finalize_xof();
    seed.zeroize();

    let len_u64 = len as u64;
    // Largest multiple of `len` that fits a u64: draws at or above it are
    // rejected so the index is uniform.
    let zone = u64::MAX - (u64::MAX % len_u64);
    let mut chosen: BTreeSet<u32> = BTreeSet::new();
    chosen.insert(u32::try_from(own).map_err(|_| BallotError::RingTooLarge)?);
    let mut attempts = 0usize;
    while chosen.len() < ANONYMITY_SET {
        attempts += 1;
        if attempts > MAX_DRAW_ATTEMPTS {
            return Err(BallotError::RingVersionUnusable);
        }
        let mut word = [0u8; 8];
        reader.fill(&mut word);
        let value = u64::from_be_bytes(word);
        if value >= zone {
            continue;
        }
        let index = value % len_u64;
        let index_u32 = u32::try_from(index).map_err(|_| BallotError::RingTooLarge)?;
        if chosen.contains(&index_u32) || ring.point(index_u32 as usize).is_err() {
            continue;
        }
        chosen.insert(index_u32);
    }
    let indices: Vec<u32> = chosen.into_iter().collect();
    Ok((version, AnonymitySet::new(&indices)?))
}

/// Cast a ballot: draw the voter's anonymity set ([`draw_set`]) at
/// `set_version` (the recorded version of the account's first vote in this
/// poll; `None` only for that first vote, which takes the newest version) and
/// prove over it.
pub fn cast(
    keys: &PollKeys,
    ring: &Ring,
    voter: &VoterKey,
    choices: &Choices,
    set_version: Option<u32>,
) -> Result<Cast> {
    let (version, set) = draw_set(keys, ring, voter, set_version)?;
    let row = cast_with_set(keys, ring, version, &set, voter, choices)?;
    Ok(Cast {
        row,
        set_version: u32::try_from(version).map_err(|_| BallotError::RingTooLarge)?,
    })
}

/// A cast ballot: the row to post and the ring version its set was drawn
/// from, which the client records in the own poll-choice register.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cast {
    /// The sealed row.
    pub row: Vec<u8>,
    /// The ring version the anonymity set was drawn from.
    pub set_version: u32,
}

/// The ring version of the voter's own latest ballot among `verified` (found
/// by its tag), or `None`. A fallback for [`cast`]'s `set_version` when the
/// own poll-choice register has not arrived from the device that voted
/// first, but that device's ballot is already readable: reusing its version
/// keeps the account on one set.
#[must_use]
pub fn own_set_version(
    keys: &PollKeys,
    voter: &VoterKey,
    verified: &[VerifiedBallot],
) -> Option<u32> {
    let tag = keys.tag_of(voter);
    verified
        .iter()
        .rev()
        .find(|b| b.tag == tag)
        .and_then(|b| u32::try_from(b.version).ok())
}

/// Cast a ballot over a caller-chosen set of a given version. Clients use
/// [`cast`]; this exists for tests, to show the verifier accepts any
/// well-formed set.
#[doc(hidden)]
pub fn cast_with_set(
    keys: &PollKeys,
    ring: &Ring,
    version: usize,
    set: &AnonymitySet,
    voter: &VoterKey,
    choices: &Choices,
) -> Result<Vec<u8>> {
    let len = ring
        .version_len(version)
        .ok_or(BallotError::RingVersionUnusable)?;
    let ring_hash = ring
        .version_hash(version)
        .ok_or(BallotError::RingVersionUnusable)?;
    set.check(len)?;
    let (set_keys, set_points) = selected(ring, set)?;
    let index = set_keys
        .iter()
        .position(|k| k == voter.public_key())
        .ok_or(BallotError::NotInRing)?;
    let view = RingRef {
        keys: &set_keys,
        points: &set_points,
    };
    let tag_point = keys.tag_base * voter.scalar().invert();
    let context = keys.context(&ring_hash, set, choices);
    let proof = proof::prove(
        view,
        index,
        voter.scalar(),
        &keys.tag_base,
        &tag_point,
        &context,
    )?;
    let ballot = Ballot {
        poll_id: keys.poll_id,
        ring_hash,
        set: set.clone(),
        choices: choices.clone(),
        tag: tag_point.compress().to_bytes(),
        proof,
    };
    seal_ballot(keys, &ballot)
}

/// Seal an encoded ballot into a row.
fn seal_ballot(keys: &PollKeys, ballot: &Ballot) -> Result<Vec<u8>> {
    let encoded = ballot.encode();
    let len = u16::try_from(encoded.len()).map_err(|_| BallotError::Malformed)?;
    let padded_len = (2 + encoded.len()).div_ceil(PAD_BLOCK) * PAD_BLOCK;
    let mut plaintext = Vec::with_capacity(padded_len);
    plaintext.extend_from_slice(&len.to_be_bytes());
    plaintext.extend_from_slice(&encoded);
    plaintext.resize(padded_len, 0);

    let mut nonce = [0u8; NONCE_LEN];
    lakko_crypto::random::fill(&mut nonce);
    let aad = row_header_without_nonce();
    let ciphertext = keys
        .cipher()
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| BallotError::Malformed)?;
    plaintext.zeroize();

    let mut row = Vec::with_capacity(aad.len() + NONCE_LEN + ciphertext.len());
    row.extend_from_slice(&aad);
    row.extend_from_slice(&nonce);
    row.extend_from_slice(&ciphertext);
    Ok(row)
}

fn row_header_without_nonce() -> Vec<u8> {
    let mut out = Vec::with_capacity(BALLOT_ROW_DOMAIN.len() + 1);
    out.extend_from_slice(BALLOT_ROW_DOMAIN);
    out.push(ROW_VERSION);
    out
}

/// Parse the padded plaintext: `len || ballot || zeros`, a multiple of
/// [`PAD_BLOCK`], the padding all zero and the shortest that reaches a
/// block boundary.
pub(crate) fn unpad(plaintext: &[u8]) -> Result<&[u8]> {
    if plaintext.is_empty()
        || plaintext.len() % PAD_BLOCK != 0
        || plaintext.len() > MAX_PLAINTEXT_LEN
    {
        return Err(BallotError::Malformed);
    }
    let (len, rest) = take::<2>(plaintext)?;
    let len = usize::from(u16::from_be_bytes(len));
    if len > rest.len() {
        return Err(BallotError::Malformed);
    }
    let (body, padding) = rest.split_at(len);
    if padding.len() >= PAD_BLOCK || padding.iter().any(|&b| b != 0) {
        return Err(BallotError::Malformed);
    }
    Ok(body)
}

/// Open a row and decode its ballot. Checks the row domain, the length
/// bounds and the version byte before any work, then the AEAD, the padding,
/// the ballot layout and the poll id. Does not verify the proof.
pub fn open_row(keys: &PollKeys, row: &[u8]) -> Result<Ballot> {
    let header = row_header_without_nonce();
    if !row.starts_with(BALLOT_ROW_DOMAIN) {
        return Err(BallotError::Malformed);
    }
    if row.len() > MAX_ROW_LEN || row.len() < header.len() + NONCE_LEN + TAG_LEN {
        return Err(BallotError::Malformed);
    }
    if row[BALLOT_ROW_DOMAIN.len()] != ROW_VERSION {
        return Err(BallotError::UnknownVersion);
    }
    let (nonce, ciphertext) = row[header.len()..].split_at(NONCE_LEN);
    let mut plaintext = keys
        .cipher()
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: &header,
            },
        )
        .map_err(|_| BallotError::RowDidNotOpen)?;
    let result = unpad(&plaintext).and_then(Ballot::decode);
    plaintext.zeroize();
    let ballot = result?;
    if ballot.poll_id != keys.poll_id {
        return Err(BallotError::WrongPoll);
    }
    Ok(ballot)
}

/// Verify a decoded ballot against the ring version it names.
///
/// Refuses a set that does not fit the version (`min(256, L_k)` indices,
/// ascending, in range) or that selects a key which is not a valid point.
/// [`BallotError::UnknownRingVersion`] means the device does not hold that
/// version yet: keep the row and retry after the next commit.
pub fn verify_ballot(keys: &PollKeys, ring: &Ring, ballot: &Ballot) -> Result<VerifiedBallot> {
    if ballot.poll_id != keys.poll_id {
        return Err(BallotError::WrongPoll);
    }
    let version = ring
        .find_version(&ballot.ring_hash)
        .ok_or(BallotError::UnknownRingVersion)?;
    let len = ring
        .version_len(version)
        .ok_or(BallotError::RingVersionUnusable)?;
    ballot.set.check(len)?;
    let (set_keys, set_points) = selected(ring, &ballot.set)?;
    let view = RingRef {
        keys: &set_keys,
        points: &set_points,
    };
    let tag_point =
        decode_nonidentity_point(&ballot.tag).map_err(|_| BallotError::ProofRejected)?;
    let context = keys.context(&ballot.ring_hash, &ballot.set, &ballot.choices);
    proof::verify(
        view,
        &keys.tag_base,
        &ballot.tag,
        &tag_point,
        &ballot.proof,
        &context,
    )?;
    Ok(VerifiedBallot {
        tag: ballot.tag,
        choices: ballot.choices.clone(),
        version,
    })
}

/// [`open_row`] then [`verify_ballot`].
pub fn open_and_verify(keys: &PollKeys, ring: &Ring, row: &[u8]) -> Result<VerifiedBallot> {
    let ballot = open_row(keys, row)?;
    verify_ballot(keys, ring, &ballot)
}

#[doc(hidden)]
pub fn fuzz_unpad_and_decode(plaintext: &[u8]) -> Result<Ballot> {
    unpad(plaintext).and_then(Ballot::decode)
}
