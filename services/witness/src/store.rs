// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Witness cosign state + the consistency-gated cosign logic (ADR-0020).
//!
//! The witness keeps ONE piece of state per log it co-signs (witness v2,
//! ADR-0098 section 5) — its last-co-signed `(tree_root, log_size)` for that
//! log. Before co-signing a new operator root it verifies
//! the root is operator-signed AND that a Merkle consistency proof shows the
//! new root append-only-extends its high-water. Two divergent forks cannot both
//! extend a single root, so the operator cannot get a co-signature on a fork —
//! that is the split-view defence (G-30 #1).

use anyhow::Context as _;
use lakko_crypto::signing::{SigningKey, VerifyingKey};
use lakko_transparency::{
    sign_witness_cosig, verify_consistency, LogId, SignedRoot, TreeHash, WITNESS_COSIG_LEN,
};
use sqlx::MySqlPool;
use tokio::sync::Mutex;

/// The witness's last-co-signed high-water — the only log state it keeps.
#[derive(Clone, Copy)]
struct HighWater {
    tree_root: TreeHash,
    log_size: u64,
}

/// Why a cosign was refused. Each variant maps to a distinct HTTP status.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CosignError {
    /// The presented root is not signed by the pinned operator key.
    #[error("operator signature on the presented root is invalid")]
    BadOperatorSignature,

    /// The presented root does not append-only-extend the last-co-signed root
    /// (a fork, or a missing/invalid consistency proof). The witness REFUSES —
    /// this is the split-view defence.
    #[error("the presented root does not append-only-extend the last-co-signed root")]
    NotConsistent,

    /// The presented `log_size` is behind the witness high-water (a rollback).
    #[error("presented log_size {presented} is behind the witness high-water {seen}")]
    Rollback {
        /// The size on the presented root.
        presented: u64,
        /// The size the witness last co-signed.
        seen: u64,
    },

    /// Storage failure while advancing the high-water.
    #[error("witness storage error: {0}")]
    Db(String),
}

/// The witness's Ed25519 key + its serialised cosign state, one high-water per
/// log. Each log has its own mutex, so a cosign for one log never waits on the
/// other.
pub(crate) struct WitnessStore {
    witness_key: SigningKey,
    kt: Mutex<HighWater>,
    moderation: Mutex<HighWater>,
}

impl WitnessStore {
    /// Load the witness key + the last-co-signed high-water of every log,
    /// seeding each log's row at the empty tree (`log_size = 0`, all-zero root)
    /// on first boot.
    ///
    /// # Errors
    ///
    /// Propagates sqlx errors / a corrupt (non-32-byte) stored root.
    pub(crate) async fn load(pool: &MySqlPool, witness_key: SigningKey) -> anyhow::Result<Self> {
        let kt = load_high_water(pool, LogId::Kt).await?;
        let moderation = load_high_water(pool, LogId::Moderation).await?;
        // The ADR-0096 size gauge follows the KT log.
        crate::metrics::log_size(kt.log_size);
        Ok(Self::from_parts(witness_key, kt, moderation))
    }

    /// Build a store from already-loaded state (also used by route tests that
    /// never reach the database).
    fn from_parts(witness_key: SigningKey, kt: HighWater, moderation: HighWater) -> Self {
        Self {
            witness_key,
            kt: Mutex::new(kt),
            moderation: Mutex::new(moderation),
        }
    }

    /// A store at the empty tree for every log (route tests).
    #[cfg(test)]
    pub(crate) fn empty_for_tests(witness_key: SigningKey) -> Self {
        let empty = HighWater {
            tree_root: [0u8; 32],
            log_size: 0,
        };
        Self::from_parts(witness_key, empty, empty)
    }

    fn state_for(&self, log: LogId) -> &Mutex<HighWater> {
        match log {
            LogId::Kt => &self.kt,
            LogId::Moderation => &self.moderation,
        }
    }

    /// The witness's verifying key — published at `GET /witness/key` + pinned by
    /// clients out-of-band.
    pub(crate) fn witness_verifying_key(&self) -> VerifyingKey {
        self.witness_key.verifying_key()
    }

    /// Verify the operator-signed `new_root` append-only-extends the witness
    /// high-water of `log`, then co-sign (binding `log`) + advance. Returns the co-signature wire blob.
    ///
    /// Serialised under a mutex so the consistency check + advance are atomic —
    /// two concurrent cosigns can never both advance past a fork.
    ///
    /// # Errors
    ///
    /// [`CosignError`] on a bad operator signature, a rollback, an inconsistent
    /// (forked) root, or a storage failure.
    pub(crate) async fn cosign(
        &self,
        pool: &MySqlPool,
        log: LogId,
        operator: &VerifyingKey,
        new_root: &SignedRoot,
        consistency_proof: &[TreeHash],
    ) -> Result<[u8; WITNESS_COSIG_LEN], CosignError> {
        // 1. The root MUST be signed by the pinned operator key.
        new_root
            .verify(operator)
            .map_err(|_| CosignError::BadOperatorSignature)?;

        let mut hw = self.state_for(log).lock().await;

        // 2. No rollback below what we already co-signed.
        if new_root.log_size < hw.log_size {
            return Err(CosignError::Rollback {
                presented: new_root.log_size,
                seen: hw.log_size,
            });
        }

        // 3. Consistency: the new root append-only-extends the last-co-signed
        //    one. At `hw.log_size == 0` (first boot) the empty tree is a prefix
        //    of anything (empty proof). At `== new size` it requires the SAME
        //    root (an idempotent re-cosign); a same-size DIFFERENT root — a fork
        //    — fails here, which is the whole defence.
        if !verify_consistency(
            hw.log_size,
            &hw.tree_root,
            new_root.log_size,
            &new_root.tree_root,
            consistency_proof,
        ) {
            return Err(CosignError::NotConsistent);
        }

        // 4. Co-sign the (tree_root, log_size).
        let cosig = sign_witness_cosig(
            &self.witness_key,
            log,
            &new_root.tree_root,
            new_root.log_size,
        );

        // 5. Advance the high-water on real growth (an idempotent same-size
        //    re-cosign leaves it untouched).
        if new_root.log_size > hw.log_size {
            sqlx::query(
                "UPDATE witness_log_state SET tree_root = ?, log_size = ? WHERE log_id = ?",
            )
            .bind(&new_root.tree_root[..])
            .bind(new_root.log_size)
            .bind(log.byte())
            .execute(pool)
            .await
            .map_err(|e| CosignError::Db(e.to_string()))?;
            hw.tree_root = new_root.tree_root;
            hw.log_size = new_root.log_size;
            if log == LogId::Kt {
                crate::metrics::log_size(hw.log_size);
            }
        }

        Ok(cosig)
    }
}

/// Read (seeding at the empty tree on first boot) one log's high-water.
async fn load_high_water(pool: &MySqlPool, log: LogId) -> anyhow::Result<HighWater> {
    sqlx::query(
        "INSERT IGNORE INTO witness_log_state (log_id, tree_root, log_size) VALUES (?, ?, 0)",
    )
    .bind(log.byte())
    .bind(&[0u8; 32][..])
    .execute(pool)
    .await
    .context("seed witness_log_state")?;

    let (root_bytes, log_size): (Vec<u8>, u64) =
        sqlx::query_as("SELECT tree_root, log_size FROM witness_log_state WHERE log_id = ?")
            .bind(log.byte())
            .fetch_one(pool)
            .await
            .context("read witness_log_state")?;
    let tree_root: TreeHash = root_bytes.as_slice().try_into().map_err(|_| {
        anyhow::anyhow!("witness_log_state.tree_root is not 32 bytes — corruption?")
    })?;
    Ok(HighWater {
        tree_root,
        log_size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::signing::SigningKey;
    use lakko_transparency::{merkle, verify_witness_cosig, LogId, SignedRoot};
    use sqlx::mysql::MySqlPoolOptions;

    /// The split-view defence end-to-end: a consistent extension is co-signed,
    /// a same-size fork is refused. DB-gated on `LAKKO_WITNESS_TEST_DATABASE_URL`.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn cosign_accepts_extension_refuses_fork() {
        let Ok(url) = std::env::var("LAKKO_WITNESS_TEST_DATABASE_URL") else {
            eprintln!(
                "skipping cosign_accepts_extension_refuses_fork: \
                 LAKKO_WITNESS_TEST_DATABASE_URL unset"
            );
            return;
        };

        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect test DB");
        crate::MIGRATIONS.run(&pool).await.expect("run migrations");
        sqlx::query("DELETE FROM witness_log_state")
            .execute(&pool)
            .await
            .expect("clear witness_log_state");

        let operator = SigningKey::from_seed(&[0x77; 32]);
        let operator_vk = operator.verifying_key();
        let witness = SigningKey::from_seed(&[0x88; 32]);
        let witness_vk = witness.verifying_key().to_bytes();
        let store = WitnessStore::load(&pool, witness)
            .await
            .expect("load witness store");

        let leaves: Vec<Vec<u8>> = (0..6).map(|i| format!("leaf-{i}").into_bytes()).collect();
        let sign_root = |m: usize| -> SignedRoot {
            let tree_root = merkle::root(&leaves[..m]);
            let payload = SignedRoot::signing_payload(&tree_root, m as u64, 1_700_000_000);
            SignedRoot {
                tree_root,
                log_size: m as u64,
                timestamp_unix: 1_700_000_000,
                signature: operator.sign(&payload),
            }
        };

        // 1. Genesis co-sign at size 3 (the witness is at the empty tree, so the
        //    consistency proof is empty). The returned co-sig verifies.
        let r3 = sign_root(3);
        let cosig3 = store
            .cosign(
                &pool,
                LogId::Kt,
                &operator_vk,
                &r3,
                &lakko_transparency::consistency_proof(0, &leaves[..3]),
            )
            .await
            .expect("genesis co-sign");
        verify_witness_cosig(&cosig3, &witness_vk, LogId::Kt, &r3.tree_root, 3)
            .expect("the witness co-sig verifies against the witness key");

        // 2. A consistent extension to size 6 is co-signed.
        let r6 = sign_root(6);
        store
            .cosign(
                &pool,
                LogId::Kt,
                &operator_vk,
                &r6,
                &lakko_transparency::consistency_proof(3, &leaves[..6]),
            )
            .await
            .expect("consistent extension co-signed");

        // 3. A FORK at the same size 6 — a DIFFERENT tree — is REFUSED. This is
        //    the split-view defence: the operator cannot get a co-signature on a
        //    history that does not extend what the witness already co-signed.
        let forked: Vec<Vec<u8>> = (0..6).map(|i| format!("forked-{i}").into_bytes()).collect();
        let forked_tree_root = merkle::root(&forked);
        let payload = SignedRoot::signing_payload(&forked_tree_root, 6, 1_700_000_001);
        let forked_root = SignedRoot {
            tree_root: forked_tree_root,
            log_size: 6,
            timestamp_unix: 1_700_000_001,
            signature: operator.sign(&payload),
        };
        let res = store
            .cosign(&pool, LogId::Kt, &operator_vk, &forked_root, &[])
            .await;
        assert!(
            matches!(res, Err(CosignError::NotConsistent)),
            "a same-size fork must be refused (split-view defence)"
        );

        // 4. A root NOT signed by the pinned operator is refused outright.
        let imposter = SigningKey::from_seed(&[0x99; 32]);
        let r7_tree = merkle::root(&[leaves.clone(), vec![b"x".to_vec()]].concat());
        let bad_payload = SignedRoot::signing_payload(&r7_tree, 7, 1_700_000_002);
        let bad_root = SignedRoot {
            tree_root: r7_tree,
            log_size: 7,
            timestamp_unix: 1_700_000_002,
            signature: imposter.sign(&bad_payload),
        };
        assert!(
            matches!(
                store
                    .cosign(&pool, LogId::Kt, &operator_vk, &bad_root, &[])
                    .await,
                Err(CosignError::BadOperatorSignature)
            ),
            "a root not signed by the pinned operator is refused"
        );

        // 5. The moderation log has its own high-water: the KT log at size 6
        //    does not stop a moderation genesis at size 3, and the cosig binds
        //    the moderation log (it does not verify as a KT cosig).
        let m3 = store
            .cosign(
                &pool,
                LogId::Moderation,
                &operator_vk,
                &r3,
                &lakko_transparency::consistency_proof(0, &leaves[..3]),
            )
            .await
            .expect("moderation genesis co-sign");
        verify_witness_cosig(&m3, &witness_vk, LogId::Moderation, &r3.tree_root, 3)
            .expect("moderation cosig verifies for the moderation log");
        assert!(verify_witness_cosig(&m3, &witness_vk, LogId::Kt, &r3.tree_root, 3).is_err());

        // 6. A reload reads both rows back.
        let reloaded = WitnessStore::load(&pool, SigningKey::from_seed(&[0x88; 32]))
            .await
            .expect("reload");
        assert_eq!(reloaded.kt.lock().await.log_size, 6);
        assert_eq!(reloaded.moderation.lock().await.log_size, 3);

        sqlx::query("DELETE FROM witness_log_state")
            .execute(&pool)
            .await
            .expect("clear witness_log_state");
        pool.close().await;
    }
}
