// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! PublisherGroup — persistent MLS-group wrapper for the
//! Phase 4 channels publisher path
//! ([ADR-0010](../../../docs/adr/0010-channels-two-tier-broadcast.md)
//! Tier 1), closing G4-2-mls-persistence.
//!
//! ## What this module is
//!
//! Phase 4 channels use a two-tier broadcast architecture:
//!
//! 1. **PublisherGroup** — a vanilla MLS group of ≤ 500 owners
//!    / moderators / verified authors. Membership changes happen
//!    here, and the group's MLS exporter is the authority for
//!    the `content_key_seed` the rest of the channel rides on.
//! 2. **Subscriber tier** — receives the seed via HPKE-sealed
//!    envelopes (see [`lakko_channels::subscribe`]).
//!
//! For Tier 1 to function as the rotation authority the
//! publisher's MLS state MUST survive process restarts. Before
//! this module, client composers (iOS / Android / web) built a
//! throwaway group on every publish — that gave correct
//! crypto on a single publish but reset the group's epoch back
//! to 0 every time the app reopened, so the exporter output
//! also reset and subscribers who had already received a
//! sealed seed via the previous epoch's rotation could no
//! longer decrypt new posts.
//!
//! ## What this module does
//!
//! [`PublisherGroup`] is a thin wrapper around [`Group`] that:
//!
//! * Derives a **deterministic** MLS group id from a secret of the
//!   channel owner and the 16-byte channel id
//!   ([`publisher_group_id_for`], ADR-0078 section 6):
//!   `BLAKE3-derive-key("lakko/pubgrp/id/v3", owner_account_seed ||
//!   channel_id)`. The owner's account seed is the Ed25519 seed of the
//!   identity whose provider holds the group, so the owner recomputes the id
//!   from what it already holds and [`Group::load`] has something stable to
//!   look up. Until ADR-0078 the id was `b"lakko/pubgrp/v2/" || channel_id`:
//!   `services/channels` knows every channel id, so the PublisherGroup
//!   journal on the Delivery Service was attributable to its channel. No
//!   server can compute the id now, and nothing inverts it: a co-publisher
//!   learns the channel id from the sealed preview of its `/g/` link and the
//!   group id from the capability, keeps both, and opens the group it joined
//!   with [`PublisherGroup::load_joined`].
//! * Exposes [`PublisherGroup::open_or_create`] — the
//!   load-or-create dance every composer needs. Reuses the
//!   already-persisted state when it exists; otherwise
//!   minted-and-persisted at first publish.
//! * Pins the exporter label + length conventions in one place
//!   so iOS / Android / web composers don't drift on the
//!   `"lakko/channel/v1"` / 32-byte boundary.
//!
//! Persistence is delegated entirely to
//! [`Identity::provider.storage()`] — the same SQLCipher
//! backing store the rest of [`Group::load`] uses. No new
//! schema; OpenMLS's serialised `MlsGroup` blob lands in the
//! `openmls_kv` table OpenMLS itself manages.
//!
//! ## What this module does NOT do
//!
//! * **Add / remove members.** PublisherGroup membership
//!   churn (promote a new moderator, demote an editor, etc.)
//!   goes through the standard [`Group::add_member`] /
//!   [`Group::remove_member`] surface. This module is the
//!   creation + exporter-seed shape; the membership ops sit
//!   on top.
//! * **Distribute the rotated seed.** That's
//!   [`lakko_channels::subscribe::seal_seed`] plus the
//!   `services/channels` batch fanout. PublisherGroup
//!   produces the bytes; subscribers receive them via the
//!   already-shipped path.
//! * **The 24 h / N-message scheduled rotation.** The cadence
//!   decision lives in the pure `RotationPolicy::should_rotate`
//!   in `lakko-channels` (24 h / 1 000 posts by default), driven
//!   by the per-platform rotation crons (web / iOS / Android).
//!   This module exposes a fresh seed on demand; that scheduling
//!   logic sits above it and calls "commit + rotate" when the
//!   policy fires.
//!
//! ## Threading
//!
//! Same posture as [`Group`]: not `Sync`. Wrap in a
//! `Mutex<PublisherGroup>` at the FFI boundary if multi-
//! threaded access is needed (matches the
//! `Mutex<ParticipantKey>` pattern from G3-sframe).
//!
//! ## Audit anchor
//!
//! `C-44` in [`docs/security/calls.md`](../../../docs/security/calls.md)
//! already pinned the MLS-exporter-as-channel-seed
//! invariant for the throwaway-group path. With this
//! module, the same invariant holds across process
//! restarts: the group's MLS state survives → the
//! exporter output for `(label, channel_id, 32)` survives
//! → the channel seed survives.

use crate::error::{Error, Result};
use crate::group::{Group, MemberAddition, GROUP_ID_LEN};
use crate::identity::Identity;

/// MLS-exporter domain-separation label for channel content
/// seeds. Mirrors the value
/// `lakko-channels::seed::CHANNEL_EXPORTER_LABEL` (the two
/// constants are intentionally identical so the iOS /
/// Android composers + web composer + this Rust wrapper all
/// agree on the exporter input).
pub const CHANNEL_EXPORTER_LABEL: &str = "lakko/channel/v1";

/// Length of a `channel_id`. ADR-0010 §"Identifiers" pins
/// channels at 16 bytes (same shape as `call_id` / `group_id`
/// pseudonyms — opaque server-side, deterministic prefix-
/// derivation for client-side persistence here).
pub const CHANNEL_ID_LEN: usize = 16;

/// 16-byte channel identifier. Newtype so the deterministic-
/// group-id derivation has a single source-of-truth shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChannelId([u8; CHANNEL_ID_LEN]);

impl ChannelId {
    /// Wrap raw bytes. Refuses any length other than
    /// [`CHANNEL_ID_LEN`].
    ///
    /// # Errors
    ///
    /// [`Error::Codec`] when `bytes.len() != CHANNEL_ID_LEN`.
    pub fn try_from_slice(bytes: &[u8]) -> Result<Self> {
        let arr: [u8; CHANNEL_ID_LEN] = bytes.try_into().map_err(|_| {
            Error::Codec(format!(
                "channel_id length {} != {CHANNEL_ID_LEN}",
                bytes.len()
            ))
        })?;
        Ok(Self(arr))
    }

    /// Raw bytes for FFI / exporter context use.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; CHANNEL_ID_LEN] {
        &self.0
    }
}

/// Domain-separation context of the PublisherGroup id derivation (ADR-0078
/// section 6). Pinned; a change re-addresses every channel's PublisherGroup.
const PUBLISHER_GROUP_ID_DOMAIN: &str = "lakko/pubgrp/id/v3";

// A PublisherGroup id is a Delivery Service journal routing key, which is
// exactly 32 bytes; the derivation's output width is that.
const _: () = assert!(GROUP_ID_LEN == 32);

/// The retired v1 prefix. Kept for exactly one purpose: letting
/// [`PublisherGroup::load`] *recognise* state persisted under the old
/// derivation so the caller gets a loud error instead of a silent fresh mint.
/// Nothing else may use it — it is not a fallback lookup path (see
/// [`PublisherGroup::load`] for why re-keying is impossible).
const PUBLISHER_GROUP_ID_PREFIX_V1: &[u8] = b"lakko/publisher-group/v1/";

/// The PublisherGroup id of the channel `channel_id` owned by the account
/// whose Ed25519 seed is `owner_account_seed`:
/// `BLAKE3-derive-key("lakko/pubgrp/id/v3", owner_account_seed ||
/// channel_id)`, exactly [`GROUP_ID_LEN`] bytes. Not computable from the
/// channel id alone.
#[must_use]
pub fn publisher_group_id_for(
    owner_account_seed: &[u8; 32],
    channel_id: &ChannelId,
) -> [u8; GROUP_ID_LEN] {
    let mut material = [0u8; 32 + CHANNEL_ID_LEN];
    material[..32].copy_from_slice(owner_account_seed);
    material[32..].copy_from_slice(channel_id.as_bytes());
    lakko_crypto::hash::blake3_derive_key_from_material(PUBLISHER_GROUP_ID_DOMAIN, &material)
}

/// The id of the PublisherGroup `identity` owns for `channel_id`.
fn publisher_group_id_bytes(identity: &Identity, channel_id: &ChannelId) -> [u8; GROUP_ID_LEN] {
    publisher_group_id_for(&identity.ed25519_seed_bytes(), channel_id)
}

/// The v1 id `channel_id` *would* have been persisted under. Used only by
/// [`refuse_if_legacy_v1_present`].
fn publisher_group_id_bytes_v1(channel_id: &ChannelId) -> Vec<u8> {
    let mut id = Vec::with_capacity(PUBLISHER_GROUP_ID_PREFIX_V1.len() + CHANNEL_ID_LEN);
    id.extend_from_slice(PUBLISHER_GROUP_ID_PREFIX_V1);
    id.extend_from_slice(channel_id.as_bytes());
    id
}

/// Refuse to proceed when this channel has MLS state persisted under the
/// retired v1 derivation. See [`PublisherGroup::load`] for why the state cannot
/// be migrated in place and why a silent fresh mint is the worse outcome.
///
/// One extra storage read, only on a v2 miss — i.e. once per channel, on the
/// path that is about to mint. `open_or_create` pays for it twice on a genuine
/// first mint (once via `load`'s miss, once via `create`); that is a single
/// additional key lookup against an empty result and is not worth threading a
/// "already checked" flag through the two public constructors to avoid.
///
/// # Errors
///
/// [`Error::Group`] when v1 state is present; propagates [`Group::load`]
/// failures otherwise.
fn refuse_if_legacy_v1_present(identity: &Identity, channel_id: &ChannelId) -> Result<()> {
    let v1 = publisher_group_id_bytes_v1(channel_id);
    if Group::load(identity, &v1)?.is_some() {
        return Err(Error::Group(
            "legacy v1 PublisherGroup state present for this channel: the v1 \
             group id was 41 bytes, which the Delivery Service rejects, and an \
             MLS group id is bound into the GroupContext so it cannot be \
             re-keyed to the 32-byte v2 id. The v1 state is left intact; this \
             channel needs a migration or a fresh channel."
                .into(),
        ));
    }
    Ok(())
}

/// Persistent MLS-group wrapper for one Lakko channel's
/// publisher tier (G4-2-mls-persistence).
pub struct PublisherGroup {
    inner: Group,
    channel_id: ChannelId,
}

impl PublisherGroup {
    /// Load the persisted PublisherGroup for `channel_id` from
    /// `identity`'s storage. Returns `Ok(None)` when no group
    /// has been minted yet for this channel; the caller should
    /// then call [`PublisherGroup::create`] (or use the
    /// load-or-create convenience [`PublisherGroup::open_or_create`]).
    ///
    /// ## Legacy v1 state
    ///
    /// A group persisted under the retired 41-byte v1 derivation is **not**
    /// migrated, and cannot be: an MLS `group_id` is a field of the
    /// `GroupContext` (openmls-0.8.1 `group/group_context.rs:46`), which is
    /// serialised into the key schedule (`schedule/mod.rs` `add_context`) and
    /// bound by every commit's confirmation tag. There is no `set_group_id` and
    /// there could not be one — renaming a group would change every epoch
    /// secret it has ever derived. A v1 PublisherGroup's identity *is* its
    /// 41-byte id; a 32-byte id is a different MLS group.
    ///
    /// So this method refuses rather than pretending. Returning `Ok(None)`
    /// would send [`PublisherGroup::open_or_create`] on to mint a fresh group
    /// at epoch 0 under the v2 id — silently swapping the channel's content-key
    /// seed while every subscriber still holds an envelope carrying the old
    /// one, so posts published before the next rotation fan-out would fail AEAD
    /// on arrival. Failing loudly leaves the v1 state untouched in storage for
    /// any future migration tool, and leaves the channel readable.
    ///
    /// # Errors
    ///
    /// Propagates [`Group::load`] errors, and returns [`Error::Group`] when no
    /// v2 group exists but v1 state for this channel does.
    pub fn load(identity: &Identity, channel_id: &ChannelId) -> Result<Option<Self>> {
        let id = publisher_group_id_bytes(identity, channel_id);
        if let Some(inner) = Group::load(identity, &id)? {
            return Ok(Some(Self {
                inner,
                channel_id: *channel_id,
            }));
        }
        refuse_if_legacy_v1_present(identity, channel_id)?;
        Ok(None)
    }

    /// Load a PublisherGroup this identity JOINED as a co-publisher (ADR-0028
    /// OQ3): its id is the owner's derivation, which a co-publisher cannot
    /// compute, so the caller passes the `group_id` it learned from the `/g/`
    /// capability and the `channel_id` from that link's sealed preview.
    /// Returns `Ok(None)` when nothing is stored under `group_id`. Never
    /// creates: minting under an id the owner derived would fork the
    /// channel.
    ///
    /// # Errors
    ///
    /// [`Error::Codec`] when `group_id` is not [`GROUP_ID_LEN`] bytes;
    /// propagates [`Group::load`] errors.
    pub fn load_joined(
        identity: &Identity,
        group_id: &[u8],
        channel_id: &ChannelId,
    ) -> Result<Option<Self>> {
        if group_id.len() != GROUP_ID_LEN {
            return Err(Error::Codec(format!(
                "publisher group id length {} != {GROUP_ID_LEN}",
                group_id.len()
            )));
        }
        Ok(Group::load(identity, group_id)?.map(|inner| Self {
            inner,
            channel_id: *channel_id,
        }))
    }

    /// Join a PublisherGroup as a co-publisher from the owner's Welcome
    /// (ADR-0028 OQ3; [ADR-0081] amendment of 2026-10-19). A PublisherGroup
    /// carries no role table, so [`Group::join_from_welcome`] refuses its
    /// Welcome; this is its entry point. `group_id` is the id the `/g/`
    /// capability conveyed and `channel_id` the one its sealed preview
    /// carried: a Welcome for any other group is refused before anything is
    /// written, and so is a conversation's Welcome. The joined state is
    /// persisted in `identity`'s provider; [`PublisherGroup::load_joined`]
    /// reopens it.
    ///
    /// # Errors
    ///
    /// [`Error::Codec`] when `group_id` is not [`GROUP_ID_LEN`] bytes or the
    /// Welcome does not decode; [`Error::Group`] for a Welcome of another
    /// group, a conversation's Welcome, or an OpenMLS failure (no KeyPackage
    /// of this identity matches, for one).
    ///
    /// [ADR-0081]: ../../../docs/adr/0081-devicegroup-commits-cannot-strand.md
    pub fn join_from_welcome(
        identity: &Identity,
        welcome_bytes: &[u8],
        group_id: &[u8],
        channel_id: &ChannelId,
    ) -> Result<Self> {
        if group_id.len() != GROUP_ID_LEN {
            return Err(Error::Codec(format!(
                "publisher group id length {} != {GROUP_ID_LEN}",
                group_id.len()
            )));
        }
        let inner = Group::join_table_less_from_welcome(welcome_bytes, identity, Some(group_id))?;
        Ok(Self {
            inner,
            channel_id: *channel_id,
        })
    }

    /// Create a fresh PublisherGroup for `channel_id` with
    /// `identity` as the sole founding member. The MLS state
    /// is persisted to `identity.provider.storage()` as a
    /// side effect of [`Group::create_with_id`]. Subsequent
    /// [`PublisherGroup::load`] calls find it.
    ///
    /// Refuses if legacy v1 state exists for this channel, for the reason given
    /// on [`PublisherGroup::load`]. `create` carries the check independently
    /// because it is reachable directly from both FFI surfaces, not only
    /// through [`PublisherGroup::open_or_create`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Group`] on any OpenMLS-side failure
    /// (e.g. ciphersuite mismatch), or when v1 state for this channel exists.
    pub fn create(identity: &Identity, channel_id: &ChannelId) -> Result<Self> {
        refuse_if_legacy_v1_present(identity, channel_id)?;
        let id = publisher_group_id_bytes(identity, channel_id);
        let inner = Group::create_with_id(identity, &id)?;
        Ok(Self {
            inner,
            channel_id: *channel_id,
        })
    }

    /// Load-or-create convenience. Tries [`PublisherGroup::load`]
    /// first; falls back to [`PublisherGroup::create`] on
    /// `Ok(None)`. This is the path every composer wants —
    /// the throwaway-group pattern collapses to a single
    /// call.
    ///
    /// # Errors
    ///
    /// Propagates failures from both inner calls.
    pub fn open_or_create(identity: &Identity, channel_id: &ChannelId) -> Result<Self> {
        if let Some(existing) = Self::load(identity, channel_id)? {
            return Ok(existing);
        }
        Self::create(identity, channel_id)
    }

    /// 32-byte channel content-key seed derived from the
    /// MLS exporter at the current group epoch. Mirror of
    /// the throwaway-group flow the composer-side
    /// `freshPublisherKeys` shipped in G-27, but reading
    /// from the persistent group's exporter so subscribers
    /// who received a sealed seed in a prior epoch continue
    /// to decrypt.
    ///
    /// The exporter is keyed on `(label, context, length)`
    /// per RFC 9420 §8.5; we pin all three at the call site:
    ///
    /// * `label` — [`CHANNEL_EXPORTER_LABEL`] (`"lakko/channel/v1"`)
    /// * `context` — the 16-byte channel id
    /// * `length` — 32 bytes
    ///
    /// # Errors
    ///
    /// Propagates [`Group::export_secret`] failures.
    pub fn channel_seed(&self, identity: &Identity) -> Result<[u8; 32]> {
        let bytes = self.inner.export_secret(
            identity,
            CHANNEL_EXPORTER_LABEL,
            self.channel_id.as_bytes(),
            32,
        )?;
        if bytes.len() != 32 {
            return Err(Error::Group(format!(
                "exporter output length {} != 32",
                bytes.len()
            )));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        Ok(out)
    }

    /// The channel id this group was minted for. Stable
    /// across `load` / `create` paths.
    #[must_use]
    pub fn channel_id(&self) -> &ChannelId {
        &self.channel_id
    }

    /// The underlying MLS `group_id` — derived from the owner's account seed
    /// and the channel id ([`publisher_group_id_for`]). ADR-0028 OQ3: a co-publisher
    /// invite-link is minted over this, and the joiner targets it when crafting
    /// an external-join proposal (ADR-0027) — the channel join-ticket works for
    /// a PublisherGroup verbatim because it is a vanilla MLS group. Stable
    /// across `load` / `create`.
    #[must_use]
    pub fn group_id(&self) -> Vec<u8> {
        self.inner.group_id()
    }

    /// The current MLS epoch of the underlying group. A prospective co-publisher
    /// reads this from the journal's public framing (it is not yet a member);
    /// this accessor is for a member that already holds the group.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.inner.epoch()
    }

    /// Rotate the channel content-key seed by advancing the
    /// MLS group epoch via a self-update commit
    /// (G4-2-mls-persistence step 4).
    ///
    /// Returns the wire-encoded commit; the publisher SHOULD
    /// broadcast it to every other PublisherGroup member so
    /// they advance to the same epoch in lock-step. The
    /// committed state is persisted as a side effect — the
    /// next [`PublisherGroup::channel_seed`] call against
    /// THIS handle, or any future
    /// [`PublisherGroup::load`] under the same channel_id,
    /// returns the new-epoch seed.
    ///
    /// ## Why "rotate"
    ///
    /// The MLS exporter is keyed on group epoch (RFC 9420
    /// §8.5). Advancing the epoch produces a fresh
    /// 32-byte exporter output for the same `(label,
    /// context, length)` triple — that is exactly what
    /// ADR-0010's "rotation on every PublisherGroup commit
    /// and on scheduled rotations" requires. The publisher-
    /// side timer (24 h / N posts) calls this method, then
    /// reads the new seed via [`PublisherGroup::channel_seed`],
    /// then runs the subscriber-batch reseal path
    /// (`lakko-channels::subscribe::seal_seed` + the
    /// services/channels batch fanout).
    ///
    /// # Errors
    ///
    /// Propagates [`Group::self_update`] failures.
    pub fn rotate(&mut self, identity: &Identity) -> Result<Vec<u8>> {
        self.inner.self_update(identity)
    }

    /// Approve a retained co-publisher external-join request (ADR-0028 OQ3),
    /// returning the Commit to broadcast + the joiner's Welcome.
    ///
    /// **This is the direct-commit approve, and this type is the only place it
    /// survives ADR-0045 S7.** A PublisherGroup is built by
    /// [`Group::create_with_id`], which deliberately seeds no ADR-0022 role
    /// table — so it has no owner, no admin set and therefore no committer to
    /// sweep a proposal. ADR-0045 leaves such a group on direct commits rather
    /// than inventing a committer for it, and the fork S7 closes is not
    /// reachable here for the same reason it is not closable: there is no
    /// designated committer to elect. The publisher fan-out already serialises
    /// on the channel owner.
    ///
    /// The **type** is the difference, and it is deliberate: this returns a
    /// [`MemberAddition`] — a commit that has already merged, with a Welcome to
    /// hand the joiner now — where [`Group::approve_external_join`] returns
    /// proposal bytes and no Welcome at all. A caller cannot confuse the two,
    /// because neither result can be used where the other is expected.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Group`] when the underlying group unexpectedly carries a
    /// role table (fail-closed: the direct route must never be available where
    /// a committer exists), when no retained request matches `proposal_ref`,
    /// when that joiner is already a member, or on any OpenMLS-side failure.
    pub fn approve_external_join(
        &mut self,
        identity: &Identity,
        proposal_ref: &[u8],
    ) -> Result<MemberAddition> {
        self.inner
            .approve_external_join_directly(identity, proposal_ref)
    }

    /// Borrow the underlying [`Group`] for the rare caller
    /// who needs the full MLS surface (membership ops,
    /// application messages between publishers, etc.).
    /// The PublisherGroup wrapper deliberately exposes only
    /// the seed-derivation shape on its own surface; the
    /// rest of MLS is reachable through this accessor when
    /// needed.
    #[must_use]
    pub fn inner(&self) -> &Group {
        &self.inner
    }

    /// Mutable borrow of the underlying [`Group`]. Same
    /// rationale as [`PublisherGroup::inner`] but for
    /// commit-producing operations (add_member,
    /// remove_member, …).
    pub fn inner_mut(&mut self) -> &mut Group {
        &mut self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-0081 amendment — a co-publisher's Welcome joins through
    /// `PublisherGroup::join_from_welcome` only: the conversation rule of
    /// `Group::join_from_welcome` refuses it (no role table), and the
    /// refusal leaves nothing, so the dedicated entry point still opens it.
    #[test]
    fn a_publisher_group_welcome_joins_only_through_its_own_entry_point() {
        let alice = Identity::generate("alice-pg-entry").unwrap();
        let bob = Identity::generate("bob-pg-entry").unwrap();
        let cid = ChannelId([0x5a; CHANNEL_ID_LEN]);
        let mut alice_pg = PublisherGroup::create(&alice, &cid).expect("create");
        let gid = alice_pg.group_id();
        let addition = alice_pg
            .inner_mut()
            .add_member(&alice, bob.create_key_package().unwrap())
            .expect("add");
        let welcome = crate::group::encode_message(&addition.welcome).expect("encode");

        let err = Group::join_from_welcome(&welcome, &bob)
            .err()
            .expect("refused as a chat");
        assert!(
            matches!(
                err,
                Error::MembershipRefused {
                    reason: crate::RefusalReason::NoRoleTable,
                    ..
                }
            ),
            "typed refusal, got {err}"
        );
        assert!(
            PublisherGroup::load_joined(&bob, &gid, &cid)
                .unwrap()
                .is_none(),
            "nothing written"
        );

        // Another id than the link conveyed: refused, nothing written.
        let wrong = vec![0x01; GROUP_ID_LEN];
        assert!(PublisherGroup::join_from_welcome(&bob, &welcome, &wrong, &cid).is_err());
        assert!(PublisherGroup::load_joined(&bob, &gid, &cid)
            .unwrap()
            .is_none());

        let bob_pg = PublisherGroup::join_from_welcome(&bob, &welcome, &gid, &cid).expect("joins");
        assert_eq!(bob_pg.group_id(), gid);
        assert_eq!(
            bob_pg.channel_seed(&bob).expect("seed"),
            alice_pg.channel_seed(&alice).expect("seed"),
        );
        assert!(PublisherGroup::load_joined(&bob, &gid, &cid)
            .unwrap()
            .is_some());
    }

    fn test_identity() -> Identity {
        Identity::generate("publisher-group-test").expect("Identity::generate")
    }

    fn test_channel_id() -> ChannelId {
        ChannelId([0xAB; CHANNEL_ID_LEN])
    }

    /// `PublisherGroup` is not `Debug`, so `expect_err` is unavailable.
    /// Renders the error of a call that MUST have failed.
    fn refusal_text<T>(res: Result<T>) -> String {
        match res {
            Ok(_) => panic!("expected a refusal, got Ok"),
            Err(e) => e.to_string(),
        }
    }

    #[test]
    fn channel_id_round_trip() {
        let bytes = [0x42u8; CHANNEL_ID_LEN];
        let cid = ChannelId::try_from_slice(&bytes).unwrap();
        assert_eq!(cid.as_bytes(), &bytes);
    }

    #[test]
    fn channel_id_rejects_wrong_length() {
        assert!(ChannelId::try_from_slice(&[0u8; 8]).is_err());
        assert!(ChannelId::try_from_slice(&[0u8; 17]).is_err());
    }

    /// The retired derivation, `b"lakko/pubgrp/v2/" || channel_id`,
    /// recomputed here only to show the new id is not it.
    fn retired_v2_group_id(channel_id: &ChannelId) -> [u8; GROUP_ID_LEN] {
        let mut id = [0u8; GROUP_ID_LEN];
        id[..16].copy_from_slice(b"lakko/pubgrp/v2/");
        id[16..].copy_from_slice(channel_id.as_bytes());
        id
    }

    #[test]
    fn group_id_is_deterministic_and_golden() {
        let cid = ChannelId([0x01; CHANNEL_ID_LEN]);
        let seed = [0x42u8; 32];
        assert_eq!(
            publisher_group_id_for(&seed, &cid),
            publisher_group_id_for(&seed, &cid)
        );
        let mut material = [0u8; 48];
        material[..32].copy_from_slice(&seed);
        material[32..].copy_from_slice(cid.as_bytes());
        assert_eq!(
            publisher_group_id_for(&seed, &cid),
            lakko_crypto::hash::blake3_derive_key_from_material("lakko/pubgrp/id/v3", &material)
        );
    }

    #[test]
    fn distinct_channels_get_distinct_group_ids() {
        let seed = [0x42u8; 32];
        let cid_a = ChannelId([0x01; CHANNEL_ID_LEN]);
        let cid_b = ChannelId([0x02; CHANNEL_ID_LEN]);
        assert_ne!(
            publisher_group_id_for(&seed, &cid_a),
            publisher_group_id_for(&seed, &cid_b)
        );
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(256))]

        /// ADR-0078 phase 6: the id is not computable from the channel id
        /// alone. One channel id under two owner seeds gives two ids (so no
        /// function of the channel id yields it), the retired
        /// `prefix || channel_id` id is never produced, and the channel id is
        /// not readable from the id.
        #[test]
        fn group_id_is_not_computable_from_the_channel_id(
            channel in proptest::prelude::any::<[u8; CHANNEL_ID_LEN]>(),
            seed_a in proptest::prelude::any::<[u8; 32]>(),
            seed_b in proptest::prelude::any::<[u8; 32]>(),
        ) {
            let cid = ChannelId(channel);
            let id_a = publisher_group_id_for(&seed_a, &cid);
            let id_b = publisher_group_id_for(&seed_b, &cid);
            if seed_a == seed_b {
                proptest::prop_assert_eq!(id_a, id_b);
            } else {
                proptest::prop_assert_ne!(id_a, id_b);
            }
            proptest::prop_assert_ne!(id_a, retired_v2_group_id(&cid));
            proptest::prop_assert!(!id_a.windows(CHANNEL_ID_LEN).any(|w| w == channel));
        }

        /// The owner's identity recomputes the same id on every open.
        #[test]
        fn group_id_is_a_pure_function_of_seed_and_channel(
            channel in proptest::prelude::any::<[u8; CHANNEL_ID_LEN]>(),
            seed in proptest::prelude::any::<[u8; 32]>(),
        ) {
            let cid = ChannelId(channel);
            proptest::prop_assert_eq!(
                publisher_group_id_for(&seed, &cid),
                publisher_group_id_for(&seed, &cid)
            );
        }
    }

    #[test]
    fn create_uses_the_owner_seed_derived_id() {
        let identity = test_identity();
        let cid = test_channel_id();
        let pg = PublisherGroup::create(&identity, &cid).expect("create");
        assert_eq!(
            pg.group_id(),
            publisher_group_id_for(&identity.ed25519_seed_bytes(), &cid).to_vec()
        );
        assert_ne!(pg.group_id(), retired_v2_group_id(&cid).to_vec());
    }

    #[test]
    fn create_then_load_round_trips() {
        let identity = test_identity();
        let cid = test_channel_id();

        // Initially nothing under this id.
        assert!(PublisherGroup::load(&identity, &cid).unwrap().is_none());

        // Create persists the group.
        let _created = PublisherGroup::create(&identity, &cid).expect("create");

        // Load now finds it.
        let loaded = PublisherGroup::load(&identity, &cid)
            .expect("load")
            .expect("Some");
        assert_eq!(loaded.channel_id(), &cid);
    }

    #[test]
    fn open_or_create_is_idempotent() {
        let identity = test_identity();
        let cid = test_channel_id();

        // First call mints. Capture the exporter output.
        let seed_first = {
            let pg = PublisherGroup::open_or_create(&identity, &cid).expect("first open_or_create");
            pg.channel_seed(&identity).expect("seed first")
        };

        // Second call MUST resolve to the same persisted
        // group, NOT mint a fresh one — otherwise we are
        // back to the throwaway-group bug G4-2-mls-
        // persistence closes. The exporter output proves it:
        // a freshly-minted MLS group has different ratchet
        // state and would emit a different seed.
        let seed_second = {
            let pg =
                PublisherGroup::open_or_create(&identity, &cid).expect("second open_or_create");
            pg.channel_seed(&identity).expect("seed second")
        };
        assert_eq!(seed_first, seed_second);
    }

    #[test]
    fn distinct_channels_get_distinct_seeds() {
        let identity = test_identity();
        let cid_a = ChannelId([0xAA; CHANNEL_ID_LEN]);
        let cid_b = ChannelId([0xBB; CHANNEL_ID_LEN]);

        let pg_a = PublisherGroup::open_or_create(&identity, &cid_a).expect("create A");
        let pg_b = PublisherGroup::open_or_create(&identity, &cid_b).expect("create B");

        let seed_a = pg_a.channel_seed(&identity).expect("seed A");
        let seed_b = pg_b.channel_seed(&identity).expect("seed B");

        // Two channels under the same identity — different
        // PublisherGroups, different ratchet state, different
        // exporter outputs. Cross-channel-decrypt confusion
        // is forbidden by ADR-0010's two-tier isolation.
        assert_ne!(seed_a, seed_b);
    }

    #[test]
    fn rotate_advances_epoch_and_changes_seed() {
        // G4-2-mls-persistence step 4 — load-bearing test.
        // Before rotation, two consecutive `channel_seed`
        // calls return the same bytes (step 1's idempotence
        // invariant). After `rotate()` the MLS epoch advances
        // by one and the exporter output MUST differ, otherwise
        // the scheduled-rotation primitive ADR-0010 §"Rotation"
        // requires has no semantic meaning.
        let identity = test_identity();
        let cid = test_channel_id();

        let mut pg = PublisherGroup::open_or_create(&identity, &cid).expect("create");
        let seed_pre = pg.channel_seed(&identity).expect("seed pre");

        let epoch_pre = pg.inner().epoch();
        let _commit_bytes = pg.rotate(&identity).expect("rotate");
        let epoch_post = pg.inner().epoch();
        assert_eq!(epoch_post, epoch_pre + 1, "epoch must advance by one");

        let seed_post = pg.channel_seed(&identity).expect("seed post");
        assert_ne!(seed_pre, seed_post, "rotation must change the seed");
    }

    #[test]
    fn rotated_seed_persists_across_reload() {
        // After `rotate()` + drop, a fresh `load` must surface
        // the post-rotation epoch's exporter output — i.e. the
        // rotation is durable, not just held in the live
        // handle's memory. Without this invariant the
        // scheduled-rotation timer would reset to epoch 0 on
        // every app restart and rotation would be meaningless.
        let identity = test_identity();
        let cid = test_channel_id();

        let post_rotation_seed = {
            let mut pg = PublisherGroup::open_or_create(&identity, &cid).expect("create");
            let _commit = pg.rotate(&identity).expect("rotate");
            pg.channel_seed(&identity).expect("seed post")
        };

        let reloaded = PublisherGroup::load(&identity, &cid)
            .expect("load")
            .expect("Some");
        let reloaded_seed = reloaded.channel_seed(&identity).expect("seed reload");
        assert_eq!(post_rotation_seed, reloaded_seed);
    }

    /// C-88 at this entry point, which is narrower than a conversation's: an
    /// active PublisherGroup is refused, typed, and so is an ENDED one, which a
    /// Welcome does not replace here. A co-publisher's re-invitation has no
    /// path through this provider, and this change does not add one.
    #[test]
    fn a_welcome_replaces_no_publisher_group() {
        let alice = Identity::generate("alice").unwrap();
        let bob = Identity::generate("bob").unwrap();
        let cid = ChannelId([0x5b; CHANNEL_ID_LEN]);
        let mut alice_pg = PublisherGroup::create(&alice, &cid).expect("create");
        let gid = alice_pg.group_id();
        let addition = alice_pg
            .inner_mut()
            .add_member(&alice, bob.create_key_package().unwrap())
            .expect("add");
        let welcome = crate::group::encode_message(&addition.welcome).expect("encode");
        let mut bob_pg =
            PublisherGroup::join_from_welcome(&bob, &welcome, &gid, &cid).expect("joins");

        let err = PublisherGroup::join_from_welcome(&bob, &welcome, &gid, &cid)
            .err()
            .expect("an active group is not replaced");
        assert_eq!(
            err.refusal_reason(),
            Some(crate::RefusalReason::GroupStillActive)
        );

        let bob_leaf = alice_pg
            .inner_mut()
            .members()
            .iter()
            .find(|m| m.signature_public_key == bob.signature_public_key())
            .expect("bob is in")
            .leaf_index;
        let removal = alice_pg
            .inner_mut()
            .remove_member(&alice, crate::LeafNodeIndex::new(bob_leaf))
            .expect("remove");
        let removal = crate::group::encode_message(&removal).expect("encode");
        bob_pg
            .inner_mut()
            .process_incoming(&bob, &removal)
            .expect("bob applies");
        assert!(!bob_pg.inner_mut().is_active());

        let addition = alice_pg
            .inner_mut()
            .add_member(&alice, bob.create_key_package().unwrap())
            .expect("add again");
        let welcome = crate::group::encode_message(&addition.welcome).expect("encode");
        let err = PublisherGroup::join_from_welcome(&bob, &welcome, &gid, &cid)
            .err()
            .expect("an ended group is not replaced here");
        assert_eq!(err.refusal_reason(), None, "the plain refusal, got {err}");
        assert!(!PublisherGroup::load_joined(&bob, &gid, &cid)
            .unwrap()
            .expect("the ended group is still stored")
            .inner_mut()
            .is_active());
    }

    #[test]
    fn rotate_emits_commit_bytes_processable_by_co_publisher() {
        // G4-2-mls-persistence step 6 — cross-publisher
        // membership propagation. Alice mints the
        // PublisherGroup, adds Bob via the underlying Group's
        // standard membership ops (the wrapper exposes
        // inner_mut() for this), Bob joins from the welcome,
        // then Alice rotates and Bob processes the commit.
        // Both end up at the same epoch with the same seed —
        // the load-bearing invariant for a multi-publisher
        // channel where any member can issue rotations and
        // the others stay in sync via MLS commit propagation.
        use crate::group::Incoming;
        use openmls_traits::OpenMlsProvider;

        let alice = Identity::generate("alice-channel").unwrap();
        let bob = Identity::generate("bob-channel").unwrap();
        let cid = ChannelId([0x77; CHANNEL_ID_LEN]);

        // Alice mints the PublisherGroup, then adds Bob via a
        // standard MLS Add.
        let mut alice_pg = PublisherGroup::create(&alice, &cid).expect("alice create");
        let bob_kp = bob.create_key_package().expect("bob kp");
        let addition = alice_pg
            .inner_mut()
            .add_member(&alice, bob_kp)
            .expect("alice adds bob");

        // Bob joins through the PublisherGroup entry point (ADR-0081
        // amendment): the conversation rule of `Group::join_from_welcome`
        // refuses a Welcome with no role table.
        let welcome_bytes =
            crate::group::encode_message(&addition.welcome).expect("welcome encode");
        let alice_gid = alice_pg.group_id();
        let bob_group = PublisherGroup::join_from_welcome(&bob, &welcome_bytes, &alice_gid, &cid)
            .expect("bob join")
            .inner;

        // Alice rotates AFTER adding Bob. Both sides advance.
        // (Note: Alice's `add_member` already merged a commit
        // and advanced her epoch from 0 -> 1; the rotation
        // bumps to 2.)
        let alice_epoch_pre_rotate = alice_pg.inner().epoch();
        let commit_bytes = alice_pg.rotate(&alice).expect("rotate");
        assert_eq!(alice_pg.inner().epoch(), alice_epoch_pre_rotate + 1);

        // Bob processes the rotation commit.
        let mut bob_group_mut = bob_group;
        let processed = bob_group_mut
            .process_incoming(&bob, &commit_bytes)
            .expect("bob processes rotate");
        assert!(matches!(processed, Incoming::Commit));

        // Both ends now at the same epoch. Exporter outputs
        // computed against (label, channel_id, 32) must
        // match — that is the cross-publisher seed-sync
        // invariant ADR-0010 §"PublisherGroup" requires.
        assert_eq!(alice_pg.inner().epoch(), bob_group_mut.epoch());

        let alice_seed = alice_pg.channel_seed(&alice).expect("alice seed");
        let bob_seed = bob_group_mut
            .export_secret(&bob, CHANNEL_EXPORTER_LABEL, cid.as_bytes(), 32)
            .expect("bob exporter");
        assert_eq!(alice_seed, bob_seed.as_slice());

        // Silence the unused-import warning the
        // `openmls_traits::OpenMlsProvider` brings in via
        // identity creation transitively — we keep it for
        // documentation of which trait the providers
        // implement, but don't reference it directly.
        #[allow(clippy::items_after_statements)]
        fn _silence(_: &impl OpenMlsProvider) {}
    }

    #[test]
    fn distinct_identities_get_distinct_seeds_for_same_channel() {
        // Two different publishers minting their own
        // PublisherGroup for "the same" channel id end up
        // with distinct ratchet state. (In production they
        // would commit-merge through MLS membership ops;
        // this test just pins that the per-identity
        // exporter outputs are not coincidentally identical.)
        let identity_a = test_identity();
        let identity_b = test_identity();
        let cid = test_channel_id();

        let pg_a = PublisherGroup::open_or_create(&identity_a, &cid).expect("a");
        let pg_b = PublisherGroup::open_or_create(&identity_b, &cid).expect("b");

        let seed_a = pg_a.channel_seed(&identity_a).expect("seed a");
        let seed_b = pg_b.channel_seed(&identity_b).expect("seed b");
        assert_ne!(seed_a, seed_b);
    }

    #[test]
    fn co_publisher_joins_via_external_join_ticket_and_shares_channel_seed() {
        // ADR-0028 OQ3 — adding a *publisher* to a channel reuses the group
        // join-ticket + external-join machinery (ADR-0027/0028) VERBATIM,
        // because a PublisherGroup is a vanilla MLS group. Owner Alice mints the
        // channel; non-member Bob crafts an external-join proposal over the
        // PublisherGroup's `group_id` (all the ticket conveys); Alice approves;
        // Bob joins from the Welcome and reconstructs the PublisherGroup under
        // the deterministic id — and BOTH derive the same channel content-key
        // seed at the shared post-add epoch, so Bob can now publish.
        use crate::group::{encode_message, Incoming};

        let alice = Identity::from_seed(&[0xA7u8; 64]).expect("alice");
        let bob = Identity::from_seed(&[0xB7u8; 64]).expect("bob");
        let cid = ChannelId([0x5C; CHANNEL_ID_LEN]);

        let mut alice_pg = PublisherGroup::create(&alice, &cid).expect("alice publisher group");
        let group_id = alice_pg.group_id();
        let epoch = alice_pg.epoch();

        // Bob (a non-member) crafts the external-join proposal for the
        // PublisherGroup's id + epoch — exactly what the `/g/` ticket conveys.
        let proposal = bob
            .create_external_join_proposal(&group_id, epoch)
            .expect("bob external-join proposal");

        // Alice processes it through the inner Group surface; the proposal is
        // retained, and Alice (a member of the open group) approves it.
        let incoming = alice_pg
            .inner_mut()
            .process_incoming(&alice, &proposal)
            .expect("process proposal");
        let bob_joiner_key = match incoming {
            Incoming::ExternalJoinProposal {
                joiner_signature_key,
                ..
            } => joiner_signature_key,
            other => panic!("expected ExternalJoinProposal, got {other:?}"),
        };
        let addition = alice_pg
            .approve_external_join(&alice, &bob_joiner_key)
            .expect("approve");
        let welcome_bytes = encode_message(&addition.welcome).expect("encode welcome");

        // Bob joins from the Welcome. He cannot derive the owner's id (it
        // needs Alice's seed); he opens the group under the id the ticket
        // conveyed, with the channel id the link's sealed preview carried.
        let _bob_group = PublisherGroup::join_from_welcome(&bob, &welcome_bytes, &group_id, &cid)
            .expect("bob joins");
        assert!(
            PublisherGroup::load(&bob, &cid)
                .expect("own load")
                .is_none(),
            "a co-publisher's own derivation names another group"
        );
        let bob_pg = PublisherGroup::load_joined(&bob, &group_id, &cid)
            .expect("bob publisher load")
            .expect("bob publisher group present");

        // The payoff: both publishers derive the identical channel content-key
        // seed at the shared post-add epoch — Bob can now post to the channel.
        let alice_seed = alice_pg.channel_seed(&alice).expect("alice seed");
        let bob_seed = bob_pg.channel_seed(&bob).expect("bob seed");
        assert_eq!(
            alice_seed, bob_seed,
            "a ticket-joined co-publisher must share the channel seed"
        );
    }

    #[test]
    fn load_joined_refuses_a_wrong_width_id_and_finds_nothing_unknown() {
        let identity = test_identity();
        let cid = test_channel_id();
        assert!(PublisherGroup::load_joined(&identity, &[0u8; 31], &cid).is_err());
        assert!(PublisherGroup::load_joined(&identity, &[0u8; 32], &cid)
            .expect("load")
            .is_none());
    }

    /// Legacy v1 state must produce a loud refusal, never a silent fresh mint.
    ///
    /// Constructed the only way it can be: mint a group directly under the v1
    /// id via the generic `Group::create_with_id`, which is exactly what the
    /// retired `PublisherGroup::create` did.
    #[test]
    fn legacy_v1_state_refuses_instead_of_minting_a_fresh_group() {
        let identity = test_identity();
        let cid = ChannelId([0x3Cu8; CHANNEL_ID_LEN]);

        // Nothing persisted yet: both constructors are clear to run.
        assert!(PublisherGroup::load(&identity, &cid)
            .expect("clean load")
            .is_none());

        // Simulate a channel that was minted before the v2 width fix.
        let _legacy =
            Group::create_with_id(&identity, &publisher_group_id_bytes_v1(&cid)).expect("v1 group");

        let load_err = refusal_text(PublisherGroup::load(&identity, &cid));
        assert!(
            load_err.contains("legacy v1 PublisherGroup state"),
            "unexpected error: {load_err}"
        );

        // open_or_create must NOT fall through to a fresh mint. Silently
        // minting would swap the channel's content-key seed under every
        // subscriber that still holds an envelope for the old one.
        assert!(
            refusal_text(PublisherGroup::open_or_create(&identity, &cid))
                .contains("legacy v1 PublisherGroup state")
        );

        // And the direct constructor, reachable from both FFI surfaces.
        assert!(refusal_text(PublisherGroup::create(&identity, &cid))
            .contains("legacy v1 PublisherGroup state"));

        // The v1 state is left intact for a future migration tool.
        assert!(
            Group::load(&identity, &publisher_group_id_bytes_v1(&cid))
                .expect("v1 load")
                .is_some(),
            "the refusal must not destroy the legacy state"
        );
    }

    /// The legacy probe must be scoped to the SAME channel — an unrelated
    /// channel's v1 state must not block a fresh v2 mint.
    #[test]
    fn legacy_probe_is_per_channel() {
        let identity = test_identity();
        let legacy_cid = ChannelId([0x11u8; CHANNEL_ID_LEN]);
        let fresh_cid = ChannelId([0x22u8; CHANNEL_ID_LEN]);

        let _legacy = Group::create_with_id(&identity, &publisher_group_id_bytes_v1(&legacy_cid))
            .expect("v1 group");

        let pg = PublisherGroup::open_or_create(&identity, &fresh_cid).expect("unrelated channel");
        assert_eq!(pg.group_id().len(), GROUP_ID_LEN);
        assert!(PublisherGroup::create(&identity, &legacy_cid).is_err());
    }
}
