// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0092 section 3 anonymous group-poll ballots on the wasm surface. The
//! UniFFI mirror is `lakko-ffi-uniffi/src/ballot.rs`; every rule is
//! `lakko-ballot`'s. Lists of 32-byte keys cross flat-concatenated.
//!
//! Errors throw an `Error` whose message is `"<code>: <reason>"`
//! (`lakko_ballot::BallotError::code`, or `bad-length`).

use lakko_ballot as core;
use lakko_ballot::point::POINT_LEN;
use wasm_bindgen::prelude::*;

fn ballot_err(e: core::BallotError) -> JsError {
    JsError::new(&format!("{}: {e}", e.code()))
}

fn fixed<const N: usize>(v: &[u8], what: &str) -> Result<[u8; N], JsError> {
    <[u8; N]>::try_from(v).map_err(|_| {
        JsError::new(&format!(
            "bad-length: {what} must be {N} bytes, got {}",
            v.len()
        ))
    })
}

fn keys_of(flat: &[u8]) -> Result<Vec<[u8; POINT_LEN]>, JsError> {
    if flat.len() % POINT_LEN != 0 {
        return Err(JsError::new(&format!(
            "bad-length: keys must be a multiple of {POINT_LEN} bytes, got {}",
            flat.len()
        )));
    }
    flat.chunks(POINT_LEN)
        .map(|k| fixed::<POINT_LEN>(k, "key"))
        .collect()
}

fn u32_of(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The hash of a ring version's keys (`keys` flat-concatenated 32-byte
/// account keys in ring order).
///
/// # Errors
///
/// `bad-length`.
#[wasm_bindgen(js_name = pollRingHash)]
pub fn poll_ring_hash(keys: &[u8]) -> Result<Vec<u8>, JsError> {
    Ok(core::ring::ring_hash(&keys_of(keys)?).to_vec())
}

/// A poll's ring of account keys and its versions.
#[wasm_bindgen]
pub struct PollRing {
    pub(crate) inner: core::Ring,
}

#[wasm_bindgen]
impl PollRing {
    /// Version 0 from flat-concatenated keys, strictly ascending.
    ///
    /// # Errors
    ///
    /// `invalid-ring`, `bad-length`.
    #[wasm_bindgen(constructor)]
    pub fn new(keys: &[u8]) -> Result<PollRing, JsError> {
        Ok(Self {
            inner: core::Ring::new(&keys_of(keys)?).map_err(ballot_err)?,
        })
    }

    /// Restore a ring from `encode()`.
    ///
    /// # Errors
    ///
    /// Malformed bytes.
    #[wasm_bindgen]
    pub fn decode(bytes: &[u8]) -> Result<PollRing, JsError> {
        Ok(Self {
            inner: core::Ring::decode(bytes).map_err(ballot_err)?,
        })
    }

    /// Apply a merged commit's added account keys (flat); returns the new
    /// version, or `undefined` when nobody new was added.
    ///
    /// # Errors
    ///
    /// `ring-too-large`, `bad-length`.
    #[wasm_bindgen]
    pub fn extend(&mut self, added: &[u8]) -> Result<Option<u32>, JsError> {
        let added = keys_of(added)?;
        Ok(self.inner.extend(&added).map_err(ballot_err)?.map(u32_of))
    }

    /// The persisted form.
    #[wasm_bindgen]
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        self.inner.encode()
    }

    /// How many versions the ring holds.
    #[wasm_bindgen(getter, js_name = versionCount)]
    #[must_use]
    pub fn version_count(&self) -> u32 {
        u32_of(self.inner.version_count())
    }

    /// The key count of a version.
    #[wasm_bindgen(js_name = versionLen)]
    #[must_use]
    pub fn version_len(&self, version: u32) -> Option<u32> {
        self.inner.version_len(version as usize).map(u32_of)
    }

    /// The hash of a version.
    #[wasm_bindgen(js_name = versionHash)]
    #[must_use]
    pub fn version_hash(&self, version: u32) -> Option<Vec<u8>> {
        self.inner
            .version_hash(version as usize)
            .map(|h| h.to_vec())
    }

    /// The version with this hash, if held.
    ///
    /// # Errors
    ///
    /// `bad-length`.
    #[wasm_bindgen(js_name = findVersion)]
    pub fn find_version(&self, hash: &[u8]) -> Result<Option<u32>, JsError> {
        let hash = fixed::<32>(hash, "hash")?;
        Ok(self.inner.find_version(&hash).map(u32_of))
    }

    /// Whether the key is in the ring.
    ///
    /// # Errors
    ///
    /// `bad-length`.
    #[wasm_bindgen]
    pub fn contains(&self, key: &[u8]) -> Result<bool, JsError> {
        Ok(self.inner.contains(&fixed::<POINT_LEN>(key, "key")?))
    }

    /// Every key in ring order, flat-concatenated.
    #[wasm_bindgen]
    #[must_use]
    pub fn keys(&self) -> Vec<u8> {
        self.inner.keys().concat()
    }
}

/// The voter's key (secret; wiped on drop).
#[wasm_bindgen]
pub struct PollVoterKey {
    pub(crate) inner: core::VoterKey,
}

#[wasm_bindgen]
impl PollVoterKey {
    /// From the account's 32-byte Ed25519 seed.
    ///
    /// # Errors
    ///
    /// `bad-length`.
    #[wasm_bindgen(js_name = fromEd25519Seed)]
    pub fn from_ed25519_seed(seed: &[u8]) -> Result<PollVoterKey, JsError> {
        let seed = fixed::<32>(seed, "seed")?;
        Ok(Self {
            inner: core::VoterKey::from_ed25519_seed(&seed),
        })
    }

    /// The public key.
    #[wasm_bindgen(getter, js_name = publicKey)]
    #[must_use]
    pub fn public_key(&self) -> Vec<u8> {
        self.inner.public_key().to_vec()
    }
}

/// The keys of one anonymous poll.
#[wasm_bindgen]
pub struct PollBallotKeys {
    inner: core::PollKeys,
}

#[wasm_bindgen]
impl PollBallotKeys {
    /// `pollId` 16 bytes, `secret` 32 bytes (from the poll body).
    ///
    /// # Errors
    ///
    /// `invalid-group-id`, `bad-length`, `kdf`.
    #[wasm_bindgen(constructor)]
    pub fn new(group_id: &[u8], poll_id: &[u8], secret: &[u8]) -> Result<PollBallotKeys, JsError> {
        let poll_id = fixed::<{ core::ballot::POLL_ID_LEN }>(poll_id, "pollId")?;
        let secret = fixed::<{ core::ballot::POLL_SECRET_LEN }>(secret, "secret")?;
        Ok(Self {
            inner: core::PollKeys::new(group_id, &poll_id, &secret).map_err(ballot_err)?,
        })
    }

    /// The poll id.
    #[wasm_bindgen(getter, js_name = pollId)]
    #[must_use]
    pub fn poll_id(&self) -> Vec<u8> {
        self.inner.poll_id().to_vec()
    }

    /// The tag `voter` produces in this poll.
    #[wasm_bindgen(js_name = tagOf)]
    #[must_use]
    pub fn tag_of(&self, voter: &PollVoterKey) -> Vec<u8> {
        self.inner.tag_of(&voter.inner).to_vec()
    }
}

/// A cast ballot.
#[wasm_bindgen]
pub struct PollBallotCast {
    row: Vec<u8>,
    set_version: u32,
}

#[wasm_bindgen]
impl PollBallotCast {
    /// The sealed journal row.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn row(&self) -> Vec<u8> {
        self.row.clone()
    }

    /// The ring version of the anonymity set (record it in the own
    /// poll-choice register).
    #[wasm_bindgen(getter, js_name = setVersion)]
    #[must_use]
    pub fn set_version(&self) -> u32 {
        self.set_version
    }
}

/// A ballot that verified.
#[wasm_bindgen]
#[derive(Clone)]
pub struct VerifiedPollBallot {
    tag: Vec<u8>,
    choices: Vec<u8>,
    version: u32,
}

#[wasm_bindgen]
impl VerifiedPollBallot {
    /// Rebuild one from stored fields (for `ownPollBallotSetVersion`).
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(tag: Vec<u8>, choices: Vec<u8>, version: u32) -> VerifiedPollBallot {
        Self {
            tag,
            choices,
            version,
        }
    }

    /// The voter's tag.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn tag(&self) -> Vec<u8> {
        self.tag.clone()
    }

    /// The choices.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn choices(&self) -> Vec<u8> {
        self.choices.clone()
    }

    /// The ring version proven against.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }
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

/// Cast a ballot. `setVersion` is the version recorded at the account's
/// first vote (`undefined` only for that first vote).
///
/// # Errors
///
/// `invalid-choices`, `not-in-ring`, `unknown-ring-version`.
#[wasm_bindgen(js_name = castPollBallot)]
pub fn cast_poll_ballot(
    keys: &PollBallotKeys,
    ring: &PollRing,
    voter: &PollVoterKey,
    choices: &[u8],
    set_version: Option<u32>,
) -> Result<PollBallotCast, JsError> {
    let choices = core::Choices::new(choices).map_err(ballot_err)?;
    let cast = core::cast(
        &keys.inner,
        &ring.inner,
        &voter.inner,
        &choices,
        set_version,
    )
    .map_err(ballot_err)?;
    Ok(PollBallotCast {
        row: cast.row,
        set_version: cast.set_version,
    })
}

/// A decoded, unverified ballot (kept to retry after the next commit).
#[wasm_bindgen]
pub struct PollBallot {
    inner: core::Ballot,
}

#[wasm_bindgen]
impl PollBallot {
    /// The voter's tag.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn tag(&self) -> Vec<u8> {
        self.inner.tag.to_vec()
    }

    /// The hash of the ring version it names.
    #[wasm_bindgen(getter, js_name = ringHash)]
    #[must_use]
    pub fn ring_hash(&self) -> Vec<u8> {
        self.inner.ring_hash.to_vec()
    }

    /// The choices.
    #[wasm_bindgen(getter)]
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
#[wasm_bindgen(js_name = openPollBallotRow)]
pub fn open_poll_ballot_row(keys: &PollBallotKeys, row: &[u8]) -> Result<PollBallot, JsError> {
    Ok(PollBallot {
        inner: core::open_row(&keys.inner, row).map_err(ballot_err)?,
    })
}

/// Verify a decoded ballot against the ring version it names.
///
/// # Errors
///
/// `unknown-ring-version` (retry after the next commit), `invalid-set`,
/// `proof-rejected`, `wrong-poll`.
#[wasm_bindgen(js_name = verifyPollBallot)]
pub fn verify_poll_ballot(
    keys: &PollBallotKeys,
    ring: &PollRing,
    ballot: &PollBallot,
) -> Result<VerifiedPollBallot, JsError> {
    core::verify_ballot(&keys.inner, &ring.inner, &ballot.inner)
        .map(Into::into)
        .map_err(ballot_err)
}

/// `openPollBallotRow` then `verifyPollBallot`.
///
/// # Errors
///
/// What either refuses.
#[wasm_bindgen(js_name = openAndVerifyPollBallot)]
pub fn open_and_verify_poll_ballot(
    keys: &PollBallotKeys,
    ring: &PollRing,
    row: &[u8],
) -> Result<VerifiedPollBallot, JsError> {
    core::open_and_verify(&keys.inner, &ring.inner, row)
        .map(Into::into)
        .map_err(ballot_err)
}

/// The ring version of the voter's own latest ballot among `verified`.
///
/// # Errors
///
/// `bad-length`, `invalid-choices`.
#[wasm_bindgen(js_name = ownPollBallotSetVersion)]
pub fn own_poll_ballot_set_version(
    keys: &PollBallotKeys,
    voter: &PollVoterKey,
    verified: Vec<VerifiedPollBallot>,
) -> Result<Option<u32>, JsError> {
    let verified = verified
        .into_iter()
        .map(|b| {
            Ok(core::VerifiedBallot {
                tag: fixed::<POINT_LEN>(&b.tag, "tag")?,
                choices: core::Choices::new(&b.choices).map_err(ballot_err)?,
                version: b.version as usize,
            })
        })
        .collect::<Result<Vec<_>, JsError>>()?;
    Ok(core::own_set_version(&keys.inner, &voter.inner, &verified))
}

/// Whether a journal row is a ballot row.
#[wasm_bindgen(js_name = isPollBallotRow)]
#[must_use]
pub fn is_poll_ballot_row(row: &[u8]) -> bool {
    core::ballot::is_ballot_row(row)
}

/// A verified ballot with its row's DS position, for `foldPollBallots`.
#[wasm_bindgen]
#[derive(Clone)]
pub struct CountedPollBallot {
    message_id: i64,
    created_at_ms: i64,
    tag: Vec<u8>,
    choices: Vec<u8>,
}

#[wasm_bindgen]
impl CountedPollBallot {
    /// `messageId` and `createdAtMs` are `bigint`s.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(message_id: i64, created_at_ms: i64, tag: Vec<u8>, choices: Vec<u8>) -> Self {
        Self {
            message_id,
            created_at_ms,
            tag,
            choices,
        }
    }
}

/// An anonymous poll's tally.
#[wasm_bindgen]
pub struct PollBallotTally {
    counts: Vec<u32>,
    voters: u32,
}

#[wasm_bindgen]
impl PollBallotTally {
    /// Votes per option.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn counts(&self) -> Vec<u32> {
        self.counts.clone()
    }

    /// Voters whose last counted ballot chose something.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn voters(&self) -> u32 {
        self.voters
    }
}

/// Fold verified ballots: last ballot per tag before the close.
/// `closeMessageId` / `closeAtMs` are `bigint | undefined`.
///
/// # Errors
///
/// `bad-length`, `invalid-choices`.
#[wasm_bindgen(js_name = foldPollBallots)]
pub fn fold_poll_ballots(
    option_count: u8,
    multiple: bool,
    close_message_id: Option<i64>,
    close_at_ms: Option<i64>,
    ballots: Vec<CountedPollBallot>,
) -> Result<PollBallotTally, JsError> {
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
        .collect::<Result<Vec<_>, JsError>>()?;
    let t = core::fold(
        &core::PollRules {
            option_count,
            multiple,
            close_message_id,
            close_at_ms,
        },
        &ballots,
    );
    Ok(PollBallotTally {
        counts: t.counts,
        voters: t.voters,
    })
}

/// The anonymity-set size (256).
#[wasm_bindgen(js_name = pollBallotAnonymitySetSize)]
#[must_use]
pub fn poll_ballot_anonymity_set_size() -> u32 {
    u32_of(core::ANONYMITY_SET)
}

/// The longest ballot row, bytes.
#[wasm_bindgen(js_name = pollBallotMaxRowLen)]
#[must_use]
pub fn poll_ballot_max_row_len() -> u32 {
    u32_of(core::ballot::MAX_ROW_LEN)
}
