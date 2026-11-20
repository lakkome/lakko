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
//! carried (see "Not stored" below); v7 added the own predecessors; v8 adds
//! the received profiles. Every older payload is refused like any other.
//!
//! ### v8
//!
//! ```text
//! u8           payload version (= 8)
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
//! u8           own_predecessors_count (<= MAX_OWN_PREDECESSORS)
//! [..]         own_predecessors_count × { [64 bytes] seed, u64be adopted_at_ms }
//! u32be        own_profile_length (<= MAX_OWN_ENVELOPE_LEN)
//! [..]         own_profile_length bytes: the account's own profile envelope
//! u32be        visibility_length (<= MAX_OWN_ENVELOPE_LEN)
//! [..]         visibility_length bytes: its "who can find me" envelope
//! u32be        received_profiles_count (<= MAX_RECEIVED_PROFILES)
//! [..]         received_profiles_count × {
//!                [32 bytes] person_key (strictly increasing across entries),
//!                u16be      envelope_length (1..=PROFILE_ENVELOPE_MAX_LEN),
//!                [..]       envelope bytes
//!              }
//! ```
//!
//! The received-profile entries together take at most
//! `MAX_RECEIVED_PROFILES_BYTES` (key, length prefix and envelope counted).
//! Nothing may follow them: trailing bytes are refused.
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
//!     `lakko_sync::SyncedState::from_bytes`, fail closed, with one
//!     exception on decode (2026-11-15): a state whose leading version byte
//!     is not the one this build reads is dropped and decodes as empty
//!     ("none held"), so a backup written before the persisted synced-state
//!     version moved still restores everything else. Any other malformed
//!     state refuses the whole payload. A decoded payload carrying a dropped
//!     state therefore re-encodes to different bytes than it was read from.
//!   - `own_profile`, `visibility` — the person's own name and photo
//!     references, and who can find them, as the envelopes
//!     `lakko_contacts` defines. A restored device with no other device
//!     alive has nowhere else to learn them. Opaque here and bounded by
//!     length; the client decodes them fail closed.
//!   - `received_profiles` — the newest profile envelope this device holds
//!     from each other person (ADR-0048), keyed on that person's account
//!     signature key. A device restored with no other device alive otherwise
//!     shows every sender without a name until each republishes. Opaque here
//!     and bounded; on restore the client feeds each envelope back through
//!     its ordinary profile receive gate (strictly newer stamp per key), so a
//!     stale backup never replaces a newer name.
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
/// whole-storage snapshot fields. v7 adds the own predecessors (ADR-0074
/// section 3.7). v8 adds the received profiles (ADR-0048).
pub const PAYLOAD_VERSION: u8 = 8;

/// Account-id length (UUIDv7 raw, mirrors
/// `lakko_contacts::ACCOUNT_ID_LEN`).
const ACCOUNT_ID_LEN: usize = 16;

/// Length of an own predecessor's phrase seed.
pub const PREDECESSOR_SEED_LEN: usize = 64;

/// Most own predecessors a backup carries: the pairing state bundle's bound.
pub const MAX_OWN_PREDECESSORS: usize = 8;

/// Hard cap on the own profile and visibility envelopes. A visibility setting
/// names up to 1024 scopes and 4096 contacts, so this sits above the largest
/// one `lakko_contacts` composes.
pub const MAX_OWN_ENVELOPE_LEN: usize = 256 * 1024;

/// Length of the key a received profile is stored under: the person's
/// account signature key (Ed25519), as resolved at receipt.
pub const RECEIVED_PROFILE_KEY_LEN: usize = 32;

/// Longest received profile envelope a backup carries: the profile codec's
/// own bound, so anything longer is not a profile.
pub const MAX_RECEIVED_PROFILE_LEN: usize = lakko_contacts::PROFILE_ENVELOPE_MAX_LEN;

// The envelope length rides a `u16`.
const _: () = assert!(MAX_RECEIVED_PROFILE_LEN <= u16::MAX as usize);

/// Most received profiles a backup carries. Chosen, not measured: well above
/// the people one person meets across contacts and groups, and with
/// [`MAX_RECEIVED_PROFILES_BYTES`] it keeps the list inside the storage
/// service's 4 MiB backup ceiling with room for the rest of the payload.
pub const MAX_RECEIVED_PROFILES: usize = 4096;

/// Most bytes the received-profile entries take together (key, length prefix
/// and envelope of every entry). Chosen, not measured: half the storage
/// service's 4 MiB backup ceiling.
pub const MAX_RECEIVED_PROFILES_BYTES: usize = 2 * 1024 * 1024;

/// Bytes an entry takes on the wire besides its envelope.
const RECEIVED_PROFILE_OVERHEAD: usize = RECEIVED_PROFILE_KEY_LEN + 2;

/// The newest profile envelope this device holds from one other person
/// (ADR-0048).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceivedProfile {
    /// The person's account signature key, the key the receiving client
    /// stored the profile under (C-68).
    pub person_key: [u8; RECEIVED_PROFILE_KEY_LEN],
    /// The profile envelope as received (`lakko_contacts::profile`), 1 to
    /// [`MAX_RECEIVED_PROFILE_LEN`] bytes. Opaque here: the client decodes it
    /// through its profile receive gate on restore.
    pub envelope: Vec<u8>,
}

impl ReceivedProfile {
    /// Bytes this entry takes on the wire.
    #[must_use]
    pub fn wire_len(&self) -> usize {
        RECEIVED_PROFILE_OVERHEAD + self.envelope.len()
    }
}

/// Choose, from `candidates` in priority order (most wanted first), the
/// received profiles a backup can carry, sorted by key as the codec requires.
///
/// The first candidate for a key wins and later ones for it are dropped; an
/// empty envelope or one over [`MAX_RECEIVED_PROFILE_LEN`] is dropped (it
/// cannot be a profile); candidates stop being taken once
/// [`MAX_RECEIVED_PROFILES`] are kept, and one that would take the list past
/// [`MAX_RECEIVED_PROFILES_BYTES`] is skipped. The result always encodes.
#[must_use]
pub fn fit_received_profiles(candidates: Vec<ReceivedProfile>) -> Vec<ReceivedProfile> {
    let mut seen = std::collections::BTreeSet::new();
    let mut kept = Vec::new();
    let mut bytes = 0usize;
    for candidate in candidates {
        if kept.len() == MAX_RECEIVED_PROFILES {
            break;
        }
        if candidate.envelope.is_empty() || candidate.envelope.len() > MAX_RECEIVED_PROFILE_LEN {
            continue;
        }
        if seen.contains(&candidate.person_key) {
            continue;
        }
        let len = candidate.wire_len();
        if bytes + len > MAX_RECEIVED_PROFILES_BYTES {
            continue;
        }
        bytes += len;
        seen.insert(candidate.person_key);
        kept.push(candidate);
    }
    kept.sort_by_key(|p| p.person_key);
    kept
}

/// One key the account rotated away from (ADR-0074 section 3.7).
///
/// `seed` opens the conversations: Stage A keeps every conversation leaf
/// under the oldest key, so a device restored from the newest phrase needs
/// this to mint a binding any conversation knows. The record lives inside the
/// backup, sealed under that phrase, whose holder already outranks it.
#[derive(Clone, PartialEq, Eq)]
pub struct OwnPredecessor {
    /// The predecessor's 64-byte phrase seed.
    pub seed: [u8; PREDECESSOR_SEED_LEN],
    /// When this account adopted the key that replaced it, unix millis.
    pub adopted_at_ms: u64,
}

impl core::fmt::Debug for OwnPredecessor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OwnPredecessor")
            .field("seed", &"<redacted>")
            .field("adopted_at_ms", &self.adopted_at_ms)
            .finish()
    }
}

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
    /// Sender's 16-byte account_id, when known. `None` for a contact
    /// accepted before the account_id was learned. (It once resolved the
    /// ADR-0021 SFU peer tag, which ADR-0078 section 2 removed; a call
    /// roster now names a peer from its own signed announcement.)
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
    /// fold would refuse is refused whole. One exception on decode: a state
    /// whose version byte this build does not read decodes as empty, so the
    /// rest of an older backup still restores; the state itself is lost.
    pub synced_state: Vec<u8>,
    /// The account's own predecessors, oldest first, at most
    /// [`MAX_OWN_PREDECESSORS`]. Empty for an account that never rotated.
    pub own_predecessors: Vec<OwnPredecessor>,
    /// The account's own profile envelope (`lakko_contacts::profile`). Empty
    /// when the person set no name. Opaque here, at most
    /// [`MAX_OWN_ENVELOPE_LEN`] bytes.
    pub own_profile: Vec<u8>,
    /// The account's own "who can find me" envelope
    /// (`lakko_contacts::visibility`). Empty when none was chosen. Opaque
    /// here, at most [`MAX_OWN_ENVELOPE_LEN`] bytes.
    pub visibility: Vec<u8>,
    /// The newest profile envelope this device holds from each other person,
    /// sorted by `person_key` with no key twice, at most
    /// [`MAX_RECEIVED_PROFILES`] entries and [`MAX_RECEIVED_PROFILES_BYTES`]
    /// in all. [`fit_received_profiles`] builds a list that satisfies this.
    ///
    /// On restore the client feeds each envelope through its ordinary profile
    /// receive gate under `person_key`, so a newer name it already holds or
    /// later receives always wins over the backed-up one.
    pub received_profiles: Vec<ReceivedProfile>,
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
                + self.synced_state.len()
                + 1
                + self.own_predecessors.len() * (PREDECESSOR_SEED_LEN + 8)
                + 4
                + self.own_profile.len()
                + 4
                + self.visibility.len()
                + 4
                + self
                    .received_profiles
                    .iter()
                    .map(ReceivedProfile::wire_len)
                    .sum::<usize>(),
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

        // ADR-0074 section 3.7: the keys the conversations still name.
        let predecessor_count = u8::try_from(self.own_predecessors.len())
            .ok()
            .filter(|count| usize::from(*count) <= MAX_OWN_PREDECESSORS)
            .ok_or_else(|| {
                BackupError::InvalidPayload(format!(
                    "own_predecessors count {} exceeds {MAX_OWN_PREDECESSORS}",
                    self.own_predecessors.len(),
                ))
            })?;
        out.push(predecessor_count);
        for predecessor in &self.own_predecessors {
            out.extend_from_slice(&predecessor.seed);
            out.extend_from_slice(&predecessor.adopted_at_ms.to_be_bytes());
        }

        put_own_envelope(&mut out, "own_profile", &self.own_profile)?;
        put_own_envelope(&mut out, "visibility", &self.visibility)?;
        put_received_profiles(&mut out, &self.received_profiles)?;

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
    ///   non-UTF-8 display name, a synced state of the current version
    ///   the fold refuses). A synced state of any other version is not an
    ///   error: it decodes as empty.
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
        // The count is attacker-chosen: reserve no more contacts than the
        // remaining bytes can hold (each starts with a PUBKEY_LEN key), so a
        // short blob cannot ask for gigabytes before `decode_contact` fails.
        let mut accepted_contacts = Vec::with_capacity(contact_count.min(cur.len() / PUBKEY_LEN));
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
        // A state of a version this build cannot read is dropped, not refused
        // (see `readable_synced_state`).
        let synced_state = readable_synced_state(take_n(&mut cur, state_len)?)?;

        let predecessor_count = usize::from(take_u8(&mut cur)?);
        if predecessor_count > MAX_OWN_PREDECESSORS {
            return Err(BackupError::InvalidPayload(format!(
                "own_predecessors count {predecessor_count} exceeds {MAX_OWN_PREDECESSORS}",
            )));
        }
        let mut own_predecessors = Vec::with_capacity(predecessor_count);
        for _ in 0..predecessor_count {
            let mut seed = [0u8; PREDECESSOR_SEED_LEN];
            seed.copy_from_slice(take_n(&mut cur, PREDECESSOR_SEED_LEN)?);
            let adopted_at_ms = u64::from_be_bytes(
                take_n(&mut cur, 8)?
                    .try_into()
                    .expect("take_n(8) length pinned"),
            );
            own_predecessors.push(OwnPredecessor {
                seed,
                adopted_at_ms,
            });
        }

        let own_profile = take_own_envelope(&mut cur, "own_profile")?;
        let visibility = take_own_envelope(&mut cur, "visibility")?;
        let received_profiles = take_received_profiles(&mut cur)?;

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
            own_predecessors,
            own_profile,
            visibility,
            received_profiles,
        })
    }
}

/// Write one length-prefixed own envelope, refusing one over its cap.
fn put_own_envelope(out: &mut Vec<u8>, field: &str, bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_OWN_ENVELOPE_LEN {
        return Err(BackupError::InvalidPayload(format!(
            "{field} length {} exceeds {MAX_OWN_ENVELOPE_LEN}",
            bytes.len(),
        )));
    }
    let len = u32::try_from(bytes.len())
        .map_err(|_| BackupError::InvalidPayload(format!("{field} length overflows u32")))?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

/// Read one length-prefixed own envelope, checking the cap before the read.
fn take_own_envelope(cur: &mut &[u8], field: &str) -> Result<Vec<u8>> {
    let len =
        u32::from_be_bytes(take_n(cur, 4)?.try_into().expect("take_n(4) length pinned")) as usize;
    if len > MAX_OWN_ENVELOPE_LEN {
        return Err(BackupError::InvalidPayload(format!(
            "{field} length {len} exceeds {MAX_OWN_ENVELOPE_LEN}",
        )));
    }
    Ok(take_n(cur, len)?.to_vec())
}

/// Write the received-profile list, refusing one the decoder would refuse.
fn put_received_profiles(out: &mut Vec<u8>, profiles: &[ReceivedProfile]) -> Result<()> {
    if profiles.len() > MAX_RECEIVED_PROFILES {
        return Err(BackupError::InvalidPayload(format!(
            "received_profiles count {} exceeds {MAX_RECEIVED_PROFILES}",
            profiles.len(),
        )));
    }
    let mut bytes = 0usize;
    let mut previous: Option<&[u8; RECEIVED_PROFILE_KEY_LEN]> = None;
    for profile in profiles {
        check_received_envelope_len(profile.envelope.len())?;
        if previous.is_some_and(|p| p >= &profile.person_key) {
            return Err(BackupError::InvalidPayload(
                "received_profiles keys not strictly increasing".to_string(),
            ));
        }
        previous = Some(&profile.person_key);
        bytes += profile.wire_len();
        if bytes > MAX_RECEIVED_PROFILES_BYTES {
            return Err(BackupError::InvalidPayload(format!(
                "received_profiles exceed {MAX_RECEIVED_PROFILES_BYTES} bytes",
            )));
        }
    }
    let count = u32::try_from(profiles.len())
        .map_err(|_| BackupError::InvalidPayload("received_profiles count overflows".into()))?;
    out.extend_from_slice(&count.to_be_bytes());
    for profile in profiles {
        out.extend_from_slice(&profile.person_key);
        let len = u16::try_from(profile.envelope.len())
            .map_err(|_| BackupError::InvalidPayload("received profile overflows u16".into()))?;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&profile.envelope);
    }
    Ok(())
}

/// Read the received-profile list: count bound before any entry, length and
/// running-size bounds before each read, keys strictly increasing.
fn take_received_profiles(cur: &mut &[u8]) -> Result<Vec<ReceivedProfile>> {
    let count =
        u32::from_be_bytes(take_n(cur, 4)?.try_into().expect("take_n(4) length pinned")) as usize;
    if count > MAX_RECEIVED_PROFILES {
        return Err(BackupError::InvalidPayload(format!(
            "received_profiles count {count} exceeds {MAX_RECEIVED_PROFILES}",
        )));
    }
    // Grown rather than pre-sized, like the group ids: the count is off the
    // wire.
    let mut profiles: Vec<ReceivedProfile> = Vec::new();
    let mut bytes = 0usize;
    for _ in 0..count {
        let mut person_key = [0u8; RECEIVED_PROFILE_KEY_LEN];
        person_key.copy_from_slice(take_n(cur, RECEIVED_PROFILE_KEY_LEN)?);
        if profiles.last().is_some_and(|p| p.person_key >= person_key) {
            return Err(BackupError::InvalidPayload(
                "received_profiles keys not strictly increasing".to_string(),
            ));
        }
        let len = usize::from(u16::from_be_bytes(
            take_n(cur, 2)?.try_into().expect("take_n(2) length pinned"),
        ));
        check_received_envelope_len(len)?;
        bytes += RECEIVED_PROFILE_OVERHEAD + len;
        if bytes > MAX_RECEIVED_PROFILES_BYTES {
            return Err(BackupError::InvalidPayload(format!(
                "received_profiles exceed {MAX_RECEIVED_PROFILES_BYTES} bytes",
            )));
        }
        let envelope = take_n(cur, len)?.to_vec();
        profiles.push(ReceivedProfile {
            person_key,
            envelope,
        });
    }
    Ok(profiles)
}

/// A received profile envelope is 1 to [`MAX_RECEIVED_PROFILE_LEN`] bytes.
fn check_received_envelope_len(len: usize) -> Result<()> {
    if len == 0 || len > MAX_RECEIVED_PROFILE_LEN {
        return Err(BackupError::InvalidPayload(format!(
            "received profile length {len} outside 1..={MAX_RECEIVED_PROFILE_LEN}",
        )));
    }
    Ok(())
}

/// Refuse synced-state bytes the fold would refuse. Empty is "none held".
///
/// The encode side: a device only ever writes the state this build holds,
/// so any refusal here, an unknown version byte included, is a caller bug.
fn check_synced_state(bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    lakko_sync::SyncedState::from_bytes(bytes)
        .map(|_| ())
        .map_err(|e| BackupError::InvalidPayload(format!("synced_state refused: {e}")))
}

/// The decode side: the synced state a restore may use, or empty.
///
/// A state whose version byte is not the one this build reads
/// (`lakko_sync::synced::CodecError::UnknownVersion`, raised only by the
/// state's leading byte) is dropped and read as "none held": the persisted
/// synced-state version moves with every new register, and refusing the
/// whole payload then lost every older backup in full (seen 2026-11-15).
/// It is never partially applied. Any other refusal (a truncated state, an
/// unknown entry tag, an entry out of bounds, trailing bytes) still refuses
/// the whole payload: a state that names the current version and does not
/// parse is not a state an honest device wrote.
fn readable_synced_state(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    match lakko_sync::SyncedState::from_bytes(bytes) {
        Ok(_) => Ok(bytes.to_vec()),
        Err(lakko_sync::synced::CodecError::UnknownVersion) => Ok(Vec::new()),
        Err(e) => Err(BackupError::InvalidPayload(format!(
            "synced_state refused: {e}"
        ))),
    }
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
            own_predecessors: Vec::new(),
            own_profile: Vec::new(),
            visibility: Vec::new(),
            received_profiles: Vec::new(),
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
    fn the_own_predecessors_round_trip_oldest_first() {
        let mut payload = sample_payload();
        payload.own_predecessors = vec![
            OwnPredecessor {
                seed: [1; PREDECESSOR_SEED_LEN],
                adopted_at_ms: 10,
            },
            OwnPredecessor {
                seed: [2; PREDECESSOR_SEED_LEN],
                adopted_at_ms: 20,
            },
        ];
        let decoded = BackupPayload::decode(&payload.encode().unwrap()).unwrap();
        assert_eq!(decoded.own_predecessors, payload.own_predecessors);
    }

    #[test]
    fn a_predecessor_seed_never_reaches_a_debug_line() {
        let predecessor = OwnPredecessor {
            seed: [0xAB; PREDECESSOR_SEED_LEN],
            adopted_at_ms: 10,
        };
        assert!(!format!("{predecessor:?}").contains("171"));
    }

    #[test]
    fn more_predecessors_than_the_bound_are_refused_both_ways() {
        let mut payload = sample_payload();
        payload.own_predecessors = vec![
            OwnPredecessor {
                seed: [1; PREDECESSOR_SEED_LEN],
                adopted_at_ms: 0,
            };
            MAX_OWN_PREDECESSORS + 1
        ];
        assert!(payload.encode().is_err());

        payload.own_predecessors.truncate(MAX_OWN_PREDECESSORS);
        let mut wire = payload.encode().unwrap();
        let count_at = wire.len() - 12 - MAX_OWN_PREDECESSORS * (PREDECESSOR_SEED_LEN + 8) - 1;
        wire[count_at] = u8::try_from(MAX_OWN_PREDECESSORS + 1).unwrap();
        assert!(BackupPayload::decode(&wire).is_err());
    }

    #[test]
    fn a_truncated_predecessor_is_refused() {
        let mut payload = sample_payload();
        payload.own_predecessors = vec![OwnPredecessor {
            seed: [1; PREDECESSOR_SEED_LEN],
            adopted_at_ms: 0,
        }];
        let mut wire = payload.encode().unwrap();
        // Drop the two empty envelopes and the empty received list, then one
        // byte of the predecessor.
        wire.truncate(wire.len() - 12);
        assert!(BackupPayload::decode(&wire[..wire.len() - 1]).is_err());
    }

    #[test]
    fn the_own_name_and_visibility_round_trip() {
        let mut payload = sample_payload();
        payload.own_profile = vec![1, 2, 3];
        payload.visibility = vec![4, 5];
        let decoded = BackupPayload::decode(&payload.encode().unwrap()).unwrap();
        assert_eq!(decoded.own_profile, payload.own_profile);
        assert_eq!(decoded.visibility, payload.visibility);
    }

    #[test]
    fn an_own_envelope_over_its_cap_is_refused_both_ways() {
        let mut payload = sample_payload();
        payload.visibility = vec![0; MAX_OWN_ENVELOPE_LEN + 1];
        assert!(payload.encode().is_err());

        payload.visibility = Vec::new();
        let mut wire = payload.encode().unwrap();
        // The visibility length sits before the empty received list.
        let at = wire.len() - 8;
        let too_long = u32::try_from(MAX_OWN_ENVELOPE_LEN + 1).unwrap();
        wire[at..at + 4].copy_from_slice(&too_long.to_be_bytes());
        assert!(BackupPayload::decode(&wire).is_err());
    }

    #[test]
    fn a_length_past_the_end_of_an_own_envelope_is_refused() {
        let payload = sample_payload();
        let mut wire = payload.encode().unwrap();
        // Drop the received list so the visibility is the tail, then claim
        // one byte of it.
        wire.truncate(wire.len() - 4);
        let tail = wire.len() - 4;
        wire[tail..].copy_from_slice(&[0, 0, 0, 1]);
        assert!(BackupPayload::decode(&wire).is_err());
    }

    #[test]
    fn the_payload_version_is_eight() {
        assert_eq!(PAYLOAD_VERSION, 8);
        assert_eq!(sample_payload().encode().unwrap()[0], 8);
    }

    #[test]
    fn the_v8_wire_layout_is_pinned() {
        // One contact with an account id, one 3-byte group id, no synced
        // state, one predecessor: every field of the layout appears once, in
        // order, and nothing sits between the contacts and the group ids.
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
            own_predecessors: vec![OwnPredecessor {
                seed: [0x77; PREDECESSOR_SEED_LEN],
                adopted_at_ms: 0x1112_1314_1516_1718,
            }],
            own_profile: vec![0xAA, 0xAB],
            visibility: vec![0xBB],
            received_profiles: vec![ReceivedProfile {
                person_key: [0x55; RECEIVED_PROFILE_KEY_LEN],
                envelope: vec![0xC1, 0xC2, 0xC3],
            }],
        };
        let mut expected = vec![8u8];
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
        expected.push(1);
        expected.extend_from_slice(&[0x77; PREDECESSOR_SEED_LEN]);
        expected.extend_from_slice(&[0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18]);
        expected.extend_from_slice(&[0, 0, 0, 2, 0xAA, 0xAB]);
        expected.extend_from_slice(&[0, 0, 0, 1, 0xBB]);
        expected.extend_from_slice(&[0, 0, 0, 1]);
        expected.extend_from_slice(&[0x55; RECEIVED_PROFILE_KEY_LEN]);
        expected.extend_from_slice(&[0, 3, 0xC1, 0xC2, 0xC3]);
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
        let mut state = sample_synced_state();
        assert!(state.set_lineage(8, &[1; 32], &[2; 32]).unwrap());
        assert!(state.set_recovery_email(9, Some("a@b.example")).unwrap());
        assert!(state.set_own_username(10, Some("bob_42")).unwrap());
        let mut payload = sample_payload();
        payload.synced_state = state.to_bytes();
        let decoded = BackupPayload::decode(&payload.encode().unwrap()).unwrap();
        let restored = SyncedState::from_bytes(&decoded.synced_state).unwrap();
        assert_eq!(restored, state);
        assert_eq!(restored.conversation_id_of(&[1; 32]), vec![2; 32]);
        assert_eq!(restored.recovery_email().as_deref(), Some("a@b.example"));
        assert_eq!(restored.own_username().as_deref(), Some("bob_42"));

        // Garbage under the current version byte: refused both ways.
        let garbage = vec![current_state_version(), 9, 9, 9];
        payload.synced_state = garbage.clone();
        assert!(payload.encode().is_err());
        // Hand-built, because the encoder refuses to produce one.
        let wire = wire_with_synced_state(&sample_payload(), &garbage);
        assert!(BackupPayload::decode(&wire).is_err());
    }

    /// The version byte this build's synced state carries.
    fn current_state_version() -> u8 {
        lakko_sync::SyncedState::new().to_bytes()[0]
    }

    /// `payload` encoded with `state` as its synced-state bytes, whatever
    /// they are: the encoder refuses a state it cannot read, so the bytes are
    /// spliced into the wire at the state's length field.
    fn wire_with_synced_state(payload: &BackupPayload, state: &[u8]) -> Vec<u8> {
        let mut without = payload.clone();
        without.synced_state = Vec::new();
        let wire = without.encode().unwrap();
        let mut head = 1 + ACCOUNT_ID_LEN + 8 + SEED_LEN + 4;
        for c in &payload.accepted_contacts {
            let mut scratch = Vec::new();
            encode_contact(&mut scratch, c).unwrap();
            head += scratch.len();
        }
        head += 4 + payload
            .known_group_ids
            .iter()
            .map(|id| 4 + id.len())
            .sum::<usize>();
        assert_eq!(&wire[head..head + 4], &[0u8; 4], "state length field");
        let mut out = wire[..head].to_vec();
        out.extend_from_slice(&u32::try_from(state.len()).unwrap().to_be_bytes());
        out.extend_from_slice(state);
        out.extend_from_slice(&wire[head + 4..]);
        out
    }

    /// A payload with every field filled, so a restore that drops the
    /// synced state can be seen to keep everything else.
    fn full_payload() -> BackupPayload {
        let mut payload = sample_payload();
        payload.own_predecessors = vec![OwnPredecessor {
            seed: [0x5A; PREDECESSOR_SEED_LEN],
            adopted_at_ms: 1_690_000_000_000,
        }];
        payload.own_profile = b"own-profile-envelope".to_vec();
        payload.visibility = b"visibility-envelope".to_vec();
        payload.received_profiles = vec![received(1, b"first"), received(2, b"second")];
        payload
    }

    #[test]
    fn a_synced_state_of_another_version_is_dropped_and_the_rest_restores() {
        let current = current_state_version();
        let payload = full_payload();
        let mut expected = payload.clone();
        expected.synced_state = Vec::new();
        let mut older = sample_synced_state().to_bytes();
        // Older, newer, the far ends of the byte, and a version byte alone:
        // the version byte decides, the rest of the state is never read.
        for version in [current - 2, current - 1, current + 1, 0, u8::MAX] {
            older[0] = version;
            for state in [older.clone(), vec![version]] {
                let wire = wire_with_synced_state(&payload, &state);
                let decoded = BackupPayload::decode(&wire)
                    .unwrap_or_else(|e| panic!("version {version} refused the payload: {e:?}"));
                assert_eq!(decoded, expected, "version {version}");
                // What it decoded to round-trips on its own.
                let reencoded = decoded.encode().unwrap();
                assert_eq!(BackupPayload::decode(&reencoded).unwrap(), expected);
                assert_eq!(reencoded, expected.encode().unwrap());
            }
            // The encoder still refuses to write such a state.
            let mut writing = payload.clone();
            writing.synced_state = older.clone();
            assert!(writing.encode().is_err(), "version {version} encoded");
        }
    }

    #[test]
    fn a_malformed_synced_state_of_the_current_version_still_refuses_the_payload() {
        let current = current_state_version();
        let good = sample_synced_state().to_bytes();
        assert_eq!(good[0], current);
        let mut trailing = good.clone();
        trailing.push(0);
        let mut bad_tag = good.clone();
        // The first entry's tag: version(1) + count(4) + hlc(8) + length(4).
        bad_tag[17] = 0xFF;
        assert_eq!(
            lakko_sync::SyncedState::from_bytes(&bad_tag),
            Err(lakko_sync::synced::CodecError::UnknownTag)
        );
        for state in [
            vec![current],                   // truncated before the count
            good[..good.len() - 1].to_vec(), // truncated entry
            trailing,                        // trailing bytes
            bad_tag,                         // unknown entry tag
        ] {
            let wire = wire_with_synced_state(&full_payload(), &state);
            assert!(
                BackupPayload::decode(&wire).is_err(),
                "a malformed current-version state was accepted: {state:?}"
            );
        }
        // The splice itself is sound: the good state decodes intact.
        let wire = wire_with_synced_state(&full_payload(), &good);
        assert_eq!(BackupPayload::decode(&wire).unwrap().synced_state, good);
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
            own_predecessors: Vec::new(),
            own_profile: Vec::new(),
            visibility: Vec::new(),
            received_profiles: Vec::new(),
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
            own_predecessors: Vec::new(),
            own_profile: Vec::new(),
            visibility: Vec::new(),
            received_profiles: Vec::new(),
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
            own_predecessors: Vec::new(),
            own_profile: Vec::new(),
            visibility: Vec::new(),
            received_profiles: Vec::new(),
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

    fn received(key: u8, envelope: &[u8]) -> ReceivedProfile {
        ReceivedProfile {
            person_key: [key; RECEIVED_PROFILE_KEY_LEN],
            envelope: envelope.to_vec(),
        }
    }

    /// The wire of `payload` with its received list replaced by `entries`
    /// written raw (key, u16 length, bytes), so the decoder sees what the
    /// encoder refuses to write.
    fn with_raw_received(payload: &BackupPayload, count: u32, entries: &[(u8, &[u8])]) -> Vec<u8> {
        let mut base = payload.clone();
        base.received_profiles = Vec::new();
        let mut wire = base.encode().unwrap();
        wire.truncate(wire.len() - 4);
        wire.extend_from_slice(&count.to_be_bytes());
        for (key, envelope) in entries {
            wire.extend_from_slice(&[*key; RECEIVED_PROFILE_KEY_LEN]);
            wire.extend_from_slice(&u16::try_from(envelope.len()).unwrap().to_be_bytes());
            wire.extend_from_slice(envelope);
        }
        wire
    }

    #[test]
    fn the_received_profiles_round_trip() {
        let mut payload = sample_payload();
        payload.received_profiles = vec![received(1, b"one"), received(2, b"two")];
        let wire = payload.encode().unwrap();
        assert_eq!(BackupPayload::decode(&wire).unwrap(), payload);
    }

    #[test]
    fn the_bound_is_the_profile_codecs_own() {
        assert_eq!(
            MAX_RECEIVED_PROFILE_LEN,
            lakko_contacts::PROFILE_ENVELOPE_MAX_LEN
        );
    }

    #[test]
    fn a_v7_payload_is_refused() {
        // A v7 body is a v8 body without the received list.
        let mut wire = sample_payload().encode().unwrap();
        wire.truncate(wire.len() - 4);
        assert!(
            BackupPayload::decode(&wire).is_err(),
            "a v7 body stamped v8 decoded"
        );
        wire[0] = 7;
        let err = BackupPayload::decode(&wire).unwrap_err();
        assert!(
            matches!(&err, BackupError::InvalidPayload(m) if m.contains("version 7")),
            "a v7 payload was not refused on its version: {err:?}"
        );
    }

    #[test]
    fn an_envelope_out_of_its_bounds_is_refused_both_ways() {
        let max = vec![0xE0; MAX_RECEIVED_PROFILE_LEN];
        let over = vec![0xE0; MAX_RECEIVED_PROFILE_LEN + 1];
        let mut payload = sample_payload();
        payload.received_profiles = vec![received(1, &max)];
        assert!(payload.encode().is_ok());
        for bad in [&[][..], &over[..]] {
            payload.received_profiles = vec![received(1, bad)];
            assert!(payload.encode().is_err());
            assert!(BackupPayload::decode(&with_raw_received(&payload, 1, &[(1, bad)])).is_err());
        }
    }

    #[test]
    fn keys_out_of_order_or_twice_are_refused_both_ways() {
        let mut payload = sample_payload();
        for keys in [[2u8, 1], [1, 1]] {
            payload.received_profiles = vec![received(keys[0], b"a"), received(keys[1], b"b")];
            assert!(payload.encode().is_err(), "{keys:?} encoded");
            let wire = with_raw_received(&payload, 2, &[(keys[0], b"a"), (keys[1], b"b")]);
            assert!(BackupPayload::decode(&wire).is_err(), "{keys:?} decoded");
        }
    }

    #[test]
    fn a_count_over_the_bound_is_refused_before_any_entry() {
        let payload = sample_payload();
        let count = u32::try_from(MAX_RECEIVED_PROFILES + 1).unwrap();
        // No entries follow: the count alone must be refused.
        let err = BackupPayload::decode(&with_raw_received(&payload, count, &[])).unwrap_err();
        assert!(matches!(&err, BackupError::InvalidPayload(m) if m.contains("count")));
        let mut too_many = sample_payload();
        too_many.received_profiles = (0..=MAX_RECEIVED_PROFILES)
            .map(|i| ReceivedProfile {
                person_key: key_of(i),
                envelope: vec![1],
            })
            .collect();
        assert!(too_many.encode().is_err());
    }

    fn key_of(i: usize) -> [u8; RECEIVED_PROFILE_KEY_LEN] {
        let mut key = [0u8; RECEIVED_PROFILE_KEY_LEN];
        key[..8].copy_from_slice(&(i as u64).to_be_bytes());
        key
    }

    #[test]
    fn a_list_over_the_byte_bound_is_refused_both_ways() {
        let per = MAX_RECEIVED_PROFILE_LEN;
        let fits = MAX_RECEIVED_PROFILES_BYTES / (RECEIVED_PROFILE_OVERHEAD + per);
        assert!(
            fits < MAX_RECEIVED_PROFILES,
            "the byte bound must be the one that bites"
        );
        let list: Vec<ReceivedProfile> = (0..=fits)
            .map(|i| ReceivedProfile {
                person_key: key_of(i),
                envelope: vec![0xE1; per],
            })
            .collect();
        let mut payload = sample_payload();
        payload.received_profiles = list[..fits].to_vec();
        let wire = payload.encode().unwrap();
        assert_eq!(BackupPayload::decode(&wire).unwrap(), payload);

        payload.received_profiles = list.clone();
        assert!(payload.encode().is_err());
        // Hand-built: the same list, written raw.
        let mut raw = sample_payload();
        raw.received_profiles = Vec::new();
        let mut wire = raw.encode().unwrap();
        wire.truncate(wire.len() - 4);
        wire.extend_from_slice(&u32::try_from(list.len()).unwrap().to_be_bytes());
        for entry in &list {
            wire.extend_from_slice(&entry.person_key);
            wire.extend_from_slice(&u16::try_from(per).unwrap().to_be_bytes());
            wire.extend_from_slice(&entry.envelope);
        }
        assert!(BackupPayload::decode(&wire).is_err());
    }

    #[test]
    fn a_truncated_received_entry_is_refused() {
        let mut payload = sample_payload();
        payload.received_profiles = vec![received(1, b"abc")];
        let wire = payload.encode().unwrap();
        for cut in 1..=(RECEIVED_PROFILE_OVERHEAD + 3) {
            assert!(
                BackupPayload::decode(&wire[..wire.len() - cut]).is_err(),
                "cut {cut}"
            );
        }
    }

    #[test]
    fn fitting_keeps_the_first_per_key_drops_unusable_and_sorts() {
        let fitted = fit_received_profiles(vec![
            received(3, b"three"),
            received(1, b"one"),
            received(3, b"older three"),
            received(2, b""),
            received(4, &vec![0; MAX_RECEIVED_PROFILE_LEN + 1]),
        ]);
        assert_eq!(fitted, vec![received(1, b"one"), received(3, b"three")]);
    }

    #[test]
    fn fitting_stops_at_the_count_bound_in_priority_order() {
        let candidates: Vec<ReceivedProfile> = (0..MAX_RECEIVED_PROFILES + 10)
            .rev()
            .map(|i| ReceivedProfile {
                person_key: key_of(i),
                envelope: vec![1],
            })
            .collect();
        let fitted = fit_received_profiles(candidates);
        assert_eq!(fitted.len(), MAX_RECEIVED_PROFILES);
        // The first candidates were the highest keys; the lowest ten lost.
        assert_eq!(fitted[0].person_key, key_of(10));
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        fn candidate() -> impl Strategy<Value = ReceivedProfile> {
            (
                any::<[u8; 4]>(),
                prop::collection::vec(any::<u8>(), 0..=MAX_RECEIVED_PROFILE_LEN + 2),
            )
                .prop_map(|(prefix, envelope)| {
                    let mut person_key = [0u8; RECEIVED_PROFILE_KEY_LEN];
                    person_key[..4].copy_from_slice(&prefix);
                    ReceivedProfile {
                        person_key,
                        envelope,
                    }
                })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(64))]

            /// Whatever a client hands in, the fitted list encodes, decodes to
            /// itself, re-encodes to the same bytes, and keeps the bounds.
            #[test]
            fn a_fitted_list_round_trips_and_keeps_the_bounds(
                candidates in prop::collection::vec(candidate(), 0..64),
            ) {
                let mut payload = sample_payload();
                payload.received_profiles = fit_received_profiles(candidates.clone());
                let wire = payload.encode().unwrap();
                let decoded = BackupPayload::decode(&wire).unwrap();
                prop_assert_eq!(&decoded, &payload);
                prop_assert_eq!(decoded.encode().unwrap(), wire);
                let list = &decoded.received_profiles;
                prop_assert!(list.len() <= MAX_RECEIVED_PROFILES);
                prop_assert!(
                    list.iter().map(ReceivedProfile::wire_len).sum::<usize>()
                        <= MAX_RECEIVED_PROFILES_BYTES
                );
                prop_assert!(list.windows(2).all(|w| w[0].person_key < w[1].person_key));
                for kept in list {
                    // The kept envelope is the first candidate for its key.
                    let first = candidates.iter().find(|c| {
                        c.person_key == kept.person_key
                            && !c.envelope.is_empty()
                            && c.envelope.len() <= MAX_RECEIVED_PROFILE_LEN
                    });
                    prop_assert_eq!(first.map(|c| &c.envelope), Some(&kept.envelope));
                }
            }

            /// Any synced-state bytes: the payload decodes with the state kept
            /// when the fold reads it, decodes with it dropped and every other
            /// field intact when only its version byte is foreign, and is
            /// refused otherwise. Never a partial state, never a panic.
            #[test]
            fn a_synced_state_is_kept_dropped_or_refused_whole(
                version in any::<u8>(),
                body in prop::collection::vec(any::<u8>(), 0..64),
                use_real_body in any::<bool>(),
            ) {
                let mut state = if use_real_body {
                    sample_synced_state().to_bytes()
                } else {
                    let mut raw = vec![0u8];
                    raw.extend_from_slice(&body);
                    raw
                };
                state[0] = version;
                let payload = full_payload();
                let wire = wire_with_synced_state(&payload, &state);
                let decoded = BackupPayload::decode(&wire);
                match lakko_sync::SyncedState::from_bytes(&state) {
                    Ok(_) => {
                        let decoded = decoded.unwrap();
                        prop_assert_eq!(&decoded.synced_state, &state);
                        prop_assert_eq!(decoded.encode().unwrap(), wire);
                    }
                    Err(lakko_sync::synced::CodecError::UnknownVersion) => {
                        prop_assert_ne!(version, current_state_version());
                        let mut expected = payload.clone();
                        expected.synced_state = Vec::new();
                        prop_assert_eq!(decoded.unwrap(), expected);
                    }
                    Err(_) => prop_assert!(decoded.is_err()),
                }
            }

            /// Arbitrary bytes after the visibility never panic the decoder,
            /// and whatever it accepts re-encodes to the same bytes.
            #[test]
            fn an_arbitrary_received_tail_never_panics(
                tail in prop::collection::vec(any::<u8>(), 0..512),
            ) {
                let mut base = sample_payload();
                base.received_profiles = Vec::new();
                let mut wire = base.encode().unwrap();
                wire.truncate(wire.len() - 4);
                wire.extend_from_slice(&tail);
                if let Ok(decoded) = BackupPayload::decode(&wire) {
                    prop_assert_eq!(decoded.encode().unwrap(), wire);
                }
            }
        }
    }
}
