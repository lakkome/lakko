// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The file-backed vault index: the vault rows and committed object versions
//! in the shared log (`V`, `O`, `X` records), reservations and the write-rate
//! window in memory. The mutex of [`FileIndex`] is the conditional update.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use async_trait::async_trait;

use super::{append, parse_hex32, FileIndex};
use crate::index::{
    decide_reserve, hour_of, new_token, ClaimOutcome, CommitOutcome, IndexError, ObjectAddr,
    ObjectRow, ReservationToken, ReserveFacts, ReserveOutcome, VaultId, VaultIndex, VaultLimits,
    VaultRow, WriteRequest,
};

/// A live reservation, in memory only.
#[derive(Debug, Clone, Copy)]
struct Reservation {
    version: u64,
    token: ReservationToken,
    size: u64,
    until_ms: u64,
}

/// One object: its committed version (0 = none) and any reservation.
#[derive(Debug, Clone, Copy, Default)]
struct FileObject {
    version: u64,
    token: Option<ReservationToken>,
    size: u64,
    reservation: Option<Reservation>,
}

impl FileObject {
    fn live_reservation(&self, now_ms: u64) -> Option<&Reservation> {
        self.reservation.as_ref().filter(|r| r.until_ms > now_ms)
    }
}

/// One vault.
#[derive(Debug, Clone)]
pub(super) struct FileVault {
    row: VaultRow,
    objects: BTreeMap<ObjectAddr, FileObject>,
    window_hour: u64,
    window_writes: u32,
}

impl FileVault {
    /// The `V` and `O` lines that rebuild this vault.
    pub(super) fn compact_lines(&self) -> String {
        let mut out = claim_line(&self.row);
        for (addr, o) in &self.objects {
            if let (true, Some(token)) = (o.version > 0, o.token) {
                out.push_str(&object_line(&self.row.id, *addr, o.version, &token, o.size));
            }
        }
        out
    }
}

fn claim_line(row: &VaultRow) -> String {
    format!(
        "V {} {} {} {}\n",
        hex::encode(row.id),
        hex::encode(row.write_pk),
        row.node,
        row.created_day
    )
}

fn object_line(
    id: &VaultId,
    addr: ObjectAddr,
    version: u64,
    token: &ReservationToken,
    size: u64,
) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "O {} {} {} {version} {} {size}",
        hex::encode(id),
        addr.kind_byte(),
        addr.chunk_no(),
        hex::encode(token)
    );
    s
}

fn parse_token(s: &str) -> Option<ReservationToken> {
    let v = hex::decode(s).ok()?;
    <ReservationToken>::try_from(v.as_slice()).ok()
}

/// Apply one `V`, `O` or `X` record of the log. `None` when the record does
/// not parse or does not fit the state (an object of an unknown vault, a
/// second claim of a vault).
pub(super) fn replay_line(vaults: &mut HashMap<VaultId, FileVault>, f: &[&str]) -> Option<()> {
    match f {
        ["V", id, pk, node, day] => {
            let id = parse_hex32(id)?;
            if vaults.contains_key(&id) {
                return None;
            }
            vaults.insert(
                id,
                FileVault {
                    row: VaultRow {
                        id,
                        write_pk: parse_hex32(pk)?,
                        node: (*node).to_string(),
                        created_day: day.parse().ok()?,
                        bytes_used: 0,
                    },
                    objects: BTreeMap::new(),
                    window_hour: 0,
                    window_writes: 0,
                },
            );
            Some(())
        },
        ["O", id, kind, chunk, version, token, size] => {
            let v = vaults.get_mut(&parse_hex32(id)?)?;
            let addr = ObjectAddr::from_parts(kind.parse().ok()?, chunk.parse().ok()?)?;
            let version: u64 = version.parse().ok()?;
            let size: u64 = size.parse().ok()?;
            if version == 0 {
                return None;
            }
            let o = v.objects.entry(addr).or_default();
            v.row.bytes_used = v.row.bytes_used.saturating_sub(o.size).saturating_add(size);
            o.version = version;
            o.token = Some(parse_token(token)?);
            o.size = size;
            Some(())
        },
        ["X", id] => {
            vaults.remove(&parse_hex32(id)?);
            Some(())
        },
        _ => None,
    }
}

#[async_trait]
impl VaultIndex for FileIndex {
    async fn vault(&self, id: &VaultId) -> Result<Option<VaultRow>, IndexError> {
        Ok(self.lock().vaults.get(id).map(|v| v.row.clone()))
    }

    async fn claim(&self, row: VaultRow) -> Result<ClaimOutcome, IndexError> {
        let mut g = self.lock();
        let inner = &mut *g;
        if let Some(existing) = inner.vaults.get(&row.id) {
            return Ok(if existing.row.write_pk == row.write_pk {
                ClaimOutcome::SameKey
            } else {
                ClaimOutcome::OtherKey
            });
        }
        append(&mut inner.log, &claim_line(&row), true)?;
        inner.vaults.insert(
            row.id,
            FileVault {
                row: VaultRow {
                    bytes_used: 0,
                    ..row
                },
                objects: BTreeMap::new(),
                window_hour: 0,
                window_writes: 0,
            },
        );
        Ok(ClaimOutcome::Created)
    }

    async fn unclaim(&self, id: &VaultId) -> Result<bool, IndexError> {
        let mut g = self.lock();
        let inner = &mut *g;
        match inner.vaults.get(id) {
            Some(v) if v.objects.is_empty() => {
                append(&mut inner.log, &format!("X {}\n", hex::encode(id)), true)?;
                inner.vaults.remove(id);
                Ok(true)
            },
            _ => Ok(false),
        }
    }

    async fn object(
        &self,
        id: &VaultId,
        addr: ObjectAddr,
    ) -> Result<Option<ObjectRow>, IndexError> {
        let g = self.lock();
        Ok(g.vaults
            .get(id)
            .and_then(|v| v.objects.get(&addr))
            .and_then(|o| match (o.version, o.token) {
                (0, _) | (_, None) => None,
                (version, Some(token)) => Some(ObjectRow {
                    version,
                    token,
                    size: o.size,
                }),
            }))
    }

    async fn reserve(
        &self,
        id: &VaultId,
        req: WriteRequest,
        limits: &VaultLimits,
    ) -> Result<ReserveOutcome, IndexError> {
        let mut g = self.lock();
        let Some(v) = g.vaults.get_mut(id) else {
            return Ok(ReserveOutcome::NoVault);
        };
        let hour = hour_of(req.now_ms);
        let object = v.objects.get(&req.addr);
        let facts = ReserveFacts {
            object: object.map(|o| (o.version, o.size)),
            object_reserved: object.is_some_and(|o| o.live_reservation(req.now_ms).is_some()),
            chunk_rows: u64::try_from(
                v.objects
                    .keys()
                    .filter(|a| matches!(a, ObjectAddr::Chunk(_)))
                    .count(),
            )
            .unwrap_or(u64::MAX),
            bytes_used: v.row.bytes_used,
            other_reserved_bytes: v
                .objects
                .iter()
                .filter(|(a, _)| **a != req.addr)
                .filter_map(|(_, o)| o.live_reservation(req.now_ms))
                .fold(0u64, |acc, r| acc.saturating_add(r.size)),
            writes_this_hour: if v.window_hour == hour {
                v.window_writes
            } else {
                0
            },
        };
        if let Err(refusal) = decide_reserve(&req, &facts, limits) {
            return Ok(refusal);
        }
        let token = new_token();
        v.objects.entry(req.addr).or_default().reservation = Some(Reservation {
            version: req.version,
            token,
            size: req.size,
            until_ms: req.now_ms.saturating_add(limits.reservation_ms),
        });
        v.window_hour = hour;
        v.window_writes = facts.writes_this_hour.saturating_add(1);
        Ok(ReserveOutcome::Reserved(token))
    }

    async fn commit(
        &self,
        id: &VaultId,
        addr: ObjectAddr,
        token: &ReservationToken,
        size: u64,
    ) -> Result<CommitOutcome, IndexError> {
        let mut g = self.lock();
        let inner = &mut *g;
        let Some(v) = inner.vaults.get_mut(id) else {
            return Ok(CommitOutcome::Lost);
        };
        let Some(o) = v.objects.get_mut(&addr) else {
            return Ok(CommitOutcome::Lost);
        };
        let Some(r) = o.reservation.filter(|r| r.token == *token) else {
            return Ok(CommitOutcome::Lost);
        };
        append(
            &mut inner.log,
            &object_line(id, addr, r.version, token, size),
            true,
        )?;
        let replaced = match (o.version, o.token) {
            (0, _) | (_, None) => None,
            (version, Some(old)) => Some((version, old)),
        };
        v.row.bytes_used = v.row.bytes_used.saturating_sub(o.size).saturating_add(size);
        o.version = r.version;
        o.token = Some(*token);
        o.size = size;
        o.reservation = None;
        Ok(CommitOutcome::Committed { replaced })
    }

    async fn release(
        &self,
        id: &VaultId,
        addr: ObjectAddr,
        token: &ReservationToken,
    ) -> Result<(), IndexError> {
        let mut g = self.lock();
        let Some(v) = g.vaults.get_mut(id) else {
            return Ok(());
        };
        let Some(o) = v.objects.get_mut(&addr) else {
            return Ok(());
        };
        if o.reservation.is_some_and(|r| r.token == *token) {
            o.reservation = None;
            if o.version == 0 {
                v.objects.remove(&addr);
            }
        }
        Ok(())
    }

    async fn delete_vault(&self, id: &VaultId) -> Result<bool, IndexError> {
        let mut g = self.lock();
        let inner = &mut *g;
        if !inner.vaults.contains_key(id) {
            return Ok(false);
        }
        append(&mut inner.log, &format!("X {}\n", hex::encode(id)), true)?;
        inner.vaults.remove(id);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::contract;

    #[tokio::test]
    async fn the_file_vault_index_meets_the_contract() {
        let dir = tempfile::tempdir().unwrap();
        let index = FileIndex::open(dir.path().join("idx/blobs.log")).unwrap();
        contract::run_vault(&index).await;
    }

    #[tokio::test]
    async fn concurrent_reservations_of_one_version_yield_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        let index = std::sync::Arc::new(FileIndex::open(dir.path().join("blobs.log")).unwrap());
        contract::run_vault_race(index).await;
    }

    #[tokio::test]
    async fn committed_versions_survive_a_reopen_and_reservations_do_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blobs.log");
        let limits = contract::limits();
        let a = contract::vault_row(0x61);
        let b = contract::vault_row(0x62);
        {
            let index = FileIndex::open(path.clone()).unwrap();
            index.claim(a.clone()).await.unwrap();
            index.claim(b.clone()).await.unwrap();
            let t = contract::reserve_ok(&index, &a.id, ObjectAddr::Head, 1, 100, &limits).await;
            index
                .commit(&a.id, ObjectAddr::Head, &t, 100)
                .await
                .unwrap();
            let t = contract::reserve_ok(&index, &a.id, ObjectAddr::Chunk(0), 1, 50, &limits).await;
            index
                .commit(&a.id, ObjectAddr::Chunk(0), &t, 50)
                .await
                .unwrap();
            // A reservation that is never committed.
            contract::reserve_ok(&index, &a.id, ObjectAddr::Head, 2, 10, &limits).await;
            assert!(index.delete_vault(&b.id).await.unwrap());
        }
        let index = FileIndex::open(path).unwrap();
        let got = index.vault(&a.id).await.unwrap().unwrap();
        assert_eq!(got.bytes_used, 150);
        assert_eq!(got.write_pk, a.write_pk);
        assert_eq!(
            index
                .object(&a.id, ObjectAddr::Head)
                .await
                .unwrap()
                .unwrap()
                .version,
            1
        );
        assert_eq!(index.vault(&b.id).await.unwrap(), None);
        // The uncommitted reservation is gone: version 2 is free at once.
        contract::reserve_ok(&index, &a.id, ObjectAddr::Head, 2, 10, &limits).await;
    }

    #[test]
    fn an_object_record_of_an_unknown_vault_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blobs.log");
        let id = "ab".repeat(32);
        let token = "cd".repeat(16);
        std::fs::write(&path, format!("O {id} 1 0 1 {token} 5\n")).unwrap();
        assert!(matches!(FileIndex::open(path), Err(IndexError::Corrupt(1))));
    }
}
