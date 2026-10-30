// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The directory writer: the owner's side of a directory (ADR-0086).
//!
//! A writer collects what changed and turns it into one [`DirectoryWrite`],
//! which the Delivery Service applies in one transaction. Rows and the filter
//! are held as plaintext until then and sealed when the write is built, so
//! everything in a write is sealed under the epoch that write ends on. A row
//! queued before a removal is not left readable by the person removed.
//!
//! A write carries the version it creates, one more than the directory's, and
//! the Delivery Service refuses any other. So two devices of one owner cannot
//! overwrite each other, and a device behind a removal cannot put a filter
//! under an old key back. A refused write is dropped and rebuilt from the
//! state the directory is really at. An identical write sent again is
//! answered as the first was, so a write that lost its answer is safe to
//! resend.
//!
//! ## State
//!
//! ```text
//! state := format(1) = 0x01 || version(8) || filter_version(8) || tree state
//! ```
//!
//! It holds no key ([`crate::tree`]), but it does say how many contacts the
//! owner has: keep it where other device-local data is kept.

use core::fmt::Write as _;
use std::collections::BTreeMap;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_crypto::{random, SecretBytes};

use crate::codec::Reader;
use crate::epoch::EpochKey;
use crate::error::CircleError;
use crate::filter::{seal_filter, FilterEntry, FilterStats, MAX_ROWS};
use crate::head::{DirectoryHead, MemberGrant, HEAD_LEN};
use crate::keys::{self, hex, DIRECTORY_ID_LEN, KEY_LEN, SALT_LEN};
use crate::row::{seal_row, ROW_CARD_MAX_LEN};
use crate::tree::{TreeOwner, TreeUpdate, Wrap};

const FORMAT_V1: u8 = 0x01;

/// Most rows in one write. A larger change goes over several writes. Chosen,
/// not measured.
pub const MAX_ROWS_PER_WRITE: usize = 256;

/// The proof epoch of the owner's requests to the Delivery Service. Read
/// proofs carry the directory's epoch, which starts at 1, so a proof of one
/// kind never passes as the other.
pub const OWNER_PROOF_EPOCH: u64 = 0;

/// The path of the directory `id` on the Delivery Service, without a prefix.
#[must_use]
pub fn directory_path(id: &[u8; DIRECTORY_ID_LEN]) -> String {
    format!("/ds/directory/{}", hex(id))
}

struct PendingFilter {
    entries: Vec<FilterEntry>,
    row_count: u32,
}

/// The owner's side of a directory. See the module doc.
pub struct DirectoryWriter {
    tree: TreeOwner,
    version: u64,
    filter_version: u64,
    update: TreeUpdate,
    chain: Option<Wrap>,
    rows: BTreeMap<u32, Option<Vec<u8>>>,
    filter: Option<PendingFilter>,
}

impl core::fmt::Debug for DirectoryWriter {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DirectoryWriter")
            .field("tree", &self.tree)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

/// One write, ready for the Delivery Service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryWrite {
    /// The directory.
    pub directory_id: [u8; DIRECTORY_ID_LEN],
    /// The version this write creates. 1 is the claim.
    pub version: u64,
    /// The epoch this write ends on.
    pub epoch: u64,
    /// The read key of that epoch.
    pub read_pk: [u8; KEY_LEN],
    /// The owner's public key.
    pub owner_pk: [u8; KEY_LEN],
    /// The new head.
    pub head: [u8; HEAD_LEN],
    /// The chain entry, when this write moves the epoch.
    pub chain: Option<Wrap>,
    /// Edges to write.
    pub puts: Vec<(u32, Wrap)>,
    /// Edges to delete.
    pub deletes: Vec<u32>,
    /// The sealed filter, when it changes.
    pub filter: Option<Vec<u8>>,
    /// Rows to write.
    pub rows: Vec<(u32, Vec<u8>)>,
    /// Rows to delete.
    pub row_deletes: Vec<u32>,
    /// What building the filter did, when one was built.
    pub filter_stats: Option<FilterStats>,
}

impl DirectoryWrite {
    /// Whether this is the claim, the first write of a directory.
    #[must_use]
    pub const fn is_claim(&self) -> bool {
        self.version == 1
    }

    /// The path this write is sent to.
    #[must_use]
    pub fn path(&self) -> String {
        let base = directory_path(&self.directory_id);
        if self.is_claim() {
            format!("{base}/claim")
        } else {
            format!("{base}/write")
        }
    }

    /// The HTTP method.
    #[must_use]
    pub const fn method(&self) -> &'static str {
        if self.is_claim() {
            "PUT"
        } else {
            "POST"
        }
    }

    /// The JSON body. Field names are defined here so no client spells one.
    #[must_use]
    pub fn body_json(&self) -> String {
        let mut out = format!(
            "{{\"version\":{},\"epoch\":{},\"head_b64\":\"{}\"",
            self.version,
            self.epoch,
            B64.encode(self.head)
        );
        if self.is_claim() || self.chain.is_some() {
            let _ = write!(out, ",\"read_pk_b64\":\"{}\"", B64.encode(self.read_pk));
        }
        if self.is_claim() {
            let _ = write!(out, ",\"owner_pk_b64\":\"{}\"", B64.encode(self.owner_pk));
        }
        if let Some(chain) = &self.chain {
            let _ = write!(out, ",\"chain_b64\":\"{}\"", B64.encode(chain));
        }
        if !self.puts.is_empty() {
            let items: Vec<String> = self
                .puts
                .iter()
                .map(|(node, wrap)| format!("[{node},\"{}\"]", B64.encode(wrap)))
                .collect();
            let _ = write!(out, ",\"puts\":[{}]", items.join(","));
        }
        if !self.deletes.is_empty() {
            let items: Vec<String> = self.deletes.iter().map(u32::to_string).collect();
            let _ = write!(out, ",\"deletes\":[{}]", items.join(","));
        }
        if let Some(filter) = &self.filter {
            let _ = write!(out, ",\"filter_b64\":\"{}\"", B64.encode(filter));
        }
        if !self.rows.is_empty() {
            let items: Vec<String> = self
                .rows
                .iter()
                .map(|(index, row)| format!("[{index},\"{}\"]", B64.encode(row)))
                .collect();
            let _ = write!(out, ",\"rows\":[{}]", items.join(","));
        }
        if !self.row_deletes.is_empty() {
            let items: Vec<String> = self.row_deletes.iter().map(u32::to_string).collect();
            let _ = write!(out, ",\"row_deletes\":[{}]", items.join(","));
        }
        out.push('}');
        out
    }
}

impl DirectoryWriter {
    /// A new directory for the account whose identity seed is `account_seed`.
    /// Its first [`Self::take_write`] is the claim.
    ///
    /// # Errors
    ///
    /// [`CircleError::Seal`] on a cipher failure.
    pub fn create(account_seed: &[u8; KEY_LEN]) -> Result<Self, CircleError> {
        let mut salt = [0u8; SALT_LEN];
        random::fill(&mut salt);
        Self::create_with_salt(account_seed, salt)
    }

    /// [`Self::create`] with a given salt, for tests and goldens.
    pub(crate) fn create_with_salt(
        account_seed: &[u8; KEY_LEN],
        salt: [u8; SALT_LEN],
    ) -> Result<Self, CircleError> {
        let (tree, update) = TreeOwner::create(keys::owner_seed(account_seed), salt)?;
        Ok(Self {
            tree,
            version: 0,
            filter_version: 0,
            update,
            chain: None,
            rows: BTreeMap::new(),
            filter: None,
        })
    }

    /// A writer from [`Self::encode_state`].
    ///
    /// # Errors
    ///
    /// [`CircleError::Length`], [`CircleError::UnknownFormat`] or
    /// [`CircleError::Malformed`].
    pub fn restore(account_seed: &[u8; KEY_LEN], state: &[u8]) -> Result<Self, CircleError> {
        let mut r = Reader::new(state);
        if r.u8()? != FORMAT_V1 {
            return Err(CircleError::UnknownFormat);
        }
        let version = r.u64()?;
        let filter_version = r.u64()?;
        if version == 0 {
            return Err(CircleError::Malformed);
        }
        let tree = TreeOwner::decode_state(keys::owner_seed(account_seed), r.take(r.remaining())?)?;
        Ok(Self {
            tree,
            version,
            filter_version,
            update: TreeUpdate::default(),
            chain: None,
            rows: BTreeMap::new(),
            filter: None,
        })
    }

    /// The state to keep. Pending changes are not part of it.
    #[must_use]
    pub fn encode_state(&self) -> Vec<u8> {
        let mut out = vec![FORMAT_V1];
        out.extend_from_slice(&self.version.to_be_bytes());
        out.extend_from_slice(&self.filter_version.to_be_bytes());
        out.extend_from_slice(&self.tree.encode_state());
        out
    }

    /// The directory.
    #[must_use]
    pub const fn directory_id(&self) -> &[u8; DIRECTORY_ID_LEN] {
        self.tree.directory_id()
    }

    /// The owner's public key.
    #[must_use]
    pub fn owner_public_key(&self) -> [u8; KEY_LEN] {
        keys::owner_public_key(self.tree.owner_seed())
    }

    /// The Ed25519 seed that signs this owner's requests, at
    /// [`OWNER_PROOF_EPOCH`].
    #[must_use]
    pub fn owner_signing_seed(&self) -> SecretBytes<KEY_LEN> {
        keys::owner_signing_seed(self.tree.owner_seed())
    }

    /// The directory version the last write created.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// The current epoch.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.tree.epoch()
    }

    /// The tree holds `1 << capacity_log` leaves.
    #[must_use]
    pub const fn capacity_log(&self) -> u8 {
        self.tree.capacity_log()
    }

    /// The leaves assigned to contacts, ascending.
    #[must_use]
    pub fn members(&self) -> Vec<u32> {
        self.tree.members().iter().copied().collect()
    }

    /// The current epoch key.
    #[must_use]
    pub fn epoch_key(&self) -> EpochKey {
        self.tree.epoch_key()
    }

    /// Give a contact a free leaf. Needs a write only when the tree doubled.
    ///
    /// # Errors
    ///
    /// [`CircleError::TreeFull`] or [`CircleError::Seal`].
    pub fn add_member(&mut self) -> Result<MemberGrant, CircleError> {
        let (grant, update) = self.tree.add_member()?;
        self.update.merge(update);
        Ok(grant)
    }

    /// Remove the members at `leaves`, moving the epoch.
    ///
    /// # Errors
    ///
    /// [`CircleError::WritePending`] while a write that moves the epoch has
    /// not been taken; see also [`TreeOwner::remove_members`].
    pub fn remove_members(&mut self, leaves: &[u32]) -> Result<(), CircleError> {
        if self.chain.is_some() {
            return Err(CircleError::WritePending);
        }
        let (update, chain) = self.tree.remove_members(leaves)?;
        self.update.merge(update);
        self.chain = Some(chain);
        Ok(())
    }

    /// Queue a card as row `index`.
    ///
    /// # Errors
    ///
    /// [`CircleError::CardTooLong`] or [`CircleError::RowOutOfRange`].
    pub fn put_row(&mut self, index: u32, card: Vec<u8>) -> Result<(), CircleError> {
        if card.len() > ROW_CARD_MAX_LEN {
            return Err(CircleError::CardTooLong);
        }
        if index >= MAX_ROWS {
            return Err(CircleError::RowOutOfRange);
        }
        self.rows.insert(index, Some(card));
        Ok(())
    }

    /// Queue the removal of row `index`.
    ///
    /// # Errors
    ///
    /// [`CircleError::RowOutOfRange`].
    pub fn delete_row(&mut self, index: u32) -> Result<(), CircleError> {
        if index >= MAX_ROWS {
            return Err(CircleError::RowOutOfRange);
        }
        self.rows.insert(index, None);
        Ok(())
    }

    /// Queue a new filter. `row_count` is the number of row indices in use.
    ///
    /// # Errors
    ///
    /// [`CircleError::RowOutOfRange`] for a row past `row_count`.
    pub fn set_filter(
        &mut self,
        entries: Vec<FilterEntry>,
        row_count: u32,
    ) -> Result<(), CircleError> {
        if row_count > MAX_ROWS || entries.iter().any(|e| e.row >= row_count) {
            return Err(CircleError::RowOutOfRange);
        }
        self.filter = Some(PendingFilter { entries, row_count });
        Ok(())
    }

    /// Whether anything is queued. The claim is always queued on a new writer.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.version == 0
            || !self.update.is_empty()
            || self.chain.is_some()
            || !self.rows.is_empty()
            || self.filter.is_some()
    }

    /// Seal what is queued, under the current epoch, into one write.
    ///
    /// At most [`MAX_ROWS_PER_WRITE`] rows go at a time and the rest stay
    /// queued; the filter goes with the last of them, so no filter names a row
    /// not yet written. `None` when nothing is queued.
    ///
    /// # Errors
    ///
    /// [`CircleError::Overflow`] at the last version, the filter and row
    /// errors. On an error nothing has changed and everything stays queued.
    pub fn take_write(&mut self) -> Result<Option<DirectoryWrite>, CircleError> {
        if !self.has_pending() {
            return Ok(None);
        }
        let version = self.version.checked_add(1).ok_or(CircleError::Overflow)?;
        let key = self.tree.epoch_key();

        let mut rows = Vec::new();
        let mut row_deletes = Vec::new();
        let indices: Vec<u32> = self.rows.keys().copied().take(MAX_ROWS_PER_WRITE).collect();
        for index in &indices {
            match self.rows.get(index) {
                Some(Some(card)) => rows.push((*index, seal_row(&key, *index, card)?)),
                Some(None) => row_deletes.push(*index),
                None => {},
            }
        }

        let mut filter_version = self.filter_version;
        let mut filter = None;
        let mut filter_stats = None;
        let rows_done = self.rows.len() <= MAX_ROWS_PER_WRITE;
        if rows_done {
            if let Some(pending) = &self.filter {
                filter_version = filter_version.checked_add(1).ok_or(CircleError::Overflow)?;
                let (wire, stats) =
                    seal_filter(&key, filter_version, &pending.entries, pending.row_count)?;
                filter = Some(wire);
                filter_stats = Some(stats);
            }
        }

        let head = DirectoryHead {
            epoch: self.tree.epoch(),
            version,
            filter_version,
            read_pk: key.read_public_key(),
            capacity_log: self.tree.capacity_log(),
        };
        let update = core::mem::take(&mut self.update);
        let write = DirectoryWrite {
            directory_id: *self.tree.directory_id(),
            version,
            epoch: self.tree.epoch(),
            read_pk: head.read_pk,
            owner_pk: self.owner_public_key(),
            head: head.encode(),
            chain: self.chain.take(),
            puts: update.puts.into_iter().collect(),
            deletes: update.deletes.into_iter().collect(),
            filter,
            rows,
            row_deletes,
            filter_stats,
        };
        for index in indices {
            self.rows.remove(&index);
        }
        if rows_done {
            self.filter = None;
        }
        self.version = version;
        self.filter_version = filter_version;
        Ok(Some(write))
    }
}
