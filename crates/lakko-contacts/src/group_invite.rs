// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! [`GroupInviteCapability`] — link-based group join-ticket (ADR-0028, G-3).
//!
//! The inverse of the admin-adds-by-username flow: an owner/admin mints a
//! signed link that conveys a group's `group_id` to a **non-member**, who then
//! crafts an MLS external `Add` proposal
//! ([`lakko_mls::Identity::create_external_join_proposal`], ADR-0027) and posts
//! it to the group journal for the admin to approve.
//!
//! Sibling of [`crate::call_invite::CallInviteCapability`] (ADR-0009) and
//! [`crate::invite::InviteCapability`] (ADR-0005): the same single-file manual
//! codec + Ed25519-over-the-prefix signature, but a leaner payload — no
//! embedded KeyPackage, no media flags, no account id. The joiner mints its
//! **own** KeyPackage inside `create_external_join_proposal`, and learns the
//! group's current epoch from the journal's public MLS framing (so the ticket
//! carries no epoch — see ADR-0028 §3).
//!
//! ## What the link carries (and does NOT carry)
//!
//! - `group_id` — the target group handle the joiner passes to
//!   `create_external_join_proposal`. Variable-length opaque MLS `GroupId`
//!   bytes (Lakko's `Group::group_id()` is a `Vec<u8>`), length-prefixed here.
//! - `issuer_signature_pubkey` — the owner/admin Ed25519 key that minted the
//!   link. Anti-tamper (the signature binds the `group_id`) + lets the admin
//!   UI surface *which* member's link a pending request arrived on, and lets
//!   the joiner render an anti-phishing fingerprint.
//! - `expiry_ms` — UNIX millis; [`NEVER_EXPIRES`] (= 0) = no expiry.
//!
//! **No epoch** (read from the journal at request time) and **no
//! authorization secret** — the security boundary is the admin's approval gate
//! (ADR-0027 role-gated, peer-verified), not the ticket. A leaked link lets a
//! stranger *ask* to join; it never lets them *in*.
//!
//! ## The preview (v2, 2026-09-26)
//!
//! A person opening a link used to see the issuer's key fingerprint and
//! nothing else, which tells them nothing about what they are joining. The
//! capability now carries an optional **sealed preview** - the group name, the
//! inviter's display name and the member count at mint time - encrypted under
//! a key derived from a random 12-byte **link secret** that travels in the URL
//! **fragment** (`https://lakko.me/g/<slug>#<secret>`, 16 base64url characters;
//! see [`preview_key`] for the derivation). A fragment never leaves the browser or
//! the app, so neither the discovery ledger (which stores pointer-link
//! capabilities) nor an access log ever sees the names; the server side stays
//! ciphertext-only (invariants #1, #2, #5). Whoever holds the whole link can
//! read the preview - that is the point of a link one shares. The preview is
//! inside the signature, so it cannot be swapped under a genuine capability;
//! it is still the ISSUER's claim (the inviter name is self-asserted like a
//! profile name), which is why the fingerprint stays available as the
//! out-of-band check.
//!
//! ## The sealed link (2026-09-28 security pass)
//!
//! The whole signed capability now travels sealed under the same link secret
//! ([`crate::link_seal`], [`seal_link`] / [`open_link`]): the discovery
//! ledger stores the sealed blob only, a link is `/g/<id>#<secret>` (the inline
//! form was removed on 2026-10-19), and a link without its
//! fragment is damaged rather than accepting whatever capability the ledger
//! returns. The preview stays sealed separately INSIDE the capability, not
//! folded into its plaintext, because the plaintext capability travels
//! beyond the link: the joiner's `group_join_attribution` carries it to the
//! group's knock journal on the Delivery Service, where it is not encrypted to
//! anyone, and the names must not appear there.
//!
//! ## The knock key (v3, ADR-0087 section 5)
//!
//! A request to join is a sealed knock row (`crate::knock`), sealed to the
//! group's versioned knock key, so the capability names that key:
//! `knock_flag(1) ‖ knock_pk(32) ‖ knock_version(2, BE)` when the flag is
//! `1`, nothing more when it is `0`. A group link and a co-publisher link
//! carry one; a channel subscriber invitation (preview kind 3) carries none,
//! because a subscriber never knocks ([`compose_with_preview`] enforces both
//! ways). The capability is sealed under the link secret on the way to the
//! joiner, and inside the sealed knock row on the way back, so the key never
//! sits in a clear row.
//!
//! ## Wire format (manual codec, v3)
//!
//! ```text
//! u8           schema version (= 3)
//! u16be        group_id_len                       — bounded by MAX_GROUP_ID_LEN
//! [gid_len]    group_id                            — opaque MLS GroupId bytes
//! [32 bytes]   issuer_signature_pubkey             — Ed25519 verifying key
//! u64be        expiry_ms                           — UNIX millis; 0 = no expiry
//! u8           knock_flag                          — 0 = no knock key, 1 = one follows
//! [32 bytes]   knock_public_key                    — only when knock_flag = 1
//! u16be        knock_version                       — only when knock_flag = 1
//! u16be        preview_len                         — 0 = no preview; else <= MAX_SEALED_PREVIEW_LEN
//! [preview]    sealed preview                      — nonce(12) ‖ ChaCha20-Poly1305 over the plaintext below,
//!                                                    under preview_key(link secret, group_id, issuer)
//! [64 bytes]   signature                           — Ed25519 over the above
//! ```
//!
//! Preview plaintext (inside the AEAD), one of three kinds told apart by the
//! first byte:
//!
//! ```text
//! u8   kind (= 1, a group)
//! u8   group_name_len  ‖ group_name    — UTF-8, the display-name rule, 0 = none
//! u8   inviter_len     ‖ inviter_name  — UTF-8, the display-name rule, 0 = none
//! u16be member_count
//! u8   avatar_len      ‖ avatar        — crate::avatar, 0 | 84 | 170
//!
//! u8   kind (= 2, a channel's co-publisher link, ADR-0078 section 6)
//! [16] channel_id
//! u8   channel_name_len ‖ channel_name — UTF-8, the display-name rule, 0 = none
//! u8   avatar_len      ‖ avatar        — crate::avatar, 0 | 84 | 170
//!
//! u8   kind (= 3, a channel subscriber invitation, ADR-0087 sections 7 and 14)
//! [16] channel_id
//! u8   access (1 private, 2 public)
//! private: [32] invite_seed ‖ [32] admission_public_key ‖ u16be admission_version
//! public:  [32] read_key
//! u8   channel_name_len ‖ channel_name — the channel-name rule, 1..=255 bytes
//! u16be description_len ‖ description  — the channel-description rule, 0..=1024
//! u8   avatar_len      ‖ avatar        — crate::avatar, 0 | 84 | 170
//! ```
//!
//! The trailing `avatar` (ADR-0087 section 18.4) is the photo of the group or
//! channel the link was made with, in the codec a person's profile uses: the
//! invite page shows it, and a link holder can fetch it, which is what holding
//! the link already allows for the name. An avatar outside its bounds refuses
//! the whole preview.
//!
//! A subscriber invitation's capability names the channel id as its
//! `group_id` (16 bytes): a subscriber never joins the PublisherGroup, so its
//! id has no business in the link. [`compose_with_preview`] and
//! [`GroupInviteCapability::open_preview`] both refuse a kind-3 preview whose
//! channel id is not the capability's `group_id`.
//!
//! - **`invite_seed`** derives the link's invite key at the channels service
//!   (`lakko_channels::invite`), with which the subscriber signs the invite
//!   proof and the admission ticket.
//! - **`admission_public_key`** is the X25519 key the ticket is sealed to,
//!   of version **`admission_version`**: a random seed the owner mints and
//!   hands to every co-publisher (`crate::admission_seed`), re-minted at
//!   version + 1 when a co-publisher leaves. The ticket names the version so
//!   a publisher picks the seed.
//! - **`read_key`** opens a public channel's seed envelopes, and needs no
//!   invite key (section 14).
//!
//! A co-publisher link carries the channel id here, never in the capability:
//! since ADR-0078 the PublisherGroup id is derived from the owner's seed, so
//! nothing inverts it, and the plaintext capability rides the join
//! attribution to the knock journal in the clear, where a channel id next to
//! the group id would name the channel's journal to the Delivery Service.
//!
//! Multi-byte integers are big-endian. The signature covers everything from
//! the schema-version byte through the sealed preview inclusive. v1 (no
//! preview field) and v2 (no knock key) are refused: there is no deployment
//! to stay compatible with.

use lakko_crypto::aead::{
    open_with_key, seal_with_key, KEY_LEN as AEAD_KEY_LEN, NONCE_LEN, TAG_LEN,
};
use lakko_crypto::kdf::hkdf_sha256;
use lakko_crypto::secret::SecretBytes;
use lakko_crypto::signing::{
    Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN as ED25519_PUB_LEN, SIGNATURE_LEN,
};

use crate::avatar::{decode_avatar_field, encode_avatar_field, Avatar, AVATAR_FIELD_MAX_LEN};
use crate::channel_text::{check_description, check_name, DESCRIPTION_MAX_BYTES, NAME_MAX_BYTES};
use crate::error::{ContactError, Result};
use crate::invite::{BLOB_ID_LEN, NEVER_EXPIRES};
use crate::knock::KNOCK_KEY_LEN;
use crate::profile::check_display_name;

/// Current schema version on the wire.
const SCHEMA_VERSION: u8 = 3;

/// The group's knock key a link names (ADR-0087 section 5): the X25519
/// public key of `knock_seed(v)` and its version `v`. A joiner seals its
/// knock row to it ([`crate::knock::seal_knock`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupKnockKey {
    /// `crate::knock::knock_public_key(knock_seed(journal_root_seed, version))`.
    pub public_key: [u8; KNOCK_KEY_LEN],
    /// The knock-key version the public key belongs to.
    pub version: u16,
}

/// Bytes the knock key takes on the wire when present (key + version).
const KNOCK_FIELD_LEN: usize = KNOCK_KEY_LEN + 2;

/// The link secret: 12 bytes (16 base64url characters) in the URL fragment,
/// never on a server. Since the 2026-09-28 security pass one secret opens
/// both layers of a `/g/` link — the sealed capability
/// ([`crate::link_seal`], [`open_link`]) and, inside it, the sealed preview
/// ([`GroupInviteCapability::open_preview`]) — under two independent HKDF
/// derivations with distinct `info`. 96 bits of CSPRNG output, the
/// maintainer's length choice for a link of about 45 characters (the
/// short-link amendment of ADR-0028, 2026-09-27).
pub use crate::link_seal::{LINK_SECRET_B64URL_LEN, LINK_SECRET_LEN};

/// HKDF-SHA256 `info` domain for the preview key (see [`preview_key`]).
pub const PREVIEW_KEY_DOMAIN_V1: &[u8] = b"lakko/group-invite/preview-key/v1\x00";

/// Preview plaintext kind byte of a group preview.
const PREVIEW_VERSION: u8 = 1;

/// Preview plaintext kind byte of a channel co-publisher preview (ADR-0078
/// section 6): it carries the channel id.
const PREVIEW_KIND_CHANNEL: u8 = 2;

/// Preview plaintext kind byte of a channel subscriber invitation (ADR-0087
/// sections 7 and 14).
const PREVIEW_KIND_SUBSCRIBER: u8 = 3;

/// Access byte of a private subscriber invitation.
const ACCESS_PRIVATE: u8 = 1;

/// Access byte of a public subscriber invitation.
const ACCESS_PUBLIC: u8 = 2;

/// Length of an invite seed, an admission public key and a read key.
pub const SUBSCRIBER_KEY_LEN: usize = 32;

/// Length of the channel id a channel preview carries (ADR-0010).
pub const PREVIEW_CHANNEL_ID_LEN: usize = 16;

/// Longest name the preview carries, in bytes (the one-byte length prefix;
/// the display-name rule caps a name at 255 bytes anyway).
pub const MAX_PREVIEW_NAME_BYTES: usize = 255;

/// Longest group preview plaintext: kind, two length-prefixed names, the
/// count and a photo with a video.
const MAX_GROUP_PREVIEW_PLAINTEXT_LEN: usize =
    1 + 1 + MAX_PREVIEW_NAME_BYTES + 1 + MAX_PREVIEW_NAME_BYTES + 2 + AVATAR_FIELD_MAX_LEN;

/// Longest subscriber-invitation plaintext: kind, channel id, access, two
/// keys (private), the name, the description and a photo with a video.
const MAX_SUBSCRIBER_PREVIEW_PLAINTEXT_LEN: usize = 1
    + PREVIEW_CHANNEL_ID_LEN
    + 1
    + 2 * SUBSCRIBER_KEY_LEN
    + 2
    + 1
    + NAME_MAX_BYTES
    + 2
    + DESCRIPTION_MAX_BYTES
    + AVATAR_FIELD_MAX_LEN;

/// Longest preview plaintext of any kind.
const MAX_PREVIEW_PLAINTEXT_LEN: usize =
    if MAX_SUBSCRIBER_PREVIEW_PLAINTEXT_LEN > MAX_GROUP_PREVIEW_PLAINTEXT_LEN {
        MAX_SUBSCRIBER_PREVIEW_PLAINTEXT_LEN
    } else {
        MAX_GROUP_PREVIEW_PLAINTEXT_LEN
    };

// A co-publisher preview (kind, channel id, one name, a photo) fits inside
// the group preview's bound.
const _: () = assert!(
    1 + PREVIEW_CHANNEL_ID_LEN + 1 + MAX_PREVIEW_NAME_BYTES + AVATAR_FIELD_MAX_LEN
        <= MAX_GROUP_PREVIEW_PLAINTEXT_LEN
);

/// Longest sealed preview on the wire (nonce + ciphertext + tag). A link that
/// claims more is refused before any allocation.
pub const MAX_SEALED_PREVIEW_LEN: usize = NONCE_LEN + MAX_PREVIEW_PLAINTEXT_LEN + TAG_LEN;

/// Hard cap on the embedded `group_id` bytes. An MLS `GroupId` is opaque
/// variable-length; Lakko's are short (16–32 bytes in practice), but the codec
/// bounds at 256 to keep URL lengths sane and parse-time work bounded without
/// pinning an exact upstream upper bound.
pub const MAX_GROUP_ID_LEN: usize = 256;

/// Re-export so callers that only `use crate::group_invite::*` get a coherent
/// surface. `GROUP_INVITE_NEVER_EXPIRES` (= 0) means no expiry.
pub use crate::invite::NEVER_EXPIRES as GROUP_INVITE_NEVER_EXPIRES;

// ============================================================================
// GroupInviteCapability — wire shape
// ============================================================================

/// Capability granted by a group owner/admin to a prospective joiner. Decoded
/// and verified from the base64url-encoded slug at the end of the
/// `https://lakko.me/g/<slug>` URL (ADR-0028 §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupInviteCapability {
    /// The target group's opaque MLS `GroupId` bytes — what the joiner passes
    /// to `Identity::create_external_join_proposal`.
    pub group_id: Vec<u8>,
    /// The issuer's long-term Ed25519 verifying key. The signature over the
    /// canonical bytes is verified against this; the admin UI can additionally
    /// check it is a current group member before approving (a forged link
    /// names a non-member issuer).
    pub issuer_signature_pubkey: VerifyingKey,
    /// UNIX milliseconds when the link stops being valid. [`NEVER_EXPIRES`]
    /// (= 0) means no expiry — UI SHOULD surface a non-expiring link as a
    /// standing-exposure note.
    pub expiry_ms: u64,
    /// The group's knock key the joiner seals its request to (ADR-0087
    /// section 5). `None` only on a channel subscriber invitation.
    pub knock: Option<GroupKnockKey>,
    /// The sealed preview (empty = the link carries none). Opaque until
    /// [`GroupInviteCapability::open_preview`] is given the link key.
    pub sealed_preview: Vec<u8>,
}

/// How a subscriber invitation lets its holder read the channel (ADR-0087
/// sections 6, 7 and 14).
#[derive(Clone, PartialEq, Eq)]
pub enum ChannelAccess {
    /// A private channel: the holder proves the invitation to the channels
    /// service with the invite key derived from `invite_seed` and seals an
    /// admission ticket to `admission_public_key`.
    Private {
        /// The link's invite seed (SECRET: whoever holds it can subscribe).
        invite_seed: [u8; SUBSCRIBER_KEY_LEN],
        /// The channel's X25519 admission public key.
        admission_public_key: [u8; SUBSCRIBER_KEY_LEN],
        /// The version of that admission key.
        admission_version: u16,
    },
    /// A public channel: the read key that opens its public seed envelopes.
    Public {
        /// The channel's read key (public by the owner's choice).
        read_key: [u8; SUBSCRIBER_KEY_LEN],
    },
}

impl core::fmt::Debug for ChannelAccess {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Private {
                admission_public_key,
                admission_version,
                ..
            } => f
                .debug_struct("Private")
                .field("admission_public_key", admission_public_key)
                .field("admission_version", admission_version)
                .finish_non_exhaustive(),
            Self::Public { .. } => f.debug_struct("Public").finish_non_exhaustive(),
        }
    }
}

/// What a subscriber invitation carries beyond the channel id and name
/// (preview kind 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriberInvite {
    /// Private or public, with the keys each needs.
    pub access: ChannelAccess,
    /// The channel description as the inviter's device holds it; may be
    /// empty. The channel-description rule.
    pub description: String,
}

/// What a link shows the person before they ask to join: the issuer's claim
/// about the group at mint time, sealed under the link key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupInvitePreview {
    /// The group's name, or `None` when it had none.
    pub group_name: Option<String>,
    /// The inviter's display name, or `None` when they have none.
    pub inviter_name: Option<String>,
    /// How many people were in the group when the link was minted.
    pub member_count: u16,
    /// ADR-0078 section 6: the channel a co-publisher link is for. When set,
    /// `group_name` is the channel's name, and `inviter_name` must be `None`
    /// and `member_count` 0 (a channel preview carries neither).
    pub channel_id: Option<[u8; PREVIEW_CHANNEL_ID_LEN]>,
    /// ADR-0087 section 7: set on a channel subscriber invitation (preview
    /// kind 3). Then `channel_id` is set, `group_name` is the channel's name
    /// (required, the channel-name rule), `inviter_name` is `None` and
    /// `member_count` 0.
    pub subscriber: Option<SubscriberInvite>,
    /// ADR-0087 section 18.4: the photo of the group or channel when the
    /// link was made, or `None`. Its keys are readable by whoever holds the
    /// link.
    pub avatar: Option<Avatar>,
}

impl GroupInvitePreview {
    /// Refuse a preview that the decoder would refuse, before sealing it.
    fn validate(&self) -> Result<()> {
        if let Some(avatar) = &self.avatar {
            avatar.validate().map_err(|e| {
                ContactError::InvalidArgument(format!("group-invite preview photo: {}", e.code()))
            })?;
        }
        if self.channel_id.is_some() && (self.inviter_name.is_some() || self.member_count != 0) {
            return Err(ContactError::InvalidArgument(
                "a channel group-invite preview carries no inviter name and no member count".into(),
            ));
        }
        if let Some(sub) = &self.subscriber {
            if self.channel_id.is_none() {
                return Err(ContactError::InvalidArgument(
                    "a subscriber invitation names its channel".into(),
                ));
            }
            let name = self.group_name.as_deref().unwrap_or("");
            check_name(name).map_err(|e| {
                ContactError::InvalidArgument(format!("subscriber invitation name: {}", e.code()))
            })?;
            check_description(&sub.description).map_err(|e| {
                ContactError::InvalidArgument(format!(
                    "subscriber invitation description: {}",
                    e.code()
                ))
            })?;
            return Ok(());
        }
        for (what, name) in [
            ("group name", &self.group_name),
            ("inviter name", &self.inviter_name),
        ] {
            if let Some(n) = name {
                if n.is_empty() || n.len() > MAX_PREVIEW_NAME_BYTES {
                    return Err(ContactError::InvalidArgument(format!(
                        "group-invite preview {what} must be 1..={MAX_PREVIEW_NAME_BYTES} bytes",
                    )));
                }
                check_display_name(n).map_err(|e| {
                    ContactError::InvalidArgument(format!("group-invite preview {what}: {e}"))
                })?;
            }
        }
        Ok(())
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(MAX_PREVIEW_PLAINTEXT_LEN);
        if let (Some(channel_id), Some(sub)) = (&self.channel_id, &self.subscriber) {
            out.push(PREVIEW_KIND_SUBSCRIBER);
            out.extend_from_slice(channel_id);
            match &sub.access {
                ChannelAccess::Private {
                    invite_seed,
                    admission_public_key,
                    admission_version,
                } => {
                    out.push(ACCESS_PRIVATE);
                    out.extend_from_slice(invite_seed);
                    out.extend_from_slice(admission_public_key);
                    out.extend_from_slice(&admission_version.to_be_bytes());
                },
                ChannelAccess::Public { read_key } => {
                    out.push(ACCESS_PUBLIC);
                    out.extend_from_slice(read_key);
                },
            }
            // `validate` bounded both; a wrong caller saturates, never overflows.
            let name = self.group_name.as_deref().map_or(&[][..], str::as_bytes);
            let name_len = u8::try_from(name.len()).unwrap_or(u8::MAX);
            out.push(name_len);
            out.extend_from_slice(&name[..usize::from(name_len)]);
            let description = sub.description.as_bytes();
            let description_len = u16::try_from(description.len()).unwrap_or(u16::MAX);
            out.extend_from_slice(&description_len.to_be_bytes());
            out.extend_from_slice(&description[..usize::from(description_len)]);
            encode_avatar_field(self.avatar.as_ref(), &mut out);
            return out;
        }
        if let Some(channel_id) = &self.channel_id {
            out.push(PREVIEW_KIND_CHANNEL);
            out.extend_from_slice(channel_id);
            let bytes = self.group_name.as_deref().map_or(&[][..], str::as_bytes);
            let len = u8::try_from(bytes.len()).unwrap_or(u8::MAX);
            out.push(len);
            out.extend_from_slice(&bytes[..usize::from(len)]);
            encode_avatar_field(self.avatar.as_ref(), &mut out);
            return out;
        }
        out.push(PREVIEW_VERSION);
        for name in [&self.group_name, &self.inviter_name] {
            let bytes = name.as_deref().map_or(&[][..], str::as_bytes);
            // `validate` bounded it; a wrong caller still cannot overflow the byte.
            let len = u8::try_from(bytes.len()).unwrap_or(u8::MAX);
            out.push(len);
            out.extend_from_slice(&bytes[..usize::from(len)]);
        }
        out.extend_from_slice(&self.member_count.to_be_bytes());
        encode_avatar_field(self.avatar.as_ref(), &mut out);
        out
    }

    fn decode(plain: &[u8]) -> Result<Self> {
        let mut cur = plain;
        let version = take_u8(&mut cur)?;
        if version == PREVIEW_KIND_CHANNEL {
            return Self::decode_channel(cur);
        }
        if version == PREVIEW_KIND_SUBSCRIBER {
            return Self::decode_subscriber(cur);
        }
        if version != PREVIEW_VERSION {
            return Err(ContactError::InvalidEncoding(format!(
                "unsupported group-invite preview version {version}",
            )));
        }
        let mut names: [Option<String>; 2] = [None, None];
        for slot in &mut names {
            let len = usize::from(take_u8(&mut cur)?);
            if len == 0 {
                continue;
            }
            let raw = take_n(&mut cur, len)?;
            let text = core::str::from_utf8(raw).map_err(|_| {
                ContactError::InvalidEncoding("group-invite preview name is not UTF-8".into())
            })?;
            check_display_name(text).map_err(|e| {
                ContactError::InvalidEncoding(format!("group-invite preview name refused: {e}"))
            })?;
            *slot = Some(text.to_string());
        }
        let count = take_n(&mut cur, 2)?;
        let avatar = take_avatar(&mut cur)?;
        if !cur.is_empty() {
            return Err(ContactError::InvalidEncoding(format!(
                "trailing {} bytes after group-invite preview",
                cur.len(),
            )));
        }
        let [group_name, inviter_name] = names;
        Ok(Self {
            group_name,
            inviter_name,
            member_count: u16::from_be_bytes([count[0], count[1]]),
            channel_id: None,
            subscriber: None,
            avatar,
        })
    }

    /// The rest of a channel preview, after its kind byte.
    fn decode_channel(mut cur: &[u8]) -> Result<Self> {
        let raw_id = take_n(&mut cur, PREVIEW_CHANNEL_ID_LEN)?;
        let mut channel_id = [0u8; PREVIEW_CHANNEL_ID_LEN];
        channel_id.copy_from_slice(raw_id);
        let len = usize::from(take_u8(&mut cur)?);
        let group_name = if len == 0 {
            None
        } else {
            let raw = take_n(&mut cur, len)?;
            let text = core::str::from_utf8(raw).map_err(|_| {
                ContactError::InvalidEncoding("group-invite preview name is not UTF-8".into())
            })?;
            check_display_name(text).map_err(|e| {
                ContactError::InvalidEncoding(format!("group-invite preview name refused: {e}"))
            })?;
            Some(text.to_string())
        };
        let avatar = take_avatar(&mut cur)?;
        if !cur.is_empty() {
            return Err(ContactError::InvalidEncoding(format!(
                "trailing {} bytes after group-invite preview",
                cur.len(),
            )));
        }
        Ok(Self {
            group_name,
            inviter_name: None,
            member_count: 0,
            channel_id: Some(channel_id),
            subscriber: None,
            avatar,
        })
    }

    /// The rest of a subscriber invitation, after its kind byte.
    fn decode_subscriber(mut cur: &[u8]) -> Result<Self> {
        let mut channel_id = [0u8; PREVIEW_CHANNEL_ID_LEN];
        channel_id.copy_from_slice(take_n(&mut cur, PREVIEW_CHANNEL_ID_LEN)?);
        let access = match take_u8(&mut cur)? {
            ACCESS_PRIVATE => {
                let mut invite_seed = [0u8; SUBSCRIBER_KEY_LEN];
                invite_seed.copy_from_slice(take_n(&mut cur, SUBSCRIBER_KEY_LEN)?);
                let mut admission_public_key = [0u8; SUBSCRIBER_KEY_LEN];
                admission_public_key.copy_from_slice(take_n(&mut cur, SUBSCRIBER_KEY_LEN)?);
                let version = take_n(&mut cur, 2)?;
                ChannelAccess::Private {
                    invite_seed,
                    admission_public_key,
                    admission_version: u16::from_be_bytes([version[0], version[1]]),
                }
            },
            ACCESS_PUBLIC => {
                let mut read_key = [0u8; SUBSCRIBER_KEY_LEN];
                read_key.copy_from_slice(take_n(&mut cur, SUBSCRIBER_KEY_LEN)?);
                ChannelAccess::Public { read_key }
            },
            other => {
                return Err(ContactError::InvalidEncoding(format!(
                    "unknown subscriber invitation access {other}",
                )))
            },
        };
        let name_len = usize::from(take_u8(&mut cur)?);
        let name = core::str::from_utf8(take_n(&mut cur, name_len)?).map_err(|_| {
            ContactError::InvalidEncoding("subscriber invitation name is not UTF-8".into())
        })?;
        check_name(name).map_err(|e| {
            ContactError::InvalidEncoding(format!("subscriber invitation name: {}", e.code()))
        })?;
        let description_len = take_n(&mut cur, 2)?;
        let description_len =
            usize::from(u16::from_be_bytes([description_len[0], description_len[1]]));
        let description =
            core::str::from_utf8(take_n(&mut cur, description_len)?).map_err(|_| {
                ContactError::InvalidEncoding(
                    "subscriber invitation description is not UTF-8".into(),
                )
            })?;
        check_description(description).map_err(|e| {
            ContactError::InvalidEncoding(format!(
                "subscriber invitation description: {}",
                e.code()
            ))
        })?;
        let avatar = take_avatar(&mut cur)?;
        if !cur.is_empty() {
            return Err(ContactError::InvalidEncoding(format!(
                "trailing {} bytes after group-invite preview",
                cur.len(),
            )));
        }
        Ok(Self {
            group_name: Some(name.to_string()),
            inviter_name: None,
            member_count: 0,
            channel_id: Some(channel_id),
            subscriber: Some(SubscriberInvite {
                access,
                description: description.to_string(),
            }),
            avatar,
        })
    }
}

/// Read the preview's trailing photo field, bounds included (ADR-0087
/// section 18.4).
fn take_avatar(cur: &mut &[u8]) -> Result<Option<Avatar>> {
    decode_avatar_field(cur).map_err(|e| {
        ContactError::InvalidEncoding(format!("group-invite preview photo: {}", e.code()))
    })
}

/// Mint a fresh link secret (the URL-fragment value). One per link, never
/// reused; it seals the capability and derives the preview key.
#[must_use]
pub fn generate_link_key() -> [u8; LINK_SECRET_LEN] {
    crate::link_seal::generate_link_secret()
}

/// Longest capability wire the codec can produce: schema version (1) +
/// `group_id_len` (2) + a [`MAX_GROUP_ID_LEN`] group id + the issuer key +
/// `expiry_ms` (8) + the knock flag (1) and key (34) + `preview_len` (2) + a
/// [`MAX_SEALED_PREVIEW_LEN`] sealed preview + the signature (1965 bytes; an
/// upper bound, since only a subscriber invitation, whose `group_id` is a
/// 16-byte channel id, carries the largest preview). The
/// sealed knock row and the `/g/` join attribution carry the plaintext
/// capability, so this bounds them too.
pub const MAX_WIRE_LEN: usize = 1
    + 2
    + MAX_GROUP_ID_LEN
    + ED25519_PUB_LEN
    + 8
    + 1
    + KNOCK_FIELD_LEN
    + 2
    + MAX_SEALED_PREVIEW_LEN
    + SIGNATURE_LEN;

/// Longest sealed `/g/` link blob (what the discovery ledger stores).
pub const MAX_SEALED_LEN: usize = crate::link_seal::sealed_len(MAX_WIRE_LEN);

/// Seal a composed capability wire for a `/g/` link under `link_secret` —
/// the same secret the preview (if any) was sealed under.
///
/// # Errors
///
/// [`ContactError::InvalidArgument`] for an empty wire; a crypto failure.
pub fn seal_link(link_secret: &[u8; LINK_SECRET_LEN], capability_wire: &[u8]) -> Result<Vec<u8>> {
    crate::link_seal::seal(
        crate::link_seal::LinkKind::Group,
        link_secret,
        capability_wire,
    )
}

/// Open a sealed `/g/` link blob under the fragment's secret and verify the
/// capability inside. Returns the capability and its plaintext wire (the
/// joiner tag and the join attribution are computed over the plaintext). A
/// `/g/` link without its fragment has nothing to open this with, and is
/// damaged.
///
/// # Errors
///
/// [`ContactError::LinkDamaged`] on any failure.
pub fn open_link(
    link_secret: &[u8; LINK_SECRET_LEN],
    sealed: &[u8],
) -> Result<(GroupInviteCapability, Vec<u8>)> {
    crate::link_seal::open_and_verify(
        crate::link_seal::LinkKind::Group,
        link_secret,
        sealed,
        decode_and_verify,
    )
}

// ============================================================================
// The channel invitation card (ADR-0087 section 14)
// ============================================================================

/// Version byte of a [`ChannelInviteCard`].
pub const CHANNEL_INVITE_CARD_VERSION_V1: u8 = 0x01;

/// Shortest card: version, ledger id, secret and the smallest sealed blob a
/// capability fits in.
pub const CHANNEL_INVITE_CARD_MIN_LEN: usize =
    1 + BLOB_ID_LEN + LINK_SECRET_LEN + crate::link_seal::sealed_len(1);

/// Longest card: version, ledger id, secret and the longest sealed `/g/`
/// blob. The chat envelope's `channel_invite` field
/// (`lakko_attachments::MAX_CHANNEL_INVITE_BYTES`) is at least this.
pub const CHANNEL_INVITE_CARD_MAX_LEN: usize = 1 + BLOB_ID_LEN + LINK_SECRET_LEN + MAX_SEALED_LEN;

/// The invitation a publisher sends a contact inside their one-to-one: the
/// whole `/g/` link (its ledger id and its secret) and the sealed capability
/// the ledger holds, so the card renders with no fetch. It rides the chat
/// envelope's `channel_invite` field, end-to-end encrypted; `Subscribe`
/// consumes the ledger row by `ledger_id` as any link does.
///
/// ```text
/// version(1)=0x01 ‖ ledger_id(6) ‖ link_secret(12) ‖ sealed /g/ blob
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct ChannelInviteCard {
    /// The discovery ledger id of the link.
    pub ledger_id: [u8; BLOB_ID_LEN],
    /// The link secret (SECRET: it opens the capability and the preview).
    pub link_secret: [u8; LINK_SECRET_LEN],
    /// The sealed `/g/` blob, as [`seal_link`] produced it.
    pub sealed: Vec<u8>,
}

impl core::fmt::Debug for ChannelInviteCard {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ChannelInviteCard")
            .field("ledger_id", &self.ledger_id)
            .field("sealed_len", &self.sealed.len())
            .finish_non_exhaustive()
    }
}

impl ChannelInviteCard {
    /// Encode the card.
    ///
    /// # Errors
    ///
    /// [`ContactError::InvalidArgument`] when the sealed blob is outside the
    /// lengths a `/g/` link can have.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let len = 1 + BLOB_ID_LEN + LINK_SECRET_LEN + self.sealed.len();
        if !(CHANNEL_INVITE_CARD_MIN_LEN..=CHANNEL_INVITE_CARD_MAX_LEN).contains(&len) {
            return Err(ContactError::InvalidArgument(
                "channel invitation card sealed blob has a bad length".into(),
            ));
        }
        let mut out = Vec::with_capacity(len);
        out.push(CHANNEL_INVITE_CARD_VERSION_V1);
        out.extend_from_slice(&self.ledger_id);
        out.extend_from_slice(&self.link_secret);
        out.extend_from_slice(&self.sealed);
        Ok(out)
    }

    /// Decode a card. Bounded and fail-closed: a known version and a length
    /// within the card bounds; the sealed blob is not opened here.
    ///
    /// # Errors
    ///
    /// [`ContactError::InvalidEncoding`].
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if !(CHANNEL_INVITE_CARD_MIN_LEN..=CHANNEL_INVITE_CARD_MAX_LEN).contains(&bytes.len()) {
            return Err(ContactError::InvalidEncoding(
                "channel invitation card has a bad length".into(),
            ));
        }
        let mut cur = bytes;
        let version = take_u8(&mut cur)?;
        if version != CHANNEL_INVITE_CARD_VERSION_V1 {
            return Err(ContactError::InvalidEncoding(format!(
                "unknown channel invitation card version {version}",
            )));
        }
        let mut ledger_id = [0u8; BLOB_ID_LEN];
        ledger_id.copy_from_slice(take_n(&mut cur, BLOB_ID_LEN)?);
        let mut link_secret = [0u8; LINK_SECRET_LEN];
        link_secret.copy_from_slice(take_n(&mut cur, LINK_SECRET_LEN)?);
        Ok(Self {
            ledger_id,
            link_secret,
            sealed: cur.to_vec(),
        })
    }

    /// Open the card's link: [`open_link`] under its own secret, then the
    /// preview, which must be a subscriber invitation.
    ///
    /// # Errors
    ///
    /// [`ContactError::LinkDamaged`] when the link does not open, and
    /// [`ContactError::InvalidEncoding`] when it carries no subscriber
    /// invitation.
    pub fn open(&self) -> Result<(GroupInviteCapability, GroupInvitePreview)> {
        let (capability, _) = open_link(&self.link_secret, &self.sealed)?;
        match capability.open_preview(&self.link_secret)? {
            Some(preview) if preview.subscriber.is_some() => Ok((capability, preview)),
            _ => Err(ContactError::InvalidEncoding(
                "channel invitation card carries no subscriber invitation".into(),
            )),
        }
    }
}

/// Derive the 32-byte preview AEAD key from the URL-carried link secret:
///
/// ```text
/// HKDF-SHA256(ikm  = link_secret,
///             salt = empty,
///             info = PREVIEW_KEY_DOMAIN_V1 ‖ u16be gid_len ‖ group_id ‖ issuer_pubkey(32))
/// ```
///
/// The secret is uniform random, so HKDF only has to expand it to the AEAD key
/// width; the key's strength is the secret's 96 bits. Binding the capability's
/// own `group_id` and issuer key into `info` is free (both are in the signed
/// capability the opener already holds) and makes every link's key derivation
/// distinct, so a search over secrets cannot be shared across many links: a
/// guessed secret has to be run through HKDF once per target link rather than
/// tested against all of them with one derivation. It also means a secret moved
/// onto a different capability opens nothing.
fn preview_key(
    link_secret: &[u8; LINK_SECRET_LEN],
    group_id: &[u8],
    issuer_pubkey: &VerifyingKey,
) -> Result<SecretBytes<AEAD_KEY_LEN>> {
    // Callers bound `group_id` to MAX_GROUP_ID_LEN; a wrong caller saturates.
    let gid_len = u16::try_from(group_id.len()).unwrap_or(u16::MAX);
    let mut info = Vec::with_capacity(PREVIEW_KEY_DOMAIN_V1.len() + 2 + group_id.len() + 32);
    info.extend_from_slice(PREVIEW_KEY_DOMAIN_V1);
    info.extend_from_slice(&gid_len.to_be_bytes());
    info.extend_from_slice(group_id);
    info.extend_from_slice(&issuer_pubkey.to_bytes());
    hkdf_sha256::<AEAD_KEY_LEN>(link_secret, &[], &info).map_err(|e| {
        ContactError::InvalidArgument(format!("group-invite preview key derivation: {e}"))
    })
}

impl GroupInviteCapability {
    /// Open the sealed preview with the link secret from the URL fragment.
    ///
    /// `Ok(None)` when the link carries no preview. A caller that has no key,
    /// or gets an `Err` (wrong key, tampered or malformed preview), shows the
    /// page without a preview - the fingerprint - and never a guess.
    ///
    /// # Errors
    ///
    /// - [`ContactError::InvalidEncoding`] when the preview does not open under
    ///   `link_secret` or its plaintext is malformed.
    pub fn open_preview(
        &self,
        link_secret: &[u8; LINK_SECRET_LEN],
    ) -> Result<Option<GroupInvitePreview>> {
        if self.sealed_preview.is_empty() {
            return Ok(None);
        }
        let key = preview_key(link_secret, &self.group_id, &self.issuer_signature_pubkey)?;
        let plain = open_with_key(key.as_bytes(), &self.sealed_preview).map_err(|_| {
            ContactError::InvalidEncoding(
                "group-invite preview does not open under this key".into(),
            )
        })?;
        let preview = GroupInvitePreview::decode(&plain)?;
        if preview.subscriber.is_some() {
            check_subscriber_capability(&self.group_id, &preview, self.knock.as_ref()).map_err(
                |_| {
                    ContactError::InvalidEncoding(
                        "subscriber invitation does not match its capability".into(),
                    )
                },
            )?;
        }
        Ok(Some(preview))
    }

    /// `true` iff the link has not expired at `now_ms`. A [`NEVER_EXPIRES`]
    /// link always returns `true`.
    #[must_use]
    pub fn is_valid_now(&self, now_ms: u64) -> bool {
        self.expiry_ms == NEVER_EXPIRES || now_ms < self.expiry_ms
    }

    /// Signal-style safety number derived from the issuer's Ed25519 key — the
    /// same anti-phishing fingerprint format as
    /// [`crate::call_invite::CallInviteCapability::owner_fingerprint`]. The
    /// joiner UI shows it so the human can compare it against what the inviter
    /// shared out-of-band before clicking "request to join".
    #[must_use]
    pub fn issuer_fingerprint(&self) -> String {
        crate::safety::safety_number(&self.issuer_signature_pubkey, &self.issuer_signature_pubkey)
    }
}

/// A subscriber invitation names its channel as the capability's `group_id`
/// and carries no knock key (see the module docs).
fn check_subscriber_capability(
    group_id: &[u8],
    preview: &GroupInvitePreview,
    knock: Option<&GroupKnockKey>,
) -> Result<()> {
    if preview
        .channel_id
        .as_ref()
        .map(<[u8; PREVIEW_CHANNEL_ID_LEN]>::as_slice)
        != Some(group_id)
    {
        return Err(ContactError::InvalidArgument(
            "a subscriber invitation's group id is its channel id".into(),
        ));
    }
    if knock.is_some() {
        return Err(ContactError::InvalidArgument(
            "a subscriber invitation carries no knock key".into(),
        ));
    }
    Ok(())
}

/// Compose + sign a group join-ticket capability naming the group's knock
/// key.
///
/// # Errors
///
/// - [`ContactError::InvalidArgument`] when `group_id` is empty or longer than
///   [`MAX_GROUP_ID_LEN`].
pub fn compose(
    issuer_signing: &SigningKey,
    group_id: &[u8],
    expiry_ms: u64,
    knock: &GroupKnockKey,
) -> Result<Vec<u8>> {
    if group_id.is_empty() {
        return Err(ContactError::InvalidArgument(
            "group-invite group_id must be non-empty".into(),
        ));
    }
    if group_id.len() > MAX_GROUP_ID_LEN {
        return Err(ContactError::InvalidArgument(format!(
            "group-invite group_id is {} bytes, max is {MAX_GROUP_ID_LEN}",
            group_id.len(),
        )));
    }
    compose_sealed(issuer_signing, group_id, expiry_ms, Some(knock), &[])
}

/// Compose + sign a group join-ticket capability that carries a preview,
/// sealed under the key [`preview_key`] derives from `link_secret` (see
/// [`generate_link_key`]). The caller puts the secret in the link's URL
/// fragment and never anywhere a server reads.
///
/// `knock` is the group's knock key; it must be `Some` for a group or
/// co-publisher preview and `None` for a subscriber invitation, whose
/// `group_id` must be its channel id.
///
/// # Errors
///
/// - [`ContactError::InvalidArgument`] when `group_id` is out of bounds, a
///   preview name fails its rule, or `knock` does not fit the preview kind.
pub fn compose_with_preview(
    issuer_signing: &SigningKey,
    group_id: &[u8],
    expiry_ms: u64,
    knock: Option<&GroupKnockKey>,
    preview: &GroupInvitePreview,
    link_secret: &[u8; LINK_SECRET_LEN],
) -> Result<Vec<u8>> {
    preview.validate()?;
    if preview.subscriber.is_some() {
        check_subscriber_capability(group_id, preview, knock)?;
    } else if knock.is_none() {
        return Err(ContactError::InvalidArgument(
            "a group or co-publisher link names the group's knock key".into(),
        ));
    }
    let key = preview_key(link_secret, group_id, &issuer_signing.verifying_key())?;
    let sealed = seal_with_key(key.as_bytes(), &preview.encode()).map_err(|e| {
        ContactError::InvalidArgument(format!("group-invite preview could not be sealed: {e}"))
    })?;
    compose_sealed(issuer_signing, group_id, expiry_ms, knock, &sealed)
}

fn compose_sealed(
    issuer_signing: &SigningKey,
    group_id: &[u8],
    expiry_ms: u64,
    knock: Option<&GroupKnockKey>,
    sealed_preview: &[u8],
) -> Result<Vec<u8>> {
    if group_id.is_empty() {
        return Err(ContactError::InvalidArgument(
            "group-invite group_id must be non-empty".into(),
        ));
    }
    if group_id.len() > MAX_GROUP_ID_LEN {
        return Err(ContactError::InvalidArgument(format!(
            "group-invite group_id is {} bytes, max is {MAX_GROUP_ID_LEN}",
            group_id.len(),
        )));
    }
    if sealed_preview.len() > MAX_SEALED_PREVIEW_LEN {
        return Err(ContactError::InvalidArgument(format!(
            "group-invite sealed preview is {} bytes, max is {MAX_SEALED_PREVIEW_LEN}",
            sealed_preview.len(),
        )));
    }
    let issuer_pubkey = issuer_signing.verifying_key();
    let mut wire =
        encode_signing_prefix(&issuer_pubkey, group_id, expiry_ms, knock, sealed_preview);
    let signature = issuer_signing.sign(&wire);
    wire.extend_from_slice(&signature.to_bytes());
    Ok(wire)
}

/// Decode + verify a group join-ticket capability.
///
/// Does **not** check `expiry_ms` — call [`GroupInviteCapability::is_valid_now`]
/// separately so the UI can render "this link expired" distinctly from a
/// malformed/forged link.
///
/// # Errors
///
/// - [`ContactError::InvalidEncoding`] — wire format malformed, unsupported
///   schema version, EOF, `group_id` length beyond [`MAX_GROUP_ID_LEN`] or
///   zero, trailing bytes, or signature verification failed.
pub fn decode_and_verify(wire: &[u8]) -> Result<GroupInviteCapability> {
    let mut cur = wire;
    let version = take_u8(&mut cur)?;
    if version != SCHEMA_VERSION {
        return Err(ContactError::InvalidEncoding(format!(
            "unsupported GroupInviteCapability schema version {version}",
        )));
    }
    let gid_len_bytes = take_n(&mut cur, 2)?;
    let gid_len = usize::from(u16::from_be_bytes([gid_len_bytes[0], gid_len_bytes[1]]));
    if gid_len == 0 {
        return Err(ContactError::InvalidEncoding(
            "group-invite group_id_len is zero".into(),
        ));
    }
    if gid_len > MAX_GROUP_ID_LEN {
        return Err(ContactError::InvalidEncoding(format!(
            "group-invite group_id_len {gid_len} > MAX_GROUP_ID_LEN {MAX_GROUP_ID_LEN}"
        )));
    }
    let group_id = take_n(&mut cur, gid_len)?.to_vec();
    let issuer_pubkey_bytes = take_n(&mut cur, ED25519_PUB_LEN)?;
    let expiry_ms_bytes = take_n(&mut cur, 8)?;
    let knock = match take_u8(&mut cur)? {
        0 => None,
        1 => {
            let field = take_n(&mut cur, KNOCK_FIELD_LEN)?;
            let mut public_key = [0u8; KNOCK_KEY_LEN];
            public_key.copy_from_slice(&field[..KNOCK_KEY_LEN]);
            Some(GroupKnockKey {
                public_key,
                version: u16::from_be_bytes([field[KNOCK_KEY_LEN], field[KNOCK_KEY_LEN + 1]]),
            })
        },
        other => {
            return Err(ContactError::InvalidEncoding(format!(
                "group-invite knock flag {other} is neither 0 nor 1"
            )))
        },
    };
    let preview_len_bytes = take_n(&mut cur, 2)?;
    let preview_len = usize::from(u16::from_be_bytes([
        preview_len_bytes[0],
        preview_len_bytes[1],
    ]));
    if preview_len > MAX_SEALED_PREVIEW_LEN {
        return Err(ContactError::InvalidEncoding(format!(
            "group-invite preview_len {preview_len} > MAX_SEALED_PREVIEW_LEN {MAX_SEALED_PREVIEW_LEN}"
        )));
    }
    if preview_len != 0 && preview_len < NONCE_LEN + TAG_LEN {
        return Err(ContactError::InvalidEncoding(
            "group-invite preview shorter than a nonce and a tag".into(),
        ));
    }
    let sealed_preview = take_n(&mut cur, preview_len)?.to_vec();
    let signature_bytes = take_n(&mut cur, SIGNATURE_LEN)?;
    if !cur.is_empty() {
        return Err(ContactError::InvalidEncoding(format!(
            "trailing {} bytes after group-invite signature",
            cur.len(),
        )));
    }

    // Verify the Ed25519 signature over everything before the signature.
    let signing_input = &wire[..wire.len() - SIGNATURE_LEN];
    let issuer_pubkey = VerifyingKey::from_bytes(issuer_pubkey_bytes).map_err(|_| {
        ContactError::InvalidEncoding("group-invite issuer_signature_pubkey malformed".into())
    })?;
    let sig = Signature::from_bytes(signature_bytes)
        .map_err(|_| ContactError::InvalidEncoding("group-invite signature length wrong".into()))?;
    issuer_pubkey
        .verify(signing_input, &sig)
        .map_err(|_| ContactError::InvalidEncoding("group-invite signature invalid".into()))?;

    let expiry_ms = u64::from_be_bytes([
        expiry_ms_bytes[0],
        expiry_ms_bytes[1],
        expiry_ms_bytes[2],
        expiry_ms_bytes[3],
        expiry_ms_bytes[4],
        expiry_ms_bytes[5],
        expiry_ms_bytes[6],
        expiry_ms_bytes[7],
    ]);

    Ok(GroupInviteCapability {
        group_id,
        issuer_signature_pubkey: issuer_pubkey,
        expiry_ms,
        knock,
        sealed_preview,
    })
}

// ============================================================================
// Internal codec helpers
// ============================================================================

fn encode_signing_prefix(
    issuer_signature_pubkey: &VerifyingKey,
    group_id: &[u8],
    expiry_ms: u64,
    knock: Option<&GroupKnockKey>,
    sealed_preview: &[u8],
) -> Vec<u8> {
    // Both lengths were bounded by the caller; a wrong caller still cannot
    // overflow the prefix, it saturates.
    let gid_len_u16 = u16::try_from(group_id.len()).unwrap_or(u16::MAX);
    let preview_len_u16 = u16::try_from(sealed_preview.len()).unwrap_or(u16::MAX);
    let mut out = Vec::with_capacity(
        1 + 2
            + group_id.len()
            + ED25519_PUB_LEN
            + 8
            + 1
            + KNOCK_FIELD_LEN
            + 2
            + sealed_preview.len(),
    );
    out.push(SCHEMA_VERSION);
    out.extend_from_slice(&gid_len_u16.to_be_bytes());
    out.extend_from_slice(group_id);
    out.extend_from_slice(&issuer_signature_pubkey.to_bytes());
    out.extend_from_slice(&expiry_ms.to_be_bytes());
    match knock {
        None => out.push(0),
        Some(k) => {
            out.push(1);
            out.extend_from_slice(&k.public_key);
            out.extend_from_slice(&k.version.to_be_bytes());
        },
    }
    out.extend_from_slice(&preview_len_u16.to_be_bytes());
    out.extend_from_slice(sealed_preview);
    out
}

/// Fuzz hook behind [`crate::fuzz_api::group_invite_preview_plaintext`]:
/// feed bytes straight into the preview plaintext decoder, which the AEAD
/// otherwise keeps out of reach of a fuzzer (a random sealed preview never
/// opens). `None` when the decoder refuses; `Some(true)` when what it accepts
/// also passes the compose-side validation and re-encodes to the same bytes;
/// `Some(false)` would be a decoder / encoder disagreement.
pub(crate) fn fuzz_preview_plaintext(plain: &[u8]) -> Option<bool> {
    let preview = GroupInvitePreview::decode(plain).ok()?;
    Some(preview.validate().is_ok() && preview.encode() == plain)
}

/// Fuzz hook behind [`crate::fuzz_api::channel_invite_card_open`].
pub(crate) fn fuzz_channel_invite_card(bytes: &[u8]) -> Option<bool> {
    let card = ChannelInviteCard::decode(bytes).ok()?;
    let _ = card.open();
    Some(card.encode().ok().as_deref() == Some(bytes))
}

fn take_u8(cur: &mut &[u8]) -> Result<u8> {
    if cur.is_empty() {
        return Err(ContactError::InvalidEncoding("unexpected EOF".to_string()));
    }
    let b = cur[0];
    *cur = &cur[1..];
    Ok(b)
}

fn take_n<'a>(cur: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
    if cur.len() < n {
        return Err(ContactError::InvalidEncoding(format!(
            "need {n} bytes, have {}",
            cur.len(),
        )));
    }
    let (head, rest) = cur.split_at(n);
    *cur = rest;
    Ok(head)
}

// ============================================================================
// Revocation (ADR-0028 OQ1 revoke) — owner-signed delete of a `/g/` blob
// ============================================================================

/// Domain separator for a group-invite revocation signature. Distinct from the
/// contact-invite ([`crate::invite::revoke_canonical_bytes`]) and call-invite
/// ([`crate::call_invite::CALL_INVITE_REVOKE_DOMAIN_V1`]) revoke domains, so a
/// captured revoke signature cannot be replayed across invite kinds.
pub const GROUP_INVITE_REVOKE_DOMAIN_V1: &[u8] = b"lakko-group-invite-revoke/v1\x00";

/// Build the canonical bytes the issuer signs to revoke a group-invite blob.
///
/// Layout: `GROUP_INVITE_REVOKE_DOMAIN_V1 (29 bytes) ‖ blob_id (6 bytes) ‖
/// signed_at_ms (8 bytes BE)`. The discovery service recovers the issuer's
/// verifying key from the stored capability's `issuer_signature_pubkey` and
/// verifies this; `signed_at_ms` gives replay protection bounded by
/// [`GROUP_INVITE_REVOKE_REPLAY_WINDOW_MS`].
#[must_use]
pub fn group_revoke_canonical_bytes(blob_id: &[u8; BLOB_ID_LEN], signed_at_ms: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(GROUP_INVITE_REVOKE_DOMAIN_V1.len() + BLOB_ID_LEN + 8);
    out.extend_from_slice(GROUP_INVITE_REVOKE_DOMAIN_V1);
    out.extend_from_slice(blob_id);
    out.extend_from_slice(&signed_at_ms.to_be_bytes());
    out
}

/// Sign a group-invite revocation with the issuer's Ed25519 key — the same key
/// that signed the [`GroupInviteCapability`]. The server verifies the signature
/// against the embedded `issuer_signature_pubkey` it stored at upload.
#[must_use]
pub fn group_sign_revocation(
    issuer_signing: &SigningKey,
    blob_id: &[u8; BLOB_ID_LEN],
    signed_at_ms: u64,
) -> [u8; SIGNATURE_LEN] {
    let canonical = group_revoke_canonical_bytes(blob_id, signed_at_ms);
    issuer_signing.sign(&canonical).to_bytes()
}

/// Re-export the shared 10-minute symmetric replay window for group-invite
/// revoke callers (same freshness ceiling as the other revoke surfaces, a
/// distinct domain separator).
pub use crate::invite::REVOKE_REPLAY_WINDOW_MS as GROUP_INVITE_REVOKE_REPLAY_WINDOW_MS;

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_signing() -> SigningKey {
        SigningKey::from_seed(&[0x5A; 32])
    }

    fn knock() -> GroupKnockKey {
        GroupKnockKey {
            public_key: [0x4B; KNOCK_KEY_LEN],
            version: 3,
        }
    }

    fn fixture_group_id() -> Vec<u8> {
        let mut gid = vec![0x11; 32];
        gid[0] = 0xCA;
        gid[31] = 0xFE;
        gid
    }

    /// A maximal group capability is the documented size and within
    /// MAX_WIRE_LEN (the bound adds the largest group id to the largest
    /// preview, the subscriber invitation's, which a 16-byte channel id
    /// carries); one secret opens the sealed link and then the preview.
    #[test]
    fn sealed_link_opens_and_the_same_secret_opens_the_preview() {
        let long_name = format!("{}{}", "\u{1F600}".repeat(63), '\u{20AC}');
        let preview = GroupInvitePreview {
            group_name: Some(long_name.clone()),
            inviter_name: Some(long_name),
            member_count: 7,
            channel_id: None,
            subscriber: None,
            avatar: Some(test_photo()),
        };
        let secret = generate_link_key();
        let wire = compose_with_preview(
            &fixture_signing(),
            &vec![0x5a; MAX_GROUP_ID_LEN],
            0,
            Some(&knock()),
            &preview,
            &secret,
        )
        .unwrap();
        let group_max = MAX_WIRE_LEN - MAX_SEALED_PREVIEW_LEN
            + NONCE_LEN
            + MAX_GROUP_PREVIEW_PLAINTEXT_LEN
            + TAG_LEN;
        assert_eq!(wire.len(), group_max);
        assert!(wire.len() <= MAX_WIRE_LEN);
        let sealed = seal_link(&secret, &wire).unwrap();
        assert!(sealed.len() <= MAX_SEALED_LEN);
        let (cap, opened) = open_link(&secret, &sealed).unwrap();
        assert_eq!(opened, wire);
        assert_eq!(cap.open_preview(&secret).unwrap(), Some(preview));
    }

    /// A `/g/` link without the right fragment opens nothing: neither a
    /// wrong secret nor another issuer's valid capability under the id.
    #[test]
    fn a_group_link_without_its_secret_is_damaged() {
        let secret = generate_link_key();
        let sealed = seal_link(
            &secret,
            &compose(&fixture_signing(), &fixture_group_id(), 0, &knock()).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            open_link(&generate_link_key(), &sealed),
            Err(ContactError::LinkDamaged)
        ));
        let theirs = seal_link(
            &generate_link_key(),
            &compose(&SigningKey::generate(), &fixture_group_id(), 0, &knock()).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            open_link(&secret, &theirs),
            Err(ContactError::LinkDamaged)
        ));
    }

    /// ADR-0078 section 6: a co-publisher link carries the channel id in its
    /// sealed preview, and the same secret that opens the link opens it.
    #[test]
    fn a_channel_preview_carries_the_channel_id_through_the_link() {
        let preview = GroupInvitePreview {
            group_name: Some("Daily notes".into()),
            inviter_name: None,
            member_count: 0,
            channel_id: Some([0xC4; PREVIEW_CHANNEL_ID_LEN]),
            subscriber: None,
            avatar: None,
        };
        let secret = generate_link_key();
        let wire = compose_with_preview(
            &fixture_signing(),
            &[0x5a; 32],
            0,
            Some(&knock()),
            &preview,
            &secret,
        )
        .unwrap();
        let sealed = seal_link(&secret, &wire).unwrap();
        let (cap, _) = open_link(&secret, &sealed).unwrap();
        let opened = cap.open_preview(&secret).unwrap().expect("preview");
        assert_eq!(opened, preview);
        assert_eq!(opened.channel_id, Some([0xC4; PREVIEW_CHANNEL_ID_LEN]));
        // The plaintext capability (what the join attribution carries to the
        // knock journal) does not contain the channel id.
        assert!(!wire
            .windows(PREVIEW_CHANNEL_ID_LEN)
            .any(|w| w == [0xC4; PREVIEW_CHANNEL_ID_LEN]));
    }

    #[test]
    fn a_channel_preview_without_a_name_round_trips() {
        let preview = GroupInvitePreview {
            group_name: None,
            inviter_name: None,
            member_count: 0,
            channel_id: Some([0x01; PREVIEW_CHANNEL_ID_LEN]),
            subscriber: None,
            avatar: None,
        };
        let plain = preview.encode();
        // kind, channel id, name length, photo length.
        assert_eq!(plain.len(), 1 + PREVIEW_CHANNEL_ID_LEN + 1 + 1);
        assert_eq!(GroupInvitePreview::decode(&plain).unwrap(), preview);
        assert_eq!(
            crate::fuzz_api::group_invite_preview_plaintext(&plain),
            Some(true)
        );
    }

    #[test]
    fn a_channel_preview_refuses_an_inviter_or_a_count() {
        for (inviter, count) in [(Some("Ann".to_string()), 0u16), (None, 3)] {
            let preview = GroupInvitePreview {
                group_name: Some("Daily notes".into()),
                inviter_name: inviter,
                member_count: count,
                channel_id: Some([0x01; PREVIEW_CHANNEL_ID_LEN]),
                subscriber: None,
                avatar: None,
            };
            assert!(compose_with_preview(
                &fixture_signing(),
                &[0x5a; 32],
                0,
                Some(&knock()),
                &preview,
                &generate_link_key()
            )
            .is_err());
        }
    }

    #[test]
    fn a_damaged_channel_preview_is_refused() {
        let preview = GroupInvitePreview {
            group_name: Some("Daily notes".into()),
            inviter_name: None,
            member_count: 0,
            channel_id: Some([0x01; PREVIEW_CHANNEL_ID_LEN]),
            subscriber: None,
            avatar: None,
        };
        let plain = preview.encode();
        for cut in 0..plain.len() {
            assert!(
                GroupInvitePreview::decode(&plain[..cut]).is_err(),
                "cut {cut}"
            );
        }
        let mut trailing = plain.clone();
        trailing.push(0);
        assert!(GroupInvitePreview::decode(&trailing).is_err());
        let mut bad_name = plain;
        // The last name byte sits before the (empty) photo field.
        let last = bad_name.len() - 2;
        bad_name[last] = b'\n';
        assert!(GroupInvitePreview::decode(&bad_name).is_err());
    }

    /// The fuzz hook reaches the plaintext decoder directly: a real preview
    /// round-trips, and what the decoder refuses (a wrong version, a name that
    /// fails the display-name rule, trailing bytes, a truncation) is `None`.
    #[test]
    fn fuzz_hook_decodes_preview_plaintext() {
        let preview = GroupInvitePreview {
            group_name: Some("Book club".into()),
            inviter_name: None,
            member_count: 7,
            channel_id: None,
            subscriber: None,
            avatar: None,
        };
        let plain = preview.encode();
        assert_eq!(
            crate::fuzz_api::group_invite_preview_plaintext(&plain),
            Some(true)
        );

        let mut wrong_version = plain.clone();
        wrong_version[0] = PREVIEW_KIND_CHANNEL + 1;
        assert_eq!(
            crate::fuzz_api::group_invite_preview_plaintext(&wrong_version),
            None
        );

        let mut trailing = plain.clone();
        trailing.push(0);
        assert_eq!(
            crate::fuzz_api::group_invite_preview_plaintext(&trailing),
            None
        );

        assert_eq!(
            crate::fuzz_api::group_invite_preview_plaintext(&plain[..plain.len() - 1]),
            None
        );
        // A name carrying a line break fails the display-name rule.
        let bad_name = [PREVIEW_VERSION, 2, b'a', b'\n', 0, 0, 1, 0];
        assert_eq!(
            crate::fuzz_api::group_invite_preview_plaintext(&bad_name),
            None
        );
        assert_eq!(crate::fuzz_api::group_invite_preview_plaintext(&[]), None);
    }

    #[test]
    fn compose_decode_round_trips() {
        let signing = fixture_signing();
        let gid = fixture_group_id();
        let wire = compose(&signing, &gid, 1_700_000_000_000, &knock()).unwrap();
        let parsed = decode_and_verify(&wire).unwrap();
        assert_eq!(parsed.group_id, gid);
        assert_eq!(
            parsed.issuer_signature_pubkey.to_bytes(),
            signing.verifying_key().to_bytes()
        );
        assert_eq!(parsed.expiry_ms, 1_700_000_000_000);
    }

    #[test]
    fn variable_length_group_id_round_trips() {
        for len in [1usize, 16, 32, MAX_GROUP_ID_LEN] {
            let gid = vec![0x7Bu8; len];
            let wire = compose(&fixture_signing(), &gid, 0, &knock()).unwrap();
            let parsed = decode_and_verify(&wire).unwrap();
            assert_eq!(parsed.group_id, gid, "len {len}");
        }
    }

    #[test]
    fn never_expires_is_valid_at_any_time() {
        let wire = compose(
            &fixture_signing(),
            &fixture_group_id(),
            GROUP_INVITE_NEVER_EXPIRES,
            &knock(),
        )
        .unwrap();
        let parsed = decode_and_verify(&wire).unwrap();
        assert!(parsed.is_valid_now(0));
        assert!(parsed.is_valid_now(u64::MAX));
    }

    #[test]
    fn expired_link_is_invalid_after_expiry() {
        let wire = compose(&fixture_signing(), &fixture_group_id(), 1_000, &knock()).unwrap();
        let parsed = decode_and_verify(&wire).unwrap();
        assert!(parsed.is_valid_now(999));
        assert!(!parsed.is_valid_now(1_000));
        assert!(!parsed.is_valid_now(2_000));
    }

    #[test]
    fn tampered_group_id_rejected() {
        let mut wire = compose(&fixture_signing(), &fixture_group_id(), 0, &knock()).unwrap();
        // Flip a byte inside the group_id region (offset 3 = first gid byte).
        wire[3] ^= 0xFF;
        match decode_and_verify(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => assert!(msg.contains("signature"), "{msg}"),
            other => panic!("expected signature-invalid, got {other:?}"),
        }
    }

    #[test]
    fn cross_signing_key_rejected() {
        let signing_a = SigningKey::from_seed(&[0xAA; 32]);
        let signing_b = SigningKey::from_seed(&[0xBB; 32]);
        let mut wire = compose(&signing_a, &fixture_group_id(), 0, &knock()).unwrap();
        // Splice B's pubkey over A's (it sits right after version + len + gid).
        let off = 1 + 2 + fixture_group_id().len();
        wire[off..off + ED25519_PUB_LEN].copy_from_slice(&signing_b.verifying_key().to_bytes());
        match decode_and_verify(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => assert!(msg.contains("signature"), "{msg}"),
            other => panic!("expected signature-invalid, got {other:?}"),
        }
    }

    #[test]
    fn truncated_wire_rejected() {
        let wire = compose(&fixture_signing(), &fixture_group_id(), 0, &knock()).unwrap();
        assert!(decode_and_verify(&wire[..wire.len() - SIGNATURE_LEN - 1]).is_err());
    }

    #[test]
    fn trailing_bytes_rejected() {
        let mut wire = compose(&fixture_signing(), &fixture_group_id(), 0, &knock()).unwrap();
        wire.push(0x00);
        match decode_and_verify(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => assert!(msg.contains("trailing"), "{msg}"),
            other => panic!("expected trailing-bytes, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_schema_version_rejected() {
        let mut wire = compose(&fixture_signing(), &fixture_group_id(), 0, &knock()).unwrap();
        // v1 (the pre-preview layout) is refused too: there is no deployment
        // to stay compatible with, and a v1 wire has no preview field.
        wire[0] = 1;
        match decode_and_verify(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => {
                assert!(msg.contains("schema version 1"), "{msg}");
            },
            other => panic!("expected unsupported-version, got {other:?}"),
        }
    }

    #[test]
    fn empty_group_id_rejected_at_compose() {
        let err = compose(&fixture_signing(), &[], 0, &knock()).unwrap_err();
        assert!(matches!(err, ContactError::InvalidArgument(_)));
    }

    #[test]
    fn oversize_group_id_rejected_at_compose() {
        let too_big = vec![0u8; MAX_GROUP_ID_LEN + 1];
        let err = compose(&fixture_signing(), &too_big, 0, &knock()).unwrap_err();
        assert!(matches!(err, ContactError::InvalidArgument(_)));
    }

    #[test]
    fn zero_group_id_len_rejected_at_decode() {
        // Hand-craft a wire with group_id_len = 0.
        let mut wire = vec![SCHEMA_VERSION, 0x00, 0x00];
        wire.extend_from_slice(&[0u8; ED25519_PUB_LEN]);
        wire.extend_from_slice(&0u64.to_be_bytes());
        wire.extend_from_slice(&[0u8; SIGNATURE_LEN]);
        match decode_and_verify(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => {
                assert!(msg.contains("group_id_len"), "{msg}");
            },
            other => panic!("expected zero-len reject, got {other:?}"),
        }
    }

    #[test]
    fn issuer_fingerprint_is_deterministic_per_pubkey() {
        let cap_a = decode_and_verify(
            &compose(&fixture_signing(), &fixture_group_id(), 1_000, &knock()).unwrap(),
        )
        .unwrap();
        let mut other_gid = fixture_group_id();
        other_gid[0] = 0x99;
        let cap_b =
            decode_and_verify(&compose(&fixture_signing(), &other_gid, 999_999, &knock()).unwrap())
                .unwrap();
        assert_eq!(
            cap_a.issuer_fingerprint(),
            cap_b.issuer_fingerprint(),
            "fingerprint must depend ONLY on the issuer pubkey"
        );
    }

    #[test]
    fn different_issuers_have_different_fingerprints() {
        let cap_a = decode_and_verify(
            &compose(
                &SigningKey::from_seed(&[0xAA; 32]),
                &fixture_group_id(),
                0,
                &knock(),
            )
            .unwrap(),
        )
        .unwrap();
        let cap_b = decode_and_verify(
            &compose(
                &SigningKey::from_seed(&[0xBB; 32]),
                &fixture_group_id(),
                0,
                &knock(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_ne!(cap_a.issuer_fingerprint(), cap_b.issuer_fingerprint());
    }

    #[test]
    fn revoke_canonical_bytes_layout_is_stable() {
        let blob_id = [0x33u8; BLOB_ID_LEN];
        let bytes = group_revoke_canonical_bytes(&blob_id, 0x0102_0304_0506_0708);
        assert_eq!(
            bytes.len(),
            GROUP_INVITE_REVOKE_DOMAIN_V1.len() + BLOB_ID_LEN + 8
        );
        assert!(bytes.starts_with(GROUP_INVITE_REVOKE_DOMAIN_V1));
        assert_eq!(
            &bytes[bytes.len() - 8..],
            &0x0102_0304_0506_0708u64.to_be_bytes()
        );
    }

    #[test]
    fn revoke_signature_verifies_under_issuer_key() {
        let issuer = fixture_signing();
        let blob_id = [0x44u8; BLOB_ID_LEN];
        let sig_bytes = group_sign_revocation(&issuer, &blob_id, 1_700_000_000_000);
        let canonical = group_revoke_canonical_bytes(&blob_id, 1_700_000_000_000);
        let sig = Signature::from_bytes(&sig_bytes).unwrap();
        assert!(issuer.verifying_key().verify(&canonical, &sig).is_ok());
        // A different signed_at_ms produces different canonical bytes → reject.
        let stale = group_revoke_canonical_bytes(&blob_id, 1_699_999_999_999);
        assert!(issuer.verifying_key().verify(&stale, &sig).is_err());
    }

    #[test]
    fn revoke_domain_isolates_from_call_invite() {
        // A group-invite revoke signature MUST NOT verify against the
        // call-invite revoke canonical bytes (distinct domain prefix).
        let issuer = fixture_signing();
        let blob_id = [0x55u8; BLOB_ID_LEN];
        let sig_bytes = group_sign_revocation(&issuer, &blob_id, 42);
        let sig = Signature::from_bytes(&sig_bytes).unwrap();
        let call_canonical = crate::call_invite::call_revoke_canonical_bytes(&blob_id, 42);
        assert!(issuer
            .verifying_key()
            .verify(&call_canonical, &sig)
            .is_err());
    }

    fn fixture_preview() -> GroupInvitePreview {
        GroupInvitePreview {
            group_name: Some("Weekend plans".to_string()),
            inviter_name: Some("Anna Weber".to_string()),
            member_count: 4,
            channel_id: None,
            subscriber: None,
            avatar: None,
        }
    }

    #[test]
    fn a_link_without_a_preview_opens_none() {
        let wire = compose(
            &fixture_signing(),
            &fixture_group_id(),
            NEVER_EXPIRES,
            &knock(),
        )
        .unwrap();
        let cap = decode_and_verify(&wire).unwrap();
        assert!(cap.sealed_preview.is_empty());
        assert_eq!(cap.open_preview(&generate_link_key()).unwrap(), None);
    }

    #[test]
    fn preview_round_trips_under_the_link_key_and_not_under_another() {
        let key = generate_link_key();
        let wire = compose_with_preview(
            &fixture_signing(),
            &fixture_group_id(),
            NEVER_EXPIRES,
            Some(&knock()),
            &fixture_preview(),
            &key,
        )
        .unwrap();
        let cap = decode_and_verify(&wire).unwrap();
        assert_eq!(cap.group_id, fixture_group_id());
        assert_eq!(cap.open_preview(&key).unwrap(), Some(fixture_preview()));
        let other = generate_link_key();
        assert!(
            cap.open_preview(&other).is_err(),
            "a wrong key must not open the preview"
        );
    }

    #[test]
    fn link_secret_is_twelve_bytes_and_sixteen_url_characters() {
        assert_eq!(LINK_SECRET_LEN, 12);
        assert_eq!(generate_link_key().len(), LINK_SECRET_LEN);
        assert_eq!(LINK_SECRET_B64URL_LEN, (LINK_SECRET_LEN * 4).div_ceil(3));
        assert_eq!(LINK_SECRET_B64URL_LEN, 16);
        assert_ne!(generate_link_key(), generate_link_key());
    }

    #[test]
    fn preview_key_is_hkdf_over_the_secret_bound_to_group_and_issuer() {
        let secret = [0x07u8; LINK_SECRET_LEN];
        let gid = fixture_group_id();
        let issuer = fixture_signing().verifying_key();
        let key = preview_key(&secret, &gid, &issuer).unwrap();
        let mut info = PREVIEW_KEY_DOMAIN_V1.to_vec();
        info.extend_from_slice(&u16::try_from(gid.len()).unwrap().to_be_bytes());
        info.extend_from_slice(&gid);
        info.extend_from_slice(&issuer.to_bytes());
        let expected = hkdf_sha256::<AEAD_KEY_LEN>(&secret, &[], &info).unwrap();
        assert_eq!(key.as_bytes(), expected.as_bytes());
        // A different group or issuer gives a different key for the same secret.
        let mut other_gid = gid.clone();
        other_gid[1] ^= 0x01;
        assert_ne!(
            preview_key(&secret, &other_gid, &issuer)
                .unwrap()
                .as_bytes(),
            key.as_bytes()
        );
        let other_issuer = SigningKey::from_seed(&[0x6B; 32]).verifying_key();
        assert_ne!(
            preview_key(&secret, &gid, &other_issuer)
                .unwrap()
                .as_bytes(),
            key.as_bytes()
        );
    }

    #[test]
    fn a_link_secret_moved_onto_another_capability_opens_nothing() {
        // The key is bound to the capability's group id and issuer, so a
        // sealed preview transplanted onto a capability naming a different
        // group or issuer does not open under the same secret.
        let secret = generate_link_key();
        let wire_a = compose_with_preview(
            &fixture_signing(),
            &fixture_group_id(),
            NEVER_EXPIRES,
            Some(&knock()),
            &fixture_preview(),
            &secret,
        )
        .unwrap();
        let cap_a = decode_and_verify(&wire_a).unwrap();
        let mut transplanted = cap_a.clone();
        transplanted.group_id[0] ^= 0x01;
        assert!(transplanted.open_preview(&secret).is_err());
        let mut other_issuer = cap_a.clone();
        other_issuer.issuer_signature_pubkey = SigningKey::from_seed(&[0x6B; 32]).verifying_key();
        assert!(other_issuer.open_preview(&secret).is_err());
        assert_eq!(
            cap_a.open_preview(&secret).unwrap(),
            Some(fixture_preview())
        );
    }

    #[test]
    fn preview_names_are_optional_and_the_count_survives() {
        let key = generate_link_key();
        let preview = GroupInvitePreview {
            group_name: None,
            inviter_name: None,
            member_count: u16::MAX,
            channel_id: None,
            subscriber: None,
            avatar: None,
        };
        let wire = compose_with_preview(
            &fixture_signing(),
            &fixture_group_id(),
            NEVER_EXPIRES,
            Some(&knock()),
            &preview,
            &key,
        )
        .unwrap();
        let cap = decode_and_verify(&wire).unwrap();
        assert_eq!(cap.open_preview(&key).unwrap(), Some(preview));
    }

    #[test]
    fn preview_names_follow_the_display_name_rule() {
        let key = generate_link_key();
        for bad in ["", "line\nbreak", "bidi\u{202E}", &"x".repeat(256)] {
            let preview = GroupInvitePreview {
                group_name: Some(bad.to_string()),
                inviter_name: None,
                member_count: 1,
                channel_id: None,
                subscriber: None,
                avatar: None,
            };
            assert!(
                compose_with_preview(
                    &fixture_signing(),
                    &fixture_group_id(),
                    NEVER_EXPIRES,
                    Some(&knock()),
                    &preview,
                    &key
                )
                .is_err(),
                "{bad:?} must be refused",
            );
        }
        let preview = GroupInvitePreview {
            group_name: Some(
                "\u{1F389} \u{412}\u{435}\u{447}\u{435}\u{440}\u{438}\u{43D}\u{43A}\u{430}"
                    .to_string(),
            ),
            inviter_name: Some("\u{41C}\u{430}\u{440}\u{438}\u{44F}".to_string()),
            member_count: 3,
            channel_id: None,
            subscriber: None,
            avatar: None,
        };
        let wire = compose_with_preview(
            &fixture_signing(),
            &fixture_group_id(),
            NEVER_EXPIRES,
            Some(&knock()),
            &preview,
            &key,
        )
        .unwrap();
        assert_eq!(
            decode_and_verify(&wire)
                .unwrap()
                .open_preview(&key)
                .unwrap(),
            Some(preview)
        );
    }

    #[test]
    fn a_tampered_preview_breaks_the_signature() {
        let key = generate_link_key();
        let mut wire = compose_with_preview(
            &fixture_signing(),
            &fixture_group_id(),
            NEVER_EXPIRES,
            Some(&knock()),
            &fixture_preview(),
            &key,
        )
        .unwrap();
        let idx = wire.len() - SIGNATURE_LEN - 5;
        wire[idx] ^= 0x01;
        assert!(decode_and_verify(&wire).is_err());
    }

    #[test]
    fn an_oversize_or_short_preview_length_is_refused_at_decode() {
        let signing = fixture_signing();
        let gid = fixture_group_id();
        let mut prefix = Vec::new();
        prefix.push(SCHEMA_VERSION);
        prefix.extend_from_slice(&u16::try_from(gid.len()).unwrap().to_be_bytes());
        prefix.extend_from_slice(&gid);
        prefix.extend_from_slice(&signing.verifying_key().to_bytes());
        prefix.extend_from_slice(&0u64.to_be_bytes());
        prefix.push(0);
        let mut over = prefix.clone();
        over.extend_from_slice(
            &u16::try_from(MAX_SEALED_PREVIEW_LEN + 1)
                .unwrap()
                .to_be_bytes(),
        );
        over.extend_from_slice(&vec![0u8; MAX_SEALED_PREVIEW_LEN + 1]);
        let sig = signing.sign(&over);
        over.extend_from_slice(&sig.to_bytes());
        assert!(decode_and_verify(&over).is_err());
        let mut short = prefix;
        short.extend_from_slice(&1u16.to_be_bytes());
        short.push(0);
        let sig = signing.sign(&short);
        short.extend_from_slice(&sig.to_bytes());
        assert!(decode_and_verify(&short).is_err());
    }

    // --- ADR-0087: the knock key, the subscriber invitation, the card -----

    fn subscriber_preview(access: ChannelAccess) -> GroupInvitePreview {
        GroupInvitePreview {
            group_name: Some("Berlin news".into()),
            inviter_name: None,
            member_count: 0,
            channel_id: Some([0xC7; PREVIEW_CHANNEL_ID_LEN]),
            subscriber: Some(SubscriberInvite {
                access,
                description: "Local news\nevery morning".into(),
            }),
            avatar: None,
        }
    }

    /// ADR-0087 section 18.4: every preview kind carries the photo at its
    /// end, through the sealed link, and a photo outside its bounds refuses
    /// the whole preview on compose and on decode.
    #[test]
    fn every_preview_kind_carries_a_photo() {
        let mut still_only = test_photo();
        still_only.video = None;
        let group = GroupInvitePreview {
            avatar: Some(test_photo()),
            ..fixture_preview()
        };
        let copublisher = GroupInvitePreview {
            group_name: Some("Daily notes".into()),
            inviter_name: None,
            member_count: 0,
            channel_id: Some([0xC4; PREVIEW_CHANNEL_ID_LEN]),
            subscriber: None,
            avatar: Some(still_only),
        };
        let subscriber = GroupInvitePreview {
            avatar: Some(test_photo()),
            ..subscriber_preview(private_access())
        };
        for (preview, group_id, knock_key) in [
            (group, fixture_group_id(), Some(knock())),
            (copublisher, vec![0x5a; 32], Some(knock())),
            (subscriber, vec![0xC7; PREVIEW_CHANNEL_ID_LEN], None),
        ] {
            let plain = preview.encode();
            assert_eq!(GroupInvitePreview::decode(&plain).unwrap(), preview);
            let without = GroupInvitePreview {
                avatar: None,
                ..preview.clone()
            };
            let len = if preview.avatar.as_ref().unwrap().video.is_some() {
                170
            } else {
                84
            };
            assert_eq!(plain.len(), without.encode().len() + len);
            let secret = generate_link_key();
            let wire = compose_with_preview(
                &fixture_signing(),
                &group_id,
                0,
                knock_key.as_ref(),
                &preview,
                &secret,
            )
            .unwrap();
            let (cap, _) = open_link(&secret, &seal_link(&secret, &wire).unwrap()).unwrap();
            assert_eq!(cap.open_preview(&secret).unwrap(), Some(preview.clone()));

            // Out of bounds: refused before sealing, and on decode.
            let mut bad = preview.clone();
            if let Some(a) = &mut bad.avatar {
                a.still.height = a.still.width - 1;
            }
            assert!(compose_with_preview(
                &fixture_signing(),
                &group_id,
                0,
                knock_key.as_ref(),
                &bad,
                &secret,
            )
            .is_err());
            assert!(GroupInvitePreview::decode(&bad.encode()).is_err());
            assert_eq!(
                crate::fuzz_api::group_invite_preview_plaintext(&bad.encode()),
                None
            );
        }
    }

    fn test_photo() -> Avatar {
        use crate::avatar::{AvatarRef, AvatarVideo};
        let reference = |byte: u8, side: u16, size: u32| AvatarRef {
            blob_id: [byte; 32],
            key: [byte.wrapping_add(1); 32],
            nonce: [byte.wrapping_add(2); 12],
            size,
            width: side,
            height: side,
        };
        Avatar {
            still: reference(0x10, 640, 40_000),
            video: Some(AvatarVideo {
                reference: reference(0x20, 480, 1_000_000),
                duration_ms: 5_000,
            }),
        }
    }

    fn private_access() -> ChannelAccess {
        ChannelAccess::Private {
            invite_seed: [0x11; SUBSCRIBER_KEY_LEN],
            admission_public_key: [0x22; SUBSCRIBER_KEY_LEN],
            admission_version: 2,
        }
    }

    #[test]
    fn the_knock_key_round_trips_and_is_signed() {
        let wire = compose(&fixture_signing(), &fixture_group_id(), 0, &knock()).unwrap();
        let cap = decode_and_verify(&wire).unwrap();
        assert_eq!(cap.knock, Some(knock()));
        // The knock version sits right before the preview length; flipping
        // it breaks the signature.
        let at = 1 + 2 + fixture_group_id().len() + ED25519_PUB_LEN + 8 + 1 + KNOCK_KEY_LEN;
        let mut tampered = wire;
        tampered[at + 1] ^= 1;
        assert!(decode_and_verify(&tampered).is_err());
    }

    #[test]
    fn a_v2_capability_and_a_bad_knock_flag_are_refused() {
        let mut wire = compose(&fixture_signing(), &fixture_group_id(), 0, &knock()).unwrap();
        wire[0] = 2;
        assert!(decode_and_verify(&wire).is_err());
        let signing = fixture_signing();
        let gid = fixture_group_id();
        let mut prefix = vec![SCHEMA_VERSION];
        prefix.extend_from_slice(&u16::try_from(gid.len()).unwrap().to_be_bytes());
        prefix.extend_from_slice(&gid);
        prefix.extend_from_slice(&signing.verifying_key().to_bytes());
        prefix.extend_from_slice(&0u64.to_be_bytes());
        prefix.push(2);
        prefix.extend_from_slice(&0u16.to_be_bytes());
        let sig = signing.sign(&prefix);
        prefix.extend_from_slice(&sig.to_bytes());
        match decode_and_verify(&prefix) {
            Err(ContactError::InvalidEncoding(msg)) => assert!(msg.contains("knock flag"), "{msg}"),
            other => panic!("expected a knock-flag refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_group_link_needs_a_knock_key_and_a_subscriber_invitation_has_none() {
        let secret = generate_link_key();
        assert!(compose_with_preview(
            &fixture_signing(),
            &fixture_group_id(),
            0,
            None,
            &fixture_preview(),
            &secret
        )
        .is_err());
        let channel = [0xC7; PREVIEW_CHANNEL_ID_LEN];
        assert!(compose_with_preview(
            &fixture_signing(),
            &channel,
            0,
            Some(&knock()),
            &subscriber_preview(private_access()),
            &secret
        )
        .is_err());
        // A subscriber invitation's group id is its channel id.
        assert!(compose_with_preview(
            &fixture_signing(),
            &fixture_group_id(),
            0,
            None,
            &subscriber_preview(private_access()),
            &secret
        )
        .is_err());
    }

    #[test]
    fn a_subscriber_invitation_round_trips_private_and_public() {
        let channel = [0xC7; PREVIEW_CHANNEL_ID_LEN];
        for access in [
            private_access(),
            ChannelAccess::Public {
                read_key: [0x33; SUBSCRIBER_KEY_LEN],
            },
        ] {
            let preview = subscriber_preview(access);
            let secret = generate_link_key();
            let wire =
                compose_with_preview(&fixture_signing(), &channel, 0, None, &preview, &secret)
                    .unwrap();
            let sealed = seal_link(&secret, &wire).unwrap();
            let (cap, _) = open_link(&secret, &sealed).unwrap();
            assert_eq!(cap.knock, None);
            assert_eq!(cap.group_id, channel);
            assert_eq!(cap.open_preview(&secret).unwrap(), Some(preview.clone()));
            let plain = preview.encode();
            assert_eq!(fuzz_preview_plaintext(&plain), Some(true));
            for cut in 0..plain.len() {
                assert!(
                    GroupInvitePreview::decode(&plain[..cut]).is_err(),
                    "cut {cut}"
                );
            }
        }
    }

    #[test]
    fn a_subscriber_invitation_that_names_another_channel_does_not_open() {
        // Hand-built: the preview names one channel, the capability another.
        let secret = generate_link_key();
        let preview = subscriber_preview(private_access());
        let other = [0x01; PREVIEW_CHANNEL_ID_LEN];
        let key = preview_key(&secret, &other, &fixture_signing().verifying_key()).unwrap();
        let sealed = seal_with_key(key.as_bytes(), &preview.encode()).unwrap();
        let wire = compose_sealed(&fixture_signing(), &other, 0, None, &sealed).unwrap();
        let cap = decode_and_verify(&wire).unwrap();
        assert!(cap.open_preview(&secret).is_err());
    }

    #[test]
    fn subscriber_invitation_fields_follow_the_channel_rules() {
        let mut no_name = subscriber_preview(private_access());
        no_name.group_name = None;
        let mut bad_description = subscriber_preview(private_access());
        if let Some(sub) = bad_description.subscriber.as_mut() {
            sub.description = "rlo\u{202E}".into();
        }
        for preview in [no_name, bad_description] {
            assert!(preview.validate().is_err());
        }
        let mut bad_access = subscriber_preview(private_access()).encode();
        bad_access[1 + PREVIEW_CHANNEL_ID_LEN] = 9;
        assert!(GroupInvitePreview::decode(&bad_access).is_err());
    }

    #[test]
    fn maximal_lengths_are_the_documented_construction() {
        assert_eq!(MAX_PREVIEW_PLAINTEXT_LEN, 1537);
        assert_eq!(MAX_SEALED_PREVIEW_LEN, 1565);
        assert_eq!(MAX_WIRE_LEN, 1965);
        assert_eq!(MAX_SEALED_LEN, 1995);
        assert_eq!(CHANNEL_INVITE_CARD_MAX_LEN, 2014);
        // The chat envelope keeps a card of up to
        // `lakko_attachments::MAX_CHANNEL_INVITE_BYTES` (2048) bytes.
        const { assert!(CHANNEL_INVITE_CARD_MAX_LEN <= 2048) };
        // A maximal subscriber invitation fills the bound exactly.
        let preview = GroupInvitePreview {
            group_name: Some(format!("{}\u{20AC}", "\u{1F600}".repeat(63))),
            inviter_name: None,
            member_count: 0,
            channel_id: Some([0xC7; PREVIEW_CHANNEL_ID_LEN]),
            subscriber: Some(SubscriberInvite {
                access: private_access(),
                description: "\u{1F600}".repeat(256),
            }),
            avatar: Some(test_photo()),
        };
        assert_eq!(preview.encode().len(), MAX_PREVIEW_PLAINTEXT_LEN);
    }

    #[test]
    fn a_channel_invite_card_round_trips_and_opens() {
        let channel = [0xC7; PREVIEW_CHANNEL_ID_LEN];
        let preview = subscriber_preview(private_access());
        let secret = generate_link_key();
        let wire =
            compose_with_preview(&fixture_signing(), &channel, 0, None, &preview, &secret).unwrap();
        let card = ChannelInviteCard {
            ledger_id: [9; BLOB_ID_LEN],
            link_secret: secret,
            sealed: seal_link(&secret, &wire).unwrap(),
        };
        let bytes = card.encode().unwrap();
        assert_eq!(ChannelInviteCard::decode(&bytes).unwrap(), card);
        assert_eq!(fuzz_channel_invite_card(&bytes), Some(true));
        let (cap, opened) = card.open().unwrap();
        assert_eq!(cap.group_id, channel);
        assert_eq!(opened, preview);
        assert!(!format!("{card:?}").contains(&format!("{secret:?}")));

        // A group link in a card is not a subscriber invitation.
        let group_wire = compose_with_preview(
            &fixture_signing(),
            &fixture_group_id(),
            0,
            Some(&knock()),
            &fixture_preview(),
            &secret,
        )
        .unwrap();
        let group_card = ChannelInviteCard {
            sealed: seal_link(&secret, &group_wire).unwrap(),
            ..card.clone()
        };
        assert!(group_card.open().is_err());

        let mut wrong_version = bytes.clone();
        wrong_version[0] = 2;
        assert!(ChannelInviteCard::decode(&wrong_version).is_err());
        assert!(ChannelInviteCard::decode(&bytes[..CHANNEL_INVITE_CARD_MIN_LEN - 1]).is_err());
        let mut long = bytes;
        long.resize(CHANNEL_INVITE_CARD_MAX_LEN + 1, 0);
        assert!(ChannelInviteCard::decode(&long).is_err());
    }
}
