// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Channel broadcast (Phase 4a — publisher path).
//!
//! Implements the cryptographic substrate described in
//! [`ADR-0010`](../../../docs/adr/0010-channels-two-tier-broadcast.md):
//! **two-tier** architecture where a small PublisherGroup (vanilla
//! MLS, ≤ 500 members) holds the rotation authority for a 32-byte
//! `content_key_seed`, and subscribers receive HPKE-sealed envelopes
//! carrying that seed.
//!
//! ## What's here (4a)
//!
//! - [`seed`] — derives the channel content-key seed from a
//!   PublisherGroup's MLS exporter. The caller (a publisher who
//!   already owns an MLS [`Group`]) hands us the 32 raw bytes that
//!   came out of `mls_exporter("lakko/channel/v1", channel_id, 32)`;
//!   we wrap them in a [`ContentKeySeed`] handle and forbid copying.
//! - [`post`] — composes and verifies a [`ChannelPost`]: per-message
//!   AEAD key + nonce HKDF off the seed, ChaCha20-Poly1305 over
//!   plaintext, Ed25519 signature over
//!   `channel_id || epoch_id || sender_pubkey || message_counter || ciphertext`.
//! - [`epoch`] — monotone `EpochId` counter + serialisation helpers,
//!   advances on every PublisherGroup commit and on scheduled
//!   rotations.
//!
//! ## What's also here (4b)
//!
//! - [`subscribe`] — HPKE-equivalent seed envelope:
//!   [`seal_seed`] / [`open_seed`] + [`SEED_ENVELOPE_LEN`].
//!   Wire shape: `eph_pub(32) || sealed_seed(32) || tag(16) = 80`.
//!   AAD binds `channel_id || epoch_id` so a misrouted envelope
//!   to the wrong channel or wrong epoch fails the AEAD
//!   authenticator. Exposed across UniFFI / wasm-bindgen as
//!   `ChannelSubscriberKeypair` + `sealSubscriberSeedEnvelope` +
//!   `openSubscriberSeedEnvelope` (sprint G4-3c).
//!
//! ## What's also here (ADR-0051)
//!
//! - [`meta`] — the channel metadata record (name + description),
//!   carried as the plaintext of an ordinary post and recognised by
//!   readers ahead of the text path via
//!   [`classify_post_plaintext`]. `compose_channel_meta` /
//!   `decode_channel_meta` / `channel_meta_is_newer`.
//!
//! ## What's also here (ADR-0059)
//!
//! - [`owner`] — the per-channel owner key
//!   (`BLAKE3-derive-key("lakko/channels/owner/v1", account_seed ‖ channel_id)`)
//!   and the owner request proof `services/channels` verifies.
//! - [`listing`] — the public directory listing codec, the fixed topic list
//!   and the language rule. [`meta`] is at schema version 2 (topic,
//!   language, listed).
//!
//! ## What's also here (ADR-0060)
//!
//! - [`member_card`] — a subscriber's profile sealed under the channel's
//!   current epoch seed for the channel member list, keyed and bound per
//!   `(channel, epoch, subscriber)` so the server cannot swap two cards.
//!   `seal_member_card` / `member_card_epoch` / `open_member_card`, and the
//!   upload signature `member_card_signed_bytes` / `sign_member_card_request`
//!   / `verify_member_card_request` shared with `services/channels`.
//! - The subscriber key is Ed25519 ([`subscribe`]): the same key signs
//!   member-card requests and, converted to X25519, receives the seed
//!   envelope.
//!
//! ## What's also here (ADR-0087)
//!
//! - [`meta`] is at schema version 3: the public/private flag replaces
//!   `listed`, the `@name` rides as a conversation-handle envelope, and every
//!   other post's plaintext is the chat envelope (`lakko_attachments`),
//!   classified by [`classify_post_plaintext`].
//! - [`public_seed`] — a public channel's one seed envelope per epoch,
//!   sealed under the read key its public record carries.
//! - [`invite`] — a private channel's invite key, the invite proof the
//!   service checks, and the sealed admission ticket the publisher checks.
//!
//! - [`write_key`], [`reaction`], [`comment`], [`ban_list`] — comments and
//!   reactions (section 13a): the per-write key a subscriber write is signed
//!   by and its proof, reaction labels a reader resolves against the emoji
//!   it knows (any emoji, the chat rule), the sealed comment with its
//!   per-channel author key, and the publisher's ban list, carried as a post
//!   like the metadata record.
//!
//! ## What's NOT here yet
//!
//! - **Rotation distribution wire (4c).** The fanout
//!   service + batch envelope generator are over in
//!   `services/channels` (already shipped — see
//!   `POST /v1/channels/:id/rotate` and `docs/security/calls.md`
//!   C-42). The client-side rotation loop that walks the
//!   subscriber registry and seals N envelopes is a follow-up
//!   wave.
//! - **`@channel` discovery.** Lives in `services/discovery` (OPRF
//!   lookup) — already shipped via G4-5.
//!
//! ## Threat-model notes (cross-link to ADR-0010)
//!
//! - The seed is the **only** secret that controls channel
//!   confidentiality. We keep it inside `Zeroizing<[u8; 32]>` and
//!   never expose the raw bytes through the public API — callers
//!   pass the [`ContentKeySeed`] handle by reference.
//! - The Ed25519 signature is what lets a subscriber distinguish
//!   posts from different publishers even though they share the
//!   same seed. A subscriber who learned the seed (legitimately or
//!   via compromise) still cannot forge a post by another publisher
//!   without that publisher's identity Ed25519 private key.
//! - HKDF binds the per-message key to `(channel_id, epoch_id,
//!   sender_pubkey)` and binds the per-message nonce to
//!   `(channel_id, epoch_id, sender_pubkey, message_counter)`. Two
//!   publishers using the same seed therefore never collide on
//!   keys; two posts from one publisher with the same counter
//!   would collide on key + nonce — the `message_counter` invariant
//!   is enforced client-side and surfaced as a hard error in
//!   `verify_and_decrypt` if a duplicate is observed.
//! - AAD on the AEAD seal is the same byte string the Ed25519
//!   signature commits to, so a subscriber who fails to validate
//!   the signature short-circuits without ever asking the AEAD to
//!   decrypt — defence in depth against ciphertext-malleability
//!   experiments.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod ban_list;
pub mod comment;
pub mod copublisher;
pub mod epoch;
pub mod error;
pub mod invite;
pub mod listing;
pub mod member_card;
pub mod meta;
pub mod owner;
pub mod poll;
pub mod post;
pub mod public_seed;
pub mod reaction;
pub mod rotation_policy;
pub mod seed;
pub mod subscribe;
pub mod unsubscribe;
pub mod write_key;

pub use ban_list::{
    ban_list_is_newer, compose_ban_list, decode_ban_list, AuthorKey, BanList, BanListError,
    BAN_LIST_DOMAIN_V1, BAN_LIST_MAX_KEYS, BAN_LIST_MAX_LEN, BAN_LIST_MIN_LEN, BAN_LIST_VERSION_V1,
};
pub use comment::{
    check_comment_text, comment_author_seed, comment_signed_bytes, open_comment, seal_comment,
    Comment, CommentAuthor, CommentError, CommentPlace, CommentTextError, OpenedComment,
    COMMENT_AUTHOR_KEY_DOMAIN, COMMENT_AUTHOR_KEY_LEN, COMMENT_KEY_INFO_V1,
    COMMENT_SEALED_CODEC_MAX_LEN, COMMENT_SEALED_MAX_LEN, COMMENT_SEALED_MIN_LEN,
    COMMENT_SIG_DOMAIN_V1, COMMENT_TEXT_MAX_BYTES, COMMENT_VERSION_V1,
};
pub use copublisher::{
    channel_copublisher_seed, compose_copublisher_binding, decode_copublisher_binding,
    verify_copublisher_proof, ChannelCoPublisherSigner, CoPublisherBinding,
    CoPublisherBindingError, CoPublisherProof, COPUBLISHER_BINDING_DOMAIN_V1,
    COPUBLISHER_BINDING_LEN, COPUBLISHER_KEY_DOMAIN, COPUBLISHER_PROOF_DOMAIN,
    COPUBLISHER_PROOF_HEADER_NAME, MAX_COPUBLISHERS,
};
pub use epoch::{EpochId, EPOCH_ID_LEN};
pub use error::{ChannelError, Result};
pub use invite::{
    admission_public_key, generate_admission_seed, invite_proof_canonical_bytes,
    invite_signing_seed, open_admission_ticket, ticket_admission_version, verify_invite_proof,
    AdmissionTicketError, ChannelInviteSigner, InviteProof, ADMISSION_SEED_LEN,
    ADMISSION_TICKET_DOMAIN, ADMISSION_TICKET_LEN, ADMISSION_TICKET_VERSION_V1, INVITE_KEY_DOMAIN,
    INVITE_KEY_LEN, INVITE_PROOF_DOMAIN, INVITE_PROOF_HEADER_NAME, INVITE_PROOF_MAX_HEADER_LEN,
};
pub use listing::{
    check_language, compose_channel_listing, decode_channel_listing, is_known_topic,
    listing_is_newer, topic_name, ChannelListing, ChannelListingError, LanguageError,
    CHANNEL_LISTING_DOMAIN_V1, CHANNEL_LISTING_MAX_LEN, CHANNEL_LISTING_MIN_LEN,
    CHANNEL_LISTING_VERSION_V1, TOPICS, TOPIC_OTHER,
};
pub use member_card::{
    member_card_binding_bytes, member_card_epoch, member_card_signed_bytes, open_member_card,
    seal_member_card, sign_member_card_request, verify_member_card_request, OpenedMemberCard,
    MAX_MEMBER_CARD_PROFILE_LEN, MEMBER_CARD_BINDING_DOMAIN_V1, MEMBER_CARD_BINDING_LEN,
    MEMBER_CARD_IDENTITY_PK_LEN, MEMBER_CARD_KEY_INFO_V1, MEMBER_CARD_MAX_LEN, MEMBER_CARD_MIN_LEN,
    MEMBER_CARD_SIG_DOMAIN_V1, MEMBER_CARD_SIG_HEADER, MEMBER_CARD_SIG_LEN, MEMBER_CARD_VERSION_V2,
    SUBSCRIBER_PK_LEN,
};
pub use meta::{
    channel_meta_is_newer, classify_post_plaintext, compose_channel_meta, decode_channel_meta,
    AllowedReactions, ChannelMeta, ChannelMetaError, ChannelMetaFieldError, PostContent,
    ALLOWED_REACTIONS_MAX, CHANNEL_META_DOMAIN_V1, CHANNEL_META_MAX_LEN, CHANNEL_META_MIN_LEN,
    CHANNEL_META_VERSION_V3,
};
pub use owner::{
    channel_owner_seed, owner_handover_body, parse_owner_handover_body, verify_owner_proof,
    ChannelOwnerSigner, OwnerHandoverError, OwnerProof, OwnerProofError, OwnerRequest,
    OWNER_HANDOVER_BODY_LEN, OWNER_KEY_DOMAIN, OWNER_PROOF_DOMAIN, OWNER_PROOF_HEADER_NAME,
    OWNER_PROOF_WINDOW_MS,
};
pub use poll::{
    check_vote_labels, decode_poll_tally, poll_option_label, poll_option_labels, vote_message,
    ChannelPollError, ChannelPollTally, VoteAction, MAX_CHANNEL_POLL_OPTIONS, MAX_VOTE_LABELS,
    MAX_VOTE_MESSAGE_LEN, MIN_CHANNEL_POLL_OPTIONS, POLL_OPTION_LABEL_DOMAIN, POLL_TALLY_DOMAIN,
    POLL_TALLY_MAX_LEN, POLL_TALLY_MIN_LEN, POLL_TALLY_VERSION_V1, VOTE_MESSAGE_DOMAIN,
};
pub use post::{
    compose_post, decode_post, verify_and_decrypt, ChannelPost, MAX_PLAINTEXT_LEN,
    POST_SIG_DOMAIN_V1,
};
pub use public_seed::{
    channel_read_key, open_public_seed, public_seed_epoch, seal_public_seed,
    PUBLIC_SEED_ENVELOPE_LEN, READ_KEY_DOMAIN, READ_KEY_LEN,
};
pub use reaction::{
    CommentReactionKey, ReactionKey, ReactionLabel, ReactionLabels, COMMENT_REACTION_KEY_INFO_V1,
    REACTION_KEY_INFO_V1, REACTION_LABEL_LEN,
};
pub use rotation_policy::{RotationPolicy, RotationState, DEFAULT_INTERVAL_MS, DEFAULT_MAX_POSTS};
pub use seed::{ContentKeySeed, CONTENT_KEY_SEED_LEN, EXPORTER_LABEL_V1};
pub use subscribe::{
    open_seed, seal_seed, subscriber_public_key, EPH_PUB_LEN, SEED_ENVELOPE_LEN, TAG_LEN,
};
pub use unsubscribe::{
    sign_unsubscribe_request, unsubscribe_signed_bytes, verify_unsubscribe_request,
    UNSUBSCRIBE_DOMAIN_V1, UNSUBSCRIBE_SIG_HEADER, UNSUBSCRIBE_SIG_LEN, UNSUBSCRIBE_WINDOW_MS,
};
pub use write_key::{
    comment_reaction_write_seed, comment_write_seed, new_comment_nonce, reaction_write_seed,
    verify_write_proof, write_proof_canonical_bytes, CommentReactionWriteKey, CommentWriteKey,
    ReactionWriteKey, SubscriberWrite, WriteProof, WriteProofError, WriteStatement,
    COMMENT_NONCE_LEN, COMMENT_REACTION_WRITE_KEY_DOMAIN, COMMENT_WRITE_KEY_DOMAIN,
    REACTION_WRITE_KEY_DOMAIN, WRITE_KEY_LEN, WRITE_PROOF_DOMAIN, WRITE_PROOF_HEADER_NAME,
    WRITE_PROOF_WINDOW_MS,
};

/// Channel identifier length — 16 bytes (the pseudonym chosen at
/// channel creation, per ADR-0010 § "Server can correlate" mitigation).
pub const CHANNEL_ID_LEN: usize = 16;

/// Entry points for the fuzz harnesses in `crates/lakko-channels/fuzz`. Not
/// part of the supported API.
#[doc(hidden)]
pub mod fuzz_api {
    /// Run the member-card binding parser (ADR-0060 section 3) on an
    /// arbitrary card plaintext under a fixed channel and subscriber key:
    /// `Some(profile length)` when the binding verifies. The AEAD keeps a
    /// fuzzer from reaching this parser through `open_member_card`.
    #[must_use]
    pub fn member_card_binding(plaintext: &[u8]) -> Option<usize> {
        crate::member_card::verify_binding(plaintext, &[0x0C; crate::CHANNEL_ID_LEN], &[0x5B; 32])
            .ok()
            .map(|opened| opened.profile.len())
    }

    /// Parse `value` as an invite-proof header (ADR-0087 section 7) and
    /// verify it for a fixed row. `None` when it does not parse; `Some(true)`
    /// when it re-encodes to the same text.
    #[must_use]
    pub fn channel_invite_proof_parse(value: &str) -> Option<bool> {
        crate::invite::fuzz_parse_proof(value)
    }

    /// Open `ticket` as an admission ticket under a fixed admission seed and
    /// row, and feed it to the ticket plaintext check directly. `true` when
    /// either accepted, which a fuzzer cannot cause.
    #[must_use]
    pub fn admission_ticket_open(ticket: &[u8]) -> bool {
        crate::invite::fuzz_open_ticket(ticket)
    }

    /// Parse `value` as a write proof header (ADR-0087 section 13a) and
    /// verify it for a fixed write. `None` when it does not parse;
    /// `Some(true)` when it re-encodes to the same text.
    #[must_use]
    pub fn write_proof_parse(value: &str) -> Option<bool> {
        crate::write_key::fuzz_parse_proof(value)
    }

    /// Open `sealed` as a comment (ADR-0087 section 13a) at a fixed place
    /// under a fixed seed, and run the plaintext and body parsers on the same
    /// bytes. `true` when every property holds (see the oracle).
    #[must_use]
    pub fn channel_comment_open(sealed: &[u8]) -> bool {
        crate::comment::fuzz_open(sealed)
    }

    /// Open `wire` as a public seed envelope (ADR-0087 section 6) under a
    /// fixed read key. `None` when it has the wrong shape; `Some(true)` when
    /// the open refused, which it must since a fuzzer cannot forge the tag.
    #[must_use]
    pub fn public_seed_open(wire: &[u8]) -> Option<bool> {
        crate::public_seed::fuzz_open(wire)
    }

    /// Decode `bytes` as a channel poll's frozen tally (ADR-0092 section
    /// 4.3). `true` when it is refused, or decodes and re-encodes to the
    /// identical bytes with every count at most its voters.
    #[must_use]
    pub fn channel_poll_tally_decode(bytes: &[u8]) -> bool {
        crate::poll::decode_poll_tally(bytes).map_or(true, |t| {
            t.counts.iter().all(|c| *c <= t.voters)
                && t.encode().is_ok_and(|e| e.as_slice() == bytes)
        })
    }
}
