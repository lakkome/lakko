// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! MLS-layer wasm-bindgen mirror — `Identity`, `Group`, `AddMember`,
//! `ProcessedMessage`. Phase 1.13a content; the rest of the FFI
//! surface (sync / push / presence / contacts / storage) lives in
//! sibling modules and is wired through `lib.rs`.

use std::sync::Mutex;

use lakko_mls::group::MemberAddition;
use lakko_mls::{
    group, key_package, Group as MlsGroup, Identity as MlsIdentity, Incoming as MlsIncoming,
};
use wasm_bindgen::prelude::*;

/// Map a `lakko_mls::Error` to a `JsError` that surfaces in JS as a
/// thrown `Error` with the message preserved. We deliberately don't
/// expose a structured JS class for errors — adding one is cheap
/// when `apps/web` actually needs to discriminate.
fn mls_err(e: lakko_mls::Error) -> JsError {
    JsError::new(&format!("{e}"))
}

// ============================================================================
// Identity
// ============================================================================

/// Lakko identity — owns the device's signing keypair, MLS credential
/// and per-device storage provider. Mirror of UniFFI's `Identity`.
///
/// JS usage:
///
/// ```js
/// import { Identity } from "@lakko/ffi-wasm";
/// const alice = new Identity("alice");
/// const pk = alice.signaturePublicKey;        // Uint8Array
/// const kp = alice.createKeyPackage();        // Uint8Array
/// ```
#[wasm_bindgen]
pub struct Identity {
    inner: MlsIdentity,
}

#[wasm_bindgen]
impl Identity {
    /// Generate a fresh identity with the given user-display label.
    #[wasm_bindgen(constructor)]
    pub fn new(name: &str) -> Result<Identity, JsError> {
        let inner = MlsIdentity::generate(name).map_err(mls_err)?;
        Ok(Self { inner })
    }

    /// Derive an identity **deterministically** from a seed.
    ///
    /// Same `seed` ⇒ byte-identical Ed25519 + X25519 keys. The
    /// expected caller is the `apps/web` auth layer, which
    /// seeds this with the OPAQUE password export key so a
    /// fresh login on any device reproduces the user's
    /// long-lived public keys without persisting Identity bytes
    /// anywhere.
    #[wasm_bindgen(js_name = fromSeed)]
    pub fn from_seed(seed: &[u8]) -> Result<Identity, JsError> {
        let inner = MlsIdentity::from_seed(seed).map_err(mls_err)?;
        Ok(Self { inner })
    }

    /// Public signature key bytes.
    #[wasm_bindgen(getter, js_name = signaturePublicKey)]
    pub fn signature_public_key(&self) -> Vec<u8> {
        self.inner.signature_public_key().to_vec()
    }

    /// Long-lived X25519 public key (32 bytes) — the HPKE
    /// init_key callers of `lakko-contacts::compose` target when
    /// sealing an envelope to this identity. Pair with the bundle
    /// uploaded to the discovery service via
    /// `POST /discovery/identity` (Phase 1.16d3a).
    #[wasm_bindgen(getter, js_name = x25519PublicKey)]
    pub fn x25519_public_key(&self) -> Vec<u8> {
        self.inner.x25519_public_key().to_vec()
    }

    /// Raw X25519 secret bytes (32 bytes). Used as input to
    /// `ContactRequest.openEnvelope(...)` when decrypting an
    /// incoming envelope.
    ///
    /// **Sensitive.** The caller MUST persist these bytes only
    /// after wrapping them with the device root key (1.16c3c2's
    /// `deriveDeviceRootKey` output). They MUST NOT touch
    /// `localStorage` or any other unencrypted store.
    #[wasm_bindgen(getter, js_name = x25519SecretKey)]
    pub fn x25519_secret_key(&self) -> Vec<u8> {
        self.inner.x25519_secret_bytes().to_vec()
    }

    /// Create a fresh KeyPackage in TLS wire form.
    #[wasm_bindgen(js_name = createKeyPackage)]
    pub fn create_key_package(&self) -> Result<Vec<u8>, JsError> {
        let kp = self.inner.create_key_package().map_err(mls_err)?;
        key_package::serialize(&kp).map_err(mls_err)
    }
}

// ============================================================================
// AddMember (record)
// ============================================================================

/// Result of [`Group::add_member`].
///
/// Distribute `commit` to all existing members for processing; deliver
/// `welcome` to the newly-added member.
#[wasm_bindgen]
pub struct AddMember {
    commit_bytes: Vec<u8>,
    welcome_bytes: Vec<u8>,
}

#[wasm_bindgen]
impl AddMember {
    /// Encoded MLS commit message — broadcast to existing members.
    #[wasm_bindgen(getter)]
    pub fn commit(&self) -> Vec<u8> {
        self.commit_bytes.clone()
    }

    /// Encoded MLS welcome message — sent to the joining member.
    #[wasm_bindgen(getter)]
    pub fn welcome(&self) -> Vec<u8> {
        self.welcome_bytes.clone()
    }
}

// ============================================================================
// ProcessedMessage (tagged result of processIncoming)
// ============================================================================

/// Discriminator for [`ProcessedMessage`]. wasm-bindgen renders this
/// as a JS number-enum; we expose a string discriminator via
/// [`ProcessedMessage::kind`] for ergonomic `switch` / `case`.
#[wasm_bindgen]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProcessedMessageKind {
    /// Application payload (decrypted plaintext available via
    /// `applicationData`).
    Application,
    /// A commit was processed and merged. Group state advanced.
    Commit,
    /// A standalone proposal was processed (no state advance).
    Proposal,
    /// External-join proposal was processed.
    ExternalJoinProposal,
}

/// Tagged result of [`Group::process_incoming`].
///
/// JS code reads `kind` (a string) to discriminate, then
/// `applicationData` for [`ProcessedMessageKind::Application`].
/// We deliberately don't expose a sum-type on the JS side because
/// wasm-bindgen has no idiomatic story for Rust enums with
/// associated data; a tagged class is what TypeScript would idiom
/// to anyway.
#[wasm_bindgen]
pub struct ProcessedMessage {
    kind: ProcessedMessageKind,
    application_data: Option<Vec<u8>>,
}

#[wasm_bindgen]
impl ProcessedMessage {
    /// Discriminator. `"Application" | "Commit" | "Proposal" |
    /// "ExternalJoinProposal"`. Useful for `switch` / `case` in
    /// TypeScript.
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> String {
        match self.kind {
            ProcessedMessageKind::Application => "Application".into(),
            ProcessedMessageKind::Commit => "Commit".into(),
            ProcessedMessageKind::Proposal => "Proposal".into(),
            ProcessedMessageKind::ExternalJoinProposal => "ExternalJoinProposal".into(),
        }
    }

    /// Application plaintext, present iff `kind == "Application"`.
    #[wasm_bindgen(getter, js_name = applicationData)]
    pub fn application_data(&self) -> Option<Vec<u8>> {
        self.application_data.clone()
    }
}

// ============================================================================
// Group
// ============================================================================

/// Lakko MLS conversation group. Mirror of UniFFI's `Group`.
#[wasm_bindgen]
pub struct Group {
    inner: Mutex<MlsGroup>,
}

#[wasm_bindgen]
impl Group {
    /// Create a fresh group with `identity` as the founding member.
    /// Mirror of UniFFI's `Group::create`.
    #[wasm_bindgen(constructor)]
    pub fn new(identity: &Identity) -> Result<Group, JsError> {
        let inner = MlsGroup::create(&identity.inner).map_err(mls_err)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// Join an existing group from a wire-format Welcome message.
    #[wasm_bindgen(js_name = joinFromWelcome)]
    pub fn join_from_welcome(welcome: &[u8], identity: &Identity) -> Result<Group, JsError> {
        let inner = MlsGroup::join_from_welcome(welcome, &identity.inner).map_err(mls_err)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// Add a member from their published KeyPackage (TLS wire form).
    #[wasm_bindgen(js_name = addMember)]
    pub fn add_member(
        &self,
        identity: &Identity,
        key_package: &[u8],
    ) -> Result<AddMember, JsError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        let MemberAddition { commit, welcome } = g
            .add_member_from_wire(&identity.inner, key_package)
            .map_err(mls_err)?;
        Ok(AddMember {
            commit_bytes: group::encode_message(&commit).map_err(mls_err)?,
            welcome_bytes: group::encode_message(&welcome).map_err(mls_err)?,
        })
    }

    /// Remove a member by their leaf index. Returns the wire-encoded
    /// commit to broadcast.
    #[wasm_bindgen(js_name = removeMember)]
    pub fn remove_member(&self, identity: &Identity, leaf_index: u32) -> Result<Vec<u8>, JsError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        let commit = g
            .remove_member(&identity.inner, lakko_mls::LeafNodeIndex::new(leaf_index))
            .map_err(mls_err)?;
        group::encode_message(&commit).map_err(mls_err)
    }

    /// Encrypt an application payload for the current epoch.
    #[wasm_bindgen(js_name = createApplicationMessage)]
    pub fn create_application_message(
        &self,
        identity: &Identity,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, JsError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        let msg = g
            .create_application_message(&identity.inner, plaintext)
            .map_err(mls_err)?;
        group::encode_message(&msg).map_err(mls_err)
    }

    /// Process an incoming MLS message.
    #[wasm_bindgen(js_name = processIncoming)]
    pub fn process_incoming(
        &self,
        identity: &Identity,
        bytes: &[u8],
    ) -> Result<ProcessedMessage, JsError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        let result = g
            .process_incoming(&identity.inner, bytes)
            .map_err(mls_err)?;
        Ok(match result {
            MlsIncoming::Application(data) => ProcessedMessage {
                kind: ProcessedMessageKind::Application,
                application_data: Some(data),
            },
            MlsIncoming::Commit => ProcessedMessage {
                kind: ProcessedMessageKind::Commit,
                application_data: None,
            },
            MlsIncoming::Proposal => ProcessedMessage {
                kind: ProcessedMessageKind::Proposal,
                application_data: None,
            },
            MlsIncoming::ExternalJoinProposal => ProcessedMessage {
                kind: ProcessedMessageKind::ExternalJoinProposal,
                application_data: None,
            },
        })
    }

    /// Derive a `length`-byte secret bound to the current epoch.
    #[wasm_bindgen(js_name = exportSecret)]
    pub fn export_secret(
        &self,
        identity: &Identity,
        label: &str,
        context: &[u8],
        length: u32,
    ) -> Result<Vec<u8>, JsError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        let secret = g
            .export_secret(&identity.inner, label, context, length as usize)
            .map_err(mls_err)?;
        Ok(secret.to_vec())
    }

    /// Local leaf index of the calling identity.
    #[wasm_bindgen(getter, js_name = ownLeafIndex)]
    pub fn own_leaf_index(&self) -> Result<u32, JsError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        Ok(g.own_leaf_index().u32())
    }

    /// Current group epoch.
    #[wasm_bindgen(getter)]
    pub fn epoch(&self) -> Result<u64, JsError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        Ok(g.epoch())
    }

    /// Number of members at the current epoch.
    #[wasm_bindgen(getter, js_name = memberCount)]
    pub fn member_count(&self) -> Result<u32, JsError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        Ok(u32::try_from(g.member_count()).unwrap_or(u32::MAX))
    }
}

// ============================================================================
// Tests
// ============================================================================
//
// Tests run on the host target (`cargo test`) — no `wasm-bindgen-test`
// runner needed for what we cover here. The wire-level invariants
// from `lakko-mls` are unchanged regardless of FFI binding; this
// suite exists to catch shape regressions in the wasm-bindgen
// surface itself (constructor signatures, getter return types, the
// ProcessedMessage tagging convention).

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirror of the lakko-mls integration test driven through the
    /// wasm-bindgen surface. Same end-to-end check the UniFFI mirror
    /// runs — exercises identity creation, group creation, member
    /// addition (commit + welcome split), application-message round-
    /// trip, exporter consistency.
    #[test]
    fn wasm_three_party_lifecycle() {
        let alice = Identity::new("alice").expect("alice identity");
        let bob = Identity::new("bob").expect("bob identity");
        let charlie = Identity::new("charlie").expect("charlie identity");

        let alice_group = Group::new(&alice).expect("alice group");

        // Alice adds Bob.
        let bob_kp = bob.create_key_package().expect("bob key package");
        let add_bob = alice_group.add_member(&alice, &bob_kp).expect("add bob");
        let bob_group = Group::join_from_welcome(&add_bob.welcome(), &bob).expect("bob join");

        // Alice → Bob application message.
        let msg = alice_group
            .create_application_message(&alice, b"hello via wasm")
            .expect("alice send");
        let received = bob_group.process_incoming(&bob, &msg).expect("bob recv");
        assert_eq!(received.kind(), "Application");
        assert_eq!(
            received.application_data().as_deref(),
            Some(&b"hello via wasm"[..]),
        );

        // Alice adds Charlie; Bob processes the commit.
        let charlie_kp = charlie.create_key_package().expect("charlie kp");
        let add_charlie = alice_group
            .add_member(&alice, &charlie_kp)
            .expect("add charlie");
        let bob_post = bob_group
            .process_incoming(&bob, &add_charlie.commit())
            .expect("bob process commit");
        assert_eq!(bob_post.kind(), "Commit");
        assert!(bob_post.application_data().is_none());

        let charlie_group =
            Group::join_from_welcome(&add_charlie.welcome(), &charlie).expect("charlie join");

        // Member counts converge.
        assert_eq!(alice_group.member_count().unwrap(), 3);
        assert_eq!(bob_group.member_count().unwrap(), 3);
        assert_eq!(charlie_group.member_count().unwrap(), 3);

        // Exporter consistency — identical secrets across the group.
        let alice_export = alice_group
            .export_secret(&alice, "lakko/test", b"ctx", 32)
            .expect("alice export");
        let charlie_export = charlie_group
            .export_secret(&charlie, "lakko/test", b"ctx", 32)
            .expect("charlie export");
        assert_eq!(alice_export, charlie_export);
    }

    #[test]
    fn identity_signature_public_key_is_32_bytes() {
        let id = Identity::new("alice").unwrap();
        let pk = id.signature_public_key();
        assert_eq!(pk.len(), 32, "Ed25519 public key length");
    }

    /// Phase 1.16d3b2 — `Identity.fromSeed` is deterministic.
    /// Same seed bytes ⇒ same Ed25519 + X25519 public keys.
    #[test]
    fn identity_from_seed_is_deterministic() {
        let seed = [0x77u8; 64];
        let a = Identity::from_seed(&seed).unwrap();
        let b = Identity::from_seed(&seed).unwrap();
        assert_eq!(a.signature_public_key(), b.signature_public_key());
        assert_eq!(a.x25519_public_key(), b.x25519_public_key());
    }

    /// Phase 1.16d3b — the X25519 envelope keypair surfaces as a
    /// pair of 32-byte `Vec<u8>` getters. Pubkey copies are fresh
    /// each call (idempotent, no shared mutable state).
    #[test]
    fn identity_x25519_keys_are_32_bytes_each() {
        let id = Identity::new("alice").unwrap();
        let pk = id.x25519_public_key();
        let sk = id.x25519_secret_key();
        assert_eq!(pk.len(), 32, "X25519 public key length");
        assert_eq!(sk.len(), 32, "X25519 secret key length");
        // Sanity: at least one byte non-zero on each — would
        // catch a default-initialised secret slipping through.
        assert!(pk.iter().any(|&b| b != 0));
        assert!(sk.iter().any(|&b| b != 0));
        // Idempotent reads — getters return copies of the same
        // underlying secret.
        assert_eq!(id.x25519_public_key(), pk);
        assert_eq!(id.x25519_secret_key(), sk);
    }

    #[test]
    fn processed_message_kind_strings_match_uniffi_enum() {
        // Every variant has a string literal — guards against
        // accidental rename on the Rust side breaking the JS API.
        let app = ProcessedMessage {
            kind: ProcessedMessageKind::Application,
            application_data: Some(vec![1, 2, 3]),
        };
        assert_eq!(app.kind(), "Application");
        assert_eq!(app.application_data().as_deref(), Some(&[1, 2, 3][..]));

        for k in [
            ProcessedMessageKind::Commit,
            ProcessedMessageKind::Proposal,
            ProcessedMessageKind::ExternalJoinProposal,
        ] {
            let m = ProcessedMessage {
                kind: k,
                application_data: None,
            };
            // Each non-Application case has no data.
            assert!(m.application_data().is_none(), "kind={k:?}");
            // And a non-empty discriminator string.
            assert!(!m.kind().is_empty());
        }
    }

    #[test]
    fn add_member_record_clones_on_each_getter_call() {
        // Confirm the getters don't move bytes out — JS callers can
        // read .commit / .welcome multiple times.
        let r = AddMember {
            commit_bytes: vec![0xAA; 4],
            welcome_bytes: vec![0xBB; 4],
        };
        assert_eq!(r.commit(), vec![0xAA; 4]);
        assert_eq!(r.commit(), vec![0xAA; 4]); // call twice → still same
        assert_eq!(r.welcome(), vec![0xBB; 4]);
    }
}
