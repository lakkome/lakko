// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for Lakko Web and Tauri clients.
//!
//! Mirrors the UniFFI surface exposed by `lakko-ffi-uniffi` (Phase
//! 0.3) so the same Rust core drives Swift / Kotlin (UniFFI) and
//! JavaScript / TypeScript (wasm-bindgen) clients with semantically
//! identical APIs. Phase 1.13a covers the same minimum-viable
//! object set as UniFFI today:
//!
//! - [`Identity`] — `new` / `signaturePublicKey` / `createKeyPackage`
//! - [`Group`] — `new` (= MLS create) / `joinFromWelcome` /
//!   `addMember` / `removeMember` / `createApplicationMessage` /
//!   `processIncoming` / `exportSecret` / `ownLeafIndex` / `epoch` /
//!   `memberCount`
//! - [`AddMember`] record (commit + welcome bytes)
//! - [`ProcessedMessage`] tagged result of `processIncoming`
//!
//! ## Naming
//!
//! Method names use camelCase via `#[wasm_bindgen(js_name = ...)]`.
//! Rust internals stay snake_case so the impl reads like normal
//! Rust; only the JS-visible name is camelCased to match TypeScript
//! conventions.
//!
//! ## What's NOT here (1.13b / 1.13c follow-ups)
//!
//! - **IndexedDB backend** for `lakko-storage` — Web's persistent
//!   store. Pending its own sub-sprint; for now `lakko-storage`
//!   on Web works only with `InMemoryStore` (state lost on page
//!   reload).
//! - **HTTP transport via `fetch`** for `lakko-transport::http`
//!   on Web. The current `reqwest`-based transport works on
//!   wasm32 via `web-sys`, but exposing it through this crate
//!   wants a separate sub-sprint.
//! - **TypeScript `.d.ts` post-processing**. wasm-bindgen produces
//!   reasonable `.d.ts` automatically; richer types (branded
//!   strings, doc-comments) come when `apps/web` (1.16) starts
//!   consuming this surface.
//! - **wasm-pack publish pipeline** — driven by `apps/web`'s build,
//!   added in 1.16.
//! - **Mirror of `lakko-contacts` / `lakko-sync` / `lakko-presence`
//!   / `lakko-push` / `lakko-storage`** — each gets its own
//!   `#[wasm_bindgen]` block when `apps/web` calls for it.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

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

    /// Public signature key bytes.
    #[wasm_bindgen(getter, js_name = signaturePublicKey)]
    pub fn signature_public_key(&self) -> Vec<u8> {
        self.inner.signature_public_key().to_vec()
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
