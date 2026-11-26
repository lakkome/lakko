// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Contact-request envelopes and pseudonym lifecycle for Lakko.
//!
//! Phase 1.9a covers ADR-0005 scenario A — "I know `@username`,
//! add them as a contact" — minus the username discovery step
//! (handled by `services/discovery` + the OPRF round-trip in
//! `lakko-transport`).
//!
//! ## What's here (1.9a)
//!
//! - [`pseudonym`] — three pseudonym classes per ADR-0005 §13.7,
//!   each with a distinct derivation domain so leaks don't cross
//!   classes. `BLAKE3-keyed` for client-side variants;
//!   `BLAKE3(domain || pubkey)` for the public identity pseudonym.
//! - [`envelope`] — a sealed-box AEAD envelope (HPKE-Base-equivalent
//!   over X25519 + HKDF-SHA-256 + ChaCha20-Poly1305). Used to
//!   encrypt the optional intro message inside a ContactRequest to
//!   the recipient's identity public key.
//! - [`request`] — the [`ContactRequest`] wire type plus
//!   [`compose`] / [`decode_and_verify`] / [`open_envelope`].
//!   Manual length-prefixed binary codec, frozen at v1.
//!   Ed25519-signed by the sender's identity key.
//!   The sealed plaintext is
//!   `profile_len ‖ profile ‖ reply_grant_len ‖ reply_grant ‖ intro`
//!   (ADR-0048 §2, ADR-0060 §6).
//! - [`account_deleted`] — the ADR-0098 account-deleted envelope a deleting
//!   device posts in every conversation, and the shared rules deciding whether
//!   a person is shown as "Deleted account" and whether their `@username`
//!   may be shown.
//! - [`committer_delegation`] — the ADR-0060 §6 committer-delegation
//!   envelope: a proposer's delegations for whichever candidate commits its
//!   `Add`, so the Welcome reaches the added person's closed mailbox.
//! - [`profile`] — the ADR-0048 profile envelope (display name,
//!   claimed username, `updated_at_ms`): bounded, fail-closed,
//!   classified by [`incoming_envelope`].
//! - [`username`] — the ADR-0048 §4 canonical-username rule
//!   (`[a-z0-9_]`, 3 to 32), shared with `lakko-discovery-client`.
//! - [`group_name`] — the ADR-0051 group-name envelope (the shared
//!   name of a group, `updated_at_ms`, empty = clear): the profile
//!   template reused, bounded, fail-closed, classified by
//!   [`incoming_envelope`]; the sender's authority is the receiver's
//!   role-table check, not this crate's.
//! - [`pin`] — the ADR-0052 shared-pin envelope (pin / unpin one message
//!   for everyone, `updated_at_ms`): fixed-length, fail-closed, classified
//!   by [`incoming_envelope`]; the order is [`pin::pin_is_newer`], the
//!   authority is the receiver's role-table check.
//! - [`group_contact_request`] — the ADR-0086 §6 envelope carrying an
//!   ordinary contact request inside a group, sealed whole to one member's
//!   X25519 key so the other members cannot tell whom it is for, for a
//!   person no held reach grant reaches;
//!   [`group_contact_request::check_group_contact_request`] is the
//!   receiver's rule.
//! - [`group_closed`] — the ADR-0073 phase-2 group-closed envelope (an admin
//!   closes a group whose owner reset their keys and names the recreated
//!   group, `closed_at_ms`): the group-name template reused, bounded,
//!   fail-closed, classified by [`incoming_envelope`]; the order is
//!   [`group_closed::group_closed_is_newer`], the authority is the receiver's
//!   role-table check.
//! - [`key_rotation`] — the ADR-0074 key-rotation envelope (the device that
//!   changes the account keys hands the new phrase entropy and the
//!   supersession proof to the account's other devices, in the DeviceGroup
//!   only and only after the untrusted device is out): fixed-length,
//!   fail-closed, classified by [`incoming_envelope`]; the receiver's checks
//!   and the sender's ordering are in the module docs.
//! - [`reach`] — ADR-0060 reach keys: the per-class, per-scope Ed25519 keys
//!   an identity mailbox accepts a write under, derived from the account
//!   seed, and the [`ReachGrant`] that hands one to someone.
//! - [`reach_plan`] — G-53: which held grant writes a Welcome or signs a
//!   committer delegation (supersession, shared-scope rule, one order for
//!   every client) and when the unsigned write to an open mailbox may follow.
//! - [`introduction`] — ADR-0060 contacts of contacts: introduction records
//!   sealed under OPRF outputs, the offer that asks a contact to relay one,
//!   and the padded bundle a contact relays.
//! - [`visibility`] — ADR-0060 "who can find me" setting and the reach-key
//!   version state, as envelopes the owner's devices exchange.
//! - [`handle`], [`handle_record`], [`conversation_handle`], [`knock`] —
//!   ADR-0087: the handle key of a group or channel and the statements it
//!   signs to discovery, the public record of a public group or channel, the
//!   envelope that tells members the `@name` and its flag, and the sealed
//!   knock row (a request to join with no epoch); [`knock_seed`], the
//!   envelope that hands an admin one version of the knock seed;
//!   [`admission_seed`], the envelope that hands a co-publisher one version
//!   of a private channel's admission seed.
//! - [`registrar_seed`] — the ADR-0057 registrar-seed envelope (a journal
//!   root hands one member its Delivery Service registrar seed, sealed to
//!   that member's account X25519 key, with the registrar public key in the
//!   clear so every member learns account -> registrar key): fixed-length,
//!   fail-closed, classified by [`incoming_envelope`].
//!
//! ## What's NOT here (1.9b and later)
//!
//! - QR safety numbers (scenario D) — Signal-style fingerprint
//!   verification.
//! - Group-invite envelope (scenario E).
//! - PoW + Privacy Pass tokens carried inside the request — the
//!   server side already exists in 1.1e; the wire-format extension
//!   waits for a real client integration sprint.
//! - `ContactAck` reply path (recipient → sender) — easy follow-up
//!   to add once we have UI to drive it.
//!
//! ## Threat-model notes (cross-link to ADR-0005)
//!
//! - Sealed envelopes provide **confidentiality** but not
//!   sender-authentication. The Ed25519 signature on the request
//!   itself is what proves "this came from `from_pubkey`".
//! - The `from_pseudonym` is fresh per recipient so the server
//!   cannot link two contact-requests Alice sends to different
//!   people. KT-log inclusion proofs over `from_pubkey` (handled
//!   in `lakko-transparency`) anchor the public key to a
//!   verifiable identity record.
//! - Server sees only `(stranger_inbox_pseudonym → wire bytes)`;
//!   it learns nothing about who Alice is or what Bob does with
//!   the request.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod account_deleted;
pub mod ack;
pub mod admission_seed;
pub mod allowed_reactions;
pub mod avatar;
pub mod call_invite;
pub mod call_kp_forward;
pub mod call_welcome;
pub mod channel_admission_mark;
pub mod channel_epoch_seeds;
pub mod channel_read_key;
pub mod channel_return;
pub mod channel_rotate_request;
pub mod channel_text;
pub mod committer_beacon;
pub mod committer_delegation;
pub mod continuity;
pub mod conversation_handle;
pub mod conversation_reentry;
pub mod device_card;
pub mod envelope;
pub mod error;
pub mod gap_history;
pub mod group_closed;
pub mod group_contact_request;
pub mod group_invite;
pub mod group_join_attribution;
pub mod group_name;
pub mod group_photo;
pub mod group_reactions;
pub mod handle;
pub mod handle_record;
pub mod history_request;
pub mod incoming_envelope;
pub mod introduction;
pub mod invite;
pub mod journal_epoch;
pub mod key_rotation;
pub mod knock;
pub mod knock_key;
pub mod knock_seed;
pub mod link_claim;
pub mod link_founded;
pub mod link_refusal;
pub mod link_seal;
pub mod mailbox;
pub mod media_state;
pub mod open_items;
pub mod own_binding;
pub mod pin;
pub mod poll_ballots;
pub mod profile;
pub mod profile_request;
pub mod pseudonym;
pub mod publisher_device_request;
pub mod publisher_reader_key;
pub mod push_routing;
pub mod push_wake;
pub mod reach;
pub mod reach_plan;
pub mod registrar_seed;
pub mod rejoin_present;
pub mod request;
pub mod safety;
pub mod shared_channels;
pub mod username;
pub mod visibility;
pub mod x25519_binding;

pub use account_deleted::{
    account_deletion_status, compose_account_deleted, decode_account_deleted, deletion_effects,
    directory_check_due, may_show_username, try_unwrap_account_deleted, verify_account_deleted,
    AccountDeleted, AccountDeletedError, AccountStatus, DeletionEffects, DirectoryAnswer,
    ACCOUNT_DELETED_ENVELOPE_DOMAIN_V1, ACCOUNT_DELETED_ENVELOPE_LEN,
    ACCOUNT_DELETED_MAX_GROUP_ID_LEN, ACCOUNT_DELETED_VERSION_V1, DIRECTORY_RECHECK_AFTER_MS,
    DIRECTORY_RETRY_AFTER_MS, USERNAME_CONFIRMATION_VALID_MS,
};
pub use ack::{compose_ack, decode_and_verify_ack, ContactAck};
pub use admission_seed::{
    admission_seed_public_key, compose_admission_seed, decode_admission_seed,
    try_unwrap_admission_seed, AdmissionSeedEnvelope, AdmissionSeedError,
    ADMISSION_SEED_ENVELOPE_DOMAIN_V1, ADMISSION_SEED_ENVELOPE_LEN, ADMISSION_SEED_VERSION_V1,
};
pub use allowed_reactions::{
    is_reaction_emoji_bytes, AllowedReactions, AllowedReactionsError,
    ALLOWED_REACTIONS_ENCODED_MAX_LEN, ALLOWED_REACTIONS_MAX, REACTION_EMOJI_MAX_BYTES,
};
pub use avatar::{
    avatar_field_len, decode_avatar_field, encode_avatar_field, read_avatar_field, Avatar,
    AvatarError, AvatarRef, AvatarVideo, AVATAR_FIELD_MAX_LEN, AVATAR_REF_LEN,
    AVATAR_STILL_MAX_BYTES, AVATAR_STILL_MAX_PX, AVATAR_STILL_ONLY_LEN, AVATAR_VIDEO_MAX_BYTES,
    AVATAR_VIDEO_MAX_DURATION_MS, AVATAR_VIDEO_MAX_PX, AVATAR_WITH_VIDEO_LEN,
};
pub use call_invite::{
    call_revoke_canonical_bytes, call_sign_revocation, compose as compose_call_invite,
    decode_and_verify as decode_and_verify_call_invite, CallInviteCapability, CallInviteMedia,
    CALL_ID_LEN, CALL_INVITE_REVOKE_DOMAIN_V1, CALL_NEVER_EXPIRES, CALL_REVOKE_REPLAY_WINDOW_MS,
    CALL_SINGLE_USE, MAX_KEY_PACKAGE_LEN,
};
pub use call_kp_forward::{
    try_unwrap_call_kp_forward, wrap_call_kp_forward, CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1,
    CALL_KP_FORWARD_MIN_PAYLOAD_LEN,
};
pub use call_welcome::{
    try_unwrap_call_welcome, wrap_call_welcome, CallWelcomeError, CALL_WELCOME_ENVELOPE_DOMAIN_V1,
    CALL_WELCOME_MIN_PAYLOAD_LEN,
};
pub use channel_admission_mark::{
    compose_channel_admission_mark, decode_channel_admission_mark,
    try_unwrap_channel_admission_mark, ChannelAdmissionMark, ChannelAdmissionMarkError,
    CHANNEL_ADMISSION_MARK_ENVELOPE_DOMAIN_V1, CHANNEL_ADMISSION_MARK_ENVELOPE_LEN,
};
pub use channel_epoch_seeds::{
    channel_epoch_seeds_envelope_len, compose_channel_epoch_seeds, decode_channel_epoch_seeds,
    try_unwrap_channel_epoch_seeds, ChannelEpochSeed, ChannelEpochSeedsEnvelope,
    ChannelEpochSeedsError, CHANNEL_EPOCH_SEEDS_ENVELOPE_DOMAIN_V1,
    CHANNEL_EPOCH_SEEDS_ENVELOPE_MAX_LEN, CHANNEL_EPOCH_SEEDS_ENVELOPE_MIN_LEN,
    CHANNEL_EPOCH_SEEDS_MAX,
};
pub use channel_read_key::{
    compose_channel_read_key, decode_channel_read_key, try_unwrap_channel_read_key,
    ChannelReadKeyEnvelope, ChannelReadKeyError, CHANNEL_READ_KEY_ENVELOPE_DOMAIN_V1,
    CHANNEL_READ_KEY_ENVELOPE_LEN,
};
pub use channel_return::{
    compose_channel_return, decode_channel_return, member_card_binding_bytes_from_hash,
    try_unwrap_channel_return, ChannelReturn, ChannelReturnError, ChannelReturnRole,
    CHANNEL_RETURN_DOMAIN_V1, CHANNEL_RETURN_MAX_INVITATION_LEN, CHANNEL_RETURN_MAX_LEN,
    CHANNEL_RETURN_MIN_LEN, MEMBER_CARD_BINDING_DOMAIN_V1,
};
pub use channel_rotate_request::{
    compose_channel_rotate_request, decode_channel_rotate_request,
    try_unwrap_channel_rotate_request, ChannelRotateRequestError,
    CHANNEL_ROTATE_REQUEST_ENVELOPE_DOMAIN_V1, CHANNEL_ROTATE_REQUEST_ENVELOPE_LEN,
};
pub use channel_text::{
    check_description as check_channel_description, check_name as check_channel_name,
    ChannelTextError,
};
pub use committer_beacon::{
    acting_committer_verdict, compose_committer_beacon, decode_committer_beacon,
    should_emit_beacon, try_unwrap_committer_beacon, ActingCommitter, BeaconObservation,
    CommitterBeacon, CommitterBeaconError, COMMITTER_BEACON_ENVELOPE_DOMAIN_V1,
    COMMITTER_BEACON_FRESHNESS_WINDOW_MS, COMMITTER_BEACON_INTERVAL_MS,
    COMMITTER_BEACON_PAYLOAD_LEN, COMMITTER_BEACON_VERSION_V1, COMMITTER_BEACON_WORST_CASE_POLL_MS,
};
pub use committer_delegation::{
    compose_committer_delegation, decode_committer_delegation, try_unwrap_committer_delegation,
    CommitterDelegationEnvelope, CommitterDelegationError, COMMITTER_DELEGATION_BLOB_LEN,
    COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1, COMMITTER_DELEGATION_ENVELOPE_MAX_LEN,
    COMMITTER_DELEGATION_ENVELOPE_MIN_LEN, COMMITTER_DELEGATION_VERSION_V2,
    MAX_COMMITTER_DELEGATIONS,
};
pub use continuity::{
    continuity_mailbox_key, continuity_mailbox_seed, verify_continuity_binding, ContinuityBinding,
    ContinuityError, CONTINUITY_BINDING_LEN, CONTINUITY_MAX_WINDOW_MS, CONTINUITY_WINDOW_MS,
};
pub use conversation_handle::{
    compose_conversation_handle, conversation_handle_is_newer, decode_conversation_handle,
    try_unwrap_conversation_handle, ConversationHandle, ConversationHandleError,
    CONVERSATION_HANDLE_CERT_LEN, CONVERSATION_HANDLE_ENVELOPE_DOMAIN_V1,
    CONVERSATION_HANDLE_ENVELOPE_MAX_LEN, CONVERSATION_HANDLE_ENVELOPE_MIN_LEN,
    CONVERSATION_HANDLE_VERSION_V1,
};
pub use conversation_reentry::{
    compose_conversation_reentry, decode_conversation_reentry, open_reentry_knock,
    seal_reentry_knock, try_unwrap_conversation_reentry, ConversationReentry,
    ConversationReentryError, CONVERSATION_REENTRY_ENVELOPE_DOMAIN_V1,
    CONVERSATION_REENTRY_ENVELOPE_MAX_LEN, CONVERSATION_REENTRY_ENVELOPE_MIN_LEN,
    CONVERSATION_REENTRY_GAP_KEY_LEN, CONVERSATION_REENTRY_MAX_KEY_PACKAGE_LEN,
    CONVERSATION_REENTRY_VERSION_V2,
};
pub use device_card::{
    compose_device_card, decode_device_card, device_card_is_newer, try_unwrap_device_card,
    DeviceCard, DeviceCardError, DeviceCardField, DeviceCardFieldError, DevicePlatform,
    DEVICE_CARD_APP_VERSION_MAX_BYTES, DEVICE_CARD_CITY_MAX_BYTES, DEVICE_CARD_COUNTRY_LEN,
    DEVICE_CARD_ENVELOPE_DOMAIN_V1, DEVICE_CARD_ENVELOPE_MAX_LEN, DEVICE_CARD_ENVELOPE_MIN_LEN,
    DEVICE_CARD_MODEL_MAX_BYTES, DEVICE_CARD_OS_MAX_BYTES, DEVICE_CARD_VERSION_V1,
};
pub use error::{ContactError, Result};
pub use gap_history::{
    gap_history_rendezvous, gap_history_start, open_gap_history, seal_gap_history, GapHistoryError,
    GapKey, OpenedGapHistory, GAP_HISTORY_MAX_BUNDLE_LEN, GAP_HISTORY_MAX_SEALED_LEN,
    GAP_HISTORY_OVERHEAD, GAP_HISTORY_VERSION_V2,
};
pub use group_closed::{
    compose_group_closed, decode_group_closed, group_closed_is_newer, try_unwrap_group_closed,
    GroupClosed, GroupClosedError, GROUP_CLOSED_ENVELOPE_DOMAIN_V1, GROUP_CLOSED_ENVELOPE_MAX_LEN,
    GROUP_CLOSED_ENVELOPE_MIN_LEN, GROUP_CLOSED_VERSION_V1, SUCCESSOR_GROUP_ID_LEN,
};
pub use group_contact_request::{
    check_group_contact_request, compose_group_contact_request, decode_group_contact_request,
    seal_group_contact_request, try_unwrap_group_contact_request, GroupContactRequest,
    GroupContactRequestError, GROUP_CONTACT_REQUEST_ENVELOPE_DOMAIN_V1,
    GROUP_CONTACT_REQUEST_ENVELOPE_MAX_LEN, GROUP_CONTACT_REQUEST_ENVELOPE_MIN_LEN,
    GROUP_CONTACT_REQUEST_MAX_REQUEST_LEN, GROUP_CONTACT_REQUEST_MAX_SEALED_LEN,
    GROUP_CONTACT_REQUEST_MIN_SEALED_LEN, GROUP_CONTACT_REQUEST_VERSION_V1,
};
pub use group_invite::{
    compose as compose_group_invite, decode_and_verify as decode_and_verify_group_invite,
    group_revoke_canonical_bytes, group_sign_revocation, ChannelAccess, ChannelInviteCard,
    GroupInviteCapability, GroupInvitePreview, GroupKnockKey, SubscriberInvite,
    CHANNEL_INVITE_CARD_MAX_LEN, CHANNEL_INVITE_CARD_MIN_LEN, GROUP_INVITE_NEVER_EXPIRES,
    GROUP_INVITE_REVOKE_DOMAIN_V1, GROUP_INVITE_REVOKE_REPLAY_WINDOW_MS, MAX_GROUP_ID_LEN,
};
pub use group_join_attribution::{
    compose_attribution as compose_group_join_attribution,
    decode_attribution as decode_group_join_attribution, GroupJoinAttribution,
};
pub use group_name::{
    compose_group_name, decode_group_name, group_name_is_newer, normalize_group_name,
    try_unwrap_group_name, GroupName, GroupNameError, GROUP_NAME_ENVELOPE_DOMAIN_V1,
    GROUP_NAME_ENVELOPE_MAX_LEN, GROUP_NAME_ENVELOPE_MIN_LEN, GROUP_NAME_VERSION_V1,
};
pub use group_photo::{
    compose_group_photo, decode_group_photo, group_photo_is_newer, try_unwrap_group_photo,
    GroupPhoto, GroupPhotoError, GROUP_PHOTO_ENVELOPE_DOMAIN_V1, GROUP_PHOTO_ENVELOPE_MAX_LEN,
    GROUP_PHOTO_ENVELOPE_MIN_LEN, GROUP_PHOTO_ENVELOPE_STILL_LEN, GROUP_PHOTO_VERSION_V1,
};
pub use group_reactions::{
    compose_group_reactions, decode_group_reactions, group_reactions_is_newer,
    try_unwrap_group_reactions, GroupReactions, GroupReactionsError,
    GROUP_REACTIONS_ENVELOPE_DOMAIN_V1, GROUP_REACTIONS_ENVELOPE_MAX_LEN,
    GROUP_REACTIONS_ENVELOPE_MIN_LEN, GROUP_REACTIONS_VERSION_V1,
};
pub use handle::{
    handle_content_hash, handle_write_public_key, handle_write_seed, HandleStatement,
    HandleStatementBody, HandleStatementError, HandleVisibility, HANDLE_STATEMENT_MAX_LEN,
    HANDLE_STATEMENT_MIN_LEN, HANDLE_STATEMENT_WINDOW_MS, HANDLE_WRITE_DOMAIN, MAX_HANDLE_TERMS,
};
pub use handle_record::{
    compose_handle_record, open_handle_record, open_search_record, HandleRecordError,
    HandleRecordKind, HandleRecordPayload, SearchRecord, HANDLE_RECORD_KNOCK_VERSION_LEN,
    HANDLE_RECORD_PAYLOAD_MAX_LEN, HANDLE_RECORD_PAYLOAD_MIN_LEN, HANDLE_RECORD_PAYLOAD_VERSION_V3,
    MAX_SIZE_BAND,
};
pub use history_request::{
    compose_history_request, decode_history_request, try_unwrap_history_request, HistoryRequest,
    HistoryRequestError, HISTORY_REQUEST_DOMAIN_V1, HISTORY_REQUEST_ENVELOPE_LEN,
};
pub use incoming_envelope::{dispatch_incoming_envelope, DispatchError, IncomingEnvelope};
pub use introduction::{
    bundle_is_newer, compose_bundle, compose_introduction, compose_introduction_offer,
    compose_public_record, decode_introduction_offer, decode_introductions, fof_tag,
    introduction_tags, open_introduction, open_public_record, try_unwrap_introduction_offer,
    try_unwrap_introductions, IntroductionEntry, IntroductionError, IntroductionPayload,
    IntroductionRecord, IntroductionsBundle, BUNDLE_SIZE_BUCKETS, FOF_KEY_DOMAIN, FOF_TAG_DOMAIN,
    FOF_TAG_LEN, INTRODUCTIONS_ENVELOPE_DOMAIN_V1, INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1,
    INTRODUCTION_RECORD_MAX_LEN, INTRODUCTION_RECORD_MIN_LEN, MAX_BUNDLE_RECORDS,
    MAX_INTRODUCTION_TERMS, OPRF_OUTPUT_LEN, PUBLIC_RECORD_MAX_LEN, PUBLIC_RECORD_MIN_LEN,
    WRAPPED_KEY_LEN,
};
pub use invite::{
    compute_joiner_tag, generate_blob_id, generate_link_id, revocation_public_key,
    revoke_canonical_bytes, sign_revocation, InviteCapability, InvitePreview, BLOB_ID_B64URL_LEN,
    BLOB_ID_LEN, JOINER_TAG_LEN, JOINER_TAG_SALT_DOMAIN_V1, LINK_ID_LEN, MAX_LABEL_LEN,
    MAX_PREVIEW_LEN, MAX_PREVIEW_NAME_BYTES, MAX_PREVIEW_USERNAME_BYTES, NEVER_EXPIRES,
    OWNER_LIST_DOMAIN_V1, REVOCATION_KEY_LEN, REVOKE_DOMAIN_V1, REVOKE_REPLAY_WINDOW_MS,
    SINGLE_USE,
};
pub use journal_epoch::{JournalEpochs, JOURNAL_EPOCHS_MAX_COMMITS};
pub use key_rotation::{
    compose_key_rotation, decode_key_rotation, try_unwrap_key_rotation, KeyRotation,
    KeyRotationError, KEY_ROTATION_ENTROPY_LEN, KEY_ROTATION_ENVELOPE_DOMAIN_V1,
    KEY_ROTATION_ENVELOPE_LEN, KEY_ROTATION_PENDING_MAX_AGE_MS, KEY_ROTATION_PROOF_LEN,
    KEY_ROTATION_VERSION_V1,
};
pub use knock::{
    is_knock_row_len, knock_public_key, knock_row_version, knock_seed, open_knock, seal_knock,
    KnockError, KnockRequest, KNOCK_LARGE_ROW_LEN, KNOCK_SMALL_ROW_LEN,
};
pub use knock_key::{
    compose_knock_key, decode_knock_key, knock_key_is_newer, try_unwrap_knock_key, KnockKey,
    KnockKeyError, KNOCK_KEY_ENVELOPE_DOMAIN_V1, KNOCK_KEY_ENVELOPE_LEN,
};
pub use knock_seed::{
    compose_knock_seed, decode_knock_seed, try_unwrap_knock_seed, KnockSeedEnvelope,
    KnockSeedError, KNOCK_SEED_ENVELOPE_DOMAIN_V1, KNOCK_SEED_ENVELOPE_LEN, KNOCK_SEED_VERSION_V1,
};
pub use link_claim::{
    compose_link_claim, decode_link_claim, link_claim_answered_elsewhere, link_claim_outcome,
    link_claim_winner, try_unwrap_link_claim, LinkClaim, LinkClaimError, LinkClaimOutcome,
    ObservedLinkClaim, LINK_CLAIM_AUTHOR_KEY_LEN, LINK_CLAIM_ENVELOPE_DOMAIN_V1,
    LINK_CLAIM_ENVELOPE_LEN, LINK_CLAIM_REQUESTER_KEY_LEN, LINK_CLAIM_REQUEST_KEY_LEN,
    LINK_CLAIM_VERSION_V1,
};
pub use link_founded::{
    compose_link_founded, decode_link_founded, try_unwrap_link_founded, LinkFounded,
    LinkFoundedError, LINK_FOUNDED_ENVELOPE_DOMAIN_V1, LINK_FOUNDED_ENVELOPE_MAX_LEN,
    LINK_FOUNDED_ENVELOPE_MIN_LEN, LINK_FOUNDED_GROUP_ID_LEN, LINK_FOUNDED_MAX_WELCOME_LEN,
    LINK_FOUNDED_VERSION_V1,
};
pub use link_refusal::{
    compose_link_refusal, decode_link_refusal, verify_link_refusal, LinkRefusal, LinkRefusalError,
    LINK_REFUSAL_DOMAIN_V1, LINK_REFUSAL_LEN, LINK_REFUSAL_VERSION_V1,
};
pub use mailbox::{
    bucket_for as mailbox_bucket_for, is_well_formed_row as is_well_formed_mailbox_row,
    mailbox_address, mailbox_seed, open_mailbox_row, seal_mailbox_row, MailboxBinding,
    MailboxError, MailboxKey, MailboxKind, MAILBOX_ADDRESS_DOMAIN, MAILBOX_BINDING_DOMAIN,
    MAILBOX_BINDING_LEN, MAILBOX_ENVELOPE_AAD_DOMAIN, MAILBOX_KEY_LEN, MAILBOX_MAX_BODY_LEN,
    MAILBOX_MAX_BUCKET, MAILBOX_MAX_ROW_LEN, MAILBOX_MIN_ROW_LEN, MAILBOX_ROW_OVERHEAD,
    MAILBOX_SEED_DOMAIN,
};
pub use media_state::{
    compose_media_state, decode_media_state, try_unwrap_media_state, MediaState, MediaStateError,
    MEDIA_STATE_ENVELOPE_DOMAIN_V1, MEDIA_STATE_FLAG_CAMERA, MEDIA_STATE_FLAG_MICROPHONE,
    MEDIA_STATE_PAYLOAD_LEN, MEDIA_STATE_VERSION_V2,
};
pub use open_items::{
    assemble_open_items, compose_open_items, decode_open_items, is_open_items_envelope,
    open_items_handover_due, open_items_sender_trusted, plan_open_items_rows,
    try_unwrap_open_items, AssembledContent, AssembledItem, HandoverAdded, HandoverItemRef,
    HandoverTick, HandoverVote, OpenItem, OpenItemContent, OpenItemEntry, OpenItemHeader,
    OpenItemPart, OpenItems, OpenItemsError, ReceivedOpenItems, OPEN_ITEMS_ENVELOPE_DOMAIN_V1,
    OPEN_ITEMS_ENVELOPE_MAX_LEN, OPEN_ITEMS_ENVELOPE_MIN_LEN, OPEN_ITEMS_MAX_BODY_LEN,
    OPEN_ITEMS_MAX_ITEMS, OPEN_ITEMS_VERSION_V1,
};
pub use pin::{
    compose_pin, decode_pin, pin_is_newer, try_unwrap_pin, PinAction, PinError, PinOp,
    PIN_ENVELOPE_DOMAIN_V1, PIN_ENVELOPE_LEN, PIN_VERSION_V1,
};
pub use poll_ballots::{
    ballot_needs_handover, compose_poll_ballots, decode_poll_ballots, is_poll_ballots_envelope,
    plan_poll_ballots_rows, try_unwrap_poll_ballots, PollBallots, PollBallotsError,
    BALLOT_HANDOVER_MARGIN_MS, JOURNAL_RETENTION_MS, POLL_BALLOTS_ENVELOPE_DOMAIN_V1,
    POLL_BALLOTS_ENVELOPE_MAX_LEN, POLL_BALLOTS_ENVELOPE_MIN_LEN, POLL_BALLOTS_MAX_ROW_LEN,
    POLL_BALLOTS_VERSION_V1,
};
pub use profile::{
    check_display_name, compose_profile, decode_profile, is_format_char, is_profile_grant_class,
    normalize_display_name, profile_is_newer, try_unwrap_profile, verify_profile, DisplayNameError,
    Profile, ProfileError, DISPLAY_NAME_MAX_BYTES, DISPLAY_NAME_MAX_CHARS, FORMAT_CHAR_RANGES,
    MAX_PROFILE_GRANTS, PROFILE_ENVELOPE_DOMAIN_V1, PROFILE_ENVELOPE_MAX_LEN,
    PROFILE_ENVELOPE_MIN_LEN, PROFILE_SIG_DOMAIN, PROFILE_SIG_LEN, PROFILE_VERSION_V1,
    PROFILE_VERSION_V2, PROFILE_VERSION_V3, PROFILE_VERSION_V6, USERNAME_CERT_LEN,
};
pub use profile_request::{
    compose_profile_request, decode_profile_request, should_request_profiles,
    try_unwrap_profile_request, ProfileRequest, ProfileRequestError,
    PROFILE_REQUEST_ENVELOPE_DOMAIN_V1, PROFILE_REQUEST_ENVELOPE_LEN,
    PROFILE_REQUEST_MIN_INTERVAL_MS, PROFILE_REQUEST_PAYLOAD_LEN, PROFILE_REQUEST_VERSION_V1,
};
pub use pseudonym::{Direction, IdentityPseudonym, PerRelationship, StrangerInbox, PSEUDONYM_LEN};
pub use publisher_device_request::{
    compose_publisher_device_request, decode_publisher_device_request,
    try_unwrap_publisher_device_request, PublisherDeviceAction, PublisherDeviceRequest,
    PublisherDeviceRequestError, PUBLISHER_DEVICE_REQUEST_ENVELOPE_DOMAIN_V1,
    PUBLISHER_DEVICE_REQUEST_ENVELOPE_MAX_LEN, PUBLISHER_DEVICE_REQUEST_ENVELOPE_MIN_LEN,
};
pub use publisher_reader_key::{
    compose_publisher_reader_key, decode_publisher_reader_key, try_unwrap_publisher_reader_key,
    PublisherReaderKey, PublisherReaderKeyError, PUBLISHER_READER_KEY_ENVELOPE_DOMAIN_V1,
    PUBLISHER_READER_KEY_ENVELOPE_LEN,
};
pub use push_routing::{
    compose_push_routing_secret, decode_push_routing_secret, derive_push_gid,
    derive_push_wake_public_key, derive_push_wake_signing_seed, derive_push_wake_unlock_key,
    push_routing_secret_is_newer, try_unwrap_push_routing_secret, PushRoutingSecretEnvelope,
    PushRoutingSecretError, PUSH_GID_DERIVE_DOMAIN_V1, PUSH_GID_LEN,
    PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1, PUSH_ROUTING_SECRET_LEN,
    PUSH_ROUTING_SECRET_MINT_EPOCH_LEN, PUSH_ROUTING_SECRET_PAYLOAD_LEN,
    PUSH_ROUTING_SECRET_VERSION_V2, PUSH_WAKE_KEY_LEN, PUSH_WAKE_SIGN_DERIVE_DOMAIN_V1,
    PUSH_WAKE_UNLOCK_DERIVE_DOMAIN_V1,
};
pub use push_wake::{
    is_well_formed_subscription_len, open_push_subscription, open_push_wake,
    seal_push_subscription, seal_push_subscription_for_secret, seal_push_wake, OpenedPushWake,
    PushGwKey, PushProvider, PushTokenEntry, PushWakeError, PushWakeKind,
    MAX_PUSH_SUBSCRIPTION_LEN, MAX_PUSH_TOKENS, MAX_PUSH_TOKEN_LEN, MIN_PUSH_SUBSCRIPTION_LEN,
    PUSH_GW_KEY_LEN, PUSH_SUBSCRIPTION_ID_LEN, PUSH_WAKE_HEADER_LEN, PUSH_WAKE_LEN,
    PUSH_WAKE_NONCE_LEN,
};
pub use reach::{
    classify_contact_key, contact_key_row_verdict, contact_key_versions, continuity_reach_seed,
    reach_seed, reach_verifying_key, ContactKeyRowVerdict, ContactKeyStanding, ReachClass,
    ReachError, ReachGrant, CONTINUITY_ACCOUNT_ID_LEN, CONTINUITY_REACH_SEED_DOMAIN,
    MAX_REACH_SCOPE_ID_LEN, REACH_GRANT_LEN, REACH_SEED_DOMAIN, REACH_SEED_LEN,
    RETIRED_CONTACT_VERSIONS_KEPT,
};
pub use registrar_seed::{
    compose_registrar_seed, decode_registrar_seed, registrar_public_key, try_unwrap_registrar_seed,
    RegistrarSeedEnvelope, RegistrarSeedError, RegistrarSeedRank,
    REGISTRAR_SEED_ENVELOPE_DOMAIN_V1, REGISTRAR_SEED_ENVELOPE_LEN, REGISTRAR_SEED_VERSION_V2,
};
pub use request::{
    compose, decode_and_verify, open_envelope, ContactRequest, LinkAnswer, OpenedRequest,
    ACCOUNT_ID_LEN, LINK_ANSWER_LEN,
};
pub use safety::{
    safety_number, safety_number_bytes, SAFETY_NUMBER_DIGITS, SAFETY_NUMBER_DIGITS_PER_GROUP,
    SAFETY_NUMBER_GROUPS,
};
pub use shared_channels::{
    compose_shared_channels, decode_shared_channels, shared_channels_is_newer,
    try_unwrap_shared_channels, SharedChannel, SharedChannels, SharedChannelsError,
    SHARED_CHANNELS_ENVELOPE_DOMAIN_V1, SHARED_CHANNELS_ENVELOPE_MAX_LEN,
    SHARED_CHANNELS_ENVELOPE_MIN_LEN, SHARED_CHANNELS_MAX, SHARED_CHANNELS_VERSION_V1,
};
pub use username::{
    check_canonical_username, login_handle, UsernameError, LOGIN_HANDLE_DOMAIN, LOGIN_HANDLE_LEN,
    USERNAME_MAX_LEN, USERNAME_MIN_LEN,
};
pub use visibility::{
    compose_reach_state, compose_visibility, decode_reach_state, decode_visibility,
    merge_reach_state, reach_state_is_newer, try_unwrap_reach_state, try_unwrap_visibility,
    visibility_is_newer, ContactsForm, GroupsForm, ReachState, ReachStateEntry, ReachStateMerge,
    ScopeId, ScopeKind, VisibilityError, VisibilityMode, VisibilitySetting, CONTACT_ACCOUNT_ID_LEN,
    MAX_CHOSEN_CONTACTS, MAX_CHOSEN_SCOPES, MAX_REACH_STATE_ENTRIES, MAX_REACH_STATE_ENVELOPE_LEN,
    MAX_SCOPE_ID_LEN, MAX_VISIBILITY_ENVELOPE_LEN, REACH_STATE_ENVELOPE_DOMAIN_V1,
    VISIBILITY_ENVELOPE_DOMAIN_V1,
};
pub use x25519_binding::{
    sign_x25519_binding, verify_x25519_binding, X25519BindingError, X25519_BINDING_DOMAIN,
    X25519_BINDING_LEN,
};

/// Entry points for the fuzz harnesses in `crates/lakko-contacts/fuzz`.
///
/// Not part of the supported API: it reaches parsers that sit behind a
/// cryptographic check a fuzzer cannot pass on random input.
#[doc(hidden)]
pub mod fuzz_api {
    /// Decode a group-invite preview **plaintext** (the bytes inside the
    /// AEAD of a v2 `/g/` capability). `None` when the decoder refuses,
    /// `Some(true)` when the accepted preview validates and re-encodes to the
    /// same bytes, `Some(false)` on a decoder / encoder disagreement (a bug).
    #[must_use]
    pub fn group_invite_preview_plaintext(plain: &[u8]) -> Option<bool> {
        crate::group_invite::fuzz_preview_plaintext(plain)
    }

    /// Open a sealed invite link (any kind, chosen by `kind_byte`) under
    /// `secret` and verify the capability inside. `None` when it refuses;
    /// `Some(true)` when whatever opened re-seals and re-opens to the same
    /// plaintext, `Some(false)` on a disagreement (a bug).
    #[must_use]
    pub fn open_sealed_link(kind_byte: u8, secret: &[u8], sealed: &[u8]) -> Option<bool> {
        crate::link_seal::fuzz_open(kind_byte, secret, sealed)
    }

    /// Split a contact request's **sealed plaintext** (the bytes inside the
    /// sealed envelope, which a fuzzer cannot reach through the AEAD) into
    /// profile, reply grant and intro. `None` when the splitter refuses;
    /// `Some(true)` when what it accepted re-frames to the same bytes (a
    /// profile only when it decoded, a reply grant always class `reply`),
    /// `Some(false)` on a disagreement (a bug).
    #[must_use]
    pub fn contact_request_plaintext(plain: &[u8]) -> Option<bool> {
        crate::request::fuzz_split(plain)
    }

    /// Decode an `/i/` capability's inviter preview (ADR-0079) directly: the
    /// owner signature hides it from a fuzzer otherwise. `None` when the
    /// decoder refuses; `Some(true)` when the accepted preview validates and
    /// re-encodes to the same bytes, `Some(false)` on a disagreement (a bug).
    #[must_use]
    pub fn invite_preview(bytes: &[u8]) -> Option<bool> {
        crate::invite::fuzz_preview(bytes)
    }

    /// Open `row` as a mailbox row (ADR-0078) under a fixed key. `None` when
    /// the open refuses; `Some(true)` when what opened re-seals to a row
    /// that opens to the same kind and body, `Some(false)` otherwise (a bug).
    #[must_use]
    pub fn mailbox_row_open(row: &[u8]) -> Option<bool> {
        crate::mailbox::fuzz_open(row)
    }

    /// Parse an opened mailbox plaintext (`kind || body_len || body ||
    /// zeros`), which a fuzzer cannot reach through the AEAD. `None` when it
    /// refuses; `Some(true)` when the body fits the plaintext.
    #[must_use]
    pub fn mailbox_plaintext(plain: &[u8]) -> Option<bool> {
        crate::mailbox::fuzz_plaintext(plain)
    }

    /// Decode a mailbox binding and verify it against a fixed account key.
    /// `None` when the decode refuses; `Some(true)` when it re-encodes to
    /// the same bytes.
    #[must_use]
    pub fn mailbox_binding_decode(bytes: &[u8]) -> Option<bool> {
        crate::mailbox::fuzz_binding(bytes)
    }

    /// Open `wire` as a sealed wake (ADR-0078 section 3) under a fixed
    /// push-gw key. `None` when the open refuses; `Some(true)` when what
    /// opened carries a known kind and a verified signature.
    #[must_use]
    pub fn push_wake_open(wire: &[u8]) -> Option<bool> {
        crate::push_wake::fuzz_open_wake(wire)
    }

    /// Parse and verify a wake plaintext, which a fuzzer cannot reach
    /// through the AEAD. `None` when it refuses; `Some(true)` when the
    /// accepted wake names the `push_gid` it carried.
    #[must_use]
    pub fn push_wake_plaintext(plain: &[u8]) -> Option<bool> {
        crate::push_wake::fuzz_wake_plaintext(plain)
    }

    /// Parse a subscription token-list plaintext, which a fuzzer cannot
    /// reach through the AEAD. `None` when it refuses; `Some(true)` when
    /// what parsed re-seals to a row that opens to the same entries.
    #[must_use]
    pub fn push_subscription_plaintext(plain: &[u8]) -> Option<bool> {
        crate::push_wake::fuzz_subscription_plaintext(plain)
    }

    /// Decode `bytes` as a link-request claim (ADR-0079) and check that the
    /// dispatcher agrees. `None` when the decoder refuses; `Some(true)` when
    /// the accepted claim re-composes to the identical bytes and the
    /// dispatcher classifies them as the same claim, `Some(false)` on a
    /// disagreement (a bug).
    #[must_use]
    pub fn link_claim_decode(bytes: &[u8]) -> Option<bool> {
        let claim = crate::link_claim::decode_link_claim(bytes).ok()?;
        let recomposed = crate::link_claim::compose_link_claim(&claim) == bytes;
        let dispatched = matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(bytes),
            Ok(crate::incoming_envelope::IncomingEnvelope::LinkClaim(c)) if c == claim
        );
        Some(recomposed && dispatched)
    }

    /// `None` when `bytes` is not a well-formed group contact request
    /// (ADR-0086 §6); else `Some(true)` when it re-composes to exactly
    /// `bytes`, lies within the advertised lengths, and the dispatcher
    /// classifies it as the same envelope, and the receiver's rule under a
    /// fixed key refuses a box the fuzzer could not have sealed to it,
    /// `Some(false)` on a disagreement (a bug).
    #[must_use]
    pub fn group_contact_request_decode(bytes: &[u8]) -> Option<bool> {
        let envelope = crate::group_contact_request::decode_group_contact_request(bytes).ok()?;
        let recomposed = crate::group_contact_request::compose_group_contact_request(&envelope)
            .ok()
            .as_deref()
            == Some(bytes);
        let within = (crate::group_contact_request::GROUP_CONTACT_REQUEST_ENVELOPE_MIN_LEN
            ..=crate::group_contact_request::GROUP_CONTACT_REQUEST_ENVELOPE_MAX_LEN)
            .contains(&bytes.len());
        let dispatched = matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(bytes),
            Ok(crate::incoming_envelope::IncomingEnvelope::GroupContactRequest(e)) if e == envelope
        );
        // The receiver's rule never panics on a box it cannot open.
        let opened = crate::group_contact_request::check_group_contact_request(
            &envelope,
            &lakko_crypto::agreement::SecretKey::from_bytes([0x5A; 32]),
            &[0u8; 32],
        );
        let refused = opened.is_err();
        Some(recomposed && within && dispatched && refused)
    }

    /// `None` when `bytes` is not a well-formed link-founded note (ADR-0079);
    /// else `Some(true)` when it re-composes to exactly `bytes`, lies within
    /// the advertised lengths, and the dispatcher classifies it as the same
    /// note, `Some(false)` on a disagreement (a bug).
    #[must_use]
    pub fn link_founded_decode(bytes: &[u8]) -> Option<bool> {
        let note = crate::link_founded::decode_link_founded(bytes).ok()?;
        let recomposed = crate::link_founded::compose_link_founded(&note)
            .ok()
            .as_deref()
            == Some(bytes);
        let within = (crate::link_founded::LINK_FOUNDED_ENVELOPE_MIN_LEN
            ..=crate::link_founded::LINK_FOUNDED_ENVELOPE_MAX_LEN)
            .contains(&bytes.len());
        let dispatched = matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(bytes),
            Ok(crate::incoming_envelope::IncomingEnvelope::LinkFounded(n)) if n == note
        );
        Some(recomposed && within && dispatched)
    }

    /// `None` when `bytes` is not a well-formed open-items envelope
    /// (ADR-0092 section 3.9); else `Some(true)` when it re-composes to
    /// exactly `bytes` within the length bounds, the dispatcher classifies it
    /// the same, and assembling it never panics; `Some(false)` on a
    /// disagreement (a bug).
    #[must_use]
    pub fn open_items_decode(bytes: &[u8]) -> Option<bool> {
        let items = crate::open_items::decode_open_items(bytes).ok()?;
        let recomposed = crate::open_items::compose_open_items(&items)
            .ok()
            .as_deref()
            == Some(bytes);
        let within = (crate::open_items::OPEN_ITEMS_ENVELOPE_MIN_LEN
            ..=crate::open_items::OPEN_ITEMS_ENVELOPE_MAX_LEN)
            .contains(&bytes.len());
        let dispatched = matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(bytes),
            Ok(crate::incoming_envelope::IncomingEnvelope::OpenItems(ref d)) if *d == items
        );
        let received = [crate::open_items::ReceivedOpenItems {
            sender_account_key: [1; 32],
            items: &items,
        }];
        let assembled =
            crate::open_items::assemble_open_items(&received, &std::collections::BTreeSet::new());
        let refs_known = assembled.iter().all(|a| {
            items
                .items
                .iter()
                .any(|e| e.header.poll_ref == a.header.poll_ref)
        });
        Some(recomposed && within && dispatched && refs_known)
    }

    /// `None` when `bytes` is not a well-formed poll-ballots envelope
    /// (ADR-0092 section 3.9); else `Some(true)` when it re-composes to
    /// exactly `bytes` within the bounds and the dispatcher classifies it
    /// the same, `Some(false)` on a disagreement (a bug).
    #[must_use]
    pub fn poll_ballots_decode(bytes: &[u8]) -> Option<bool> {
        let ballots = crate::poll_ballots::decode_poll_ballots(bytes).ok()?;
        let recomposed = crate::poll_ballots::compose_poll_ballots(&ballots)
            .ok()
            .as_deref()
            == Some(bytes);
        let within = (crate::poll_ballots::POLL_BALLOTS_ENVELOPE_MIN_LEN
            ..=crate::poll_ballots::POLL_BALLOTS_ENVELOPE_MAX_LEN)
            .contains(&bytes.len());
        let dispatched = matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(bytes),
            Ok(crate::incoming_envelope::IncomingEnvelope::PollBallots(ref d)) if *d == ballots
        );
        Some(recomposed && within && dispatched)
    }

    /// Decode `bytes` as a handle statement (ADR-0087 section 3). `None`
    /// when the decoder refuses; `Some(true)` when what decoded re-encodes
    /// to the same bytes (and verifying it never panicked).
    #[must_use]
    pub fn handle_statement_decode(bytes: &[u8]) -> Option<bool> {
        crate::handle::fuzz_decode(bytes)
    }

    /// Decode `bytes` as a group's or channel's record payload and as a
    /// public record opened under a fixed OPRF output (ADR-0087 section 4).
    /// `None` when both refuse; `Some(true)` when what decoded re-encodes to
    /// the same bytes.
    #[must_use]
    pub fn handle_record_decode(bytes: &[u8]) -> Option<bool> {
        crate::handle_record::fuzz_decode(bytes)
    }

    /// Decode `bytes` as a conversation-handle envelope (ADR-0087 section
    /// 11) and check the dispatcher agrees. `None` when the decoder refuses;
    /// `Some(true)` when it re-composes to the same bytes within the
    /// advertised lengths and dispatches as the same envelope.
    #[must_use]
    pub fn conversation_handle_decode(bytes: &[u8]) -> Option<bool> {
        let ok = crate::conversation_handle::fuzz_decode(bytes)?;
        let dispatched = matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(bytes),
            Ok(crate::incoming_envelope::IncomingEnvelope::ConversationHandle(_))
        );
        Some(ok && dispatched)
    }

    /// Decode `bytes` as a knock-seed envelope (ADR-0087 section 5) and check
    /// the dispatcher agrees. `None` when the decoder refuses; `Some(true)`
    /// when it re-encodes to the same bytes at the fixed length and
    /// dispatches as the same envelope.
    #[must_use]
    pub fn knock_seed_decode(bytes: &[u8]) -> Option<bool> {
        let ok = crate::knock_seed::fuzz_decode(bytes)?;
        let dispatched = matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(bytes),
            Ok(crate::incoming_envelope::IncomingEnvelope::KnockSeed(_))
        );
        Some(ok && dispatched)
    }

    /// Decode `bytes` as an admission-seed envelope (ADR-0087 section 7) and
    /// check the dispatcher agrees. `None` when the decoder refuses;
    /// `Some(true)` when it re-encodes to the same bytes at the fixed length
    /// and dispatches as the same envelope.
    #[must_use]
    pub fn admission_seed_decode(bytes: &[u8]) -> Option<bool> {
        let ok = crate::admission_seed::fuzz_decode(bytes)?;
        let dispatched = matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(bytes),
            Ok(crate::incoming_envelope::IncomingEnvelope::AdmissionSeed(_))
        );
        Some(ok && dispatched)
    }

    /// Decode `bytes` as a conversation re-entry request (ADR-0091 phase 2)
    /// and check the dispatcher agrees. `None` when the decoder refuses;
    /// `Some(true)` when it re-composes to the same bytes within the
    /// advertised bounds and dispatches as the same request. The opened
    /// knock-row plaintext parser is driven on the same bytes.
    #[must_use]
    pub fn conversation_reentry_decode(bytes: &[u8]) -> Option<bool> {
        let ok = crate::conversation_reentry::fuzz_decode(bytes)?;
        let request = crate::conversation_reentry::decode_conversation_reentry(bytes).ok()?;
        let dispatched = matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(bytes),
            Ok(crate::incoming_envelope::IncomingEnvelope::ConversationReentry(r)) if r == request
        );
        Some(ok && dispatched)
    }

    /// Open `bytes` as a sealed gap history (ADR-0091 section 7) under a
    /// fixed gap secret and conversation: never accepted, since a fuzzer
    /// cannot sign and seal to that key; the signed-plaintext parser behind
    /// the box accepts nothing a fuzzer writes; and a bundle this crate signs
    /// and seals to that key opens back to itself and its signer, with the
    /// signer key nowhere in the clear. `true` when all hold.
    #[must_use]
    pub fn gap_history_open(bytes: &[u8]) -> bool {
        crate::gap_history::fuzz_open(bytes)
    }

    /// Verify `bytes` as an account X25519 binding: nothing verifies under
    /// a fixed key the fuzzer does not hold, and a binding this crate signs
    /// verifies. `true` when both hold.
    #[must_use]
    pub fn x25519_binding_verify(bytes: &[u8]) -> bool {
        crate::x25519_binding::fuzz_verify(bytes)
    }

    /// Decode `bytes` as a continuity binding (ADR-0101 section 2): whatever
    /// decodes re-encodes to the same bytes, never verifies under a fixed
    /// identity key the fuzzer does not hold, and a binding that key signs
    /// verifies inside its window. `None` when the decoder refuses;
    /// `Some(true)` when all hold.
    #[must_use]
    pub fn continuity_binding_decode(bytes: &[u8]) -> Option<bool> {
        crate::continuity::fuzz_decode(bytes)
    }

    /// Decode `bytes` as a channel return (ADR-0101 section 7) and check
    /// the dispatcher agrees. `None` when the decoder refuses; `Some(true)`
    /// when it re-composes to the same bytes within the advertised bounds,
    /// its card binding never verifies under a fixed key the fuzzer does not
    /// hold, and it dispatches as the same return.
    #[must_use]
    pub fn channel_return_decode(bytes: &[u8]) -> Option<bool> {
        let ok = crate::channel_return::fuzz_decode(bytes)?;
        let ret = crate::channel_return::decode_channel_return(bytes).ok()?;
        let dispatched = matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(bytes),
            Ok(crate::incoming_envelope::IncomingEnvelope::ChannelReturn(r)) if r == ret
        );
        Some(ok && dispatched)
    }

    /// Decode `bytes` as a history request (ADR-0101 section 9) and check
    /// the dispatcher agrees. `None` when the decoder refuses; `Some(true)`
    /// when it re-composes to the same bytes at the fixed length and
    /// dispatches as the same request.
    #[must_use]
    pub fn history_request_decode(bytes: &[u8]) -> Option<bool> {
        let ok = crate::history_request::fuzz_decode(bytes)?;
        let req = crate::history_request::decode_history_request(bytes).ok()?;
        let dispatched = matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(bytes),
            Ok(crate::incoming_envelope::IncomingEnvelope::HistoryRequest(r)) if r == req
        );
        Some(ok && dispatched)
    }

    /// Decode `bytes` as a knock-key envelope (ADR-0091 phase 2, route 3)
    /// and check the dispatcher agrees. `None` when the decoder refuses;
    /// `Some(true)` when it re-composes to the same bytes at the fixed length
    /// and dispatches as the same key.
    #[must_use]
    pub fn knock_key_decode(bytes: &[u8]) -> Option<bool> {
        let ok = crate::knock_key::fuzz_decode(bytes)?;
        let key = crate::knock_key::decode_knock_key(bytes).ok()?;
        let dispatched = matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(bytes),
            Ok(crate::incoming_envelope::IncomingEnvelope::KnockKey(k)) if k == key
        );
        Some(ok && dispatched)
    }

    /// Decode `bytes` as a channel rotate-request envelope (ADR-0087 section
    /// 15, amended 2026-11-08) and check the dispatcher agrees. `None` when
    /// the decoder refuses; `Some(true)` when it re-composes to the same
    /// bytes at the fixed length and dispatches as the same channel.
    #[must_use]
    pub fn channel_rotate_request_decode(bytes: &[u8]) -> Option<bool> {
        let ok = crate::channel_rotate_request::fuzz_decode(bytes)?;
        let channel = crate::channel_rotate_request::decode_channel_rotate_request(bytes).ok()?;
        let dispatched = matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(bytes),
            Ok(crate::incoming_envelope::IncomingEnvelope::ChannelRotateRequest(c)) if c == channel
        );
        Some(ok && dispatched)
    }

    /// Decode `bytes` as a publisher device-request envelope (ADR-0090
    /// section 2.2) and check the dispatcher agrees. `None` when the decoder
    /// refuses; `Some(true)` when it re-composes to the same bytes within the
    /// advertised bounds and dispatches as the same request.
    #[must_use]
    pub fn publisher_device_request_decode(bytes: &[u8]) -> Option<bool> {
        let ok = crate::publisher_device_request::fuzz_decode(bytes)?;
        let request =
            crate::publisher_device_request::decode_publisher_device_request(bytes).ok()?;
        let dispatched = matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(bytes),
            Ok(crate::incoming_envelope::IncomingEnvelope::PublisherDeviceRequest(r)) if r == request
        );
        Some(ok && dispatched)
    }

    /// Decode `bytes` as a channel admission-mark envelope (ADR-0090 section
    /// 6) and check the dispatcher agrees. `None` when the decoder refuses;
    /// `Some(true)` when it re-composes to the same bytes at the fixed length
    /// and dispatches as the same mark.
    #[must_use]
    pub fn channel_admission_mark_decode(bytes: &[u8]) -> Option<bool> {
        let ok = crate::channel_admission_mark::fuzz_decode(bytes)?;
        let mark = crate::channel_admission_mark::decode_channel_admission_mark(bytes).ok()?;
        let dispatched = matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(bytes),
            Ok(crate::incoming_envelope::IncomingEnvelope::ChannelAdmissionMark(m)) if m == mark
        );
        Some(ok && dispatched)
    }

    /// Decode `bytes` as a publisher reader-key envelope (UX 15.1, live walk
    /// 2026-11-24) and check the dispatcher agrees. `None` when the decoder
    /// refuses; `Some(true)` when it re-composes to the same bytes at the
    /// fixed length and dispatches as the same key.
    #[must_use]
    pub fn publisher_reader_key_decode(bytes: &[u8]) -> Option<bool> {
        let ok = crate::publisher_reader_key::fuzz_decode(bytes)?;
        let key = crate::publisher_reader_key::decode_publisher_reader_key(bytes).ok()?;
        let dispatched = matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(bytes),
            Ok(crate::incoming_envelope::IncomingEnvelope::PublisherReaderKey(k)) if k == key
        );
        Some(ok && dispatched)
    }

    /// Decode `bytes` as a channel epoch-seeds envelope (ADR-0087 section 7,
    /// amended 2026-11-08) and check the dispatcher agrees. `None` when the
    /// decoder refuses; `Some(true)` when it re-encodes to the same bytes at
    /// the length its count names and dispatches as the same envelope.
    #[must_use]
    pub fn channel_epoch_seeds_decode(bytes: &[u8]) -> Option<bool> {
        let ok = crate::channel_epoch_seeds::fuzz_decode(bytes)?;
        let dispatched = matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(bytes),
            Ok(crate::incoming_envelope::IncomingEnvelope::ChannelEpochSeeds(_))
        );
        Some(ok && dispatched)
    }

    /// Decode `bytes` as a channel read-key envelope (ADR-0087 phase B) and
    /// check the dispatcher agrees. `None` when the decoder refuses;
    /// `Some(true)` when it re-encodes to the same bytes at the fixed length
    /// and dispatches as the same envelope.
    #[must_use]
    pub fn channel_read_key_decode(bytes: &[u8]) -> Option<bool> {
        let ok = crate::channel_read_key::fuzz_decode(bytes)?;
        let dispatched = matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(bytes),
            Ok(crate::incoming_envelope::IncomingEnvelope::ChannelReadKey(
                _
            ))
        );
        Some(ok && dispatched)
    }

    /// Decode `bytes` as a channel invitation card (ADR-0087 section 14) and
    /// try to open it. `None` when the decoder refuses; `Some(true)` when
    /// what decoded re-encodes to the same bytes (opening never panicked).
    #[must_use]
    pub fn channel_invite_card_open(bytes: &[u8]) -> Option<bool> {
        crate::group_invite::fuzz_channel_invite_card(bytes)
    }

    /// Open `row` as a sealed knock (ADR-0087 section 5) under a fixed seed.
    /// `None` when the open refuses; `Some(true)` when what opened re-seals
    /// and re-opens to the same request.
    #[must_use]
    pub fn sealed_knock_open(row: &[u8]) -> Option<bool> {
        crate::knock::fuzz_open(row)
    }

    /// Parse an opened knock plaintext, which a fuzzer cannot reach through
    /// the AEAD. `None` when the parser refuses; `Some(true)` when what
    /// parsed re-seals and re-opens to the same request.
    #[must_use]
    pub fn knock_plaintext(plain: &[u8]) -> Option<bool> {
        crate::knock::fuzz_plaintext(plain)
    }
}
