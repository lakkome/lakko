// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Slim decrypt-only UniFFI surface — ADR-0041 Phase 2.
//!
//! The iOS `UNNotificationServiceExtension` (NSE) runs in a separate process
//! with a hard ~30 MB RAM ceiling. Linking the full 333 MB / ~111 MB-per-slice
//! `LakkoCore.xcframework` (from `lakko-ffi-uniffi`) into it is very likely to
//! bust the budget on load, and it drags transport / calls / SFU / channels
//! code the background-decrypt path never touches into a memory-starved
//! process. This crate is the **slim** alternative: it re-exports only the
//! read/decrypt subset the NSE needs, over the SAME audited `lakko-mls` +
//! `lakko-attachments` crates the full surface uses — it does not fork them.
//!
//! The exposed surface is exactly the read-only-throwaway decrypt path web +
//! Android already ship (ADR-0041 §Phase 3), and nothing more:
//!
//! - [`Identity::from_seed`] — reconstruct the account identity from its
//!   64-byte phrase seed (ADR-0017), the root of the NSE decrypt.
//! - [`Identity::restore_storage`] — rehydrate the OpenMLS provider from the
//!   app's last-persisted snapshot (read from the shared App Group container).
//! - [`load_group`] — rebuild a temporary `Group` handle by `group_id`.
//! - [`Group::process_incoming_verifying_roles`] — decrypt a fetched frame
//!   through the audited ADR-0022 role-verifying path.
//! - [`Group::account_key_at`] — resolve a leaf to the account key that names
//!   the person behind it (ADR-0050), so the extension can title a wake with a
//!   contact's name instead of "Lakko". One leaf in, one key out; never the
//!   roster.
//! - [`attachments::parse_body_envelope`] — classify the decrypted application
//!   body (ADR-0023/0024) into a [`attachments::ParsedBody`].
//! - [`call_signaling::parse_call_signal`] — classify a decrypted payload as
//!   call signalling, so the extension can RING (`docs/ux/calls.md` §2) instead
//!   of suppressing every call frame as an unrecognised service envelope — the
//!   single transport change §7 records the whole of §2 as depending on.
//!   Classify only: there is no compose, so the extension can never answer,
//!   decline or end a call, all of which would advance the sender ratchet.
//! - [`push_routing::decode_push_routing_secret`] — recognise an ADR-0042 /
//!   ADR-0044 push-routing-secret SERVICE envelope so the NSE filters it out
//!   instead of rendering it. Service envelopes ride the same group journal as
//!   chat, and `parse_body_envelope`'s deliberate legacy fallback would
//!   otherwise degrade one to a `Chat` body full of raw binary.
//! - [`committer_beacon::decode_committer_beacon`] — the same filter question
//!   for the ADR-0045 committer-liveness beacon, which rides the same journal
//!   and hits the same lossy fallback. Emitting one is deliberately NOT
//!   exposed: this process structurally cannot sweep (C-59), so a beacon from
//!   it would be a lie.
//! - [`committer_beacon::committer_beacon_envelope_len`] — the one wire
//!   constant that filter's cheap pre-peek needs, so the length is not
//!   hand-copied into Swift where nothing pins it. A pure value on the READ
//!   side of the classification question; it opens no write path.
//! - [`profile::decode_profile_envelope`] +
//!   [`profile::profile_envelope_min_len`] / [`profile::profile_envelope_max_len`]
//!   — the same filter question for the ADR-0048 profile envelope. Decode
//!   returns no fields: the NSE must not learn or store profiles (C-59).
//! - [`group_name::decode_group_name_envelope`] +
//!   [`group_name::group_name_envelope_min_len`] /
//!   [`group_name::group_name_envelope_max_len`] — the same filter question
//!   for the ADR-0051 group-name envelope. Decode returns the name (it is
//!   not personal data and may title a preview) but nothing here composes,
//!   orders or persists one: acceptance stays the app's decision (C-59).
//! - [`pin::decode_pin_envelope`] + [`pin::pin_envelope_len`] — the same
//!   filter question for the ADR-0052 shared-pin envelope. Decode returns the
//!   operation (to word a preview); nothing here composes, orders or persists
//!   one (C-59).
//!
//! There is deliberately **no compose / send / call / channel / transport
//! surface**: the NSE only reads and decrypts, then discards. It NEVER writes
//! back the advanced snapshot or the cursor (ADR-0041 §Phase 3 read-only
//! throwaway discipline), so the foreground app's ratchet is never advanced
//! behind its back.
//!
//! The Swift API shape mirrors the equivalent types in `lakko-ffi-uniffi` so
//! the Phase-3 NSE handler can call the same names against this slim framework.

#![forbid(unsafe_code)]

use std::sync::{Arc, Mutex};

use lakko_mls::{Group as MlsGroup, Identity as MlsIdentity, Incoming as MlsIncoming};

pub mod attachments;
pub mod call_signaling;
pub mod committer_beacon;
pub mod ds_proof;
pub mod group_name;
pub mod pin;
pub mod profile;
pub mod profile_request;
pub mod push_routing;

uniffi::setup_scaffolding!();

/// Errors surfaced to Swift callers of the slim decrypt surface.
///
/// A focused subset of `lakko-ffi-uniffi`'s `FfiError` — only the variants the
/// decrypt path can produce. Variants intentionally collapse internal detail;
/// callers branch on the variant, not the message string (which is for logs
/// only). Every failure here maps, in the NSE, to a fail-closed degrade to the
/// generic "New message" banner (ADR-0041 §Phase 3, invariant #4).
#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum FfiError {
    /// Identity / signing-key error — a malformed seed at [`Identity::from_seed`]
    /// or a malformed snapshot at [`Identity::restore_storage`].
    #[error("identity error: {0}")]
    Identity(String),

    /// KeyPackage validation or serialization error.
    #[error("key package error: {0}")]
    KeyPackage(String),

    /// Group state-machine error — a frame that will not process / authenticate.
    #[error("group error: {0}")]
    Group(String),

    /// Application-message error.
    #[error("message error: {0}")]
    Message(String),

    /// Exporter error.
    #[error("exporter error: {0}")]
    Exporter(String),

    /// Wire-format codec error.
    #[error("codec error: {0}")]
    Codec(String),

    /// Unsupported or unexpected wire message.
    #[error("unsupported: {0}")]
    Unsupported(String),

    /// Contacts-layer envelope error — surfaced by
    /// [`push_routing::decode_push_routing_secret`] when a plaintext is NOT a
    /// push-routing-secret envelope. On the NSE filter path this is the
    /// EXPECTED, common outcome (every chat message lands here), not a fault:
    /// the caller reads it as "not a service envelope" and falls through to the
    /// normal chat parse. Same variant name as `lakko-ffi-uniffi`'s `FfiError`
    /// so the two frameworks present one error shape to Swift.
    #[error("contacts error: {0}")]
    Contacts(String),

    /// Lock contention / poisoned mutex on the shared [`Group`] state.
    #[error("internal lock error")]
    LockPoisoned,

    /// Call-signalling codec error — surfaced by
    /// [`call_signaling::parse_call_signal`] when a plaintext is NOT a
    /// well-formed call-signalling payload. As with [`Self::Contacts`], on the
    /// NSE filter path this is the EXPECTED, common outcome (every chat message
    /// lands here), not a fault.
    ///
    /// **Appended at the end on purpose.** `#[uniffi(flat_error)]` lowers a
    /// variant as its ORDINAL, so inserting one mid-enum would make an older
    /// binding decode unrelated errors as the wrong variant. New variants go
    /// last, always.
    #[error("call error: {0}")]
    Call(String),
}

impl From<lakko_mls::Error> for FfiError {
    fn from(e: lakko_mls::Error) -> Self {
        match e {
            lakko_mls::Error::Identity(m) => Self::Identity(m),
            lakko_mls::Error::KeyPackage(m) => Self::KeyPackage(m),
            lakko_mls::Error::Group(m) => Self::Group(m),
            lakko_mls::Error::Message(m) => Self::Message(m),
            lakko_mls::Error::Exporter(m) => Self::Exporter(m),
            lakko_mls::Error::Codec(m) => Self::Codec(m),
            lakko_mls::Error::Unsupported(m) => Self::Unsupported(m.to_string()),
            // A membership refusal cannot arise in this process — the slim
            // surface exposes no propose, commit or role path (C-59) — but the
            // match is exhaustive, so the arm is written rather than
            // wildcarded, and it keeps the sentence.
            lakko_mls::Error::MembershipRefused { detail, .. } => Self::Group(detail),
            // ADR-0058 DeviceGroup folder payloads are never parsed here;
            // the arm exists because the match is exhaustive.
            lakko_mls::Error::PayloadBounds(e) => Self::Codec(e.to_string()),
        }
    }
}

/// Result of [`Group::process_incoming_verifying_roles`].
///
/// Mirror of `lakko-ffi-uniffi`'s `ProcessedMessage`; the NSE consumes the
/// `Application` variant's `data` (feeding it to
/// [`attachments::parse_body_envelope`]) and treats the other variants as "no
/// preview to render" (a commit / proposal / external-join advances state on
/// the foreground app, not in the throwaway NSE view).
#[derive(uniffi::Enum)]
pub enum ProcessedMessage {
    /// Decrypted application payload.
    Application {
        /// Plaintext body bytes — hand to [`attachments::parse_body_envelope`].
        data: Vec<u8>,
        /// The sender's leaf index, or `None` for an external sender.
        sender_leaf: Option<u32>,
        /// The account the sender leaf resolves to (ADR-0050), or `None`.
        sender_account: Option<Vec<u8>>,
    },
    /// A commit was processed and merged. Group state advanced.
    Commit,
    /// A standalone proposal was processed (no state advance).
    ///
    /// Deliberately **data-less**, unlike `lakko-ffi-uniffi`'s ADR-0045
    /// `Proposal { record }`. The NSE only asks "is this a chat message worth
    /// previewing", and the answer for every proposal kind is no — it has no
    /// membership UI to drive, cannot sweep (this crate exposes no commit
    /// path), and must stay minimal to fit the ~30 MB extension budget. Adding
    /// the record would import `ProposalKind` for a value nothing here reads.
    Proposal,
    /// A non-member's external-join proposal was processed + retained
    /// (ADR-0027). `joiner_signature_key` is the prospective joiner's leaf
    /// signature public key.
    ExternalJoinProposal {
        /// The prospective joiner's leaf signature public key.
        joiner_signature_key: Vec<u8>,
    },
}

/// Lakko identity — owns the device's signing keypair, MLS credential and
/// per-device storage provider.
///
/// The NSE reconstructs a **throwaway** identity from the shared seed; it never
/// persists any mutation back.
#[derive(uniffi::Object)]
pub struct Identity {
    inner: MlsIdentity,
}

#[uniffi::export]
impl Identity {
    /// Derive a deterministic identity from a 64-byte phrase seed (ADR-0017).
    ///
    /// Same seed ⇒ same Ed25519 + X25519 keypairs ⇒ same device identity, so
    /// the NSE reconstructs exactly the identity the foreground app persisted.
    /// Mirror of `lakko-ffi-uniffi`'s `Identity::from_seed`; the underlying
    /// `MlsIdentity::from_seed` rejects a wrong-length seed.
    #[uniffi::constructor]
    pub fn from_seed(seed: Vec<u8>) -> Result<Self, FfiError> {
        let inner = MlsIdentity::from_seed(&seed)?;
        Ok(Self { inner })
    }

    /// Restore the OpenMLS provider storage from a snapshot blob.
    ///
    /// Overwrites whatever state the local (fresh, throwaway) provider had —
    /// pair this with a freshly-constructed [`Identity::from_seed`] so the
    /// public keys match the snapshot's secrets. The NSE reads the snapshot
    /// from the shared App Group container (ADR-0041 §Phase 1) and restores it
    /// into an in-memory provider it discards after the decrypt.
    ///
    /// # Errors
    ///
    /// [`FfiError::Identity`] — the snapshot is malformed (truncated counter,
    /// length-prefix overflow, …).
    pub fn restore_storage(&self, snapshot: Vec<u8>) -> Result<(), FfiError> {
        self.inner.restore_storage(&snapshot)?;
        Ok(())
    }
}

/// Lakko MLS conversation group — a temporary, restored-from-snapshot handle in
/// the NSE. The slim surface exposes only the read/decrypt operations; there is
/// no add / remove / commit / send here.
#[derive(uniffi::Object)]
pub struct Group {
    inner: Mutex<MlsGroup>,
}

#[uniffi::export]
impl Group {
    /// Apply an incoming MLS message ENFORCING the ADR-0022 role policy — a
    /// commit whose committer's role does not authorise its add / remove / role
    /// change is REFUSED (not merged), fail-closed. The audited path the
    /// foreground chat receive loop uses; the NSE runs it on a **throwaway**
    /// restored group and never persists the advanced state.
    ///
    /// For an `Application` result, feed the returned `data` to
    /// [`attachments::parse_body_envelope`] to render the preview.
    pub fn process_incoming_verifying_roles(
        &self,
        identity: &Identity,
        bytes: Vec<u8>,
    ) -> Result<ProcessedMessage, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let result = g.process_incoming_verifying_roles(&identity.inner, &bytes)?;
        drop(g); // release the lock before classifying — the result is owned.
        Ok(match result {
            MlsIncoming::Application {
                bytes,
                sender_leaf,
                sender_account,
            } => ProcessedMessage::Application {
                data: bytes,
                sender_leaf,
                sender_account,
            },
            MlsIncoming::Commit => ProcessedMessage::Commit,
            // ADR-0045: the record is discarded on purpose — see
            // `ProcessedMessage::Proposal`.
            MlsIncoming::Proposal(_) => ProcessedMessage::Proposal,
            // `retained` is deliberately dropped: this process cannot approve
            // anything (C-59 — it holds no write path), so the one thing the
            // flag decides has no consumer here.
            MlsIncoming::ExternalJoinProposal {
                joiner_signature_key,
                ..
            } => ProcessedMessage::ExternalJoinProposal {
                joiner_signature_key,
            },
        })
    }

    /// Group identifier bytes (RFC 9420 `group_id`). Stable across the group's
    /// lifetime; the NSE already knows it (it loaded by it) — exposed for
    /// symmetry with the full surface and debug logging.
    pub fn group_id(&self) -> Result<Vec<u8>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.group_id().clone())
    }

    /// Current group epoch of the restored (throwaway) handle.
    pub fn epoch(&self) -> Result<u64, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.epoch())
    }

    /// The **account key** the leaf at `leaf_index` resolves to, or `None` for
    /// a leaf that is not in the roster or whose credential did not resolve.
    ///
    /// This is how the NSE names a caller without opening the chat: the app
    /// side looks the returned 32-byte key up in its own accepted-contacts
    /// store and gets the display name, so the system call screen can say who
    /// is calling (`docs/ux/calls.md` §7, "caller identity in the wake,
    /// resolved locally").
    ///
    /// **At receipt the key already comes with the message**, on
    /// [`ProcessedMessage::Application`]'s `sender_account` — resolved from the
    /// sender leaf at the one moment the roster IS the epoch the message was
    /// sealed at, which is the C-68 rule. Prefer that field. This accessor is
    /// for the case where a leaf index is held WITHOUT its receipt (a leaf
    /// carried inside a payload, say), and is the narrowest read that answers
    /// it: one leaf in, one key out, never the roster.
    ///
    /// It is an account key and not a leaf signature key deliberately: under
    /// ADR-0050 a person has one leaf per device, and the account key is the
    /// published, pinned identity a contact is stored under — so it names the
    /// *person*, which is what a call screen must show, while a leaf key would
    /// name one of their phones and miss on the others.
    ///
    /// Read-only, like everything else here: it consults the restored
    /// throwaway roster and writes nothing (C-59).
    pub fn account_key_at(&self, leaf_index: u32) -> Result<Option<Vec<u8>>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.account_key_at(leaf_index))
    }

    /// The ADR-0053 access signer for this journal at the restored epoch, so
    /// the read-only wake fetch of a claimed group journal can carry its
    /// proof. A pure exporter read over the throwaway group (C-59).
    pub fn ds_access_signer(
        &self,
        identity: &Identity,
    ) -> Result<Arc<ds_proof::DsProofSigner>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        ds_proof::group_signer(&g, &identity.inner)
    }
}

/// Load an existing MLS group from the provider's persisted storage by its
/// `group_id`, returning `None` when the provider has no such group.
///
/// The NSE calls this after [`Identity::restore_storage`] to rebuild the
/// temporary `Group` for the wake payload's target conversation. Free function
/// rather than a `Group::load` constructor because UniFFI rejects
/// `Option<Self>` in constructor signatures — the standard "constructor that
/// may return null" workaround, same shape as `lakko-ffi-uniffi`'s `load_group`.
#[uniffi::export]
pub fn load_group(identity: &Identity, group_id: Vec<u8>) -> Result<Option<Arc<Group>>, FfiError> {
    let loaded = MlsGroup::load(&identity.inner, &group_id)?;
    Ok(loaded.map(|inner| {
        Arc::new(Group {
            inner: Mutex::new(inner),
        })
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attachments::{parse_body_envelope, MessageBodyKind};
    use lakko_mls::group;

    /// End-to-end proof that the slim decrypt surface recovers a plaintext
    /// message via the exact NSE call sequence
    /// (`fromSeed` → `restoreStorage` → `loadGroup` →
    /// `processIncomingVerifyingRoles` → `parseBodyEnvelope`).
    ///
    /// The "foreground app" half (found a group, add a member, seal a chat
    /// message) uses `lakko-mls` + `lakko-attachments` directly — the crates the
    /// slim surface re-exports. The "NSE" half goes exclusively through this
    /// crate's slim FFI types.
    #[test]
    fn nse_style_decrypt_round_trip_recovers_plaintext() {
        // ---- Foreground app: two members, seal a chat message with lakko-mls. ----
        let bob_seed = vec![0xBBu8; 32];
        let alice = MlsIdentity::from_seed(&[0xAAu8; 32]).unwrap();
        let bob = MlsIdentity::from_seed(&bob_seed).unwrap();

        let mut alice_group = MlsGroup::create(&alice).unwrap();
        let bob_kp = bob.create_key_package().unwrap();
        let bob_kp_wire = lakko_mls::key_package::serialize(&bob_kp).unwrap();
        let addition = alice_group
            .add_member_from_wire(&alice, &bob_kp_wire)
            .unwrap();

        // Bob joins from the Welcome — this writes the group into his provider,
        // which the snapshot below captures.
        let welcome_wire = group::encode_message(&addition.welcome).unwrap();
        let bob_group = MlsGroup::join_from_welcome(&welcome_wire, &bob).unwrap();
        let group_id = bob_group.group_id().clone();

        // Alice seals a chat envelope as an MLS Application message (the frame
        // the NSE would fetch from the DS and decrypt).
        let plaintext = "silent-push preview body";
        let envelope = lakko_attachments::compose(plaintext, Vec::new()).unwrap();
        let sealed = alice_group
            .create_application_message(&alice, &envelope)
            .unwrap();
        let sealed_wire = group::encode_message(&sealed).unwrap();

        // Bob's provider snapshot — what the NSE reads from the App Group
        // container in Phase 3.
        let bob_snapshot = bob.snapshot_storage().unwrap();

        // ---- NSE: reconstruct + decrypt exclusively through the SLIM FFI. ----
        let slim_bob = Identity::from_seed(bob_seed).unwrap();
        slim_bob.restore_storage(bob_snapshot).unwrap();

        let loaded = load_group(&slim_bob, group_id.clone()).unwrap();
        let ffi_group = loaded.expect("group present after restore");
        assert_eq!(ffi_group.group_id().unwrap(), group_id);

        let processed = ffi_group
            .process_incoming_verifying_roles(&slim_bob, sealed_wire)
            .unwrap();
        let data = match processed {
            ProcessedMessage::Application {
                data, sender_leaf, ..
            } => {
                assert!(
                    sender_leaf.is_some(),
                    "a group member's message carries a sender leaf",
                );
                data
            },
            _ => panic!("expected an Application ProcessedMessage"),
        };

        // The audited body classifier recovers the exact plaintext.
        let parsed = parse_body_envelope(data);
        assert_eq!(parsed.kind, MessageBodyKind::Chat);
        assert_eq!(parsed.text, plaintext);
        assert!(parsed.attachments.is_empty());
    }

    /// A truncated / malformed snapshot is rejected fail-closed rather than
    /// corrupting the throwaway in-memory state (the NSE degrades to the
    /// generic banner on this error).
    #[test]
    fn restore_rejects_malformed_snapshot() {
        let id = Identity::from_seed(vec![0x11u8; 32]).unwrap();
        let err = id.restore_storage(vec![]).unwrap_err();
        assert!(matches!(err, FfiError::Identity(_)));
    }

    /// `load_group` returns `None` before any restore — no group in a fresh
    /// throwaway provider.
    #[test]
    fn load_group_is_none_without_restore() {
        let id = Identity::from_seed(vec![0x22u8; 32]).unwrap();
        let missing = load_group(&id, vec![0u8; 32]).unwrap();
        assert!(missing.is_none());
    }
}
