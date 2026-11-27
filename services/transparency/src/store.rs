// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Persistent wrapper around the crate's in-memory append-only log.
//!
//! [`LogStore`] holds a [`InMemoryLog`] behind a [`tokio::sync::Mutex`]
//! so concurrent appends serialize and the in-memory Merkle tree never
//! races itself. The DB ([`log_entries`]) is the durable source of
//! truth: on boot we replay every row in `idx` order to rebuild the
//! identical tree, and every append writes the new leaf row so the tree
//! survives a restart.
//!
//! ## Load-on-boot
//!
//! [`LogStore::load`] SELECTs all `log_entries` ORDER BY `idx`, appends
//! each into a fresh `InMemoryLog`, and **asserts** the index the log
//! returns equals the DB `idx`. A mismatch means the persisted rows are
//! not a contiguous 0-based sequence (a gap, a duplicate, or an
//! out-of-order write) — the Merkle tree would be silently forked from
//! what clients have already proven inclusion against, so we fail loud
//! rather than serve a divergent root.
//!
//! ## Append consistency model
//!
//! We must append in-memory FIRST to learn the index (the crate's
//! `append` allocates it), then INSERT the row carrying that exact
//! index. The two steps are therefore NOT atomic: if the in-memory
//! append succeeds but the DB INSERT fails, the in-memory tree has a
//! leaf the DB does not. That leaves the process in a state where its
//! live root commits to an entry that will VANISH on the next restart
//! (the replay won't see it). We treat that as fatal-ish: we log an
//! `error` and return the failure to the caller (→ HTTP 500). The
//! operator should restart the process, which re-syncs the in-memory
//! tree to the durable DB state. A future hardening could hold the row
//! INSERT inside the same lock as a write-ahead step, but the crate's
//! API hands the index out only on append, so first-write-to-DB is not
//! possible without duplicating the index-allocation logic here.

use lakko_attest::verify_genesis_fresh;
use lakko_transparency::{
    verify_leaf_genesis, verify_supersession, AnnouncementLeaf, InMemoryLog, InclusionProof,
    LogEntry, SignedRoot, TreeHash, WITNESS_COSIG_LEN,
};
use sqlx::MySqlPool;
use tokio::sync::Mutex;

use lakko_crypto::signing;

use crate::witness_client::WitnessClient;

/// Errors the store surfaces to the HTTP layer.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StoreError {
    /// A database error during load or append.
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    /// The persisted rows are not a contiguous 0-based `idx` sequence —
    /// the log is corrupt and the tree cannot be faithfully rebuilt.
    #[error("log corruption: row at db idx {db_idx} mapped to log index {log_idx}")]
    Corrupt {
        /// The `idx` column value of the offending row.
        db_idx: u64,
        /// The index the in-memory log returned for it.
        log_idx: u64,
    },
    /// A wire error from the underlying crate (e.g. index out of bounds
    /// on a proof request).
    #[error("transparency log error: {0}")]
    Log(#[from] lakko_transparency::Error),
    /// The leaf authorisation (G-30 #2, ADR-0019) for a NEW binding was
    /// absent or did not verify — the operator refuses to append a
    /// fabricated / unauthorised leaf. Maps to HTTP 403.
    #[error("leaf authorisation rejected: {0}")]
    Unauthorised(&'static str),
    /// An announcement whose `seq` is not above every one the account has
    /// logged (ADR-0101 section 10). Maps to HTTP 409.
    #[error("announcement seq is not above the logged ones")]
    Stale,
}

/// One replayed `log_entries` row: `(idx, username_hash,
/// identity_pubkey, version, is_reset)`. `is_reset` BOOLEAN comes back
/// as MariaDB `TINYINT(1)` → `i8`.
type LeafRow = (u64, Vec<u8>, Vec<u8>, u64, i8);

/// One `latest`-query row: `(idx, identity_pubkey, version, is_reset,
/// leaf_auth, genesis_attest, reset_seal)`. `is_reset` → `i8`; the sidecars
/// VARBINARY NULL → `Option<Vec<u8>>` (G-30 #2 / ADR-0036 / ADR-0101).
type LatestRow = (
    u64,
    Vec<u8>,
    u64,
    i8,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
);

/// One replayed `log_announcements` row: `(idx, username_hash,
/// key_generation, seq, announcement_hash)` (ADR-0101 section 10).
type AnnouncementRow = (u64, Vec<u8>, u64, u64, Vec<u8>);

/// The inputs to the ADR-0036 S4 auth-attested genesis gate, bundled so the
/// [`LogStore::append`] argument list stays within bounds.
///
/// A genesis/reset leaf (no prior entry, or an ADR-0017 `is_reset` mint) may
/// carry an account-authenticated `GenesisAttestation` proving the party that
/// OPAQUE-authenticated as the account presented THIS pubkey as its genesis
/// key. When [`required`](Self::required) is set the gate REFUSES such a leaf
/// unless a valid attestation is present (fail-closed, invariant #4); rotation
/// leaves are unaffected (their supersession proof already binds them).
#[derive(Clone, Copy, Default)]
pub(crate) struct GenesisAttestGate<'a> {
    /// The opaque genesis-attestation blob supplied with the append, if any.
    /// Stored verbatim in the `genesis_attest` sidecar regardless of the gate.
    pub(crate) attestation: Option<&'a [u8]>,
    /// The pinned auth attest verifying key
    /// (`LAKKO_TRANSPARENCY_GENESIS_ATTEST_KEY`), or `None` when unconfigured.
    pub(crate) key: Option<&'a signing::VerifyingKey>,
    /// `LAKKO_TRANSPARENCY_REQUIRE_GENESIS_ATTEST` — when `true`, a genesis/reset
    /// leaf without a valid attestation is refused.
    pub(crate) required: bool,
}

/// The outcome of a successful [`LogStore::append`] — the assigned leaf
/// index and the server-assigned per-username version (KT-log S2
/// decision B).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AppendResult {
    /// The 0-based leaf index the entry was appended at.
    pub(crate) idx: u64,
    /// The monotonic per-`username_hash` version assigned to this entry
    /// (`>= 1`).
    pub(crate) version: u64,
}

/// The latest (highest-version) log entry for a given `username_hash`
/// (KT-log S3a). What a client needs to learn the current identity for
/// an account-key and then locate its leaf for an inclusion proof: the
/// leaf `idx`, the assigned `version`, the `identity_pubkey`, and the
/// `is_reset` fresh-start marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LatestEntry {
    /// The 0-based leaf index of the latest entry (== `/proof` idx).
    pub(crate) idx: u64,
    /// The monotonic per-`username_hash` version of the latest entry
    /// (`>= 1`).
    pub(crate) version: u64,
    /// The identity public key the account currently publishes.
    pub(crate) identity_pubkey: Vec<u8>,
    /// ADR-0017 fresh-start marker — `true` if the latest entry was a
    /// no-supersession-proof owner reset.
    pub(crate) is_reset: bool,
    /// G-30 #2 (ADR-0019): the opaque client leaf authorisation stored
    /// with this entry (a genesis self-sig or a supersession proof), or
    /// `None` for a pre-Sprint-2 / unauthenticated row. Served so a client
    /// can verify the genesis self-sig on a first observation.
    pub(crate) leaf_auth: Option<Vec<u8>>,
    /// ADR-0036 S4: the opaque auth-attested genesis attestation stored with
    /// this entry, or `None` for a rotation / a pre-ADR-0036 row. Served so a
    /// client can verify the account-authenticated genesis binding on a first
    /// observation (`verify_genesis_binding`).
    pub(crate) genesis_attest: Option<Vec<u8>>,
    /// ADR-0101 section 10: the reset seal stored with a reset leaf, or
    /// `None` (an unsealed reset, or not a reset).
    pub(crate) reset_seal: Option<Vec<u8>>,
}

/// The newest seal-announcement leaf of one account and generation
/// (ADR-0101 section 10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AnnouncementEntry {
    /// The leaf index (== `/proof` idx).
    pub(crate) idx: u64,
    /// The generation it was made at.
    pub(crate) key_generation: u64,
    /// Its `seq`.
    pub(crate) seq: u64,
    /// The announcement bytes as appended.
    pub(crate) announcement: Vec<u8>,
}

/// The last witness-co-signed root + its co-signature BUNDLE (ADR-0020 Sprint 4;
/// ADR-0039 S3 K-of-N). Only the `(tree_root, log_size)` bind the bundle (the
/// served root is re-signed fresh each request — the operator signature carries
/// a timestamp, the co-signatures do not), so that pair + the opaque co-sig
/// blobs (one per witness that co-signed this `(tree_root, log_size)`) is all we
/// keep. Each blob is a fixed [`WITNESS_COSIG_LEN`] bytes.
#[derive(Clone)]
struct Witnessed {
    tree_root: TreeHash,
    log_size: u64,
    cosigs: Vec<Vec<u8>>,
}

/// Persistent append-only transparency log.
pub(crate) struct LogStore {
    inner: Mutex<InMemoryLog>,
    /// The last witness-co-signed root, cached + DB-backed (ADR-0020 Sprint 4).
    witnessed: Mutex<Option<Witnessed>>,
}

impl LogStore {
    /// Rebuild the log from the durable `log_entries` rows, signing
    /// future roots with `operator`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Db`] on a query failure, or
    /// [`StoreError::Corrupt`] if the rows are not a contiguous 0-based
    /// `idx` sequence (the replay would fork the tree).
    pub(crate) async fn load(
        pool: &MySqlPool,
        operator: signing::SigningKey,
    ) -> Result<Self, StoreError> {
        // Runtime (not compile-time-checked) query — the service builds
        // without a live DB, same posture as the discovery service.
        let rows: Vec<LeafRow> = sqlx::query_as(
            "SELECT idx, username_hash, identity_pubkey, version, is_reset \
             FROM log_entries ORDER BY idx ASC",
        )
        .fetch_all(pool)
        .await?;
        // ADR-0101 section 10: the seal-announcement leaves share the index
        // sequence; replay both kinds in index order.
        let announcements: Vec<AnnouncementRow> = sqlx::query_as(
            "SELECT idx, username_hash, key_generation, seq, announcement_hash \
             FROM log_announcements ORDER BY idx ASC",
        )
        .fetch_all(pool)
        .await?;
        let mut identities = rows.into_iter().peekable();
        let mut announced = announcements.into_iter().peekable();

        let mut log = InMemoryLog::new(operator);
        loop {
            let take_identity = match (identities.peek(), announced.peek()) {
                (None, None) => break,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (Some(i), Some(a)) => i.0 < a.0,
            };
            let (db_idx, assigned) = if take_identity {
                let Some((db_idx, username_hash, identity_pubkey, version, _)) = identities.next()
                else {
                    break;
                };
                let entry = LogEntry {
                    username_hash: clamp_hash(&username_hash),
                    identity_pubkey,
                    version,
                };
                (db_idx, log.append(entry))
            } else {
                let Some((db_idx, username_hash, key_generation, seq, hash)) = announced.next()
                else {
                    break;
                };
                let leaf = AnnouncementLeaf {
                    username_hash: clamp_hash(&username_hash),
                    key_generation,
                    seq,
                    announcement_hash: clamp_hash(&hash),
                };
                (db_idx, log.append_announcement(&leaf))
            };
            if assigned != db_idx {
                // The replay must reconstruct the SAME index sequence the
                // rows were written under; otherwise the Merkle leaf
                // positions shift and the rebuilt root diverges.
                return Err(StoreError::Corrupt {
                    db_idx,
                    log_idx: assigned,
                });
            }
        }

        tracing::info!(size = log.size(), "transparency log rebuilt from DB");
        crate::metrics::log_size(log.size());

        // ADR-0020 Sprint 4: restore the last witness-co-signed root, so a
        // restart serves the same co-signed `(tree_root, log_size)` without
        // re-co-signing from scratch.
        let witnessed = load_witnessed(pool).await?;
        if let Some(w) = &witnessed {
            tracing::info!(
                log_size = w.log_size,
                cosig_count = w.cosigs.len(),
                "restored witnessed root co-signature bundle"
            );
            crate::metrics::cosigned(w.log_size, false);
        }

        Ok(Self {
            inner: Mutex::new(log),
            witnessed: Mutex::new(witnessed),
        })
    }

    /// Construct a store directly over an already-built log. Test +
    /// in-memory-only helper; production goes through [`LogStore::load`].
    #[cfg(test)]
    pub(crate) fn from_log(log: InMemoryLog) -> Self {
        Self {
            inner: Mutex::new(log),
            witnessed: Mutex::new(None),
        }
    }

    /// Append a leaf for `username_hash` → `identity_pubkey` to the log
    /// and persist the leaf row. The transparency service ASSIGNS the
    /// `version` (KT-log S2 decision B): the caller does NOT supply it.
    ///
    /// ## Idempotency (KT-log S2.5)
    ///
    /// Under the same `Mutex` lock, BEFORE appending we query the LATEST
    /// entry for this `username_hash`. If one exists AND its
    /// `identity_pubkey` already equals the requested pubkey, this is a
    /// re-append of the already-current identity (a retry, or the S2.5
    /// reconciliation sweep re-asserting a key that was actually logged)
    /// — we RETURN that existing `(idx, version)` WITHOUT appending, so a
    /// sweep can never create a duplicate leaf. Only a DIFFERENT pubkey
    /// (a genuine new version) appends.
    ///
    /// Otherwise, still under the same lock that serialises the in-memory
    /// append, we query `MAX(version)` for this `username_hash` and use
    /// `max + 1` (so versions are monotonic per `username_hash`, assigned
    /// atomically — two concurrent appends for the same username can
    /// never collide because the lock serialises them and the
    /// `MAX`-query reflects the loser's committed row).
    ///
    /// Then we append in-memory to learn the index and INSERT the row
    /// with that exact index. See the module docs for the consistency
    /// model: a DB failure after the in-memory append is logged at
    /// `error` and returned (→ 500); the in-memory and durable state are
    /// then out of sync until a restart re-syncs them.
    ///
    /// Returns the assigned (or, on a no-op re-append, the already-stored)
    /// `(idx, version)`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Db`] if the latest-entry query, the
    /// `MAX`-query, or the INSERT fails, or [`StoreError::Unauthorised`] if the
    /// leaf authorisation or (ADR-0036 S4) the genesis attestation is rejected.
    #[cfg(test)]
    pub(crate) async fn append(
        &self,
        pool: &MySqlPool,
        username_hash: [u8; 32],
        identity_pubkey: Vec<u8>,
        is_reset: bool,
        leaf_auth: Option<&[u8]>,
        genesis: GenesisAttestGate<'_>,
        now_unix_secs: i64,
    ) -> Result<AppendResult, StoreError> {
        self.append_with_seal(
            pool,
            username_hash,
            identity_pubkey,
            is_reset,
            leaf_auth,
            genesis,
            None,
            now_unix_secs,
        )
        .await
    }

    /// [`Self::append`] with the reset seal sidecar of ADR-0101 section 10:
    /// on a reset leaf (`is_reset`) the 214-byte seal is checked (its own
    /// signature, and that it seals exactly `identity_pubkey`) and stored in
    /// `log_entries.reset_seal`, never in the leaf's bytes; an invalid one is
    /// refused like an invalid leaf authorisation. Ignored on a non-reset
    /// leaf. Whether its seal key was announced is the peer's question.
    ///
    /// # Errors
    ///
    /// As [`Self::append`].
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(crate) async fn append_with_seal(
        &self,
        pool: &MySqlPool,
        username_hash: [u8; 32],
        identity_pubkey: Vec<u8>,
        is_reset: bool,
        leaf_auth: Option<&[u8]>,
        genesis: GenesisAttestGate<'_>,
        reset_seal: Option<&[u8]>,
        now_unix_secs: i64,
    ) -> Result<AppendResult, StoreError> {
        let reset_seal = reset_seal.filter(|_| is_reset);
        if let Some(seal) = reset_seal {
            let opened = lakko_attest::ResetSeal::open(seal)
                .map_err(|_| StoreError::Unauthorised("reset seal did not verify"))?;
            if opened.new_signature_pubkey.as_slice() != identity_pubkey.as_slice() {
                return Err(StoreError::Unauthorised("reset seal names another key"));
            }
        }
        let mut log = self.inner.lock().await;

        // KT-log S2.5 idempotency: read the latest entry for this
        // username_hash FIRST, under the same lock. If the already-latest
        // identity matches the requested pubkey, this is a no-op
        // re-append (a retry / a reconciliation sweep) — return the
        // existing coordinates without writing a duplicate leaf. The query
        // runs against the DB (the durable source of truth) so it sees the
        // committed history even for a freshly-restarted process.
        let latest: Option<(Vec<u8>, u64, u64)> = sqlx::query_as(
            "SELECT identity_pubkey, idx, version FROM log_entries \
             WHERE username_hash = ? ORDER BY version DESC LIMIT 1",
        )
        .bind(&username_hash[..])
        .fetch_optional(pool)
        .await?;
        if let Some((latest_pubkey, latest_idx, latest_version)) = &latest {
            if latest_pubkey.as_slice() == identity_pubkey.as_slice() {
                tracing::debug!(
                    idx = latest_idx,
                    version = latest_version,
                    "transparency append is a no-op (identity already latest) — \
                     returning existing coordinates"
                );
                crate::metrics::append(crate::metrics::AppendOutcome::Unchanged);
                return Ok(AppendResult {
                    idx: *latest_idx,
                    version: *latest_version,
                });
            }
        }

        // G-30 #2 (ADR-0019): a NEW binding (a pubkey different from the
        // current latest) must be AUTHORISED by the account. We verify the
        // `leaf_auth` when it is present; the handler's presence policy
        // (`LAKKO_TRANSPARENCY_REQUIRE_LEAF_AUTH`) governs whether an ABSENT
        // value is refused. This is the operator-side defence-in-depth reject
        // — the client rotation chain stays the real gate (the server is
        // untrusted), but a self-respecting operator never appends a
        // fabricated / unauthorised leaf.
        //
        //   * genesis (no prior entry) OR a fresh-start reset (`is_reset` —
        //     the old key is gone, so no supersession proof can exist): a
        //     SELF-signature by the leaf's OWN key over the
        //     `(account_key, pubkey)` binding.
        //   * a normal rotation (a prior entry exists, not a reset): a
        //     SUPERSESSION proof signed by the PREVIOUS key, authorising
        //     EXACTLY the new key.
        if let Some(auth) = leaf_auth {
            match latest.as_ref() {
                Some((prev_pubkey, _, _)) if !is_reset => {
                    let prev: [u8; 32] = prev_pubkey
                        .as_slice()
                        .try_into()
                        .map_err(|_| StoreError::Unauthorised("previous pubkey is not 32 bytes"))?;
                    let authorised =
                        verify_supersession(auth, &prev, now_unix_secs).map_err(|_| {
                            StoreError::Unauthorised("supersession proof did not verify")
                        })?;
                    if authorised.as_slice() != identity_pubkey.as_slice() {
                        return Err(StoreError::Unauthorised(
                            "supersession proof authorises a different key",
                        ));
                    }
                },
                _ => {
                    verify_leaf_genesis(auth, &username_hash, &identity_pubkey).map_err(|_| {
                        StoreError::Unauthorised("genesis self-signature did not verify")
                    })?;
                },
            }
        }

        // ADR-0036 S4: auth-attested genesis gate (defence-in-depth, flag-gated).
        // A genesis (no prior entry) or fresh-start reset leaf must — when the
        // flag is ON — carry a valid `GenesisAttestation` minted by the account's
        // OPAQUE-authenticated session, binding the served `(username_hash,
        // pubkey)` and still fresh. Only the account holder can mint one, so this
        // turns first-entry squatting from DETECTED (ADR-0019's self-sig above,
        // which STAYS as the independent belt-and-suspenders proof) into
        // PREVENTED. A rotation is untouched — its supersession proof already
        // binds the predecessor key. Fail-closed (invariant #4): under the flag,
        // an absent attestation, an unconfigured pin key, a non-32-byte pubkey, or
        // an invalid / expired attestation all refuse the append.
        let is_genesis_or_reset = latest.is_none() || is_reset;
        if genesis.required && is_genesis_or_reset {
            let attestation = genesis
                .attestation
                .ok_or(StoreError::Unauthorised("genesis attestation required"))?;
            let key = genesis.key.ok_or(StoreError::Unauthorised(
                "genesis attestation verifying key not configured",
            ))?;
            let pubkey: [u8; 32] = identity_pubkey.as_slice().try_into().map_err(|_| {
                StoreError::Unauthorised(
                    "identity pubkey is not 32 bytes for a genesis attestation",
                )
            })?;
            verify_genesis_fresh(attestation, key, &username_hash, &pubkey, now_unix_secs)
                .map_err(|_| StoreError::Unauthorised("genesis attestation did not verify"))?;
        }

        // Assign the version: latest existing for this username_hash, + 1.
        // We already hold the latest row above; `latest_version + 1` (or 1
        // for a brand-new username) is the next version. Run under the lock
        // so the assignment is race-free against a concurrent append for
        // the same username.
        let version = latest.map_or(1, |(_, _, v)| v.saturating_add(1));

        let entry = LogEntry {
            username_hash,
            identity_pubkey,
            version,
        };

        // Allocate the index by appending in-memory.
        let idx = log.append(entry.clone());

        // Persist the leaf. If this fails the in-memory tree now holds a
        // leaf the DB does not — fatal-ish, see module docs.
        let insert = sqlx::query(
            "INSERT INTO log_entries \
             (idx, username_hash, identity_pubkey, version, is_reset, leaf_auth, genesis_attest, \
              reset_seal) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(idx)
        .bind(&entry.username_hash[..])
        .bind(&entry.identity_pubkey[..])
        .bind(entry.version)
        .bind(is_reset)
        .bind(leaf_auth)
        .bind(genesis.attestation)
        .bind(reset_seal)
        .execute(pool)
        .await;

        if let Err(e) = insert {
            tracing::error!(
                idx,
                error = %e,
                "FATAL: log_entries INSERT failed after in-memory append — \
                 in-memory tree and DB are now out of sync; restart to re-sync"
            );
            return Err(StoreError::Db(e));
        }

        crate::metrics::append(crate::metrics::AppendOutcome::Appended);
        crate::metrics::log_size(log.size());
        Ok(AppendResult { idx, version })
    }

    /// Sign the current root with the operator key at `timestamp_unix`.
    pub(crate) async fn signed_root(&self, timestamp_unix: u64) -> SignedRoot {
        let log = self.inner.lock().await;
        log.sign_current_root(timestamp_unix)
    }

    /// Sign the current head root and (when the witness integration is on)
    /// attach the witness co-signature BUNDLE (ADR-0020 Sprint 4; ADR-0039 S3
    /// K-of-N; ADR-0039 S3b per-witness catch-up).
    ///
    /// Lazy co-signing: an UNCHANGED head serves the cached bundle straight back
    /// (it binds the same `(tree_root, log_size)` and so stays valid for the
    /// freshly-signed root — no DB, no network). On a GROWTH round it fans out to
    /// EVERY configured witness and caches the collected bundle.
    ///
    /// ## Per-witness catch-up (ADR-0039 S3b — anti-stranding)
    ///
    /// Each witness is handed a consistency proof from ITS OWN last-co-signed
    /// high-water (`witness_high_water`), NEVER a single shared base. A witness
    /// that was DOWN during a partial-success round keeps its old high-water, so
    /// the NEXT growth re-sends it a proof from that still-old position and it
    /// auto-rejoins — the S3 shared-base fan-out would instead have advanced the
    /// one base past the down witness on the first partial success, permanently
    /// stranding it (its own high-water forever behind the base it was proved
    /// from). Only a witness that successfully co-signs this round advances its
    /// row (to the current head). A fresh witness (no row) starts from the
    /// genesis base `0`, exactly the pre-S3 first-cosig posture.
    ///
    /// The N `cosign` calls are fanned out CONCURRENTLY (ADR-0039 F1 —
    /// `futures::future::join_all`) and each is TIMEOUT-bounded (the witness
    /// client's connect + total deadline), so a witness that accepts-but-stalls
    /// can no longer serialise / block this hot read path (with N witnesses the
    /// latencies previously added up sequentially). Each `cosign` is BEST-EFFORT
    /// — a failure (unreachable, a timeout, a 409 fork-refusal, a decode error,
    /// or a wrong-length blob) means that witness contributes NOTHING this round
    /// (logged at `warn`, witness index + kind only) while the others still
    /// contribute.
    ///
    /// Returns `(signed_root, cosigs)` — a `Vec` of 0..N co-signatures. An EMPTY
    /// bundle (no witness configured, all witnesses down/refused this round, or a
    /// high-water load failure) is served as-is: the CLIENT enforces the K-of-N
    /// threshold and fails closed (ADR-0039), the server never blocks/waits and
    /// never serves a STALE bundle (a partial/empty round is not cached against a
    /// mismatched head, so the fan-out is re-run on the next growth).
    pub(crate) async fn root_with_cosig(
        &self,
        pool: &MySqlPool,
        witnesses: &[WitnessClient],
        timestamp_unix: u64,
    ) -> (SignedRoot, Vec<Vec<u8>>) {
        let cached = self.witnessed.lock().await.clone();

        // Cheap head snapshot for the cache-hit fast path — no DB, no network.
        let (head_root, head_size) = {
            let log = self.inner.lock().await;
            (log.sign_current_root(timestamp_unix), log.size())
        };

        // The head is still at the cached size + root → the cached bundle binds
        // the same `(tree_root, log_size)`, valid for the fresh root. Serve it
        // without touching the DB or any witness (the pre-S3 cache-hit posture,
        // generalised over the bundle).
        if let Some(w) = &cached {
            if w.log_size == head_size && w.tree_root == head_root.tree_root {
                return (head_root, w.cosigs.clone());
            }
        }

        // The head grew (or nothing co-signed yet) — fan out to every witness,
        // best-effort. No witness configured → the dormant (empty-bundle) path.
        if witnesses.is_empty() {
            return (head_root, Vec::new());
        }

        // ADR-0039 S3b — load each witness's own catch-up base. If the load fails
        // we cannot build correct per-witness proofs, so serve an empty bundle
        // this round (fail-safe — the client fails closed on the threshold, the
        // next request retries) rather than fan out proofs from a wrong base.
        let bases = match load_witness_high_water_sizes(pool, witnesses.len()).await {
            Ok(bases) => bases,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "load witness high-waters failed — serving an empty co-signature bundle this round"
                );
                return (head_root, Vec::new());
            },
        };

        // Snapshot the head + build every per-witness proof under ONE lock so the
        // signed root and all proofs agree on the same head size, then DROP it
        // before any witness round-trip (appends never block on the network).
        let (head_root, head_size, proofs) = {
            let log = self.inner.lock().await;
            let head_root = log.sign_current_root(timestamp_unix);
            let head_size = log.size();
            let proofs: Vec<Vec<TreeHash>> = bases
                .iter()
                .map(|&base| {
                    if head_size > base {
                        log.consistency_proof(base)
                    } else {
                        Vec::new()
                    }
                })
                .collect();
            (head_root, head_size, proofs)
        };

        // ADR-0039 F1 — fan the N `cosign` calls out CONCURRENTLY (bounded by
        // each client's request timeout) and fold the index-ordered results,
        // advancing only the high-waters of the witnesses that co-signed (S3b
        // per-witness semantics unchanged). Extracted so this hot-path method
        // stays readable.
        let cosigs = fan_out_cosigs(pool, witnesses, &head_root, head_size, &proofs).await;

        crate::metrics::witnesses_answered(cosigs.len());
        if cosigs.is_empty() {
            // Every witness was down/refused — serve an empty bundle (the client
            // fails closed on the threshold). Do NOT cache: no witnessed row is
            // written so the next request re-runs the fan-out.
            tracing::warn!(
                head_size,
                witness_count = witnesses.len(),
                "no witness co-signed the head — serving an empty co-signature bundle"
            );
            return (head_root, Vec::new());
        }

        // Cache + persist the (partial or full) bundle at the head. A partial
        // bundle is reused until the next growth (a cache-hit reuse, matching the
        // pre-S3 single-witness behaviour); the missing witnesses re-run then.
        if let Err(e) = persist_witnessed(pool, &head_root.tree_root, head_size, &cosigs).await {
            tracing::error!(error = %e, "persist witnessed bundle failed (cached in-memory)");
        }
        crate::metrics::cosigned(head_size, true);
        *self.witnessed.lock().await = Some(Witnessed {
            tree_root: head_root.tree_root,
            log_size: head_size,
            cosigs: cosigs.clone(),
        });
        (head_root, cosigs)
    }

    /// Build an inclusion proof for the entry at `idx`, returning the
    /// proven entry alongside it (the leaf's canonical bytes are what a
    /// client feeds to `verify_inclusion`).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Log`] if `idx >= size()`.
    #[cfg(test)]
    pub(crate) async fn proof(&self, idx: u64) -> Result<(LogEntry, InclusionProof), StoreError> {
        let log = self.inner.lock().await;
        let entry = log.entry(idx)?.clone();
        let proof = log.prove_inclusion(idx)?;
        Ok((entry, proof))
    }

    /// Look up the latest (highest-version) entry for `username_hash`
    /// (KT-log S3a). Read-only against the durable DB — no in-memory lock
    /// is taken because the query touches only committed rows (the source
    /// of truth) and never mutates the Merkle tree. Returns `None` when
    /// no entry for that account-key has ever been logged.
    ///
    /// The `idx_username (username_hash, version)` index covers the
    /// `ORDER BY version DESC LIMIT 1`, so this is a single index seek.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Db`] on a query failure.
    pub(crate) async fn latest(
        &self,
        pool: &MySqlPool,
        username_hash: [u8; 32],
    ) -> Result<Option<LatestEntry>, StoreError> {
        // `is_reset` BOOLEAN comes back as MariaDB `TINYINT(1)` → `i8`;
        // `leaf_auth` + `genesis_attest` VARBINARY NULL → `Option<Vec<u8>>`
        // (G-30 #2 / ADR-0036 S4).
        let row: Option<LatestRow> = sqlx::query_as(
            "SELECT idx, identity_pubkey, version, is_reset, leaf_auth, genesis_attest, \
                    reset_seal \
             FROM log_entries WHERE username_hash = ? ORDER BY version DESC LIMIT 1",
        )
        .bind(&username_hash[..])
        .fetch_optional(pool)
        .await?;
        Ok(row.map(
            |(idx, identity_pubkey, version, is_reset, leaf_auth, genesis_attest, reset_seal)| {
                LatestEntry {
                    idx,
                    version,
                    identity_pubkey,
                    is_reset: is_reset != 0,
                    leaf_auth,
                    genesis_attest,
                    reset_seal,
                }
            },
        ))
    }

    /// The leaf bytes at `idx`, of either kind, and their inclusion proof
    /// against the current tree (what `GET /transparency/proof/{idx}` serves).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Log`] if `idx >= size()`.
    pub(crate) async fn leaf_proof(
        &self,
        idx: u64,
    ) -> Result<(Vec<u8>, InclusionProof), StoreError> {
        let log = self.inner.lock().await;
        let bytes = log.leaf_bytes(idx)?.to_vec();
        let proof = log.prove_inclusion(idx)?;
        Ok((bytes, proof))
    }

    /// Append an account's seal announcement as an announcement leaf
    /// (ADR-0101 section 10). Under the log's lock: the announcement must
    /// verify under the account's latest identity key in this log
    /// ([`StoreError::Unauthorised`] otherwise, and when the account has no
    /// identity leaf yet); the same announcement again returns its existing
    /// index (a retry); a `seq` not above every one the account has logged is
    /// [`StoreError::Stale`]. The leaf names the generation and `seq` the
    /// announcement carries and commits its bytes by hash.
    ///
    /// # Errors
    ///
    /// As above; [`StoreError::Db`].
    pub(crate) async fn append_announcement(
        &self,
        pool: &MySqlPool,
        username_hash: [u8; 32],
        announcement: &[u8],
    ) -> Result<u64, StoreError> {
        let mut log = self.inner.lock().await;
        let latest: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT identity_pubkey FROM log_entries \
             WHERE username_hash = ? ORDER BY version DESC LIMIT 1",
        )
        .bind(&username_hash[..])
        .fetch_optional(pool)
        .await?;
        let key = latest
            .as_deref()
            .and_then(|k| <[u8; 32]>::try_from(k).ok())
            .and_then(|k| signing::VerifyingKey::from_bytes(&k).ok())
            .ok_or(StoreError::Unauthorised(
                "no identity leaf for the announcement",
            ))?;
        let claims = lakko_attest::SealAnnouncement::open(announcement, &key)
            .map_err(|_| StoreError::Unauthorised("announcement did not verify"))?;
        let leaf = AnnouncementLeaf::for_announcement(
            username_hash,
            claims.key_generation,
            claims.seq,
            announcement,
        );
        let existing: Option<u64> = sqlx::query_scalar(
            "SELECT idx FROM log_announcements WHERE username_hash = ? AND announcement_hash = ?",
        )
        .bind(&username_hash[..])
        .bind(&leaf.announcement_hash[..])
        .fetch_optional(pool)
        .await?;
        if let Some(idx) = existing {
            crate::metrics::append(crate::metrics::AppendOutcome::Unchanged);
            return Ok(idx);
        }
        let highest: Option<u64> =
            sqlx::query_scalar("SELECT MAX(seq) FROM log_announcements WHERE username_hash = ?")
                .bind(&username_hash[..])
                .fetch_one(pool)
                .await?;
        if highest.is_some_and(|h| claims.seq <= h) {
            return Err(StoreError::Stale);
        }
        let idx = log.append_announcement(&leaf);
        let insert = sqlx::query(
            "INSERT INTO log_announcements \
             (idx, username_hash, key_generation, seq, announcement_hash, announcement) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(idx)
        .bind(&username_hash[..])
        .bind(claims.key_generation)
        .bind(claims.seq)
        .bind(&leaf.announcement_hash[..])
        .bind(announcement)
        .execute(pool)
        .await;
        if let Err(e) = insert {
            tracing::error!(
                idx,
                error = %e,
                "FATAL: log_announcements INSERT failed after in-memory append — \
                 in-memory tree and DB are now out of sync; restart to re-sync"
            );
            return Err(StoreError::Db(e));
        }
        crate::metrics::append(crate::metrics::AppendOutcome::Appended);
        crate::metrics::log_size(log.size());
        Ok(idx)
    }

    /// The newest announcement leaf (highest `seq`) of `username_hash` at
    /// `key_generation`, or `None` (ADR-0101 section 10). Read-only against
    /// the durable rows, like [`Self::latest`].
    ///
    /// # Errors
    ///
    /// [`StoreError::Db`].
    pub(crate) async fn announcement(
        &self,
        pool: &MySqlPool,
        username_hash: [u8; 32],
        key_generation: u64,
    ) -> Result<Option<AnnouncementEntry>, StoreError> {
        let row: Option<(u64, u64, Vec<u8>)> = sqlx::query_as(
            "SELECT idx, seq, announcement FROM log_announcements \
             WHERE username_hash = ? AND key_generation = ? ORDER BY seq DESC LIMIT 1",
        )
        .bind(&username_hash[..])
        .bind(key_generation)
        .fetch_optional(pool)
        .await?;
        Ok(row.map(|(idx, seq, announcement)| AnnouncementEntry {
            idx,
            key_generation,
            seq,
            announcement,
        }))
    }

    /// The operator's verifying key — clients pin this out-of-band.
    pub(crate) async fn operator_verifying_key(&self) -> signing::VerifyingKey {
        let log = self.inner.lock().await;
        log.operator_key()
    }

    /// Number of entries currently in the log.
    #[cfg(test)]
    pub(crate) async fn size(&self) -> u64 {
        let log = self.inner.lock().await;
        log.size()
    }
}

/// Coerce a stored 32-byte hash into the fixed array. A wrong length
/// here would be DB corruption (the column is `BINARY(32)`); we pad /
/// truncate defensively rather than panic, but in practice the column
/// constraint guarantees 32 bytes.
fn clamp_hash(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let n = bytes.len().min(32);
    out[..n].copy_from_slice(&bytes[..n]);
    out
}

/// Fan the witness `cosign` calls out CONCURRENTLY (ADR-0039 F1) then fold the
/// results, returning the collected 0..N co-signature bundle.
///
/// Each of the N calls is independent, timeout-bounded I/O over its OWN client,
/// so a witness that accepts-but-stalls can no longer serialise / block the hot
/// `GET /transparency/root` read path (with N witnesses the per-request
/// latencies previously added up sequentially). Every future borrows the local
/// `head_root` + its own `proofs[i]` slice immutably, so the fan-out is
/// borrow-clean; `join_all` preserves input order, so results come back
/// index-ordered (deterministic bundle order + logs).
///
/// AFTER the concurrent network phase the results are folded SEQUENTIALLY: for
/// each SUCCESS the witness's high-water advances to the head it just co-signed
/// and its cosig joins the bundle; a failure/timeout/wrong-length leaves that
/// witness's high-water UNTOUCHED (S3b anti-stranding — it re-proves from its
/// own base next growth and auto-rejoins) and contributes nothing. The DB
/// high-water writes stay sequential — they are cheap and this avoids pool
/// contention from N simultaneous upserts. A reqwest timeout surfaces in the
/// `Err` arm identically to a down/refusing witness (best-effort).
async fn fan_out_cosigs(
    pool: &MySqlPool,
    witnesses: &[WitnessClient],
    head_root: &SignedRoot,
    head_size: u64,
    proofs: &[Vec<TreeHash>],
) -> Vec<Vec<u8>> {
    let cosign_results = futures::future::join_all(witnesses.iter().enumerate().map(
        |(witness_index, witness)| {
            let proof = &proofs[witness_index];
            async move { (witness_index, witness.cosign(head_root, proof).await) }
        },
    ))
    .await;

    let mut cosigs: Vec<Vec<u8>> = Vec::with_capacity(witnesses.len());
    for (witness_index, result) in cosign_results {
        match result {
            Ok(cosig) if cosig.len() == WITNESS_COSIG_LEN => {
                crate::metrics::witness_cosign(crate::metrics::CosignOutcome::Ok);
                cosigs.push(cosig);
                // This witness co-signed the current head → advance ITS
                // high-water so its next proof starts from here. A persist
                // failure only means the next round re-proves from the OLD base
                // (a redundant-but-valid catch-up), never a strand.
                if let Err(e) = persist_witness_high_water(
                    pool,
                    u32::try_from(witness_index).unwrap_or(u32::MAX),
                    &head_root.tree_root,
                    head_size,
                )
                .await
                {
                    tracing::error!(
                        witness_index,
                        error = %e,
                        "persist witness high-water failed (advance deferred to the next round)"
                    );
                }
            },
            Ok(cosig) => {
                crate::metrics::witness_cosign(crate::metrics::CosignOutcome::BadResponse);
                // A wrong-length blob would break the fixed-chunk bundle framing
                // (and a client would reject it anyway) — drop it. Left the
                // high-water untouched → the witness re-proves next round.
                tracing::warn!(
                    witness_index,
                    cosig_len = cosig.len(),
                    "witness returned a wrong-length co-signature — dropping it from the bundle"
                );
            },
            Err(e) => {
                crate::metrics::witness_cosign(e.outcome());
                // Left this witness's high-water untouched → the next growth
                // re-sends a proof from its own (still-old) base and it
                // auto-rejoins. No permanent stranding (ADR-0039 S3b). A reqwest
                // timeout (ADR-0039 F1) surfaces here identically to a
                // down/refusing witness — best-effort, contributes nothing.
                tracing::warn!(
                    witness_index,
                    error = %e,
                    head_size,
                    "witness cosign failed — this witness contributes no co-signature this round"
                );
            },
        }
    }
    cosigs
}

/// UPSERT the single `witnessed_root` row with the last co-signed
/// `(tree_root, log_size, cosigs)` (ADR-0020 Sprint 4; ADR-0039 S3 K-of-N).
///
/// The bundle is stored as the concatenation of its fixed [`WITNESS_COSIG_LEN`]
/// co-signatures (the caller guarantees each is exactly that length), which
/// [`chunk_cosigs`] splits back unambiguously on load.
async fn persist_witnessed(
    pool: &MySqlPool,
    tree_root: &TreeHash,
    log_size: u64,
    cosigs: &[Vec<u8>],
) -> Result<(), sqlx::Error> {
    let blob: Vec<u8> = cosigs.concat();
    sqlx::query(
        "INSERT INTO witnessed_root (id, tree_root, log_size, cosigs) VALUES (1, ?, ?, ?) \
         ON DUPLICATE KEY UPDATE tree_root = VALUES(tree_root), \
         log_size = VALUES(log_size), cosigs = VALUES(cosigs)",
    )
    .bind(&tree_root[..])
    .bind(log_size)
    .bind(blob)
    .execute(pool)
    .await
    .map(|_| ())
}

/// Load the persisted witnessed root bundle, if any (ADR-0020 Sprint 4;
/// ADR-0039 S3 K-of-N). A pre-S3 single-cosig row (105 bytes) chunks to a
/// one-element bundle, so an existing DB restores transparently.
async fn load_witnessed(pool: &MySqlPool) -> Result<Option<Witnessed>, StoreError> {
    let row: Option<(Vec<u8>, u64, Vec<u8>)> =
        sqlx::query_as("SELECT tree_root, log_size, cosigs FROM witnessed_root WHERE id = 1")
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(tree_root, log_size, blob)| {
        if blob.len() % WITNESS_COSIG_LEN != 0 {
            // We only ever write whole multiples; a remainder is DB corruption.
            tracing::warn!(
                blob_len = blob.len(),
                "persisted witness cosig bundle length is not a multiple of the cosig size — \
                 trailing bytes ignored"
            );
        }
        Witnessed {
            tree_root: clamp_hash(&tree_root),
            log_size,
            cosigs: chunk_cosigs(&blob),
        }
    }))
}

/// Split a persisted concatenated cosig bundle back into individual fixed
/// [`WITNESS_COSIG_LEN`] co-signatures. A trailing partial chunk (only possible
/// under DB corruption — we only ever write whole multiples) is dropped.
fn chunk_cosigs(blob: &[u8]) -> Vec<Vec<u8>> {
    blob.chunks_exact(WITNESS_COSIG_LEN)
        .map(<[u8]>::to_vec)
        .collect()
}

/// Map the loaded `witness_high_water` rows to a positional base vector of length
/// `n` — index `i` is witness `i`'s last-co-signed `log_size`, `0` (the genesis
/// base, exactly the pre-S3 first-cosig posture) for a witness with no row. A row
/// whose `witness_index >= n` (the configured witness list shrank) is ignored.
///
/// This base is the per-witness catch-up start (ADR-0039 S3b): witness `i`'s
/// consistency proof is built from `bases[i]`, NEVER a single shared base, so a
/// witness that fell behind during a partial-success round is caught up from its
/// OWN position and is never permanently stranded.
fn high_water_bases(rows: &[(u32, u64)], n: usize) -> Vec<u64> {
    let mut bases = vec![0u64; n];
    for &(idx, size) in rows {
        if let Some(slot) = usize::try_from(idx).ok().and_then(|i| bases.get_mut(i)) {
            *slot = size;
        }
    }
    bases
}

/// Load the per-witness catch-up bases (ADR-0039 S3b): a positional `Vec<u64>` of
/// length `witness_count`, index `i` = witness `i`'s last-co-signed `log_size`
/// (`0` for a witness with no row). Reads only `(witness_index, log_size)` — the
/// `tree_root` is persisted for completeness but the proof base needs only the
/// size (the sub-tree root at that size is recomputed from the current tree).
async fn load_witness_high_water_sizes(
    pool: &MySqlPool,
    witness_count: usize,
) -> Result<Vec<u64>, sqlx::Error> {
    let rows: Vec<(u32, u64)> =
        sqlx::query_as("SELECT witness_index, log_size FROM witness_high_water")
            .fetch_all(pool)
            .await?;
    Ok(high_water_bases(&rows, witness_count))
}

/// UPSERT witness `witness_index`'s high-water to `(tree_root, log_size)` — the
/// head it just successfully co-signed (ADR-0039 S3b). Called ONLY on a
/// successful cosign; a failed witness's row is left untouched so its next proof
/// is built from its own still-old base and it auto-rejoins.
async fn persist_witness_high_water(
    pool: &MySqlPool,
    witness_index: u32,
    tree_root: &TreeHash,
    log_size: u64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO witness_high_water (witness_index, tree_root, log_size) VALUES (?, ?, ?) \
         ON DUPLICATE KEY UPDATE tree_root = VALUES(tree_root), log_size = VALUES(log_size)",
    )
    .bind(witness_index)
    .bind(&tree_root[..])
    .bind(log_size)
    .execute(pool)
    .await
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_transparency::verify_inclusion;

    fn make_entry(name: &str, version: u64) -> LogEntry {
        LogEntry {
            username_hash: lakko_crypto::hash::blake3(name.as_bytes()),
            identity_pubkey: vec![0xCC; 32],
            version,
        }
    }

    /// Append several entries to an in-memory-only store (no DB),
    /// then prove inclusion of each against the signed root.
    #[tokio::test]
    async fn append_proof_verify_round_trip_no_db() {
        let operator = signing::SigningKey::generate();
        let mut log = InMemoryLog::new(operator);
        for i in 0..8 {
            log.append(make_entry(&format!("user-{i}"), 1));
        }
        let store = LogStore::from_log(log);

        let root = store.signed_root(1_700_000_000).await;
        root.verify(&store.operator_verifying_key().await)
            .expect("root verifies under operator key");

        for i in 0..8 {
            let (entry, proof) = store.proof(i).await.expect("proof");
            verify_inclusion(&entry, &proof, &root).expect("inclusion verifies");
        }
        assert_eq!(store.size().await, 8);
    }

    /// The proof for index i must NOT verify a different entry — the
    /// store delegates to the crate's binding so this is really a
    /// guard that we hand back the right (entry, proof) pair.
    #[tokio::test]
    async fn proof_is_bound_to_its_entry() {
        let operator = signing::SigningKey::generate();
        let mut log = InMemoryLog::new(operator);
        log.append(make_entry("alice", 1));
        log.append(make_entry("bob", 1));
        let store = LogStore::from_log(log);

        let root = store.signed_root(1_700_000_000).await;
        let (_alice_entry, alice_proof) = store.proof(0).await.unwrap();
        let (bob_entry, _bob_proof) = store.proof(1).await.unwrap();
        // Bob's entry against Alice's proof must fail.
        assert!(verify_inclusion(&bob_entry, &alice_proof, &root).is_err());
    }

    /// Out-of-bounds proof requests surface as a typed error, not a
    /// panic.
    #[tokio::test]
    async fn proof_out_of_bounds_errors() {
        let store = LogStore::from_log(InMemoryLog::new(signing::SigningKey::generate()));
        assert!(matches!(store.proof(0).await, Err(StoreError::Log(_))));
    }

    /// ADR-0039 S3 — the fixed-`WITNESS_COSIG_LEN` bundle persistence framing
    /// round-trips: concatenating a K-cosig bundle and chunking it back yields
    /// the same K co-signatures (this is the exact `persist_witnessed` /
    /// `load_witnessed` blob encoding, tested without a DB).
    #[test]
    fn cosig_bundle_concat_chunk_round_trips() {
        // Zero, one, and several fixed-length cosigs.
        for k in [0usize, 1, 3, 7] {
            let bundle: Vec<Vec<u8>> = (0..k)
                .map(|i| vec![u8::try_from(i).unwrap_or(0); WITNESS_COSIG_LEN])
                .collect();
            let blob = bundle.concat();
            assert_eq!(blob.len(), k * WITNESS_COSIG_LEN);
            assert_eq!(chunk_cosigs(&blob), bundle, "k={k}");
        }
    }

    /// ADR-0039 S3 — a trailing partial chunk (only reachable under DB
    /// corruption) is dropped rather than panicking or mis-framing.
    #[test]
    fn chunk_cosigs_drops_a_trailing_partial() {
        let mut blob = vec![0xABu8; WITNESS_COSIG_LEN];
        blob.extend_from_slice(&[0xCD; 5]); // a stray partial chunk
        let got = chunk_cosigs(&blob);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].len(), WITNESS_COSIG_LEN);
    }

    /// ADR-0039 S3 — the dormant (no-witness) fan-out path serves an EMPTY
    /// bundle and never touches the DB. Uses a lazy pool (never connected)
    /// because the empty-witness branch returns before any query.
    #[tokio::test]
    async fn root_with_cosig_dormant_returns_empty_bundle() {
        use sqlx::mysql::MySqlPoolOptions;

        let mut log = InMemoryLog::new(signing::SigningKey::generate());
        for i in 0..4 {
            log.append(make_entry(&format!("u{i}"), 1));
        }
        let store = LogStore::from_log(log);

        // `connect_lazy` does not open a connection; the empty-witness path
        // returns before any query, so no live DB is needed.
        let pool = MySqlPoolOptions::new()
            .connect_lazy("mysql://unused:unused@127.0.0.1:1/unused")
            .expect("lazy pool from a well-formed URL");

        let (root, cosigs) = store.root_with_cosig(&pool, &[], 1_700_000_000).await;
        assert!(cosigs.is_empty(), "no witness configured -> empty bundle");
        root.verify(&store.operator_verifying_key().await)
            .expect("root still verifies under the operator key");
    }

    /// ADR-0039 S3b — `high_water_bases` maps `witness_high_water` rows to a
    /// positional catch-up base vector. A witness with no row starts at the
    /// genesis base 0 (the pre-S3 first-cosig posture); an out-of-range index (a
    /// shrunk witness list) is ignored. When NO witness is behind — the
    /// N=1-unchanged and all-witnesses-up cases — every base equals the common
    /// last-co-signed size, so the fan-out behaves exactly like the pre-S3 single
    /// shared base (backward-compat).
    #[test]
    fn high_water_bases_maps_rows_positionally() {
        // Empty table -> all bases at the genesis base 0 (fresh witnesses).
        assert_eq!(high_water_bases(&[], 3), vec![0, 0, 0]);

        // N=1 unchanged: the single witness's base is its own high-water — the
        // same value the pre-S3 shared base held. A fresh single witness starts
        // at the genesis base 0 (the pre-S3 first-cosig posture).
        assert_eq!(high_water_bases(&[(0, 7)], 1), vec![7]);
        assert_eq!(high_water_bases(&[], 1), vec![0]);

        // All-up happy path: every witness advanced together to the same head,
        // so every base is identical (== a single shared base).
        assert_eq!(
            high_water_bases(&[(0, 10), (1, 10), (2, 10)], 3),
            vec![10, 10, 10]
        );

        // A witness that fell behind keeps its OWN lower base while the others
        // are ahead — exactly the anti-stranding state.
        assert_eq!(high_water_bases(&[(0, 7), (1, 4)], 2), vec![7, 4]);

        // A missing row defaults to 0; an out-of-range index (list shrank) is
        // ignored.
        assert_eq!(high_water_bases(&[(2, 9)], 3), vec![0, 0, 9]);
        assert_eq!(high_water_bases(&[(5, 9)], 3), vec![0, 0, 0]);
    }

    /// ADR-0039 S3b — the anti-stranding regression proof (pure, real merkle).
    ///
    /// The S3 shared-base fan-out advanced ONE base whenever ANY witness
    /// co-signed, permanently stranding a witness that was down during a
    /// partial-success round (its own high-water fell behind the shared base it
    /// was thereafter proved from, so `verify_consistency` could never pass
    /// again). The per-witness catch-up sends EACH witness a proof from ITS OWN
    /// high-water, so a previously-down witness rejoins.
    ///
    /// Model: witness A + witness B both co-sign at size 4 (round 0). Round 1
    /// grows to 7 with B DOWN — A advances to 7, B stays at 4. Round 2 grows to
    /// 11 with both up. The per-witness bases are then `[7, 4]`
    /// (`high_water_bases` confirms), and B is handed a proof from ITS OWN base 4
    /// → it verifies. The contrast: the OLD shared base (7, advanced past B by A)
    /// would hand B a proof from 7, which B — still at high-water 4 — REJECTS.
    #[test]
    fn per_witness_catch_up_does_not_strand_a_down_witness() {
        use lakko_transparency::verify_consistency;

        let ts = 1_700_000_000;
        let mut log = InMemoryLog::new(signing::SigningKey::generate());

        // Grow to size 4 and capture root@4 (the round-0 base both witnesses
        // co-signed).
        for i in 0..4 {
            log.append(make_entry(&format!("u{i}"), 1));
        }
        let root_at_4 = log.sign_current_root(ts).tree_root;

        // Grow to size 7 and capture root@7 (round 1: A advances here, B is down).
        for i in 4..7 {
            log.append(make_entry(&format!("u{i}"), 1));
        }
        let root_at_7 = log.sign_current_root(ts).tree_root;

        // Grow to size 11 and capture root@11 (round 2 head).
        for i in 7..11 {
            log.append(make_entry(&format!("u{i}"), 1));
        }
        let root_at_11 = log.sign_current_root(ts).tree_root;

        // After round 1: A advanced to 7, B (down) still at 4. The per-witness
        // bases the fan-out uses in round 2 are exactly [7, 4].
        let bases = high_water_bases(&[(0, 7), (1, 4)], 2);
        assert_eq!(bases, vec![7, 4]);

        // Round 2 per-witness proofs, each from that witness's OWN base.
        let proof_a = log.consistency_proof(bases[0]); // 7 -> 11
        let proof_b = log.consistency_proof(bases[1]); // 4 -> 11

        // A (high-water 7) accepts its 7 -> 11 proof.
        assert!(
            verify_consistency(7, &root_at_7, 11, &root_at_11, &proof_a),
            "witness A catches up from its own base"
        );
        // B (high-water 4) accepts its 4 -> 11 proof — it is NOT stranded.
        assert!(
            verify_consistency(4, &root_at_4, 11, &root_at_11, &proof_b),
            "the previously-down witness rejoins from its OWN base"
        );

        // The bug the fix avoids: the S3 shared base (7) would prove B from 7,
        // but B is still at high-water 4 — `verify_consistency` against B's real
        // (root@4, size 4) high-water with a proof built from 7 REJECTS, i.e. B
        // would be permanently stranded.
        assert!(
            !verify_consistency(4, &root_at_4, 11, &root_at_11, &proof_a),
            "a proof from the SHARED base strands the witness that stayed behind"
        );
    }

    /// ADR-0039 S3b — the per-witness high-water persist/load round-trip. Upsert
    /// witness rows, reload the positional base vector, and assert a re-upsert
    /// advances only that witness (leaving a down witness's base behind). DB-gated
    /// on `LAKKO_TRANSPARENCY_TEST_DATABASE_URL` (skips, does not fail, when
    /// unset).
    #[tokio::test]
    async fn witness_high_water_persist_load_round_trips() {
        use sqlx::mysql::MySqlPoolOptions;

        let Ok(url) = std::env::var("LAKKO_TRANSPARENCY_TEST_DATABASE_URL") else {
            eprintln!(
                "skipping witness_high_water_persist_load_round_trips: \
                 LAKKO_TRANSPARENCY_TEST_DATABASE_URL unset"
            );
            return;
        };

        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect test DB");
        crate::MIGRATIONS.run(&pool).await.expect("run migrations");
        sqlx::query("DELETE FROM witness_high_water")
            .execute(&pool)
            .await
            .expect("clear witness_high_water");

        let root_a = [0xA1u8; 32];
        let root_b = [0xB2u8; 32];

        // No rows yet -> all bases at the genesis base 0.
        assert_eq!(
            load_witness_high_water_sizes(&pool, 3)
                .await
                .expect("load empty"),
            vec![0, 0, 0]
        );

        // Advance witness 0 -> 5 and witness 2 -> 8; witness 1 stays behind.
        persist_witness_high_water(&pool, 0, &root_a, 5)
            .await
            .expect("persist w0");
        persist_witness_high_water(&pool, 2, &root_a, 8)
            .await
            .expect("persist w2");
        assert_eq!(
            load_witness_high_water_sizes(&pool, 3)
                .await
                .expect("load partial"),
            vec![5, 0, 8]
        );

        // A re-upsert advances only witness 0 (to 9), leaving the others.
        persist_witness_high_water(&pool, 0, &root_b, 9)
            .await
            .expect("re-upsert w0");
        assert_eq!(
            load_witness_high_water_sizes(&pool, 3)
                .await
                .expect("load advanced"),
            vec![9, 0, 8]
        );

        // A shorter witness_count truncates the vector (an out-of-range row is
        // ignored on load).
        assert_eq!(
            load_witness_high_water_sizes(&pool, 1)
                .await
                .expect("load n=1"),
            vec![9]
        );

        sqlx::query("DELETE FROM witness_high_water")
            .execute(&pool)
            .await
            .expect("clear witness_high_water");
        pool.close().await;
    }

    /// KT-log S2 decision B — the store ASSIGNS the version monotonically
    /// per `username_hash`. DB-gated on
    /// `LAKKO_TRANSPARENCY_TEST_DATABASE_URL` (skips, does not fail, when
    /// unset — mirrors `tests/db_reload_e2e.rs`).
    ///
    /// Appending twice for the same username_hash yields versions 1 then
    /// 2; a different username_hash starts again at version 1.
    #[tokio::test]
    async fn append_assigns_monotonic_version_per_username() {
        use sqlx::mysql::MySqlPoolOptions;

        let Ok(url) = std::env::var("LAKKO_TRANSPARENCY_TEST_DATABASE_URL") else {
            eprintln!(
                "skipping append_assigns_monotonic_version_per_username: \
                 LAKKO_TRANSPARENCY_TEST_DATABASE_URL unset"
            );
            return;
        };

        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect test DB");
        crate::MIGRATIONS.run(&pool).await.expect("run migrations");
        sqlx::query("DELETE FROM log_entries")
            .execute(&pool)
            .await
            .expect("clear log_entries");

        let store = LogStore::from_log(InMemoryLog::new(signing::SigningKey::generate()));

        let alice = lakko_crypto::hash::blake3(b"alice");
        let bob = lakko_crypto::hash::blake3(b"bob");

        // Two appends for alice -> versions 1, 2; indices 0, 1.
        let r0 = store
            .append(
                &pool,
                alice,
                vec![0x01; 32],
                false,
                None,
                GenesisAttestGate::default(),
                0,
            )
            .await
            .expect("append alice v1");
        assert_eq!((r0.idx, r0.version), (0, 1));
        let r1 = store
            .append(
                &pool,
                alice,
                vec![0x02; 32],
                false,
                None,
                GenesisAttestGate::default(),
                0,
            )
            .await
            .expect("append alice v2");
        assert_eq!((r1.idx, r1.version), (1, 2));

        // First append for bob -> version 1 (a different username_hash
        // restarts the version counter), index 2.
        let r2 = store
            .append(
                &pool,
                bob,
                vec![0x03; 32],
                true,
                None,
                GenesisAttestGate::default(),
                0,
            )
            .await
            .expect("append bob v1");
        assert_eq!((r2.idx, r2.version), (2, 1));

        sqlx::query("DELETE FROM log_entries")
            .execute(&pool)
            .await
            .expect("clear log_entries");
        pool.close().await;
    }

    /// KT-log S2.5 idempotency — re-appending the SAME
    /// `(username_hash, pubkey)` is a no-op (identical `{idx, version}`,
    /// log size unchanged); a DIFFERENT pubkey for the same username
    /// bumps the version. DB-gated on
    /// `LAKKO_TRANSPARENCY_TEST_DATABASE_URL` (skips, does not fail, when
    /// unset).
    #[tokio::test]
    async fn append_is_idempotent_for_same_pubkey() {
        use sqlx::mysql::MySqlPoolOptions;

        let Ok(url) = std::env::var("LAKKO_TRANSPARENCY_TEST_DATABASE_URL") else {
            eprintln!(
                "skipping append_is_idempotent_for_same_pubkey: \
                 LAKKO_TRANSPARENCY_TEST_DATABASE_URL unset"
            );
            return;
        };

        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect test DB");
        crate::MIGRATIONS.run(&pool).await.expect("run migrations");
        sqlx::query("DELETE FROM log_entries")
            .execute(&pool)
            .await
            .expect("clear log_entries");

        let store = LogStore::from_log(InMemoryLog::new(signing::SigningKey::generate()));
        let alice = lakko_crypto::hash::blake3(b"alice");

        // First append for alice -> idx 0, version 1, size 1.
        let r0 = store
            .append(
                &pool,
                alice,
                vec![0x01; 32],
                false,
                None,
                GenesisAttestGate::default(),
                0,
            )
            .await
            .expect("append alice v1");
        assert_eq!((r0.idx, r0.version), (0, 1));
        assert_eq!(store.size().await, 1);

        // Re-append the SAME pubkey -> no-op: identical coordinates, size
        // unchanged (no duplicate leaf written).
        let r0_again = store
            .append(
                &pool,
                alice,
                vec![0x01; 32],
                false,
                None,
                GenesisAttestGate::default(),
                0,
            )
            .await
            .expect("re-append alice same pubkey");
        assert_eq!((r0_again.idx, r0_again.version), (0, 1));
        assert_eq!(
            store.size().await,
            1,
            "no-op re-append must not grow the log"
        );

        // A reset-flagged re-append of the SAME pubkey is STILL a no-op
        // (the dedup keys on the pubkey, not the is_reset flag).
        let r0_reset = store
            .append(
                &pool,
                alice,
                vec![0x01; 32],
                true,
                None,
                GenesisAttestGate::default(),
                0,
            )
            .await
            .expect("re-append alice same pubkey is_reset=true");
        assert_eq!((r0_reset.idx, r0_reset.version), (0, 1));
        assert_eq!(store.size().await, 1);

        // A DIFFERENT pubkey for the same username -> version+1, new leaf.
        let r1 = store
            .append(
                &pool,
                alice,
                vec![0x02; 32],
                false,
                None,
                GenesisAttestGate::default(),
                0,
            )
            .await
            .expect("append alice v2 (different pubkey)");
        assert_eq!((r1.idx, r1.version), (1, 2));
        assert_eq!(store.size().await, 2);

        sqlx::query("DELETE FROM log_entries")
            .execute(&pool)
            .await
            .expect("clear log_entries");
        pool.close().await;
    }

    /// G-30 #2 (ADR-0019) — the store's append-side leaf-authorisation gate.
    /// A PRESENT `leaf_auth` is verified: a valid genesis self-sig and a valid
    /// rotation supersession proof are accepted; a garbage auth and a proof
    /// authorising a DIFFERENT key are refused with `Unauthorised`. (Absence
    /// is governed by the handler's presence policy, not the store.) DB-gated
    /// on `LAKKO_TRANSPARENCY_TEST_DATABASE_URL`.
    #[tokio::test]
    #[allow(clippy::too_many_lines)] // DB-gated multi-case gate walkthrough reads best whole
    async fn append_verifies_leaf_authorisation() {
        use lakko_transparency::{sign_leaf_genesis, SupersessionProof};
        use sqlx::mysql::MySqlPoolOptions;

        let Ok(url) = std::env::var("LAKKO_TRANSPARENCY_TEST_DATABASE_URL") else {
            eprintln!(
                "skipping append_verifies_leaf_authorisation: \
                 LAKKO_TRANSPARENCY_TEST_DATABASE_URL unset"
            );
            return;
        };

        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect test DB");
        crate::MIGRATIONS.run(&pool).await.expect("run migrations");
        sqlx::query("DELETE FROM log_entries")
            .execute(&pool)
            .await
            .expect("clear log_entries");

        let store = LogStore::from_log(InMemoryLog::new(signing::SigningKey::generate()));
        let alice = lakko_crypto::hash::blake3(b"alice");
        let now = 1_700_000_000_i64;

        // Genesis: a valid self-sig by the leaf's OWN key is accepted.
        let k1 = signing::SigningKey::from_seed(&[0x11; 32]);
        let pk1 = k1.verifying_key().to_bytes().to_vec();
        let genesis = sign_leaf_genesis(&k1, &alice, &pk1).to_vec();
        let r0 = store
            .append(
                &pool,
                alice,
                pk1.clone(),
                false,
                Some(&genesis),
                GenesisAttestGate::default(),
                now,
            )
            .await
            .expect("genesis self-sig accepted");
        assert_eq!(r0.version, 1);

        // A new key carrying a GARBAGE auth is refused.
        let k_bad = signing::SigningKey::from_seed(&[0x55; 32]);
        let pk_bad = k_bad.verifying_key().to_bytes().to_vec();
        let bad = store
            .append(
                &pool,
                alice,
                pk_bad,
                false,
                Some(&[0u8; 64]),
                GenesisAttestGate::default(),
                now,
            )
            .await;
        assert!(
            matches!(bad, Err(StoreError::Unauthorised(_))),
            "garbage auth must be refused"
        );

        // Rotation: a supersession proof signed by the PREVIOUS key (k1)
        // authorising the new key (k2) is accepted.
        let k2 = signing::SigningKey::from_seed(&[0x22; 32]);
        let pk2 = k2.verifying_key().to_bytes().to_vec();
        let proof = SupersessionProof::new(
            k1.verifying_key().to_bytes(),
            k2.verifying_key().to_bytes(),
            now,
            now + 1000,
        )
        .sign(&k1)
        .to_vec();
        let r1 = store
            .append(
                &pool,
                alice,
                pk2,
                false,
                Some(&proof),
                GenesisAttestGate::default(),
                now + 10,
            )
            .await
            .expect("rotation proof accepted");
        assert_eq!(r1.version, 2);

        // A proof signed by the current key (k2) that authorises k3, but used
        // to append a DIFFERENT key (k4), is refused.
        let k3 = signing::SigningKey::from_seed(&[0x33; 32]);
        let k4 = signing::SigningKey::from_seed(&[0x44; 32]);
        let pk4 = k4.verifying_key().to_bytes().to_vec();
        let proof2 = SupersessionProof::new(
            k2.verifying_key().to_bytes(),
            k3.verifying_key().to_bytes(),
            now,
            now + 1000,
        )
        .sign(&k2)
        .to_vec();
        let mismatch = store
            .append(
                &pool,
                alice,
                pk4,
                false,
                Some(&proof2),
                GenesisAttestGate::default(),
                now + 10,
            )
            .await;
        assert!(
            matches!(mismatch, Err(StoreError::Unauthorised(_))),
            "a proof authorising a different key must be refused"
        );

        sqlx::query("DELETE FROM log_entries")
            .execute(&pool)
            .await
            .expect("clear log_entries");
        pool.close().await;
    }

    /// ADR-0036 S4 — the store's append-side auth-attested genesis gate.
    /// With the flag ON (`required: true` + a pinned attest key), a genesis leaf
    /// carrying a VALID `GenesisAttestation` (bound to the served
    /// `(username_hash, pubkey)` + still fresh) is accepted; a MISSING, an
    /// INVALID (garbage), and an EXPIRED attestation are each refused with
    /// `Unauthorised`. With the flag OFF the attestation is ignored (back-compat)
    /// and the genesis leaf appends. Distinct usernames keep every case a genesis
    /// (a rotation would bypass the gate by design). DB-gated on
    /// `LAKKO_TRANSPARENCY_TEST_DATABASE_URL`.
    #[tokio::test]
    #[allow(clippy::too_many_lines)] // DB-gated multi-case gate walkthrough reads best whole
    async fn append_verifies_genesis_attestation() {
        use lakko_attest::GenesisAttestation;
        use sqlx::mysql::MySqlPoolOptions;

        let Ok(url) = std::env::var("LAKKO_TRANSPARENCY_TEST_DATABASE_URL") else {
            eprintln!(
                "skipping append_verifies_genesis_attestation: \
                 LAKKO_TRANSPARENCY_TEST_DATABASE_URL unset"
            );
            return;
        };

        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect test DB");
        crate::MIGRATIONS.run(&pool).await.expect("run migrations");
        sqlx::query("DELETE FROM log_entries")
            .execute(&pool)
            .await
            .expect("clear log_entries");

        let store = LogStore::from_log(InMemoryLog::new(signing::SigningKey::generate()));
        let now = 1_700_000_000_i64;

        // The auth attest key: the store verifies the attestation against its
        // VERIFYING half (which the operator pins from auth's JWKS).
        let attest_signer = signing::SigningKey::from_seed(&[0xE1; 32]);
        let attest_key = attest_signer.verifying_key();
        let account_id = [0x09u8; 16];

        // Valid: a genesis attestation bound to (alice, pkA), still fresh.
        let alice = lakko_crypto::hash::blake3(b"alice-genesis-attest");
        let pk_a = [0xA1u8; 32];
        let att_ok =
            GenesisAttestation::new(account_id, alice, pk_a, now + 300).sign(&attest_signer);
        let gate_ok = GenesisAttestGate {
            attestation: Some(&att_ok),
            key: Some(&attest_key),
            required: true,
        };
        let r0 = store
            .append(&pool, alice, pk_a.to_vec(), false, None, gate_ok, now)
            .await
            .expect("valid genesis attestation accepted");
        assert_eq!(r0.version, 1);
        assert_eq!(store.size().await, 1);
        // The blob is persisted opaque and served back on `latest`.
        let latest = store
            .latest(&pool, alice)
            .await
            .expect("latest alice")
            .expect("alice has an entry");
        assert_eq!(latest.genesis_attest.as_deref(), Some(&att_ok[..]));

        // Missing: the flag is ON but no attestation is supplied — refused.
        let bob = lakko_crypto::hash::blake3(b"bob-genesis-attest");
        let pk_b = [0xB2u8; 32];
        let gate_missing = GenesisAttestGate {
            attestation: None,
            key: Some(&attest_key),
            required: true,
        };
        let missing = store
            .append(&pool, bob, pk_b.to_vec(), false, None, gate_missing, now)
            .await;
        assert!(
            matches!(missing, Err(StoreError::Unauthorised(_))),
            "a genesis leaf with no attestation must be refused under the flag"
        );

        // Invalid: a garbage blob of the right length does not verify — refused.
        let carol = lakko_crypto::hash::blake3(b"carol-genesis-attest");
        let pk_c = [0xC3u8; 32];
        let garbage = [0u8; lakko_attest::GENESIS_ATTESTATION_LEN];
        let gate_garbage = GenesisAttestGate {
            attestation: Some(&garbage),
            key: Some(&attest_key),
            required: true,
        };
        let invalid = store
            .append(&pool, carol, pk_c.to_vec(), false, None, gate_garbage, now)
            .await;
        assert!(
            matches!(invalid, Err(StoreError::Unauthorised(_))),
            "a garbage attestation must be refused"
        );

        // Expired: a validly-signed attestation past its expiry — refused.
        let dave = lakko_crypto::hash::blake3(b"dave-genesis-attest");
        let pk_d = [0xD4u8; 32];
        let att_expired =
            GenesisAttestation::new(account_id, dave, pk_d, now - 1).sign(&attest_signer);
        let gate_expired = GenesisAttestGate {
            attestation: Some(&att_expired),
            key: Some(&attest_key),
            required: true,
        };
        let expired = store
            .append(&pool, dave, pk_d.to_vec(), false, None, gate_expired, now)
            .await;
        assert!(
            matches!(expired, Err(StoreError::Unauthorised(_))),
            "an expired attestation must be refused"
        );

        // A wrong-pubkey attestation (bound to pkA but appending pkE) is refused.
        let erin = lakko_crypto::hash::blake3(b"erin-genesis-attest");
        let pk_e = [0xE5u8; 32];
        let att_wrong_pk =
            GenesisAttestation::new(account_id, erin, pk_a, now + 300).sign(&attest_signer);
        let gate_wrong_pk = GenesisAttestGate {
            attestation: Some(&att_wrong_pk),
            key: Some(&attest_key),
            required: true,
        };
        let wrong_pk = store
            .append(&pool, erin, pk_e.to_vec(), false, None, gate_wrong_pk, now)
            .await;
        assert!(
            matches!(wrong_pk, Err(StoreError::Unauthorised(_))),
            "an attestation bound to a different pubkey must be refused"
        );

        // Only the one valid append wrote a row.
        assert_eq!(
            store.size().await,
            1,
            "refused appends must not grow the log"
        );

        // Flag OFF: a genesis leaf with no attestation appends (back-compat).
        let frank = lakko_crypto::hash::blake3(b"frank-genesis-attest");
        let pk_f = [0xF6u8; 32];
        let gate_off = GenesisAttestGate {
            attestation: None,
            key: None,
            required: false,
        };
        let r_off = store
            .append(&pool, frank, pk_f.to_vec(), false, None, gate_off, now)
            .await
            .expect("flag OFF ignores a missing attestation");
        assert_eq!(r_off.version, 1);
        assert_eq!(store.size().await, 2);

        sqlx::query("DELETE FROM log_entries")
            .execute(&pool)
            .await
            .expect("clear log_entries");
        pool.close().await;
    }

    /// KT-log S3a — `latest` returns the highest-version entry for a
    /// `username_hash` (or `None` for an unseen account-key), carrying the
    /// leaf `idx`, `version`, `identity_pubkey`, and `is_reset`. DB-gated
    /// on `LAKKO_TRANSPARENCY_TEST_DATABASE_URL` (skips, does not fail,
    /// when unset).
    #[tokio::test]
    async fn latest_returns_highest_version_entry() {
        use sqlx::mysql::MySqlPoolOptions;

        let Ok(url) = std::env::var("LAKKO_TRANSPARENCY_TEST_DATABASE_URL") else {
            eprintln!(
                "skipping latest_returns_highest_version_entry: \
                 LAKKO_TRANSPARENCY_TEST_DATABASE_URL unset"
            );
            return;
        };

        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect test DB");
        crate::MIGRATIONS.run(&pool).await.expect("run migrations");
        sqlx::query("DELETE FROM log_entries")
            .execute(&pool)
            .await
            .expect("clear log_entries");

        let store = LogStore::from_log(InMemoryLog::new(signing::SigningKey::generate()));
        let alice = lakko_crypto::hash::blake3(b"alice");
        let bob = lakko_crypto::hash::blake3(b"bob");

        // Unseen account-key -> None.
        assert_eq!(store.latest(&pool, alice).await.expect("latest none"), None);

        // alice v1 (idx 0), then alice v2 (idx 1, is_reset=true).
        store
            .append(
                &pool,
                alice,
                vec![0x01; 32],
                false,
                None,
                GenesisAttestGate::default(),
                0,
            )
            .await
            .expect("append alice v1");
        store
            .append(
                &pool,
                alice,
                vec![0x02; 32],
                true,
                None,
                GenesisAttestGate::default(),
                0,
            )
            .await
            .expect("append alice v2");
        // bob v1 (idx 2) — a different account-key in between.
        store
            .append(
                &pool,
                bob,
                vec![0x03; 32],
                false,
                None,
                GenesisAttestGate::default(),
                0,
            )
            .await
            .expect("append bob v1");

        // latest(alice) must be v2 (the highest version), not v1.
        let latest = store
            .latest(&pool, alice)
            .await
            .expect("latest alice")
            .expect("alice has an entry");
        assert_eq!(
            latest,
            LatestEntry {
                idx: 1,
                version: 2,
                identity_pubkey: vec![0x02; 32],
                is_reset: true,
                leaf_auth: None,
                genesis_attest: None,
                reset_seal: None,
            }
        );

        // latest(bob) is its sole v1 entry, is_reset=false.
        let latest_bob = store
            .latest(&pool, bob)
            .await
            .expect("latest bob")
            .expect("bob has an entry");
        assert_eq!(
            latest_bob,
            LatestEntry {
                idx: 2,
                version: 1,
                identity_pubkey: vec![0x03; 32],
                is_reset: false,
                leaf_auth: None,
                genesis_attest: None,
                reset_seal: None,
            }
        );

        sqlx::query("DELETE FROM log_entries")
            .execute(&pool)
            .await
            .expect("clear log_entries");
        pool.close().await;
    }

    /// ADR-0101 section 10: an announcement leaf is appended only under the
    /// account's latest identity key with a rising `seq`, a retry returns its
    /// index, the newest per generation is served, and a reload replays both
    /// leaf kinds into the same tree; a reset leaf keeps its seal sidecar.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn announcements_append_serve_and_replay() {
        use lakko_attest::{
            key_reset_account_ref, password_seal_key, ResetSeal, SealAnnouncement, SealEntry,
            SealKind,
        };
        use sqlx::mysql::MySqlPoolOptions;

        let Ok(url) = std::env::var("LAKKO_TRANSPARENCY_TEST_DATABASE_URL") else {
            eprintln!(
                "skipping announcements_append_serve_and_replay: \
                 LAKKO_TRANSPARENCY_TEST_DATABASE_URL unset"
            );
            return;
        };
        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect test DB");
        crate::MIGRATIONS.run(&pool).await.expect("run migrations");
        for clear in ["DELETE FROM log_entries", "DELETE FROM log_announcements"] {
            sqlx::query(clear).execute(&pool).await.expect("clear");
        }

        let operator = signing::SigningKey::from_seed(&[0x0F; 32]);
        let store = LogStore::from_log(InMemoryLog::new(operator.clone()));
        let account = [0x31u8; 16];
        let user = lakko_transparency::account_username_hash(&account);
        let old_key = signing::SigningKey::from_seed(&[0x41; 32]);
        let new_key = signing::SigningKey::from_seed(&[0x42; 32]);
        let seal_key = password_seal_key(&[0x5E; 64]);
        let announce = |key: &signing::SigningKey, generation: u64, seq: u64| {
            SealAnnouncement::new(
                key_reset_account_ref(&account),
                generation,
                seq,
                1,
                vec![SealEntry {
                    kind: SealKind::Password,
                    tag: [0; 16],
                    seal_pk: seal_key.verifying_key().to_bytes(),
                }],
            )
            .sign(key)
            .unwrap()
        };

        // No identity leaf yet: refused.
        let a1 = announce(&old_key, 1, 1);
        assert!(matches!(
            store.append_announcement(&pool, user, &a1).await,
            Err(StoreError::Unauthorised(_))
        ));
        store
            .append(
                &pool,
                user,
                old_key.verifying_key().to_bytes().to_vec(),
                false,
                None,
                GenesisAttestGate::default(),
                0,
            )
            .await
            .expect("identity v1");
        // Signed by another key: refused.
        assert!(matches!(
            store
                .append_announcement(&pool, user, &announce(&new_key, 1, 1))
                .await,
            Err(StoreError::Unauthorised(_))
        ));
        let i1 = store
            .append_announcement(&pool, user, &a1)
            .await
            .expect("a1");
        assert_eq!(i1, 1);
        assert_eq!(
            store.append_announcement(&pool, user, &a1).await.unwrap(),
            1
        );
        let a3 = announce(&old_key, 1, 3);
        let i3 = store
            .append_announcement(&pool, user, &a3)
            .await
            .expect("a3");
        assert!(matches!(
            store
                .append_announcement(&pool, user, &announce(&old_key, 1, 2))
                .await,
            Err(StoreError::Stale)
        ));

        // The sealed reset leaf; its seal is a sidecar.
        let seal = ResetSeal {
            account_ref: key_reset_account_ref(&account),
            announcement_seq: 3,
            from_generation: 1,
            to_generation: 2,
            new_signature_pubkey: new_key.verifying_key().to_bytes(),
            kind: SealKind::Password,
            seal_pk: seal_key.verifying_key().to_bytes(),
        }
        .sign(&seal_key)
        .unwrap();
        // A seal naming another key is refused.
        assert!(matches!(
            store
                .append_with_seal(
                    &pool,
                    user,
                    vec![0x55; 32],
                    true,
                    None,
                    GenesisAttestGate::default(),
                    Some(&seal),
                    0,
                )
                .await,
            Err(StoreError::Unauthorised(_))
        ));
        let reset = store
            .append_with_seal(
                &pool,
                user,
                new_key.verifying_key().to_bytes().to_vec(),
                true,
                None,
                GenesisAttestGate::default(),
                Some(&seal),
                0,
            )
            .await
            .expect("reset leaf");
        let latest = store.latest(&pool, user).await.unwrap().unwrap();
        assert_eq!(latest.reset_seal.as_deref(), Some(seal.as_slice()));
        let b4 = announce(&new_key, 2, 4);
        store
            .append_announcement(&pool, user, &b4)
            .await
            .expect("b4");

        // Served: the newest of each generation.
        let g1 = store.announcement(&pool, user, 1).await.unwrap().unwrap();
        assert_eq!(
            (g1.idx, g1.seq, g1.announcement.as_slice()),
            (i3, 3, a3.as_slice())
        );
        let g2 = store.announcement(&pool, user, 2).await.unwrap().unwrap();
        assert_eq!(g2.seq, 4);
        assert!(store.announcement(&pool, user, 3).await.unwrap().is_none());

        // The proof served for the announcement leaf proves it before the
        // reset leaf, and a reload rebuilds the same root.
        let root = store.signed_root(9).await;
        let (leaf_bytes, proof) = store.leaf_proof(i3).await.unwrap();
        let leaf = lakko_transparency::AnnouncementLeaf::for_announcement(user, 1, 3, &a3);
        assert_eq!(leaf_bytes, leaf.canonical_bytes());
        lakko_transparency::verify_announcement_before_reset(&leaf, &proof, reset.idx, &root)
            .expect("announcement before reset");
        let reloaded = LogStore::load(&pool, operator).await.expect("reload");
        assert_eq!(reloaded.signed_root(9).await.tree_root, root.tree_root);
        assert_eq!(reloaded.size().await, 5);

        for clear in ["DELETE FROM log_entries", "DELETE FROM log_announcements"] {
            sqlx::query(clear).execute(&pool).await.expect("clear");
        }
        pool.close().await;
    }
}
