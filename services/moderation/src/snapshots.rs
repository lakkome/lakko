// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The suspension snapshots this service builds and serves (ADR-0099
//! section 5, steps 4 and 5: suspended accounts and removed channels).
//!
//! Once per period (`LAKKO_MODERATION_SNAPSHOT_PERIOD_SECS`, a day by
//! default) the builder takes every leaf of the moderation log, builds the
//! snapshot of active suspensions and channel removals with
//! [`lakko_moderation::snapshot::build_snapshot`] and appends its root to
//! the log as a kind-3 leaf, right after the leaves it was built from (the
//! append is refused and retried at the next tick if the log grew in
//! between). It checks once a minute whether the current period has a
//! snapshot. The newest snapshot and the one before it are kept in memory.
//!
//! At startup the newest two kind-3 leaves are rebuilt from the log and
//! compared byte for byte with the stored roots; one that differs is
//! logged as an error and dropped, and when it was the newest a fresh
//! snapshot is built for the current period.

use std::sync::Arc;
use std::time::Duration;

use lakko_moderation::snapshot::{
    build_snapshot, period_of, Snapshot, SnapshotRoot, DEFAULT_SNAPSHOT_PERIOD_SECS,
};
use tokio::sync::Mutex;

use crate::log::{LogError, ModerationLog, LEAF_SNAPSHOT_ROOT};

/// The shortest period accepted at startup (a minute). Chosen, not
/// measured: the builder checks once a minute.
pub const MIN_SNAPSHOT_PERIOD_SECS: u32 = 60;

/// How often the builder checks for a missing snapshot.
pub const BUILD_CHECK_INTERVAL: Duration = Duration::from_secs(60);

/// How many snapshots are kept: the newest and the one before it.
const KEPT: usize = 2;

/// Parse `LAKKO_MODERATION_SNAPSHOT_PERIOD_SECS`: unset or blank is the
/// default, anything else must be a whole number of seconds of at least
/// [`MIN_SNAPSHOT_PERIOD_SECS`].
///
/// # Errors
///
/// A message naming the variable.
pub fn parse_period_secs(value: Option<&str>) -> Result<u32, String> {
    let Some(text) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(DEFAULT_SNAPSHOT_PERIOD_SECS);
    };
    let secs: u32 = text
        .parse()
        .map_err(|_| "LAKKO_MODERATION_SNAPSHOT_PERIOD_SECS is not a whole number".to_string())?;
    if secs < MIN_SNAPSHOT_PERIOD_SECS {
        return Err(format!(
            "LAKKO_MODERATION_SNAPSHOT_PERIOD_SECS must be at least {MIN_SNAPSHOT_PERIOD_SECS}"
        ));
    }
    Ok(secs)
}

/// Why a build failed.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// The log failed.
    #[error("log: {0}")]
    Log(#[from] LogError),
    /// The snapshot could not be built.
    #[error("build: {0}")]
    Build(#[from] lakko_moderation::ModerationError),
    /// The build task did not finish.
    #[error("build task failed")]
    Task,
}

/// A snapshot and the log index of its root leaf.
#[derive(Debug, Clone)]
pub struct Held {
    /// The snapshot.
    pub snapshot: Arc<Snapshot>,
    /// The index of its kind-3 leaf in the moderation log.
    pub index: u64,
}

#[derive(Debug, Default)]
struct HeldState {
    /// Oldest first; at most [`KEPT`].
    held: Vec<Held>,
    /// The newest stored snapshot did not rebuild; build one now even if a
    /// snapshot of the current period is held.
    force: bool,
}

/// The snapshots this instance holds.
#[derive(Debug)]
pub struct Snapshots {
    period_secs: u32,
    state: Mutex<HeldState>,
    build: Mutex<()>,
}

async fn build_off_thread(
    leaves: Vec<Vec<u8>>,
    built_at_ms: u64,
    period_secs: u32,
) -> Result<Snapshot, BuildError> {
    tokio::task::spawn_blocking(move || {
        let refs: Vec<&[u8]> = leaves.iter().map(Vec::as_slice).collect();
        build_snapshot(&refs, built_at_ms, period_secs)
    })
    .await
    .map_err(|_| BuildError::Task)?
    .map_err(BuildError::from)
}

impl Snapshots {
    /// No snapshot held yet.
    #[must_use]
    pub fn new(period_secs: u32) -> Self {
        Self {
            period_secs,
            state: Mutex::new(HeldState::default()),
            build: Mutex::new(()),
        }
    }

    /// The configured period length.
    #[must_use]
    pub const fn period_secs(&self) -> u32 {
        self.period_secs
    }

    /// Reload the newest two snapshots from the log, each rebuilt and
    /// compared byte for byte with its stored root. Returns how many were
    /// kept.
    pub async fn reload(&self, log: &ModerationLog) -> usize {
        let _build = self.build.lock().await;
        let leaves = log.leaves().await;
        let newest: Vec<usize> = leaves
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, l)| l.first() == Some(&LEAF_SNAPSHOT_ROOT))
            .map(|(i, _)| i)
            .take(KEPT)
            .collect();
        let mut kept = Vec::new();
        let mut force = false;
        for (rank, &index) in newest.iter().enumerate() {
            match Self::rebuild(&leaves, index).await {
                Ok(snapshot) => kept.push(Held {
                    snapshot: Arc::new(snapshot),
                    index: index as u64,
                }),
                Err(reason) => {
                    tracing::error!(
                        log_index = index,
                        reason,
                        "moderation snapshot: a stored snapshot does not rebuild; dropped"
                    );
                    if rank == 0 {
                        force = true;
                    }
                },
            }
        }
        kept.reverse();
        let count = kept.len();
        *self.state.lock().await = HeldState { held: kept, force };
        count
    }

    async fn rebuild(leaves: &[Vec<u8>], index: usize) -> Result<Snapshot, &'static str> {
        let leaf = leaves.get(index).ok_or("missing leaf")?;
        let stored = SnapshotRoot::decode_log_leaf(leaf).map_err(|_| "does not decode")?;
        let covered = usize::try_from(stored.covered_log_size).map_err(|_| "covered size")?;
        if covered > index {
            return Err("covers leaves after its own");
        }
        let snapshot = build_off_thread(
            leaves[..covered].to_vec(),
            stored.built_at_ms,
            stored.period_secs,
        )
        .await
        .map_err(|_| "does not build")?;
        if snapshot.root().log_leaf() != *leaf {
            return Err("rebuilt root differs");
        }
        Ok(snapshot)
    }

    /// Build and append a snapshot when the period of `now_ms` has none.
    /// Returns whether one was appended.
    ///
    /// # Errors
    ///
    /// [`BuildError`].
    pub async fn tick(&self, log: &ModerationLog, now_ms: u64) -> Result<bool, BuildError> {
        let _build = self.build.lock().await;
        let current = period_of(now_ms, self.period_secs);
        {
            let state = self.state.lock().await;
            let have = state
                .held
                .last()
                .is_some_and(|h| h.snapshot.root().period == current);
            if have && !state.force {
                return Ok(false);
            }
        }
        let leaves = log.leaves().await;
        let at = leaves.len() as u64;
        let snapshot = build_off_thread(leaves, now_ms, self.period_secs).await?;
        let payload = snapshot.root().encode();
        let key = lakko_crypto::hash::blake3(&payload);
        let Some(appended) = log
            .append_at(LEAF_SNAPSHOT_ROOT, key, &payload, at, now_ms)
            .await?
        else {
            // The log grew while the snapshot was built; the next tick
            // builds again over the larger log.
            return Ok(false);
        };
        tracing::info!(
            log_index = appended.index,
            period = snapshot.root().period,
            entries = snapshot.root().entry_count,
            "moderation snapshot appended"
        );
        let mut state = self.state.lock().await;
        state.held.push(Held {
            snapshot: Arc::new(snapshot),
            index: appended.index,
        });
        if state.held.len() > KEPT {
            state.held.remove(0);
        }
        state.force = false;
        Ok(true)
    }

    /// The held snapshot of `period`, or the newest one for `None`.
    pub async fn get(&self, period: Option<u64>) -> Option<Held> {
        let state = self.state.lock().await;
        match period {
            None => state.held.last().cloned(),
            Some(p) => state
                .held
                .iter()
                .rev()
                .find(|h| h.snapshot.root().period == p)
                .cloned(),
        }
    }

    /// Run the builder: a tick now, then one every
    /// [`BUILD_CHECK_INTERVAL`].
    pub fn spawn(self: Arc<Self>, log: Arc<ModerationLog>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(BUILD_CHECK_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                if let Err(e) = self.tick(&log, now_ms()).await {
                    tracing::error!(error = %e, "moderation snapshot: build failed");
                }
            }
        })
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn the_period_variable() {
        assert_eq!(parse_period_secs(None), Ok(DEFAULT_SNAPSHOT_PERIOD_SECS));
        assert_eq!(
            parse_period_secs(Some(" ")),
            Ok(DEFAULT_SNAPSHOT_PERIOD_SECS)
        );
        assert_eq!(parse_period_secs(Some("60")), Ok(60));
        assert_eq!(parse_period_secs(Some("3600")), Ok(3600));
        assert!(parse_period_secs(Some("0")).is_err());
        assert!(parse_period_secs(Some("59")).is_err());
        assert!(parse_period_secs(Some("-1")).is_err());
        assert!(parse_period_secs(Some("1h")).is_err());
    }
}
