// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `BackupPayload` — the structured bytes sealed inside the
//! Phase 2a1 backup envelope.
//!
//! ## Wire format
//!
//! Hand-rolled length-prefixed binary codec. Same pattern
//! `lakko-contacts` uses for its wire types — keeps the parser
//! surface small and fuzz-friendly, and avoids pulling in a
//! schema dep (prost / serde-binary) that would force every
//! caller through a wider attack surface.
//!
//! One version is recognised on decode: the current one. Lakko has no
//! deployment and no stored backups, so every other payload version is
//! refused rather than migrated. v6 dropped the two MLS snapshot fields v5
//! carried (see "Not stored" below); a v5 payload is refused like any other.
//!
//! ### v6
//!
//! ```text
//! u8           payload version (= 6)
//! [16 bytes]   account_id (UUIDv7 raw)
//! u64be        created_at_ms (the wall-clock at upload time)
//! [32 bytes]   ed25519_seed (the identity signing seed)
//! u32be        accepted_contacts_count
//! [..]         accepted_contacts_count × AcceptedContactRecord
//! u32be        known_group_ids_count
//! [..]         known_group_ids_count × { u32be len (<= MAX_GROUP_ID_LEN), bytes }
//! u32be        synced_state_length
//! [..]         synced_state_length bytes: `lakko_sync::SyncedState::to_bytes`
//!              (empty = no synced state held)
//! ```
//!
//! Nothing may follow the synced state: trailing bytes are refused.
//!
//! Each [`AcceptedContactRecord`]:
//!
//! ```text
//! [32 bytes]   ed25519_pubkey
//! [32 bytes]   x25519_pubkey
//! u16be        display_name length (UTF-8 bytes, ≤ MAX_DISPLAY_NAME_LEN)
//! [..]         display_name bytes
//! u8           account_id presence: 0 = absent, 1 = present
//! [16 bytes]   account_id (only when the presence byte is 1)
//! ```
//!
//! ## What's stored, what's not
//!
//! Stored:
//!   - `account_id` — needed to bind the blob to the right
//!     server-side row on restore.
//!   - `created_at_ms` — for staleness display + UI consistency.
//!   - `ed25519_seed` — the identity-signing seed. From this the
//!     client re-derives Ed25519 + X25519 keypairs deterministically
//!     (the same `Identity::from_seed` flow Phase 1 already uses).
//!   - `accepted_contacts` — the local address book. Public bytes
//!     only (pubkeys + display name); no secret material.
//!   - `known_group_ids` — the ids of the conversations the account's
//!     devices knew, so a restored device knows which conversations
//!     exist before any member has admitted it. Ids only: no group
//!     secret, no membership.
//!   - `synced_state` — the account's folded DeviceGroup state (ADR-0058:
//!     chat folders, pins, mutes, archive, read cursors, drafts,
//!     conversation order), so a restore with no other device alive seeds
//!     the fold from it. Validated on encode and decode through
//!     `lakko_sync::SyncedState::from_bytes`, fail closed.
//!
//! Not stored (intentional):
//!   - MLS state is not stored: no OpenMLS provider snapshot, whole or
//!     per group. A snapshot belongs to the device that made it (its
//!     leaf, its signature key, its ratchet counters), so loading it on
//!     another device clones a leaf (ADR-0050) and loading it on the same
//!     device after a reinstall rolls that device back behind commits it
//!     already published (ADR-0040) and re-uses ratchet positions (G-34);
//!     a restored device is admitted to its conversations by other
//!     members (ADR-0050 section 2.5a), never from the backup.
//!   - Message history. Flows through the 2a3d DeviceGroup
//!     catch-up bundle path. Backup envelope is "small,
//!     frequent-write, fast-restore"; history is "large,
//!     append-only, syncs over MLS-encrypted channels".
//!   - Per-device session tokens. Each device re-authenticates
//!     after restore.
//!   - X25519 secret separately. Derived from `ed25519_seed`
//!     via the existing `Identity::from_seed` flow; no need to
//!     duplicate-store.

use crate::error::{BackupError, Result};

/// The payload schema version: the only one emitted and the only one
/// accepted. v6 carries no MLS state: it is v5 without the per-group and
/// whole-storage snapshot fields.
pub const PAYLOAD_VERSION: u8 = 6;

/// Account-id length (UUIDv7 raw, mirrors
/// `lakko_contacts::ACCOUNT_ID_LEN`).
const ACCOUNT_ID_LEN: usize = 16;

/// Identity signing-seed length (Ed25519 seed, mirrors
/// `lakko_crypto::signing::SECRET_KEY_LEN`).
const SEED_LEN: usize = 32;

/// Per-pubkey length in the contact records — Ed25519 verifying
/// key and X25519 public key both 32 bytes.
const PUBKEY_LEN: usize = 32;

/// Hard cap on per-contact display-name UTF-8 bytes. Keeps the
/// inner codec's worst-case payload bounded for fuzzing.
pub const MAX_DISPLAY_NAME_LEN: usize = 256;

/// Hard cap on one MLS `group_id`. Lakko mints 32-byte ids; the cap is loose
/// enough to outlive that choice and tight enough that a malformed payload
/// cannot ask for an unbounded read.
pub const MAX_GROUP_ID_LEN: usize = 128;

/// One row in the local "accepted contacts" address book.
/// Mirrors the public fields the web client's `AcceptedContact`
/// type holds — see `apps/web/src/contacts/local-store.ts`. Only
/// public bytes; no secret material.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedContactRecord {
    /// Sender's Ed25519 identity public key.
    pub ed25519_pubkey: [u8; PUBKEY_LEN],
    /// Sender's X25519 HPKE public key. Empty / zeroed if not
    /// known yet (legacy v1 ContactRequest); the receive path
    /// surfaces it as the sealing key for future replies.
    pub x25519_pubkey: [u8; PUBKEY_LEN],
    /// User-chosen display name (UTF-8). Empty string is valid;
    /// `MAX_DISPLAY_NAME_LEN` bytes max.
    pub display_name: String,
    /// Sender's 16-byte account_id (ADR-0021), when known. `None` for a
    /// contact accepted before the account_id was learned. Lets a restored
    /// contact resolve to an SFU group-call roster `peer_tag`.
    pub account_id: Option<[u8; ACCOUNT_ID_LEN]>,
}

/// The structured state we want to roundtrip through the
/// backup envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackupPayload {
    /// Owner of this backup. Bound to the server-side row.
    pub account_id: [u8; ACCOUNT_ID_LEN],
    /// Wall-clock UNIX millis at backup-creation time.
    pub created_at_ms: u64,
    /// The identity signing seed — re-derive Ed25519 + X25519
    /// keypairs on restore via the existing `Identity::from_seed`
    /// flow.
    pub ed25519_seed: [u8; SEED_LEN],
    /// Local "accepted contacts" address book.
    pub accepted_contacts: Vec<AcceptedContactRecord>,
    /// The MLS `group_id`s of the conversations the account's devices knew
    /// at backup time.
    ///
    /// The backup carries no MLS state, so these are names and nothing more:
    /// a restored device learns from them which conversations exist before
    /// any member has admitted it, and can show each one as waiting for
    /// admission. Membership itself comes from the other members (ADR-0050
    /// section 2.5a), never from this list.
    ///
    /// Empty `Vec` for an account that knew no conversations.
    pub known_group_ids: Vec<Vec<u8>>,
    /// The folded DeviceGroup state (ADR-0058 section 5.2), as
    /// `lakko_sync::SyncedState::to_bytes` wrote it. Empty = none held.
    ///
    /// Bytes rather than the typed state so this crate's value stays a plain
    /// codec record; [`Self::encode`] and [`Self::decode`] both run
    /// `SyncedState::from_bytes` over it, so a payload carrying a state the
    /// fold would refuse is refused whole.
    pub synced_state: Vec<u8>,
}

impl BackupPayload {
    /// Encode the payload to the on-the-wire bytes that go into
    /// the AEAD plaintext of [`crate::seal`].
    ///
    /// # Errors
    ///
    /// * [`BackupError::InvalidPayload`] when a per-record length
    ///   would overflow its width or exceed its cap (display_name over
    ///   `MAX_DISPLAY_NAME_LEN`; a `group_id` over `MAX_GROUP_ID_LEN`;
    ///   contact / group-id count or synced-state length > u32::MAX), or
    ///   when `synced_state` is not a state the fold accepts.
    pub fn encode(&self) -> Result<Vec<u8>> {
        // Pre-size estimate keeps reallocs cheap. Display names and
        // group ids are variable; we round down to a sensible starting
        // capacity.
        let mut out = Vec::with_capacity(
            1 + ACCOUNT_ID_LEN
                + 8
                + SEED_LEN
                + 4
                + (self.accepted_contacts.len() * 72)
                + 4
                + (self.known_group_ids.len() * 36)
                + 4
                + self.synced_state.len(),
        );

        out.push(PAYLOAD_VERSION);
        out.extend_from_slice(&self.account_id);
        out.extend_from_slice(&self.created_at_ms.to_be_bytes());
        out.extend_from_slice(&self.ed25519_seed);

        let contact_count = u32::try_from(self.accepted_contacts.len()).map_err(|_| {
            BackupError::InvalidPayload(format!(
                "accepted_contacts count {} overflows u32",
                self.accepted_contacts.len(),
            ))
        })?;
        out.extend_from_slice(&contact_count.to_be_bytes());
        for c in &self.accepted_contacts {
            encode_contact(&mut out, c)?;
        }

        // The conversations the account's devices knew.
        let group_count = u32::try_from(self.known_group_ids.len()).map_err(|_| {
            BackupError::InvalidPayload(format!(
                "known_group_ids count {} overflows u32",
                self.known_group_ids.len(),
            ))
        })?;
        out.extend_from_slice(&group_count.to_be_bytes());
        for id in &self.known_group_ids {
            if id.len() > MAX_GROUP_ID_LEN {
                return Err(BackupError::InvalidPayload(format!(
                    "group_id length {} exceeds MAX_GROUP_ID_LEN {MAX_GROUP_ID_LEN}",
                    id.len(),
                )));
            }
            let id_len = u32::try_from(id.len()).map_err(|_| {
                BackupError::InvalidPayload(format!("group_id length {} overflows u32", id.len()))
            })?;
            out.extend_from_slice(&id_len.to_be_bytes());
            out.extend_from_slice(id);
        }

        // The ADR-0058 synced state.
        check_synced_state(&self.synced_state)?;
        let state_len = u32::try_from(self.synced_state.len()).map_err(|_| {
            BackupError::InvalidPayload(format!(
                "synced_state length {} overflows u32",
                self.synced_state.len(),
            ))
        })?;
        out.extend_from_slice(&state_len.to_be_bytes());
        out.extend_from_slice(&self.synced_state);

        Ok(out)
    }

    /// Decode wire bytes back into a `BackupPayload`.
    ///
    /// # Errors
    ///
    /// * [`BackupError::InvalidPayload`] when the wire bytes are
    ///   malformed (any version other than [`PAYLOAD_VERSION`],
    ///   truncated, length-prefix overflow, trailing bytes after the
    ///   structured tail, display-name or group-id over its cap,
    ///   non-UTF-8 display name, a synced state the fold refuses).
    pub fn decode(wire: &[u8]) -> Result<Self> {
        let mut cur = wire;

        let version = take_u8(&mut cur)?;
        if version != PAYLOAD_VERSION {
            return Err(BackupError::InvalidPayload(format!(
                "unsupported payload version {version} (expected {PAYLOAD_VERSION})",
            )));
        }

        let mut account_id = [0u8; ACCOUNT_ID_LEN];
        account_id.copy_from_slice(take_n(&mut cur, ACCOUNT_ID_LEN)?);

        let created_at_ms = u64::from_be_bytes(
            take_n(&mut cur, 8)?
                .try_into()
                .expect("take_n(8) length pinned"),
        );

        let mut ed25519_seed = [0u8; SEED_LEN];
        ed25519_seed.copy_from_slice(take_n(&mut cur, SEED_LEN)?);

        let contact_count = u32::from_be_bytes(
            take_n(&mut cur, 4)?
                .try_into()
                .expect("take_n(4) length pinned"),
        ) as usize;
        let mut accepted_contacts = Vec::with_capacity(contact_count);
        for _ in 0..contact_count {
            accepted_contacts.push(decode_contact(&mut cur)?);
        }

        let count = u32::from_be_bytes(
            take_n(&mut cur, 4)?
                .try_into()
                .expect("take_n(4) length pinned"),
        ) as usize;
        // Grown rather than pre-sized: the count is read off the wire, and
        // reserving for it would let a malformed payload ask for an
        // allocation no real backup needs.
        let mut known_group_ids = Vec::new();
        for _ in 0..count {
            let len = u32::from_be_bytes(
                take_n(&mut cur, 4)?
                    .try_into()
                    .expect("take_n(4) length pinned"),
            ) as usize;
            if len > MAX_GROUP_ID_LEN {
                return Err(BackupError::InvalidPayload(format!(
                    "group_id length {len} exceeds MAX_GROUP_ID_LEN {MAX_GROUP_ID_LEN}",
                )));
            }
            known_group_ids.push(take_n(&mut cur, len)?.to_vec());
        }

        let state_len = u32::from_be_bytes(
            take_n(&mut cur, 4)?
                .try_into()
                .expect("take_n(4) length pinned"),
        ) as usize;
        let synced_state = take_n(&mut cur, state_len)?.to_vec();
        check_synced_state(&synced_state)?;

        if !cur.is_empty() {
            return Err(BackupError::InvalidPayload(format!(
                "{} trailing bytes after payload tail",
                cur.len(),
            )));
        }

        Ok(Self {
            account_id,
            created_at_ms,
            ed25519_seed,
            accepted_contacts,
            known_group_ids,
            synced_state,
        })
    }
}

/// Refuse synced-state bytes the fold would refuse. Empty is "none held".
fn check_synced_state(bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    lakko_sync::SyncedState::from_bytes(bytes)
        .map(|_| ())
        .map_err(|e| BackupError::InvalidPayload(format!("synced_state refused: {e}")))
}

// ============================================================================
// Per-record codec
// ============================================================================

fn encode_contact(out: &mut Vec<u8>, c: &AcceptedContactRecord) -> Result<()> {
    out.extend_from_slice(&c.ed25519_pubkey);
    out.extend_from_slice(&c.x25519_pubkey);

    let name_bytes = c.display_name.as_bytes();
    if name_bytes.len() > MAX_DISPLAY_NAME_LEN {
        return Err(BackupError::InvalidPayload(format!(
            "display_name length {} exceeds MAX_DISPLAY_NAME_LEN {}",
            name_bytes.len(),
            MAX_DISPLAY_NAME_LEN,
        )));
    }
    let len = u16::try_from(name_bytes.len()).expect("bounded by MAX_DISPLAY_NAME_LEN above");
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(name_bytes);

    // Optional account_id: a 1-byte presence flag, then 16 bytes when
    // present.
    match c.account_id {
        Some(id) => {
            out.push(1);
            out.extend_from_slice(&id);
        },
        None => out.push(0),
    }
    Ok(())
}

fn decode_contact(cur: &mut &[u8]) -> Result<AcceptedContactRecord> {
    let mut ed25519_pubkey = [0u8; PUBKEY_LEN];
    ed25519_pubkey.copy_from_slice(take_n(cur, PUBKEY_LEN)?);

    let mut x25519_pubkey = [0u8; PUBKEY_LEN];
    x25519_pubkey.copy_from_slice(take_n(cur, PUBKEY_LEN)?);

    let len =
        u16::from_be_bytes(take_n(cur, 2)?.try_into().expect("take_n(2) length pinned")) as usize;
    if len > MAX_DISPLAY_NAME_LEN {
        return Err(BackupError::InvalidPayload(format!(
            "display_name length {len} exceeds MAX_DISPLAY_NAME_LEN {MAX_DISPLAY_NAME_LEN}",
        )));
    }
    let name_bytes = take_n(cur, len)?;
    let display_name = std::str::from_utf8(name_bytes)
        .map_err(|e| BackupError::InvalidPayload(format!("display_name not utf-8: {e}")))?
        .to_owned();

    // Optional account_id (presence flag + 16 bytes).
    let account_id = match take_u8(cur)? {
        0 => None,
        1 => {
            let mut id = [0u8; ACCOUNT_ID_LEN];
            id.copy_from_slice(take_n(cur, ACCOUNT_ID_LEN)?);
            Some(id)
        },
        other => {
            return Err(BackupError::InvalidPayload(format!(
                "invalid account_id presence byte {other} (expected 0 or 1)",
            )));
        },
    };

    Ok(AcceptedContactRecord {
        ed25519_pubkey,
        x25519_pubkey,
        display_name,
        account_id,
    })
}

// ============================================================================
// Codec helpers
// ============================================================================

fn take_u8(cur: &mut &[u8]) -> Result<u8> {
    if cur.is_empty() {
        return Err(BackupError::InvalidPayload("unexpected EOF".to_string()));
    }
    let b = cur[0];
    *cur = &cur[1..];
    Ok(b)
}

fn take_n<'a>(cur: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
    if cur.len() < n {
        return Err(BackupError::InvalidPayload(format!(
            "need {n} bytes, have {}",
            cur.len(),
        )));
    }
    let (head, rest) = cur.split_at(n);
    *cur = rest;
    Ok(head)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_payload() -> BackupPayload {
        BackupPayload {
            account_id: [0xA1; ACCOUNT_ID_LEN],
            created_at_ms: 1_700_000_000_000,
            ed25519_seed: [0x42; SEED_LEN],
            accepted_contacts: vec![
                AcceptedContactRecord {
                    ed25519_pubkey: [0x01; PUBKEY_LEN],
                    x25519_pubkey: [0x02; PUBKEY_LEN],
                    display_name: "alice".to_string(),
                    account_id: Some([0xCC; ACCOUNT_ID_LEN]),
                },
                AcceptedContactRecord {
                    ed25519_pubkey: [0x03; PUBKEY_LEN],
                    x25519_pubkey: [0x04; PUBKEY_LEN],
                    display_name: String::new(),
                    account_id: None,
                },
            ],
            known_group_ids: vec![vec![7u8; 32], vec![9u8; 32]],
            synced_state: Vec::new(),
        }
    }

    /// A real, non-empty ADR-0058 synced state: one folder.
    fn sample_synced_state() -> lakko_sync::SyncedState {
        use lakko_sync::synced::{FolderMeta, SyncEntry, SyncedState, INCLUDE_GROUPS};
        let mut state = SyncedState::new();
        state
            .apply(
                7,
                SyncEntry::FolderMeta(FolderMeta {
                    folder_id: [3; 16],
                    name: "Work".into(),
                    include_kinds: INCLUDE_GROUPS,
                    exclude_flags: 0,
                }),
            )
            .unwrap();
        state
    }

    /// The retired v5 layout, written by hand: the same fields as v6 plus
    /// the per-group snapshot list and the whole-storage snapshot between
    /// the contacts and the group ids. `version` is the first byte, so the
    /// same bytes can be stamped as v5 (what a real old payload carries) or
    /// as v6 (a v5 body passed off as current).
    fn v5_shaped_wire(
        version: u8,
        payload: &BackupPayload,
        mls_snapshots: &[(&[u8], &[u8])],
        whole_storage_snapshot: &[u8],
    ) -> Vec<u8> {
        fn put_len(out: &mut Vec<u8>, len: usize) {
            out.extend_from_slice(&u32::try_from(len).unwrap().to_be_bytes());
        }
        let mut out = vec![version];
        out.extend_from_slice(&payload.account_id);
        out.extend_from_slice(&payload.created_at_ms.to_be_bytes());
        out.extend_from_slice(&payload.ed25519_seed);
        put_len(&mut out, payload.accepted_contacts.len());
        for c in &payload.accepted_contacts {
            encode_contact(&mut out, c).unwrap();
        }
        put_len(&mut out, mls_snapshots.len());
        for (group_id, snapshot) in mls_snapshots {
            put_len(&mut out, group_id.len());
            out.extend_from_slice(group_id);
            put_len(&mut out, snapshot.len());
            out.extend_from_slice(snapshot);
        }
        put_len(&mut out, whole_storage_snapshot.len());
        out.extend_from_slice(whole_storage_snapshot);
        put_len(&mut out, payload.known_group_ids.len());
        for id in &payload.known_group_ids {
            put_len(&mut out, id.len());
            out.extend_from_slice(id);
        }
        put_len(&mut out, payload.synced_state.len());
        out.extend_from_slice(&payload.synced_state);
        out
    }

    #[test]
    fn the_payload_version_is_six() {
        assert_eq!(PAYLOAD_VERSION, 6);
        assert_eq!(sample_payload().encode().unwrap()[0], 6);
    }

    #[test]
    fn the_v6_wire_layout_is_pinned() {
        // One contact with an account id, one 3-byte group id, no synced
        // state: every field of the layout appears once, in order, and
        // nothing sits between the contacts and the group ids.
        let payload = BackupPayload {
            account_id: [0xA1; ACCOUNT_ID_LEN],
            created_at_ms: 0x0102_0304_0506_0708,
            ed25519_seed: [0x42; SEED_LEN],
            accepted_contacts: vec![AcceptedContactRecord {
                ed25519_pubkey: [0x01; PUBKEY_LEN],
                x25519_pubkey: [0x02; PUBKEY_LEN],
                display_name: "al".to_string(),
                account_id: Some([0xCC; ACCOUNT_ID_LEN]),
            }],
            known_group_ids: vec![vec![7, 8, 9]],
            synced_state: Vec::new(),
        };
        let mut expected = vec![6u8];
        expected.extend_from_slice(&[0xA1; ACCOUNT_ID_LEN]);
        expected.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        expected.extend_from_slice(&[0x42; SEED_LEN]);
        expected.extend_from_slice(&[0, 0, 0, 1]);
        expected.extend_from_slice(&[0x01; PUBKEY_LEN]);
        expected.extend_from_slice(&[0x02; PUBKEY_LEN]);
        expected.extend_from_slice(&[0, 2]);
        expected.extend_from_slice(b"al");
        expected.push(1);
        expected.extend_from_slice(&[0xCC; ACCOUNT_ID_LEN]);
        expected.extend_from_slice(&[0, 0, 0, 1]);
        expected.extend_from_slice(&[0, 0, 0, 3]);
        expected.extend_from_slice(&[7, 8, 9]);
        expected.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(payload.encode().unwrap(), expected);
        assert_eq!(BackupPayload::decode(&expected).unwrap(), payload);
    }

    #[test]
    fn a_v6_payload_with_contacts_group_ids_and_synced_state_round_trips() {
        let mut payload = sample_payload();
        payload.synced_state = sample_synced_state().to_bytes();
        assert!(!payload.accepted_contacts.is_empty());
        assert!(!payload.known_group_ids.is_empty());
        assert!(!payload.synced_state.is_empty());

        let wire = payload.encode().unwrap();
        let decoded = BackupPayload::decode(&wire).unwrap();
        assert_eq!(decoded, payload);
        assert_eq!(
            lakko_sync::SyncedState::from_bytes(&decoded.synced_state).unwrap(),
            sample_synced_state()
        );
        // Canonical: what decodes re-encodes to the same bytes.
        assert_eq!(decoded.encode().unwrap(), wire);
    }

    #[test]
    fn a_v5_shaped_payload_is_refused() {
        let mut payload = sample_payload();
        payload.synced_state = sample_synced_state().to_bytes();
        let refused = |mls_snapshots: &[(&[u8], &[u8])], whole_storage_snapshot: &[u8]| {
            // As stored: refused on the version byte.
            let v5 = v5_shaped_wire(5, &payload, mls_snapshots, whole_storage_snapshot);
            let err = BackupPayload::decode(&v5).unwrap_err();
            assert!(
                matches!(&err, BackupError::InvalidPayload(m) if m.contains("version 5")),
                "a v5 payload was not refused on its version: {err:?}"
            );
            // Stamped as current: the body still does not parse as v6.
            let relabelled = v5_shaped_wire(
                PAYLOAD_VERSION,
                &payload,
                mls_snapshots,
                whole_storage_snapshot,
            );
            assert!(
                BackupPayload::decode(&relabelled).is_err(),
                "a v5 body stamped v6 decoded"
            );
        };
        // What the v5 encoder actually wrote: no per-group entries.
        refused(&[], b"opaque-whole-storage-blob");
        // Both snapshot fields populated.
        refused(
            &[(b"group-id-bytes", b"opaque-snapshot")],
            b"opaque-whole-storage-blob",
        );
        // Both empty: the closest a v5 body gets to a v6 one.
        refused(&[], b"");
    }

    #[test]
    fn every_version_but_the_current_one_is_refused() {
        let wire = sample_payload().encode().unwrap();
        for version in u8::MIN..=u8::MAX {
            let mut wire = wire.clone();
            wire[0] = version;
            assert_eq!(
                BackupPayload::decode(&wire).is_ok(),
                version == PAYLOAD_VERSION,
                "version {version}"
            );
        }
    }

    #[test]
    fn known_group_ids_round_trip() {
        let payload = sample_payload();
        let decoded = BackupPayload::decode(&payload.encode().unwrap()).unwrap();
        assert_eq!(decoded.known_group_ids, vec![vec![7u8; 32], vec![9u8; 32]]);
    }

    #[test]
    fn an_older_payload_version_is_refused() {
        for old in 1..PAYLOAD_VERSION {
            let mut wire = sample_payload().encode().unwrap();
            wire[0] = old;
            assert!(BackupPayload::decode(&wire).is_err(), "v{old} decoded");
        }
    }

    #[test]
    fn the_synced_state_round_trips_and_a_bad_one_is_refused() {
        use lakko_sync::SyncedState;
        let state = sample_synced_state();
        let mut payload = sample_payload();
        payload.synced_state = state.to_bytes();
        let decoded = BackupPayload::decode(&payload.encode().unwrap()).unwrap();
        assert_eq!(
            SyncedState::from_bytes(&decoded.synced_state).unwrap(),
            state
        );

        payload.synced_state = vec![9, 9, 9];
        assert!(payload.encode().is_err());
        // Hand-built, because the encoder refuses to produce one.
        let mut good = sample_payload();
        good.synced_state = Vec::new();
        let mut wire = good.encode().unwrap();
        wire.truncate(wire.len() - 4);
        wire.extend_from_slice(&3u32.to_be_bytes());
        wire.extend_from_slice(&[9, 9, 9]);
        assert!(BackupPayload::decode(&wire).is_err());
    }

    #[test]
    fn an_over_long_group_id_is_refused_on_encode() {
        let mut payload = sample_payload();
        payload.known_group_ids = vec![vec![0u8; MAX_GROUP_ID_LEN + 1]];
        assert!(payload.encode().is_err());
    }

    #[test]
    fn an_over_long_group_id_is_refused_on_decode() {
        // Hand-built, because the encoder refuses to produce one: strip the
        // group and state tails and write an over-long id.
        let mut payload = sample_payload();
        payload.known_group_ids = Vec::new();
        let mut wire = payload.encode().unwrap();
        wire.truncate(wire.len() - 8);
        wire.extend_from_slice(&1u32.to_be_bytes());
        wire.extend_from_slice(&u32::try_from(MAX_GROUP_ID_LEN + 1).unwrap().to_be_bytes());
        wire.extend_from_slice(&[0u8; MAX_GROUP_ID_LEN + 1]);
        wire.extend_from_slice(&0u32.to_be_bytes());
        assert!(BackupPayload::decode(&wire).is_err());
    }

    #[test]
    fn a_truncated_group_tail_is_refused_rather_than_read_short() {
        let payload = sample_payload();
        let wire = payload.encode().unwrap();
        for cut in 1..=(4 + 32 + 4) {
            assert!(
                BackupPayload::decode(&wire[..wire.len() - cut]).is_err(),
                "a wire cut {cut} bytes short decoded"
            );
        }
    }

    #[test]
    fn round_trip_full_payload() {
        let payload = sample_payload();
        let wire = payload.encode().unwrap();
        let decoded = BackupPayload::decode(&wire).unwrap();
        assert_eq!(decoded, payload);
    }

    #[test]
    fn round_trip_empty_lists() {
        let payload = BackupPayload {
            account_id: [0; ACCOUNT_ID_LEN],
            created_at_ms: 0,
            ed25519_seed: [0; SEED_LEN],
            accepted_contacts: vec![],
            known_group_ids: Vec::new(),
            synced_state: Vec::new(),
        };
        let wire = payload.encode().unwrap();
        let decoded = BackupPayload::decode(&wire).unwrap();
        assert_eq!(decoded, payload);
    }

    #[test]
    fn wrong_version_rejected() {
        let mut wire = sample_payload().encode().unwrap();
        wire[0] = 99;
        let err = BackupPayload::decode(&wire).unwrap_err();
        assert!(matches!(err, BackupError::InvalidPayload(_)));
    }

    #[test]
    fn truncated_payload_rejected() {
        let wire = sample_payload().encode().unwrap();
        let err = BackupPayload::decode(&wire[..wire.len() - 5]).unwrap_err();
        assert!(matches!(err, BackupError::InvalidPayload(_)));
    }

    #[test]
    fn trailing_bytes_rejected() {
        let mut wire = sample_payload().encode().unwrap();
        wire.push(0x00);
        let err = BackupPayload::decode(&wire).unwrap_err();
        assert!(matches!(err, BackupError::InvalidPayload(_)));
    }

    #[test]
    fn display_name_over_cap_rejected_on_encode() {
        let payload = BackupPayload {
            account_id: [0; ACCOUNT_ID_LEN],
            created_at_ms: 0,
            ed25519_seed: [0; SEED_LEN],
            accepted_contacts: vec![AcceptedContactRecord {
                ed25519_pubkey: [0; PUBKEY_LEN],
                x25519_pubkey: [0; PUBKEY_LEN],
                display_name: "x".repeat(MAX_DISPLAY_NAME_LEN + 1),
                account_id: None,
            }],
            known_group_ids: Vec::new(),
            synced_state: Vec::new(),
        };
        let err = payload.encode().unwrap_err();
        assert!(matches!(err, BackupError::InvalidPayload(_)));
    }

    #[test]
    fn display_name_at_cap_is_supported() {
        let payload = BackupPayload {
            account_id: [0; ACCOUNT_ID_LEN],
            created_at_ms: 0,
            ed25519_seed: [0; SEED_LEN],
            accepted_contacts: vec![AcceptedContactRecord {
                ed25519_pubkey: [0; PUBKEY_LEN],
                x25519_pubkey: [0; PUBKEY_LEN],
                display_name: "x".repeat(MAX_DISPLAY_NAME_LEN),
                account_id: None,
            }],
            known_group_ids: Vec::new(),
            synced_state: Vec::new(),
        };
        let wire = payload.encode().unwrap();
        let decoded = BackupPayload::decode(&wire).unwrap();
        assert_eq!(decoded, payload);
    }

    /// End-to-end: encode the payload, seal it in the envelope,
    /// open + decode. This is the canonical flow the
    /// application layer drives.
    #[test]
    fn round_trip_through_envelope() {
        let phrase = b"recovery phrase 24 words abracadabra";
        let payload = sample_payload();
        let payload_bytes = payload.encode().unwrap();

        let wire = crate::envelope::seal(phrase, &payload_bytes).unwrap();
        let opened = crate::envelope::open(phrase, &wire).unwrap();
        let decoded = BackupPayload::decode(&opened).unwrap();
        assert_eq!(decoded, payload);
    }
}
