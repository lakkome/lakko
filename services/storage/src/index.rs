// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The storage index (ADR-0065 section 1, the index of ADR-0064 section 1).
//!
//! The index is the coordinator and the lookup; the files under
//! `LAKKO_BLOB_DIR` remain the truth for content. Every blob request is
//! answered from the index: a blob with no row does not exist for a client,
//! whatever lies on disk.
//!
//! Two implementations behind one trait, the `ChannelStore` pattern of
//! `services/channels`:
//!
//! - [`sql::SqlIndex`] over MariaDB (`LAKKO_STORAGE_DATABASE_URL`), what
//!   several instances would share;
//! - [`file::FileIndex`], an append-only log file beside the blobs replayed
//!   into memory at startup, for development, tests and a single instance
//!   with no database.
//!
//! [`BlobIndex`] carries the blob rows. Its sibling [`VaultIndex`] carries the
//! vault rows of ADR-0064 section 1 (one row per vault: its write key, the
//! bytes it uses, its write-rate counter and its node; one row per object:
//! its current version, size and any reservation), implemented by the same
//! two structs over the same database or index file.
//!
//! # What a row holds
//!
//! Size, storage node, creation day, last access day, the delete public
//! key, and an optional idle life in days (ADR-0087 section 13: a channel
//! file sets 45; a row without one lives the service's default lifetime).
//! Nothing names an account (invariant #5). Days are unix day numbers
//! (`unix_secs / 86400`), so nothing finer than the day is recorded about
//! when a file is read.

use async_trait::async_trait;

pub(crate) mod file;
pub(crate) mod sql;

/// A blob id: the SHA-256 of its ciphertext.
pub(crate) type BlobId = [u8; 32];

/// Seconds in a day, for [`day_of`].
const SECS_PER_DAY: u64 = 86_400;

/// The unix day number of `unix_secs`.
pub(crate) fn day_of(unix_secs: u64) -> u32 {
    u32::try_from(unix_secs / SECS_PER_DAY).unwrap_or(u32::MAX)
}

/// Whether a row last accessed on `last_access_day`, with its own idle life
/// `idle_days` or else `default_lifetime_days`, is past its life on `today`:
/// its last access is more than its life back.
pub(crate) fn is_expired(
    last_access_day: u32,
    idle_days: Option<u8>,
    today: u32,
    default_lifetime_days: u32,
) -> bool {
    let life = idle_days.map_or(default_lifetime_days, u32::from);
    u64::from(last_access_day) + u64::from(life) < u64::from(today)
}

/// Today's unix day number by the system clock.
pub(crate) fn today() -> u32 {
    day_of(lakko_anon_token_server::now_unix_secs())
}

/// One blob's index row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BlobRow {
    /// The blob id.
    pub(crate) id: BlobId,
    /// Ciphertext length in bytes.
    pub(crate) size: u64,
    /// The storage node holding the file.
    pub(crate) node: String,
    /// Day of the first upload (or of the import).
    pub(crate) created_day: u32,
    /// Day of the last download, keep or repeated upload.
    pub(crate) last_access_day: u32,
    /// The Ed25519 key a `DELETE` must be signed with; `None` for a row that
    /// cannot be deleted (an imported blob).
    pub(crate) delete_pk: Option<[u8; 32]>,
    /// The blob's own idle life in days (1..=255), set by its first upload;
    /// `None` lives the service's default lifetime.
    pub(crate) idle_days: Option<u8>,
}

/// What [`BlobIndex::record_upload`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UploadOutcome {
    /// A new row was written.
    Created,
    /// A row already existed; its last access was moved to the upload day
    /// and its delete key left as it was (the first uploader's).
    Existed,
}

/// An index store failure. Callers answer 500 and do not honour the request.
#[derive(Debug, thiserror::Error)]
pub(crate) enum IndexError {
    /// The database failed.
    #[error("index database error: {0}")]
    Db(#[from] sqlx::Error),
    /// The index file failed.
    #[error("index file error: {0}")]
    Io(#[from] std::io::Error),
    /// The index file holds a line this build cannot read.
    #[error("index file corrupt at line {0}")]
    Corrupt(usize),
}

/// The blob index.
#[async_trait]
pub(crate) trait BlobIndex: Send + Sync {
    /// The row of `id`, if any.
    async fn get(&self, id: &BlobId) -> Result<Option<BlobRow>, IndexError>;

    /// Record a stored upload. A new row is written as `row`; an existing
    /// row keeps its delete key, creation day and idle life and has its last
    /// access raised to `row.last_access_day`.
    async fn record_upload(&self, row: BlobRow) -> Result<UploadOutcome, IndexError>;

    /// Raise the last access day of each existing id in `ids` to `day`. Ids
    /// without a row are ignored; a row already at `day` is not written.
    async fn touch(&self, ids: &[BlobId], day: u32) -> Result<(), IndexError>;

    /// Remove the row of `id`. Returns whether a row was removed.
    async fn remove(&self, id: &BlobId) -> Result<bool, IndexError>;

    /// Up to `limit` ids on `node` past their life on `today` ([`is_expired`]:
    /// a row's own idle days, else `default_lifetime_days`), oldest access
    /// first.
    async fn expired(
        &self,
        node: &str,
        today: u32,
        default_lifetime_days: u32,
        limit: usize,
    ) -> Result<Vec<BlobId>, IndexError>;

    /// Remove the row of `id` only if it is still past its life on `today`,
    /// so a download that raced the sweep keeps the blob. Returns whether a
    /// row was removed.
    async fn remove_if_expired(
        &self,
        id: &BlobId,
        today: u32,
        default_lifetime_days: u32,
    ) -> Result<bool, IndexError>;

    /// Whether the one-time import has run for `node`.
    async fn import_done(&self, node: &str) -> Result<bool, IndexError>;

    /// Insert `rows` where no row exists yet, then mark the import done for
    /// `node` on `day`. Returns how many rows were inserted.
    async fn import(&self, rows: Vec<BlobRow>, node: &str, day: u32) -> Result<usize, IndexError>;
}

// ============================================================================
// The vault index (ADR-0064 section 1)
// ============================================================================

/// A vault id: 32 bytes the client derives from its vault root and the
/// vault's reference. Names no account.
pub(crate) type VaultId = [u8; 32];

/// A reservation token: 16 random bytes that name one reservation, so a
/// stale writer cannot commit over the writer that took its reservation, and
/// so every object file name is written once.
pub(crate) type ReservationToken = [u8; 16];

/// Which object of a vault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum ObjectAddr {
    /// The head.
    Head,
    /// Chunk `n`.
    Chunk(u32),
}

impl ObjectAddr {
    /// The kind byte stored in the index (the `lakko-vault` object kind: 1
    /// head, 2 chunk).
    pub(crate) const fn kind_byte(self) -> u8 {
        match self {
            Self::Head => 1,
            Self::Chunk(_) => 2,
        }
    }

    /// The chunk number stored in the index (0 for the head).
    pub(crate) const fn chunk_no(self) -> u32 {
        match self {
            Self::Head => 0,
            Self::Chunk(n) => n,
        }
    }

    /// Read back what [`Self::kind_byte`] and [`Self::chunk_no`] stored.
    pub(crate) const fn from_parts(kind: u8, chunk_no: u32) -> Option<Self> {
        match (kind, chunk_no) {
            (1, 0) => Some(Self::Head),
            (2, n) => Some(Self::Chunk(n)),
            _ => None,
        }
    }
}

/// One vault's index row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VaultRow {
    /// The vault id.
    pub(crate) id: VaultId,
    /// The Ed25519 key every head, chunk and delete request is signed with.
    pub(crate) write_pk: [u8; 32],
    /// The storage node holding the vault's objects.
    pub(crate) node: String,
    /// Day of the claim.
    pub(crate) created_day: u32,
    /// Bytes of the vault's committed objects.
    pub(crate) bytes_used: u64,
}

/// One object's committed state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObjectRow {
    /// The current version, at least 1.
    pub(crate) version: u64,
    /// The reservation that wrote it; part of the file name.
    pub(crate) token: ReservationToken,
    /// Stored bytes (version prefix excluded).
    pub(crate) size: u64,
}

/// What [`VaultIndex::claim`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaimOutcome {
    /// A new vault row was written.
    Created,
    /// The vault was already claimed under the same key.
    SameKey,
    /// The vault is claimed under another key.
    OtherKey,
}

/// The per-vault limits (all chosen, not measured; see `crate::vault`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VaultLimits {
    /// Most bytes a vault's objects may hold, counting live reservations.
    pub(crate) max_bytes: u64,
    /// Most chunk numbers a vault may hold.
    pub(crate) max_chunks: u32,
    /// Most write reservations per vault per clock hour.
    pub(crate) max_writes_per_hour: u32,
    /// How long a reservation holds its version before another writer may
    /// take it.
    pub(crate) reservation_ms: u64,
}

/// A write a client asks to reserve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WriteRequest {
    /// Which object.
    pub(crate) addr: ObjectAddr,
    /// The version the body carries.
    pub(crate) version: u64,
    /// The bytes it will store.
    pub(crate) size: u64,
    /// The server clock, unix milliseconds.
    pub(crate) now_ms: u64,
}

/// What [`VaultIndex::reserve`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReserveOutcome {
    /// The version is held for this writer under `token` until the
    /// reservation times out.
    Reserved(ReservationToken),
    /// No vault row.
    NoVault,
    /// The version is not `current + 1`, or another writer holds a live
    /// reservation on the object.
    VersionConflict,
    /// A chunk number above the vault's chunk count, or at the count when
    /// the vault already holds the most chunks it may.
    ChunkOutOfRange,
    /// The write would take the vault past its byte limit.
    VaultFull,
    /// The vault used its writes for this hour.
    RateLimited,
}

/// What [`VaultIndex::commit`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommitOutcome {
    /// The version is current; the object it replaced (if any) may be
    /// deleted.
    Committed {
        /// The replaced object's version and token.
        replaced: Option<(u64, ReservationToken)>,
    },
    /// The reservation is gone (taken by another writer after it timed out,
    /// or the vault was deleted); the written file is an orphan to remove.
    Lost,
}

/// The unix clock hour of `now_ms`, the write-rate window.
pub(crate) const fn hour_of(now_ms: u64) -> u64 {
    now_ms / 3_600_000
}

/// A fresh reservation token.
pub(crate) fn new_token() -> ReservationToken {
    let mut t = [0u8; 16];
    lakko_crypto::random::fill(&mut t);
    t
}

/// What an index knows about a vault and one of its objects when a write
/// asks to reserve; [`decide_reserve`] turns it into a decision, so both
/// implementations decide alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ReserveFacts {
    /// The object's row: its committed version (0 = none yet) and size.
    pub(crate) object: Option<(u64, u64)>,
    /// Another writer holds a reservation on this object that has not timed
    /// out.
    pub(crate) object_reserved: bool,
    /// Chunk rows the vault holds (committed or reserved).
    pub(crate) chunk_rows: u64,
    /// Bytes of the vault's committed objects.
    pub(crate) bytes_used: u64,
    /// Bytes held by live reservations on the vault's other objects.
    pub(crate) other_reserved_bytes: u64,
    /// Reservations already made in the current hour.
    pub(crate) writes_this_hour: u32,
}

/// Decide a reservation: `Ok(())` to reserve, else the refusal. Checked in
/// this order: version, chunk number, bytes, write rate, so a refused write
/// costs no write from the hour's allowance.
pub(crate) fn decide_reserve(
    req: &WriteRequest,
    facts: &ReserveFacts,
    limits: &VaultLimits,
) -> Result<(), ReserveOutcome> {
    if facts.object_reserved {
        return Err(ReserveOutcome::VersionConflict);
    }
    let (current, current_size) = facts.object.unwrap_or((0, 0));
    if current.checked_add(1) != Some(req.version) {
        return Err(ReserveOutcome::VersionConflict);
    }
    if let ObjectAddr::Chunk(n) = req.addr {
        if facts.object.is_none() {
            let n = u64::from(n);
            if n > facts.chunk_rows || facts.chunk_rows >= u64::from(limits.max_chunks) {
                return Err(ReserveOutcome::ChunkOutOfRange);
            }
        }
    }
    let after = facts
        .bytes_used
        .saturating_add(facts.other_reserved_bytes)
        .saturating_sub(current_size)
        .saturating_add(req.size);
    if after > limits.max_bytes {
        return Err(ReserveOutcome::VaultFull);
    }
    if facts.writes_this_hour >= limits.max_writes_per_hour {
        return Err(ReserveOutcome::RateLimited);
    }
    Ok(())
}

/// The vault index.
///
/// A write is three steps: [`Self::reserve`] holds `current + 1` (one
/// conditional update; a second writer gets [`ReserveOutcome::VersionConflict`]),
/// the caller stores the object file, then [`Self::commit`] makes the version
/// current, or [`Self::release`] drops the reservation when the file could
/// not be stored. A reservation whose writer never came back stops blocking
/// once it times out: the next reservation takes it over.
#[async_trait]
pub(crate) trait VaultIndex: Send + Sync {
    /// The row of `id`, if any.
    async fn vault(&self, id: &VaultId) -> Result<Option<VaultRow>, IndexError>;

    /// Claim `row.id` for `row.write_pk`: the first claim wins.
    async fn claim(&self, row: VaultRow) -> Result<ClaimOutcome, IndexError>;

    /// Remove a vault row that has no object, after its claim object could
    /// not be stored. Returns whether a row was removed.
    async fn unclaim(&self, id: &VaultId) -> Result<bool, IndexError>;

    /// The committed state of an object, if it has a committed version.
    async fn object(&self, id: &VaultId, addr: ObjectAddr)
        -> Result<Option<ObjectRow>, IndexError>;

    /// Reserve `req.version` of `req.addr`; see [`ReserveOutcome`].
    async fn reserve(
        &self,
        id: &VaultId,
        req: WriteRequest,
        limits: &VaultLimits,
    ) -> Result<ReserveOutcome, IndexError>;

    /// Make the reservation `token` of `addr` current with `size` bytes.
    async fn commit(
        &self,
        id: &VaultId,
        addr: ObjectAddr,
        token: &ReservationToken,
        size: u64,
    ) -> Result<CommitOutcome, IndexError>;

    /// Drop the reservation `token` of `addr` (an object that never had a
    /// committed version is forgotten with it).
    async fn release(
        &self,
        id: &VaultId,
        addr: ObjectAddr,
        token: &ReservationToken,
    ) -> Result<(), IndexError>;

    /// Remove the vault row and every object row. Returns whether a row was
    /// removed.
    async fn delete_vault(&self, id: &VaultId) -> Result<bool, IndexError>;
}

#[cfg(test)]
pub(crate) mod contract {
    //! The behaviour both implementations must share, run against each.

    use std::sync::Arc;

    use super::{
        BlobIndex, BlobRow, ClaimOutcome, CommitOutcome, ObjectAddr, ReservationToken,
        ReserveOutcome, UploadOutcome, VaultId, VaultIndex, VaultLimits, VaultRow, WriteRequest,
    };

    /// Small limits so every refusal is reachable in a test.
    pub(crate) const fn limits() -> VaultLimits {
        VaultLimits {
            max_bytes: 1_000,
            max_chunks: 3,
            max_writes_per_hour: 20,
            reservation_ms: 60_000,
        }
    }

    /// A vault row with a random id (so a persistent database can be reused)
    /// and a write key of `key` bytes.
    pub(crate) fn vault_row(key: u8) -> VaultRow {
        let mut id = [0u8; 32];
        lakko_crypto::random::fill(&mut id);
        VaultRow {
            id,
            write_pk: [key; 32],
            node: "local".to_string(),
            created_day: 100,
            bytes_used: 0,
        }
    }

    /// A clock well inside one hour.
    pub(crate) const NOW: u64 = 1_700_000_100_000;

    async fn reserve_at(
        index: &dyn VaultIndex,
        id: &VaultId,
        addr: ObjectAddr,
        version: u64,
        size: u64,
        now_ms: u64,
        limits: &VaultLimits,
    ) -> ReserveOutcome {
        index
            .reserve(
                id,
                WriteRequest {
                    addr,
                    version,
                    size,
                    now_ms,
                },
                limits,
            )
            .await
            .unwrap()
    }

    /// Reserve and expect success.
    pub(crate) async fn reserve_ok(
        index: &dyn VaultIndex,
        id: &VaultId,
        addr: ObjectAddr,
        version: u64,
        size: u64,
        limits: &VaultLimits,
    ) -> ReservationToken {
        match reserve_at(index, id, addr, version, size, NOW, limits).await {
            ReserveOutcome::Reserved(t) => t,
            other => panic!("expected a reservation, got {other:?}"),
        }
    }

    /// Reserve, store, commit.
    async fn write(
        index: &dyn VaultIndex,
        id: &VaultId,
        addr: ObjectAddr,
        version: u64,
        size: u64,
        limits: &VaultLimits,
    ) -> CommitOutcome {
        let t = reserve_ok(index, id, addr, version, size, limits).await;
        index.commit(id, addr, &t, size).await.unwrap()
    }

    /// The vault behaviour both implementations must share.
    pub(crate) async fn run_vault(index: &dyn VaultIndex) {
        let lim = limits();
        let a = vault_row(1);
        let id = a.id;

        // Claim: first wins, same key is idempotent, another key is refused.
        assert_eq!(index.vault(&id).await.unwrap(), None);
        assert_eq!(
            reserve_at(index, &id, ObjectAddr::Head, 1, 1, NOW, &lim).await,
            ReserveOutcome::NoVault
        );
        assert_eq!(index.claim(a.clone()).await.unwrap(), ClaimOutcome::Created);
        assert_eq!(index.claim(a.clone()).await.unwrap(), ClaimOutcome::SameKey);
        let mut other = a.clone();
        other.write_pk = [2; 32];
        assert_eq!(index.claim(other).await.unwrap(), ClaimOutcome::OtherKey);
        assert_eq!(index.vault(&id).await.unwrap().unwrap().write_pk, [1; 32]);

        // Version rule: only current + 1.
        assert_eq!(
            reserve_at(index, &id, ObjectAddr::Head, 2, 10, NOW, &lim).await,
            ReserveOutcome::VersionConflict
        );
        assert_eq!(index.object(&id, ObjectAddr::Head).await.unwrap(), None);
        let t1 = reserve_ok(index, &id, ObjectAddr::Head, 1, 100, &lim).await;
        // A second writer for the same version meets the live reservation.
        assert_eq!(
            reserve_at(index, &id, ObjectAddr::Head, 1, 100, NOW, &lim).await,
            ReserveOutcome::VersionConflict
        );
        // Not yet committed: nothing to read.
        assert_eq!(index.object(&id, ObjectAddr::Head).await.unwrap(), None);
        assert_eq!(
            index.commit(&id, ObjectAddr::Head, &t1, 100).await.unwrap(),
            CommitOutcome::Committed { replaced: None }
        );
        let head = index.object(&id, ObjectAddr::Head).await.unwrap().unwrap();
        assert_eq!((head.version, head.token, head.size), (1, t1, 100));
        // A committed token commits nothing again.
        assert_eq!(
            index.commit(&id, ObjectAddr::Head, &t1, 100).await.unwrap(),
            CommitOutcome::Lost
        );
        assert_eq!(
            reserve_at(index, &id, ObjectAddr::Head, 1, 100, NOW, &lim).await,
            ReserveOutcome::VersionConflict
        );
        assert_eq!(
            write(index, &id, ObjectAddr::Head, 2, 120, &lim).await,
            CommitOutcome::Committed {
                replaced: Some((1, t1))
            }
        );
        assert_eq!(index.vault(&id).await.unwrap().unwrap().bytes_used, 120);

        // A stale reservation is taken over; the stale writer's commit is lost.
        let stale = reserve_ok(index, &id, ObjectAddr::Head, 3, 10, &lim).await;
        let later = NOW + lim.reservation_ms + 1;
        let ReserveOutcome::Reserved(fresh) =
            reserve_at(index, &id, ObjectAddr::Head, 3, 10, later, &lim).await
        else {
            panic!("a timed-out reservation must be taken over");
        };
        assert_eq!(
            index
                .commit(&id, ObjectAddr::Head, &stale, 10)
                .await
                .unwrap(),
            CommitOutcome::Lost
        );
        assert!(matches!(
            index
                .commit(&id, ObjectAddr::Head, &fresh, 10)
                .await
                .unwrap(),
            CommitOutcome::Committed {
                replaced: Some((2, _))
            }
        ));
        assert_eq!(index.vault(&id).await.unwrap().unwrap().bytes_used, 10);

        // A released reservation frees its version.
        let t = reserve_ok(index, &id, ObjectAddr::Head, 4, 10, &lim).await;
        index.release(&id, ObjectAddr::Head, &t).await.unwrap();
        let t = reserve_ok(index, &id, ObjectAddr::Head, 4, 10, &lim).await;
        index.commit(&id, ObjectAddr::Head, &t, 10).await.unwrap();
        vault_chunks_bytes_rate_delete(index, &a).await;
    }

    /// The second half of [`run_vault`], on the vault `a` it left behind.
    async fn vault_chunks_bytes_rate_delete(index: &dyn VaultIndex, a: &VaultRow) {
        let lim = limits();
        let id = a.id;

        // Chunks: a number at most the count; the count is capped.
        assert_eq!(
            reserve_at(index, &id, ObjectAddr::Chunk(1), 1, 10, NOW, &lim).await,
            ReserveOutcome::ChunkOutOfRange
        );
        for n in 0..3 {
            write(index, &id, ObjectAddr::Chunk(n), 1, 10, &lim).await;
        }
        assert_eq!(
            reserve_at(index, &id, ObjectAddr::Chunk(3), 1, 10, NOW, &lim).await,
            ReserveOutcome::ChunkOutOfRange
        );
        // An existing chunk takes a new version.
        write(index, &id, ObjectAddr::Chunk(1), 2, 20, &lim).await;
        assert_eq!(
            index
                .object(&id, ObjectAddr::Chunk(1))
                .await
                .unwrap()
                .unwrap()
                .version,
            2
        );
        // A released first reservation of a chunk forgets the chunk number.
        let second = vault_row(3);
        index.claim(second.clone()).await.unwrap();
        let t = reserve_ok(index, &second.id, ObjectAddr::Chunk(0), 1, 10, &lim).await;
        index
            .release(&second.id, ObjectAddr::Chunk(0), &t)
            .await
            .unwrap();
        assert_eq!(
            reserve_at(index, &second.id, ObjectAddr::Chunk(1), 1, 10, NOW, &lim).await,
            ReserveOutcome::ChunkOutOfRange
        );

        // Bytes: committed plus live reservations, the replaced object's
        // size freed. Vault `a` holds 10 (head) + 10 + 20 + 10 (chunks).
        assert_eq!(index.vault(&id).await.unwrap().unwrap().bytes_used, 50);
        assert_eq!(
            reserve_at(index, &id, ObjectAddr::Head, 5, 961, NOW, &lim).await,
            ReserveOutcome::VaultFull
        );
        let pending = reserve_ok(index, &id, ObjectAddr::Chunk(0), 2, 500, &lim).await;
        assert_eq!(
            reserve_at(index, &id, ObjectAddr::Head, 5, 461, NOW, &lim).await,
            ReserveOutcome::VaultFull
        );
        index
            .release(&id, ObjectAddr::Chunk(0), &pending)
            .await
            .unwrap();
        write(index, &id, ObjectAddr::Head, 5, 950, &lim).await;
        assert_eq!(index.vault(&id).await.unwrap().unwrap().bytes_used, 990);

        // Write rate: a refused write costs nothing; the hour's allowance
        // ends at the limit and comes back the next hour.
        let third = vault_row(4);
        index.claim(third.clone()).await.unwrap();
        for v in 1..=u64::from(lim.max_writes_per_hour) {
            write(index, &third.id, ObjectAddr::Head, v, 1, &lim).await;
        }
        let next = u64::from(lim.max_writes_per_hour) + 1;
        assert_eq!(
            reserve_at(index, &third.id, ObjectAddr::Head, next, 1, NOW, &lim).await,
            ReserveOutcome::RateLimited
        );
        let next_hour = (NOW / 3_600_000 + 1) * 3_600_000;
        assert!(matches!(
            reserve_at(index, &third.id, ObjectAddr::Head, next, 1, next_hour, &lim).await,
            ReserveOutcome::Reserved(_)
        ));

        // Delete: the vault and its objects go; the id can be claimed again.
        assert!(index.delete_vault(&id).await.unwrap());
        assert!(!index.delete_vault(&id).await.unwrap());
        assert_eq!(index.vault(&id).await.unwrap(), None);
        assert_eq!(index.object(&id, ObjectAddr::Head).await.unwrap(), None);
        let mut again = a.clone();
        again.write_pk = [9; 32];
        assert_eq!(index.claim(again).await.unwrap(), ClaimOutcome::Created);
        assert_eq!(index.vault(&id).await.unwrap().unwrap().bytes_used, 0);

        // Unclaim removes only a vault with no object.
        assert!(index.unclaim(&id).await.unwrap());
        assert!(!index.unclaim(&third.id).await.unwrap());
        for v in [&second.id, &third.id] {
            index.delete_vault(v).await.unwrap();
        }
    }

    /// Many concurrent reservations of one version: exactly one wins.
    pub(crate) async fn run_vault_race<I: VaultIndex + 'static>(index: Arc<I>) {
        let l = limits();
        let a = vault_row(5);
        index.claim(a.clone()).await.unwrap();
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let index = index.clone();
            let id = a.id;
            tasks.push(tokio::spawn(async move {
                reserve_at(index.as_ref(), &id, ObjectAddr::Head, 1, 10, NOW, &l).await
            }));
        }
        let mut won = 0;
        for t in tasks {
            match t.await.unwrap() {
                ReserveOutcome::Reserved(_) => won += 1,
                ReserveOutcome::VersionConflict => {},
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(won, 1);
        index.delete_vault(&a.id).await.unwrap();
    }

    pub(crate) fn row(seed: u8, day: u32, key: Option<u8>) -> BlobRow {
        BlobRow {
            id: [seed; 32],
            size: u64::from(seed) * 10,
            node: "local".to_string(),
            created_day: day,
            last_access_day: day,
            delete_pk: key.map(|k| [k; 32]),
            idle_days: None,
        }
    }

    /// Exercise every method once. `base` offsets the ids so a persistent
    /// database can be reused across runs.
    pub(crate) async fn run(index: &dyn BlobIndex, base: u8) {
        let a = row(base, 100, Some(7));
        let b = row(base.wrapping_add(1), 100, None);
        let _ = index.remove(&a.id).await.unwrap();
        let _ = index.remove(&b.id).await.unwrap();

        assert_eq!(index.get(&a.id).await.unwrap(), None);
        assert_eq!(
            index.record_upload(a.clone()).await.unwrap(),
            UploadOutcome::Created
        );
        assert_eq!(index.get(&a.id).await.unwrap(), Some(a.clone()));

        // A re-upload keeps the first key and creation day, raises the access.
        let mut again = a.clone();
        again.created_day = 105;
        again.last_access_day = 105;
        again.delete_pk = Some([9; 32]);
        assert_eq!(
            index.record_upload(again).await.unwrap(),
            UploadOutcome::Existed
        );
        let got = index.get(&a.id).await.unwrap().unwrap();
        assert_eq!(got.delete_pk, Some([7; 32]));
        assert_eq!(got.created_day, 100);
        assert_eq!(got.last_access_day, 105);

        // Touch never lowers the access day and ignores unknown ids.
        index.touch(&[a.id, [0xEE; 32]], 103).await.unwrap();
        assert_eq!(
            index.get(&a.id).await.unwrap().unwrap().last_access_day,
            105
        );
        index.touch(&[a.id], 110).await.unwrap();
        assert_eq!(
            index.get(&a.id).await.unwrap().unwrap().last_access_day,
            110
        );

        index.record_upload(b.clone()).await.unwrap();
        // Today 111 with a 10-day default life: `b` (access 100) is past it,
        // `a` (access 110) is not.
        let expired = index.expired("local", 111, 10, 100).await.unwrap();
        assert!(expired.contains(&b.id));
        assert!(!expired.contains(&a.id));
        assert!(!index
            .expired("elsewhere", 111, 10, 100)
            .await
            .unwrap()
            .contains(&b.id));

        // The conditional removal spares a row touched since.
        assert!(!index.remove_if_expired(&a.id, 111, 10).await.unwrap());
        assert!(index.remove_if_expired(&b.id, 111, 10).await.unwrap());
        assert_eq!(index.get(&b.id).await.unwrap(), None);

        // ADR-0087 section 13: a row's own idle life replaces the default,
        // shorter or longer, and a re-upload keeps the first one.
        let mut short = row(base.wrapping_add(2), 100, Some(5));
        short.idle_days = Some(3);
        let mut long = row(base.wrapping_add(3), 100, None);
        long.idle_days = Some(200);
        let _ = index.remove(&short.id).await.unwrap();
        let _ = index.remove(&long.id).await.unwrap();
        index.record_upload(short.clone()).await.unwrap();
        index.record_upload(long.clone()).await.unwrap();
        assert_eq!(index.get(&short.id).await.unwrap(), Some(short.clone()));
        let mut again = short.clone();
        again.idle_days = None;
        index.record_upload(again).await.unwrap();
        assert_eq!(
            index.get(&short.id).await.unwrap().unwrap().idle_days,
            Some(3)
        );
        // Day 103: `short` (100 + 3) is still in its life, day 104 it is not;
        // `long` outlives the 10-day default.
        let on_103 = index.expired("local", 103, 10, 100).await.unwrap();
        assert!(!on_103.contains(&short.id));
        let on_111 = index.expired("local", 111, 10, 100).await.unwrap();
        assert!(on_111.contains(&short.id));
        assert!(!on_111.contains(&long.id));
        assert!(!index.remove_if_expired(&short.id, 103, 10).await.unwrap());
        assert!(!index.remove_if_expired(&long.id, 111, 10).await.unwrap());
        assert!(index.remove_if_expired(&short.id, 104, 10).await.unwrap());
        assert!(index.remove_if_expired(&long.id, 301, 10).await.unwrap());

        assert!(index.remove(&a.id).await.unwrap());
        assert!(!index.remove(&a.id).await.unwrap());
    }
}
