// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI exposure of `lakko-circle` - circle directories ([ADR-0086]).
//!
//! The owner's side is [`CircleWriter`], built from the ACCOUNT identity. It
//! gives contacts their leaves, removes them, queues cards and the filter,
//! and hands each change over as a [`CircleWrite`]: a method, a path and a JSON
//! body, which the client signs with [`CircleWriter::owner_signer`] and sends.
//! Its state ([`CircleWriter::encode_state`]) holds no key.
//!
//! A contact's side starts from a grant, the 69 bytes the owner sent once:
//!
//! 1. `GET .../header` for the head ([`circle_decode_head`]);
//! 2. [`circle_path_nodes`], then `GET .../header?nodes=...`;
//! 3. [`circle_member_epoch_key`] gives a [`CircleEpochKey`], or a refusal when
//!    the header is stale or the contact was removed;
//! 4. [`CircleEpochKey::read_signer`] signs the filter and row reads;
//! 5. [`CircleEpochKey::previous`] walks the chain down to the epoch a filter
//!    or a row names ([`circle_filter_epoch`], [`circle_row_epoch`]);
//! 6. [`CircleFilter::lookup`] tests a term tag.
//!
//! The filter test is local. Nothing here touches the network.
//!
//! Every refusal is [`FfiError::Circle`] with the message `"<code>: <reason>"`.
//! The code is the frozen `lakko_circle::CircleError::code()`, or `keyLength`,
//! `tagLength`, `wrapLength` for an argument of the wrong length.
//!
//! Not on the notification extension surface: a background process does not
//! search (ADR-0086 section 4 has no background work).
//!
//! [ADR-0086]: ../../../docs/adr/0086-circle-directories.md

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lakko_circle::{
    directory_path, filter_header, member_epoch_key, open_filter, open_row, path_nodes, row_epoch,
    CircleError, DirectoryHead, DirectoryWrite, DirectoryWriter, EpochKey, FilterEntry,
    MemberGrant, OpenedFilter, Wrap, GRANT_LEN, MAX_ROWS_PER_TERM, MAX_ROWS_PER_WRITE,
    OWNER_PROOF_EPOCH, ROW_CARD_MAX_LEN, TAG_LEN_BYTES, WRAP_LEN,
};
use lakko_crypto::SecretBytes;
use lakko_ds_proof::DsProofSigner as InnerSigner;

use crate::ds_proof::DsProofSigner;
use crate::{FfiError, Identity};

fn refusal(code: &str, reason: impl std::fmt::Display) -> FfiError {
    FfiError::Circle(format!("{code}: {reason}"))
}

impl From<CircleError> for FfiError {
    fn from(e: CircleError) -> Self {
        refusal(e.code(), e)
    }
}

fn fixed<const N: usize>(bytes: &[u8], code: &str, what: &str) -> Result<[u8; N], FfiError> {
    <[u8; N]>::try_from(bytes).map_err(|_| {
        refusal(
            code,
            format_args!("{what} must be {N} bytes, got {}", bytes.len()),
        )
    })
}

/// A leaf given to a contact.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CircleGrant {
    /// The contact's leaf. Keep it beside the contact: a removal names it.
    pub leaf: u32,
    /// The grant to send, once, in the one-to-one conversation. Secret.
    pub grant: Vec<u8>,
}

/// What a grant says, without its key.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CircleGrantInfo {
    /// The directory it opens.
    pub directory_id: Vec<u8>,
    /// The leaf.
    pub leaf: u32,
}

/// One filter entry: this term tag is on this row.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CircleFilterEntry {
    /// The 32-byte term tag (`fofTag`).
    pub tag: Vec<u8>,
    /// The row.
    pub row: u32,
}

/// One edge of the header, as the header route returned it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CircleEdge {
    /// The node.
    pub node: u32,
    /// The 60-byte wrap.
    pub wrap: Vec<u8>,
}

/// A decoded head.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CircleHead {
    /// The current epoch.
    pub epoch: u64,
    /// The directory version.
    pub version: u64,
    /// The current filter's version; 0 until one is written.
    pub filter_version: u64,
    /// The current read key.
    pub read_public_key: Vec<u8>,
    /// The tree holds `1 << capacity_log` leaves.
    pub capacity_log: u8,
}

/// One write, ready to send.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CircleWrite {
    /// `PUT` for the claim, which also needs a `circle-claim` token, else
    /// `POST`.
    pub method: String,
    /// The service path.
    pub path: String,
    /// The JSON body. Send it again unchanged when the answer is lost.
    pub body_json: String,
    /// The directory version this creates.
    pub version: u64,
    /// The epoch this ends on.
    pub epoch: u64,
    /// Whether this is the claim.
    pub is_claim: bool,
}

impl From<DirectoryWrite> for CircleWrite {
    fn from(write: DirectoryWrite) -> Self {
        Self {
            method: write.method().to_string(),
            path: write.path(),
            body_json: write.body_json(),
            version: write.version,
            epoch: write.epoch,
            is_claim: write.is_claim(),
        }
    }
}

/// The owner's side of a directory. See the module doc.
#[derive(uniffi::Object)]
pub struct CircleWriter {
    inner: Mutex<DirectoryWriter>,
}

impl CircleWriter {
    fn lock(&self) -> MutexGuard<'_, DirectoryWriter> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A new directory for the account. `account_identity` is the ACCOUNT
/// identity, the same on every device. Its first write is the claim.
///
/// # Errors
///
/// [`FfiError::Circle`] on a cipher failure.
#[uniffi::export]
pub fn circle_writer_create(account_identity: &Identity) -> Result<Arc<CircleWriter>, FfiError> {
    let seed = SecretBytes::new(account_identity.inner.ed25519_seed_bytes());
    Ok(Arc::new(CircleWriter {
        inner: Mutex::new(DirectoryWriter::create(seed.as_bytes())?),
    }))
}

/// A writer from its kept state.
///
/// # Errors
///
/// [`FfiError::Circle`] when the state does not decode.
#[uniffi::export]
pub fn circle_writer_restore(
    account_identity: &Identity,
    state: Vec<u8>,
) -> Result<Arc<CircleWriter>, FfiError> {
    let seed = SecretBytes::new(account_identity.inner.ed25519_seed_bytes());
    Ok(Arc::new(CircleWriter {
        inner: Mutex::new(DirectoryWriter::restore(seed.as_bytes(), &state)?),
    }))
}

#[uniffi::export]
impl CircleWriter {
    /// The directory id.
    #[must_use]
    pub fn directory_id(&self) -> Vec<u8> {
        self.lock().directory_id().to_vec()
    }

    /// The owner's public key.
    #[must_use]
    pub fn owner_public_key(&self) -> Vec<u8> {
        self.lock().owner_public_key().to_vec()
    }

    /// The signer of this owner's requests, at [`circle_owner_proof_epoch`].
    #[must_use]
    pub fn owner_signer(&self) -> Arc<DsProofSigner> {
        let seed = self.lock().owner_signing_seed();
        Arc::new(DsProofSigner::from_inner(InnerSigner::for_group(
            OWNER_PROOF_EPOCH,
            seed.as_bytes(),
        )))
    }

    /// The state to keep. Holds no key. Keep it beside other device-local
    /// data: it says how many contacts the owner has.
    #[must_use]
    pub fn encode_state(&self) -> Vec<u8> {
        self.lock().encode_state()
    }

    /// The directory version the last write created.
    #[must_use]
    pub fn version(&self) -> u64 {
        self.lock().version()
    }

    /// The current epoch.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.lock().epoch()
    }

    /// The tree holds `1 << capacity_log` leaves.
    #[must_use]
    pub fn capacity_log(&self) -> u8 {
        self.lock().capacity_log()
    }

    /// The leaves assigned to contacts, ascending.
    #[must_use]
    pub fn members(&self) -> Vec<u32> {
        self.lock().members()
    }

    /// The current epoch key.
    #[must_use]
    pub fn epoch_key(&self) -> Arc<CircleEpochKey> {
        Arc::new(CircleEpochKey {
            inner: self.lock().epoch_key(),
        })
    }

    /// Give a contact a free leaf.
    ///
    /// # Errors
    ///
    /// [`FfiError::Circle`]: `treeFull`, or a cipher failure.
    pub fn add_member(&self) -> Result<CircleGrant, FfiError> {
        let grant = self.lock().add_member()?;
        Ok(CircleGrant {
            leaf: grant.leaf,
            grant: grant.encode().to_vec(),
        })
    }

    /// Remove the contacts at `leaves`, moving the epoch.
    ///
    /// # Errors
    ///
    /// [`FfiError::Circle`]: `empty`, `notAMember`, `writePending`.
    pub fn remove_members(&self, leaves: Vec<u32>) -> Result<(), FfiError> {
        Ok(self.lock().remove_members(&leaves)?)
    }

    /// Queue a card as row `index`.
    ///
    /// # Errors
    ///
    /// [`FfiError::Circle`]: `cardTooLong`, `rowOutOfRange`.
    pub fn put_row(&self, index: u32, card: Vec<u8>) -> Result<(), FfiError> {
        Ok(self.lock().put_row(index, card)?)
    }

    /// Queue the removal of row `index`.
    ///
    /// # Errors
    ///
    /// [`FfiError::Circle`]: `rowOutOfRange`.
    pub fn delete_row(&self, index: u32) -> Result<(), FfiError> {
        Ok(self.lock().delete_row(index)?)
    }

    /// Queue a new filter. `row_count` is the number of row indices in use.
    ///
    /// # Errors
    ///
    /// [`FfiError::Circle`]: `rowOutOfRange`, `tagLength`.
    pub fn set_filter(
        &self,
        entries: Vec<CircleFilterEntry>,
        row_count: u32,
    ) -> Result<(), FfiError> {
        let mut decoded = Vec::with_capacity(entries.len());
        for entry in entries {
            decoded.push(FilterEntry {
                tag: fixed::<TAG_LEN_BYTES>(&entry.tag, "tagLength", "a term tag")?,
                row: entry.row,
            });
        }
        Ok(self.lock().set_filter(decoded, row_count)?)
    }

    /// Whether anything is queued.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.lock().has_pending()
    }

    /// Seal what is queued into one write, under the current epoch. Call until
    /// it returns nothing: a large change goes over several writes.
    ///
    /// # Errors
    ///
    /// [`FfiError::Circle`]: `filterTooLarge`, `overflow`, a cipher failure.
    pub fn take_write(&self) -> Result<Option<CircleWrite>, FfiError> {
        Ok(self.lock().take_write()?.map(CircleWrite::from))
    }
}

/// The key of one epoch. It crosses as an object, never as bytes.
#[derive(uniffi::Object)]
pub struct CircleEpochKey {
    inner: EpochKey,
}

#[uniffi::export]
impl CircleEpochKey {
    /// The epoch.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.inner.epoch()
    }

    /// The signer of reads at this epoch. Only the current epoch's is
    /// accepted; an older key is for opening what it sealed.
    #[must_use]
    pub fn read_signer(&self) -> Arc<DsProofSigner> {
        Arc::new(DsProofSigner::from_inner(InnerSigner::for_group(
            self.inner.epoch(),
            self.inner.read_seed().as_bytes(),
        )))
    }

    /// The epoch key before this one, from this epoch's chain entry.
    ///
    /// # Errors
    ///
    /// [`FfiError::Circle`]: `wrapLength`, `authentication`, `malformed`.
    pub fn previous(&self, chain_wrap: Vec<u8>) -> Result<Arc<Self>, FfiError> {
        let wrap: Wrap = fixed::<WRAP_LEN>(&chain_wrap, "wrapLength", "a chain entry")?;
        Ok(Arc::new(Self {
            inner: self.inner.previous(&wrap)?,
        }))
    }

    /// Open a filter sealed at this epoch.
    ///
    /// # Errors
    ///
    /// [`FfiError::Circle`]: `wrongEpoch`, `authentication`, `length`,
    /// `unknownFormat`, `malformed`.
    pub fn open_filter(&self, wire: Vec<u8>) -> Result<Arc<CircleFilter>, FfiError> {
        Ok(Arc::new(CircleFilter {
            inner: open_filter(&self.inner, &wire)?,
        }))
    }

    /// Open row `index`, sealed at this epoch, to its card.
    ///
    /// # Errors
    ///
    /// [`FfiError::Circle`]: `wrongEpoch`, `authentication`, `length`,
    /// `unknownFormat`, `malformed`.
    pub fn open_row(&self, index: u32, row: Vec<u8>) -> Result<Vec<u8>, FfiError> {
        Ok(open_row(&self.inner, index, &row)?)
    }
}

/// An opened filter.
#[derive(uniffi::Object)]
pub struct CircleFilter {
    inner: OpenedFilter,
}

#[uniffi::export]
impl CircleFilter {
    /// The rows this term tag is on. Local. Never misses a stored term; a
    /// false positive is a row that does not carry the term, which the caller
    /// sees when it opens the card.
    ///
    /// # Errors
    ///
    /// [`FfiError::Circle`]: `tagLength`.
    pub fn lookup(&self, tag: Vec<u8>) -> Result<Vec<u32>, FfiError> {
        let tag = fixed::<TAG_LEN_BYTES>(&tag, "tagLength", "a term tag")?;
        Ok(self.inner.lookup(&tag))
    }

    /// Row indices in use.
    #[must_use]
    pub fn row_count(&self) -> u32 {
        self.inner.row_count()
    }
}

/// Derive the current epoch key from a grant, the encoded head and the edges
/// of the grant's path.
///
/// # Errors
///
/// [`FfiError::Circle`]: `headerMismatch` when the header is stale or partial,
/// or this contact was removed; `authentication`, `notAMember`, `length`,
/// `unknownFormat`, `malformed`, `wrapLength`.
#[uniffi::export]
pub fn circle_member_epoch_key(
    grant: Vec<u8>,
    head: Vec<u8>,
    edges: Vec<CircleEdge>,
) -> Result<Arc<CircleEpochKey>, FfiError> {
    let grant = MemberGrant::decode(&grant)?;
    let head = DirectoryHead::decode(&head)?;
    let mut decoded = Vec::with_capacity(edges.len());
    for edge in edges {
        decoded.push((
            edge.node,
            fixed::<WRAP_LEN>(&edge.wrap, "wrapLength", "an edge")?,
        ));
    }
    Ok(Arc::new(CircleEpochKey {
        inner: member_epoch_key(&grant, &head, &decoded)?,
    }))
}

/// Decode a head.
///
/// # Errors
///
/// [`FfiError::Circle`]: `length`, `unknownFormat`, `malformed`.
#[uniffi::export]
pub fn circle_decode_head(head: Vec<u8>) -> Result<CircleHead, FfiError> {
    let head = DirectoryHead::decode(&head)?;
    Ok(CircleHead {
        epoch: head.epoch,
        version: head.version,
        filter_version: head.filter_version,
        read_public_key: head.read_pk.to_vec(),
        capacity_log: head.capacity_log,
    })
}

/// What a grant names, without its key.
///
/// # Errors
///
/// [`FfiError::Circle`]: `length`, `unknownFormat`, `malformed`.
#[uniffi::export]
pub fn circle_grant_info(grant: Vec<u8>) -> Result<CircleGrantInfo, FfiError> {
    let grant = MemberGrant::decode(&grant)?;
    Ok(CircleGrantInfo {
        directory_id: grant.directory_id.to_vec(),
        leaf: grant.leaf,
    })
}

/// The nodes whose edges a member at `leaf` reads.
///
/// # Errors
///
/// [`FfiError::Circle`]: `notAMember`.
#[uniffi::export]
pub fn circle_path_nodes(leaf: u32, capacity_log: u8) -> Result<Vec<u32>, FfiError> {
    Ok(path_nodes(leaf, capacity_log)?)
}

/// The epoch whose key opens this sealed filter.
///
/// # Errors
///
/// [`FfiError::Circle`]: `length`, `unknownFormat`.
#[uniffi::export]
pub fn circle_filter_epoch(wire: Vec<u8>) -> Result<u64, FfiError> {
    Ok(filter_header(&wire)?.epoch)
}

/// The epoch whose key opens this sealed row.
///
/// # Errors
///
/// [`FfiError::Circle`]: `length`, `unknownFormat`.
#[uniffi::export]
pub fn circle_row_epoch(row: Vec<u8>) -> Result<u64, FfiError> {
    Ok(row_epoch(&row)?)
}

/// The path of a directory on the Delivery Service, without a prefix. Its
/// routes hang off it: `/header`, `/filter`, `/rows/<index>`.
///
/// # Errors
///
/// [`FfiError::Circle`]: `keyLength`.
#[uniffi::export]
pub fn circle_directory_path(directory_id: Vec<u8>) -> Result<String, FfiError> {
    Ok(directory_path(&fixed::<32>(
        &directory_id,
        "keyLength",
        "a directory id",
    )?))
}

/// The proof epoch of an owner's requests.
#[uniffi::export]
#[must_use]
pub const fn circle_owner_proof_epoch() -> u64 {
    OWNER_PROOF_EPOCH
}

/// The length of a grant.
#[uniffi::export]
#[must_use]
pub const fn circle_grant_len() -> u32 {
    #[allow(clippy::cast_possible_truncation)] // a small constant
    let value = GRANT_LEN as u32;
    value
}

/// The longest card a row carries.
#[uniffi::export]
#[must_use]
pub const fn circle_row_card_max_len() -> u32 {
    #[allow(clippy::cast_possible_truncation)] // a small constant
    let value = ROW_CARD_MAX_LEN as u32;
    value
}

/// The most rows a lookup returns.
#[uniffi::export]
#[must_use]
pub const fn circle_max_rows_per_term() -> u32 {
    #[allow(clippy::cast_possible_truncation)] // a small constant
    let value = MAX_ROWS_PER_TERM as u32;
    value
}

/// The most rows one write carries.
#[uniffi::export]
#[must_use]
pub const fn circle_max_rows_per_write() -> u32 {
    #[allow(clippy::cast_possible_truncation)] // a small constant
    let value = MAX_ROWS_PER_WRITE as u32;
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(seed: u8) -> Identity {
        Identity {
            inner: lakko_mls::Identity::from_seed(&[seed; 64]).unwrap(),
        }
    }

    fn edges_for(writes: &[CircleWrite], leaf: u32, capacity_log: u8) -> Vec<CircleEdge> {
        // A stand-in for the header route: the last value of each path node
        // across the writes' JSON bodies.
        use base64::Engine as _;
        let path = circle_path_nodes(leaf, capacity_log).unwrap();
        let mut out: Vec<CircleEdge> = Vec::new();
        for write in writes {
            let body: serde_json::Value = serde_json::from_str(&write.body_json).unwrap();
            if let Some(deletes) = body["deletes"].as_array() {
                for node in deletes {
                    out.retain(|e| u64::from(e.node) != node.as_u64().unwrap());
                }
            }
            if let Some(puts) = body["puts"].as_array() {
                for item in puts {
                    let node = u32::try_from(item[0].as_u64().unwrap()).unwrap();
                    if path.contains(&node) {
                        out.retain(|e| e.node != node);
                        let wrap = base64::engine::general_purpose::STANDARD
                            .decode(item[1].as_str().unwrap())
                            .unwrap();
                        out.push(CircleEdge { node, wrap });
                    }
                }
            }
        }
        out
    }

    fn head_of(write: &CircleWrite) -> Vec<u8> {
        use base64::Engine as _;
        let body: serde_json::Value = serde_json::from_str(&write.body_json).unwrap();
        base64::engine::general_purpose::STANDARD
            .decode(body["head_b64"].as_str().unwrap())
            .unwrap()
    }

    #[test]
    fn a_directory_round_trips_through_the_surface() {
        let writer = circle_writer_create(&identity(1)).unwrap();
        let bob = writer.add_member().unwrap();
        let carol = writer.add_member().unwrap();
        writer.put_row(0, b"anna".to_vec()).unwrap();
        writer
            .set_filter(
                vec![CircleFilterEntry {
                    tag: vec![7; 32],
                    row: 0,
                }],
                1,
            )
            .unwrap();
        let claim = writer.take_write().unwrap().unwrap();
        assert!(claim.is_claim);
        assert_eq!(claim.method, "PUT");
        assert!(claim.path.ends_with("/claim"));
        assert_eq!(writer.take_write().unwrap(), None);

        let info = circle_grant_info(bob.grant.clone()).unwrap();
        assert_eq!(info.directory_id, writer.directory_id());
        assert_eq!(info.leaf, bob.leaf);

        let head = head_of(&claim);
        let decoded = circle_decode_head(head.clone()).unwrap();
        assert_eq!(decoded.epoch, 1);
        let sent = vec![claim.clone()];
        let edges = edges_for(&sent, bob.leaf, decoded.capacity_log);
        let key = circle_member_epoch_key(bob.grant.clone(), head, edges).unwrap();
        assert_eq!(key.epoch(), 1);

        // The removal, then a new key for Carol and none for Bob.
        writer.remove_members(vec![bob.leaf]).unwrap();
        let removal = writer.take_write().unwrap().unwrap();
        assert!(!removal.is_claim);
        assert_eq!(removal.method, "POST");
        let sent = vec![claim, removal.clone()];
        let head = head_of(&removal);
        let capacity = circle_decode_head(head.clone()).unwrap().capacity_log;
        let path_of_carol = edges_for(&sent, carol.leaf, capacity);
        let after_removal =
            circle_member_epoch_key(carol.grant.clone(), head.clone(), path_of_carol).unwrap();
        assert_eq!(after_removal.epoch(), 2);
        let path_of_bob = edges_for(&sent, bob.leaf, capacity);
        let refused = circle_member_epoch_key(bob.grant, head, path_of_bob);
        assert!(matches!(refused, Err(FfiError::Circle(m)) if m.starts_with("headerMismatch:")));
    }

    #[test]
    fn a_writer_restores_and_refuses_a_bad_state() {
        let id = identity(2);
        let writer = circle_writer_create(&id).unwrap();
        writer.add_member().unwrap();
        writer.take_write().unwrap();
        let again = circle_writer_restore(&id, writer.encode_state()).unwrap();
        assert_eq!(again.directory_id(), writer.directory_id());
        assert_eq!(again.members(), vec![0]);
        assert!(matches!(
            circle_writer_restore(&id, vec![1, 2, 3]),
            Err(FfiError::Circle(_))
        ));
    }

    #[test]
    fn a_wrong_length_is_a_coded_refusal() {
        let writer = circle_writer_create(&identity(3)).unwrap();
        let refused = writer.set_filter(
            vec![CircleFilterEntry {
                tag: vec![1; 31],
                row: 0,
            }],
            1,
        );
        assert!(matches!(refused, Err(FfiError::Circle(m)) if m.starts_with("tagLength:")));
        let key = writer.epoch_key();
        assert!(
            matches!(key.previous(vec![0; 3]), Err(FfiError::Circle(m)) if m.starts_with("wrapLength:"))
        );
    }

    #[test]
    fn the_owner_and_read_signers_sign_under_their_own_epochs() {
        let writer = circle_writer_create(&identity(4)).unwrap();
        assert_eq!(writer.owner_signer().epoch(), circle_owner_proof_epoch());
        assert_eq!(
            writer.owner_signer().verifying_key(),
            writer.owner_public_key()
        );
        let key = writer.epoch_key();
        assert_eq!(key.read_signer().epoch(), 1);
    }

    // The longest card `lakko_contacts` can compose today fits a row.
    const _: () =
        assert!(lakko_contacts::introduction::INTRODUCTION_PAYLOAD_MAX_LEN <= ROW_CARD_MAX_LEN);

    #[test]
    fn a_grant_is_sixty_nine_bytes() {
        assert_eq!(circle_grant_len(), 69);
    }
}
