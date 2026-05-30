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
//! * Derives a **deterministic** MLS group id from the
//!   16-byte channel id (`prefix("lakko/channel/v1") || channel_id`).
//!   That gives [`Group::load`] something stable to look up.
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
//! * **The 24 h / N-message scheduled rotation.** That lives
//!   in a publisher-side timer (deferred — see ROADMAP).
//!   This module exposes a fresh seed on demand; the timer
//!   that calls "commit + rotate" sits above it.
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
use crate::group::Group;
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

/// Domain-separation prefix for the deterministic
/// PublisherGroup MLS group id. Pinned at v1 — a future
/// schema bump would land as `"lakko/publisher-group/v2"`
/// and force a parallel migration.
const PUBLISHER_GROUP_ID_PREFIX: &[u8] = b"lakko/publisher-group/v1/";

/// Build the deterministic MLS group id for `channel_id`.
/// Shape: `b"lakko/publisher-group/v1/" || channel_id`.
///
/// The prefix domain-separates this byte string from any
/// other channel-id-derived value (the channel-exporter
/// label, the subscriber-envelope AAD, etc.) so a future
/// confusion attack cannot reinterpret bytes minted in one
/// role as a valid input to another.
fn publisher_group_id_bytes(channel_id: &ChannelId) -> Vec<u8> {
    let mut id = Vec::with_capacity(PUBLISHER_GROUP_ID_PREFIX.len() + CHANNEL_ID_LEN);
    id.extend_from_slice(PUBLISHER_GROUP_ID_PREFIX);
    id.extend_from_slice(channel_id.as_bytes());
    id
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
    /// # Errors
    ///
    /// Propagates [`Group::load`] errors.
    pub fn load(identity: &Identity, channel_id: &ChannelId) -> Result<Option<Self>> {
        let id = publisher_group_id_bytes(channel_id);
        match Group::load(identity, &id)? {
            Some(inner) => Ok(Some(Self {
                inner,
                channel_id: *channel_id,
            })),
            None => Ok(None),
        }
    }

    /// Create a fresh PublisherGroup for `channel_id` with
    /// `identity` as the sole founding member. The MLS state
    /// is persisted to `identity.provider.storage()` as a
    /// side effect of [`Group::create_with_id`]. Subsequent
    /// [`PublisherGroup::load`] calls find it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Group`] on any OpenMLS-side failure
    /// (e.g. ciphersuite mismatch).
    pub fn create(identity: &Identity, channel_id: &ChannelId) -> Result<Self> {
        let id = publisher_group_id_bytes(channel_id);
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

    fn test_identity() -> Identity {
        Identity::generate("publisher-group-test").expect("Identity::generate")
    }

    fn test_channel_id() -> ChannelId {
        ChannelId([0xAB; CHANNEL_ID_LEN])
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

    #[test]
    fn group_id_bytes_are_deterministic() {
        let cid = ChannelId([0x01; CHANNEL_ID_LEN]);
        let id1 = publisher_group_id_bytes(&cid);
        let id2 = publisher_group_id_bytes(&cid);
        assert_eq!(id1, id2);
        assert!(id1.starts_with(PUBLISHER_GROUP_ID_PREFIX));
        assert_eq!(id1.len(), PUBLISHER_GROUP_ID_PREFIX.len() + CHANNEL_ID_LEN);
    }

    #[test]
    fn distinct_channels_get_distinct_group_ids() {
        let cid_a = ChannelId([0x01; CHANNEL_ID_LEN]);
        let cid_b = ChannelId([0x02; CHANNEL_ID_LEN]);
        assert_ne!(
            publisher_group_id_bytes(&cid_a),
            publisher_group_id_bytes(&cid_b)
        );
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

        // Encode the welcome to bytes, give Bob a fresh
        // Group::join_from_welcome handle. Bob's PublisherGroup
        // is constructed by hand from that handle since we
        // don't expose a "join PublisherGroup from welcome"
        // shortcut yet (would be the natural next API; for
        // step 6 the test goes through Group::from_mls_group
        // analogue isn't accessible publicly, so we use the
        // standard Group::join_from_welcome + a custom assemble).
        let welcome_bytes =
            crate::group::encode_message(&addition.welcome).expect("welcome encode");
        let bob_group = crate::Group::join_from_welcome(&welcome_bytes, &bob).expect("bob join");

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
}
