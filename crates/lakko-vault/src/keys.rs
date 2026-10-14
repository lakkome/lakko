// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Vault names and keys (ADR-0064 section 1, ADR-0065 section 6).
//!
//! ## Derivations
//!
//! ```text
//! vault_root  = BLAKE3-derive-key("lakko/vault/root/v1",  account_identity_seed(32))
//! vault_id    = BLAKE3-derive-key("lakko/vault/id/v1",    vault_root || ref_bytes)
//! vault_key   = BLAKE3-derive-key("lakko/vault/key/v1",   vault_root || ref_bytes)
//! vault_write = BLAKE3-derive-key("lakko/vault/write/v1", vault_root || ref_bytes)
//! blob_delete = BLAKE3-derive-key("lakko/blob/delete/v1", vault_root || blob_id(32))
//! ```
//!
//! `account_identity_seed` is the account identity's 32-byte Ed25519 seed,
//! the input `lakko_ds_proof::statement::journal_root_seed` also takes. Each
//! output is 32 bytes: `vault_id` names the vault on the storage service,
//! `vault_key` is the ChaCha20-Poly1305 key of its objects, and `vault_write`
//! and `blob_delete` are Ed25519 seeds whose public halves the server stores.
//!
//! The context strings separate the five derivations, so no output is another
//! output's input and a key of one purpose is never a key of another. Without
//! `vault_root`, two vault ids of one account are unrelated to each other and
//! to the account.
//!
//! ## `ref_bytes`
//!
//! ```text
//! conversation := 0x01 || kind(1) || id_len(1) || id(1..=64)
//! index        := 0x02
//! devices      := 0x03
//! ```
//!
//! The leading tag keeps the three shapes apart: a conversation id can never
//! spell the index or the devices vault. `vault_root` is a fixed 32 bytes, so
//! `vault_root || ref_bytes` is unambiguous, and the length byte lets the same
//! encoding sit inside a saved record ([`crate::record`]).
//!
//! The conversation kinds and the id bound are the ADR-0058 conversation
//! reference, mirrored by value from `lakko_sync::synced::entry`
//! (`ConversationRefKind::to_wire`, `CONVERSATION_REF_ID_MAX_LEN`). This
//! crate does not depend on `lakko-sync`; the test
//! `ref_kinds_and_bound_are_the_adr_0058_values` pins the numbers.
//!
//! ## Secrets
//!
//! `vault_root`, `vault_key` and the two seeds are returned in
//! [`SecretBytes`], which is zeroed on drop, compares in constant time and
//! prints nothing in `Debug`. The concatenation fed to BLAKE3 is built in a
//! [`SecretBytes`] scratch buffer for the same reason. That is best effort:
//! the compiler may leave a moved copy on the stack, and the hasher's own
//! state is not this crate's to clear.

use lakko_crypto::hash::blake3_derive_key_from_material;
use lakko_crypto::SecretBytes;
use thiserror::Error;

/// Context of the `vault_root` derivation. Frozen.
pub const VAULT_ROOT_CONTEXT: &str = "lakko/vault/root/v1";
/// Context of the `vault_id` derivation. Frozen.
pub const VAULT_ID_CONTEXT: &str = "lakko/vault/id/v1";
/// Context of the `vault_key` derivation. Frozen.
pub const VAULT_KEY_CONTEXT: &str = "lakko/vault/key/v1";
/// Context of the `vault_write` derivation. Frozen.
pub const VAULT_WRITE_CONTEXT: &str = "lakko/vault/write/v1";
/// Context of the `blob_delete` derivation (ADR-0065 section 6). Frozen.
pub const BLOB_DELETE_CONTEXT: &str = "lakko/blob/delete/v1";

/// Length of every derived value, and of the account identity seed.
pub const KEY_LEN: usize = 32;
/// Length of a vault id.
pub const VAULT_ID_LEN: usize = 32;
/// Length of a blob id (the SHA-256 of the stored ciphertext).
pub const BLOB_ID_LEN: usize = 32;

/// Longest conversation id, in bytes. `CONVERSATION_REF_ID_MAX_LEN` of
/// `lakko_sync::synced::entry`, mirrored by value.
pub const MAX_CONVERSATION_ID_LEN: usize = 64;

/// Leading byte of a conversation's `ref_bytes`.
pub const REF_TAG_CONVERSATION: u8 = 0x01;
/// The whole `ref_bytes` of the index vault.
pub const REF_TAG_INDEX: u8 = 0x02;
/// The whole `ref_bytes` of the devices vault.
pub const REF_TAG_DEVICES: u8 = 0x03;

/// Longest `ref_bytes`: tag, kind, length byte and a full id.
pub const MAX_REF_BYTES: usize = 3 + MAX_CONVERSATION_ID_LEN;

/// Longest derivation input: the root followed by the longest tail.
const MAX_MATERIAL_LEN: usize = KEY_LEN + MAX_REF_BYTES;

// A blob id is the other tail; it must fit the same scratch buffer.
const _: () = assert!(BLOB_ID_LEN <= MAX_REF_BYTES);

/// Why a conversation reference was refused. Reason codes only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum VaultRefError {
    /// The kind byte is not one of the ADR-0058 kinds.
    #[error("unknown conversation reference kind")]
    UnknownKind,
    /// The id is empty or longer than [`MAX_CONVERSATION_ID_LEN`].
    #[error("conversation reference id length is out of range")]
    IdLength,
}

impl VaultRefError {
    /// Stable, language-neutral reason code for the FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnknownKind => "unknownKind",
            Self::IdLength => "idLength",
        }
    }
}

/// Which kind of conversation a [`ConversationRef`] names.
///
/// The wire numbers are `ConversationRefKind::to_wire` of
/// `lakko_sync::synced::entry` (ADR-0058), mirrored by value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RefKind {
    /// A one-to-one conversation, by MLS group id.
    OneToOne,
    /// A group, by MLS group id.
    Group,
    /// A channel, by channel id.
    Channel,
}

impl RefKind {
    /// The wire number.
    #[must_use]
    pub const fn to_wire(self) -> u8 {
        match self {
            Self::OneToOne => 1,
            Self::Group => 2,
            Self::Channel => 3,
        }
    }

    /// Read a wire number; `0` and anything unknown is `None`.
    #[must_use]
    pub const fn from_wire(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::OneToOne),
            2 => Some(Self::Group),
            3 => Some(Self::Channel),
            _ => None,
        }
    }
}

/// A conversation, named the way ADR-0058 names it: a kind and an id of
/// `1..=MAX_CONVERSATION_ID_LEN` bytes.
///
/// The fields are private so a value of this type always satisfies the id
/// bound; build one with [`ConversationRef::new`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConversationRef {
    kind: RefKind,
    id: Vec<u8>,
}

impl ConversationRef {
    /// A reference to the conversation `id` of kind `kind`.
    ///
    /// # Errors
    ///
    /// [`VaultRefError::IdLength`] when the id is empty or too long.
    pub fn new(kind: RefKind, id: &[u8]) -> Result<Self, VaultRefError> {
        if id.is_empty() || id.len() > MAX_CONVERSATION_ID_LEN {
            return Err(VaultRefError::IdLength);
        }
        Ok(Self {
            kind,
            id: id.to_vec(),
        })
    }

    /// The same, from the kind's wire number.
    ///
    /// # Errors
    ///
    /// [`VaultRefError::UnknownKind`] or [`VaultRefError::IdLength`].
    pub fn from_wire(kind: u8, id: &[u8]) -> Result<Self, VaultRefError> {
        let kind = RefKind::from_wire(kind).ok_or(VaultRefError::UnknownKind)?;
        Self::new(kind, id)
    }

    /// Which kind of conversation this is.
    #[must_use]
    pub const fn kind(&self) -> RefKind {
        self.kind
    }

    /// The MLS group id, or the channel id.
    #[must_use]
    pub fn id(&self) -> &[u8] {
        &self.id
    }

    /// The canonical encoding: `0x01 || kind || id_len || id`.
    #[must_use]
    pub fn ref_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(3 + self.id.len());
        self.write_to(&mut out);
        out
    }

    /// Append the canonical encoding to `out`.
    pub(crate) fn write_to(&self, out: &mut Vec<u8>) {
        out.push(REF_TAG_CONVERSATION);
        out.push(self.kind.to_wire());
        // The constructor bounds the id at 64 bytes, so the length fits.
        out.push(u8::try_from(self.id.len()).unwrap_or(u8::MAX));
        out.extend_from_slice(&self.id);
    }
}

/// Which vault: one conversation's, or one of the two account-level vaults.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VaultRef {
    /// The vault of one conversation or channel.
    Conversation(ConversationRef),
    /// The vault that lists the account's conversations (ADR-0064 section 1).
    Index,
    /// The vault of the account's own device group (ADR-0064 section 5).
    Devices,
}

impl VaultRef {
    /// The canonical encoding the derivations take (see the module docs).
    #[must_use]
    pub fn ref_bytes(&self) -> Vec<u8> {
        match self {
            Self::Conversation(conversation) => conversation.ref_bytes(),
            Self::Index => vec![REF_TAG_INDEX],
            Self::Devices => vec![REF_TAG_DEVICES],
        }
    }
}

impl From<ConversationRef> for VaultRef {
    fn from(conversation: ConversationRef) -> Self {
        Self::Conversation(conversation)
    }
}

/// `BLAKE3-derive-key(context, root || tail)`.
///
/// `tail` is at most [`MAX_REF_BYTES`] bytes for every caller in this module
/// (a `ref_bytes`, or a 32-byte blob id). The copy stops at the scratch
/// buffer's end regardless, so no input can make it panic.
fn derive(context: &str, root: &SecretBytes<KEY_LEN>, tail: &[u8]) -> [u8; KEY_LEN] {
    let mut material = [0u8; MAX_MATERIAL_LEN];
    let input = root.as_bytes().iter().chain(tail.iter());
    let mut len = 0usize;
    for (slot, byte) in material.iter_mut().zip(input) {
        *slot = *byte;
        len += 1;
    }
    // Zeroed when it goes out of scope: the buffer holds the root.
    let scratch = SecretBytes::new(material);
    blake3_derive_key_from_material(context, &scratch.as_bytes()[..len])
}

/// The root every vault name and key of one account derives from.
///
/// Whoever holds it can name, read and rewrite every vault of the account
/// and delete every file the account uploaded. It never leaves the device,
/// and this type hands out only what is derived from it, never the root.
#[derive(Clone, PartialEq, Eq)]
pub struct VaultRoot(SecretBytes<KEY_LEN>);

impl core::fmt::Debug for VaultRoot {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("VaultRoot(<redacted>)")
    }
}

impl VaultRoot {
    /// Derive the root from the account identity's 32-byte Ed25519 seed.
    #[must_use]
    pub fn from_account_seed(account_identity_seed: &[u8; KEY_LEN]) -> Self {
        Self(SecretBytes::new(blake3_derive_key_from_material(
            VAULT_ROOT_CONTEXT,
            account_identity_seed,
        )))
    }

    /// The name of the vault on the storage service. Not secret: it is what
    /// the request path carries.
    #[must_use]
    pub fn vault_id(&self, vault: &VaultRef) -> [u8; VAULT_ID_LEN] {
        derive(VAULT_ID_CONTEXT, &self.0, &vault.ref_bytes())
    }

    /// The ChaCha20-Poly1305 key of the vault's objects.
    #[must_use]
    pub fn vault_key(&self, vault: &VaultRef) -> SecretBytes<KEY_LEN> {
        SecretBytes::new(derive(VAULT_KEY_CONTEXT, &self.0, &vault.ref_bytes()))
    }

    /// The Ed25519 seed that signs the vault's requests. Its public half
    /// ([`crate::proof::public_key`]) is what a claim stores.
    #[must_use]
    pub fn vault_write_seed(&self, vault: &VaultRef) -> SecretBytes<KEY_LEN> {
        SecretBytes::new(derive(VAULT_WRITE_CONTEXT, &self.0, &vault.ref_bytes()))
    }

    /// The Ed25519 seed that signs `DELETE /blob/{hex}` for one uploaded
    /// blob (ADR-0065 section 6). Its public half travels with the upload.
    #[must_use]
    pub fn blob_delete_seed(&self, blob_id: &[u8; BLOB_ID_LEN]) -> SecretBytes<KEY_LEN> {
        SecretBytes::new(derive(BLOB_DELETE_CONTEXT, &self.0, blob_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex;

    const SEED: [u8; 32] = [0x42; 32];

    fn group() -> VaultRef {
        ConversationRef::new(RefKind::Group, &[0xA7; 32])
            .unwrap()
            .into()
    }

    #[test]
    fn contexts_are_frozen() {
        assert_eq!(VAULT_ROOT_CONTEXT, "lakko/vault/root/v1");
        assert_eq!(VAULT_ID_CONTEXT, "lakko/vault/id/v1");
        assert_eq!(VAULT_KEY_CONTEXT, "lakko/vault/key/v1");
        assert_eq!(VAULT_WRITE_CONTEXT, "lakko/vault/write/v1");
        assert_eq!(BLOB_DELETE_CONTEXT, "lakko/blob/delete/v1");
    }

    #[test]
    fn ref_kinds_and_bound_are_the_adr_0058_values() {
        // `lakko_sync::synced::entry`: ConversationRefKind::to_wire and
        // CONVERSATION_REF_ID_MAX_LEN.
        assert_eq!(RefKind::OneToOne.to_wire(), 1);
        assert_eq!(RefKind::Group.to_wire(), 2);
        assert_eq!(RefKind::Channel.to_wire(), 3);
        assert_eq!(MAX_CONVERSATION_ID_LEN, 64);
        for byte in 0..=u8::MAX {
            let kind = RefKind::from_wire(byte);
            assert_eq!(kind.is_some(), (1..=3).contains(&byte));
            if let Some(kind) = kind {
                assert_eq!(kind.to_wire(), byte);
            }
        }
    }

    #[test]
    fn ref_bytes_are_pinned() {
        let one = ConversationRef::new(RefKind::OneToOne, &[0xAB, 0xCD]).unwrap();
        assert_eq!(one.ref_bytes(), [0x01, 0x01, 0x02, 0xAB, 0xCD]);
        let channel = ConversationRef::from_wire(3, &[7]).unwrap();
        assert_eq!(VaultRef::from(channel).ref_bytes(), [0x01, 0x03, 0x01, 7]);
        assert_eq!(VaultRef::Index.ref_bytes(), [0x02]);
        assert_eq!(VaultRef::Devices.ref_bytes(), [0x03]);
        let longest = ConversationRef::new(RefKind::Group, &[0; MAX_CONVERSATION_ID_LEN]).unwrap();
        assert_eq!(longest.ref_bytes().len(), MAX_REF_BYTES);
    }

    #[test]
    fn id_bounds_are_enforced() {
        assert_eq!(
            ConversationRef::new(RefKind::Group, &[]),
            Err(VaultRefError::IdLength)
        );
        assert!(ConversationRef::new(RefKind::Group, &[1]).is_ok());
        assert!(ConversationRef::new(RefKind::Group, &[1; MAX_CONVERSATION_ID_LEN]).is_ok());
        assert_eq!(
            ConversationRef::new(RefKind::Group, &[1; MAX_CONVERSATION_ID_LEN + 1]),
            Err(VaultRefError::IdLength)
        );
        assert_eq!(
            ConversationRef::from_wire(0, &[1]),
            Err(VaultRefError::UnknownKind)
        );
        assert_eq!(
            ConversationRef::from_wire(4, &[1]),
            Err(VaultRefError::UnknownKind)
        );
    }

    #[test]
    fn the_three_shapes_never_collide() {
        // An id that spells another shape's tag still differs: the
        // conversation encoding opens with its own tag.
        let root = VaultRoot::from_account_seed(&SEED);
        let mut ids = vec![
            root.vault_id(&VaultRef::Index),
            root.vault_id(&VaultRef::Devices),
        ];
        for kind in [RefKind::OneToOne, RefKind::Group, RefKind::Channel] {
            for id in [&[0x02u8][..], &[0x03], &[0x01], &[0x01, 0x02]] {
                ids.push(root.vault_id(&ConversationRef::new(kind, id).unwrap().into()));
            }
        }
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn each_derivation_is_the_documented_formula() {
        let root = VaultRoot::from_account_seed(&SEED);
        assert_eq!(
            root.0.as_bytes(),
            &blake3_derive_key_from_material("lakko/vault/root/v1", &SEED)
        );
        let vault = group();
        let mut material = root.0.as_bytes().to_vec();
        material.extend_from_slice(&[0x01, 0x02, 32]);
        material.extend_from_slice(&[0xA7; 32]);
        assert_eq!(
            root.vault_id(&vault),
            blake3_derive_key_from_material("lakko/vault/id/v1", &material)
        );
        assert_eq!(
            root.vault_key(&vault).as_bytes(),
            &blake3_derive_key_from_material("lakko/vault/key/v1", &material)
        );
        assert_eq!(
            root.vault_write_seed(&vault).as_bytes(),
            &blake3_derive_key_from_material("lakko/vault/write/v1", &material)
        );
        let blob_id = [0x5C; 32];
        let mut blob_material = root.0.as_bytes().to_vec();
        blob_material.extend_from_slice(&blob_id);
        assert_eq!(
            root.blob_delete_seed(&blob_id).as_bytes(),
            &blake3_derive_key_from_material("lakko/blob/delete/v1", &blob_material)
        );
    }

    // Pinned outputs for the fixed seed 0x42 x 32. A change to a context
    // string, to `ref_bytes` or to the concatenation must fail here, not on a
    // device that can no longer find its own vaults.
    const GOLDEN_ROOT: &str = "8ae91d57cb7cfb5ba878e91ec99c726f0737d792bc3435a0b83091a1898f844e";
    const GOLDEN_GROUP_ID: &str =
        "6f2b10ce1b302ff19bd17c5f1621aaeb114667c224db16486ffe6534e13f6925";
    const GOLDEN_GROUP_KEY: &str =
        "ae9f9071a15536347c0124d9354b992bb8acd42cf542bdf1f56ffa439a262736";
    const GOLDEN_GROUP_WRITE: &str =
        "030f9777526fd8e19bed0513fb796fd07392e133b1a1bd9833aefa4c7cb36a81";
    const GOLDEN_INDEX_ID: &str =
        "a6b4d86822faebbb66410078b4c3c19d75481d6f6fb5136eb77f484971265b8c";
    const GOLDEN_INDEX_KEY: &str =
        "035160d083ae72a42da1401cf14e829d75a886f683da6249206f259e0b9cec49";
    const GOLDEN_INDEX_WRITE: &str =
        "d62749601c6771cdafdc957b6b359990a6f378c1f3403733f41f8df28a058d74";
    const GOLDEN_DEVICES_ID: &str =
        "54a70c1643f2f7b8c2207155210ba1b2f9617123575a2d117a9a7d577e21aaad";
    const GOLDEN_DEVICES_KEY: &str =
        "ba3720b76c43bbe34465d4f6dc86c9df5c229ff02647cc1bceaacf64c0ce9a31";
    const GOLDEN_DEVICES_WRITE: &str =
        "3885da581f778dacc4bcbc85737f0a56d0d79e55b7401ae2dce645b1cfea8640";
    const GOLDEN_BLOB_DELETE: &str =
        "ba8733d1482c61f6a6a64f27e7fb4fadf801a1c1efc6692bbf26410301df3036";

    #[test]
    fn derivations_are_pinned() {
        let root = VaultRoot::from_account_seed(&SEED);
        assert_eq!(hex(root.0.as_bytes()), GOLDEN_ROOT);

        let vault = group();
        assert_eq!(hex(&root.vault_id(&vault)), GOLDEN_GROUP_ID);
        assert_eq!(hex(root.vault_key(&vault).as_bytes()), GOLDEN_GROUP_KEY);
        assert_eq!(
            hex(root.vault_write_seed(&vault).as_bytes()),
            GOLDEN_GROUP_WRITE
        );

        assert_eq!(hex(&root.vault_id(&VaultRef::Index)), GOLDEN_INDEX_ID);
        assert_eq!(
            hex(root.vault_key(&VaultRef::Index).as_bytes()),
            GOLDEN_INDEX_KEY
        );
        assert_eq!(
            hex(root.vault_write_seed(&VaultRef::Index).as_bytes()),
            GOLDEN_INDEX_WRITE
        );

        assert_eq!(hex(&root.vault_id(&VaultRef::Devices)), GOLDEN_DEVICES_ID);
        assert_eq!(
            hex(root.vault_key(&VaultRef::Devices).as_bytes()),
            GOLDEN_DEVICES_KEY
        );
        assert_eq!(
            hex(root.vault_write_seed(&VaultRef::Devices).as_bytes()),
            GOLDEN_DEVICES_WRITE
        );

        assert_eq!(
            hex(root.blob_delete_seed(&[0x5C; 32]).as_bytes()),
            GOLDEN_BLOB_DELETE
        );
    }

    #[test]
    fn outputs_of_one_vault_are_pairwise_distinct() {
        let root = VaultRoot::from_account_seed(&SEED);
        let vault = group();
        let id = root.vault_id(&vault);
        let key = *root.vault_key(&vault).as_bytes();
        let write = *root.vault_write_seed(&vault).as_bytes();
        // A blob id equal to nothing in particular; the context alone
        // separates it from the vault derivations.
        let delete = *root.blob_delete_seed(&[0xA7; 32]).as_bytes();
        let all = [*root.0.as_bytes(), id, key, write, delete];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn another_account_gets_unrelated_vaults() {
        let a = VaultRoot::from_account_seed(&SEED);
        let b = VaultRoot::from_account_seed(&[0x43; 32]);
        assert_ne!(a.vault_id(&group()), b.vault_id(&group()));
        assert_ne!(
            a.blob_delete_seed(&[1; 32]).as_bytes(),
            b.blob_delete_seed(&[1; 32]).as_bytes()
        );
    }

    #[test]
    fn debug_does_not_print_the_root() {
        let root = VaultRoot::from_account_seed(&SEED);
        let shown = format!("{root:?}");
        assert_eq!(shown, "VaultRoot(<redacted>)");
        assert!(!shown.contains(&hex(root.0.as_bytes())[..8]));
    }
}
