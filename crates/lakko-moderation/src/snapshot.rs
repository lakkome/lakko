// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The suspension snapshot and its bucket query ([ADR-0099] section 5,
//! step 4).
//!
//! Once a period (a day by default) the moderation service builds, from its
//! own public log, the set of accounts suspended and channels removed at
//! that moment, spreads it
//! over [`BUCKET_COUNT`] buckets by the first two bytes of each target, puts
//! the buckets in a Merkle tree and appends the tree's root to the log as a
//! leaf of kind [`LOG_LEAF_SNAPSHOT_ROOT`]. A client that shows an account
//! or a channel asks for one bucket (a 16-bit prefix of the target, so the
//! service learns 16 bits about what it is looking at) and checks the answer
//! against the log head it pins ([`check_target`], [`check_account`]).
//!
//! ```text
//! entry       = kind(1) ‖ target(kind's length) ‖ until_ms(8, 0 = no end)
//!               ‖ log_index(8)
//! bucket      = prefix(2) ‖ count(4) ‖ count × entry
//!               entries strictly increasing by (target bytes, kind); every
//!               target starts with the prefix; count <= MAX_BUCKET_ENTRIES
//! bucket tree = RFC 6962 tree over exactly BUCKET_COUNT leaves, leaf i = the
//!               bytes of bucket i, empty ones included (path length 16)
//! root        = DOMAIN ‖ version(1) ‖ period(8) ‖ period_secs(4)
//!               ‖ built_at_ms(8) ‖ covered_log_size(8) ‖ bucket_root(32)
//!               ‖ entry_count(8)
//! log leaf    = 0x03 ‖ root
//! ```
//!
//! All integers are big-endian. `period = floor(built_at_ms / 1000 /
//! period_secs)`; `covered_log_size` is how many log leaves the snapshot was
//! built from (every leaf before its own), so anyone holding the public log
//! rebuilds it with [`build_snapshot`] and compares `bucket_root`.
//!
//! Two kinds of entry are built: an account suspension (step 4) and a
//! channel removal (step 5, `until_ms` always 0). The entry carries its
//! target kind, so the format did not change when channels joined.
//!
//! A mark is not taken from the snapshot alone: a client acts on a
//! suspended or removed standing only after [`prove_standing`] has checked
//! the statement behind the entry (the log entry at the entry's index, its
//! inclusion under the same head, and the moderator's signature under a
//! root-signed moderator list). A snapshot whose builder was compromised can
//! then only hide a mark, not invent one.
//!
//! [ADR-0099]: ../../../docs/adr/0099-reporting-and-moderation.md

use std::collections::BTreeMap;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_crypto::signing::{Signature, VerifyingKey};
use lakko_transparency::merkle::{hash_leaf, hash_node, verify_inclusion};
use lakko_transparency::{verify_witness_quorum, LogId, SignedRoot, TreeHash};
use serde::{Deserialize, Serialize};

use crate::moderators::ModeratorList;
use crate::statement::{Action, SignedStatement, Statement};
use crate::target::{Target, TargetKind};
use crate::wire::{header, Reader};
use crate::ModerationError;

/// How many buckets a snapshot has: one per 16-bit prefix.
pub const BUCKET_COUNT: usize = 65_536;
/// The length of a bucket's audit path in the bucket tree.
pub const BUCKET_PATH_LEN: usize = 16;
/// The most entries one bucket may hold. Chosen, not measured: at a
/// uniform spread it allows about a billion active entries in all. The
/// decoder checks it before allocating.
pub const MAX_BUCKET_ENTRIES: usize = 16_384;
/// The default snapshot period: a day. Chosen, not measured.
pub const DEFAULT_SNAPSHOT_PERIOD_SECS: u32 = 86_400;
/// The snapshot root's domain separator. Frozen.
pub const SNAPSHOT_DOMAIN: &[u8] = b"lakko/moderation/snapshot/v1\0";
/// The only snapshot root version.
pub const SNAPSHOT_VERSION: u8 = 0x01;
/// The exact length of an encoded snapshot root.
pub const SNAPSHOT_ROOT_LEN: usize = SNAPSHOT_DOMAIN.len() + 1 + 8 + 4 + 8 + 8 + 32 + 8;
/// Log leaf kind of a signed statement (the moderation log's kind 1).
pub const LOG_LEAF_STATEMENT: u8 = 1;
/// Log leaf kind of a snapshot root (the moderation log's kind 3).
pub const LOG_LEAF_SNAPSHOT_ROOT: u8 = 3;
/// The largest bucket answer [`check_account`] reads. Chosen, not measured:
/// a full bucket is about 800 KiB in base64.
pub const MAX_ANSWER_BYTES: usize = 2 * 1024 * 1024;
/// The most entries of an audit path or of a co-signature list in an
/// answer. Chosen, not measured (a log of `2^64` leaves needs 64).
pub const MAX_ANSWER_LIST: usize = 64;
/// How far in the future a snapshot's build time may be, against the
/// reader's clock (ten minutes). Chosen, not measured.
pub const CLOCK_SKEW_MS: u64 = 600_000;

/// The shortest encoded entry (a channel target).
const MIN_ENTRY_LEN: usize = 1 + 16 + 8 + 8;

/// Why a snapshot answer was refused.
///
/// No message echoes input.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotError {
    /// The answer is over [`MAX_ANSWER_BYTES`] or a list is over
    /// [`MAX_ANSWER_LIST`].
    #[error("answer too large")]
    TooLarge,
    /// The answer is not the expected JSON, or a field has the wrong length.
    #[error("malformed answer: {0}")]
    Malformed(&'static str),
    /// A pinned key is not a valid key.
    #[error("bad key")]
    BadKey,
    /// The log head does not verify under the pinned operator key.
    #[error("log root signature does not verify")]
    BadRootSignature,
    /// Witness keys are pinned and the head lacks the threshold of their
    /// co-signatures.
    #[error("witness quorum not met")]
    WitnessQuorum,
    /// The snapshot leaf is not in the log at the given index.
    #[error("snapshot is not in the log")]
    SnapshotNotIncluded,
    /// The bucket is not in the snapshot at its prefix.
    #[error("bucket is not in the snapshot")]
    BucketNotIncluded,
    /// The answer's period is not the snapshot's.
    #[error("period does not match")]
    WrongPeriod,
    /// The bucket is not the account's.
    #[error("bucket prefix does not match")]
    WrongPrefix,
    /// The snapshot is too old, or dated too far in the future.
    #[error("snapshot is stale")]
    Stale,
    /// A snapshot root or a bucket does not decode.
    #[error("snapshot: {0}")]
    Decode(#[from] ModerationError),
    /// [`prove_standing`]: the bucket holds no entry for the target.
    #[error("no entry for the target")]
    NoEntry,
    /// [`prove_standing`]: the target's entry has ended at the time given.
    #[error("the entry has ended")]
    NotSuspended,
    /// [`prove_standing`]: the log entry is not the one the bucket entry
    /// names, is for another tree size than the answer's head, or the bucket
    /// entry names a leaf the snapshot did not cover.
    #[error("log entry does not match")]
    EntryMismatch,
    /// [`prove_standing`]: the log entry is not in the answer's head.
    #[error("log entry is not in the log")]
    EntryNotIncluded,
    /// [`prove_standing`]: the log leaf is not a signed statement.
    #[error("log leaf is not a statement")]
    NotAStatement,
    /// [`prove_standing`]: the moderator list does not verify under the
    /// moderation root key.
    #[error("moderator list does not verify")]
    BadModeratorList,
    /// [`prove_standing`]: the moderator was not authorised by the list when
    /// it signed, or the list is older than the statement's.
    #[error("moderator is not authorised")]
    NotAuthorised,
    /// [`prove_standing`]: the statement does not decode or its assertion
    /// fails.
    #[error("statement does not verify")]
    BadStatement,
    /// [`prove_standing`]: the statement is not a suspension (account) or a
    /// removal (channel).
    #[error("statement is another action")]
    WrongAction,
    /// [`prove_standing`]: the statement names another target.
    #[error("statement names another target")]
    WrongTarget,
    /// [`prove_standing`]: the suspension's end differs from the entry's.
    #[error("statement end does not match the entry")]
    UntilMismatch,
}

impl SnapshotError {
    /// A stable code for the FFI surfaces.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::TooLarge => "too-large",
            Self::Malformed(_) => "malformed",
            Self::BadKey => "bad-key",
            Self::BadRootSignature => "bad-root-signature",
            Self::WitnessQuorum => "witness-quorum",
            Self::SnapshotNotIncluded => "snapshot-not-included",
            Self::BucketNotIncluded => "bucket-not-included",
            Self::WrongPeriod => "wrong-period",
            Self::WrongPrefix => "wrong-prefix",
            Self::Stale => "stale",
            Self::Decode(_) => "malformed-snapshot",
            Self::NoEntry => "no-entry",
            Self::NotSuspended => "not-suspended",
            Self::EntryMismatch => "entry-mismatch",
            Self::EntryNotIncluded => "entry-not-included",
            Self::NotAStatement => "not-a-statement",
            Self::BadModeratorList => "bad-list",
            Self::NotAuthorised => "not-authorised",
            Self::BadStatement => "bad-statement",
            Self::WrongAction => "wrong-action",
            Self::WrongTarget => "wrong-target",
            Self::UntilMismatch => "until-mismatch",
        }
    }
}

// ---------------------------------------------------------------------------
// Prefixes.
// ---------------------------------------------------------------------------

/// The bucket prefix of a target: its first two bytes, big-endian.
#[must_use]
pub fn target_prefix(target: &Target) -> u16 {
    let b = target.bytes();
    // Every target kind is at least 16 bytes long.
    u16::from_be_bytes([b[0], b[1]])
}

/// The bucket prefix of an account, from its 32-byte account key.
#[must_use]
pub fn account_prefix(account_key: &[u8; 32]) -> u16 {
    let t = crate::target::account_target_hash(account_key);
    u16::from_be_bytes([t[0], t[1]])
}

/// The four lower-case hex characters of an account's bucket prefix, as the
/// query URL carries them.
#[must_use]
pub fn account_prefix_hex(account_key: &[u8; 32]) -> String {
    prefix_hex(account_prefix(account_key))
}

/// The bucket prefix of a channel: the first two bytes of its id.
#[must_use]
pub const fn channel_prefix(channel_id: &[u8; 16]) -> u16 {
    u16::from_be_bytes([channel_id[0], channel_id[1]])
}

/// The four lower-case hex characters of a channel's bucket prefix.
#[must_use]
pub fn channel_prefix_hex(channel_id: &[u8; 16]) -> String {
    prefix_hex(channel_prefix(channel_id))
}

/// Four lower-case hex characters of a prefix.
#[must_use]
pub fn prefix_hex(prefix: u16) -> String {
    format!("{prefix:04x}")
}

/// Parse exactly four hex characters (either case) as a prefix.
#[must_use]
pub fn parse_prefix(text: &str) -> Option<u16> {
    if text.len() != 4 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u16::from_str_radix(text, 16).ok()
}

// ---------------------------------------------------------------------------
// Buckets.
// ---------------------------------------------------------------------------

/// One active moderation outcome in a bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotEntry {
    /// What is suspended (an account target) or removed (a channel).
    pub target: Target,
    /// The end of the suspension (unix ms), `0` for no end; always `0` for
    /// a channel removal.
    pub until_ms: u64,
    /// The log index of the statement that made it active.
    pub log_index: u64,
}

impl SnapshotEntry {
    fn sort_key(&self) -> (&[u8], u8) {
        (self.target.bytes(), self.target.kind().as_u8())
    }
}

/// One bucket: every entry whose target starts with `prefix`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bucket {
    /// The 16-bit prefix.
    pub prefix: u16,
    /// The entries, strictly increasing by (target bytes, kind).
    pub entries: Vec<SnapshotEntry>,
}

/// The encoding of an empty bucket: `prefix ‖ 00000000`.
#[must_use]
pub fn empty_bucket_bytes(prefix: u16) -> [u8; 6] {
    let p = prefix.to_be_bytes();
    [p[0], p[1], 0, 0, 0, 0]
}

impl Bucket {
    fn check(&self) -> Result<(), ModerationError> {
        if self.entries.len() > MAX_BUCKET_ENTRIES {
            return Err(ModerationError::Invalid("bucket over its bound"));
        }
        for entry in &self.entries {
            if target_prefix(&entry.target) != self.prefix {
                return Err(ModerationError::Invalid("entry outside its bucket"));
            }
        }
        for pair in self.entries.windows(2) {
            if pair[0].sort_key() >= pair[1].sort_key() {
                return Err(ModerationError::Invalid("entries not in order"));
            }
        }
        Ok(())
    }

    /// Encode, refusing a bucket [`Self::decode`] would refuse.
    ///
    /// # Errors
    ///
    /// [`ModerationError::Invalid`].
    pub fn encode(&self) -> Result<Vec<u8>, ModerationError> {
        self.check()?;
        let count = u32::try_from(self.entries.len())
            .map_err(|_| ModerationError::Invalid("bucket over its bound"))?;
        let mut out = Vec::with_capacity(6 + self.entries.len() * (1 + 32 + 16));
        out.extend_from_slice(&self.prefix.to_be_bytes());
        out.extend_from_slice(&count.to_be_bytes());
        for e in &self.entries {
            e.target.write(&mut out);
            out.extend_from_slice(&e.until_ms.to_be_bytes());
            out.extend_from_slice(&e.log_index.to_be_bytes());
        }
        Ok(out)
    }

    /// Decode, exact and canonical.
    ///
    /// # Errors
    ///
    /// [`ModerationError::Malformed`], [`ModerationError::UnknownValue`] for
    /// an unknown target kind, [`ModerationError::Invalid`] for a bucket
    /// that is not canonical.
    pub fn decode(bytes: &[u8]) -> Result<Self, ModerationError> {
        let mut r = Reader::new(bytes);
        let prefix = r.u16()?;
        let count =
            usize::try_from(r.u32()?).map_err(|_| ModerationError::Malformed("bucket count"))?;
        if count > MAX_BUCKET_ENTRIES {
            return Err(ModerationError::Malformed("bucket count over bound"));
        }
        if count.saturating_mul(MIN_ENTRY_LEN) > r.remaining().len() {
            return Err(ModerationError::Malformed("truncated"));
        }
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            entries.push(SnapshotEntry {
                target: Target::read(&mut r)?,
                until_ms: r.u64()?,
                log_index: r.u64()?,
            });
        }
        r.finish()?;
        let bucket = Self { prefix, entries };
        bucket.check()?;
        Ok(bucket)
    }

    /// The entry for `target`, if the bucket holds one.
    #[must_use]
    pub fn lookup(&self, target: &Target) -> Option<&SnapshotEntry> {
        let key = (target.bytes(), target.kind().as_u8());
        self.entries
            .binary_search_by(|e| e.sort_key().cmp(&key))
            .ok()
            .and_then(|i| self.entries.get(i))
    }
}

// ---------------------------------------------------------------------------
// The snapshot root.
// ---------------------------------------------------------------------------

/// The snapshot root: the payload of a log leaf of kind
/// [`LOG_LEAF_SNAPSHOT_ROOT`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRoot {
    /// `floor(built_at_ms / 1000 / period_secs)`.
    pub period: u64,
    /// The period length in seconds; never zero.
    pub period_secs: u32,
    /// When it was built (unix ms).
    pub built_at_ms: u64,
    /// How many log leaves it was built from.
    pub covered_log_size: u64,
    /// The root of the bucket tree.
    pub bucket_root: TreeHash,
    /// How many entries all buckets hold together.
    pub entry_count: u64,
}

/// The period of `built_at_ms` for a period of `period_secs` seconds.
#[must_use]
pub const fn period_of(built_at_ms: u64, period_secs: u32) -> u64 {
    if period_secs == 0 {
        return 0;
    }
    built_at_ms / 1000 / period_secs as u64
}

impl SnapshotRoot {
    /// Encode.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(SNAPSHOT_ROOT_LEN);
        out.extend_from_slice(SNAPSHOT_DOMAIN);
        out.push(SNAPSHOT_VERSION);
        out.extend_from_slice(&self.period.to_be_bytes());
        out.extend_from_slice(&self.period_secs.to_be_bytes());
        out.extend_from_slice(&self.built_at_ms.to_be_bytes());
        out.extend_from_slice(&self.covered_log_size.to_be_bytes());
        out.extend_from_slice(&self.bucket_root);
        out.extend_from_slice(&self.entry_count.to_be_bytes());
        out
    }

    /// The log leaf: `0x03 ‖ root`.
    #[must_use]
    pub fn log_leaf(&self) -> Vec<u8> {
        let mut leaf = Vec::with_capacity(1 + SNAPSHOT_ROOT_LEN);
        leaf.push(LOG_LEAF_SNAPSHOT_ROOT);
        leaf.extend_from_slice(&self.encode());
        leaf
    }

    /// Decode, exact. Refuses a zero period length and a period that does
    /// not follow from `built_at_ms`.
    ///
    /// # Errors
    ///
    /// Any decode variant, or [`ModerationError::Invalid`].
    pub fn decode(bytes: &[u8]) -> Result<Self, ModerationError> {
        if bytes.len() != SNAPSHOT_ROOT_LEN {
            return Err(ModerationError::Malformed("snapshot root length"));
        }
        let mut r = header(bytes, SNAPSHOT_DOMAIN, SNAPSHOT_VERSION)?;
        let root = Self {
            period: r.u64()?,
            period_secs: r.u32()?,
            built_at_ms: r.u64()?,
            covered_log_size: r.u64()?,
            bucket_root: r.array()?,
            entry_count: r.u64()?,
        };
        r.finish()?;
        if root.period_secs == 0 {
            return Err(ModerationError::Invalid("period length"));
        }
        if root.period != period_of(root.built_at_ms, root.period_secs) {
            return Err(ModerationError::Invalid("period"));
        }
        Ok(root)
    }

    /// Decode a log leaf `0x03 ‖ root`.
    ///
    /// # Errors
    ///
    /// [`ModerationError::Malformed`] for another leaf kind; any error of
    /// [`Self::decode`].
    pub fn decode_log_leaf(leaf: &[u8]) -> Result<Self, ModerationError> {
        match leaf.split_first() {
            Some((&LOG_LEAF_SNAPSHOT_ROOT, rest)) => Self::decode(rest),
            _ => Err(ModerationError::Malformed("not a snapshot leaf")),
        }
    }
}

// ---------------------------------------------------------------------------
// Building.
// ---------------------------------------------------------------------------

/// A built snapshot: the root, the non-empty buckets and the bucket tree.
#[derive(Clone)]
pub struct Snapshot {
    root: SnapshotRoot,
    buckets: BTreeMap<u16, Vec<u8>>,
    /// `levels[0]` holds the [`BUCKET_COUNT`] leaf hashes, `levels[16]` the
    /// root. About 4 MiB.
    levels: Vec<Vec<TreeHash>>,
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshot")
            .field("root", &self.root)
            .field("non_empty_buckets", &self.buckets.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy)]
struct Decision {
    issued_at_ms: u64,
    log_index: u64,
    suspend: bool,
    until_ms: u64,
}

impl Snapshot {
    /// The root.
    #[must_use]
    pub const fn root(&self) -> &SnapshotRoot {
        &self.root
    }

    /// The bytes of the bucket at `prefix`.
    #[must_use]
    pub fn bucket(&self, prefix: u16) -> Vec<u8> {
        self.buckets
            .get(&prefix)
            .cloned()
            .unwrap_or_else(|| empty_bucket_bytes(prefix).to_vec())
    }

    /// The audit path of the bucket at `prefix` in the bucket tree, leaf-most
    /// sibling first ([`BUCKET_PATH_LEN`] hashes).
    #[must_use]
    pub fn bucket_path(&self, prefix: u16) -> Vec<TreeHash> {
        let mut path = Vec::with_capacity(BUCKET_PATH_LEN);
        let mut index = usize::from(prefix);
        for level in self.levels.iter().take(BUCKET_PATH_LEN) {
            if let Some(sibling) = level.get(index ^ 1) {
                path.push(*sibling);
            }
            index >>= 1;
        }
        path
    }
}

/// Per (target bytes, kind), the decisive account or channel statement of
/// `leaves`: the greatest `(issued_at_ms, log index)`. The map's key order
/// is the bucket order.
fn decisive_statements(leaves: &[&[u8]]) -> BTreeMap<(Vec<u8>, u8), Decision> {
    let mut decisive: BTreeMap<(Vec<u8>, u8), Decision> = BTreeMap::new();
    for (index, leaf) in leaves.iter().enumerate() {
        let Some((&LOG_LEAF_STATEMENT, rest)) = leaf.split_first() else {
            continue;
        };
        let Ok(signed) = SignedStatement::decode(rest) else {
            continue;
        };
        let Ok(statement) = Statement::decode(&signed.statement) else {
            continue;
        };
        // `suspend` is "active": a suspension, or a channel removal.
        let suspend = match statement.action {
            Action::Suspend | Action::ChannelRemove => true,
            Action::Lift | Action::ChannelRestore => false,
            _ => continue,
        };
        if !matches!(
            statement.target.kind(),
            TargetKind::Account | TargetKind::Channel
        ) {
            continue;
        }
        let candidate = Decision {
            issued_at_ms: statement.issued_at_ms,
            log_index: index as u64,
            suspend,
            until_ms: statement.until_ms,
        };
        decisive
            .entry((
                statement.target.bytes().to_vec(),
                statement.target.kind().as_u8(),
            ))
            .and_modify(|held| {
                if (candidate.issued_at_ms, candidate.log_index)
                    > (held.issued_at_ms, held.log_index)
                {
                    *held = candidate;
                }
            })
            .or_insert(candidate);
    }
    decisive
}

/// Build the snapshot of `leaves` (moderation log leaves, `kind ‖ bytes`, in
/// log order) as of `built_at_ms`.
///
/// Only kind-1 leaves count, and of those only a suspension or a lift of an
/// account and a removal or a restore of a channel (a delist or a handle
/// block is not a removal and does not count). A leaf that does not decode
/// is skipped; signatures are not checked again (the log accepts only
/// verified statements). Per target the decisive statement is the one with
/// the greatest `issued_at_ms`, a tie broken by the greater log index, which
/// is the order the applying services apply them in. An account is in the
/// snapshot when its decisive statement is a suspension with no end or an
/// end after `built_at_ms`; a channel when its decisive statement is a
/// removal.
///
/// # Errors
///
/// [`ModerationError::Invalid`] for a zero `period_secs` or a bucket over
/// [`MAX_BUCKET_ENTRIES`].
pub fn build_snapshot(
    leaves: &[&[u8]],
    built_at_ms: u64,
    period_secs: u32,
) -> Result<Snapshot, ModerationError> {
    if period_secs == 0 {
        return Err(ModerationError::Invalid("period length"));
    }
    let decisive = decisive_statements(leaves);

    // The map's (bytes, kind) order is the bucket order.
    let mut grouped: BTreeMap<u16, Vec<SnapshotEntry>> = BTreeMap::new();
    let mut entry_count: u64 = 0;
    for ((bytes, kind), d) in decisive {
        if !d.suspend || (d.until_ms != 0 && d.until_ms <= built_at_ms) {
            continue;
        }
        let target = Target::new(TargetKind::from_u8(kind)?, &bytes)?;
        grouped
            .entry(target_prefix(&target))
            .or_default()
            .push(SnapshotEntry {
                target,
                until_ms: d.until_ms,
                log_index: d.log_index,
            });
        entry_count += 1;
    }
    let mut buckets = BTreeMap::new();
    for (prefix, entries) in grouped {
        buckets.insert(prefix, Bucket { prefix, entries }.encode()?);
    }

    let mut leaf_hashes = Vec::with_capacity(BUCKET_COUNT);
    for prefix in 0..=u16::MAX {
        leaf_hashes.push(
            buckets
                .get(&prefix)
                .map_or_else(|| hash_leaf(&empty_bucket_bytes(prefix)), |b| hash_leaf(b)),
        );
    }
    let mut levels = vec![leaf_hashes];
    while let Some(below) = levels.last() {
        if below.len() <= 1 {
            break;
        }
        let above: Vec<TreeHash> = below
            .chunks_exact(2)
            .map(|pair| hash_node(&pair[0], &pair[1]))
            .collect();
        levels.push(above);
    }
    let bucket_root = levels
        .last()
        .and_then(|top| top.first())
        .copied()
        .ok_or(ModerationError::Invalid("empty tree"))?;

    Ok(Snapshot {
        root: SnapshotRoot {
            period: period_of(built_at_ms, period_secs),
            period_secs,
            built_at_ms,
            covered_log_size: leaves.len() as u64,
            bucket_root,
            entry_count,
        },
        buckets,
        levels,
    })
}

// ---------------------------------------------------------------------------
// The answer and the client check.
// ---------------------------------------------------------------------------

/// The signed, witnessed log head in an answer (the `/moderation/log/root`
/// shape). Base64 is standard, padded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerRoot {
    /// The tree root.
    pub tree_root_b64: String,
    /// The number of leaves.
    pub log_size: u64,
    /// The operator's timestamp (seconds).
    pub timestamp_unix: u64,
    /// The operator's signature.
    pub signature_b64: String,
    /// Witness co-signatures under the `moderation` log.
    pub witness_cosigs_b64: Vec<String>,
}

/// The answer of `GET /moderation/snapshot/{period}/bucket/{prefix}`.
/// Base64 is standard, padded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BucketAnswer {
    /// The snapshot's period.
    pub period: u64,
    /// Four lower-case hex characters.
    pub prefix: String,
    /// The bucket's bytes.
    pub bucket_b64: String,
    /// The bucket's path in the bucket tree ([`BUCKET_PATH_LEN`] hashes).
    pub bucket_path_b64: Vec<String>,
    /// The log index of the snapshot leaf.
    pub snapshot_index: u64,
    /// The snapshot leaf, `0x03 ‖ root`.
    pub snapshot_leaf_b64: String,
    /// The snapshot leaf's path in the tree of `root.log_size` leaves.
    pub snapshot_path_b64: Vec<String>,
    /// The log head the path is for.
    pub root: AnswerRoot,
}

impl BucketAnswer {
    /// Compose the answer for the bucket at `prefix` of `snapshot`, whose
    /// leaf is at `snapshot_index` with `snapshot_path` in the tree of
    /// `head`, co-signed by `cosigs`.
    #[must_use]
    pub fn new(
        snapshot: &Snapshot,
        prefix: u16,
        snapshot_index: u64,
        snapshot_path: &[TreeHash],
        head: &SignedRoot,
        cosigs: &[Vec<u8>],
    ) -> Self {
        Self {
            period: snapshot.root.period,
            prefix: prefix_hex(prefix),
            bucket_b64: B64.encode(snapshot.bucket(prefix)),
            bucket_path_b64: snapshot
                .bucket_path(prefix)
                .iter()
                .map(|h| B64.encode(h))
                .collect(),
            snapshot_index,
            snapshot_leaf_b64: B64.encode(snapshot.root.log_leaf()),
            snapshot_path_b64: snapshot_path.iter().map(|h| B64.encode(h)).collect(),
            root: AnswerRoot {
                tree_root_b64: B64.encode(head.tree_root),
                log_size: head.log_size,
                timestamp_unix: head.timestamp_unix,
                signature_b64: B64.encode(head.signature.to_bytes()),
                witness_cosigs_b64: cosigs.iter().map(|c| B64.encode(c)).collect(),
            },
        }
    }

    /// The JSON body of the route.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::Malformed`] if serialisation fails.
    pub fn to_json(&self) -> Result<Vec<u8>, SnapshotError> {
        serde_json::to_vec(self).map_err(|_| SnapshotError::Malformed("json"))
    }
}

/// The answer of `GET /moderation/log/entry/{index}?size={log_size}`: one
/// log leaf with its inclusion path in the tree of `log_size` leaves. Base64
/// is standard, padded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntryAnswer {
    /// The leaf's index.
    pub index: u64,
    /// The leaf, `kind ‖ bytes`.
    pub leaf_b64: String,
    /// The leaf's path, leaf-most sibling first.
    pub audit_path_b64: Vec<String>,
    /// The size of the tree the path is for.
    pub log_size: u64,
}

/// Whether an account is suspended, or a channel removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// Not in the snapshot, or its suspension has ended.
    NotSuspended,
    /// Suspended (an account) or removed (a channel); `until_ms` is the end,
    /// `None` for no end (always `None` for a channel).
    Suspended {
        /// The end (unix ms).
        until_ms: Option<u64>,
    },
}

/// The result of [`check_target`] and [`check_account`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetStanding {
    /// The target's standing.
    pub standing: Standing,
    /// The snapshot's period.
    pub period: u64,
    /// When the snapshot was built (unix ms).
    pub built_at_ms: u64,
    /// Until when a client may reuse this answer for the same prefix:
    /// `built_at_ms + period_secs * 1000`.
    pub reuse_until_ms: u64,
    /// The log index of the statement behind the target's entry, when the
    /// bucket holds one (also for an entry that has ended): what
    /// [`prove_standing`] fetches.
    pub log_index: Option<u64>,
    /// The entry's end as the snapshot holds it (`Some(0)` for no end), when
    /// the bucket holds an entry.
    pub until_ms: Option<u64>,
}

/// The result of [`check_account`]: the same as [`TargetStanding`].
pub type AccountStanding = TargetStanding;

fn b64(text: &str, what: &'static str) -> Result<Vec<u8>, SnapshotError> {
    B64.decode(text).map_err(|_| SnapshotError::Malformed(what))
}

fn b64_hash(text: &str, what: &'static str) -> Result<TreeHash, SnapshotError> {
    b64(text, what)?
        .try_into()
        .map_err(|_| SnapshotError::Malformed(what))
}

fn b64_path(items: &[String], what: &'static str) -> Result<Vec<TreeHash>, SnapshotError> {
    items.iter().map(|s| b64_hash(s, what)).collect()
}

/// Check a bucket answer and read an account's standing from it:
/// [`check_target`] for the account target of `account_key`.
///
/// # Errors
///
/// Every [`SnapshotError`] variant; see each for its meaning.
pub fn check_account(
    answer_json: &[u8],
    account_key: &[u8; 32],
    log_operator_key: &[u8; 32],
    witness_keys: &[&[u8]],
    witness_threshold: usize,
    now_ms: i64,
) -> Result<AccountStanding, SnapshotError> {
    let target = Target::account(account_key)?;
    check_target(
        answer_json,
        target.kind(),
        target.bytes(),
        log_operator_key,
        witness_keys,
        witness_threshold,
        now_ms,
    )
}

/// Check a bucket answer and read a target's standing from it: an account
/// target (32 bytes, the account-target hash) or a channel (its 16-byte id).
///
/// In order: the answer's size and shape; the log head under the pinned
/// operator key; when `witness_keys` is not empty, a quorum of
/// `witness_threshold` of them under the `moderation` log (empty keys is the
/// operator-only posture); the snapshot leaf's inclusion in the head; the
/// snapshot root and its period; the bucket's inclusion at its prefix, which
/// must be the target's; freshness (`now_ms` before `built_at_ms + 2 ×
/// period`, and `built_at_ms` at most [`CLOCK_SKEW_MS`] after `now_ms`);
/// then the lookup. An entry whose end has passed reads as not suspended.
///
/// # Errors
///
/// Every [`SnapshotError`] variant; see each for its meaning. A target of
/// the wrong length for `kind` is [`SnapshotError::Decode`].
pub fn check_target(
    answer_json: &[u8],
    kind: TargetKind,
    target: &[u8],
    log_operator_key: &[u8; 32],
    witness_keys: &[&[u8]],
    witness_threshold: usize,
    now_ms: i64,
) -> Result<TargetStanding, SnapshotError> {
    let target = Target::new(kind, target)?;
    let answer = parse_answer(answer_json)?;
    verify_head(&answer, log_operator_key, witness_keys, witness_threshold)?;
    let located = locate(&answer, &target)?;
    let root = &located.root;

    // Freshness.
    let now = u64::try_from(now_ms).map_err(|_| SnapshotError::Stale)?;
    let period_ms = u64::from(root.period_secs).saturating_mul(1000);
    if now >= root.built_at_ms.saturating_add(period_ms.saturating_mul(2))
        || root.built_at_ms > now.saturating_add(CLOCK_SKEW_MS)
    {
        return Err(SnapshotError::Stale);
    }

    let entry = located.bucket.lookup(&target);
    let standing = match entry {
        Some(e) if e.until_ms == 0 => Standing::Suspended { until_ms: None },
        Some(e) if e.until_ms > now => Standing::Suspended {
            until_ms: Some(e.until_ms),
        },
        _ => Standing::NotSuspended,
    };
    Ok(TargetStanding {
        standing,
        period: root.period,
        built_at_ms: root.built_at_ms,
        reuse_until_ms: root.built_at_ms.saturating_add(period_ms),
        log_index: entry.map(|e| e.log_index),
        until_ms: entry.map(|e| e.until_ms),
    })
}

/// The answer's size and shape.
fn parse_answer(answer_json: &[u8]) -> Result<BucketAnswer, SnapshotError> {
    if answer_json.len() > MAX_ANSWER_BYTES {
        return Err(SnapshotError::TooLarge);
    }
    let answer: BucketAnswer =
        serde_json::from_slice(answer_json).map_err(|_| SnapshotError::Malformed("json"))?;
    if answer.bucket_path_b64.len() > MAX_ANSWER_LIST
        || answer.snapshot_path_b64.len() > MAX_ANSWER_LIST
        || answer.root.witness_cosigs_b64.len() > MAX_ANSWER_LIST
    {
        return Err(SnapshotError::TooLarge);
    }
    Ok(answer)
}

/// The log head under the pinned operator key and, when `witness_keys` is
/// not empty, a quorum of them.
fn verify_head(
    answer: &BucketAnswer,
    log_operator_key: &[u8; 32],
    witness_keys: &[&[u8]],
    witness_threshold: usize,
) -> Result<(), SnapshotError> {
    let operator = VerifyingKey::from_bytes(log_operator_key).map_err(|_| SnapshotError::BadKey)?;
    let tree_root = b64_hash(&answer.root.tree_root_b64, "tree root")?;
    let signature = Signature::from_bytes(&b64(&answer.root.signature_b64, "signature")?)
        .map_err(|_| SnapshotError::Malformed("signature"))?;
    let head = SignedRoot {
        tree_root,
        log_size: answer.root.log_size,
        timestamp_unix: answer.root.timestamp_unix,
        signature,
    };
    head.verify(&operator)
        .map_err(|_| SnapshotError::BadRootSignature)?;
    if !witness_keys.is_empty() {
        let cosigs = answer
            .root
            .witness_cosigs_b64
            .iter()
            .map(|c| b64(c, "co-signature"))
            .collect::<Result<Vec<_>, _>>()?;
        let refs: Vec<&[u8]> = cosigs.iter().map(Vec::as_slice).collect();
        verify_witness_quorum(
            &refs,
            witness_keys,
            witness_threshold,
            LogId::Moderation,
            &tree_root,
            head.log_size,
        )
        .map_err(|_| SnapshotError::WitnessQuorum)?;
    }
    Ok(())
}

/// The snapshot and the bucket of an answer, found under its head.
struct Located {
    tree_root: TreeHash,
    root: SnapshotRoot,
    bucket: Bucket,
}

/// The snapshot leaf's inclusion under the answer's head, the snapshot
/// root and its period, and the bucket's inclusion at its prefix, which
/// must be the target's. Does not check the head's signature.
fn locate(answer: &BucketAnswer, target: &Target) -> Result<Located, SnapshotError> {
    let tree_root = b64_hash(&answer.root.tree_root_b64, "tree root")?;
    let leaf = b64(&answer.snapshot_leaf_b64, "snapshot leaf")?;
    let log_path = b64_path(&answer.snapshot_path_b64, "snapshot path")?;
    if !verify_inclusion(
        &leaf,
        answer.snapshot_index,
        answer.root.log_size,
        &log_path,
        &tree_root,
    ) {
        return Err(SnapshotError::SnapshotNotIncluded);
    }
    let root = SnapshotRoot::decode_log_leaf(&leaf)?;
    if root.period != answer.period {
        return Err(SnapshotError::WrongPeriod);
    }

    let prefix = parse_prefix(&answer.prefix).ok_or(SnapshotError::Malformed("prefix"))?;
    let bucket_bytes = b64(&answer.bucket_b64, "bucket")?;
    let bucket_path = b64_path(&answer.bucket_path_b64, "bucket path")?;
    if !verify_inclusion(
        &bucket_bytes,
        u64::from(prefix),
        BUCKET_COUNT as u64,
        &bucket_path,
        &root.bucket_root,
    ) {
        return Err(SnapshotError::BucketNotIncluded);
    }
    let bucket = Bucket::decode(&bucket_bytes)?;
    if bucket.prefix != prefix || prefix != target_prefix(target) {
        return Err(SnapshotError::WrongPrefix);
    }
    Ok(Located {
        tree_root,
        root,
        bucket,
    })
}

/// Prove the mark a bucket answer gives a target, from the moderator's
/// signed statement behind it.
///
/// `answer_json` is a bucket answer the caller has already checked with
/// [`check_target`] (or [`check_account`]) against the pinned operator and
/// witness keys; its head is not verified again here. `entry_json` is the
/// answer of `GET /moderation/log/entry/{log_index}?size={log_size}` for the
/// standing's [`TargetStanding::log_index`] and the answer head's size
/// ([`LogEntryAnswer`]). `moderator_list` is the root-signed list (`GET
/// /moderation/moderators`), verified here under `moderation_root_key`.
///
/// In order: the target, the answer's shape, the snapshot and the bucket
/// under the answer's head ([`SnapshotError::SnapshotNotIncluded`],
/// [`SnapshotError::BucketNotIncluded`], [`SnapshotError::WrongPrefix`]);
/// the target's entry ([`SnapshotError::NoEntry`]) still active at `now_ms`
/// ([`SnapshotError::NotSuspended`]); the log entry's size and index are the
/// head's and the bucket entry's, and the bucket entry names a leaf the
/// snapshot covered ([`SnapshotError::EntryMismatch`]); the leaf's inclusion
/// under the head ([`SnapshotError::EntryNotIncluded`]); the leaf is a
/// signed statement ([`SnapshotError::NotAStatement`]); the list verifies
/// ([`SnapshotError::BadModeratorList`]); the statement verifies under it
/// ([`SnapshotError::NotAuthorised`], [`SnapshotError::BadStatement`]); it
/// is a suspension for an account, a removal for a channel
/// ([`SnapshotError::WrongAction`]); it names the target
/// ([`SnapshotError::WrongTarget`]); a suspension's end is the entry's
/// ([`SnapshotError::UntilMismatch`]).
///
/// # Errors
///
/// Every [`SnapshotError`] variant listed above, and the shape errors of
/// [`check_target`].
#[allow(clippy::too_many_arguments)]
pub fn prove_standing(
    answer_json: &[u8],
    entry_json: &[u8],
    moderator_list: &[u8],
    moderation_root_key: &[u8; 32],
    kind: TargetKind,
    target: &[u8],
    now_ms: i64,
) -> Result<(), SnapshotError> {
    let target = Target::new(kind, target)?;
    let expected_action = match kind {
        TargetKind::Account => Action::Suspend,
        TargetKind::Channel => Action::ChannelRemove,
        _ => return Err(SnapshotError::WrongAction),
    };
    let answer = parse_answer(answer_json)?;
    let located = locate(&answer, &target)?;
    let entry = located
        .bucket
        .lookup(&target)
        .ok_or(SnapshotError::NoEntry)?;
    let now = u64::try_from(now_ms).map_err(|_| SnapshotError::Stale)?;
    if entry.until_ms != 0 && entry.until_ms <= now {
        return Err(SnapshotError::NotSuspended);
    }

    if entry_json.len() > MAX_ANSWER_BYTES {
        return Err(SnapshotError::TooLarge);
    }
    let log_entry: LogEntryAnswer =
        serde_json::from_slice(entry_json).map_err(|_| SnapshotError::Malformed("entry json"))?;
    if log_entry.audit_path_b64.len() > MAX_ANSWER_LIST {
        return Err(SnapshotError::TooLarge);
    }
    if log_entry.log_size != answer.root.log_size
        || log_entry.index != entry.log_index
        || entry.log_index >= located.root.covered_log_size
    {
        return Err(SnapshotError::EntryMismatch);
    }
    let leaf = b64(&log_entry.leaf_b64, "entry leaf")?;
    let path = b64_path(&log_entry.audit_path_b64, "entry path")?;
    if !verify_inclusion(
        &leaf,
        log_entry.index,
        log_entry.log_size,
        &path,
        &located.tree_root,
    ) {
        return Err(SnapshotError::EntryNotIncluded);
    }
    let signed = match leaf.split_first() {
        Some((&LOG_LEAF_STATEMENT, rest)) => {
            SignedStatement::decode(rest).map_err(|_| SnapshotError::NotAStatement)?
        },
        _ => return Err(SnapshotError::NotAStatement),
    };
    let list = ModeratorList::verify(moderator_list, moderation_root_key)
        .map_err(|_| SnapshotError::BadModeratorList)?;
    let statement = signed.verify(&list).map_err(|e| match e {
        ModerationError::NotAuthorised => SnapshotError::NotAuthorised,
        _ => SnapshotError::BadStatement,
    })?;
    if statement.action != expected_action {
        return Err(SnapshotError::WrongAction);
    }
    if statement.target != target {
        return Err(SnapshotError::WrongTarget);
    }
    if expected_action == Action::Suspend && statement.until_ms != entry.until_ms {
        return Err(SnapshotError::UntilMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::reason::Reason;
    use crate::statement::reason_commitment;
    use crate::target::account_target_hash;
    use crate::webauthn::Assertion;
    use lakko_crypto::signing::SigningKey;
    use lakko_transparency::merkle::{audit_path, root as merkle_root, root_from_hashes};
    use lakko_transparency::sign_witness_cosig;
    use proptest::prelude::*;
    use std::sync::OnceLock;

    const OPERATOR_SEED: [u8; 32] = [11; 32];
    const WITNESS_SEED: [u8; 32] = [12; 32];
    const PERIOD: u32 = 86_400;
    const BUILT_AT: u64 = 1_800_000_000_000;

    fn op_public() -> [u8; 32] {
        SigningKey::from_seed(&OPERATOR_SEED)
            .verifying_key()
            .to_bytes()
    }

    fn statement_leaf(
        id: u16,
        action: Action,
        account: &[u8; 32],
        issued_at_ms: u64,
        until_ms: u64,
    ) -> Vec<u8> {
        target_leaf(
            id,
            action,
            Target::account(account).unwrap(),
            issued_at_ms,
            until_ms,
        )
    }

    fn channel_leaf(id: u16, action: Action, channel: &[u8; 16], issued_at_ms: u64) -> Vec<u8> {
        target_leaf(
            id,
            action,
            Target::new(TargetKind::Channel, channel).unwrap(),
            issued_at_ms,
            0,
        )
    }

    fn target_leaf(
        id: u16,
        action: Action,
        target: Target,
        issued_at_ms: u64,
        until_ms: u64,
    ) -> Vec<u8> {
        let mut statement_id = [0u8; 16];
        statement_id[..2].copy_from_slice(&id.to_be_bytes());
        let s = Statement {
            statement_id,
            action,
            target,
            reason_commitment: reason_commitment(Reason::Spam, &[1; 16]),
            until_ms,
            issued_at_ms,
            moderator_id: [2; 16],
            list_version: 1,
        };
        let signed = SignedStatement {
            statement: s.encode_checked().unwrap(),
            assertion: Assertion {
                authenticator_data: vec![1],
                client_data_json: vec![2],
                signature: vec![3],
            },
        };
        let mut leaf = vec![LOG_LEAF_STATEMENT];
        leaf.extend_from_slice(&signed.encode().unwrap());
        leaf
    }

    struct Served {
        snapshot: Snapshot,
        index: u64,
        log: Vec<Vec<u8>>,
    }

    /// Build a snapshot of `statements`, append its leaf after them (and
    /// after `extra` more leaves) and return everything needed to serve it.
    fn serve(statements: &[Vec<u8>], built_at_ms: u64, extra: usize) -> Served {
        let refs: Vec<&[u8]> = statements.iter().map(Vec::as_slice).collect();
        let snapshot = build_snapshot(&refs, built_at_ms, PERIOD).unwrap();
        let mut log = statements.to_vec();
        log.push(snapshot.root().log_leaf());
        for i in 0..extra {
            log.push(vec![2, u8::try_from(i % 256).unwrap()]);
        }
        Served {
            snapshot,
            index: statements.len() as u64,
            log,
        }
    }

    fn answer(served: &Served, prefix: u16, witnessed: bool) -> Vec<u8> {
        let op = SigningKey::from_seed(&OPERATOR_SEED);
        let tree_root = merkle_root(&served.log);
        let size = served.log.len() as u64;
        let head = SignedRoot {
            tree_root,
            log_size: size,
            timestamp_unix: 7,
            signature: op.sign(&SignedRoot::signing_payload(&tree_root, size, 7)),
        };
        let cosigs = if witnessed {
            vec![sign_witness_cosig(
                &SigningKey::from_seed(&WITNESS_SEED),
                LogId::Moderation,
                &tree_root,
                size,
            )
            .to_vec()]
        } else {
            Vec::new()
        };
        let path = audit_path(&served.log, usize::try_from(served.index).unwrap());
        serde_json::to_vec(&BucketAnswer::new(
            &served.snapshot,
            prefix,
            served.index,
            &path,
            &head,
            &cosigs,
        ))
        .unwrap()
    }

    fn check(
        answer: &[u8],
        account: &[u8; 32],
        now_ms: i64,
    ) -> Result<AccountStanding, SnapshotError> {
        check_account(answer, account, &op_public(), &[], 1, now_ms)
    }

    fn now() -> i64 {
        i64::try_from(BUILT_AT).unwrap() + 1000
    }

    #[test]
    fn the_bucket_tree_is_the_rfc6962_tree() {
        let account = [5u8; 32];
        let served = serve(
            &[statement_leaf(1, Action::Suspend, &account, 10, 0)],
            BUILT_AT,
            0,
        );
        let hashes: Vec<TreeHash> = (0..=u16::MAX)
            .map(|p| hash_leaf(&served.snapshot.bucket(p)))
            .collect();
        assert_eq!(
            root_from_hashes(&hashes),
            served.snapshot.root().bucket_root
        );
        for p in [0u16, 1, 0x1234, account_prefix(&account), u16::MAX] {
            let path = served.snapshot.bucket_path(p);
            assert_eq!(path.len(), BUCKET_PATH_LEN);
            assert!(verify_inclusion(
                &served.snapshot.bucket(p),
                u64::from(p),
                BUCKET_COUNT as u64,
                &path,
                &served.snapshot.root().bucket_root,
            ));
        }
        assert_eq!(served.snapshot.root().entry_count, 1);
        assert_eq!(served.snapshot.root().covered_log_size, 1);
    }

    #[test]
    fn a_suspended_account_reads_suspended_and_others_do_not() {
        let a = [5u8; 32];
        let b = [6u8; 32];
        let served = serve(
            &[
                statement_leaf(1, Action::Suspend, &a, 10, 0),
                statement_leaf(2, Action::Suspend, &b, 10, BUILT_AT + 50_000),
            ],
            BUILT_AT,
            3,
        );
        let got = check(&answer(&served, account_prefix(&a), false), &a, now()).unwrap();
        assert_eq!(got.standing, Standing::Suspended { until_ms: None });
        assert_eq!(got.period, period_of(BUILT_AT, PERIOD));
        assert_eq!(got.reuse_until_ms, BUILT_AT + u64::from(PERIOD) * 1000);
        let got = check(&answer(&served, account_prefix(&b), false), &b, now()).unwrap();
        assert_eq!(
            got.standing,
            Standing::Suspended {
                until_ms: Some(BUILT_AT + 50_000)
            }
        );
        // After its end, the entry reads as not suspended.
        let later = i64::try_from(BUILT_AT + 60_000).unwrap();
        let got = check(&answer(&served, account_prefix(&b), false), &b, later).unwrap();
        assert_eq!(got.standing, Standing::NotSuspended);
        let c = [9u8; 32];
        let got = check(&answer(&served, account_prefix(&c), false), &c, now()).unwrap();
        assert_eq!(got.standing, Standing::NotSuspended);
    }

    #[test]
    fn the_latest_issued_decision_wins_ties_by_log_index() {
        let a = [5u8; 32];
        // Suspend, then a later lift: not suspended.
        let served = serve(
            &[
                statement_leaf(1, Action::Suspend, &a, 10, 0),
                statement_leaf(2, Action::Lift, &a, 20, 0),
            ],
            BUILT_AT,
            0,
        );
        assert_eq!(served.snapshot.root().entry_count, 0);
        // A lift logged after a suspension issued later loses.
        let served = serve(
            &[
                statement_leaf(1, Action::Suspend, &a, 30, 0),
                statement_leaf(2, Action::Lift, &a, 20, 0),
            ],
            BUILT_AT,
            0,
        );
        assert_eq!(served.snapshot.root().entry_count, 1);
        // Same issue time: the later log index wins.
        let served = serve(
            &[
                statement_leaf(1, Action::Lift, &a, 30, 0),
                statement_leaf(2, Action::Suspend, &a, 30, 0),
            ],
            BUILT_AT,
            0,
        );
        let b = Bucket::decode(&served.snapshot.bucket(account_prefix(&a))).unwrap();
        assert_eq!(b.entries.len(), 1);
        assert_eq!(b.entries[0].log_index, 1);
        // A suspension that ended before the build is not in the snapshot.
        let served = serve(
            &[statement_leaf(1, Action::Suspend, &a, 30, BUILT_AT)],
            BUILT_AT,
            0,
        );
        assert_eq!(served.snapshot.root().entry_count, 0);
    }

    #[test]
    fn other_leaves_are_ignored() {
        let a = [5u8; 32];
        let served = serve(
            &[
                vec![2, 1, 2, 3],
                vec![LOG_LEAF_STATEMENT, 9, 9],
                vec![],
                statement_leaf(1, Action::HideFromSearch, &a, 10, 0),
                statement_leaf(2, Action::KeepSuspension, &a, 11, 0),
            ],
            BUILT_AT,
            0,
        );
        assert_eq!(served.snapshot.root().entry_count, 0);
        assert_eq!(served.snapshot.root().covered_log_size, 5);
        // A keep after a suspension changes nothing: the suspension stands.
        let served = serve(
            &[
                statement_leaf(1, Action::Suspend, &a, 10, 0),
                statement_leaf(2, Action::KeepSuspension, &a, 20, 0),
            ],
            BUILT_AT,
            0,
        );
        let bucket = Bucket::decode(&served.snapshot.bucket(account_prefix(&a))).unwrap();
        assert_eq!(bucket.entries.len(), 1);
        assert_eq!(bucket.entries[0].log_index, 0);
    }

    fn check_channel(
        answer: &[u8],
        channel: &[u8; 16],
        now_ms: i64,
    ) -> Result<TargetStanding, SnapshotError> {
        check_target(
            answer,
            TargetKind::Channel,
            channel,
            &op_public(),
            &[],
            1,
            now_ms,
        )
    }

    #[test]
    fn a_removed_channel_reads_removed_and_a_restore_or_delist_does_not() {
        let removed = [0x21u8; 16];
        let restored = [0x22u8; 16];
        let delisted = [0x23u8; 16];
        let served = serve(
            &[
                channel_leaf(1, Action::ChannelRemove, &removed, 10),
                channel_leaf(2, Action::ChannelRemove, &restored, 10),
                channel_leaf(3, Action::ChannelRestore, &restored, 20),
                channel_leaf(4, Action::ChannelDelist, &delisted, 10),
                channel_leaf(5, Action::ChannelBlockHandle, &delisted, 11),
            ],
            BUILT_AT,
            1,
        );
        assert_eq!(served.snapshot.root().entry_count, 1);
        let ans = answer(&served, channel_prefix(&removed), true);
        let got = check_channel(&ans, &removed, now()).unwrap();
        assert_eq!(got.standing, Standing::Suspended { until_ms: None });
        // Long after: a removal has no end.
        let later = i64::try_from(BUILT_AT + 100_000_000).unwrap();
        let got = check_channel(&ans, &removed, later).unwrap();
        assert_eq!(got.standing, Standing::Suspended { until_ms: None });
        for c in [restored, delisted] {
            let ans = answer(&served, channel_prefix(&c), false);
            assert_eq!(
                check_channel(&ans, &c, now()).unwrap().standing,
                Standing::NotSuspended
            );
        }
        // A restore logged after the removal but issued before it loses.
        let served = serve(
            &[
                channel_leaf(1, Action::ChannelRemove, &removed, 30),
                channel_leaf(2, Action::ChannelRestore, &removed, 20),
            ],
            BUILT_AT,
            0,
        );
        assert_eq!(served.snapshot.root().entry_count, 1);
        assert_eq!(channel_prefix_hex(&removed), "2121");
    }

    #[test]
    fn an_account_and_a_channel_share_a_bucket_and_do_not_mix() {
        // A channel id whose first two bytes are the account target's.
        let account = [5u8; 32];
        let target = account_target_hash(&account);
        let mut channel = [0u8; 16];
        channel.copy_from_slice(&target[..16]);
        let served = serve(
            &[
                statement_leaf(1, Action::Suspend, &account, 10, 0),
                channel_leaf(2, Action::ChannelRemove, &channel, 10),
            ],
            BUILT_AT,
            0,
        );
        let prefix = account_prefix(&account);
        assert_eq!(prefix, channel_prefix(&channel));
        let bucket = Bucket::decode(&served.snapshot.bucket(prefix)).unwrap();
        assert_eq!(bucket.entries.len(), 2);
        let ans = answer(&served, prefix, false);
        assert_eq!(
            check(&ans, &account, now()).unwrap().standing,
            Standing::Suspended { until_ms: None }
        );
        assert_eq!(
            check_channel(&ans, &channel, now()).unwrap().standing,
            Standing::Suspended { until_ms: None }
        );
        // The channel's bytes as an account target (wrong length) and the
        // account target as a channel are refused, not read.
        assert!(matches!(
            check_target(
                &ans,
                TargetKind::Channel,
                &target,
                &op_public(),
                &[],
                1,
                now()
            ),
            Err(SnapshotError::Decode(_))
        ));
        // Another channel in the same bucket is not removed.
        let mut other = channel;
        other[15] ^= 1;
        assert_eq!(
            check_channel(&ans, &other, now()).unwrap().standing,
            Standing::NotSuspended
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn every_refusal() {
        let a = [5u8; 32];
        let served = serve(
            &[statement_leaf(1, Action::Suspend, &a, 10, 0)],
            BUILT_AT,
            2,
        );
        let p = account_prefix(&a);
        let good = answer(&served, p, true);
        let witness = SigningKey::from_seed(&WITNESS_SEED)
            .verifying_key()
            .to_bytes();
        let keys: Vec<&[u8]> = vec![&witness];

        // Witnessed: ok with the key, refused when the threshold is not met.
        assert!(check_account(&good, &a, &op_public(), &keys, 1, now()).is_ok());
        let other_witness = [1u8; 32];
        let two: Vec<&[u8]> = vec![&witness, &other_witness];
        assert_eq!(
            check_account(&good, &a, &op_public(), &two, 2, now()),
            Err(SnapshotError::WitnessQuorum)
        );
        assert_eq!(
            check_account(&good, &a, &op_public(), &keys, 0, now()),
            Err(SnapshotError::WitnessQuorum)
        );
        let unwitnessed = answer(&served, p, false);
        assert_eq!(
            check_account(&unwitnessed, &a, &op_public(), &keys, 1, now()),
            Err(SnapshotError::WitnessQuorum)
        );

        // Too large, malformed, bad key, bad signature.
        let big = vec![b' '; MAX_ANSWER_BYTES + 1];
        assert_eq!(check(&big, &a, now()), Err(SnapshotError::TooLarge));
        assert_eq!(
            check(b"{}", &a, now()),
            Err(SnapshotError::Malformed("json"))
        );
        let other_op = SigningKey::from_seed(&[13; 32]).verifying_key().to_bytes();
        assert_eq!(
            check_account(&good, &a, &other_op, &[], 1, now()),
            Err(SnapshotError::BadRootSignature)
        );

        let mut v: BucketAnswer = serde_json::from_slice(&good).unwrap();
        let edit = |f: &dyn Fn(&mut BucketAnswer)| {
            let mut x = v.clone();
            f(&mut x);
            serde_json::to_vec(&x).unwrap()
        };
        assert_eq!(
            check(
                &edit(&|x| x.bucket_path_b64 = vec![String::new(); 65]),
                &a,
                now()
            ),
            Err(SnapshotError::TooLarge)
        );
        assert_eq!(
            check(&edit(&|x| x.root.tree_root_b64 = "AA==".into()), &a, now()),
            Err(SnapshotError::Malformed("tree root"))
        );
        assert_eq!(
            check(&edit(&|x| x.snapshot_index = 0), &a, now()),
            Err(SnapshotError::SnapshotNotIncluded)
        );
        assert_eq!(
            check(&edit(&|x| x.period += 1), &a, now()),
            Err(SnapshotError::WrongPeriod)
        );
        let neighbour = p ^ 1;
        let neighbour_path: Vec<String> = served
            .snapshot
            .bucket_path(neighbour)
            .iter()
            .map(|h| B64.encode(h))
            .collect();
        assert_eq!(
            check(
                &edit(&|x| x.bucket_path_b64 = neighbour_path.clone()),
                &a,
                now()
            ),
            Err(SnapshotError::BucketNotIncluded)
        );
        assert_eq!(
            check(&edit(&|x| x.prefix = "zz00".into()), &a, now()),
            Err(SnapshotError::Malformed("prefix"))
        );
        // A genuine answer for another prefix.
        let other = answer(&served, neighbour, false);
        assert_eq!(check(&other, &a, now()), Err(SnapshotError::WrongPrefix));

        // Freshness.
        let built = i64::try_from(BUILT_AT).unwrap();
        let two_periods = i64::from(PERIOD) * 2000;
        assert!(check(&good, &a, built + two_periods - 1).is_ok());
        assert_eq!(
            check(&good, &a, built + two_periods),
            Err(SnapshotError::Stale)
        );
        assert!(check(&good, &a, built - 600_000).is_ok());
        assert_eq!(check(&good, &a, built - 600_001), Err(SnapshotError::Stale));
        assert_eq!(check(&good, &a, -1), Err(SnapshotError::Stale));

        // A leaf of another kind at the snapshot index.
        let mut log = served.log.clone();
        log[1] = vec![2, 0];
        let bad = Served {
            snapshot: served.snapshot.clone(),
            index: 1,
            log,
        };
        let mut x: BucketAnswer = serde_json::from_slice(&answer(&bad, p, false)).unwrap();
        x.snapshot_leaf_b64 = B64.encode([2u8, 0]);
        assert_eq!(
            check(&serde_json::to_vec(&x).unwrap(), &a, now()),
            Err(SnapshotError::Decode(ModerationError::Malformed(
                "not a snapshot leaf"
            )))
        );
        v.prefix = prefix_hex(p).to_uppercase();
        assert!(check(&serde_json::to_vec(&v).unwrap(), &a, now()).is_ok());
    }

    // -- prove_standing ---------------------------------------------------

    /// A leaf of `action` on `target`, signed by moderator `moderator` of the
    /// test list (1 holds the Ed25519 key, 9 is not listed).
    fn real_leaf(id: u16, action: Action, target: Target, until_ms: u64, moderator: u8) -> Vec<u8> {
        use crate::moderators::tests::{entry, list};
        use crate::webauthn::tests::{keys, make};
        let [ed, _] = keys();
        let mut l = list();
        l.entries = vec![entry(1, ed.clone())];
        let mut statement_id = [0u8; 16];
        statement_id[..2].copy_from_slice(&id.to_be_bytes());
        let s = Statement {
            statement_id,
            action,
            target,
            reason_commitment: reason_commitment(Reason::Spam, &[1; 16]),
            until_ms,
            issued_at_ms: 5_000,
            moderator_id: [moderator; 16],
            list_version: 3,
        };
        let signed = SignedStatement {
            statement: s.encode_checked().unwrap(),
            assertion: make(&l, &ed, &s.challenge(), 0x05),
        };
        let mut leaf = vec![LOG_LEAF_STATEMENT];
        leaf.extend_from_slice(&signed.encode().unwrap());
        leaf
    }

    fn moderator_list() -> Vec<u8> {
        use crate::moderators::tests::{entry, list, ROOT};
        use crate::webauthn::tests::keys;
        let [ed, _] = keys();
        let mut l = list();
        l.entries = vec![entry(1, ed)];
        l.sign(&ROOT).unwrap()
    }

    fn mod_root() -> [u8; 32] {
        crate::moderators::tests::root_public()
    }

    /// Serve a snapshot built from `claimed` over a log whose first leaves
    /// are `real` (the leaves a client fetches), with the snapshot leaf after
    /// them. A builder that lies is a `claimed` that is not `real`.
    fn serve_over(claimed: &[Vec<u8>], real: &[Vec<u8>]) -> Served {
        let refs: Vec<&[u8]> = claimed.iter().map(Vec::as_slice).collect();
        let snapshot = build_snapshot(&refs, BUILT_AT, PERIOD).unwrap();
        let mut log = real.to_vec();
        log.push(snapshot.root().log_leaf());
        Served {
            snapshot,
            index: real.len() as u64,
            log,
        }
    }

    fn entry_answer(served: &Served, index: u64) -> Vec<u8> {
        let i = usize::try_from(index).unwrap();
        serde_json::to_vec(&LogEntryAnswer {
            index,
            leaf_b64: B64.encode(&served.log[i]),
            audit_path_b64: audit_path(&served.log, i)
                .iter()
                .map(|h| B64.encode(h))
                .collect(),
            log_size: served.log.len() as u64,
        })
        .unwrap()
    }

    fn prove(
        served: &Served,
        target: &Target,
        entry: &[u8],
        now_ms: i64,
    ) -> Result<(), SnapshotError> {
        let ans = answer(served, target_prefix(target), false);
        prove_standing(
            &ans,
            entry,
            &moderator_list(),
            &mod_root(),
            target.kind(),
            target.bytes(),
            now_ms,
        )
    }

    #[test]
    fn a_signed_suspension_and_removal_are_proven() {
        let a = Target::account(&[5u8; 32]).unwrap();
        let ch = Target::new(TargetKind::Channel, &[0x31; 16]).unwrap();
        let until = BUILT_AT + 50_000;
        let leaves = vec![
            real_leaf(1, Action::Suspend, a.clone(), until, 1),
            real_leaf(2, Action::ChannelRemove, ch.clone(), 0, 1),
        ];
        let served = serve_over(&leaves, &leaves);
        let ans = answer(&served, target_prefix(&a), false);
        let got = check_account(&ans, &[5u8; 32], &op_public(), &[], 1, now()).unwrap();
        assert_eq!(got.log_index, Some(0));
        assert_eq!(got.until_ms, Some(until));
        assert_eq!(prove(&served, &a, &entry_answer(&served, 0), now()), Ok(()));
        assert_eq!(
            prove(&served, &ch, &entry_answer(&served, 1), now()),
            Ok(())
        );
        // An account the bucket does not hold has no log index.
        let other = [6u8; 32];
        let ans = answer(&served, account_prefix(&other), false);
        let got = check_account(&ans, &other, &op_public(), &[], 1, now()).unwrap();
        assert_eq!((got.log_index, got.until_ms), (None, None));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn a_mark_without_its_statement_is_refused() {
        let a = Target::account(&[5u8; 32]).unwrap();
        let b = Target::account(&[6u8; 32]).unwrap();
        let until = BUILT_AT + 50_000;
        let honest = vec![real_leaf(1, Action::Suspend, a.clone(), until, 1)];
        let served = serve_over(&honest, &honest);
        let good = entry_answer(&served, 0);

        // The entry's index, the head's size, and a tampered leaf.
        let mut x: LogEntryAnswer = serde_json::from_slice(&good).unwrap();
        x.index = 1;
        assert_eq!(
            prove(&served, &a, &serde_json::to_vec(&x).unwrap(), now()),
            Err(SnapshotError::EntryMismatch)
        );
        let mut x: LogEntryAnswer = serde_json::from_slice(&good).unwrap();
        x.log_size += 1;
        assert_eq!(
            prove(&served, &a, &serde_json::to_vec(&x).unwrap(), now()),
            Err(SnapshotError::EntryMismatch)
        );
        let mut x: LogEntryAnswer = serde_json::from_slice(&good).unwrap();
        let mut leaf = B64.decode(&x.leaf_b64).unwrap();
        let last = leaf.len() - 1;
        leaf[last] ^= 1;
        x.leaf_b64 = B64.encode(&leaf);
        assert_eq!(
            prove(&served, &a, &serde_json::to_vec(&x).unwrap(), now()),
            Err(SnapshotError::EntryNotIncluded)
        );
        assert_eq!(
            prove(&served, &a, b"{}", now()),
            Err(SnapshotError::Malformed("entry json"))
        );
        // The list under another root.
        let ans = answer(&served, target_prefix(&a), false);
        assert_eq!(
            prove_standing(
                &ans,
                &good,
                &moderator_list(),
                &[1; 32],
                TargetKind::Account,
                a.bytes(),
                now()
            ),
            Err(SnapshotError::BadModeratorList)
        );
        // No entry, and an entry that has ended.
        assert_eq!(
            prove(&served, &b, &good, now()),
            Err(SnapshotError::NoEntry)
        );
        let later = i64::try_from(until).unwrap();
        assert_eq!(
            prove(&served, &a, &good, later),
            Err(SnapshotError::NotSuspended)
        );
        // A snapshot entry naming its own leaf, which the snapshot did not
        // cover.
        let ans = answer(&served, target_prefix(&a), false);
        assert_eq!(
            prove_standing(
                &ans,
                &entry_answer(&served, served.index),
                &moderator_list(),
                &mod_root(),
                TargetKind::Account,
                a.bytes(),
                now()
            ),
            Err(SnapshotError::EntryMismatch)
        );

        // A builder that lies: the snapshot claims a suspension of `a` at
        // index 0, and the log holds something else there.
        let claimed = vec![statement_leaf(1, Action::Suspend, &[5u8; 32], 5_000, until)];
        let forged = {
            let leaf = real_leaf(1, Action::Suspend, a.clone(), until, 1);
            let mut signed = SignedStatement::decode(&leaf[1..]).unwrap();
            signed.assertion.signature[0] ^= 1;
            let mut leaf = vec![LOG_LEAF_STATEMENT];
            leaf.extend_from_slice(&signed.encode().unwrap());
            leaf
        };
        let cases: Vec<(Vec<u8>, SnapshotError)> = vec![
            // Not a statement at all.
            (vec![2, 1, 2, 3], SnapshotError::NotAStatement),
            // A listed moderator's statement whose signature fails.
            (forged, SnapshotError::BadStatement),
            // A statement by a moderator id the list does not hold.
            (claimed[0].clone(), SnapshotError::NotAuthorised),
            // A moderator the list does not hold.
            (
                real_leaf(1, Action::Suspend, a.clone(), until, 9),
                SnapshotError::NotAuthorised,
            ),
            // A signed lift.
            (
                real_leaf(1, Action::Lift, a.clone(), 0, 1),
                SnapshotError::WrongAction,
            ),
            // A signed keep.
            (
                real_leaf(1, Action::KeepSuspension, a.clone(), 0, 1),
                SnapshotError::WrongAction,
            ),
            // A signed suspension of someone else.
            (
                real_leaf(1, Action::Suspend, b.clone(), until, 1),
                SnapshotError::WrongTarget,
            ),
            // A signed suspension of `a` with another end.
            (
                real_leaf(1, Action::Suspend, a.clone(), 0, 1),
                SnapshotError::UntilMismatch,
            ),
        ];
        for (real, expect) in cases {
            let served = serve_over(&claimed, &[real]);
            assert_eq!(
                prove(&served, &a, &entry_answer(&served, 0), now()),
                Err(expect.clone()),
                "{expect:?}"
            );
        }
        // A target kind no snapshot holds.
        assert_eq!(
            prove_standing(
                b"{}",
                b"{}",
                &moderator_list(),
                &mod_root(),
                TargetKind::Blob,
                &[1; 32],
                now()
            ),
            Err(SnapshotError::WrongAction)
        );
    }

    #[test]
    fn the_codes_are_stable() {
        assert_eq!(SnapshotError::Stale.code(), "stale");
        assert_eq!(SnapshotError::NoEntry.code(), "no-entry");
        assert_eq!(SnapshotError::NotSuspended.code(), "not-suspended");
        assert_eq!(SnapshotError::EntryMismatch.code(), "entry-mismatch");
        assert_eq!(SnapshotError::EntryNotIncluded.code(), "entry-not-included");
        assert_eq!(SnapshotError::NotAStatement.code(), "not-a-statement");
        assert_eq!(SnapshotError::BadModeratorList.code(), "bad-list");
        assert_eq!(SnapshotError::NotAuthorised.code(), "not-authorised");
        assert_eq!(SnapshotError::BadStatement.code(), "bad-statement");
        assert_eq!(SnapshotError::WrongAction.code(), "wrong-action");
        assert_eq!(SnapshotError::WrongTarget.code(), "wrong-target");
        assert_eq!(SnapshotError::UntilMismatch.code(), "until-mismatch");
        assert_eq!(
            SnapshotError::Decode(ModerationError::WrongDomain).code(),
            "malformed-snapshot"
        );
    }

    #[test]
    fn prefixes() {
        let key = [3u8; 32];
        let t = account_target_hash(&key);
        assert_eq!(
            account_prefix_hex(&key),
            format!("{:02x}{:02x}", t[0], t[1])
        );
        assert_eq!(parse_prefix("00ff"), Some(0x00ff));
        assert_eq!(parse_prefix("ABCD"), Some(0xabcd));
        assert_eq!(parse_prefix("+abc"), None);
        assert_eq!(parse_prefix("abcde"), None);
        assert_eq!(parse_prefix("abc"), None);
    }

    #[test]
    fn root_codec() {
        let root = SnapshotRoot {
            period: period_of(BUILT_AT, PERIOD),
            period_secs: PERIOD,
            built_at_ms: BUILT_AT,
            covered_log_size: 4,
            bucket_root: [7; 32],
            entry_count: 2,
        };
        let bytes = root.encode();
        assert_eq!(bytes.len(), SNAPSHOT_ROOT_LEN);
        assert_eq!(SnapshotRoot::decode(&bytes), Ok(root.clone()));
        assert_eq!(
            SnapshotRoot::decode_log_leaf(&root.log_leaf()),
            Ok(root.clone())
        );
        let mut wrong = root.clone();
        wrong.period += 1;
        assert!(SnapshotRoot::decode(&wrong.encode()).is_err());
        let mut zero = root.clone();
        zero.period_secs = 0;
        zero.period = 0;
        assert!(SnapshotRoot::decode(&zero.encode()).is_err());
        assert!(SnapshotRoot::decode(&bytes[..bytes.len() - 1]).is_err());
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(SnapshotRoot::decode(&longer).is_err());
        let mut v2 = bytes;
        v2[SNAPSHOT_DOMAIN.len()] = 2;
        assert!(SnapshotRoot::decode(&v2).is_err());
        assert!(build_snapshot(&[], BUILT_AT, 0).is_err());
    }

    #[test]
    fn bucket_refusals() {
        let mut t1 = [0u8; 32];
        t1[..2].copy_from_slice(&[0x12, 0x34]);
        let mut t2 = t1;
        t2[31] = 1;
        let e = |t: [u8; 32]| SnapshotEntry {
            target: Target::new(TargetKind::Account, &t).unwrap(),
            until_ms: 0,
            log_index: 1,
        };
        let ok = Bucket {
            prefix: 0x1234,
            entries: vec![e(t1), e(t2)],
        };
        let bytes = ok.encode().unwrap();
        assert_eq!(Bucket::decode(&bytes), Ok(ok.clone()));
        assert_eq!(ok.lookup(&e(t2).target).map(|x| x.log_index), Some(1));
        assert!(Bucket {
            prefix: 0x1234,
            entries: vec![e(t2), e(t1)]
        }
        .encode()
        .is_err());
        assert!(Bucket {
            prefix: 0x1234,
            entries: vec![e(t1), e(t1)]
        }
        .encode()
        .is_err());
        assert!(Bucket {
            prefix: 0x1235,
            entries: vec![e(t1)]
        }
        .encode()
        .is_err());
        let mut over = bytes.clone();
        over[2..6].copy_from_slice(&u32::try_from(MAX_BUCKET_ENTRIES + 1).unwrap().to_be_bytes());
        assert!(Bucket::decode(&over).is_err());
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(Bucket::decode(&trailing).is_err());
        assert!(Bucket::decode(&bytes[..bytes.len() - 1]).is_err());
        assert_eq!(
            Bucket::decode(&empty_bucket_bytes(7)),
            Ok(Bucket {
                prefix: 7,
                entries: Vec::new()
            })
        );
    }

    fn fixture() -> &'static (Vec<u8>, [u8; 32], AccountStanding) {
        static F: OnceLock<(Vec<u8>, [u8; 32], AccountStanding)> = OnceLock::new();
        F.get_or_init(|| {
            let a = [5u8; 32];
            let served = serve(
                &[statement_leaf(
                    1,
                    Action::Suspend,
                    &a,
                    10,
                    BUILT_AT + 9_000_000,
                )],
                BUILT_AT,
                5,
            );
            let ans = answer(&served, account_prefix(&a), true);
            let standing = check(&ans, &a, now()).unwrap();
            (ans, a, standing)
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(12))]

        #[test]
        fn build_serve_check_round_trip(
            ops in proptest::collection::vec((0u8..4, any::<bool>(), 0u64..50, 0u64..3), 0..12),
            extra in 0usize..4,
            now_offset in 0u64..172_800_000,
        ) {
            // Four accounts; each op suspends (with no end, an end before the
            // build, or one after it) or lifts, at an issue time in 0..50.
            let accounts: Vec<[u8; 32]> = (0u8..4).map(|i| [i + 40; 32]).collect();
            let mut leaves = Vec::new();
            let mut model: BTreeMap<u8, (u64, u64, bool, u64)> = BTreeMap::new();
            for (i, (who, suspend, issued, end)) in ops.iter().enumerate() {
                let until = if *suspend {
                    match end { 0 => 0, 1 => 100, _ => BUILT_AT + 100_000 }
                } else { 0 };
                let action = if *suspend { Action::Suspend } else { Action::Lift };
                leaves.push(statement_leaf(
                    u16::try_from(i).unwrap(),
                    action,
                    &accounts[usize::from(*who)],
                    *issued,
                    until,
                ));
                let cand = (*issued, i as u64, *suspend, until);
                let held = model.entry(*who).or_insert(cand);
                if (cand.0, cand.1) > (held.0, held.1) {
                    *held = cand;
                }
            }
            let served = serve(&leaves, BUILT_AT, extra);
            let now_ms = BUILT_AT + now_offset;
            for (i, a) in accounts.iter().enumerate() {
                let ans = answer(&served, account_prefix(a), false);
                let got = check(&ans, a, i64::try_from(now_ms).unwrap()).unwrap();
                let expect = match model.get(&u8::try_from(i).unwrap()) {
                    Some(&(_, _, true, 0)) => Standing::Suspended { until_ms: None },
                    Some(&(_, _, true, until))
                        if until > BUILT_AT && until > now_ms =>
                        Standing::Suspended { until_ms: Some(until) },
                    _ => Standing::NotSuspended,
                };
                prop_assert_eq!(got.standing, expect);
            }
        }
    }

    proptest! {
        #[test]
        fn a_flipped_byte_is_refused_or_changes_nothing(
            position in any::<prop::sample::Index>(),
            mask in 1u8..=255,
        ) {
            let (ans, a, standing) = fixture();
            let mut bytes = ans.clone();
            let i = position.index(bytes.len());
            bytes[i] ^= mask;
            let witness = SigningKey::from_seed(&WITNESS_SEED).verifying_key().to_bytes();
            let keys: Vec<&[u8]> = vec![&witness];
            if let Ok(got) = check_account(&bytes, a, &op_public(), &keys, 1, now()) {
                prop_assert_eq!(got.standing, standing.standing);
            }
        }

        #[test]
        fn bucket_codec_round_trips(
            tails in proptest::collection::btree_set(any::<[u8; 30]>(), 0..20),
            prefix in any::<u16>(),
            until in any::<u64>(),
        ) {
            let entries: Vec<SnapshotEntry> = tails
                .iter()
                .enumerate()
                .map(|(i, tail)| {
                    let mut t = [0u8; 32];
                    t[..2].copy_from_slice(&prefix.to_be_bytes());
                    t[2..].copy_from_slice(tail);
                    SnapshotEntry {
                        target: Target::new(TargetKind::Account, &t).unwrap(),
                        until_ms: until,
                        log_index: i as u64,
                    }
                })
                .collect();
            let bucket = Bucket { prefix, entries };
            let bytes = bucket.encode().unwrap();
            prop_assert_eq!(Bucket::decode(&bytes).unwrap(), bucket);
        }

        #[test]
        fn decoders_are_exact(bytes in proptest::collection::vec(any::<u8>(), 0..300)) {
            if let Ok(b) = Bucket::decode(&bytes) {
                prop_assert_eq!(b.encode().unwrap(), bytes.clone());
            }
            if let Ok(r) = SnapshotRoot::decode(&bytes) {
                prop_assert_eq!(r.encode(), bytes.clone());
            }
        }
    }
}
