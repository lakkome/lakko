// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0092 section 3 anonymous group-poll ballots on the UniFFI surface.
//!
//! The ring ([`PollRing`]) is the poll's electorate and its versions: built
//! from version 0 (the poll body's `ring0Hash` must equal its hash) and
//! extended with every merged commit's added account keys. A member casts
//! with [`cast_poll_ballot`], posts the row to the journal, and records the
//! returned set version in its own poll-choice register; it reads others'
//! rows with [`open_and_verify_poll_ballot`] and counts with
//! [`fold_poll_ballots`]. Every rule is `lakko-ballot`'s.
//!
//! Errors: [`FfiError::Ballot`] `"<code>: <reason>"`
//! (`lakko_ballot::BallotError::code`, or `bad-length`).

use std::sync::{Arc, Mutex};

use lakko_ballot as core;
use lakko_ballot::point::POINT_LEN;

use crate::FfiError;

fn ballot_err(e: core::BallotError) -> FfiError {
    FfiError::Ballot(format!("{}: {e}", e.code()))
}

fn fixed<const N: usize>(v: &[u8], what: &str) -> Result<[u8; N], FfiError> {
    <[u8; N]>::try_from(v).map_err(|_| {
        FfiError::Ballot(format!(
            "bad-length: {what} must be {N} bytes, got {}",
            v.len()
        ))
    })
}

fn keys_of(keys: &[Vec<u8>]) -> Result<Vec<[u8; POINT_LEN]>, FfiError> {
    keys.iter().map(|k| fixed::<POINT_LEN>(k, "key")).collect()
}

fn u32_of(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The hash of a ring version's keys (what a poll body's `ring0Hash`
/// carries for version 0). Keys are 32-byte account keys, in ring order.
///
/// # Errors
///
/// [`FfiError::Ballot`] `bad-length`.
#[uniffi::export]
pub fn poll_ring_hash(keys: Vec<Vec<u8>>) -> Result<Vec<u8>, FfiError> {
    Ok(core::ring::ring_hash(&keys_of(&keys)?).to_vec())
}

/// A poll's ring of account keys and its versions.
#[derive(uniffi::Object)]
pub struct PollRing {
    inner: Mutex<core::Ring>,
}

impl PollRing {
    pub(crate) fn from_core(ring: core::Ring) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(ring),
        })
    }

    pub(crate) fn lock(&self) -> Result<std::sync::MutexGuard<'_, core::Ring>, FfiError> {
        self.inner.lock().map_err(|_| FfiError::LockPoisoned)
    }
}

#[uniffi::export]
impl PollRing {
    /// Version 0: the keys strictly ascending, non-empty.
    ///
    /// # Errors
    ///
    /// [`FfiError::Ballot`] `invalid-ring` / `bad-length`.
    #[uniffi::constructor]
    pub fn new(keys: Vec<Vec<u8>>) -> Result<Arc<Self>, FfiError> {
        Ok(Arc::new(Self {
            inner: Mutex::new(core::Ring::new(&keys_of(&keys)?).map_err(ballot_err)?),
        }))
    }

    /// Restore a ring from [`Self::encode`].
    ///
    /// # Errors
    ///
    /// [`FfiError::Ballot`] for malformed bytes.
    #[uniffi::constructor]
    pub fn decode(bytes: Vec<u8>) -> Result<Arc<Self>, FfiError> {
        Ok(Arc::new(Self {
            inner: Mutex::new(core::Ring::decode(&bytes).map_err(ballot_err)?),
        }))
    }

    /// Apply a merged commit's added account keys; returns the new version,
    /// or `None` when nobody new was added.
    ///
    /// # Errors
    ///
    /// [`FfiError::Ballot`] `ring-too-large` / `bad-length`; `LockPoisoned`.
    pub fn extend(&self, added: Vec<Vec<u8>>) -> Result<Option<u32>, FfiError> {
        let added = keys_of(&added)?;
        Ok(self.lock()?.extend(&added).map_err(ballot_err)?.map(u32_of))
    }

    /// The persisted form.
    ///
    /// # Errors
    ///
    /// `LockPoisoned`.
    pub fn encode(&self) -> Result<Vec<u8>, FfiError> {
        Ok(self.lock()?.encode())
    }

    /// How many versions the ring holds.
    ///
    /// # Errors
    ///
    /// `LockPoisoned`.
    pub fn version_count(&self) -> Result<u32, FfiError> {
        Ok(u32_of(self.lock()?.version_count()))
    }

    /// The key count of a version.
    ///
    /// # Errors
    ///
    /// `LockPoisoned`.
    pub fn version_len(&self, version: u32) -> Result<Option<u32>, FfiError> {
        Ok(self.lock()?.version_len(version as usize).map(u32_of))
    }

    /// The hash of a version.
    ///
    /// # Errors
    ///
    /// `LockPoisoned`.
    pub fn version_hash(&self, version: u32) -> Result<Option<Vec<u8>>, FfiError> {
        Ok(self
            .lock()?
            .version_hash(version as usize)
            .map(|h| h.to_vec()))
    }

    /// The version with this hash, if held.
    ///
    /// # Errors
    ///
    /// `bad-length`; `LockPoisoned`.
    pub fn find_version(&self, hash: Vec<u8>) -> Result<Option<u32>, FfiError> {
        let hash = fixed::<32>(&hash, "hash")?;
        Ok(self.lock()?.find_version(&hash).map(u32_of))
    }

    /// Whether the key is in the ring.
    ///
    /// # Errors
    ///
    /// `bad-length`; `LockPoisoned`.
    pub fn contains(&self, key: Vec<u8>) -> Result<bool, FfiError> {
        let key = fixed::<POINT_LEN>(&key, "key")?;
        Ok(self.lock()?.contains(&key))
    }

    /// Every key in ring order.
    ///
    /// # Errors
    ///
    /// `LockPoisoned`.
    pub fn keys(&self) -> Result<Vec<Vec<u8>>, FfiError> {
        Ok(self.lock()?.keys().iter().map(|k| k.to_vec()).collect())
    }
}

/// The voter's key: the Ed25519 signing scalar of the account seed its leaf
/// is bound to. Secret; wiped on drop.
#[derive(uniffi::Object)]
pub struct PollVoterKey {
    inner: core::VoterKey,
}

#[uniffi::export]
impl PollVoterKey {
    /// From the account's 32-byte Ed25519 seed.
    ///
    /// # Errors
    ///
    /// `bad-length`.
    #[uniffi::constructor]
    pub fn from_ed25519_seed(seed: Vec<u8>) -> Result<Arc<Self>, FfiError> {
        let seed = fixed::<32>(&seed, "seed")?;
        Ok(Arc::new(Self {
            inner: core::VoterKey::from_ed25519_seed(&seed),
        }))
    }

    /// The public key (the account's Ed25519 verifying key).
    #[must_use]
    pub fn public_key(&self) -> Vec<u8> {
        self.inner.public_key().to_vec()
    }
}

/// The keys of one anonymous poll: group id, poll id, the seal key from the
/// poll's secret and the tag base.
#[derive(uniffi::Object)]
pub struct PollBallotKeys {
    inner: core::PollKeys,
}

#[uniffi::export]
impl PollBallotKeys {
    /// `poll_id` 16 bytes, `secret` 32 bytes (both from the poll body).
    ///
    /// # Errors
    ///
    /// `invalid-group-id`, `bad-length`, `kdf`.
    #[uniffi::constructor]
    pub fn new(
        group_id: Vec<u8>,
        poll_id: Vec<u8>,
        secret: Vec<u8>,
    ) -> Result<Arc<Self>, FfiError> {
        let poll_id = fixed::<{ core::ballot::POLL_ID_LEN }>(&poll_id, "pollId")?;
        let secret = fixed::<{ core::ballot::POLL_SECRET_LEN }>(&secret, "secret")?;
        Ok(Arc::new(Self {
            inner: core::PollKeys::new(&group_id, &poll_id, &secret).map_err(ballot_err)?,
        }))
    }

    /// The poll id.
    #[must_use]
    pub fn poll_id(&self) -> Vec<u8> {
        self.inner.poll_id().to_vec()
    }

    /// The tag `voter` produces in this poll.
    #[must_use]
    pub fn tag_of(&self, voter: Arc<PollVoterKey>) -> Vec<u8> {
        self.inner.tag_of(&voter.inner).to_vec()
    }
}

/// A cast ballot: the row to post and the ring version its anonymity set
/// was drawn from (record it in the own poll-choice register).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct PollBallotCast {
    /// The sealed journal row.
    pub row: Vec<u8>,
    /// The ring version of the anonymity set.
    pub set_version: u32,
}

/// A ballot that verified.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct VerifiedPollBallot {
    /// The voter's tag (the same for every ballot of one account in one
    /// poll).
    pub tag: Vec<u8>,
    /// The choices (empty = retract).
    pub choices: Vec<u8>,
    /// The ring version the ballot proved against.
    pub version: u32,
}

impl From<core::VerifiedBallot> for VerifiedPollBallot {
    fn from(b: core::VerifiedBallot) -> Self {
        Self {
            tag: b.tag.to_vec(),
            choices: b.choices.indices().to_vec(),
            version: u32_of(b.version),
        }
    }
}

/// Cast a ballot. `set_version` is the version recorded at the account's
/// first vote in this poll (`None` only for that first vote).
///
/// # Errors
///
/// `invalid-choices`, `not-in-ring`, `unknown-ring-version` (retry after
/// the next commit), `LockPoisoned`.
#[uniffi::export]
pub fn cast_poll_ballot(
    keys: Arc<PollBallotKeys>,
    ring: Arc<PollRing>,
    voter: Arc<PollVoterKey>,
    choices: Vec<u8>,
    set_version: Option<u32>,
) -> Result<PollBallotCast, FfiError> {
    let choices = core::Choices::new(&choices).map_err(ballot_err)?;
    let ring = ring.lock()?;
    let cast =
        core::cast(&keys.inner, &ring, &voter.inner, &choices, set_version).map_err(ballot_err)?;
    Ok(PollBallotCast {
        row: cast.row,
        set_version: cast.set_version,
    })
}

/// A decoded, unverified ballot (kept to retry [`verify_poll_ballot`] after
/// the next commit when the ring version is not held yet).
#[derive(uniffi::Object)]
pub struct PollBallot {
    inner: core::Ballot,
}

#[uniffi::export]
impl PollBallot {
    /// The voter's tag.
    #[must_use]
    pub fn tag(&self) -> Vec<u8> {
        self.inner.tag.to_vec()
    }

    /// The hash of the ring version it names.
    #[must_use]
    pub fn ring_hash(&self) -> Vec<u8> {
        self.inner.ring_hash.to_vec()
    }

    /// The choices.
    #[must_use]
    pub fn choices(&self) -> Vec<u8> {
        self.inner.choices.indices().to_vec()
    }
}

/// Open a journal row and decode its ballot (no proof check).
///
/// # Errors
///
/// `malformed`, `unknown-version`, `row-did-not-open`, `wrong-poll`.
#[uniffi::export]
pub fn open_poll_ballot_row(
    keys: Arc<PollBallotKeys>,
    row: Vec<u8>,
) -> Result<Arc<PollBallot>, FfiError> {
    Ok(Arc::new(PollBallot {
        inner: core::open_row(&keys.inner, &row).map_err(ballot_err)?,
    }))
}

/// Verify a decoded ballot against the ring version it names.
///
/// # Errors
///
/// `unknown-ring-version` (keep it, retry after the next commit),
/// `invalid-set`, `proof-rejected`, `wrong-poll`, `LockPoisoned`.
#[uniffi::export]
pub fn verify_poll_ballot(
    keys: Arc<PollBallotKeys>,
    ring: Arc<PollRing>,
    ballot: Arc<PollBallot>,
) -> Result<VerifiedPollBallot, FfiError> {
    let ring = ring.lock()?;
    core::verify_ballot(&keys.inner, &ring, &ballot.inner)
        .map(Into::into)
        .map_err(ballot_err)
}

/// [`open_poll_ballot_row`] then [`verify_poll_ballot`].
///
/// # Errors
///
/// What either refuses.
#[uniffi::export]
pub fn open_and_verify_poll_ballot(
    keys: Arc<PollBallotKeys>,
    ring: Arc<PollRing>,
    row: Vec<u8>,
) -> Result<VerifiedPollBallot, FfiError> {
    let ring = ring.lock()?;
    core::open_and_verify(&keys.inner, &ring, &row)
        .map(Into::into)
        .map_err(ballot_err)
}

/// The ring version of the voter's own latest ballot among `verified`, the
/// fallback for `set_version` when the own poll-choice register has not
/// arrived yet.
///
/// # Errors
///
/// `bad-length`, `invalid-choices` for a malformed entry.
#[uniffi::export]
pub fn own_poll_ballot_set_version(
    keys: Arc<PollBallotKeys>,
    voter: Arc<PollVoterKey>,
    verified: Vec<VerifiedPollBallot>,
) -> Result<Option<u32>, FfiError> {
    let verified = verified
        .into_iter()
        .map(|b| {
            Ok(core::VerifiedBallot {
                tag: fixed::<POINT_LEN>(&b.tag, "tag")?,
                choices: core::Choices::new(&b.choices).map_err(ballot_err)?,
                version: b.version as usize,
            })
        })
        .collect::<Result<Vec<_>, FfiError>>()?;
    Ok(core::own_set_version(&keys.inner, &voter.inner, &verified))
}

/// Whether a journal row is a ballot row (never fed to MLS processing).
#[uniffi::export]
#[must_use]
pub fn is_poll_ballot_row(row: Vec<u8>) -> bool {
    core::ballot::is_ballot_row(&row)
}

/// What the tally needs to know about the poll.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct PollBallotRules {
    /// The number of options.
    pub option_count: u8,
    /// Several options may be chosen.
    pub multiple: bool,
    /// The accepted close's DS `messageId`, if any.
    pub close_message_id: Option<i64>,
    /// The poll's automatic close time, if any.
    pub close_at_ms: Option<i64>,
}

/// A verified ballot with its row's DS position.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CountedPollBallot {
    /// The row's DS `messageId`.
    pub message_id: i64,
    /// The row's DS `created_at`, milliseconds.
    pub created_at_ms: i64,
    /// The voter's tag.
    pub tag: Vec<u8>,
    /// The choices.
    pub choices: Vec<u8>,
}

/// An anonymous poll's tally.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct PollBallotTally {
    /// Votes per option.
    pub counts: Vec<u32>,
    /// Voters whose last counted ballot chose something.
    pub voters: u32,
}

/// Fold verified ballots into a tally: last ballot per tag before the close.
/// Order-independent.
///
/// # Errors
///
/// `bad-length` for a tag, `invalid-choices` for a malformed choice list.
#[uniffi::export]
pub fn fold_poll_ballots(
    rules: PollBallotRules,
    ballots: Vec<CountedPollBallot>,
) -> Result<PollBallotTally, FfiError> {
    let ballots = ballots
        .into_iter()
        .map(|b| {
            Ok(core::CountedBallot {
                message_id: b.message_id,
                created_at_ms: b.created_at_ms,
                tag: fixed::<POINT_LEN>(&b.tag, "tag")?,
                choices: core::Choices::new(&b.choices).map_err(ballot_err)?,
            })
        })
        .collect::<Result<Vec<_>, FfiError>>()?;
    let tally = core::fold(
        &core::PollRules {
            option_count: rules.option_count,
            multiple: rules.multiple,
            close_message_id: rules.close_message_id,
            close_at_ms: rules.close_at_ms,
        },
        &ballots,
    );
    Ok(PollBallotTally {
        counts: tally.counts,
        voters: tally.voters,
    })
}

/// The anonymity-set size (256).
#[uniffi::export]
#[must_use]
pub fn poll_ballot_anonymity_set_size() -> u32 {
    u32_of(core::ANONYMITY_SET)
}

/// The longest ballot row, bytes.
#[uniffi::export]
#[must_use]
pub fn poll_ballot_max_row_len() -> u32 {
    u32_of(core::ballot::MAX_ROW_LEN)
}

/// The ballot row's domain prefix (`lakko/poll-ballot/v1\0`), for a cheap
/// byte peek.
#[uniffi::export]
#[must_use]
pub fn poll_ballot_row_domain() -> Vec<u8> {
    core::ballot::BALLOT_ROW_DOMAIN.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(n: u8) -> Vec<u8> {
        vec![n; 32]
    }

    #[test]
    fn three_voters_cast_and_a_tally_counts_them() {
        let voters: Vec<Arc<PollVoterKey>> = (1..=3)
            .map(|n| PollVoterKey::from_ed25519_seed(seed(n)).unwrap())
            .collect();
        let mut keys: Vec<Vec<u8>> = voters.iter().map(|v| v.public_key()).collect();
        keys.sort();
        let ring = PollRing::new(keys.clone()).unwrap();
        assert_eq!(
            ring.version_hash(0).unwrap().unwrap(),
            poll_ring_hash(keys).unwrap()
        );
        let poll_keys = PollBallotKeys::new(vec![9; 32], vec![1; 16], vec![2; 32]).unwrap();
        let mut counted = Vec::new();
        for (i, v) in voters.iter().enumerate() {
            let cast = cast_poll_ballot(poll_keys.clone(), ring.clone(), v.clone(), vec![0], None)
                .unwrap();
            assert!(is_poll_ballot_row(cast.row.clone()));
            let verified =
                open_and_verify_poll_ballot(poll_keys.clone(), ring.clone(), cast.row).unwrap();
            assert_eq!(verified.tag, poll_keys.tag_of(v.clone()));
            counted.push(CountedPollBallot {
                message_id: i64::try_from(i).unwrap() + 10,
                created_at_ms: 0,
                tag: verified.tag,
                choices: verified.choices,
            });
        }
        let tally = fold_poll_ballots(
            PollBallotRules {
                option_count: 2,
                multiple: false,
                close_message_id: None,
                close_at_ms: None,
            },
            counted,
        )
        .unwrap();
        assert_eq!(tally.counts, vec![3, 0]);
        assert_eq!(tally.voters, 3);
        let restored = PollRing::decode(ring.encode().unwrap()).unwrap();
        assert_eq!(restored.keys().unwrap(), ring.keys().unwrap());
    }

    #[test]
    fn a_wrong_length_is_a_ballot_error_with_a_code() {
        let Err(FfiError::Ballot(m)) = PollRing::new(vec![vec![1; 31]]) else {
            panic!("expected a ballot error");
        };
        assert!(m.starts_with("bad-length: "));
    }
}
