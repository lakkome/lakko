// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Bringing a device that fell behind back into a conversation ([ADR-0091]
//! phase 2): the re-admitter's side (check a request, propose the
//! replacement) and the returning device's side ([`Group::join_reentry`],
//! M3).
//!
//! The rules the commit must pass on every peer are in [`crate::reentry`];
//! this module only drives them.
//!
//! [ADR-0091]: ../../../docs/adr/0091-conversation-rejoin.md

use openmls::ciphersuite::hash_ref::KeyPackageRef;
use openmls::framing::MlsMessageBodyIn;
use openmls::group::{MlsGroup, StagedWelcome};
use openmls::prelude::{KeyPackage, LeafNodeIndex, ProtocolVersion};
use openmls::treesync::EncryptionKey;
use openmls_traits::OpenMlsProvider as _;

use crate::error::{Error, Result};
use crate::group::{
    admit_welcome, decode_message, delete_stored_group, welcome_join_config, Group,
    WelcomeAdmission,
};
use crate::group_kind::KindState;
use crate::group_proposals::ProposalKind;
use crate::identity::Identity;
use crate::reentry::{marker_of_key_package, reentry_add_of_key_package, ReentryAdd, ReentryScope};
use crate::refusal::RefusalReason;

/// What a re-admitter learned from a request's KeyPackage: the leaf it
/// replaces and whose it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReentryTarget {
    /// The current leaf the KeyPackage replaces.
    pub replaced_leaf: u32,
    /// The leaf's (and the KeyPackage's) Ed25519 signature key.
    pub signature_key: Vec<u8>,
    /// The account the leaf belongs to (ADR-0050).
    pub account_key: Vec<u8>,
    /// The epoch the marker names, which is this group's current epoch.
    pub epoch_left: u64,
}

/// The two proposals of a re-entry replacement, wire-encoded, in the order
/// they must be published: the `Add` first, so every peer holds it when the
/// `Remove` that stands on it arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReentryProposals {
    /// The `Add(K)` proposal.
    pub add: Vec<u8>,
    /// The `Remove(L)` proposal.
    pub remove: Vec<u8>,
}

fn refused(detail: &str) -> Error {
    Error::refused(RefusalReason::ReentryPairInvalid, detail)
}

fn welcome_refused(detail: &str) -> Error {
    Error::refused(RefusalReason::ReentryWelcomeRefused, detail)
}

fn validate(identity: &Identity, bytes: &[u8]) -> Result<KeyPackage> {
    crate::key_package::deserialize(bytes)?
        .validate(identity.provider.crypto(), ProtocolVersion::Mls10)
        .map_err(|e| Error::KeyPackage(format!("validate: {e:?}")))
}

/// Decode and stage a Welcome over whatever is stored (writing nothing) and
/// run the ordinary conversation admission on it (role table, readable kind,
/// every member's device binding; C-86). Returns the staged Welcome and the
/// `KeyPackageRef`s its secrets are addressed to.
fn stage_conversation_welcome(
    welcome_bytes: &[u8],
    identity: &Identity,
) -> Result<(StagedWelcome, Vec<KeyPackageRef>)> {
    let MlsMessageBodyIn::Welcome(welcome) = decode_message(welcome_bytes)?.extract() else {
        return Err(Error::Unsupported("expected Welcome message"));
    };
    let welcomed: Vec<KeyPackageRef> = welcome
        .secrets()
        .iter()
        .map(openmls::messages::EncryptedGroupSecrets::new_member)
        .collect();
    let staged =
        StagedWelcome::build_from_welcome(&identity.provider, &welcome_join_config(), welcome)
            .map_err(|e| Error::Group(format!("staged welcome: {e:?}")))?
            .replace_old_group()
            .build()
            .map_err(|e| Error::Group(format!("staged welcome: {e:?}")))?;

    admit_welcome(
        staged.group_context().extensions(),
        WelcomeAdmission::Conversation,
    )?;
    let unbound = staged.members().any(|m| {
        crate::device_binding::account_key_of_credential(
            m.credential.serialized_content(),
            &m.signature_key,
        )
        .is_none()
    });
    if unbound {
        return Err(Error::refused(
            RefusalReason::DeviceBindingInvalid,
            "welcome refused: a member's device binding does not verify",
        ));
    }

    Ok((staged, welcomed))
}

/// `true` when `account` is the account of a leaf of `stored`.
fn account_in_roster(stored: &MlsGroup, account: [u8; 32]) -> bool {
    stored.members().any(|m| {
        crate::device_binding::account_key_of_credential(
            m.credential.serialized_content(),
            &m.signature_key,
        ) == Some(account)
    })
}

/// `true` when a re-entry Welcome from `sender` may replace `stored`
/// (ADR-0091 section 4, security review 2026-11-21): the sender is in the
/// stored roster AND is a device of this device's own account (route 1, or
/// an ADR-0050 self-service sweep), or the owner or an admin of the STORED
/// role table (route 3, the acting committer), or, in a one-to-one, the other
/// person (route 2: a one-to-one's two people are fixed, and its other
/// person is the only re-admitter it has). A plain member of a group cannot
/// bring a device back, so its Welcome cannot put this device on a tree it
/// chose.
///
/// `own` is this device's account; `sender` the account the sender's leaf
/// resolves to. Also the rule for who may sign a gap history
/// ([`Group::gap_history_signer_accepted`]), judged there on the joined group.
fn sender_may_readmit(stored: &MlsGroup, own: Option<[u8; 32]>, sender: Option<[u8; 32]>) -> bool {
    let Some(sender) = sender else {
        return false;
    };
    if !account_in_roster(stored, sender) {
        return false;
    }
    if own == Some(sender) {
        return true;
    }
    let extensions = stored.extensions();
    if KindState::of(extensions).kind() == Some(crate::group_kind::GroupKind::OneToOne) {
        return true;
    }
    extensions
        .unknown(crate::group_roles::ROLE_EXTENSION_TYPE)
        .and_then(|body| crate::group_roles::RoleTable::decode(&body.0).ok())
        .is_some_and(|table| table.can_administer(&sender))
}

/// What the staged Welcome says this device joined as.
struct JoinedAs<'a> {
    group_id: &'a [u8],
    welcome_epoch: u64,
    own_key: &'a [u8],
    new_leaf_key: &'a EncryptionKey,
    welcomed: &'a [KeyPackageRef],
}

impl JoinedAs<'_> {
    /// `true` when the recorded wire KeyPackage `bytes` is the one this
    /// Welcome added: its marker names this group and the epoch the commit
    /// left, it is this device's, its leaf is the new leaf, and the Welcome's
    /// secrets name it.
    fn is_recorded(&self, identity: &Identity, bytes: &[u8]) -> bool {
        let Ok(kp) = validate(identity, bytes) else {
            return false;
        };
        let Some(Ok(marker)) = marker_of_key_package(&kp) else {
            return false;
        };
        let Ok(kp_ref) = kp.hash_ref(identity.provider.crypto()) else {
            return false;
        };
        marker.group_id.as_slice() == self.group_id
            && marker.epoch_left.checked_add(1) == Some(self.welcome_epoch)
            && kp.leaf_node().signature_key().as_slice() == self.own_key
            && kp.leaf_node().encryption_key() == self.new_leaf_key
            && self.welcomed.contains(&kp_ref)
    }
}

impl Group {
    /// Check a re-entry request's KeyPackage against this group (ADR-0091
    /// section 4, the part the core can see): it is a valid KeyPackage (its
    /// signature verifies), its marker names this group and this group's
    /// current epoch, a current leaf has its signature key, and its device
    /// binding resolves to that leaf's account. The rest of the decision
    /// (the device is still the account's, the leaf is not writing, the
    /// per-leaf limits) is [`crate::reentry_admission_verdict`].
    ///
    /// `identity` supplies the crypto provider.
    ///
    /// # Errors
    ///
    /// [`Error::KeyPackage`] when the bytes are not a valid KeyPackage;
    /// [`RefusalReason::ReentryPairInvalid`] when it is not a re-entry for
    /// this group at this epoch.
    pub fn reentry_request_target(
        &self,
        identity: &Identity,
        key_package_bytes: &[u8],
    ) -> Result<ReentryTarget> {
        let kp = validate(identity, key_package_bytes)?;
        let Some(Ok(marker)) = marker_of_key_package(&kp) else {
            return Err(refused(
                "re-entry refused: the KeyPackage carries no readable re-entry marker",
            ));
        };
        let members = self.members();
        let group_id = self.group_id();
        let scope = ReentryScope {
            group_id: &group_id,
            epoch: self.epoch(),
            members: &members,
        };
        let ReentryAdd::Valid { replaced_leaf } = reentry_add_of_key_package(scope, &kp) else {
            return Err(refused(
                "re-entry refused: the KeyPackage is not a re-entry for this group, this epoch \
                 and one of its current members",
            ));
        };
        let member = members
            .iter()
            .find(|m| m.leaf_index == replaced_leaf)
            .ok_or_else(|| refused("re-entry refused: the replaced leaf is gone"))?;
        Ok(ReentryTarget {
            replaced_leaf,
            signature_key: member.signature_public_key.clone(),
            account_key: member.account_key.clone(),
            epoch_left: marker.epoch_left,
        })
    }

    /// Propose the replacement a re-entry request asks for: `Add(K)` and
    /// then `Remove(L)`, both from this device (ADR-0091 section 4). The
    /// commit comes from whichever device sweeps (this one, the account's,
    /// or the acting committer), through the ADR-0081 commit route.
    ///
    /// Everything is checked before either proposal is made, so a refusal
    /// leaves the pending store as it was: the request
    /// ([`Self::reentry_request_target`]), room for two proposals, and the
    /// removal standing either on the ordinary rules or on the pair rule.
    ///
    /// # Errors
    ///
    /// As [`Self::reentry_request_target`]; [`RefusalReason::NoRoleTable`]
    /// for a group without a role table; [`RefusalReason::PendingSetFull`];
    /// the ordinary refusal of the removal when the pair rule does not lift
    /// it; and OpenMLS failures.
    pub fn propose_reentry_replacement(
        &mut self,
        identity: &Identity,
        key_package_bytes: &[u8],
    ) -> Result<ReentryProposals> {
        let target = self.reentry_request_target(identity, key_package_bytes)?;
        self.role_table_or_refuse("re-entry")?;
        let capacity = super::pending_capacity(false);
        if self.inner.pending_proposals().count() + 2 > capacity {
            return Err(Error::refused(
                RefusalReason::PendingSetFull,
                format!(
                    "re-entry refused: this group already holds close to {capacity} pending \
                     proposals — a committer must sweep them first"
                ),
            ));
        }
        let remove_kind = ProposalKind::Remove {
            target_leaf: target.replaced_leaf,
        };
        if let Err(e) = self.authorise_own_proposal(identity, &remove_kind) {
            if !e
                .refusal_reason()
                .is_some_and(crate::reentry::refusal_lifted_by_pair)
            {
                return Err(e);
            }
        }
        let kp = validate(identity, key_package_bytes)?;
        let add = self.propose_add_member(identity, kp)?;
        let remove =
            self.propose_remove_member(identity, LeafNodeIndex::new(target.replaced_leaf))?;
        Ok(ReentryProposals { add, remove })
    }

    /// Join a conversation over the stale active group this device holds,
    /// from the Welcome of the commit that replaced its leaf (ADR-0091
    /// section 5, M3).
    ///
    /// [`Group::join_from_welcome`] refuses an active stored group (C-88) and
    /// keeps doing so. This replaces it only when ALL hold, checked on the
    /// staged Welcome before anything is written:
    ///
    /// * the Welcome passes the ordinary conversation admission (role table,
    ///   readable kind, every member's device binding verifies; C-86);
    /// * an active group with the Welcome's id is stored, its kind equals the
    ///   Welcome's, and the Welcome's epoch is above it;
    /// * the Welcome's sender resolves (ADR-0050) to an account in the stored
    ///   roster that is this device's own account, the owner or an admin of
    ///   the stored role table, or, in a one-to-one, the other person (a
    ///   plain member of a group is refused; security review 2026-11-21);
    /// * the stored own leaf and exactly one leaf of the new tree carry this
    ///   identity's signature key;
    /// * one of `recorded_key_packages` (the wire KeyPackages this device
    ///   recorded durably before sending its requests) is that new leaf: its
    ///   marker names this group, the Welcome's epoch is one above the epoch
    ///   it names, the Welcome's secrets name its `KeyPackageRef`, and its
    ///   leaf's encryption key is the new leaf's.
    ///
    /// Otherwise it refuses and changes nothing; the KeyPackages stay usable
    /// (they are last-resort).
    ///
    /// # Errors
    ///
    /// [`Error::Codec`] / [`Error::Unsupported`] / [`Error::Group`] for bytes
    /// that are not a Welcome for this identity; the admission refusals of
    /// [`Group::join_from_welcome`]; [`RefusalReason::ReentryWelcomeRefused`]
    /// for any re-entry condition above.
    pub fn join_reentry(
        welcome_bytes: &[u8],
        identity: &Identity,
        recorded_key_packages: &[Vec<u8>],
    ) -> Result<Self> {
        let (staged, welcomed) = stage_conversation_welcome(welcome_bytes, identity)?;

        let group_id = staged.group_context().group_id().as_slice().to_vec();
        let welcome_epoch = staged.group_context().epoch().as_u64();
        let stored = MlsGroup::load(
            identity.provider.storage(),
            staged.group_context().group_id(),
        )
        .map_err(|e| Error::Group(format!("load stored group: {e:?}")))?;
        let Some(mut stored) = stored.filter(MlsGroup::is_active) else {
            return Err(welcome_refused(
                "re-entry refused: no active group with that id is stored",
            ));
        };
        if welcome_epoch <= stored.epoch().as_u64() {
            return Err(welcome_refused(
                "re-entry refused: the welcome is not newer than the stored group",
            ));
        }
        if KindState::of(staged.group_context().extensions()).kind()
            != KindState::of(stored.extensions()).kind()
        {
            return Err(welcome_refused(
                "re-entry refused: the welcome is for another kind of conversation",
            ));
        }

        let sender = staged
            .welcome_sender()
            .map_err(|e| Error::Group(format!("welcome sender: {e:?}")))?;
        let own_account = stored.own_leaf_node().and_then(|leaf| {
            crate::device_binding::account_key_of_credential(
                leaf.credential().serialized_content(),
                leaf.signature_key().as_slice(),
            )
        });
        if !sender_may_readmit(
            &stored,
            own_account,
            crate::device_binding::account_key_of_credential(
                sender.credential().serialized_content(),
                sender.signature_key().as_slice(),
            ),
        ) {
            return Err(welcome_refused(
                "re-entry refused: the welcome's sender is not this account, an owner or admin \
                 of the stored group, or the other person of a one-to-one",
            ));
        }

        let own_key = identity.signature_public_key();
        let stored_own_is_this_device = stored
            .own_leaf_node()
            .is_some_and(|leaf| leaf.signature_key().as_slice() == own_key);
        let own_leaves: Vec<Vec<u8>> = staged
            .members()
            .filter(|m| m.signature_key == own_key)
            .map(|m| m.encryption_key)
            .collect();
        let [new_leaf_encryption_key] = own_leaves.as_slice() else {
            return Err(welcome_refused(
                "re-entry refused: the new tree does not hold exactly one leaf of this device",
            ));
        };
        if !stored_own_is_this_device {
            return Err(welcome_refused(
                "re-entry refused: the stored group's own leaf is not this device's",
            ));
        }
        let new_leaf_key = EncryptionKey::from(new_leaf_encryption_key.clone());

        let joined_as = JoinedAs {
            group_id: &group_id,
            welcome_epoch,
            own_key,
            new_leaf_key: &new_leaf_key,
            welcomed: &welcomed,
        };
        if !recorded_key_packages
            .iter()
            .any(|bytes| joined_as.is_recorded(identity, bytes))
        {
            return Err(welcome_refused(
                "re-entry refused: the welcome adds none of this device's recorded re-entry \
                 key packages",
            ));
        }

        delete_stored_group(identity, &mut stored)?;
        let inner = staged
            .into_group(&identity.provider)
            .map_err(|e| Error::Group(format!("welcome -> group: {e:?}")))?;
        Ok(Self::from_mls_group(inner))
    }

    /// Whether a gap history signed by `signer_leaf_key` may be kept by this
    /// device (ADR-0091 section 7, security review 2026-11-21): the same rule
    /// [`Group::join_reentry`] applies to a re-entry Welcome's sender, judged
    /// on this group (the one the device just joined). `true` when the signer
    /// is a current leaf of this group whose ADR-0050 binding resolves to this
    /// device's own account (`identity`'s), to the owner or an admin of this
    /// group's role table, or, in a one-to-one, to the other person. A plain
    /// member of a group, a leaf not in the roster, or a key of another length
    /// is `false`: drop the bundle. It does not depend on which device
    /// committed the replacement, so the proposer or the committer may send.
    #[must_use]
    pub fn gap_history_signer_accepted(&self, identity: &Identity, signer_leaf_key: &[u8]) -> bool {
        let Some(signer_account) = self.inner.members().find_map(|m| {
            (m.signature_key == signer_leaf_key)
                .then(|| {
                    crate::device_binding::account_key_of_credential(
                        m.credential.serialized_content(),
                        &m.signature_key,
                    )
                })
                .flatten()
        }) else {
            return false;
        };
        sender_may_readmit(
            &self.inner,
            identity.account_public_key(),
            Some(signer_account),
        )
    }
}

#[cfg(test)]
mod reentry_tests {
    //! ADR-0091 phase 2 property tests. The names are the ADR's contract
    //! (section 6); the two behind-verdict properties live with their rule in
    //! `conversation_behind.rs`.

    use openmls::framing::MlsMessageOut;
    use openmls::group::QueuedProposal;
    use openmls::prelude::{KeyPackage, LeafNodeIndex};
    use openmls_traits::OpenMlsProvider as _;
    use proptest::prelude::*;

    use crate::group::{encode_message, Group, Incoming, SweepContext, SweepState};
    use crate::group_kind::GroupKind;
    use crate::group_roles::RoleTable;
    use crate::identity::Identity;
    use crate::refusal::RefusalReason;

    fn wire(m: MlsMessageOut) -> Vec<u8> {
        encode_message(&m).expect("encode")
    }

    fn bytes(kp: &KeyPackage) -> Vec<u8> {
        crate::key_package::serialize(kp).expect("serialize")
    }

    fn kp_wire(id: &Identity) -> Vec<u8> {
        bytes(&id.create_key_package().expect("key package"))
    }

    fn person(tag: u8) -> Identity {
        Identity::from_seed(&[tag; 64]).expect("identity")
    }

    fn key(id: &Identity) -> [u8; 32] {
        id.signature_public_key().try_into().expect("32-byte key")
    }

    fn leaf_of(g: &Group, id: &Identity) -> u32 {
        g.members()
            .iter()
            .find(|m| m.signature_public_key == id.signature_public_key())
            .expect("member present")
            .leaf_index
    }

    fn leaves_of(g: &Group, id: &Identity) -> usize {
        g.members()
            .iter()
            .filter(|m| m.signature_public_key == id.signature_public_key())
            .count()
    }

    fn reason<T>(r: crate::Result<T>) -> RefusalReason {
        match r {
            Ok(_) => panic!("expected a refusal, got success"),
            Err(e) => e
                .refusal_reason()
                .unwrap_or_else(|| panic!("expected a typed refusal, got {e}")),
        }
    }

    fn stored(g: &mut Group, id: &Identity, msg: &[u8]) -> bool {
        match g
            .process_incoming_verifying_roles(id, msg)
            .expect("processes")
        {
            Incoming::Proposal(record) => record.stored,
            other => panic!("expected a proposal, got {other:?}"),
        }
    }

    /// alice owns, bob is an admin, charlie a plain member, in a group whose
    /// id is `[tag; 32]`.
    struct Scene {
        alice: Identity,
        bob: Identity,
        charlie: Identity,
        a: Group,
        b: Group,
        c: Group,
    }

    fn group_scene(tag: u8) -> Scene {
        let alice = person(0x10 ^ tag);
        let bob = person(0x20 ^ tag);
        let charlie = person(0x30 ^ tag);
        let mut a = Group::create_conversation_at(&alice, GroupKind::Group, &[tag; 32])
            .expect("alice founds");
        let add = a
            .add_members_from_wire(&alice, &[&kp_wire(&bob), &kp_wire(&charlie)])
            .expect("founding commit");
        let welcome = wire(add.welcome);
        let mut b = Group::join_from_welcome(&welcome, &bob).expect("bob joins");
        let mut c = Group::join_from_welcome(&welcome, &charlie).expect("charlie joins");
        let promote = a
            .set_group_roles(&alice, &RoleTable::new(key(&alice), vec![key(&bob)]))
            .expect("bob becomes admin");
        b.process_incoming_verifying_roles(&bob, &promote)
            .expect("bob applies");
        c.process_incoming_verifying_roles(&charlie, &promote)
            .expect("charlie applies");
        Scene {
            alice,
            bob,
            charlie,
            a,
            b,
            c,
        }
    }

    /// alice adds dave by a swept proposal; bob applies the commit, charlie
    /// misses it and is behind. Returns dave's Welcome.
    fn charlie_misses_a_commit(s: &mut Scene, dave: &Identity) -> Vec<u8> {
        let proposal =
            s.a.propose_add_member_from_wire(&s.alice, &kp_wire(dave))
                .expect("alice asks for dave");
        assert!(stored(&mut s.b, &s.bob, &proposal));
        let swept =
            s.a.commit_pending_proposals(&s.alice)
                .expect("sweep")
                .expect("a commit");
        s.b.process_incoming_verifying_roles(&s.bob, &wire(swept.commit))
            .expect("bob applies");
        wire(swept.welcome.expect("a welcome for dave"))
    }

    /// `admitter` proposes the replacement, `peer` stores both proposals,
    /// `admitter` sweeps and `peer` applies. Returns the Welcome.
    fn readmit(
        admitter_g: &mut Group,
        admitter: &Identity,
        peer_g: &mut Group,
        peer: &Identity,
        k: &[u8],
    ) -> Vec<u8> {
        let props = admitter_g
            .propose_reentry_replacement(admitter, k)
            .expect("the re-admitter proposes the replacement");
        assert!(stored(peer_g, peer, &props.add), "the peer keeps the add");
        assert!(
            stored(peer_g, peer, &props.remove),
            "the peer keeps the remove"
        );
        let swept = admitter_g
            .commit_pending_proposals(admitter)
            .expect("sweep")
            .expect("a commit");
        assert_eq!(swept.swept.len(), 2, "the pair rides one commit");
        peer_g
            .process_incoming_verifying_roles(peer, &wire(swept.commit))
            .expect("the peer's commit gate accepts the replacement");
        wire(swept.welcome.expect("the replacement's welcome"))
    }

    /// A commit built straight in OpenMLS with its proposals by value — a
    /// modified committer, the case the peer commit gate exists for.
    fn forge(g: &mut Group, id: &Identity, adds: Vec<KeyPackage>, removes: &[u32]) -> Vec<u8> {
        let bundle = g
            .inner
            .commit_builder()
            .consume_proposal_store(false)
            .propose_adds(adds)
            .propose_removals(removes.iter().map(|l| LeafNodeIndex::new(*l)))
            .load_psks(id.provider.storage())
            .expect("psks")
            .build(
                id.provider.rand(),
                id.provider.crypto(),
                &id.signature_keys,
                |_| true,
            )
            .expect("openmls builds it")
            .stage_commit(&id.provider)
            .expect("stage");
        let commit = wire(bundle.into_commit());
        g.inner
            .clear_pending_commit(id.provider.storage())
            .expect("the forger discards its own copy");
        commit
    }

    fn gate_refuses(g: &mut Group, id: &Identity, commit: &[u8]) -> RefusalReason {
        let epoch = g.epoch();
        let r = reason(g.process_incoming_verifying_roles(id, commit));
        assert_eq!(g.epoch(), epoch, "a refused commit is not merged");
        r
    }

    #[test]
    fn reentry_replacement_keeps_the_signature_key_and_the_account() {
        let mut s = group_scene(0x01);
        let dave = person(0x41);
        charlie_misses_a_commit(&mut s, &dave);
        let k = bytes(
            &s.charlie
                .create_reentry_key_package(&s.a.group_id(), s.a.epoch())
                .expect("charlie mints K"),
        );
        let target =
            s.b.reentry_request_target(&s.bob, &k)
                .expect("a valid request");
        assert_eq!(target.replaced_leaf, leaf_of(&s.b, &s.charlie));
        assert_eq!(target.signature_key, s.charlie.signature_public_key());
        assert_eq!(target.account_key, key(&s.charlie));
        assert_eq!(target.epoch_left, s.b.epoch());
        let roles = s.a.group_roles();
        let count = s.a.members().len();

        readmit(&mut s.b, &s.bob, &mut s.a, &s.alice, &k);

        for g in [&s.a, &s.b] {
            assert_eq!(g.members().len(), count, "one leaf replaced, none added");
            assert_eq!(leaves_of(g, &s.charlie), 1);
            let m = g
                .members()
                .into_iter()
                .find(|m| m.signature_public_key == s.charlie.signature_public_key())
                .expect("charlie");
            assert_eq!(m.account_key, key(&s.charlie).to_vec());
            assert_eq!(g.group_roles(), roles, "the role table is untouched");
        }
    }

    #[test]
    fn a_device_brought_back_reads_the_next_epoch() {
        let mut s = group_scene(0x02);
        let dave = person(0x42);
        charlie_misses_a_commit(&mut s, &dave);
        assert!(s.c.epoch() < s.a.epoch(), "charlie is behind");
        let k = bytes(
            &s.charlie
                .create_reentry_key_package(&s.a.group_id(), s.a.epoch())
                .expect("charlie mints K"),
        );
        let welcome = readmit(&mut s.b, &s.bob, &mut s.a, &s.alice, &k);

        let mut c2 = Group::join_reentry(&welcome, &s.charlie, &[k]).expect("charlie is back");
        assert_eq!(c2.epoch(), s.a.epoch());
        assert!(c2.is_active());
        // Who may sign charlie's gap history does not depend on who swept:
        // the owner (alice) and the admin (bob) may, charlie's own account
        // may, a stranger's key may not.
        assert!(c2.gap_history_signer_accepted(&s.charlie, s.alice.signature_public_key()));
        assert!(c2.gap_history_signer_accepted(&s.charlie, s.bob.signature_public_key()));
        assert!(c2.gap_history_signer_accepted(&s.charlie, s.charlie.signature_public_key()));
        assert!(!c2.gap_history_signer_accepted(&s.charlie, &[0x11; 32]));

        let hello = wire(
            s.a.create_application_message(&s.alice, b"hello again")
                .expect("alice sends"),
        );
        let Incoming::Application {
            bytes,
            sender_account,
            ..
        } = c2
            .process_incoming_verifying_roles(&s.charlie, &hello)
            .expect("charlie decrypts")
        else {
            panic!("expected an application message");
        };
        assert_eq!(bytes, b"hello again");
        assert_eq!(sender_account, Some(key(&s.alice).to_vec()));

        // C-68: charlie is the same author as before.
        let reply = wire(
            c2.create_application_message(&s.charlie, b"back")
                .expect("charlie sends"),
        );
        for (g, id) in [(&mut s.a, &s.alice), (&mut s.b, &s.bob)] {
            let Incoming::Application { sender_account, .. } = g
                .process_incoming_verifying_roles(id, &reply)
                .expect("decrypts")
            else {
                panic!("expected an application message");
            };
            assert_eq!(sender_account, Some(key(&s.charlie).to_vec()));
        }
    }

    #[test]
    fn a_reentry_marker_for_another_group_or_epoch_is_refused_at_every_gate() {
        for (i, variant) in ["other group", "stale epoch", "future epoch"]
            .iter()
            .enumerate()
        {
            let tag = 0x10 + u8::try_from(i).expect("small");
            let mut s = group_scene(tag);
            let dave = person(0x50 ^ tag);
            charlie_misses_a_commit(&mut s, &dave);
            let gid = s.a.group_id();
            let epoch = s.a.epoch();
            let bad = match *variant {
                "other group" => s.charlie.create_reentry_key_package(&[0xEE; 32], epoch),
                "stale epoch" => s.charlie.create_reentry_key_package(&gid, epoch - 1),
                _ => s.charlie.create_reentry_key_package(&gid, epoch + 1),
            }
            .expect("charlie mints it");
            let charlie_leaf = leaf_of(&s.b, &s.charlie);

            // The proposer's own gates.
            assert_eq!(
                reason(s.b.reentry_request_target(&s.bob, &bytes(&bad))),
                RefusalReason::ReentryPairInvalid,
                "{variant}: target",
            );
            assert_eq!(
                reason(s.b.propose_add_member(&s.bob, bad.clone())),
                RefusalReason::ReentryPairInvalid,
                "{variant}: proposer",
            );
            // The receipt gate.
            let ungated =
                s.b.ungated_propose_add(&s.bob, bad.clone())
                    .expect("a modified admin emits it");
            assert!(!stored(&mut s.a, &s.alice, &ungated), "{variant}: receipt");
            // The sweep: bob's own store holds the ungated add.
            assert!(
                s.b.commit_pending_proposals(&s.bob)
                    .expect("sweep")
                    .is_none(),
                "{variant}: the sweep carries nothing",
            );
            // The commit gate, against a commit with the add by value.
            let mut forger = Group::load(&s.bob, &gid)
                .expect("load")
                .expect("bob's group");
            forger
                .inner
                .clear_pending_proposals(s.bob.provider.storage())
                .expect("clear");
            let commit = forge(&mut forger, &s.bob, vec![bad], &[charlie_leaf]);
            assert_eq!(
                gate_refuses(&mut s.a, &s.alice, &commit),
                RefusalReason::ReentryPairInvalid,
                "{variant}: commit gate",
            );
        }
    }

    #[test]
    fn a_key_package_colliding_with_a_current_leaf_without_a_marker_is_refused() {
        let mut s = group_scene(0x20);
        let plain = s.charlie.create_key_package().expect("an ordinary package");
        let charlie_leaf = leaf_of(&s.b, &s.charlie);
        assert_eq!(
            reason(s.b.propose_add_member(&s.bob, plain.clone())),
            RefusalReason::ReentryPairInvalid,
        );
        assert_eq!(
            reason(s.b.reentry_request_target(&s.bob, &bytes(&plain))),
            RefusalReason::ReentryPairInvalid,
        );
        let ungated =
            s.b.ungated_propose_add(&s.bob, plain.clone())
                .expect("emitted anyway");
        assert!(!stored(&mut s.a, &s.alice, &ungated));
        s.b.inner
            .clear_pending_proposals(s.bob.provider.storage())
            .expect("clear");
        let commit = forge(&mut s.b, &s.bob, vec![plain], &[charlie_leaf]);
        assert_eq!(
            gate_refuses(&mut s.a, &s.alice, &commit),
            RefusalReason::ReentryPairInvalid,
        );
        assert_eq!(leaves_of(&s.a, &s.charlie), 1, "charlie keeps his leaf");
    }

    #[test]
    fn a_replayed_directory_key_package_cannot_replace_a_device() {
        let mut s = group_scene(0x21);
        // The package charlie publishes to the directory, replayed by an
        // admin that wants charlie's device swapped out.
        let published = s.charlie.create_key_package().expect("published");
        let charlie_leaf = leaf_of(&s.b, &s.charlie);
        assert_eq!(
            reason(s.b.propose_add_member(&s.bob, published.clone())),
            RefusalReason::ReentryPairInvalid,
        );
        let commit = forge(&mut s.b, &s.bob, vec![published], &[charlie_leaf]);
        assert_eq!(
            gate_refuses(&mut s.a, &s.alice, &commit),
            RefusalReason::ReentryPairInvalid,
        );

        // An old re-entry package, replayed after the replacement it was for
        // landed: its epoch is gone.
        let dave = person(0x61);
        charlie_misses_a_commit(&mut s, &dave);
        let k = s
            .charlie
            .create_reentry_key_package(&s.a.group_id(), s.a.epoch())
            .expect("K");
        let welcome = readmit(&mut s.b, &s.bob, &mut s.a, &s.alice, &bytes(&k));
        let _c2 = Group::join_reentry(&welcome, &s.charlie, &[bytes(&k)]).expect("back");
        assert_eq!(
            reason(s.b.propose_add_member(&s.bob, k.clone())),
            RefusalReason::ReentryPairInvalid,
        );
        let charlie_leaf = leaf_of(&s.b, &s.charlie);
        let commit = forge(&mut s.b, &s.bob, vec![k], &[charlie_leaf]);
        assert_eq!(
            gate_refuses(&mut s.a, &s.alice, &commit),
            RefusalReason::ReentryPairInvalid,
        );
    }

    #[test]
    fn the_owners_last_leaf_can_be_replaced_but_never_removed() {
        let mut s = group_scene(0x22);
        let alice_leaf = leaf_of(&s.b, &s.alice);
        // Never removed: by proposal, and by a forged commit.
        assert_eq!(
            reason(s.b.propose_remove_member(&s.bob, LeafNodeIndex::new(alice_leaf))),
            RefusalReason::OwnerUnremovable,
        );
        let commit = forge(&mut s.b, &s.bob, vec![], &[alice_leaf]);
        assert_eq!(
            gate_refuses(&mut s.c, &s.charlie, &commit),
            RefusalReason::OwnerUnremovable,
        );

        // alice falls behind: bob adds dave, charlie applies, alice does not.
        let dave = person(0x62);
        let p =
            s.b.propose_add_member_from_wire(&s.bob, &kp_wire(&dave))
                .expect("bob asks for dave");
        assert!(stored(&mut s.c, &s.charlie, &p));
        let swept =
            s.b.commit_pending_proposals(&s.bob)
                .expect("sweep")
                .expect("commit");
        s.c.process_incoming_verifying_roles(&s.charlie, &wire(swept.commit))
            .expect("charlie applies");

        let k = bytes(
            &s.alice
                .create_reentry_key_package(&s.b.group_id(), s.b.epoch())
                .expect("alice mints K"),
        );
        let welcome = readmit(&mut s.b, &s.bob, &mut s.c, &s.charlie, &k);
        let a2 = Group::join_reentry(&welcome, &s.alice, &[k]).expect("alice is back");
        for g in [&a2, &s.b, &s.c] {
            assert_eq!(
                g.group_roles().expect("roles").owner(),
                &key(&s.alice),
                "alice is still the owner",
            );
            assert_eq!(leaves_of(g, &s.alice), 1);
        }
    }

    /// alice founds a one-to-one with bob; both hold one leaf.
    fn one_to_one(tag: u8) -> (Identity, Identity, Group, Group) {
        let alice = person(0x70 ^ tag);
        let bob = person(0x80 ^ tag);
        let mut a =
            Group::create_conversation_at(&alice, GroupKind::OneToOne, &[tag; 32]).expect("found");
        let add = a
            .add_members_from_wire(&alice, &[&kp_wire(&bob)])
            .expect("founding commit");
        let b = Group::join_from_welcome(&wire(add.welcome), &bob).expect("bob joins");
        (alice, bob, a, b)
    }

    #[test]
    fn a_one_to_one_leaf_can_be_replaced_but_the_people_never_change() {
        let (alice, bob, mut a, mut b) = one_to_one(0x23);
        let alice_leaf = leaf_of(&b, &alice);
        assert_eq!(
            reason(b.propose_remove_member(&bob, LeafNodeIndex::new(alice_leaf))),
            RefusalReason::OneToOneMembershipFixed,
        );
        let commit = forge(&mut b, &bob, vec![], &[alice_leaf]);
        assert!(
            a.process_incoming_verifying_roles(&alice, &commit).is_err(),
            "a forged removal of a person is refused",
        );

        // alice falls behind: bob rotates his leaf, alice misses it.
        let update = b.self_update(&bob).expect("bob updates");
        let _ = update;
        assert!(a.epoch() < b.epoch());

        // bob, a plain member, brings alice's only device back.
        let k = bytes(
            &alice
                .create_reentry_key_package(&b.group_id(), b.epoch())
                .expect("alice mints K"),
        );
        let props = b
            .propose_reentry_replacement(&bob, &k)
            .expect("bob proposes the replacement");
        let _ = props;
        let swept = b
            .commit_pending_proposals(&bob)
            .expect("sweep")
            .expect("commit");
        assert_eq!(swept.swept.len(), 2);
        let mut a2 = Group::join_reentry(&wire(swept.welcome.expect("welcome")), &alice, &[k])
            .expect("alice is back");
        // In a one-to-one the other person may sign the gap history.
        assert!(a2.gap_history_signer_accepted(&alice, bob.signature_public_key()));
        assert_eq!(a2.group_kind(), Some(GroupKind::OneToOne));
        let mut people: Vec<Vec<u8>> = a2.members().into_iter().map(|m| m.account_key).collect();
        people.sort();
        let mut expected = vec![key(&alice).to_vec(), key(&bob).to_vec()];
        expected.sort();
        assert_eq!(people, expected, "the same two people");
        let hi = wire(b.create_application_message(&bob, b"hi").expect("send"));
        assert!(matches!(
            a2.process_incoming_verifying_roles(&alice, &hi)
                .expect("alice reads"),
            Incoming::Application { .. }
        ));
    }

    #[test]
    fn a_remove_without_its_paired_reentry_add_is_refused_at_receipt_and_in_the_sweep() {
        let mut s = group_scene(0x24);
        let gid = s.a.group_id();
        let alice_leaf = leaf_of(&s.b, &s.alice);
        // charlie proposes alice's re-entry add; bob holds it.
        let k = s
            .alice
            .create_reentry_key_package(&gid, s.a.epoch())
            .expect("K");
        let add =
            s.c.propose_add_member(&s.charlie, k)
                .expect("charlie asks for alice's re-entry");
        assert!(stored(&mut s.b, &s.bob, &add));
        // bob's removal of alice cannot stand on CHARLIE's add.
        assert_eq!(
            reason(s.b.propose_remove_member(&s.bob, LeafNodeIndex::new(alice_leaf))),
            RefusalReason::OwnerUnremovable,
        );
        let (lone, _) =
            s.b.inner
                .propose_remove_member(
                    &s.bob.provider,
                    &s.bob.signature_keys,
                    LeafNodeIndex::new(alice_leaf),
                )
                .expect("a modified admin emits it");
        let lone = wire(lone);
        assert!(
            !stored(&mut s.c, &s.charlie, &lone),
            "refused at receipt: the add beside it is another sender's",
        );
        // The sweep: bob holds charlie's add and his own ungated removal.
        let swept = s.b.commit_pending_proposals(&s.bob).expect("sweep");
        assert!(
            swept.is_none(),
            "neither can be carried: the removal has no pair"
        );
        assert_eq!(leaves_of(&s.b, &s.alice), 1);
    }

    #[test]
    fn a_sweep_keeps_a_reentry_pair_together_or_drops_both() {
        let mut s = group_scene(0x25);
        let gid = s.b.group_id();
        let k = s
            .alice
            .create_reentry_key_package(&gid, s.b.epoch())
            .expect("K");
        s.b.propose_reentry_replacement(&s.bob, &bytes(&k))
            .expect("bob proposes the pair");
        let queued: Vec<QueuedProposal> = s.b.inner.pending_proposals().cloned().collect();
        assert_eq!(queued.len(), 2);
        let refs: Vec<_> = queued
            .iter()
            .map(|qp| qp.proposal_reference_ref().clone())
            .collect();
        let members = s.b.members();
        // The pair is admissible together, the removal never alone.
        assert!(s.b.selection_is_admissible(&queued, &refs, &members));
        assert!(!s.b.selection_is_admissible(&queued, &refs[1..], &members));
        // The exclusion search never keeps the removal without its add.
        let narrowed =
            s.b.narrow_sweep_by_exclusion(&s.bob, &queued, &members, &refs[1..]);
        assert!(narrowed.is_empty());
        // The enumeration keeps the removal only after keeping its add.
        let table = s.b.group_roles().expect("roles");
        let removed = vec![leaf_of(&s.b, &s.alice)];
        let admission = s.b.admission_requirements(&removed);
        let kind = s.b.kind_state();
        let ctx = SweepContext {
            table: &table,
            members: &members,
            kind_before: &kind,
            role_change_survives: true,
            removed_leaves: &removed,
            admission: &admission,
            identity: &s.bob,
            member_scope: None,
            group_id: &gid,
            epoch: s.b.epoch(),
            pending: &queued,
            removed_by_others: &[],
        };
        let mut fresh = SweepState::default();
        assert!(
            !crate::group::sweep_keeps(&ctx, &mut fresh, &queued[1]),
            "the removal alone is dropped",
        );
        let mut state = SweepState::default();
        assert!(
            crate::group::sweep_keeps(&ctx, &mut state, &queued[0]),
            "the add"
        );
        assert!(
            crate::group::sweep_keeps(&ctx, &mut state, &queued[1]),
            "then the removal"
        );
        // ...and the real sweep carries both.
        let swept =
            s.b.commit_pending_proposals(&s.bob)
                .expect("sweep")
                .expect("commit");
        assert_eq!(swept.swept.len(), 2);
    }

    /// The ways a Welcome offered as a re-entry is not one.
    #[derive(Debug, Clone, Copy)]
    enum Wrong {
        OtherKeyPackage,
        NotNewer,
        OtherGroup,
        SenderOutsideRoster,
        OtherKind,
    }

    /// Build the refused case, try it, and check nothing changed. Returns the
    /// refusal.
    fn try_wrong(wrong: Wrong, tag: u8) -> RefusalReason {
        let mut s = group_scene(tag);
        let dave = person(0x90 ^ tag);
        let dave_welcome = charlie_misses_a_commit(&mut s, &dave);
        let gid = s.a.group_id();
        let k = bytes(
            &s.charlie
                .create_reentry_key_package(&gid, s.a.epoch())
                .expect("K"),
        );
        let (welcome, recorded): (Vec<u8>, Vec<Vec<u8>>) = match wrong {
            Wrong::OtherKeyPackage => {
                let other = bytes(
                    &s.charlie
                        .create_reentry_key_package(&gid, s.a.epoch())
                        .expect("another K"),
                );
                (
                    readmit(&mut s.b, &s.bob, &mut s.a, &s.alice, &k),
                    vec![other],
                )
            },
            Wrong::NotNewer => {
                let w = readmit(&mut s.b, &s.bob, &mut s.a, &s.alice, &k);
                Group::join_reentry(&w, &s.charlie, std::slice::from_ref(&k))
                    .expect("the first join");
                (w, vec![k])
            },
            Wrong::OtherGroup => {
                let mut other = Group::create(&s.alice, GroupKind::Group).expect("another group");
                let add = other
                    .add_members_from_wire(&s.alice, &[&kp_wire(&s.charlie)])
                    .expect("alice invites charlie elsewhere");
                (wire(add.welcome), vec![k])
            },
            Wrong::SenderOutsideRoster => {
                let mut d = Group::join_from_welcome(&dave_welcome, &dave).expect("dave joins");
                (readmit(&mut d, &dave, &mut s.a, &s.alice, &k), vec![k])
            },
            Wrong::OtherKind => {
                let eve = person(0xA0 ^ tag);
                let mut e = Group::create_conversation_at(&eve, GroupKind::OneToOne, &[tag; 32])
                    .expect("eve founds a one-to-one under the same id");
                while e.epoch() < s.c.epoch() {
                    e.self_update(&eve).expect("eve advances");
                }
                let k = s
                    .charlie
                    .create_reentry_key_package(&gid, e.epoch())
                    .expect("a K for eve's epoch");
                let add = e
                    .add_members_from_wire(&eve, &[&bytes(&k)])
                    .expect("eve adds it");
                (wire(add.welcome), vec![bytes(&k)])
            },
        };
        let before = s.charlie.snapshot_storage().expect("snapshot");
        let refused = reason(Group::join_reentry(&welcome, &s.charlie, &recorded));
        assert_eq!(
            s.charlie.snapshot_storage().expect("snapshot"),
            before,
            "{wrong:?}: a refused join changes nothing",
        );
        let kept = Group::load(&s.charlie, &gid)
            .expect("load")
            .expect("still stored");
        assert!(
            kept.is_active(),
            "{wrong:?}: the stored group is still active"
        );
        refused
    }

    #[test]
    fn a_refused_join_reentry_changes_nothing() {
        for (i, wrong) in [
            Wrong::OtherKeyPackage,
            Wrong::NotNewer,
            Wrong::OtherGroup,
            Wrong::SenderOutsideRoster,
            Wrong::OtherKind,
        ]
        .into_iter()
        .enumerate()
        {
            let tag = 0x30 + u8::try_from(i).expect("small");
            assert_eq!(
                try_wrong(wrong, tag),
                RefusalReason::ReentryWelcomeRefused,
                "{wrong:?}",
            );
        }
    }

    /// Every pending proposal swept by reference with no filter: what a
    /// modified committer could publish, for the peer commit gate.
    fn sweep_everything(g: &mut Group, id: &Identity) -> Vec<u8> {
        let bundle = g
            .inner
            .commit_builder()
            .consume_proposal_store(true)
            .load_psks(id.provider.storage())
            .expect("psks")
            .build(
                id.provider.rand(),
                id.provider.crypto(),
                &id.signature_keys,
                |_| true,
            )
            .expect("openmls builds it")
            .stage_commit(&id.provider)
            .expect("stage");
        let commit = wire(bundle.into_commit());
        g.inner
            .clear_pending_commit(id.provider.storage())
            .expect("the forger discards its own copy");
        commit
    }

    /// bob (an admin) proposes charlie's removal while charlie holds on by
    /// `on_hold`, an `Add` charlie authored of a device of its own account.
    /// alice's sweep carries the removal and not the `Add`, so charlie is out;
    /// and a commit carrying both is refused by a peer's commit gate.
    fn removed_member_holds_on_by(tag: u8, on_hold: impl Fn(&Scene) -> KeyPackage) {
        let mut s = group_scene(tag);
        let charlie_leaf = leaf_of(&s.a, &s.charlie);
        let held = on_hold(&s);
        let add =
            s.c.propose_add_member(&s.charlie, held)
                .expect("charlie asks for its own device");
        let remove =
            s.b.propose_remove_member(&s.bob, LeafNodeIndex::new(charlie_leaf))
                .expect("bob asks to remove charlie");
        for (g, id) in [(&mut s.a, &s.alice), (&mut s.b, &s.bob)] {
            for msg in [&add, &remove] {
                if g.process_incoming_verifying_roles(id, msg).is_err() {
                    // bob's own proposal echo: already in his store.
                }
            }
        }
        // A modified committer that carries both is refused by a peer.
        let both = sweep_everything(&mut s.a, &s.alice);
        assert_eq!(
            gate_refuses(&mut s.b, &s.bob, &both),
            RefusalReason::ReentryPairInvalid,
            "a peer refuses a commit that keeps a removed member by its own add",
        );
        // alice's own sweep keeps the removal and drops the add.
        let swept =
            s.a.commit_pending_proposals(&s.alice)
                .expect("sweep")
                .expect("a commit");
        s.b.process_incoming_verifying_roles(&s.bob, &wire(swept.commit))
            .expect("bob applies alice's sweep");
        let charlie_account = key(&s.charlie).to_vec();
        for g in [&s.a, &s.b] {
            assert!(
                g.members().iter().all(|m| m.account_key != charlie_account),
                "charlie is out on every peer",
            );
        }
    }

    #[test]
    fn a_member_being_removed_cannot_survive_by_its_own_reentry_add() {
        removed_member_holds_on_by(0x5E, |s| {
            s.charlie
                .create_reentry_key_package(&s.a.group_id(), s.a.epoch())
                .expect("charlie mints a re-entry package of its own leaf")
        });
    }

    #[test]
    fn a_member_being_removed_cannot_survive_by_adding_another_device() {
        removed_member_holds_on_by(0x5F, |s| {
            let second = Identity::from_seed_bound(
                &[0x6F; 32],
                &s.charlie.ed25519_seed_bytes(),
                1_700_000_000,
            )
            .expect("charlie's second device");
            second.create_key_package().expect("its key package")
        });
    }

    /// Security review 2026-11-21: a plain member of a group may propose and
    /// sweep a re-entry, but its Welcome does not replace the stored group of
    /// the returning device (ADR-0091 section 4 names the own account, the
    /// owner and the admins as re-admitters); the refusal changes nothing.
    #[test]
    fn a_plain_members_reentry_welcome_is_refused_and_changes_nothing() {
        let mut s = group_scene(0x5B);
        let erin = person(0x5C);
        let add =
            s.a.add_members_from_wire(&s.alice, &[&kp_wire(&erin)])
                .expect("alice adds erin");
        let commit = wire(add.commit);
        s.b.process_incoming_verifying_roles(&s.bob, &commit)
            .expect("bob applies");
        s.c.process_incoming_verifying_roles(&s.charlie, &commit)
            .expect("charlie applies");
        let mut e = Group::join_from_welcome(&wire(add.welcome), &erin).expect("erin joins");

        // charlie misses the next commit.
        let dave = person(0x5D);
        let proposal =
            s.a.propose_add_member_from_wire(&s.alice, &kp_wire(&dave))
                .expect("alice asks for dave");
        assert!(stored(&mut s.b, &s.bob, &proposal));
        assert!(stored(&mut e, &erin, &proposal));
        let swept =
            s.a.commit_pending_proposals(&s.alice)
                .expect("sweep")
                .expect("a commit");
        let commit = wire(swept.commit);
        s.b.process_incoming_verifying_roles(&s.bob, &commit)
            .expect("bob applies");
        e.process_incoming_verifying_roles(&erin, &commit)
            .expect("erin applies");

        let gid = s.a.group_id();
        let k = bytes(
            &s.charlie
                .create_reentry_key_package(&gid, s.a.epoch())
                .expect("K"),
        );
        // erin, a plain member, proposes and sweeps the replacement.
        let welcome = readmit(&mut e, &erin, &mut s.b, &s.bob, &k);

        let before = s.charlie.snapshot_storage().expect("snapshot");
        assert_eq!(
            reason(Group::join_reentry(&welcome, &s.charlie, &[k])),
            RefusalReason::ReentryWelcomeRefused,
        );
        assert_eq!(
            s.charlie.snapshot_storage().expect("snapshot"),
            before,
            "a refused join changes nothing",
        );
        let kept = Group::load(&s.charlie, &gid)
            .expect("load")
            .expect("still stored");
        assert!(kept.is_active(), "the stored group is still active");

        // bob (an admin) brings charlie back; on the group charlie joined, the
        // gap-history signer rule matches the Welcome rule: the owner, an
        // admin and charlie's own account may sign, erin (a plain member) and
        // a key outside the roster may not.
        let k2 = bytes(
            &s.charlie
                .create_reentry_key_package(&gid, s.b.epoch())
                .expect("K2"),
        );
        let welcome = readmit(&mut s.b, &s.bob, &mut e, &erin, &k2);
        let c2 = Group::join_reentry(&welcome, &s.charlie, &[k2]).expect("charlie is back");
        assert!(c2.gap_history_signer_accepted(&s.charlie, s.alice.signature_public_key()));
        assert!(c2.gap_history_signer_accepted(&s.charlie, s.bob.signature_public_key()));
        assert!(c2.gap_history_signer_accepted(&s.charlie, s.charlie.signature_public_key()));
        assert!(
            !c2.gap_history_signer_accepted(&s.charlie, erin.signature_public_key()),
            "a plain member's gap history is dropped"
        );
        assert!(!c2.gap_history_signer_accepted(&s.charlie, dave.signature_public_key()));
        assert!(!c2.gap_history_signer_accepted(&s.charlie, &[0x11; 31]));
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 10, ..ProptestConfig::default() })]

        /// ADR-0091 section 6: the Welcome replaces the stored group only for
        /// the recorded KeyPackage, a newer epoch, the same group and kind,
        /// and a sender in the stored roster; and with the right record it
        /// does.
        #[test]
        fn join_reentry_replaces_only_for_the_recorded_key_package(
            which in 0usize..5,
            tag in 0x40u8..0x7f,
        ) {
            let wrong = [
                Wrong::OtherKeyPackage,
                Wrong::NotNewer,
                Wrong::OtherGroup,
                Wrong::SenderOutsideRoster,
                Wrong::OtherKind,
            ][which];
            prop_assert_eq!(try_wrong(wrong, tag), RefusalReason::ReentryWelcomeRefused);

            // The control: the recorded package and its own Welcome join.
            let mut s = group_scene(tag);
            let dave = person(0x90 ^ tag);
            charlie_misses_a_commit(&mut s, &dave);
            let k = bytes(
                &s.charlie
                    .create_reentry_key_package(&s.a.group_id(), s.a.epoch())
                    .expect("K"),
            );
            let other = bytes(
                &s.charlie
                    .create_reentry_key_package(&s.a.group_id(), s.a.epoch())
                    .expect("another K"),
            );
            let w = readmit(&mut s.b, &s.bob, &mut s.a, &s.alice, &k);
            let c2 = Group::join_reentry(&w, &s.charlie, &[other, k]).expect("joins");
            prop_assert_eq!(c2.epoch(), s.a.epoch());
        }
    }
}
