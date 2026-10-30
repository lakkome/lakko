// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen exposure of `lakko-circle` - circle directories ([ADR-0086]).
//!
//! Mirror of `lakko-ffi-uniffi/src/circle.rs`; the names are the same. The
//! owner's side is [`CircleWriter`]; a contact's side starts from a grant:
//! `circleDecodeHead`, `circlePathNodes`, `circleMemberEpochKey`, then
//! [`CircleEpochKey`] signs reads, walks the chain and opens what a read
//! returned, and [`CircleFilter`] tests a term tag, locally.
//!
//! ## Shapes
//!
//! Lists of fixed-length values cross flat: `setFilter(tags, rows, rowCount)`
//! takes the tags concatenated, 32 bytes each, and `circleMemberEpochKey(grant,
//! head, nodes, wraps)` the wraps concatenated, 60 bytes each, `nodes[i]` going
//! with the `i`-th. 64-bit integers are `bigint`.
//!
//! ## Errors
//!
//! Every refusal throws an `Error` whose message is `"<code>: <reason>"`. The
//! code is the frozen `lakko_circle::CircleError::code()`, or `keyLength`,
//! `tagLength`, `wrapLength` for an argument of the wrong length.
//!
//! [ADR-0086]: ../../../docs/adr/0086-circle-directories.md

use std::fmt;

use lakko_circle::{
    directory_path, filter_header, member_epoch_key, open_filter, open_row, path_nodes, row_epoch,
    CircleError, DirectoryHead, DirectoryWrite, DirectoryWriter, EpochKey, FilterEntry,
    MemberGrant, OpenedFilter, Wrap, GRANT_LEN, MAX_ROWS_PER_TERM, MAX_ROWS_PER_WRITE,
    OWNER_PROOF_EPOCH, ROW_CARD_MAX_LEN, TAG_LEN_BYTES, WRAP_LEN,
};
use lakko_crypto::SecretBytes;
use lakko_ds_proof::DsProofSigner as InnerSigner;
use wasm_bindgen::prelude::*;

use crate::ds_proof::DsProofSigner;
use crate::mls::Identity;

/// A refusal on its way to a thrown `Error`: `"<code>: <reason>"`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Refusal(String);

impl Refusal {
    fn new(code: &str, reason: impl fmt::Display) -> Self {
        Self(format!("{code}: {reason}"))
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refusal {}

impl From<CircleError> for Refusal {
    fn from(e: CircleError) -> Self {
        Self::new(e.code(), e)
    }
}

fn fixed<const N: usize>(bytes: &[u8], code: &str, what: &str) -> Result<[u8; N], Refusal> {
    <[u8; N]>::try_from(bytes).map_err(|_| {
        Refusal::new(
            code,
            format_args!("{what} must be {N} bytes, got {}", bytes.len()),
        )
    })
}

/// A leaf given to a contact.
#[wasm_bindgen]
pub struct CircleGrant {
    leaf: u32,
    grant: Vec<u8>,
}

#[wasm_bindgen]
impl CircleGrant {
    /// The contact's leaf. Keep it beside the contact: a removal names it.
    #[wasm_bindgen(getter)]
    pub fn leaf(&self) -> u32 {
        self.leaf
    }

    /// The grant to send, once, in the one-to-one conversation. Secret.
    #[wasm_bindgen(getter)]
    pub fn grant(&self) -> Vec<u8> {
        self.grant.clone()
    }
}

/// What a grant says, without its key.
#[wasm_bindgen]
pub struct CircleGrantInfo {
    directory_id: Vec<u8>,
    leaf: u32,
}

#[wasm_bindgen]
impl CircleGrantInfo {
    /// The directory it opens.
    #[wasm_bindgen(getter, js_name = directoryId)]
    pub fn directory_id(&self) -> Vec<u8> {
        self.directory_id.clone()
    }

    /// The leaf.
    #[wasm_bindgen(getter)]
    pub fn leaf(&self) -> u32 {
        self.leaf
    }
}

/// A decoded head.
#[wasm_bindgen]
pub struct CircleHead {
    inner: DirectoryHead,
}

#[wasm_bindgen]
impl CircleHead {
    /// The current epoch.
    #[wasm_bindgen(getter)]
    pub fn epoch(&self) -> u64 {
        self.inner.epoch
    }

    /// The directory version.
    #[wasm_bindgen(getter)]
    pub fn version(&self) -> u64 {
        self.inner.version
    }

    /// The current filter's version; 0 until one is written.
    #[wasm_bindgen(getter, js_name = filterVersion)]
    pub fn filter_version(&self) -> u64 {
        self.inner.filter_version
    }

    /// The current read key.
    #[wasm_bindgen(getter, js_name = readPublicKey)]
    pub fn read_public_key(&self) -> Vec<u8> {
        self.inner.read_pk.to_vec()
    }

    /// The tree holds `1 << capacityLog` leaves.
    #[wasm_bindgen(getter, js_name = capacityLog)]
    pub fn capacity_log(&self) -> u8 {
        self.inner.capacity_log
    }
}

/// One write, ready to send.
#[wasm_bindgen]
pub struct CircleWrite {
    inner: DirectoryWrite,
}

#[wasm_bindgen]
impl CircleWrite {
    /// `PUT` for the claim, which also needs a `circle-claim` token, else
    /// `POST`.
    #[wasm_bindgen(getter)]
    pub fn method(&self) -> String {
        self.inner.method().to_string()
    }

    /// The service path.
    #[wasm_bindgen(getter)]
    pub fn path(&self) -> String {
        self.inner.path()
    }

    /// The JSON body. Send it again unchanged when the answer is lost.
    #[wasm_bindgen(getter, js_name = bodyJson)]
    pub fn body_json(&self) -> String {
        self.inner.body_json()
    }

    /// The directory version this creates.
    #[wasm_bindgen(getter)]
    pub fn version(&self) -> u64 {
        self.inner.version
    }

    /// The epoch this ends on.
    #[wasm_bindgen(getter)]
    pub fn epoch(&self) -> u64 {
        self.inner.epoch
    }

    /// Whether this is the claim.
    #[wasm_bindgen(getter, js_name = isClaim)]
    pub fn is_claim(&self) -> bool {
        self.inner.is_claim()
    }
}

/// The owner's side of a directory.
#[wasm_bindgen]
pub struct CircleWriter {
    inner: DirectoryWriter,
}

fn try_writer_create(account_identity: &Identity) -> Result<CircleWriter, Refusal> {
    let seed = SecretBytes::new(account_identity.mls_identity().ed25519_seed_bytes());
    Ok(CircleWriter {
        inner: DirectoryWriter::create(seed.as_bytes())?,
    })
}

fn try_writer_restore(account_identity: &Identity, state: &[u8]) -> Result<CircleWriter, Refusal> {
    let seed = SecretBytes::new(account_identity.mls_identity().ed25519_seed_bytes());
    Ok(CircleWriter {
        inner: DirectoryWriter::restore(seed.as_bytes(), state)?,
    })
}

/// A new directory for the account. `accountIdentity` is the ACCOUNT identity,
/// the same on every device. Its first write is the claim.
#[wasm_bindgen(js_name = circleWriterCreate)]
pub fn circle_writer_create(account_identity: &Identity) -> Result<CircleWriter, JsError> {
    Ok(try_writer_create(account_identity)?)
}

/// A writer from its kept state.
#[wasm_bindgen(js_name = circleWriterRestore)]
pub fn circle_writer_restore(
    account_identity: &Identity,
    state: &[u8],
) -> Result<CircleWriter, JsError> {
    Ok(try_writer_restore(account_identity, state)?)
}

impl CircleWriter {
    fn try_set_filter(&mut self, tags: &[u8], rows: &[u32], row_count: u32) -> Result<(), Refusal> {
        if tags.len() % TAG_LEN_BYTES != 0 || tags.len() / TAG_LEN_BYTES != rows.len() {
            return Err(Refusal::new(
                "tagLength",
                format_args!("{} bytes of tags for {} rows", tags.len(), rows.len()),
            ));
        }
        let mut entries = Vec::with_capacity(rows.len());
        for (tag, row) in tags.chunks_exact(TAG_LEN_BYTES).zip(rows) {
            entries.push(FilterEntry {
                tag: fixed::<TAG_LEN_BYTES>(tag, "tagLength", "a term tag")?,
                row: *row,
            });
        }
        Ok(self.inner.set_filter(entries, row_count)?)
    }
}

#[wasm_bindgen]
impl CircleWriter {
    /// The directory id.
    #[wasm_bindgen(js_name = directoryId)]
    pub fn directory_id(&self) -> Vec<u8> {
        self.inner.directory_id().to_vec()
    }

    /// The owner's public key.
    #[wasm_bindgen(js_name = ownerPublicKey)]
    pub fn owner_public_key(&self) -> Vec<u8> {
        self.inner.owner_public_key().to_vec()
    }

    /// The signer of this owner's requests, at `circleOwnerProofEpoch()`.
    #[wasm_bindgen(js_name = ownerSigner)]
    pub fn owner_signer(&self) -> DsProofSigner {
        DsProofSigner::from_inner(InnerSigner::for_group(
            OWNER_PROOF_EPOCH,
            self.inner.owner_signing_seed().as_bytes(),
        ))
    }

    /// The state to keep. Holds no key. Keep it beside other device-local
    /// data: it says how many contacts the owner has.
    #[wasm_bindgen(js_name = encodeState)]
    pub fn encode_state(&self) -> Vec<u8> {
        self.inner.encode_state()
    }

    /// The directory version the last write created.
    #[wasm_bindgen(getter)]
    pub fn version(&self) -> u64 {
        self.inner.version()
    }

    /// The current epoch.
    #[wasm_bindgen(getter)]
    pub fn epoch(&self) -> u64 {
        self.inner.epoch()
    }

    /// The tree holds `1 << capacityLog` leaves.
    #[wasm_bindgen(getter, js_name = capacityLog)]
    pub fn capacity_log(&self) -> u8 {
        self.inner.capacity_log()
    }

    /// The leaves assigned to contacts, ascending.
    pub fn members(&self) -> Vec<u32> {
        self.inner.members()
    }

    /// The current epoch key.
    #[wasm_bindgen(js_name = epochKey)]
    pub fn epoch_key(&self) -> CircleEpochKey {
        CircleEpochKey {
            inner: self.inner.epoch_key(),
        }
    }

    /// Give a contact a free leaf.
    #[wasm_bindgen(js_name = addMember)]
    pub fn add_member(&mut self) -> Result<CircleGrant, JsError> {
        let grant = self.inner.add_member().map_err(Refusal::from)?;
        Ok(CircleGrant {
            leaf: grant.leaf,
            grant: grant.encode().to_vec(),
        })
    }

    /// Remove the contacts at `leaves`, moving the epoch.
    #[wasm_bindgen(js_name = removeMembers)]
    pub fn remove_members(&mut self, leaves: &[u32]) -> Result<(), JsError> {
        Ok(self.inner.remove_members(leaves).map_err(Refusal::from)?)
    }

    /// Queue a card as row `index`.
    #[wasm_bindgen(js_name = putRow)]
    pub fn put_row(&mut self, index: u32, card: Vec<u8>) -> Result<(), JsError> {
        Ok(self.inner.put_row(index, card).map_err(Refusal::from)?)
    }

    /// Queue the removal of row `index`.
    #[wasm_bindgen(js_name = deleteRow)]
    pub fn delete_row(&mut self, index: u32) -> Result<(), JsError> {
        Ok(self.inner.delete_row(index).map_err(Refusal::from)?)
    }

    /// Queue a new filter: `tags` concatenated, 32 bytes each, `rows[i]` going
    /// with the `i`-th. `rowCount` is the number of row indices in use.
    #[wasm_bindgen(js_name = setFilter)]
    pub fn set_filter(&mut self, tags: &[u8], rows: &[u32], row_count: u32) -> Result<(), JsError> {
        Ok(self.try_set_filter(tags, rows, row_count)?)
    }

    /// Whether anything is queued.
    #[wasm_bindgen(js_name = hasPending)]
    pub fn has_pending(&self) -> bool {
        self.inner.has_pending()
    }

    /// Seal what is queued into one write, under the current epoch. Call until
    /// it returns `undefined`: a large change goes over several writes.
    #[wasm_bindgen(js_name = takeWrite)]
    pub fn take_write(&mut self) -> Result<Option<CircleWrite>, JsError> {
        Ok(self
            .inner
            .take_write()
            .map_err(Refusal::from)?
            .map(|inner| CircleWrite { inner }))
    }
}

/// The key of one epoch. It crosses as an object, never as bytes.
#[wasm_bindgen]
pub struct CircleEpochKey {
    inner: EpochKey,
}

impl CircleEpochKey {
    fn try_previous(&self, chain_wrap: &[u8]) -> Result<Self, Refusal> {
        let wrap: Wrap = fixed::<WRAP_LEN>(chain_wrap, "wrapLength", "a chain entry")?;
        Ok(Self {
            inner: self.inner.previous(&wrap)?,
        })
    }
}

#[wasm_bindgen]
impl CircleEpochKey {
    /// The epoch.
    #[wasm_bindgen(getter)]
    pub fn epoch(&self) -> u64 {
        self.inner.epoch()
    }

    /// The signer of reads at this epoch. Only the current epoch's is
    /// accepted; an older key is for opening what it sealed.
    #[wasm_bindgen(js_name = readSigner)]
    pub fn read_signer(&self) -> DsProofSigner {
        DsProofSigner::from_inner(InnerSigner::for_group(
            self.inner.epoch(),
            self.inner.read_seed().as_bytes(),
        ))
    }

    /// The epoch key before this one, from this epoch's chain entry.
    pub fn previous(&self, chain_wrap: &[u8]) -> Result<CircleEpochKey, JsError> {
        Ok(self.try_previous(chain_wrap)?)
    }

    /// Open a filter sealed at this epoch.
    #[wasm_bindgen(js_name = openFilter)]
    pub fn open_filter(&self, wire: &[u8]) -> Result<CircleFilter, JsError> {
        Ok(CircleFilter {
            inner: open_filter(&self.inner, wire).map_err(Refusal::from)?,
        })
    }

    /// Open row `index`, sealed at this epoch, to its card.
    #[wasm_bindgen(js_name = openRow)]
    pub fn open_row(&self, index: u32, row: &[u8]) -> Result<Vec<u8>, JsError> {
        Ok(open_row(&self.inner, index, row).map_err(Refusal::from)?)
    }
}

/// An opened filter.
#[wasm_bindgen]
pub struct CircleFilter {
    inner: OpenedFilter,
}

impl CircleFilter {
    fn try_lookup(&self, tag: &[u8]) -> Result<Vec<u32>, Refusal> {
        let tag = fixed::<TAG_LEN_BYTES>(tag, "tagLength", "a term tag")?;
        Ok(self.inner.lookup(&tag))
    }
}

#[wasm_bindgen]
impl CircleFilter {
    /// The rows this term tag is on. Local. Never misses a stored term; a
    /// false positive is a row that does not carry the term, which the caller
    /// sees when it opens the card.
    pub fn lookup(&self, tag: &[u8]) -> Result<Vec<u32>, JsError> {
        Ok(self.try_lookup(tag)?)
    }

    /// Row indices in use.
    #[wasm_bindgen(getter, js_name = rowCount)]
    pub fn row_count(&self) -> u32 {
        self.inner.row_count()
    }
}

fn try_member_epoch_key(
    grant: &[u8],
    head: &[u8],
    nodes: &[u32],
    wraps: &[u8],
) -> Result<CircleEpochKey, Refusal> {
    let grant = MemberGrant::decode(grant)?;
    let head = DirectoryHead::decode(head)?;
    if wraps.len() % WRAP_LEN != 0 || wraps.len() / WRAP_LEN != nodes.len() {
        return Err(Refusal::new(
            "wrapLength",
            format_args!("{} bytes of wraps for {} nodes", wraps.len(), nodes.len()),
        ));
    }
    let mut edges = Vec::with_capacity(nodes.len());
    for (wrap, node) in wraps.chunks_exact(WRAP_LEN).zip(nodes) {
        edges.push((*node, fixed::<WRAP_LEN>(wrap, "wrapLength", "an edge")?));
    }
    Ok(CircleEpochKey {
        inner: member_epoch_key(&grant, &head, &edges)?,
    })
}

/// Derive the current epoch key from a grant, the encoded head and the edges
/// of the grant's path. A `headerMismatch` refusal means the header is stale or
/// partial, or this contact was removed.
#[wasm_bindgen(js_name = circleMemberEpochKey)]
pub fn circle_member_epoch_key(
    grant: &[u8],
    head: &[u8],
    nodes: &[u32],
    wraps: &[u8],
) -> Result<CircleEpochKey, JsError> {
    Ok(try_member_epoch_key(grant, head, nodes, wraps)?)
}

/// Decode a head.
#[wasm_bindgen(js_name = circleDecodeHead)]
pub fn circle_decode_head(head: &[u8]) -> Result<CircleHead, JsError> {
    Ok(CircleHead {
        inner: DirectoryHead::decode(head).map_err(Refusal::from)?,
    })
}

/// What a grant names, without its key.
#[wasm_bindgen(js_name = circleGrantInfo)]
pub fn circle_grant_info(grant: &[u8]) -> Result<CircleGrantInfo, JsError> {
    let grant = MemberGrant::decode(grant).map_err(Refusal::from)?;
    Ok(CircleGrantInfo {
        directory_id: grant.directory_id.to_vec(),
        leaf: grant.leaf,
    })
}

/// The nodes whose edges a member at `leaf` reads.
#[wasm_bindgen(js_name = circlePathNodes)]
pub fn circle_path_nodes(leaf: u32, capacity_log: u8) -> Result<Vec<u32>, JsError> {
    Ok(path_nodes(leaf, capacity_log).map_err(Refusal::from)?)
}

/// The epoch whose key opens this sealed filter.
#[wasm_bindgen(js_name = circleFilterEpoch)]
pub fn circle_filter_epoch(wire: &[u8]) -> Result<u64, JsError> {
    Ok(filter_header(wire).map_err(Refusal::from)?.epoch)
}

/// The epoch whose key opens this sealed row.
#[wasm_bindgen(js_name = circleRowEpoch)]
pub fn circle_row_epoch(row: &[u8]) -> Result<u64, JsError> {
    Ok(row_epoch(row).map_err(Refusal::from)?)
}

/// The path of a directory on the Delivery Service, without a prefix. Its
/// routes hang off it: `/header`, `/filter`, `/rows/<index>`.
#[wasm_bindgen(js_name = circleDirectoryPath)]
pub fn circle_directory_path(directory_id: &[u8]) -> Result<String, JsError> {
    Ok(directory_path(&fixed::<32>(
        directory_id,
        "keyLength",
        "a directory id",
    )?))
}

/// The proof epoch of an owner's requests.
#[wasm_bindgen(js_name = circleOwnerProofEpoch)]
#[must_use]
pub fn circle_owner_proof_epoch() -> u64 {
    OWNER_PROOF_EPOCH
}

/// The length of a grant.
#[wasm_bindgen(js_name = circleGrantLen)]
#[must_use]
pub fn circle_grant_len() -> u32 {
    #[allow(clippy::cast_possible_truncation)] // a small constant
    let value = GRANT_LEN as u32;
    value
}

/// The longest card a row carries.
#[wasm_bindgen(js_name = circleRowCardMaxLen)]
#[must_use]
pub fn circle_row_card_max_len() -> u32 {
    #[allow(clippy::cast_possible_truncation)] // a small constant
    let value = ROW_CARD_MAX_LEN as u32;
    value
}

/// The most rows a lookup returns.
#[wasm_bindgen(js_name = circleMaxRowsPerTerm)]
#[must_use]
pub fn circle_max_rows_per_term() -> u32 {
    #[allow(clippy::cast_possible_truncation)] // a small constant
    let value = MAX_ROWS_PER_TERM as u32;
    value
}

/// The most rows one write carries.
#[wasm_bindgen(js_name = circleMaxRowsPerWrite)]
#[must_use]
pub fn circle_max_rows_per_write() -> u32 {
    #[allow(clippy::cast_possible_truncation)] // a small constant
    let value = MAX_ROWS_PER_WRITE as u32;
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Identity {
        Identity::from_seed(&[3; 64]).unwrap()
    }

    #[test]
    fn a_grant_opens_the_directory_it_names() {
        let mut writer = try_writer_create(&identity()).unwrap();
        let grant = writer.inner.add_member().unwrap();
        let claim = writer.inner.take_write().unwrap().unwrap();
        let head = claim.head;
        let path = path_nodes(
            grant.leaf,
            DirectoryHead::decode(&head).unwrap().capacity_log,
        )
        .unwrap();
        let mut nodes = Vec::new();
        let mut wraps = Vec::new();
        for (node, wrap) in &claim.puts {
            if path.contains(node) {
                nodes.push(*node);
                wraps.extend_from_slice(wrap);
            }
        }
        let key = try_member_epoch_key(&grant.encode(), &head, &nodes, &wraps).unwrap();
        assert_eq!(key.epoch(), 1);
        assert_eq!(key.read_signer().epoch(), 1);
        assert_eq!(
            writer.owner_signer().verifying_key(),
            writer.owner_public_key()
        );
    }

    #[test]
    fn flat_lists_are_checked_against_each_other() {
        let mut writer = try_writer_create(&identity()).unwrap();
        let refused = writer.try_set_filter(&[0; 31], &[0], 1).unwrap_err();
        assert!(refused.0.starts_with("tagLength:"));
        writer.try_set_filter(&[0; 32], &[0], 1).unwrap();
        let grant = writer.inner.add_member().unwrap();
        let head = writer.inner.take_write().unwrap().unwrap().head;
        let refused = try_member_epoch_key(&grant.encode(), &head, &[2], &[0; 59])
            .err()
            .unwrap();
        assert!(refused.0.starts_with("wrapLength:"));
    }

    #[test]
    fn a_state_restores() {
        let id = identity();
        let mut writer = try_writer_create(&id).unwrap();
        writer.inner.add_member().unwrap();
        writer.inner.take_write().unwrap();
        let again = try_writer_restore(&id, &writer.encode_state()).unwrap();
        assert_eq!(again.members(), vec![0]);
        assert!(try_writer_restore(&id, &[1, 2, 3]).is_err());
    }
}
