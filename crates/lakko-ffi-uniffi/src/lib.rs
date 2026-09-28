// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI bindings — Swift (iOS / iPadOS / macOS / watchOS) and Kotlin (Android).
//!
//! This crate exposes the stable parts of [`lakko_mls`] through UniFFI's
//! procedural-macro API. The generated `.swift` / `.kt` files are produced
//! by `uniffi-bindgen` (see `just regen-bindings`).
//!
//! Phase 0 surface — the absolute minimum to drive a chat group:
//!
//! - [`Identity::new`] / [`Identity::signature_public_key`] / [`Identity::create_key_package`]
//! - [`Group::create`] / [`Group::join_from_welcome`]
//! - [`Group::add_member_from_wire`] / [`Group::remove_member`]
//! - [`Group::create_application_message`] / [`Group::process_incoming`]
//! - [`Group::export_secret`]
//! - [`Group::epoch`] / [`Group::member_count`]
//!
//! Phase 1.15d1 — auth surface, mirror of `lakko-ffi-wasm`:
//!
//! - [`auth::OpaqueRegister`] / [`auth::OpaqueLogin`] — OPAQUE-PAKE client.
//! - [`mnemonic::RecoveryMnemonic`] — 24-word BIP39 recovery passphrase.
//! - [`pow::PowSolver`] / [`pow::PowProof`] — Equix proof-of-work.
//!
//! Public-key serialization, error types and message-routing structs are
//! exposed as plain UniFFI records / enums.

#![forbid(unsafe_code)]

use std::sync::{Arc, Mutex};

use lakko_mls::group::MemberAddition;
use lakko_mls::{
    group, key_package, ChannelId as MlsChannelId, DeviceGroup as MlsDeviceGroup,
    Group as MlsGroup, Identity as MlsIdentity, Incoming as MlsIncoming,
    PublisherGroup as MlsPublisherGroup, RefusalReason as MlsRefusalReason, Role as MlsRole,
    RoleTable as MlsRoleTable,
};

pub mod anon_token;
pub mod attachments;
pub mod auth;
pub mod backup;
pub mod channels;
pub mod contacts;
pub mod device_link;
pub mod discovery;
pub mod ds_proof;
pub mod l10n;
pub mod mnemonic;
pub mod pairing;
pub mod pow;
pub mod release;
pub mod sframe;
pub mod sfu_signaling;
pub mod shamir;
pub mod transport;
pub mod ui_rules;
pub mod ws;

uniffi::setup_scaffolding!();

/// Errors surfaced to Swift / Kotlin callers.
///
/// Variants intentionally collapse internal detail; callers branch on the
/// variant, not on the message string. Inner messages are for logs only.
#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum FfiError {
    /// Identity / signing-key error.
    #[error("identity error: {0}")]
    Identity(String),

    /// KeyPackage creation, validation or serialization error.
    #[error("key package error: {0}")]
    KeyPackage(String),

    /// Group state-machine error.
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

    /// OPAQUE-PAKE registration / login error.
    ///
    /// Same elision policy as the underlying `AuthClientError`:
    /// "wrong password" and "unknown account" are deliberately
    /// indistinguishable on the wire and at the FFI.
    #[error("auth error: {0}")]
    Auth(String),

    /// An `OpaqueRegister` / `OpaqueLogin` handle was finalized
    /// twice. Calling `finish` a second time is a wiring bug —
    /// the underlying `opaque-ke` state machine consumes the
    /// session by value, and a re-run would either produce a
    /// fresh-but-unusable envelope or corrupt OPAQUE state.
    /// Surface it loudly.
    #[error("OPAQUE session already finalized")]
    AuthAlreadyFinalized,

    /// BIP39 mnemonic parse / generation error.
    #[error("mnemonic error: {0}")]
    Mnemonic(String),

    /// Equix proof-of-work solve error.
    #[error("pow error: {0}")]
    Pow(String),

    /// Discovery / OPRF blinder error.
    ///
    /// Same elision posture as the wasm side: a normalised
    /// empty-username failure, a base64-decode failure, and a
    /// `voprf` math failure all collapse into this variant.
    #[error("discovery error: {0}")]
    Discovery(String),

    /// An `OprfBlinder` handle was finalized twice. Single-use
    /// is enforced because `OprfClient::finalize` consumes the
    /// blinding state by value; a re-run would either corrupt
    /// math or produce a nonsense output without a clear
    /// failure signal.
    #[error("OPRF blinder already finalized")]
    DiscoveryAlreadyFinalized,

    /// Contact-request envelope / pseudonym error.
    ///
    /// Same elision posture as the wasm side: the underlying
    /// `ContactError` distinguishes wire-format / signature /
    /// AEAD / argument categories internally; UI-side handling
    /// would not benefit from carving that into separate
    /// variants here.
    #[error("contacts error: {0}")]
    Contacts(String),

    /// Crypto primitive (Ed25519 / X25519 / AEAD) error at the
    /// FFI boundary. Surfaced when input bytes don't decode to a
    /// valid public / private key, or an AEAD op fails — both
    /// are typically caller-side wiring bugs.
    #[error("crypto error: {0}")]
    Crypto(String),

    /// Attachment encryption / decryption / wire-format error
    /// (Phase 1.19b). Same elision posture as the rest of the
    /// `FfiError` enum: a wrong-length key, an AEAD tag mismatch,
    /// an oversize plaintext, and a malformed base64 in the
    /// envelope all collapse into this variant.
    #[error("attachment error: {0}")]
    Attachment(String),

    /// Shamir's-Secret-Sharing split / combine / encode / decode
    /// error (Phase 1.21b). Same elision posture as the rest:
    /// invalid threshold, duplicate share indices, mismatched
    /// share lengths, oversize secret, malformed wire-form
    /// share string — all collapse into this variant.
    #[error("shamir error: {0}")]
    Shamir(String),

    /// Backup envelope / payload error (Phase 2a1c). Same
    /// elision posture as the rest: wrong recovery phrase,
    /// tampered envelope bytes, malformed payload, wrong-length
    /// account_id / ed25519_seed inputs — all collapse here.
    /// Also carries the **hard fail-closed** class of backup-head
    /// verify failures (ADR-0037): a malformed / bad-length /
    /// bad-version head, a signature that does not verify, an
    /// `account_id` mismatch, or an `envelope_hash` mismatch —
    /// all unambiguous tamper / corruption signals with no
    /// legitimate cause, so no user override.
    #[error("backup error: {0}")]
    Backup(String),

    /// A backup head verified *authentically* but its `signed_at`
    /// falls outside the client's freshness window (ADR-0037 §6,
    /// G-11). Kept DISTINCT from [`FfiError::Backup`] so a
    /// restoring client can treat staleness as a
    /// **user-acknowledged override** ("this backup is older than
    /// expected — restore anyway?") rather than a hard
    /// fail-closed refusal: a stale head is indistinguishable from
    /// a rollback replay (so the default is refuse, invariant #4)
    /// but a genuinely long-dormant user holds a real stale latest
    /// head, so the downgrade is offered behind deliberate human
    /// consent. Every other backup-head failure stays
    /// [`FfiError::Backup`] (hard fail-closed).
    #[error("backup head is stale (signed_at outside the freshness window)")]
    BackupHeadStale,

    /// Channel post / subscribe envelope error (Phase 4 G4-2-
    /// crypto). Same elision posture: wrong-length seed,
    /// channel-id mismatch, tampered ChannelPost ciphertext,
    /// Ed25519 verify failure on a post signature — all
    /// collapse here.
    #[error("channels error: {0}")]
    Channels(String),

    /// Call-side cryptography error (G3-sframe). Covers:
    /// wrong-length CallKey seed, SFrame counter
    /// exhaustion, AEAD seal / open failure on
    /// ParticipantKey. Same elision posture as the
    /// other variants — the message is for logs, the
    /// variant for the wire.
    #[error("call crypto error: {0}")]
    Call(String),

    /// SFU signaling wire-frame error (SFU multi-party subsprint
    /// 1c). Covers: a malformed / unknown-kind / oversize server
    /// frame off the WebSocket, and a base64url decode failure on
    /// a relay payload. Same elision posture as the rest — the
    /// message is for logs, the variant for the wire.
    #[error("sfu signaling error: {0}")]
    Sfu(String),

    /// Anti-censorship transport / tunneled-HTTP error (ADR-0026 Phase 3).
    /// Covers: bad bootstrap config, trust-anchor verify failure, a fail-closed
    /// dial (no transport connected — never a silent direct fallback), and a
    /// tunneled-request transport/parse failure. Message for logs, variant for
    /// the wire.
    #[error("transport error: {0}")]
    Transport(String),

    /// Release-manifest verify error (ADR-0038, "update available" notice).
    /// Covers every [`lakko_attest::ReleaseManifestError`] cause — a malformed /
    /// wrong-length / wrong-version manifest, a signature that does not verify, a
    /// stale manifest, or a bad verifying key. Unlike the backup-head split
    /// (ADR-0037), staleness is NOT carved into a distinct variant: the ADR-0038
    /// UX fails toward "up to date" on ANY verify failure (an unverified manifest
    /// must never drive an update nag), so the caller catches this one variant and
    /// treats it as up-to-date. Message for logs, variant for the wire.
    #[error("release manifest error: {0}")]
    Release(String),

    /// A second ADR-0045 sweep was staged while one is still awaiting its
    /// journal verdict.
    ///
    /// Carved out of [`FfiError::Group`] for the same reason
    /// [`FfiError::BackupHeadStale`] is carved out of [`FfiError::Backup`]: it
    /// is the one refusal on the staged path a caller is expected to **handle**
    /// rather than log. It is never a wire or crypto failure — it means this
    /// device already published a sweep and has not yet told the core whether
    /// that sweep won ([`Group::merge_staged_sweep`]) or lost
    /// ([`Group::abandon_staged_sweep`]). The correct response is to reach the
    /// outstanding verdict, not to retry the stage.
    ///
    /// Building a second sweep is refused **fail-closed** (invariant #4)
    /// because it would silently replace a commit that may already be on the
    /// wire — the loser of that replacement would be a commit peers had
    /// already applied, which is precisely the fork ADR-0045 closes.
    /// The message is the core's own sentence — this variant carries it rather
    /// than restating one, so every membership refusal reads identically in a
    /// log wherever it was raised. (The Swift / Kotlin shape is unchanged: a
    /// `flat_error` gives **every** variant a `message` regardless of its Rust
    /// fields.)
    #[error("{0}")]
    SweepAlreadyStaged(String),

    /// Lock contention / poisoned mutex on the shared Group state.
    #[error("internal lock error")]
    LockPoisoned,

    // ------------------------------------------------------------------
    // Typed membership refusals (ADR-0045 §9.1 item 1 / §12 item 6).
    //
    // One variant per `lakko_mls::RefusalReason`, and the reason is the
    // variant — because `#[uniffi(flat_error)]` lowers ONLY the variant
    // discriminant and the `Display` string, dropping associated data. So a
    // struct field carrying the reason would simply not arrive in Swift or
    // Kotlin, and the variant is the one zero-parse channel this error type
    // has. (Removing `flat_error` would carry the field, and would also change
    // the generated shape and `message` of every existing variant, breaking
    // callers that have nothing to do with membership.)
    //
    // `Display` is the core's sentence verbatim, so logs are unchanged. Select
    // copy from the VARIANT — never from the message, which is written for a
    // developer and is reworded freely. `membership_refusal_class` answers
    // whether waiting helps; do not re-decide that per client.
    //
    // The one refusal that already had a variant, `SweepAlreadyStaged`, keeps
    // it rather than gaining a duplicate under a `Membership` prefix.
    //
    // APPENDED here rather than grouped beside `Group`, deliberately: uniffi
    // lowers a flat error as its variant's ORDINAL, and the generated Swift and
    // Kotlin decode by that number. Inserting mid-enum renumbers every later
    // variant, so bindings generated before this change would silently decode
    // an unrelated error as the wrong variant until `just regen-bindings` runs.
    // Appending leaves every existing ordinal where it was; stale bindings
    // simply fail to recognise the new ones. Keep new variants at the end.
    // ------------------------------------------------------------------
    /// The proposal's sender is not a resolvable member of the group.
    #[error("{0}")]
    MembershipNotAMember(String),

    /// Adding a member needs owner or admin.
    #[error("{0}")]
    MembershipAddNotOwnerOrAdmin(String),

    /// Removing somebody else needs owner or admin.
    #[error("{0}")]
    MembershipRemoveNotOwnerOrAdmin(String),

    /// The group owner's leaf cannot be removed, by anyone.
    #[error("{0}")]
    MembershipOwnerUnremovable(String),

    /// The group owner cannot leave while owner — transfer ownership first.
    #[error("{0}")]
    MembershipOwnerCannotLeave(String),

    /// Changing the role table, ownership transfer included, is owner-only.
    #[error("{0}")]
    MembershipNotOwner(String),

    /// A leaf `Update` proposal, refused for every role.
    #[error("{0}")]
    MembershipLeafUpdateRefused(String),

    /// A proposal type Lakko never emits.
    #[error("{0}")]
    MembershipUnsupportedProposal(String),

    /// The group carries no role table, so it has no committer.
    #[error("{0}")]
    MembershipNoRoleTable(String),

    /// The role table names an owner who is not a member of the group.
    #[error("{0}")]
    MembershipOwnerNotAMember(String),

    /// The commit would delete the group's role table rather than change it.
    #[error("{0}")]
    MembershipRoleTableWouldVanish(String),

    /// This device's role does not authorise it to commit.
    #[error("{0}")]
    MembershipNotACommitter(String),

    /// Another role-table change is already pending. Availability, not
    /// authorisation: waiting is the answer.
    #[error("{0}")]
    MembershipRoleChangePending(String),

    /// A direct commit was refused because proposals are pending. Availability.
    #[error("{0}")]
    MembershipProposalsPending(String),

    /// The pending-proposal set is full. Retryable.
    #[error("{0}")]
    MembershipPendingSetFull(String),

    /// No retained external-join request matches that key — a stale view.
    #[error("{0}")]
    MembershipNoSuchJoinRequest(String),

    /// The joiner named by an approve is already a member.
    #[error("{0}")]
    MembershipJoinerAlreadyAMember(String),

    // ------------------------------------------------------------------
    // Typed username / display-name refusals (ADR-0048 §1, §4).
    //
    // Same shape and the same reason as the membership block above: a
    // `flat_error` lowers only the variant ordinal and the `Display`
    // string, so the reason has to BE the variant for Swift / Kotlin to
    // branch on it without parsing prose. APPENDED at the end for the same
    // ordinal-stability reason — keep new variants after these.
    //
    // The message is the core's English sentence and never carries the
    // offending input (a username is a lookup target; a display name is
    // personal data). Select UI copy from the VARIANT.
    // ------------------------------------------------------------------
    /// A username is empty after normalisation (`""`, `"   "`, `"@"`).
    #[error("{0}")]
    UsernameEmpty(String),

    /// A username is shorter than 3 characters after normalisation.
    #[error("{0}")]
    UsernameTooShort(String),

    /// A username is longer than 32 characters after normalisation.
    #[error("{0}")]
    UsernameTooLong(String),

    /// A username contains a character outside `[a-z0-9_]` after
    /// normalisation — Cyrillic look-alikes, dashes, dots, spaces, emoji.
    #[error("{0}")]
    UsernameInvalidCharacters(String),

    /// A display name contains a line break.
    #[error("{0}")]
    DisplayNameLineBreak(String),

    /// A display name contains a control or format character (including
    /// the right-to-left override and the zero-width characters).
    #[error("{0}")]
    DisplayNameControlOrFormatCharacter(String),

    /// A display name is longer than 64 characters.
    #[error("{0}")]
    DisplayNameTooLong(String),

    /// A display name is longer than 255 UTF-8 bytes.
    #[error("{0}")]
    DisplayNameTooManyBytes(String),

    // ------------------------------------------------------------------
    // ADR-0050 per-device leaves — appended, for the ordinal reason above.
    // ------------------------------------------------------------------
    /// The added leaf's device binding does not verify (ADR-0050).
    #[error("{0}")]
    MembershipDeviceBindingInvalid(String),

    /// The account already holds the maximum number of devices in this group
    /// (ADR-0050).
    #[error("{0}")]
    MembershipDeviceLimitReached(String),

    // ------------------------------------------------------------------
    // ADR-0051 channel metadata — appended, for the ordinal reason above.
    // ------------------------------------------------------------------
    /// A channel metadata record (name + description) violates a bound or
    /// is malformed (ADR-0051 §4). The message starts with the frozen
    /// `lakko_channels::meta::ChannelMetaError::code()` (for example
    /// `name.empty`, `description.tooLong`) followed by `": "` and the
    /// human-readable reason, so a client can select copy from the code
    /// without parsing prose.
    #[error("{0}")]
    ChannelMeta(String),

    // ------------------------------------------------------------------
    // ADR-0054 anonymous tokens — appended, for the ordinal reason above.
    // ------------------------------------------------------------------
    /// An anonymous-token request, finalize or epoch computation refused.
    /// The message starts with the frozen `AnonTokenError::code()` (for
    /// example `proof-verification`, `bad-batch-size`, `unknown-context`)
    /// followed by `": "` and the reason, so a client branches on the code.
    #[error("{0}")]
    AnonToken(String),

    /// `AnonTokenRequest.finalize` was called a second time; the request
    /// (and the blinding factors it held) is consumed by the first call.
    #[error("anonymous-token request already finalized")]
    AnonTokenAlreadyFinalized,

    // ------------------------------------------------------------------
    // ADR-0049 section 2 device link — appended, for the ordinal reason above.
    // ------------------------------------------------------------------
    /// The device-link key confirmation did not verify: the code typed on the
    /// new device is not the code the signed-in device showed, or a value
    /// was changed in transit. The request is spent; start a new one.
    #[error("the device-link code did not match")]
    DeviceLinkWrongCode,

    /// Any other device-link refusal. The message starts with a code
    /// (`invalid-code`, `malformed`, `bad-input`, `already-used`,
    /// `not-ready`, `internal`) followed by `": "` and the reason.
    #[error("{0}")]
    DeviceLink(String),

    // ------------------------------------------------------------------
    // Sealed invite links (2026-09-28 security pass of ADR-0028 / ADR-0009)
    // — appended, for the ordinal reason above.
    // ------------------------------------------------------------------
    /// An invite link (`/i/`, `/c/`, `/g/`) did not open: no or a wrong
    /// secret in the fragment, a blob of another kind, a truncated, tampered
    /// or substituted blob, or a capability that does not verify inside it.
    /// One variant on purpose: the page says "this link is damaged".
    #[error("{0}")]
    InviteLinkDamaged(String),

    // ------------------------------------------------------------------
    // ADR-0058 interface localization — appended, for the ordinal reason
    // above.
    // ------------------------------------------------------------------
    /// A catalog, translation or translation bundle was refused. A client
    /// falls back (bundle -> built-in, built-in -> English) and never shows
    /// this message to a person.
    #[error("{0}")]
    L10n(String),
}

impl From<lakko_contacts::username::UsernameError> for FfiError {
    fn from(e: lakko_contacts::username::UsernameError) -> Self {
        use lakko_contacts::username::UsernameError as U;
        let msg = e.to_string();
        match e {
            U::Empty => Self::UsernameEmpty(msg),
            U::TooShort { .. } => Self::UsernameTooShort(msg),
            U::TooLong { .. } => Self::UsernameTooLong(msg),
            U::InvalidCharacters => Self::UsernameInvalidCharacters(msg),
        }
    }
}

impl From<lakko_contacts::profile::DisplayNameError> for FfiError {
    fn from(e: lakko_contacts::profile::DisplayNameError) -> Self {
        use lakko_contacts::profile::DisplayNameError as D;
        let msg = e.to_string();
        match e {
            D::LineBreak => Self::DisplayNameLineBreak(msg),
            D::ControlOrFormatCharacter => Self::DisplayNameControlOrFormatCharacter(msg),
            D::TooLong { .. } => Self::DisplayNameTooLong(msg),
            D::TooManyBytes { .. } => Self::DisplayNameTooManyBytes(msg),
            // Unreachable from user input on the FFI paths, which trim and
            // map blank to "no name" first; a wire decode reports its own
            // failures through `Contacts`, like every sibling envelope.
            D::Empty | D::NotTrimmed | D::NotUtf8 => Self::Contacts(format!("display name: {msg}")),
        }
    }
}

impl From<lakko_contacts::profile::ProfileError> for FfiError {
    fn from(e: lakko_contacts::profile::ProfileError) -> Self {
        use lakko_contacts::profile::ProfileError as P;
        match e {
            P::DisplayName(d) => d.into(),
            P::Username(u) => u.into(),
            other => Self::Contacts(other.to_string()),
        }
    }
}

impl From<lakko_contacts::group_name::GroupNameError> for FfiError {
    fn from(e: lakko_contacts::group_name::GroupNameError) -> Self {
        use lakko_contacts::group_name::GroupNameError as G;
        match e {
            // The group name reuses the profile display-name rule, so it
            // reuses the typed `DisplayName*` variants too — no new variant,
            // so no ordinal to append (ADR-0051 §1).
            G::Name(d) => d.into(),
            other => Self::Contacts(other.to_string()),
        }
    }
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
            lakko_mls::Error::MembershipRefused { reason, detail } => {
                Self::from_refusal(reason, detail)
            },
        }
    }
}

impl FfiError {
    /// The variant that carries `reason`.
    ///
    /// **The single place the reason -> variant mapping is written.** It is an
    /// exhaustive match with no wildcard, so a new
    /// [`lakko_mls::RefusalReason`] fails to compile here rather than silently
    /// collapsing into a generic error a client cannot tell from any other.
    fn from_refusal(reason: MlsRefusalReason, detail: String) -> Self {
        match reason {
            MlsRefusalReason::NotAMember => Self::MembershipNotAMember(detail),
            MlsRefusalReason::AddNotOwnerOrAdmin => Self::MembershipAddNotOwnerOrAdmin(detail),
            MlsRefusalReason::RemoveNotOwnerOrAdmin => {
                Self::MembershipRemoveNotOwnerOrAdmin(detail)
            },
            MlsRefusalReason::OwnerUnremovable => Self::MembershipOwnerUnremovable(detail),
            MlsRefusalReason::OwnerCannotLeave => Self::MembershipOwnerCannotLeave(detail),
            MlsRefusalReason::NotOwner => Self::MembershipNotOwner(detail),
            MlsRefusalReason::LeafUpdateRefused => Self::MembershipLeafUpdateRefused(detail),
            MlsRefusalReason::UnsupportedProposal => Self::MembershipUnsupportedProposal(detail),
            MlsRefusalReason::NoRoleTable => Self::MembershipNoRoleTable(detail),
            MlsRefusalReason::OwnerNotAMember => Self::MembershipOwnerNotAMember(detail),
            MlsRefusalReason::RoleTableWouldVanish => Self::MembershipRoleTableWouldVanish(detail),
            MlsRefusalReason::NotACommitter => Self::MembershipNotACommitter(detail),
            MlsRefusalReason::RoleChangePending => Self::MembershipRoleChangePending(detail),
            MlsRefusalReason::ProposalsPending => Self::MembershipProposalsPending(detail),
            MlsRefusalReason::PendingSetFull => Self::MembershipPendingSetFull(detail),
            MlsRefusalReason::NoSuchJoinRequest => Self::MembershipNoSuchJoinRequest(detail),
            MlsRefusalReason::JoinerAlreadyAMember => Self::MembershipJoinerAlreadyAMember(detail),
            MlsRefusalReason::SweepAlreadyStaged => Self::SweepAlreadyStaged(detail),
            MlsRefusalReason::DeviceBindingInvalid => Self::MembershipDeviceBindingInvalid(detail),
            MlsRefusalReason::DeviceLimitReached => Self::MembershipDeviceLimitReached(detail),
        }
    }

    /// The inverse of [`Self::from_refusal`], or `None` when this error is not
    /// a membership refusal at all.
    ///
    /// Rust-side only: a `flat_error` cannot be *lifted* back across the FFI
    /// (uniffi generates a `try_lift` that refuses), so this can never be an
    /// exported method taking an error. It exists to pin the mapping's
    /// injectivity in a test — a collision between two reasons would be
    /// invisible and unfixable from the client side — and to document the
    /// correspondence in one readable place. Test-only for that reason.
    #[cfg(test)]
    pub(crate) fn refusal_reason(&self) -> Option<FfiRefusalReason> {
        Some(match self {
            Self::MembershipNotAMember(_) => FfiRefusalReason::NotAMember,
            Self::MembershipAddNotOwnerOrAdmin(_) => FfiRefusalReason::AddNotOwnerOrAdmin,
            Self::MembershipRemoveNotOwnerOrAdmin(_) => FfiRefusalReason::RemoveNotOwnerOrAdmin,
            Self::MembershipOwnerUnremovable(_) => FfiRefusalReason::OwnerUnremovable,
            Self::MembershipOwnerCannotLeave(_) => FfiRefusalReason::OwnerCannotLeave,
            Self::MembershipNotOwner(_) => FfiRefusalReason::NotOwner,
            Self::MembershipLeafUpdateRefused(_) => FfiRefusalReason::LeafUpdateRefused,
            Self::MembershipUnsupportedProposal(_) => FfiRefusalReason::UnsupportedProposal,
            Self::MembershipNoRoleTable(_) => FfiRefusalReason::NoRoleTable,
            Self::MembershipOwnerNotAMember(_) => FfiRefusalReason::OwnerNotAMember,
            Self::MembershipRoleTableWouldVanish(_) => FfiRefusalReason::RoleTableWouldVanish,
            Self::MembershipNotACommitter(_) => FfiRefusalReason::NotACommitter,
            Self::MembershipRoleChangePending(_) => FfiRefusalReason::RoleChangePending,
            Self::MembershipProposalsPending(_) => FfiRefusalReason::ProposalsPending,
            Self::MembershipPendingSetFull(_) => FfiRefusalReason::PendingSetFull,
            Self::MembershipNoSuchJoinRequest(_) => FfiRefusalReason::NoSuchJoinRequest,
            Self::MembershipJoinerAlreadyAMember(_) => FfiRefusalReason::JoinerAlreadyAMember,
            Self::SweepAlreadyStaged(_) => FfiRefusalReason::SweepAlreadyStaged,
            Self::MembershipDeviceBindingInvalid(_) => FfiRefusalReason::DeviceBindingInvalid,
            Self::MembershipDeviceLimitReached(_) => FfiRefusalReason::DeviceLimitReached,
            _ => return None,
        })
    }
}

/// Why a group-membership operation was refused — FFI projection of
/// [`lakko_mls::RefusalReason`] (ADR-0045 §9.1 item 1).
///
/// # Do not select copy by matching the error message
///
/// Every refusal below arrives as its own `FfiError` variant carrying the
/// core's sentence. The **variant** is the contract; the sentence is written
/// for a developer reading a log and is reworded freely. A client that
/// substring-matches prose silently falls through to a generic string — or to
/// the wrong one — the day the core rewords a message, with nothing failing
/// anywhere. That had already happened three different ways across three
/// clients before this type existed, which is why it does.
///
/// This enum is the same set in value form, so a caller can hold a reason, pass
/// it to [`membership_refusal_class`], and let the core answer whether waiting
/// helps rather than re-deciding it per platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiRefusalReason {
    /// The proposal's sender is not a resolvable member of the group.
    NotAMember,
    /// Adding a member needs owner or admin.
    AddNotOwnerOrAdmin,
    /// Removing somebody else needs owner or admin.
    RemoveNotOwnerOrAdmin,
    /// The group owner's leaf cannot be removed, by anyone.
    OwnerUnremovable,
    /// The group owner cannot leave while owner.
    OwnerCannotLeave,
    /// Changing the role table, ownership transfer included, is owner-only.
    NotOwner,
    /// A leaf `Update` proposal, refused for every role.
    LeafUpdateRefused,
    /// A proposal type Lakko never emits.
    UnsupportedProposal,
    /// The group carries no role table, so it has no committer.
    NoRoleTable,
    /// The role table names an owner who is not a member of the group.
    OwnerNotAMember,
    /// The commit would delete the group's role table rather than change it.
    RoleTableWouldVanish,
    /// This device's role does not authorise it to commit.
    NotACommitter,
    /// Another role-table change is already pending.
    RoleChangePending,
    /// A direct commit was refused because proposals are pending.
    ProposalsPending,
    /// The pending-proposal set is full.
    PendingSetFull,
    /// No retained external-join request matches that key — a stale view.
    NoSuchJoinRequest,
    /// The joiner named by an approve is already a member.
    JoinerAlreadyAMember,
    /// A staged sweep is already awaiting its journal verdict.
    SweepAlreadyStaged,
    /// The added leaf's device binding does not verify (ADR-0050).
    DeviceBindingInvalid,
    /// The account already holds the maximum number of devices in this group
    /// (ADR-0050).
    DeviceLimitReached,
}

impl From<MlsRefusalReason> for FfiRefusalReason {
    fn from(r: MlsRefusalReason) -> Self {
        match r {
            MlsRefusalReason::NotAMember => Self::NotAMember,
            MlsRefusalReason::AddNotOwnerOrAdmin => Self::AddNotOwnerOrAdmin,
            MlsRefusalReason::RemoveNotOwnerOrAdmin => Self::RemoveNotOwnerOrAdmin,
            MlsRefusalReason::OwnerUnremovable => Self::OwnerUnremovable,
            MlsRefusalReason::OwnerCannotLeave => Self::OwnerCannotLeave,
            MlsRefusalReason::NotOwner => Self::NotOwner,
            MlsRefusalReason::LeafUpdateRefused => Self::LeafUpdateRefused,
            MlsRefusalReason::UnsupportedProposal => Self::UnsupportedProposal,
            MlsRefusalReason::NoRoleTable => Self::NoRoleTable,
            MlsRefusalReason::OwnerNotAMember => Self::OwnerNotAMember,
            MlsRefusalReason::RoleTableWouldVanish => Self::RoleTableWouldVanish,
            MlsRefusalReason::NotACommitter => Self::NotACommitter,
            MlsRefusalReason::RoleChangePending => Self::RoleChangePending,
            MlsRefusalReason::ProposalsPending => Self::ProposalsPending,
            MlsRefusalReason::PendingSetFull => Self::PendingSetFull,
            MlsRefusalReason::NoSuchJoinRequest => Self::NoSuchJoinRequest,
            MlsRefusalReason::JoinerAlreadyAMember => Self::JoinerAlreadyAMember,
            MlsRefusalReason::SweepAlreadyStaged => Self::SweepAlreadyStaged,
            MlsRefusalReason::DeviceBindingInvalid => Self::DeviceBindingInvalid,
            MlsRefusalReason::DeviceLimitReached => Self::DeviceLimitReached,
        }
    }
}

impl From<FfiRefusalReason> for MlsRefusalReason {
    fn from(r: FfiRefusalReason) -> Self {
        match r {
            FfiRefusalReason::NotAMember => Self::NotAMember,
            FfiRefusalReason::AddNotOwnerOrAdmin => Self::AddNotOwnerOrAdmin,
            FfiRefusalReason::RemoveNotOwnerOrAdmin => Self::RemoveNotOwnerOrAdmin,
            FfiRefusalReason::OwnerUnremovable => Self::OwnerUnremovable,
            FfiRefusalReason::OwnerCannotLeave => Self::OwnerCannotLeave,
            FfiRefusalReason::NotOwner => Self::NotOwner,
            FfiRefusalReason::LeafUpdateRefused => Self::LeafUpdateRefused,
            FfiRefusalReason::UnsupportedProposal => Self::UnsupportedProposal,
            FfiRefusalReason::NoRoleTable => Self::NoRoleTable,
            FfiRefusalReason::OwnerNotAMember => Self::OwnerNotAMember,
            FfiRefusalReason::RoleTableWouldVanish => Self::RoleTableWouldVanish,
            FfiRefusalReason::NotACommitter => Self::NotACommitter,
            FfiRefusalReason::RoleChangePending => Self::RoleChangePending,
            FfiRefusalReason::ProposalsPending => Self::ProposalsPending,
            FfiRefusalReason::PendingSetFull => Self::PendingSetFull,
            FfiRefusalReason::NoSuchJoinRequest => Self::NoSuchJoinRequest,
            FfiRefusalReason::JoinerAlreadyAMember => Self::JoinerAlreadyAMember,
            FfiRefusalReason::SweepAlreadyStaged => Self::SweepAlreadyStaged,
            FfiRefusalReason::DeviceBindingInvalid => Self::DeviceBindingInvalid,
            FfiRefusalReason::DeviceLimitReached => Self::DeviceLimitReached,
        }
    }
}

/// What the caller should do about a refusal — FFI projection of
/// [`lakko_mls::RefusalDisposition`].
///
/// **Three-valued, and the middle value is the point.** The obvious shape is
/// two — *you may not* against *not yet* — and it is wrong, because "not yet"
/// silently merges a request the group is too busy to accept right now with one
/// that cannot be accepted as formed, ever. The first retries successfully; the
/// second loops forever.
///
/// The rule, stated positively: **a refusal is retryable exactly when a bounded
/// queue was full**, and never when it is about the request's own state.
///
/// It is answered by the core so three clients cannot answer it three ways.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiRefusalDisposition {
    /// *Not yet.* A bounded queue is full. The **identical** call succeeds once
    /// a committer has swept, with the caller doing nothing else in between.
    /// Offer a retry; say the group is busy, not that the user may not.
    RetryLater,
    /// *Not like this.* The call will never succeed as it stands, but the
    /// caller is not forbidden — the request's content, an outstanding
    /// obligation of the caller's, or the thing the request names has to change
    /// first. Do not offer a plain retry: say what must change, or refresh and
    /// let the user ask again.
    DoNotRetry,
    /// *You may not.* An authority answer. State it as a fact and offer no
    /// action at all.
    NotPermitted,
}

/// What the caller should do about `reason`.
///
/// Ask this rather than deciding it from the variant's name — the rule lives in
/// the core precisely so it cannot drift between platforms.
#[uniffi::export]
#[must_use]
pub fn membership_refusal_disposition(reason: FfiRefusalReason) -> FfiRefusalDisposition {
    match MlsRefusalReason::from(reason).disposition() {
        lakko_mls::RefusalDisposition::RetryLater => FfiRefusalDisposition::RetryLater,
        lakko_mls::RefusalDisposition::DoNotRetry => FfiRefusalDisposition::DoNotRetry,
        lakko_mls::RefusalDisposition::NotPermitted => FfiRefusalDisposition::NotPermitted,
    }
}

/// `true` when the **identical** call will succeed once a committer has swept,
/// with the caller doing nothing else — i.e. when the UI may offer a plain
/// retry.
///
/// Exactly [`FfiRefusalDisposition::RetryLater`]. Deliberately **not** the
/// complement of "permitted": a [`FfiRefusalDisposition::DoNotRetry`] refusal is
/// not a permission problem and is still not retryable.
#[uniffi::export]
#[must_use]
pub fn membership_refusal_retry_may_succeed(reason: FfiRefusalReason) -> bool {
    MlsRefusalReason::from(reason).retry_may_succeed()
}

/// The stable machine-readable code for `reason` — the same string the wasm
/// surface carries on `err.refusalReason`, so a refusal logged from Swift,
/// Kotlin and the web reads identically.
#[uniffi::export]
#[must_use]
pub fn membership_refusal_code(reason: FfiRefusalReason) -> String {
    MlsRefusalReason::from(reason).code().to_string()
}

/// Result of [`Group::add_member_from_wire`].
///
/// Distribute `commit` to all existing members for processing; deliver
/// `welcome` to the newly added member.
#[derive(uniffi::Record)]
pub struct AddMember {
    /// Encoded MLS commit message — broadcast to existing members.
    pub commit: Vec<u8>,
    /// Encoded MLS welcome message — sent to the joining member.
    pub welcome: Vec<u8>,
}

/// What an ADR-0045 proposal asks the group to do. FFI projection of
/// [`lakko_mls::ProposalKind`].
///
/// A proposal type Lakko never emits classifies as
/// [`ProposalKind::Unsupported`] and is always refused by the core's
/// authorisation rule (fail-closed, invariant #4). Each data-carrying variant
/// owns its payload, so a `Remove`'s target leaf can never be read as an
/// `Add`'s signature key.
#[derive(Debug, uniffi::Enum)]
pub enum ProposalKind {
    /// Add a prospective member, carrying their Ed25519 signature public key —
    /// the stable identity handle the role table and the contact graph key on.
    Add {
        /// The prospective member's 32-byte Ed25519 signature public key.
        signature_key: Vec<u8>,
    },
    /// Remove the member sitting at `target_leaf`.
    Remove {
        /// The leaf index the proposal removes.
        target_leaf: u32,
    },
    /// The sender leaves the group (RFC 9420 `SelfRemove`) — structurally
    /// targets the sender's own leaf and can name no other.
    Leave,
    /// The sender rotates its own leaf node. **Always refused**, so it only
    /// ever arrives with `stored == false`: openmls 0.8.1 does not bind an
    /// `Update`'s leaf node to the sender's existing signature key, so it can
    /// rotate the very key the ADR-0022 role table is keyed on, and it is the
    /// one kind whose proposal reference is fresh on every emission, which made
    /// it floodable past the pending-set cap. A Lakko member rotates its leaf
    /// with `selfUpdate`, which composes no proposal at all.
    Update,
    /// Rewrite the GroupContext extensions — in Lakko, the ADR-0022 role table.
    RoleChange,
    /// A proposal type Lakko never emits. Always refused.
    Unsupported,
}

/// One ADR-0045 proposal as the receive path saw it. FFI projection of
/// [`lakko_mls::ProposalRecord`].
///
/// `stored == false` is the wire-visible form of a **refusal**: a proposal does
/// not advance the epoch, so dropping it unstored *is* the refusal, and the
/// core reports it as data rather than as an error. A client renders or logs a
/// dropped proposal differently from a stored one — a stored proposal is a
/// pending request awaiting a sweep, a dropped one was refused by the
/// authorisation gate and will never be committed.
#[derive(Debug, uniffi::Record)]
pub struct ProposalRecord {
    /// The proposing member's leaf index, or `None` for a sender that is not a
    /// resolvable group member (always unauthorised — fail-closed).
    pub sender_leaf: Option<u32>,
    /// What the proposal asks for.
    pub kind: ProposalKind,
    /// `true` when the proposal was authorised and retained in the pending set
    /// so a later sweep can commit it; `false` when it was dropped.
    pub stored: bool,
}

/// Result of [`Group::commit_pending_proposals`] — the ADR-0045 sweep. FFI
/// projection of [`lakko_mls::ProposalCommit`].
///
/// Mirrors [`AddMember`]'s shape so ADR-0040's durable pending-commit outbox
/// wraps it unchanged, with two differences the proposal path forces: the
/// Welcome is **optional** (a sweep that carried no `Add` produces none), and
/// `swept` names what the commit actually carried so the caller can mark those
/// requests applied.
#[derive(uniffi::Record)]
pub struct ProposalCommit {
    /// Encoded MLS commit message — broadcast to all existing members.
    pub commit: Vec<u8>,
    /// Encoded MLS welcome message — delivered to the newly added members,
    /// present only when the sweep carried at least one `Add`.
    pub welcome: Option<Vec<u8>>,
    /// The proposals this commit carried, in sweep order. Read back off the
    /// staged commit, so it reports what the commit really carries rather than
    /// what the caller predicted it would.
    pub swept: Vec<ProposalRecord>,
}

impl From<lakko_mls::ProposalKind> for ProposalKind {
    fn from(k: lakko_mls::ProposalKind) -> Self {
        match k {
            lakko_mls::ProposalKind::Add { signature_key, .. } => Self::Add { signature_key },
            lakko_mls::ProposalKind::Remove { target_leaf } => Self::Remove { target_leaf },
            lakko_mls::ProposalKind::Leave => Self::Leave,
            lakko_mls::ProposalKind::Update => Self::Update,
            lakko_mls::ProposalKind::RoleChange => Self::RoleChange,
            lakko_mls::ProposalKind::Unsupported => Self::Unsupported,
        }
    }
}

impl From<lakko_mls::ProposalRecord> for ProposalRecord {
    fn from(r: lakko_mls::ProposalRecord) -> Self {
        Self {
            sender_leaf: r.sender_leaf,
            kind: r.kind.into(),
            stored: r.stored,
        }
    }
}

/// Result of [`Group::stage_pending_proposals`] — an ADR-0045 sweep that has
/// been **built but not merged**.
///
/// Carries byte-for-byte what [`ProposalCommit`] carries, and is a **separate
/// type on purpose**: the payloads are identical but the post-conditions are
/// opposite. Holding a [`ProposalCommit`] means the local epoch has **already**
/// advanced; holding a `StagedSweep` means it has **not**, and that this device
/// owes the core a verdict. Making that visible in the type means a staged
/// sweep cannot be handed to eager-path code — or the reverse — without a
/// deliberate, reviewer-visible conversion.
///
/// The obligation attached to this value:
///
/// 1. durably record `commit` (and `welcome`) — ADR-0040's outbox;
/// 2. publish them to the group journal;
/// 3. read the journal order back;
/// 4. **won** → [`Group::merge_staged_sweep`]; **lost** →
///    [`Group::abandon_staged_sweep`].
///
/// Steps 3 and 4 are not optional. A staged sweep is persisted by OpenMLS, so
/// leaving one unadjudicated freezes this group's own membership operations
/// **across restarts** — see [`Group::has_staged_sweep`], which is how a
/// freshly loaded group discovers it owes a verdict.
#[derive(Debug, uniffi::Record)]
pub struct StagedSweep {
    /// Encoded MLS commit message — publish to all existing members. The local
    /// epoch has **not** advanced; it advances only on
    /// [`Group::merge_staged_sweep`].
    pub commit: Vec<u8>,
    /// Encoded MLS welcome message — deliver to the newly added members,
    /// present only when the sweep carried at least one `Add`.
    pub welcome: Option<Vec<u8>>,
    /// The proposals this commit carried, in sweep order. Read back off the
    /// staged commit by the core, so it reports what the commit really carries
    /// rather than what the caller predicted it would.
    pub swept: Vec<ProposalRecord>,
}

/// What a verdict call did to the sweep staged by
/// [`Group::stage_pending_proposals`] (ADR-0045).
///
/// Named rather than a bare `Bool`, because the third state is the one that
/// matters: [`StagedSweepOutcome::NothingStaged`] is **not** a failure — it is
/// what a verdict reports on a group that never staged anything, and it is what
/// makes both verdict entry points idempotent, so ADR-0040's restart sweep can
/// run them twice without consequence.
///
/// Each entry point produces only two of the three variants:
///
/// | Call | Possible outcomes |
/// |---|---|
/// | [`Group::merge_staged_sweep`] | `Merged`, `NothingStaged` |
/// | [`Group::abandon_staged_sweep`] | `Abandoned`, `NothingStaged` |
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum StagedSweepOutcome {
    /// A staged sweep was merged: this device is now at the new epoch — the
    /// same one the peers who applied the published commit reached.
    Merged,
    /// A staged sweep was discarded: this device stayed at the epoch the sweep
    /// was built against, and is free to apply the winner's commit. The
    /// proposals the sweep selected are still pending for the winner to sweep.
    Abandoned,
    /// Nothing was staged, so nothing was decided and no state changed.
    NothingStaged,
}

/// What becomes of the outgoing owner when ownership is transferred (ADR-0022's
/// deferred follow-up, built in ADR-0045). FFI projection of
/// [`lakko_mls::OutgoingOwner`].
///
/// **This is a required argument, never a default.** The core deliberately
/// forces the caller to state it, because either silent choice is wrong in one
/// direction: silently keeping admin leaves an unwanted administrator behind in
/// a group the outgoing owner meant to hand over and walk away from, and
/// silently dropping to plain member strips them, in one irreversible commit,
/// of every power they had in a group they created. That property is carried
/// across the FFI verbatim — this enum deliberately does **not** derive
/// `Default`, and neither
/// [`Group::transfer_ownership`] nor [`Group::propose_ownership_transfer`]
/// declares a UniFFI default for the parameter, so the generated Swift and
/// Kotlin signatures make it mandatory at every call site.
///
/// A UI should still **pre-select** [`OutgoingOwner::Admin`] — not because it
/// preserves a way back (it does not: a role change is owner-only, so a
/// transfer cannot be undone by the person who made it) but because it is the
/// recoverable side of an unrecoverable action. The outgoing owner can shed
/// admin at any time by asking the new owner for one ordinary role change; the
/// reverse mistake needs the new owner's cooperation too *and* costs the group
/// an administrator in the meantime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum OutgoingOwner {
    /// Keep the outgoing owner in the admin set. The variant to pre-select.
    Admin,
    /// Demote the outgoing owner to a plain member.
    Member,
}

impl From<OutgoingOwner> for lakko_mls::OutgoingOwner {
    fn from(o: OutgoingOwner) -> Self {
        match o {
            OutgoingOwner::Admin => Self::Admin,
            OutgoingOwner::Member => Self::Member,
        }
    }
}

/// FFI projection of [`lakko_mls::WelcomeRendezvous`] (ADR-0027 Phase 3, G-3) —
/// the carrier that delivers an approved external joiner's Welcome through the
/// group journal.
#[derive(uniffi::Record)]
pub struct WelcomeRendezvous {
    /// The prospective joiner's signature key — the rendezvous tag the joiner
    /// matches against its own key.
    pub joiner_signature_key: Vec<u8>,
    /// The wire-encoded Welcome the joiner opens via `joinFromWelcome`.
    pub welcome: Vec<u8>,
}

/// Wrap an approved joiner's Welcome into a rendezvous envelope the sweeping
/// device posts to the group journal (ADR-0027 Phase 3). `joiner_signature_key`
/// is the added member's signature key; `welcome` is the `StagedSweep.welcome`
/// the ADR-0045 sweep produced (or, on a `PublisherGroup`, the
/// `AddMember.welcome` from its direct `approveExternalJoin`).
///
/// **A sweep cannot tell which of its adds came from a `/g/` link** — that is
/// what makes an approval indistinguishable from an ordinary add, and it is
/// deliberate — so the caller wraps one envelope per added key, origin-blind.
///
/// # Errors
///
/// [`FfiError`] if a field is empty or exceeds the envelope bounds.
#[uniffi::export]
fn wrap_welcome_rendezvous(
    joiner_signature_key: Vec<u8>,
    welcome: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    Ok(lakko_mls::WelcomeRendezvous {
        joiner_signature_key,
        welcome,
    }
    .encode()?)
}

/// Parse a rendezvous envelope from a group-journal entry (ADR-0027 Phase 3) —
/// the joiner decodes each entry and matches `joiner_signature_key` against its
/// own key, then opens the inner Welcome. Bounded + fail-closed.
///
/// # Errors
///
/// [`FfiError`] for a malformed / out-of-bounds envelope.
#[uniffi::export]
fn parse_welcome_rendezvous(bytes: Vec<u8>) -> Result<WelcomeRendezvous, FfiError> {
    let r = lakko_mls::WelcomeRendezvous::decode(&bytes)?;
    Ok(WelcomeRendezvous {
        joiner_signature_key: r.joiner_signature_key,
        welcome: r.welcome,
    })
}

/// FFI projection of [`lakko_mls::group::MemberRecord`]
/// (Phase 2a3c).
///
/// One row of the `devices()` snapshot — a single leaf in the
/// DeviceGroup tree. UI surfaces this as a list row in the
/// "Your devices" management screen.
#[derive(uniffi::Record)]
pub struct DeviceLeaf {
    /// OpenMLS leaf position. `u32` mirrors `LeafNodeIndex`'s
    /// internal repr. Stable inside an epoch; can renumber on
    /// the next add/remove cycle.
    pub leaf_index: u32,
    /// 32-byte Ed25519 verifying key for the device's
    /// signature credential. UI renders a hex prefix as a
    /// fingerprint so the user can tell devices apart.
    pub signature_public_key: Vec<u8>,
}

/// Result of [`Group::process_incoming`].
#[derive(uniffi::Enum)]
pub enum ProcessedMessage {
    /// Application payload arrived (decrypted).
    Application {
        /// Plaintext bytes the application can consume.
        data: Vec<u8>,
        /// The sender's leaf index (ADR-0023), or `None` for an external
        /// sender. Still surfaced because SFrame and the DeviceGroup key on
        /// the leaf; authorship does not (see `sender_account`).
        sender_leaf: Option<u32>,
        /// The **account** the sender leaf resolves to (ADR-0050), or `None`
        /// for an external sender or an unresolvable leaf. This is what the
        /// author-only edit/delete gate, reaction attribution and read state
        /// key on, so a person is one author across all their devices.
        sender_account: Option<Vec<u8>>,
    },
    /// A commit was processed and merged. Group state advanced.
    Commit,
    /// A standalone ADR-0045 proposal was processed. **No state advance** — a
    /// proposal is not epoch-advancing, which is the whole point: two members
    /// proposing concurrently compose instead of forking the loser.
    ///
    /// `record` names the proposer's leaf, what was proposed, and whether the
    /// ADR-0045 receipt gate retained it (`ProposalRecord::stored`). A dropped
    /// record is a **refusal** the caller should surface or log distinctly from
    /// a stored one — see [`ProposalRecord::stored`].
    Proposal {
        /// The proposal as the receive path saw it.
        record: ProposalRecord,
    },
    /// A non-member's external-join proposal was processed (ADR-0027, G-3).
    /// `joiner_signature_key` is the handle for
    /// [`Group::approve_external_join`] / [`Group::reject_external_join`], and
    /// the key the contact graph resolves to a display name.
    ExternalJoinProposal {
        /// The prospective joiner's leaf signature public key.
        joiner_signature_key: Vec<u8>,
        /// `true` when the request was retained and an approve can act on it.
        ///
        /// **`false` means an approve would fail, so do not offer one.** The
        /// event fires either way — a request arriving while the stranger
        /// quarantine is full is surfaced but not kept, because a stranger must
        /// not be able to make a member's receive loop error. Rendering an
        /// unretained request as actionable is exactly the shape that offers a
        /// user an action that cannot work.
        ///
        /// The remedy belongs to the joiner (ask again later), not the admin.
        retained: bool,
    },
}

/// Typed body of a DeviceGroup cross-device payload
/// (Phase 2a3e2). Mirror of [`lakko_mls::device_group::DeviceGroupBody`]
/// on the FFI boundary. Four variants matching the
/// `DeviceGroupPayload.body` `oneof` in
/// `schemas/mls-app/devicegroup_v1.proto`.
#[derive(uniffi::Enum)]
pub enum DeviceGroupBody {
    /// "I read up to this point in this thread." Other devices
    /// last-write-wins by HLC to compute their own unread count.
    ReadCursor {
        /// The conversation group's MLS group_id.
        group_id: Vec<u8>,
        /// The message id the sender claims to have read up to.
        up_to_message_id: Vec<u8>,
        /// Wall-clock UNIX millis when the read happened.
        read_at_ms: u64,
    },
    /// In-flight composer text for a single thread. Empty `text`
    /// clears the draft (receivers wipe their local copy).
    Draft {
        /// The conversation group_id the draft belongs to.
        group_id: Vec<u8>,
        /// Composer text. UTF-8, may be empty.
        text: String,
        /// Non-empty when the draft was started as a reply.
        reply_to_id: Vec<u8>,
    },
    /// Per-thread UI flags (last-write-wins snapshot of the
    /// whole struct).
    Flags {
        /// The conversation group_id the flags apply to.
        group_id: Vec<u8>,
        /// Pinned threads stay at the top of the list.
        pinned: bool,
        /// Muted threads suppress push notifications + unread
        /// indicators.
        muted: bool,
        /// Archived threads drop out of the main list.
        archived: bool,
        /// Manual unread override after the thread was read.
        unread_override: bool,
    },
    /// Explicit display ordering for the conversation list.
    Order {
        /// Conversation group_ids in display order, top-to-bottom.
        group_ids: Vec<Vec<u8>>,
    },
}

/// Parsed envelope returned by
/// [`parse_devicegroup_payload`] (Phase 2a3e2). Wraps the
/// typed [`DeviceGroupBody`] with the metadata callers need for
/// ordering (HLC) and originator identification (sender_leaf).
#[derive(uniffi::Record)]
pub struct ParsedDeviceGroupPayload {
    /// Hybrid logical clock timestamp from the envelope.
    pub hlc: u64,
    /// Leaf index of the device that authored this payload.
    pub sender_leaf: u32,
    /// Concrete payload — one of `ReadCursor` / `Draft` /
    /// `Flags` / `Order`.
    pub body: DeviceGroupBody,
}

impl From<DeviceGroupBody> for lakko_mls::device_group::DeviceGroupBody {
    fn from(value: DeviceGroupBody) -> Self {
        use lakko_mls::device_group::DeviceGroupBody as Inner;
        match value {
            DeviceGroupBody::ReadCursor {
                group_id,
                up_to_message_id,
                read_at_ms,
            } => Inner::ReadCursor {
                group_id,
                up_to_message_id,
                read_at_ms,
            },
            DeviceGroupBody::Draft {
                group_id,
                text,
                reply_to_id,
            } => Inner::Draft {
                group_id,
                text,
                reply_to_id,
            },
            DeviceGroupBody::Flags {
                group_id,
                pinned,
                muted,
                archived,
                unread_override,
            } => Inner::Flags {
                group_id,
                pinned,
                muted,
                archived,
                unread_override,
            },
            DeviceGroupBody::Order { group_ids } => Inner::Order { group_ids },
        }
    }
}

impl From<lakko_mls::device_group::DeviceGroupBody> for DeviceGroupBody {
    fn from(value: lakko_mls::device_group::DeviceGroupBody) -> Self {
        use lakko_mls::device_group::DeviceGroupBody as Inner;
        match value {
            Inner::ReadCursor {
                group_id,
                up_to_message_id,
                read_at_ms,
            } => Self::ReadCursor {
                group_id,
                up_to_message_id,
                read_at_ms,
            },
            Inner::Draft {
                group_id,
                text,
                reply_to_id,
            } => Self::Draft {
                group_id,
                text,
                reply_to_id,
            },
            Inner::Flags {
                group_id,
                pinned,
                muted,
                archived,
                unread_override,
            } => Self::Flags {
                group_id,
                pinned,
                muted,
                archived,
                unread_override,
            },
            Inner::Order { group_ids } => Self::Order { group_ids },
        }
    }
}

/// Lakko identity — owns the device's signing keypair, MLS credential and
/// per-device storage provider.
#[derive(uniffi::Object)]
pub struct Identity {
    inner: MlsIdentity,
}

#[uniffi::export]
impl Identity {
    /// Generate a fresh identity with the given user-display label.
    #[uniffi::constructor]
    pub fn new(name: String) -> Result<Self, FfiError> {
        let inner = MlsIdentity::generate(&name)?;
        Ok(Self { inner })
    }

    /// Derive a deterministic identity from a 32-byte seed.
    ///
    /// The OPAQUE export key (1.15d) is the canonical seed source
    /// in production: same password ⇒ same Ed25519 + X25519
    /// keypairs ⇒ same device identity across re-installs.
    ///
    /// Mirror of `apps/web`'s `Identity.fromSeed`. Caller MUST
    /// pass exactly 32 bytes; longer / shorter input is rejected
    /// at the underlying `MlsIdentity::from_seed` call.
    #[uniffi::constructor]
    pub fn from_seed(seed: Vec<u8>) -> Result<Self, FfiError> {
        let inner = MlsIdentity::from_seed(&seed)?;
        Ok(Self { inner })
    }

    /// A **device** identity bound to an account (ADR-0050): derive it from
    /// `seed` and mint its device binding under `account_seed` (the account
    /// identity's 32-byte Ed25519 seed, `Identity::ed25519_seed` of the
    /// phrase-derived identity). Every KeyPackage and every group this identity
    /// creates carries the binding in its leaf credential.
    ///
    /// # Errors
    ///
    /// [`FfiError::Identity`] when either seed is malformed.
    #[uniffi::constructor]
    pub fn from_seed_bound(
        seed: Vec<u8>,
        account_seed: Vec<u8>,
        issued_at_unix_secs: u64,
    ) -> Result<Self, FfiError> {
        let account: [u8; 32] = account_seed
            .as_slice()
            .try_into()
            .map_err(|_| FfiError::Identity("account seed must be 32 bytes".into()))?;
        let inner = MlsIdentity::from_seed_bound(&seed, &account, issued_at_unix_secs)?;
        Ok(Self { inner })
    }

    /// Rebuild a bound device identity from its persisted seed and its
    /// persisted binding bytes (ADR-0050), so `issued_at` stays what it was.
    ///
    /// # Errors
    ///
    /// [`FfiError::Identity`] when `binding` is not a valid binding for this
    /// identity's key.
    #[uniffi::constructor]
    pub fn from_seed_with_binding(seed: Vec<u8>, binding: Vec<u8>) -> Result<Self, FfiError> {
        let inner = MlsIdentity::from_seed_with_binding(&seed, &binding)?;
        Ok(Self { inner })
    }

    /// Public signature key bytes (Ed25519, 32 bytes).
    pub fn signature_public_key(&self) -> Vec<u8> {
        self.inner.signature_public_key().to_vec()
    }

    /// Long-term X25519 public key bytes (32 bytes). Recipients
    /// publish this in their identity bundle; senders use it to
    /// seal the [`crate::contacts::ContactRequest`] intro
    /// envelope.
    pub fn x25519_public_key(&self) -> Vec<u8> {
        self.inner.x25519_public_key().to_vec()
    }

    /// Long-term X25519 secret key bytes (32 bytes).
    ///
    /// Sensitive — the caller is expected to use it
    /// transactionally (open one envelope, wipe) and never to
    /// persist outside the Keystore-wrapped device-root layer
    /// 1.15d3 set up.
    pub fn x25519_secret_key(&self) -> Vec<u8> {
        self.inner.x25519_secret_bytes().to_vec()
    }

    /// Ed25519 signing-seed bytes (32 bytes).
    ///
    /// `lakko_contacts::compose` consumes this to sign the
    /// `ContactRequest` envelope; the wasm bridge exposes the
    /// same accessor for the equivalent flow in `apps/web`.
    ///
    /// Sensitive — same lifecycle constraints as
    /// [`x25519_secret_key`].
    pub fn ed25519_seed(&self) -> Vec<u8> {
        self.inner.ed25519_seed_bytes().to_vec()
    }

    /// The wire bytes of this identity's ADR-0050 device binding, or `None`
    /// for an unbound identity. Persist them beside the device seed.
    pub fn device_binding(&self) -> Option<Vec<u8>> {
        self.inner.device_binding().map(<[u8]>::to_vec)
    }

    /// The account key this identity acts for (ADR-0050): the binding's
    /// account key when bound, else its own signature key.
    pub fn account_public_key(&self) -> Option<Vec<u8>> {
        self.inner.account_public_key().map(|k| k.to_vec())
    }

    /// Create a fresh KeyPackage in TLS wire form.
    pub fn create_key_package(&self) -> Result<Vec<u8>, FfiError> {
        let kp = self.inner.create_key_package()?;
        Ok(key_package::serialize(&kp)?)
    }

    /// Create an external-join proposal — the joiner side of ADR-0027 (G-3).
    ///
    /// A non-member crafts an external Add proposal for a group's current
    /// `group_id` + `epoch` (learned out-of-band via a join ticket). The
    /// returned wire bytes are posted to the group journal; an owner/admin
    /// approves via `Group.approveExternalJoin`, and whichever device then
    /// sweeps produces the Welcome the joiner opens via
    /// `Group.joinFromWelcome`. The proposal embeds a fresh KeyPackage
    /// whose private keys persist in this identity's provider, so the SAME
    /// identity must later open the Welcome.
    pub fn create_external_join_proposal(
        &self,
        group_id: Vec<u8>,
        epoch: u64,
    ) -> Result<Vec<u8>, FfiError> {
        Ok(self.inner.create_external_join_proposal(&group_id, epoch)?)
    }

    /// Serialise the entire OpenMLS provider storage to a
    /// length-prefixed byte blob.
    ///
    /// The blob carries every secret OpenMLS holds for this
    /// identity: per-group ratchet state, init-key private
    /// bytes, leaf-secret material. Treat it as **the most
    /// sensitive value any Lakko process touches** — wrap it
    /// under the Keystore-resident device root key before
    /// writing to disk. The Kotlin / Swift caller is responsible
    /// for the AEAD layer; this function only produces the
    /// plaintext bytes.
    ///
    /// Mirror of the wasm `Identity.snapshotStorage` flow that
    /// `apps/web` 1.16e6c1 uses to persist MLS state to
    /// IndexedDB. Same wire format on both sides — a snapshot
    /// produced on web can be restored on Android with the same
    /// OPAQUE export key.
    pub fn snapshot_storage(&self) -> Result<Vec<u8>, FfiError> {
        Ok(self.inner.snapshot_storage()?)
    }

    /// Restore the OpenMLS provider storage from a
    /// [`snapshot_storage`] blob.
    ///
    /// Overwrites whatever state the local provider had — pair
    /// this with a freshly-constructed [`Identity::from_seed`]
    /// so the public keys match the snapshot's secrets.
    ///
    /// # Errors
    ///
    /// * [`FfiError::Identity`] — snapshot is malformed
    ///   (truncated counter, length-prefix overflow, …).
    pub fn restore_storage(&self, snapshot: Vec<u8>) -> Result<(), FfiError> {
        self.inner.restore_storage(&snapshot)?;
        Ok(())
    }
}

/// One member of a chat [`Group`] at the current epoch — the roster row the
/// group-chat UI renders (member list + per-member Remove affordance). Mirror
/// of the wasm `Group.members()` shape + the [`DeviceLeaf`] device-roster row.
#[derive(uniffi::Record)]
pub struct GroupMember {
    /// OpenMLS leaf position. Stable inside an epoch; can renumber on the next
    /// add/remove cycle, so the UI MUST NOT cache it across a state change.
    pub leaf_index: u32,
    /// 32-byte Ed25519 verifying key of the member's **leaf** (its device).
    pub signature_public_key: Vec<u8>,
    /// The **account** key the leaf belongs to (ADR-0050) — what the UI
    /// matches against the contacts list to render a name, so one person with
    /// three devices is one person. Empty when the leaf's credential did not
    /// resolve (never admitted by a Lakko gate).
    pub account_key: Vec<u8>,
}

/// A member's role in an ADR-0022 role-enabled group. The UI gates the
/// add / remove / promote affordances on the local member's role.
#[derive(uniffi::Enum)]
pub enum Role {
    /// The group creator — exactly one, unremovable, the only role that may
    /// change the role table.
    Owner,
    /// Appointed by the owner — may add / remove non-owner members.
    Admin,
    /// May not add or remove anyone.
    Member,
}

/// The ADR-0022 admin role table: the owner's signature key + the admin set,
/// each a raw 32-byte Ed25519 signature public key (the same key the roster
/// matches against). Mirror of `lakko_mls::RoleTable`.
#[derive(uniffi::Record)]
pub struct RoleTable {
    /// Owner signature public key (32 bytes).
    pub owner: Vec<u8>,
    /// Admin signature public keys (each 32 bytes), sorted + deduplicated.
    pub admins: Vec<Vec<u8>>,
}

/// Build a [`MlsRoleTable`] from the raw key material the FFI carries,
/// refusing any key that is not exactly 32 bytes.
///
/// Shared by [`Group::set_group_roles`] and [`Group::propose_group_roles`] so
/// the two paths cannot drift on what they accept.
fn role_table_from_parts(owner: Vec<u8>, admins: Vec<Vec<u8>>) -> Result<MlsRoleTable, FfiError> {
    let owner: [u8; 32] = owner
        .try_into()
        .map_err(|_| FfiError::Codec("owner key must be 32 bytes".into()))?;
    let mut admin_keys = Vec::with_capacity(admins.len());
    for a in admins {
        let k: [u8; 32] = a
            .try_into()
            .map_err(|_| FfiError::Codec("admin key must be 32 bytes".into()))?;
        admin_keys.push(k);
    }
    Ok(MlsRoleTable::new(owner, admin_keys))
}

/// Lakko MLS conversation group.
#[derive(uniffi::Object)]
pub struct Group {
    inner: Mutex<MlsGroup>,
}

#[uniffi::export]
impl Group {
    /// Create a fresh group with `identity` as the founding member.
    #[uniffi::constructor]
    pub fn create(identity: &Identity) -> Result<Self, FfiError> {
        let inner = MlsGroup::create(&identity.inner)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// Join an existing group from a wire-format Welcome message.
    #[uniffi::constructor]
    pub fn join_from_welcome(welcome: Vec<u8>, identity: &Identity) -> Result<Self, FfiError> {
        let inner = MlsGroup::join_from_welcome(&welcome, &identity.inner)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// Add a member from their published KeyPackage (TLS wire form).
    pub fn add_member_from_wire(
        &self,
        identity: &Identity,
        key_package: Vec<u8>,
    ) -> Result<AddMember, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let MemberAddition { commit, welcome } =
            g.add_member_from_wire(&identity.inner, &key_package)?;
        Ok(AddMember {
            commit: group::encode_message(&commit)?,
            welcome: group::encode_message(&welcome)?,
        })
    }

    /// Add several members in ONE commit with ONE Welcome (ADR-0050): a
    /// person's devices, or a founder's own other devices, must share a
    /// commit, else every leaf but the last is stranded at a Welcome epoch the
    /// founder advanced past with nobody to broadcast the commit to. The
    /// returned Welcome is delivered byte-identical to each added leaf's
    /// mailbox.
    pub fn add_members_from_wire(
        &self,
        identity: &Identity,
        key_packages: Vec<Vec<u8>>,
    ) -> Result<AddMember, FfiError> {
        let borrowed: Vec<&[u8]> = key_packages.iter().map(Vec::as_slice).collect();
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let MemberAddition { commit, welcome } =
            g.add_members_from_wire(&identity.inner, &borrowed)?;
        Ok(AddMember {
            commit: group::encode_message(&commit)?,
            welcome: group::encode_message(&welcome)?,
        })
    }

    /// Remove a member by their leaf index.
    ///
    /// Returns the wire-encoded commit to broadcast.
    pub fn remove_member(&self, identity: &Identity, leaf_index: u32) -> Result<Vec<u8>, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let commit = g.remove_member(&identity.inner, lakko_mls::LeafNodeIndex::new(leaf_index))?;
        Ok(group::encode_message(&commit)?)
    }

    /// Approve a retained external-join request — **proposes** adding the
    /// joiner (ADR-0027 G-3, on the ADR-0045 proposal path since S7).
    /// `proposal_ref` is the joiner's signature key from a
    /// `ProcessedMessage::ExternalJoinProposal` or
    /// [`Self::pending_external_joins`].
    ///
    /// **Returns the wire-encoded Proposal to publish — not a Commit, and no
    /// Welcome.** The epoch does not advance; the Welcome is produced by
    /// whichever device sweeps this into a commit
    /// ([`Self::commit_pending_proposals`] / [`Self::stage_pending_proposals`],
    /// whose `welcome` the sweeping device posts as a `WelcomeRendezvous` for
    /// each added key). That is the whole of S7: two admins approving two
    /// joiners against one epoch used to eagerly merge two commits and fork the
    /// group; now they compose into one sweep.
    ///
    /// The approval goes on the wire as an ordinary admin-authored `Add` — the
    /// admin's own MLS-authenticated signature *is* the approval — so it is
    /// indistinguishable from adding a contact and needs no new authorisation
    /// channel. The joiner's own external proposal is still never stored and
    /// never committed: a stranger with a `/g/` link may only ask.
    ///
    /// Role-gated to owner/admin, locally and again by every peer. A refused
    /// approve emits nothing and leaves the request pending, so it can be
    /// retried. A group with no role table has no committer and is refused —
    /// see [`PublisherGroup::approve_external_join`], whose return type differs
    /// because its outcome does.
    pub fn approve_external_join(
        &self,
        identity: &Identity,
        proposal_ref: Vec<u8>,
    ) -> Result<Vec<u8>, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.approve_external_join(&identity.inner, &proposal_ref)?)
    }

    /// Reject a retained external-join proposal — discards it locally without
    /// adding the joiner or advancing the epoch (ADR-0027).
    /// `identity` is what the durable copy of the pending set is written
    /// through (the request set rides the MLS storage snapshot, so a decision
    /// survives a restart).
    pub fn reject_external_join(
        &self,
        identity: &Identity,
        proposal_ref: Vec<u8>,
    ) -> Result<(), FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        g.reject_external_join(&identity.inner, &proposal_ref)?;
        Ok(())
    }

    /// The signature key of every external-join proposal currently awaiting a
    /// decision (ADR-0027) — each is a handle for `approveExternalJoin` /
    /// `rejectExternalJoin`.
    pub fn pending_external_joins(&self) -> Result<Vec<Vec<u8>>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.pending_external_joins())
    }

    /// The account key behind a pending external-join request (32 bytes),
    /// resolved from the joiner's verified ADR-0050 device binding, or `None`.
    /// Use it to name a request from a contact; approving still goes by the
    /// leaf key `pendingExternalJoins` returns.
    pub fn pending_external_join_account_key(
        &self,
        joiner_signature_key: Vec<u8>,
    ) -> Result<Option<Vec<u8>>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.pending_external_join_account_key(&joiner_signature_key)
            .map(|k| k.to_vec()))
    }

    /// **Propose** adding a member from their published KeyPackage (TLS wire
    /// form) — the ADR-0045 proposal-path twin of
    /// [`Self::add_member_from_wire`].
    ///
    /// Returns the wire-encoded Proposal to publish to the group journal. The
    /// epoch does **not** advance and there is no Welcome yet: the Welcome is
    /// produced by whichever device sweeps this into a commit
    /// ([`Self::commit_pending_proposals`]). Because a proposal is not
    /// epoch-advancing, two members proposing concurrently compose instead of
    /// forking the loser off the group.
    ///
    /// Role-gated locally to owner/admin, and re-gated by every peer's receipt
    /// gate — a refused proposal is simply never stored.
    pub fn propose_add_member_from_wire(
        &self,
        identity: &Identity,
        key_package: Vec<u8>,
    ) -> Result<Vec<u8>, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.propose_add_member_from_wire(&identity.inner, &key_package)?)
    }

    /// **Propose** removing the member at `leaf_index` — the ADR-0045
    /// proposal-path twin of [`Self::remove_member`].
    ///
    /// Naming **this device's own** leaf is how a member leaves and needs no
    /// role; any other leaf needs owner or admin, and the owner's leaf is
    /// refused for everyone. Returns the wire-encoded Proposal to publish; the
    /// epoch does not advance.
    pub fn propose_remove_member(
        &self,
        identity: &Identity,
        leaf_index: u32,
    ) -> Result<Vec<u8>, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.propose_remove_member(&identity.inner, lakko_mls::LeafNodeIndex::new(leaf_index))?)
    }

    /// **Propose** rewriting the group's ADR-0022 role table — the ADR-0045
    /// proposal-path twin of [`Self::set_group_roles`]. Owner-only.
    ///
    /// `owner` and every `admins` entry must be a 32-byte Ed25519 signature
    /// public key. Returns the wire-encoded Proposal to publish; the epoch does
    /// not advance.
    ///
    /// **One role change at a time.** This FAILS while another role change —
    /// including an ownership transfer — is already pending, because RFC 9420
    /// admits at most one `GroupContextExtensions` proposal per commit, so a
    /// second could not be swept. A pending add or remove does **not** block
    /// it; only another role change does. To pre-empt the failure rather than
    /// surface it, call [`Self::pending_proposals`] and look for a
    /// `ProposalKind::RoleChange` — that is the same question the core asks.
    pub fn propose_group_roles(
        &self,
        identity: &Identity,
        owner: Vec<u8>,
        admins: Vec<Vec<u8>>,
    ) -> Result<Vec<u8>, FfiError> {
        let table = role_table_from_parts(owner, admins)?;
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.propose_group_roles(&identity.inner, &table)?)
    }

    /// Every ADR-0045 proposal currently in this device's pending set, in the
    /// order OpenMLS holds them.
    ///
    /// This is the committer's "N changes waiting to apply" surface and the
    /// proposer's "did my request land" surface. Every entry reports
    /// `stored == true` by construction — the receipt gate admits nothing else,
    /// and OpenMLS empties the whole store on every merged commit, so the set
    /// is exactly what a sweep would consider.
    pub fn pending_proposals(&self) -> Result<Vec<ProposalRecord>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.pending_proposals().into_iter().map(Into::into).collect())
    }

    /// This device's ADR-0045 committer rank: `Some(0)` for the owner,
    /// `Some(k + 1)` for the `k`-th admin in the role table's canonical order,
    /// and **`None`** for a plain member or a group with no role table.
    ///
    /// `None` and `Some(0)` are **not** the same thing and must never be
    /// conflated: `Some(0)` is the owner, who acts as committer immediately,
    /// while `None` means this device is not in the committer set at all and
    /// must never sweep. Test for absence explicitly (Swift `if let` /
    /// Kotlin `?.let`) — never coerce the optional to a default.
    pub fn own_committer_rank(&self, identity: &Identity) -> Result<Option<u32>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.own_committer_rank(&identity.inner))
    }

    /// **Another** member's ADR-0045 committer rank, by its 32-byte Ed25519
    /// signature public key: `Some(0)` for the owner, `Some(k + 1)` for the
    /// `k`-th admin in the role table's canonical order, and `None` for a plain
    /// member, a non-member, a wrong-length key, or a group with no role table.
    ///
    /// The peer-facing twin of [`Self::own_committer_rank`], and the input
    /// `BeaconObservation.rank` needs: a beacon carries no identity, so the
    /// receiver resolves its MLS sender leaf to a signature key through
    /// [`Self::members`] and ranks that key here.
    ///
    /// # Rank the key when you take the verdict, not when the beacon arrived
    ///
    /// A rank is a **position** in the role table's canonical order, not a
    /// stable identity: promoting, demoting or removing any admin renumbers
    /// every admin sorting after it. `acting_committer_verdict` compares each
    /// observation's rank against this device's own, which is necessarily
    /// derived from the current table — so a rank captured at receipt and
    /// compared later is not merely stale, it is a position in a *different*
    /// ordering. Persist the sender's signature key with each observed beacon
    /// and call this at fold time. Never persist a leaf index for this purpose:
    /// OpenMLS reuses a vacated leaf for the next member added, so a stored
    /// leaf can silently come to name someone else.
    ///
    /// This orders the authorised committers; it does not widen them. The set
    /// that may commit is exactly ADR-0022's owner-plus-admins, unchanged.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`] if the group mutex was poisoned.
    pub fn committer_rank(&self, signature_key: Vec<u8>) -> Result<Option<u32>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.committer_rank_of(&signature_key))
    }

    /// Sweep the authorised pending proposals into **one** Commit and merge it
    /// immediately (ADR-0045) — the **eager** committer path.
    ///
    /// **A committer that can lose a race must not use this.** It is exactly
    /// [`Self::stage_pending_proposals`] followed at once by
    /// [`Self::merge_staged_sweep`], and merging before the DS journal has
    /// ordered the commit is precisely what strands the loser at an epoch
    /// nobody else reached — silently and permanently (G-38 residual 1). Two
    /// devices can both act as committer during any handover, so unless the
    /// caller can prove it is the only one committing, the staged pair is the
    /// correct entry point. This one is kept, unchanged, for the callers that
    /// have not migrated.
    ///
    /// Returns `Ok(None)` when nothing authorised is pending: deliberately not
    /// an empty commit, which would advance the epoch (and race other
    /// committers) for no effect. Otherwise the local state is already at the
    /// new epoch when this returns, so the caller MUST durably record the
    /// returned artifacts before publishing them (ADR-0040).
    ///
    /// Role-gated to owner/admin, and the sweep re-runs the ADR-0045
    /// authorisation rule over every candidate, so this can never commit an
    /// unauthorised proposal however the pending store was populated.
    pub fn commit_pending_proposals(
        &self,
        identity: &Identity,
    ) -> Result<Option<ProposalCommit>, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let Some(swept_commit) = g.commit_pending_proposals(&identity.inner)? else {
            return Ok(None);
        };
        let welcome = match swept_commit.welcome {
            Some(w) => Some(group::encode_message(&w)?),
            None => None,
        };
        Ok(Some(ProposalCommit {
            commit: group::encode_message(&swept_commit.commit)?,
            welcome,
            swept: swept_commit.swept.into_iter().map(Into::into).collect(),
        }))
    }

    /// Sweep the authorised pending proposals into **one** Commit and **stop
    /// before merging it** — the ADR-0045 rollback point (G-38 residual 1).
    ///
    /// This is the committer entry point for any device that can lose a race,
    /// which in practice is every device: ADR-0045's designated committer makes
    /// two simultaneous sweeps rare, not impossible, because no liveness signal
    /// can distinguish a dead committer from a slow one. The DS journal already
    /// arbitrates — every member observes its order identically — so the
    /// collision needs no negotiation. What it needs is for the loser to be
    /// able to *survive losing*, which the eager shape denies it.
    ///
    /// **The sequence, in order. Every step is required.**
    ///
    /// 1. `stagePendingProposals` — the group stays at epoch N holding an
    ///    un-merged commit.
    /// 2. Durably record [`StagedSweep::commit`] (and
    ///    [`StagedSweep::welcome`]) — ADR-0040's outbox — then publish them.
    /// 3. Read the journal order back: did this commit land first?
    /// 4. **Won** → [`Self::merge_staged_sweep`]. **Lost** →
    ///    [`Self::abandon_staged_sweep`], then apply the winner's commit
    ///    normally.
    ///
    /// The two mechanisms answer different questions and neither subsumes the
    /// other: the outbox answers *did my commit reach the wire*, this pair
    /// answers *did it win the wire order*. The first must be `yes` before the
    /// second is meaningful. A commit that **won** must be merged and never
    /// rolled back — rolling one back after peers applied it manufactures the
    /// very fork this closes.
    ///
    /// **Abandoning costs nothing.** OpenMLS empties the proposal store on
    /// *merge*, and an abandoned sweep was never merged, so the proposals it
    /// selected are still pending for the winner's next sweep.
    ///
    /// Returns `Ok(None)` when nothing authorised is pending — the same
    /// deliberate non-empty-commit rule as [`Self::commit_pending_proposals`],
    /// and in that case nothing is staged and no verdict is owed.
    ///
    /// # Errors
    ///
    /// [`FfiError::SweepAlreadyStaged`] when a sweep is already awaiting its
    /// verdict — handle it by reaching that verdict, never by retrying the
    /// stage. Otherwise [`FfiError::Group`] when the group has no role table,
    /// when this device's role does not authorise it to commit, or on any
    /// OpenMLS-side failure.
    pub fn stage_pending_proposals(
        &self,
        identity: &Identity,
    ) -> Result<Option<StagedSweep>, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        // Projected here, under the SAME lock guard the stage runs under, so
        // there is no window between the check and the call. The core carries
        // its own fail-closed guard and stays authoritative; this exists only
        // to give the caller a variant it can branch on instead of a message it
        // would have to string-match, and never to relax the refusal.
        if g.has_staged_sweep() {
            return Err(FfiError::SweepAlreadyStaged(
                "sweep refused: a staged sweep is already awaiting a verdict".into(),
            ));
        }
        let Some(staged) = g.stage_pending_proposals(&identity.inner)? else {
            return Ok(None);
        };
        let welcome = match staged.welcome {
            Some(w) => Some(group::encode_message(&w)?),
            None => None,
        };
        Ok(Some(StagedSweep {
            commit: group::encode_message(&staged.commit)?,
            welcome,
            swept: staged.swept.into_iter().map(Into::into).collect(),
        }))
    }

    /// `true` when a sweep built by [`Self::stage_pending_proposals`] is staged
    /// and still awaiting its journal verdict (ADR-0045).
    ///
    /// **This is the restart-time obligation check, and it is why the verdict
    /// lives on the group rather than on the staged value.** A staged commit is
    /// persisted by OpenMLS and read back by `Group.load`, so it outlives the
    /// process — a handle object returned by the stage call could not. After
    /// loading a group, a client that finds this `true` still owes a verdict on
    /// a commit it may have already published, and must resolve it (ADR-0040's
    /// restart sweep re-publishes, re-reads the journal order, then merges or
    /// abandons).
    ///
    /// Leaving one unresolved is not inert: while a sweep is staged, OpenMLS
    /// refuses this group's own `propose*` / add / remove / role-change
    /// operations, so the group's membership is frozen until the verdict lands.
    /// Incoming messages are unaffected.
    ///
    /// Every eager path leaves this `false` — [`Self::add_member_from_wire`],
    /// [`Self::remove_member`], [`Self::set_group_roles`] and
    /// [`Self::commit_pending_proposals`] all merge before they return — which
    /// is why a rollback here can never reach a path that never had one.
    /// [`Self::approve_external_join`] left that list with ADR-0045 S7: it
    /// merges nothing, because it commits nothing.
    pub fn has_staged_sweep(&self) -> Result<bool, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.has_staged_sweep())
    }

    /// **Won.** Merge the sweep staged by [`Self::stage_pending_proposals`],
    /// advancing this device to the new epoch — the same one the peers who
    /// applied the published commit reached (ADR-0045).
    ///
    /// Call this only after the journal has confirmed this device's commit was
    /// ordered first. Merging on any weaker signal is the eager shape again,
    /// with all of its consequences.
    ///
    /// Returns [`StagedSweepOutcome::Merged`], or
    /// [`StagedSweepOutcome::NothingStaged`] when there was nothing staged —
    /// not a failure, and what makes a re-run after the merge a no-op.
    ///
    /// # Errors
    ///
    /// [`FfiError::Group`] on any OpenMLS-side merge failure.
    pub fn merge_staged_sweep(&self, identity: &Identity) -> Result<StagedSweepOutcome, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(if g.merge_staged_sweep(&identity.inner)? {
            StagedSweepOutcome::Merged
        } else {
            StagedSweepOutcome::NothingStaged
        })
    }

    /// **Lost.** Discard the sweep staged by [`Self::stage_pending_proposals`],
    /// returning this group to the epoch the sweep was built against so it can
    /// apply the winner's commit and carry on (ADR-0045).
    ///
    /// This touches one group and no other: the rollback sets **this** group's
    /// state back to operational and writes nothing else, unlike a
    /// whole-provider `restoreStorage`, which would roll sibling groups back
    /// past commits they have already published.
    ///
    /// Returns [`StagedSweepOutcome::Abandoned`], or
    /// [`StagedSweepOutcome::NothingStaged`] when there was nothing staged —
    /// which is what every eagerly-merged path reports, and is why calling this
    /// after one cannot undo anything.
    ///
    /// # Errors
    ///
    /// [`FfiError::Group`] if the discard did not take effect. The core
    /// re-reads the group state afterwards and refuses to report a rollback it
    /// did not actually perform (fail-closed, invariant #4).
    pub fn abandon_staged_sweep(
        &self,
        identity: &Identity,
    ) -> Result<StagedSweepOutcome, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(if g.abandon_staged_sweep(&identity.inner)? {
            StagedSweepOutcome::Abandoned
        } else {
            StagedSweepOutcome::NothingStaged
        })
    }

    /// Hand the group's owner slot to `new_owner`, as a **direct commit**
    /// (ADR-0022's deferred ownership-transfer follow-up, built in ADR-0045).
    ///
    /// `new_owner` is the member's 32-byte Ed25519 signature public key — the
    /// same stable handle [`MemberRecord::signature_public_key`] reports and
    /// the role table keys on, never a leaf index. `outgoing` decides what
    /// becomes of **this** device's own role and is **required**; see
    /// [`OutgoingOwner`].
    ///
    /// **Owner-only and irreversible.** Only the owner may transfer, because a
    /// role change is owner-only and a transfer is a role change; anything
    /// weaker would be a coup, since an admin able to transfer could name
    /// itself. Once the commit lands, only the new owner can rewrite the table.
    /// This does **not** recover a *lost* owner either — minting the commit
    /// needs the owner's key.
    ///
    /// Transfer is what makes leaving possible: an owner may not leave while
    /// owner, but the former owner is an ordinary member and may. The two are
    /// necessarily separate commits — both proposals are judged against the
    /// pre-merge table in which the transferor is still owner, so a fused
    /// transfer-and-leave has its `Leave` half refused.
    ///
    /// Transfer is deliberately **not** a new commit type: it is the ordinary
    /// owner-only role-table rewrite with the owner slot moved, so it rides the
    /// audited [`Self::set_group_roles`] path and is judged by the existing
    /// peer gate. Returns the wire-encoded commit to broadcast; the local epoch
    /// has already advanced, so record it durably before publishing
    /// (ADR-0040). In a group that has moved to the ADR-0045 proposal path,
    /// prefer [`Self::propose_ownership_transfer`].
    ///
    /// # Errors
    ///
    /// [`FfiError::Codec`] when `new_owner` is not 32 bytes.
    /// [`FfiError::Group`] when the group carries no role table, when this
    /// device is not the owner, when `new_owner` is already the owner, or when
    /// `new_owner` is **not a member of this group** — the last refused because
    /// an owner outside the group can never be replaced (a role change is
    /// owner-only) and never removed (the owner's leaf is unremovable), leaving
    /// the group permanently un-administrable. Peers refuse such a commit
    /// independently (C-63), so this local refusal is the clear error, not the
    /// enforcement.
    pub fn transfer_ownership(
        &self,
        identity: &Identity,
        new_owner: Vec<u8>,
        outgoing: OutgoingOwner,
    ) -> Result<Vec<u8>, FfiError> {
        if new_owner.len() != 32 {
            return Err(FfiError::Codec("new owner key must be 32 bytes".into()));
        }
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.transfer_ownership(&identity.inner, &new_owner, outgoing.into())?)
    }

    /// **Propose** handing the group's owner slot to `new_owner` (ADR-0045) —
    /// the proposal-path twin of [`Self::transfer_ownership`], with identical
    /// authorisation and identical refusals.
    ///
    /// The proposal is judged, by this device and by every peer, against the
    /// **pre-merge** table in which this device is still the owner, so it needs
    /// no new rule: it is a role change, which is owner-only. Any owner-or-admin
    /// may sweep it, including the incoming owner. Returns the wire-encoded
    /// Proposal to publish; the epoch does **not** advance.
    ///
    /// # Errors
    ///
    /// As [`Self::transfer_ownership`].
    pub fn propose_ownership_transfer(
        &self,
        identity: &Identity,
        new_owner: Vec<u8>,
        outgoing: OutgoingOwner,
    ) -> Result<Vec<u8>, FfiError> {
        if new_owner.len() != 32 {
            return Err(FfiError::Codec("new owner key must be 32 bytes".into()));
        }
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.propose_ownership_transfer(&identity.inner, &new_owner, outgoing.into())?)
    }

    /// Encrypt an application payload for the current epoch.
    pub fn create_application_message(
        &self,
        identity: &Identity,
        plaintext: Vec<u8>,
    ) -> Result<Vec<u8>, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let msg = g.create_application_message(&identity.inner, &plaintext)?;
        Ok(group::encode_message(&msg)?)
    }

    /// Process an incoming MLS message.
    pub fn process_incoming(
        &self,
        identity: &Identity,
        bytes: Vec<u8>,
    ) -> Result<ProcessedMessage, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let result = g.process_incoming(&identity.inner, &bytes)?;
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
            MlsIncoming::Proposal(record) => ProcessedMessage::Proposal {
                record: record.into(),
            },
            // ADR-0027 Phase 1: the core variant now carries the joiner's
            // signature key; the FFI surface is enriched in Phase 2. Match the
            // new shape and drop the field for now.
            MlsIncoming::ExternalJoinProposal {
                joiner_signature_key,
                retained,
            } => ProcessedMessage::ExternalJoinProposal {
                joiner_signature_key,
                retained,
            },
        })
    }

    /// The ADR-0053 access signer for this journal at the current epoch:
    /// `export_secret("lakko/ds/access/v1", groupId, 32)` wrapped as a signer
    /// whose `sign` produces the `x-lakko-ds-proof` header a claimed journal
    /// requires and whose `groupClaimBodyJson` registers the epoch's key.
    /// Re-derive after every commit.
    pub fn ds_access_signer(
        &self,
        identity: &Identity,
    ) -> Result<Arc<ds_proof::DsProofSigner>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        ds_proof::group_signer(&g, &identity.inner)
    }

    /// The ADR-0053 access signer for the epoch a staged sweep moves to
    /// (N+1), while this group still sits at N; `None` when nothing is
    /// staged. Register its key, proven under the current signer, BEFORE
    /// publishing the sweep, so no peer that applies the commit ever holds an
    /// epoch the Delivery Service does not know.
    pub fn ds_staged_access_signer(
        &self,
        identity: &Identity,
    ) -> Result<Option<Arc<ds_proof::DsProofSigner>>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        ds_proof::staged_group_signer(&g, &identity.inner)
    }

    /// Derive a `length`-byte secret bound to the current epoch.
    pub fn export_secret(
        &self,
        identity: &Identity,
        label: String,
        context: Vec<u8>,
        length: u32,
    ) -> Result<Vec<u8>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let secret = g.export_secret(&identity.inner, &label, &context, length as usize)?;
        Ok(secret.to_vec())
    }

    /// Whether this device is still a member of the group.
    ///
    /// `false` means a Commit removing this device's own leaf has been applied.
    /// The state is **terminal** — RFC 9420 offers no way back, and it survives
    /// a snapshot / restore, so this answers the same at launch as it did in
    /// the poll batch that carried the removal. Rejoining is a new `Group`.
    ///
    /// Nothing in the receive path signals the transition:
    /// `processIncomingVerifyingRoles` returns the same content-less `Commit`
    /// for a removing commit as for any other. **This is the only signal.**
    ///
    /// Once it is `false`, reads still answer (`members`, `epoch`,
    /// `memberCount`, `ownLeafIndex`, `groupId`, `groupRoles`) but are frozen
    /// at the removing epoch and will never move again, while
    /// `createApplicationMessage`, `exportSecret` and every further decrypt
    /// fail — including for rows still sitting unfetched on the DS, since the
    /// refusal is not epoch-scoped.
    ///
    /// The UI gates on it: withdraw the composer and the membership
    /// affordances, stop polling and stop emitting beacons, and keep rendering
    /// the history, which is still readable. Prefer this over inferring
    /// membership from `members()` — the inference rests on a leaf signature
    /// key being the account key, which ADR-0022's per-device-leaf follow-up
    /// would break silently.
    pub fn is_active(&self) -> Result<bool, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.is_active())
    }

    /// Local leaf index of the calling identity.
    pub fn own_leaf_index(&self) -> Result<u32, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.own_leaf_index().u32())
    }

    /// Current group epoch.
    pub fn epoch(&self) -> Result<u64, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.epoch())
    }

    /// Number of members at the current epoch.
    pub fn member_count(&self) -> Result<u32, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(u32::try_from(g.member_count()).unwrap_or(u32::MAX))
    }

    /// Snapshot of every member's `(leaf_index, signature_public_key)` at the
    /// current epoch — the roster the group-chat UI renders. Ordering matches
    /// `lakko_mls::Group::members` (left-to-right over the leaf tree, stable
    /// inside one epoch). Mirror of the wasm `Group.members()`; the native
    /// clients match each `signature_public_key` against their contacts to
    /// label a row, and offer a Remove on every leaf but [`Self::own_leaf_index`].
    pub fn members(&self) -> Result<Vec<GroupMember>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.members()
            .into_iter()
            .map(|m| GroupMember {
                leaf_index: m.leaf_index,
                signature_public_key: m.signature_public_key,
                account_key: m.account_key,
            })
            .collect())
    }

    /// This member's ADR-0022 role in the group, or `None` when the group has
    /// no role table (an open / legacy group). The UI gates the
    /// add / remove / promote affordances on this.
    pub fn own_role(&self, identity: &Identity) -> Result<Option<Role>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.own_role(&identity.inner).map(|r| match r {
            MlsRole::Owner => Role::Owner,
            MlsRole::Admin => Role::Admin,
            MlsRole::Member => Role::Member,
        }))
    }

    /// The group's ADR-0022 role table, or `None` for an open / legacy group.
    pub fn group_roles(&self) -> Result<Option<RoleTable>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.group_roles().map(|t| RoleTable {
            owner: t.owner().to_vec(),
            admins: t.admins().iter().map(|a| a.to_vec()).collect(),
        }))
    }

    /// Rewrite the group's ADR-0022 role table (promote / demote admins). Only
    /// the owner's commit is merged by peers (enforced on the receiving side by
    /// [`Self::process_incoming_verifying_roles`]). `owner` and every `admins`
    /// entry must be a 32-byte Ed25519 signature public key. Returns the
    /// `GroupContextExtensions` commit wire bytes to broadcast to the journal.
    pub fn set_group_roles(
        &self,
        identity: &Identity,
        owner: Vec<u8>,
        admins: Vec<Vec<u8>>,
    ) -> Result<Vec<u8>, FfiError> {
        let table = role_table_from_parts(owner, admins)?;
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.set_group_roles(&identity.inner, &table)?)
    }

    /// Process one row read from this journal's **knock** queue (ADR-0053).
    /// Anything that is not a stranger's external-join proposal for this group
    /// is refused BEFORE it is processed - a knock row is an unauthenticated
    /// write, and applying a commit or message someone posted there instead of
    /// the journal would fork the admins who read knocks from everyone else.
    /// A row that passes is processed exactly like `processIncomingVerifyingRoles`.
    pub fn process_knock(
        &self,
        identity: &Identity,
        bytes: Vec<u8>,
    ) -> Result<ProcessedMessage, FfiError> {
        {
            let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
            if !lakko_mls::is_external_join_proposal_for(&bytes, &g.group_id()) {
                return Err(FfiError::Message(
                    "knock row is not an external-join proposal for this group".to_string(),
                ));
            }
        }
        self.process_incoming_verifying_roles(identity, bytes)
    }

    /// Apply an incoming commit ENFORCING the ADR-0022 role policy — a commit
    /// whose committer's role does not authorise its add / remove / role change
    /// is REFUSED (not merged), fail-closed. A group with no role table behaves
    /// like [`Self::process_incoming`]. Use this on the chat-message receive
    /// loop; do NOT use it for call-group receives (the call group is
    /// role-enabled but its welcome-ferry / KP-forward adds are legitimately
    /// non-owner — ADR-0022 call-path caveat).
    pub fn process_incoming_verifying_roles(
        &self,
        identity: &Identity,
        bytes: Vec<u8>,
    ) -> Result<ProcessedMessage, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let result = g.process_incoming_verifying_roles(&identity.inner, &bytes)?;
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
            MlsIncoming::Proposal(record) => ProcessedMessage::Proposal {
                record: record.into(),
            },
            // ADR-0027 Phase 1: the core variant now carries the joiner's
            // signature key; the FFI surface is enriched in Phase 2. Match the
            // new shape and drop the field for now.
            MlsIncoming::ExternalJoinProposal {
                joiner_signature_key,
                retained,
            } => ProcessedMessage::ExternalJoinProposal {
                joiner_signature_key,
                retained,
            },
        })
    }

    /// Group identifier bytes (RFC 9420 `group_id`).
    ///
    /// Stable across the lifetime of the group and at every
    /// epoch — `lakko-svc-ds` uses this value (hex-encoded) to
    /// key the `/ds/group/{group_id_hex}/messages` journal that
    /// carries application messages + commits.
    ///
    /// Length is cipher-suite-dependent (32 bytes for the
    /// suites we ship; the underlying MLS layer does not promise
    /// a fixed size, so callers should treat the byte array as
    /// opaque and just hand it to `DsClient.publishGroup` /
    /// `DsClient.fetchGroup`).
    pub fn group_id(&self) -> Result<Vec<u8>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.group_id().clone())
    }
}

/// Load an existing MLS group from the provider's persisted
/// storage by its `group_id`.
///
/// Returns `null` (on the Kotlin side; `None` on the Rust side
/// — UniFFI maps the `Option` to the platform's null shape)
/// when the provider has no group with that id (e.g. a
/// freshly-bootstrapped session that hasn't yet rehydrated, or
/// a typo in the id the caller is passing).
///
/// **Bootstrap-side eager rebuild** (1.15f5) calls this for
/// every previously-known group id after
/// [`Identity::restore_storage`] runs — the resulting `Group`
/// handles let `pollGroup` decrypt peers' commits + application
/// messages without first having to wait for a user-initiated
/// send / receive on each thread.
///
/// Free function rather than `Group::load` constructor because
/// UniFFI 0.28 rejects `Option<Self>` in constructor signatures —
/// the standard workaround for "constructor that may return
/// null", same shape as `pow_solve` (1.15d1).
#[uniffi::export]
pub fn load_group(identity: &Identity, group_id: Vec<u8>) -> Result<Option<Arc<Group>>, FfiError> {
    let loaded = MlsGroup::load(&identity.inner, &group_id)?;
    Ok(loaded.map(|inner| {
        Arc::new(Group {
            inner: Mutex::new(inner),
        })
    }))
}

/// Extract the 32-byte leaf (Ed25519 signature) public key from a wire-form
/// KeyPackage — the stable per-identity key that distinct KeyPackages minted by
/// the same identity share (only the HPKE init key is fresh per KP). Used by the
/// call-admission dedup (ADR-0032 / G-22) so a peer re-minting fresh KeyPackages
/// under one identity is admitted at most once.
///
/// # Errors
///
/// [`FfiError`] on a KeyPackage parse failure.
#[uniffi::export]
pub fn key_package_signature_key(key_package: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    Ok(key_package::signature_key(&key_package)?)
}

/// Resolve a wire-form KeyPackage's leaf to the **account** it belongs to
/// (ADR-0050): the device binding's account key, the leaf key itself for an
/// unbound Phase 1 leaf, or `None` for a leaf whose binding does not verify.
/// The call-admission dedup and a contact's device list key on this.
///
/// # Errors
///
/// [`FfiError::Codec`] on parse failure.
#[uniffi::export]
pub fn key_package_account_key(key_package: Vec<u8>) -> Result<Option<Vec<u8>>, FfiError> {
    Ok(key_package::account_key(&key_package)?.map(|k| k.to_vec()))
}

// ============================================================================
// PublisherGroup (G4-2-mls-persistence step 2 — FFI surface for the persistent
// Tier-1 MLS group that owns the channel content-key rotation authority. See
// `crates/lakko-mls/src/publisher_group.rs` for the Rust core; the wrapper here
// only mediates the `Mutex<>` + UniFFI handle conventions the iOS / Android
// composers consume.)
// ============================================================================

/// Persistent MLS-group wrapper for one Lakko channel's
/// publisher tier (ADR-0010 Tier 1).
///
/// Mirror of [`Group`] but with the channel-id keyed
/// persistence + the `channel_seed` exporter shortcut baked
/// in. The iOS / Android channels composer should construct
/// via [`PublisherGroup::open_or_create`] on every publish —
/// the load-or-create dance ensures the MLS state from a
/// prior session is reused (so the exporter output is stable
/// across app restarts).
#[derive(uniffi::Object)]
pub struct PublisherGroup {
    inner: Mutex<MlsPublisherGroup>,
}

#[uniffi::export]
impl PublisherGroup {
    /// Load the persisted PublisherGroup for `channel_id`.
    /// Returns `null` if no group exists yet for this channel
    /// under the caller's identity — pair with
    /// [`PublisherGroup::create`] or use the
    /// [`publisher_group_open_or_create`] free function for
    /// the load-or-create shortcut.
    ///
    /// Free-function shape (not a constructor) because UniFFI
    /// 0.28 rejects `Option<Self>` in `#[uniffi::constructor]`
    /// signatures — same workaround as [`load_group`].
    #[uniffi::constructor]
    pub fn create(identity: &Identity, channel_id: Vec<u8>) -> Result<Self, FfiError> {
        let cid = MlsChannelId::try_from_slice(&channel_id)?;
        let inner = MlsPublisherGroup::create(&identity.inner, &cid)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// 32-byte channel content-key seed derived from the MLS
    /// exporter at the **current** group epoch. Stable across
    /// process restarts once the group has been persisted —
    /// that is the load-bearing invariant
    /// G4-2-mls-persistence closes.
    ///
    /// Wire shape matches the existing
    /// `channel_seed_from_mls_exporter_output` byte-pipe:
    /// 32 bytes the channels composer hands to
    /// `compose_channel_post` as the per-channel content seed.
    pub fn channel_seed(&self, identity: &Identity) -> Result<Vec<u8>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let seed = g.channel_seed(&identity.inner)?;
        Ok(seed.to_vec())
    }

    /// Channel id this group was minted for.
    pub fn channel_id(&self) -> Result<Vec<u8>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.channel_id().as_bytes().to_vec())
    }

    /// G4-2-mls-persistence step 4 — rotate the channel
    /// content-key seed by advancing the MLS group epoch
    /// via a self-update commit. Returns the wire-encoded
    /// commit bytes; the caller broadcasts them to every
    /// other PublisherGroup member so they advance to the
    /// same epoch in lock-step.
    ///
    /// After `rotate` returns, the next [`channel_seed`]
    /// call against this handle (or any future
    /// [`publisher_group_load`]) yields the **new-epoch**
    /// seed. Pair with `seal_subscriber_seed_envelope` +
    /// the services/channels batch fanout to reseal the
    /// seed to every subscriber under the new epoch id.
    pub fn rotate(&self, identity: &Identity) -> Result<Vec<u8>, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.rotate(&identity.inner)?)
    }

    // ADR-0028 OQ3 — co-publisher external-join surface. A PublisherGroup is a
    // vanilla MLS group, so adding a publisher reuses the group join-ticket +
    // external-join machinery (ADR-0027/0028) verbatim. These delegate to the
    // inner Group; the owner mints a `/g/` link over `group_id`, applies the
    // joiner's external-add via `process_incoming_verifying_roles`, and approves.

    /// The ADR-0053 access signer for the publisher journal at the current
    /// epoch - the same derivation as `Group.dsAccessSigner` over the
    /// underlying group. Re-derive after every commit (every rotation).
    pub fn ds_access_signer(
        &self,
        identity: &Identity,
    ) -> Result<Arc<ds_proof::DsProofSigner>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        ds_proof::group_signer(g.inner(), &identity.inner)
    }

    /// ADR-0028 OQ3 — the underlying MLS `group_id`. A co-publisher invite-link
    /// is minted over this; the joiner targets it when crafting its
    /// external-join proposal.
    pub fn group_id(&self) -> Result<Vec<u8>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.group_id())
    }

    /// Process one row read from this journal's **knock** queue (ADR-0053).
    /// Anything that is not a stranger's external-join proposal for this group
    /// is refused BEFORE it is processed - a knock row is an unauthenticated
    /// write, and applying a commit or message someone posted there instead of
    /// the journal would fork the admins who read knocks from everyone else.
    /// A row that passes is processed exactly like `processIncomingVerifyingRoles`.
    pub fn process_knock(
        &self,
        identity: &Identity,
        bytes: Vec<u8>,
    ) -> Result<ProcessedMessage, FfiError> {
        {
            let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
            if !lakko_mls::is_external_join_proposal_for(&bytes, &g.group_id()) {
                return Err(FfiError::Message(
                    "knock row is not an external-join proposal for this group".to_string(),
                ));
            }
        }
        self.process_incoming_verifying_roles(identity, bytes)
    }

    /// ADR-0028 OQ3 — process an incoming MLS message on the publisher tier,
    /// role-gated (ADR-0022). Surfaces a non-member's external-join proposal as
    /// `ProcessedMessage::ExternalJoinProposal` so the owner can approve a
    /// co-publisher.
    pub fn process_incoming_verifying_roles(
        &self,
        identity: &Identity,
        bytes: Vec<u8>,
    ) -> Result<ProcessedMessage, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let result = g
            .inner_mut()
            .process_incoming_verifying_roles(&identity.inner, &bytes)?;
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
            MlsIncoming::Proposal(record) => ProcessedMessage::Proposal {
                record: record.into(),
            },
            MlsIncoming::ExternalJoinProposal {
                joiner_signature_key,
                retained,
            } => ProcessedMessage::ExternalJoinProposal {
                joiner_signature_key,
                retained,
            },
        })
    }

    /// ADR-0028 OQ3 — the signature key of every pending co-publisher
    /// external-join proposal (a handle for `approve_external_join` /
    /// `reject_external_join`).
    pub fn pending_external_joins(&self) -> Result<Vec<Vec<u8>>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.inner().pending_external_joins())
    }

    /// ADR-0028 OQ3 — approve a retained co-publisher external-join; returns
    /// the Commit + the joiner's Welcome, as it always has.
    ///
    /// **This is the direct approve, and this is the only type it survives on**
    /// (ADR-0045 S7). A PublisherGroup carries no ADR-0022 role table, so it has
    /// no committer to sweep a proposal, and ADR-0045 leaves such a group on
    /// direct commits. The return type is the difference and it is deliberate:
    /// [`Group::approve_external_join`] hands back proposal bytes and no
    /// Welcome, this hands back a merged Commit and a Welcome to deliver now,
    /// and neither can be used where the other is expected.
    pub fn approve_external_join(
        &self,
        identity: &Identity,
        proposal_ref: Vec<u8>,
    ) -> Result<AddMember, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let MemberAddition { commit, welcome } =
            g.approve_external_join(&identity.inner, &proposal_ref)?;
        Ok(AddMember {
            commit: group::encode_message(&commit)?,
            welcome: group::encode_message(&welcome)?,
        })
    }

    /// ADR-0028 OQ3 — reject a retained co-publisher external-join (local drop;
    /// no epoch advance).
    /// `identity` is what the durable copy of the pending set is written
    /// through (the request set rides the MLS storage snapshot, so a decision
    /// survives a restart).
    pub fn reject_external_join(
        &self,
        identity: &Identity,
        proposal_ref: Vec<u8>,
    ) -> Result<(), FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        g.inner_mut()
            .reject_external_join(&identity.inner, &proposal_ref)?;
        Ok(())
    }
}

/// Load the persisted PublisherGroup for `channel_id` if any.
/// Returns `null` when no group has been minted yet for this
/// (identity, channel_id) pair under `identity`'s provider
/// storage.
///
/// Free function rather than a constructor because UniFFI
/// 0.28 rejects `Option<Self>` in constructor returns — same
/// shape rationale as the existing [`load_group`].
#[uniffi::export]
pub fn publisher_group_load(
    identity: &Identity,
    channel_id: Vec<u8>,
) -> Result<Option<Arc<PublisherGroup>>, FfiError> {
    let cid = MlsChannelId::try_from_slice(&channel_id)?;
    let loaded = MlsPublisherGroup::load(&identity.inner, &cid)?;
    Ok(loaded.map(|inner| {
        Arc::new(PublisherGroup {
            inner: Mutex::new(inner),
        })
    }))
}

/// Load-or-create convenience — the path every composer
/// should take. Tries [`publisher_group_load`] first; on
/// `null`, mints a fresh group via
/// [`PublisherGroup::create`]. Idempotent: once a group has
/// been minted for `(identity, channel_id)`, every
/// subsequent call returns the same persisted state and
/// therefore the same exporter output for the current
/// epoch.
#[uniffi::export]
pub fn publisher_group_open_or_create(
    identity: &Identity,
    channel_id: Vec<u8>,
) -> Result<Arc<PublisherGroup>, FfiError> {
    if let Some(existing) = publisher_group_load(identity, channel_id.clone())? {
        return Ok(existing);
    }
    let created = PublisherGroup::create(identity, channel_id)?;
    Ok(Arc::new(created))
}

/// ADR-0028 OQ3 — recover the 16-byte channel id from a PublisherGroup
/// `group_id` (the inverse of the deterministic derivation). Returns `None` for
/// bytes that do not carry the publisher-group prefix. A co-publisher uses this
/// after joining via a `/g/` ticket — which conveys only the `group_id` — to
/// reconstruct the channel + derive its content-key seed.
#[uniffi::export]
#[must_use]
pub fn channel_id_from_publisher_group_id(group_id: Vec<u8>) -> Option<Vec<u8>> {
    lakko_mls::channel_id_from_group_id(&group_id).map(|c| c.as_bytes().to_vec())
}

// ============================================================================
// Channel rotation policy (G4-2-rotation-timer)
// ============================================================================
//
// Mirrors `lakko_channels::rotation_policy`. Pure logic — no
// I/O, no clock side-effects. The Swift / Kotlin call site
// passes `now_ms` from `Date().timeIntervalSince1970 * 1000`
// / `System.currentTimeMillis()` so the host's monotonic-
// adjacent clock drives the decision.

/// FFI mirror of [`lakko_channels::rotation_policy::RotationPolicy`].
#[derive(uniffi::Record, Clone, Copy)]
pub struct ChannelRotationPolicy {
    /// Maximum milliseconds between rotations. `0` disables
    /// the time trigger.
    pub interval_ms: u64,
    /// Maximum posts between rotations. `0` disables the
    /// count trigger.
    pub max_posts: u32,
}

impl From<ChannelRotationPolicy> for lakko_channels::RotationPolicy {
    fn from(p: ChannelRotationPolicy) -> Self {
        Self::new(p.interval_ms, p.max_posts)
    }
}

/// FFI mirror of [`lakko_channels::rotation_policy::RotationState`].
/// Caller-persisted snapshot of when the channel was last
/// rotated + how many posts since.
#[derive(uniffi::Record, Clone, Copy)]
pub struct ChannelRotationState {
    /// Unix milliseconds when the last rotation completed.
    /// `0` means "never rotated yet".
    pub last_rotation_ms: u64,
    /// Count of posts published since the last rotation.
    pub posts_since_rotation: u32,
}

impl From<ChannelRotationState> for lakko_channels::RotationState {
    fn from(s: ChannelRotationState) -> Self {
        Self {
            last_rotation_ms: s.last_rotation_ms,
            posts_since_rotation: s.posts_since_rotation,
        }
    }
}

impl From<lakko_channels::RotationState> for ChannelRotationState {
    fn from(s: lakko_channels::RotationState) -> Self {
        Self {
            last_rotation_ms: s.last_rotation_ms,
            posts_since_rotation: s.posts_since_rotation,
        }
    }
}

/// Default rotation policy — 24 h or 1 000 posts, whichever
/// lands first. Pinned in ADR-0010 §"PublisherGroup".
#[uniffi::export]
pub fn channel_rotation_policy_default() -> ChannelRotationPolicy {
    let p = lakko_channels::RotationPolicy::DEFAULT;
    ChannelRotationPolicy {
        interval_ms: p.interval_ms,
        max_posts: p.max_posts,
    }
}

/// Decide whether the publisher should rotate now. Pure
/// function — see crate docs. Saturating arithmetic on the
/// time delta keeps a host clock jump backward from
/// surfacing as a spurious rotation.
#[uniffi::export]
pub fn channel_should_rotate(
    policy: ChannelRotationPolicy,
    state: ChannelRotationState,
    now_ms: u64,
) -> bool {
    let p: lakko_channels::RotationPolicy = policy.into();
    let s: lakko_channels::RotationState = state.into();
    p.should_rotate(now_ms, s)
}

/// Stamp a fresh rotation state right after a successful
/// fanout — server returned 2xx and the new epoch landed.
#[uniffi::export]
pub fn channel_rotation_state_after_rotation(now_ms: u64) -> ChannelRotationState {
    lakko_channels::RotationState::after_rotation(now_ms).into()
}

/// Bump the post counter (saturating at `u32::MAX`). Called
/// after every successfully-published `ChannelPost`.
#[uniffi::export]
pub fn channel_rotation_state_after_post(state: ChannelRotationState) -> ChannelRotationState {
    let s: lakko_channels::RotationState = state.into();
    s.after_post().into()
}

// ============================================================================
// DeviceGroup (Phase 2a3b1d)
// ============================================================================

/// Per-user multi-device MLS group (ADR-0007).
///
/// Wraps `lakko_mls::DeviceGroup` for FFI use. The leaves are
/// the user's own devices; the deterministic `group_id` is
/// `BLAKE3("lakko-device-group/v1" || account_id)`.
///
/// FFI subset of what `lakko_mls::DeviceGroup` exposes — just
/// what 2a3b's add-device flow needs:
///
///   * Bootstrap a fresh device group (`create_founder`).
///   * Join an already-running device group from a Welcome
///     (`join_from_welcome`).
///   * Add a new device by its KeyPackage bytes
///     (`add_device_from_wire`) — returns the
///     (commit, welcome) pair the caller fans out.
///   * Read the deterministic `group_id`.
///
/// 2a3c will widen this with `devices()` + `remove_device(...)`
/// once the per-platform UI lands.
#[derive(uniffi::Object)]
pub struct DeviceGroup {
    inner: Mutex<MlsDeviceGroup>,
}

#[uniffi::export]
impl DeviceGroup {
    /// Bootstrap a fresh DeviceGroup with `identity` as the
    /// founding (and only) leaf.
    ///
    /// `account_id` MUST be the raw 16-byte UUIDv7. The
    /// resulting group's `group_id` is derived
    /// deterministically — every device that knows the same
    /// `account_id` agrees on the same `group_id` without
    /// server coordination.
    #[uniffi::constructor]
    pub fn create_founder(identity: &Identity, account_id: Vec<u8>) -> Result<Self, FfiError> {
        if account_id.len() != 16 {
            return Err(FfiError::Codec("account_id must be 16 bytes".into()));
        }
        let mut acct = [0u8; 16];
        acct.copy_from_slice(&account_id);
        let inner = MlsDeviceGroup::create_founder(&identity.inner, &acct)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// Join a DeviceGroup from a Welcome the scanner device
    /// produced via [`DeviceGroup::add_device_from_wire`].
    ///
    /// Verifies that the embedded `group_id` matches
    /// `BLAKE3("lakko-device-group/v1" || account_id)` — a
    /// misrouted Welcome from a different user's DeviceGroup
    /// is rejected before any state advance.
    #[uniffi::constructor]
    pub fn join_from_welcome(
        welcome: Vec<u8>,
        identity: &Identity,
        account_id: Vec<u8>,
    ) -> Result<Self, FfiError> {
        if account_id.len() != 16 {
            return Err(FfiError::Codec("account_id must be 16 bytes".into()));
        }
        let mut acct = [0u8; 16];
        acct.copy_from_slice(&account_id);
        let inner = MlsDeviceGroup::join_from_welcome(&welcome, &identity.inner, &acct)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// Add a device by its TLS-encoded KeyPackage. Returns the
    /// `(commit, welcome)` pair:
    ///
    ///   * `commit` — broadcast to every existing DeviceGroup
    ///     member via the DS group journal.
    ///   * `welcome` — sent to the joining device via the DS
    ///     mailbox at the rendezvous pseudonym the pairing QR
    ///     supplied.
    pub fn add_device_from_wire(
        &self,
        identity: &Identity,
        key_package: Vec<u8>,
    ) -> Result<AddMember, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let MemberAddition { commit, welcome } =
            g.add_device_from_wire(&identity.inner, &key_package)?;
        Ok(AddMember {
            commit: group::encode_message(&commit)?,
            welcome: group::encode_message(&welcome)?,
        })
    }

    /// The ADR-0053 access signer for the DeviceGroup journal at the current
    /// epoch - the same derivation as `Group.dsAccessSigner` over the
    /// underlying group. Re-derive after every commit.
    pub fn ds_access_signer(
        &self,
        identity: &Identity,
    ) -> Result<Arc<ds_proof::DsProofSigner>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        ds_proof::group_signer(g.as_group(), &identity.inner)
    }

    /// The deterministic `group_id` this DeviceGroup is bound
    /// to. Same value `MlsDeviceGroup::group_id_for(account_id)`
    /// computes for the same `account_id`.
    pub fn group_id(&self) -> Result<Vec<u8>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.group_id())
    }

    /// Snapshot of every device leaf currently in the
    /// DeviceGroup (Phase 2a3c). One row per device — the UI
    /// renders the list, marks the row whose `leaf_index`
    /// matches [`Self::own_leaf_index`] as "(this device)",
    /// and offers a Remove affordance on every other row.
    pub fn devices(&self) -> Result<Vec<DeviceLeaf>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.devices()
            .into_iter()
            .map(|m| DeviceLeaf {
                leaf_index: m.leaf_index,
                signature_public_key: m.signature_public_key,
            })
            .collect())
    }

    /// Leaf index of THIS device inside the DeviceGroup
    /// (Phase 2a3c). See [`MlsDeviceGroup::own_leaf_index`] for
    /// the lifecycle caveats — caller MUST NOT cache across
    /// state changes.
    pub fn own_leaf_index(&self) -> Result<u32, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.own_leaf_index())
    }

    /// Revoke a device leaf (Phase 2a3c). Returns the encoded
    /// Commit bytes that the caller MUST broadcast to every
    /// remaining device via the DS group journal so they
    /// advance past the revoked leaf and ratchet the group
    /// secrets out of the removed device's reach.
    ///
    /// `leaf_index` must come from a [`Self::devices`] snapshot
    /// at the SAME epoch; concurrent commits can renumber
    /// leaves, in which case the caller refetches and retries.
    /// Attempting to remove the local device's own leaf
    /// surfaces as `FfiError::Mls("…cannot remove own leaf…")`.
    pub fn remove_device_at(
        &self,
        identity: &Identity,
        leaf_index: u32,
    ) -> Result<Vec<u8>, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let bytes = g.remove_device_at(&identity.inner, leaf_index)?;
        Ok(bytes)
    }

    /// Derive the per-pairing history-bundle exporter secret
    /// (Phase 2a3d8). Returns 32 bytes; the scanner side feeds
    /// it into [`seal_history_bundle`], the joining side feeds
    /// it into [`open_history_bundle`]. Both devices reach the
    /// same value because they end up at the same DeviceGroup
    /// epoch after add_device_from_wire / join_from_welcome.
    pub fn history_bundle_secret(&self, identity: &Identity) -> Result<Vec<u8>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let secret = lakko_mls::history_bundle::exporter_secret(&g, &identity.inner)?;
        Ok(secret.to_vec())
    }

    /// Derive a `length`-byte secret bound to this DeviceGroup's current
    /// epoch (ADR-0018 Part 2a, factor (a) — "another-device" identity
    /// transfer, Path B). Mirrors [`Group::export_secret`]. A **read-only**
    /// MLS exporter call (no state mutation). After a fresh device joins via
    /// [`DeviceGroup::join_from_welcome`], the existing and fresh device sit at
    /// the same epoch and derive the SAME bytes for the same
    /// `(label, context, length)`; the existing device seals
    /// `(grant_token ‖ phrase_seed)` under this key (via [`seal_with_key`])
    /// and the fresh device opens it (via [`open_with_key`]).
    pub fn export_secret(
        &self,
        identity: &Identity,
        label: String,
        context: Vec<u8>,
        length: u32,
    ) -> Result<Vec<u8>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let secret = g.export_secret(&identity.inner, &label, &context, length as usize)?;
        Ok(secret.to_vec())
    }

    /// Compose + encrypt a typed cross-device payload
    /// (Phase 2a3e2). Wraps
    /// [`lakko_mls::DeviceGroup::create_devicegroup_message`]
    /// with a FFI-friendly enum. Returns the wire-format MLS
    /// application-message bytes ready to PUT to the
    /// DeviceGroup DS journal.
    ///
    /// `hlc` is the caller-supplied hybrid logical clock value
    /// — callers wire a single per-account HLC generator at
    /// the application layer so total ordering survives across
    /// devices.
    pub fn create_devicegroup_message(
        &self,
        identity: &Identity,
        body: DeviceGroupBody,
        hlc: u64,
    ) -> Result<Vec<u8>, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let bytes = g.create_devicegroup_message(&identity.inner, body.into(), hlc)?;
        Ok(bytes)
    }

    /// Decrypt / advance the state machine for an inbound
    /// DeviceGroup wire message (Phase 2a3e2). Same shape as
    /// [`Group::process_incoming`]:
    ///
    ///   * `ProcessedMessage::Application { data }` — the
    ///     application-message plaintext. Pass `data` to
    ///     [`parse_devicegroup_payload`] to get a typed
    ///     `ParsedDeviceGroupPayload`.
    ///   * `ProcessedMessage::Commit` — a Commit was processed and
    ///     merged. DeviceGroup state advanced. Caller refreshes
    ///     its `devices()` snapshot.
    ///   * `ProcessedMessage::Proposal` /
    ///     `::ExternalJoinProposal` — defensive; no state
    ///     advance.
    pub fn process_incoming(
        &self,
        identity: &Identity,
        message_bytes: Vec<u8>,
    ) -> Result<ProcessedMessage, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let incoming = g.process_incoming(&identity.inner, &message_bytes)?;
        Ok(match incoming {
            lakko_mls::Incoming::Application {
                bytes,
                sender_leaf,
                sender_account,
            } => ProcessedMessage::Application {
                data: bytes,
                sender_leaf,
                sender_account,
            },
            lakko_mls::Incoming::Commit => ProcessedMessage::Commit,
            lakko_mls::Incoming::Proposal(record) => ProcessedMessage::Proposal {
                record: record.into(),
            },
            // ADR-0027 Phase 1: drop the new joiner-key field (Phase 2 enriches).
            lakko_mls::Incoming::ExternalJoinProposal {
                joiner_signature_key,
                retained,
            } => ProcessedMessage::ExternalJoinProposal {
                joiner_signature_key,
                retained,
            },
        })
    }

    /// G-13 (ADR-0015): flip this DeviceGroup's policy to **require a
    /// peer-verified attestation for device removals**. Returns the encoded
    /// Commit the caller broadcasts over the DS group journal; every device
    /// that processes it then enforces the policy. Call when the account
    /// first enables a passkey (OPAQUE-only accounts never call it).
    pub fn set_require_attested_removals(&self, identity: &Identity) -> Result<Vec<u8>, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let bytes = g.set_require_attested_removals(&identity.inner)?;
        Ok(bytes)
    }

    /// G-13 (ADR-0015): whether this DeviceGroup currently requires attested
    /// removals (read from authenticated group state).
    pub fn require_attested_removals(&self) -> Result<bool, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.require_attested_removals())
    }

    /// G-13 (ADR-0015): apply an incoming DeviceGroup commit **enforcing the
    /// attested-removal policy**. When the policy is on and the commit
    /// removes leaves, every removed leaf must be covered by an attestation
    /// in `attestations` that verifies under `attest_verifying_key` (the
    /// 32-byte Ed25519 key the auth service publishes at the device-attest
    /// JWKS) — bound to this account, the removed leaf, and fresh at
    /// `now_unix_secs`. A removal lacking a valid attestation is refused (the
    /// commit is NOT merged) and surfaces as `FfiError::Group`. Policy-off /
    /// non-remove commits behave like [`Self::process_incoming`].
    pub fn process_incoming_verifying_removals(
        &self,
        identity: &Identity,
        message_bytes: Vec<u8>,
        attestations: Vec<Vec<u8>>,
        attest_verifying_key: Vec<u8>,
        now_unix_secs: i64,
    ) -> Result<ProcessedMessage, FfiError> {
        let vk = lakko_crypto::signing::VerifyingKey::from_bytes(&attest_verifying_key)
            .map_err(|_| FfiError::Identity("invalid attestation verifying key".into()))?;
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let incoming = g.process_incoming_verifying_removals(
            &identity.inner,
            &message_bytes,
            &attestations,
            &vk,
            now_unix_secs,
        )?;
        Ok(match incoming {
            lakko_mls::Incoming::Application {
                bytes,
                sender_leaf,
                sender_account,
            } => ProcessedMessage::Application {
                data: bytes,
                sender_leaf,
                sender_account,
            },
            lakko_mls::Incoming::Commit => ProcessedMessage::Commit,
            lakko_mls::Incoming::Proposal(record) => ProcessedMessage::Proposal {
                record: record.into(),
            },
            // ADR-0027 Phase 1: drop the new joiner-key field (Phase 2 enriches).
            lakko_mls::Incoming::ExternalJoinProposal {
                joiner_signature_key,
                retained,
            } => ProcessedMessage::ExternalJoinProposal {
                joiner_signature_key,
                retained,
            },
        })
    }
}

/// G-13 (ADR-0015): the action hash a remove-device passkey ceremony and its
/// attestation bind to — `SHA-256("lakko/action/remove-device/v1" ‖
/// removed_leaf_signature_pubkey)`. A client computes this from the leaf it
/// is about to remove to drive the `/auth/action/{start,finish}` ceremony,
/// so it matches what a peer recomputes when verifying the attestation.
#[uniffi::export]
pub fn remove_device_action_hash(removed_signature_pubkey: Vec<u8>) -> Vec<u8> {
    lakko_mls::device_group::remove_device_action_hash(&removed_signature_pubkey).to_vec()
}

/// Parse a DeviceGroup application-payload plaintext into the
/// typed [`ParsedDeviceGroupPayload`] (Phase 2a3e2). Callers
/// feed in the `data` field of
/// [`ProcessedMessage::Application`] from
/// [`DeviceGroup::process_incoming`].
///
/// Free function because the receiver-side flow is
/// stateless — no `DeviceGroup` state to consult.
///
/// # Errors
///
/// - [`FfiError::Codec`] when the proto decode fails.
/// - [`FfiError::Mls`] with an `Unsupported` body when the
///   payload's `body` is absent or carries a variant this
///   binary doesn't recognise (older client reading a
///   future-version payload).
#[uniffi::export]
pub fn parse_devicegroup_payload(plaintext: Vec<u8>) -> Result<ParsedDeviceGroupPayload, FfiError> {
    let parsed = lakko_mls::DeviceGroup::parse_devicegroup_payload(&plaintext)?;
    Ok(ParsedDeviceGroupPayload {
        hlc: parsed.hlc,
        sender_leaf: parsed.sender_leaf,
        body: parsed.body.into(),
    })
}

/// AEAD-seal a [`lakko_proto::mls_app::history_bundle::v1::HistoryBundle`]
/// (Phase 2a3d8).
///
/// `secret` MUST be exactly 32 bytes — typically the output of
/// [`DeviceGroup::history_bundle_secret`]. `bundle_bytes` is
/// the prost-encoded `HistoryBundle` proto; the caller builds
/// it from its local message store. Returns the wire-format
/// [`SealedBundle`] proto bytes ready to PUT to the rendezvous
/// mailbox.
///
/// Refuses to seal bundles whose total message count exceeds
/// [`lakko_mls::history_bundle::MAX_BUNDLE_MESSAGES`] (1000).
#[uniffi::export]
pub fn seal_history_bundle(secret: Vec<u8>, bundle_bytes: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    use prost::Message as _;
    if secret.len() != 32 {
        return Err(FfiError::Codec(
            "history-bundle secret must be 32 bytes".into(),
        ));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&secret);

    let bundle =
        lakko_proto::mls_app::history_bundle::v1::HistoryBundle::decode(bundle_bytes.as_slice())
            .map_err(|e| FfiError::Codec(format!("HistoryBundle decode: {e}")))?;
    let sealed = lakko_mls::history_bundle::seal_bundle(&key, &bundle)?;
    Ok(sealed)
}

/// Inverse of [`seal_history_bundle`]. Returns the prost-encoded
/// `HistoryBundle` bytes (the caller decodes them in its native
/// proto runtime — TypeScript via the wasm proto, Swift via
/// `SwiftProtobuf`, Kotlin via the generated `lakko-proto`
/// classes).
#[uniffi::export]
pub fn open_history_bundle(secret: Vec<u8>, sealed_bytes: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    use prost::Message as _;
    if secret.len() != 32 {
        return Err(FfiError::Codec(
            "history-bundle secret must be 32 bytes".into(),
        ));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&secret);

    let bundle = lakko_mls::history_bundle::open_bundle(&key, &sealed_bytes)?;
    let mut out = Vec::with_capacity(bundle.encoded_len());
    bundle
        .encode(&mut out)
        .map_err(|e| FfiError::Codec(format!("HistoryBundle re-encode: {e}")))?;
    Ok(out)
}

/// AEAD-seal `plaintext` under a raw 32-byte `key` (ADR-0018 Part 2a, factor
/// (a) — "another-device" identity transfer, Path B). Returns the self-
/// describing wire blob `nonce(12) ‖ ciphertext+tag` (ChaCha20-Poly1305, empty
/// AAD; the nonce is generated internally and prepended). The existing device
/// derives `key` via [`DeviceGroup::export_secret`] and seals
/// `(grant_token ‖ phrase_seed)` here, then publishes the blob to the
/// rendezvous mailbox; the fresh device derives the same `key` and opens it
/// via [`open_with_key`].
///
/// # Errors
///
/// - [`FfiError::Crypto`] when `key` is not exactly 32 bytes, or on internal
///   AEAD failure.
#[uniffi::export]
pub fn seal_with_key(key: Vec<u8>, plaintext: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    let key = key_32(&key)?;
    lakko_crypto::aead::seal_with_key(&key, &plaintext)
        .map_err(|e| FfiError::Crypto(format!("seal_with_key: {e}")))
}

/// Inverse of [`seal_with_key`]: open a `nonce(12) ‖ ciphertext+tag` wire blob
/// under the same raw 32-byte `key`. Returns the recovered plaintext.
///
/// # Errors
///
/// - [`FfiError::Crypto`] when `key` is not exactly 32 bytes, the blob is too
///   short, or authentication fails (wrong key / tampered blob).
#[uniffi::export]
pub fn open_with_key(key: Vec<u8>, wire: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    let key = key_32(&key)?;
    lakko_crypto::aead::open_with_key(&key, &wire)
        .map_err(|e| FfiError::Crypto(format!("open_with_key: {e}")))
}

/// Validate + copy a caller-supplied key into a fixed 32-byte array, the input
/// shape [`lakko_crypto::aead::seal_with_key`] / `open_with_key` require.
fn key_32(key: &[u8]) -> Result<[u8; 32], FfiError> {
    let arr: [u8; 32] = key
        .try_into()
        .map_err(|_| FfiError::Crypto(format!("key must be 32 bytes, got {}", key.len())))?;
    Ok(arr)
}

// ============================================================================
// Phase 2a3d8-ui — structured history-bundle FFI
//
// The byte-oriented `seal_history_bundle` / `open_history_bundle` above
// assume the caller hands in / receives a prost-encoded `HistoryBundle`
// proto. Three clients × three proto runtimes isn't a great trade so
// the surface below mirrors the same operation in plain `uniffi::Record`
// + `uniffi::Object` types. Native callers (Swift / Kotlin) never see a
// proto byte buffer; they build / consume the Records.
//
// Wire format is identical to the byte path's — same prost-encoded
// `HistoryBundle` + `SealedBundle` wrappers underneath — so the two
// callers interop on the wire.
// ============================================================================

/// One message in a [`StructuredHistoryGroup`] (Phase 2a3d8-ui).
///
/// `payload_bytes` is the already-decoded MLS `ApplicationPayload`
/// blob — the same bytes the receiver-side
/// `Group::process_incoming` produced when the message first
/// landed. The receiving device decodes it via its native chat
/// envelope parser before projecting into the local message
/// store.
#[derive(uniffi::Record)]
pub struct StructuredHistoryMessage {
    /// TLS-encoded `mls_app::v1::ApplicationPayload` bytes from
    /// the original receive path.
    pub payload_bytes: Vec<u8>,
    /// Sender MLS leaf index at the time of original receipt.
    pub sender_leaf: u32,
    /// Local store's per-message id (stable across snapshots).
    /// Used by the receiver for dedup against its own log.
    pub message_id: Vec<u8>,
    /// UNIX millis when the sender first saw the message.
    pub received_at_ms: u64,
}

/// All history rows for a single conversation group.
#[derive(uniffi::Record)]
pub struct StructuredHistoryGroup {
    /// MLS group_id the messages belong to.
    pub group_id: Vec<u8>,
    /// Messages, newest-first. Receiver inserts in reverse so
    /// the local store ends up with oldest-first ordering.
    pub messages: Vec<StructuredHistoryMessage>,
    /// `true` when the list was truncated to a per-group cap.
    /// UI may surface a "load older" affordance later.
    pub count_capped: bool,
}

/// Top-level result of [`open_history_bundle_structured`].
#[derive(uniffi::Record)]
pub struct StructuredHistoryBundle {
    pub groups: Vec<StructuredHistoryGroup>,
    /// HLC of the most recent message included in the bundle.
    pub max_hlc: u64,
}

/// AEAD-seal a structured history bundle (Phase 2a3d8-ui).
///
/// Same wire format as [`seal_history_bundle`] — the function
/// internally builds a `HistoryBundle` proto from the supplied
/// Records and runs the byte path's seal logic.
///
/// `secret` MUST be 32 bytes (typically the output of
/// [`DeviceGroup::history_bundle_secret`]).
#[uniffi::export]
pub fn seal_history_bundle_structured(
    secret: Vec<u8>,
    groups: Vec<StructuredHistoryGroup>,
    max_hlc: u64,
) -> Result<Vec<u8>, FfiError> {
    if secret.len() != 32 {
        return Err(FfiError::Codec(
            "history-bundle secret must be 32 bytes".into(),
        ));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&secret);

    let bundle = build_history_bundle_proto(groups, max_hlc);
    let sealed = lakko_mls::history_bundle::seal_bundle(&key, &bundle)?;
    Ok(sealed)
}

/// Inverse of [`seal_history_bundle_structured`]. Returns the
/// structured projection — no proto runtime needed on the
/// caller side.
#[uniffi::export]
pub fn open_history_bundle_structured(
    secret: Vec<u8>,
    sealed_bytes: Vec<u8>,
) -> Result<StructuredHistoryBundle, FfiError> {
    if secret.len() != 32 {
        return Err(FfiError::Codec(
            "history-bundle secret must be 32 bytes".into(),
        ));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&secret);

    let bundle = lakko_mls::history_bundle::open_bundle(&key, &sealed_bytes)?;
    Ok(StructuredHistoryBundle {
        max_hlc: bundle.max_hlc,
        groups: bundle
            .groups
            .into_iter()
            .map(|g| StructuredHistoryGroup {
                group_id: g.group_id,
                count_capped: g.count_capped,
                messages: g
                    .messages
                    .into_iter()
                    .map(|m| StructuredHistoryMessage {
                        payload_bytes: m.payload_bytes,
                        sender_leaf: m.sender_leaf,
                        message_id: m.message_id,
                        received_at_ms: m.received_at_ms,
                    })
                    .collect(),
            })
            .collect(),
    })
}

fn build_history_bundle_proto(
    groups: Vec<StructuredHistoryGroup>,
    max_hlc: u64,
) -> lakko_proto::mls_app::history_bundle::v1::HistoryBundle {
    use lakko_proto::mls_app::history_bundle::v1::{HistoryBundle, HistoryGroup, HistoryMessage};
    HistoryBundle {
        max_hlc,
        groups: groups
            .into_iter()
            .map(|g| HistoryGroup {
                group_id: g.group_id,
                count_capped: g.count_capped,
                messages: g
                    .messages
                    .into_iter()
                    .map(|m| HistoryMessage {
                        payload_bytes: m.payload_bytes,
                        sender_leaf: m.sender_leaf,
                        message_id: m.message_id,
                        received_at_ms: m.received_at_ms,
                    })
                    .collect(),
            })
            .collect(),
    }
}

// ============================================================================
// History-bundle per-message placeholder codec (G-6)
//
// Canonical cross-platform encode/decode for the
// `StructuredHistoryMessage.payload_bytes` display projection.
// Web / iOS / Android all call these so the bytes agree on the
// wire (previously each platform JSON-encoded its own
// `PersistedMessage` shape, which did NOT interoperate web <->
// native — see lakko_mls::history_placeholder + known-gaps G-6).
// ============================================================================

/// Message direction relative to the local account.
#[derive(uniffi::Enum)]
pub enum HistoryMessageDirection {
    /// Sent by this account.
    Outgoing,
    /// Received from a peer.
    Incoming,
    /// System / control message.
    System,
}

impl From<HistoryMessageDirection> for lakko_mls::history_placeholder::HistoryDirection {
    fn from(d: HistoryMessageDirection) -> Self {
        match d {
            HistoryMessageDirection::Outgoing => Self::Outgoing,
            HistoryMessageDirection::Incoming => Self::Incoming,
            HistoryMessageDirection::System => Self::System,
        }
    }
}

impl From<lakko_mls::history_placeholder::HistoryDirection> for HistoryMessageDirection {
    fn from(d: lakko_mls::history_placeholder::HistoryDirection) -> Self {
        use lakko_mls::history_placeholder::HistoryDirection as D;
        match d {
            D::Outgoing => Self::Outgoing,
            D::Incoming => Self::Incoming,
            D::System => Self::System,
        }
    }
}

/// One attachment reference inside a [`HistoryPlaceholder`].
/// `blob_id` / `key` MUST be 32 bytes, `nonce` 12 bytes.
#[derive(uniffi::Record)]
pub struct HistoryAttachment {
    /// 32-byte SHA-256 blob id.
    pub blob_id: Vec<u8>,
    /// 32-byte content-encryption key.
    pub key: Vec<u8>,
    /// 12-byte AEAD nonce.
    pub nonce: Vec<u8>,
    /// Plaintext size in bytes.
    pub size: u64,
    /// Optional MIME type.
    pub mime: Option<String>,
    /// Optional file name.
    pub name: Option<String>,
    /// ADR-0047 media kind (`voice`, `video_note`, `gif`, or unknown).
    #[uniffi(default = None)]
    pub kind: Option<String>,
    /// ADR-0047 playback length in milliseconds.
    #[uniffi(default = None)]
    pub duration_ms: Option<u32>,
    /// ADR-0047 waveform amplitude bytes (at most 128).
    #[uniffi(default = None)]
    pub waveform: Option<Vec<u8>>,
    /// ADR-0047 pixel width.
    #[uniffi(default = None)]
    pub width: Option<u32>,
    /// ADR-0047 pixel height.
    #[uniffi(default = None)]
    pub height: Option<u32>,
    /// ADR-0047 poster frame.
    #[uniffi(default = None)]
    pub thumb: Option<HistoryThumb>,
}

/// A poster-frame blob on a [`HistoryAttachment`] (ADR-0047).
/// `blob_id` / `key` MUST be 32 bytes, `nonce` 12 bytes.
#[derive(uniffi::Record)]
pub struct HistoryThumb {
    /// 32-byte SHA-256 blob id.
    pub blob_id: Vec<u8>,
    /// 32-byte content-encryption key.
    pub key: Vec<u8>,
    /// 12-byte AEAD nonce.
    pub nonce: Vec<u8>,
    /// Plaintext size in bytes.
    pub size: u64,
    /// Optional MIME type.
    #[uniffi(default = None)]
    pub mime: Option<String>,
    /// Pixel width.
    #[uniffi(default = None)]
    pub width: Option<u32>,
    /// Pixel height.
    #[uniffi(default = None)]
    pub height: Option<u32>,
}

/// Apply the ADR-0047 bounds (`lakko_attachments::RawMediaMetadata`) to a
/// history attachment, so a restored message keeps exactly what the live
/// envelope would have kept.
fn bound_history_media(
    mut a: lakko_mls::history_placeholder::HistoryAttachmentPlaceholder,
) -> lakko_mls::history_placeholder::HistoryAttachmentPlaceholder {
    let raw = lakko_attachments::RawMediaMetadata {
        kind: a.kind.take(),
        duration_ms: a.duration_ms,
        waveform: a.waveform.take(),
        width: a.width,
        height: a.height,
        thumb: a
            .thumb
            .as_ref()
            .map(|t| lakko_attachments::RawThumbMetadata {
                size: t.size,
                mime: t.mime.clone(),
                width: t.width,
                height: t.height,
            }),
    }
    .bounded();
    a.kind = raw.kind;
    a.duration_ms = raw.duration_ms;
    a.waveform = raw.waveform;
    a.width = raw.width;
    a.height = raw.height;
    a.thumb = match (a.thumb.take(), raw.thumb) {
        (Some(mut t), Some(bounded)) => {
            t.mime = bounded.mime;
            t.width = bounded.width;
            t.height = bounded.height;
            Some(t)
        },
        _ => None,
    };
    a
}

/// The display projection of one history-bundle message —
/// the structured form of `StructuredHistoryMessage.payload_bytes`.
#[derive(uniffi::Record)]
pub struct HistoryPlaceholder {
    /// HLC-derived message id.
    pub message_id: u64,
    /// Direction relative to the local account.
    pub direction: HistoryMessageDirection,
    /// Display timestamp string as the platform stores it
    /// (ISO-8601 today), carried verbatim.
    pub created_at: String,
    /// Message text.
    pub text: String,
    /// Attachment references.
    pub attachments: Vec<HistoryAttachment>,
}

fn fixed_bytes<const N: usize>(v: &[u8], field: &str) -> Result<[u8; N], FfiError> {
    if v.len() != N {
        return Err(FfiError::Codec(format!(
            "history placeholder: {field} must be {N} bytes, got {}",
            v.len()
        )));
    }
    let mut arr = [0u8; N];
    arr.copy_from_slice(v);
    Ok(arr)
}

/// Encode a [`HistoryPlaceholder`] to the canonical wire bytes
/// for `StructuredHistoryMessage.payload_bytes`.
///
/// # Errors
///
/// [`FfiError::Codec`] on a wrong-length `blob_id` / `key` /
/// `nonce`, or when a field exceeds the codec's hard caps
/// (see `lakko_mls::history_placeholder`).
#[uniffi::export]
pub fn encode_history_placeholder(placeholder: HistoryPlaceholder) -> Result<Vec<u8>, FfiError> {
    use lakko_mls::history_placeholder::{
        HistoryAttachmentPlaceholder, HistoryMessagePlaceholder, HistoryThumbPlaceholder,
    };
    let mut attachments = Vec::with_capacity(placeholder.attachments.len());
    for a in placeholder.attachments {
        let thumb = match a.thumb {
            None => None,
            Some(t) => Some(HistoryThumbPlaceholder {
                blob_id: fixed_bytes::<32>(&t.blob_id, "thumb blob_id")?,
                key: fixed_bytes::<32>(&t.key, "thumb key")?,
                nonce: fixed_bytes::<12>(&t.nonce, "thumb nonce")?,
                size: t.size,
                mime: t.mime,
                width: t.width,
                height: t.height,
            }),
        };
        attachments.push(bound_history_media(HistoryAttachmentPlaceholder {
            blob_id: fixed_bytes::<32>(&a.blob_id, "blob_id")?,
            key: fixed_bytes::<32>(&a.key, "key")?,
            nonce: fixed_bytes::<12>(&a.nonce, "nonce")?,
            size: a.size,
            mime: a.mime,
            name: a.name,
            kind: a.kind,
            duration_ms: a.duration_ms,
            waveform: a.waveform,
            width: a.width,
            height: a.height,
            thumb,
        }));
    }
    let msg = HistoryMessagePlaceholder {
        message_id: placeholder.message_id,
        direction: placeholder.direction.into(),
        created_at: placeholder.created_at,
        text: placeholder.text,
        attachments,
    };
    Ok(msg.encode()?)
}

/// Decode canonical `StructuredHistoryMessage.payload_bytes`
/// produced by [`encode_history_placeholder`] (on any platform)
/// back into a [`HistoryPlaceholder`].
///
/// # Errors
///
/// [`FfiError::Codec`] on a malformed buffer (bad version,
/// truncation, over-cap length, non-UTF-8 string, unknown
/// direction tag, trailing bytes).
#[uniffi::export]
pub fn decode_history_placeholder(bytes: Vec<u8>) -> Result<HistoryPlaceholder, FfiError> {
    let msg = lakko_mls::history_placeholder::HistoryMessagePlaceholder::decode(&bytes)?;
    Ok(HistoryPlaceholder {
        message_id: msg.message_id,
        direction: msg.direction.into(),
        created_at: msg.created_at,
        text: msg.text,
        attachments: msg
            .attachments
            .into_iter()
            .map(bound_history_media)
            .map(|a| HistoryAttachment {
                blob_id: a.blob_id.to_vec(),
                key: a.key.to_vec(),
                nonce: a.nonce.to_vec(),
                size: a.size,
                mime: a.mime,
                name: a.name,
                kind: a.kind,
                duration_ms: a.duration_ms,
                waveform: a.waveform,
                width: a.width,
                height: a.height,
                thumb: a.thumb.map(|t| HistoryThumb {
                    blob_id: t.blob_id.to_vec(),
                    key: t.key.to_vec(),
                    nonce: t.nonce.to_vec(),
                    size: t.size,
                    mime: t.mime,
                    width: t.width,
                    height: t.height,
                }),
            })
            .collect(),
    })
}

#[cfg(test)]
mod history_placeholder_media_tests {
    use super::*;

    fn attachment(kind: &str, duration_ms: u32) -> HistoryAttachment {
        HistoryAttachment {
            blob_id: vec![1; 32],
            key: vec![2; 32],
            nonce: vec![3; 12],
            size: 1000,
            mime: Some("audio/mp4".into()),
            name: Some("voice.m4a".into()),
            kind: Some(kind.into()),
            duration_ms: Some(duration_ms),
            waveform: Some(vec![10, 20, 30]),
            width: Some(0),
            height: None,
            thumb: Some(HistoryThumb {
                blob_id: vec![4; 32],
                key: vec![5; 32],
                nonce: vec![6; 12],
                size: 5000,
                mime: Some("image/jpeg".into()),
                width: Some(240),
                height: Some(99_999),
            }),
        }
    }

    fn round_trip(att: HistoryAttachment) -> HistoryAttachment {
        let bytes = encode_history_placeholder(HistoryPlaceholder {
            message_id: 7,
            direction: HistoryMessageDirection::Incoming,
            created_at: "2026-09-14T12:00:00Z".into(),
            text: String::new(),
            attachments: vec![att],
        })
        .expect("encodes");
        decode_history_placeholder(bytes)
            .expect("decodes")
            .attachments
            .pop()
            .expect("one attachment")
    }

    #[test]
    fn voice_metadata_survives_the_history_bundle() {
        let back = round_trip(attachment("voice", 7_250));
        assert_eq!(back.kind.as_deref(), Some("voice"));
        assert_eq!(back.duration_ms, Some(7_250));
        assert_eq!(back.waveform, Some(vec![10, 20, 30]));
        // Out-of-range values get the envelope's treatment: dropped alone.
        assert_eq!(back.width, None);
        let thumb = back.thumb.expect("thumb kept");
        assert_eq!(
            (thumb.size, thumb.width, thumb.height),
            (5000, Some(240), None)
        );
    }

    #[test]
    fn a_duration_the_kind_does_not_allow_is_dropped() {
        assert_eq!(round_trip(attachment("gif", 1_000)).duration_ms, None);
        assert_eq!(
            round_trip(attachment("video_note", 61_000)).duration_ms,
            None
        );
    }
}

/// Compute the deterministic DeviceGroup `group_id` from an
/// account_id, without building a `DeviceGroup` instance.
/// Useful on the joining-device side when polling the DS
/// journal for catch-up Commits before any Welcome has been
/// processed.
#[uniffi::export]
pub fn device_group_id_for(account_id: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    if account_id.len() != 16 {
        return Err(FfiError::Codec("account_id must be 16 bytes".into()));
    }
    let mut acct = [0u8; 16];
    acct.copy_from_slice(&account_id);
    Ok(MlsDeviceGroup::group_id_for(&acct))
}

/// Load an existing DeviceGroup for `account_id` from
/// `identity`'s MLS provider storage. Returns `null` (on the
/// Kotlin side; `None` on the Rust side) when no row is
/// present.
///
/// Free function rather than `DeviceGroup::load` constructor —
/// UniFFI 0.28 rejects `Option<Self>` in constructor
/// signatures (mirror of the `load_group` shape from the
/// Group surface).
#[uniffi::export]
pub fn load_device_group(
    identity: &Identity,
    account_id: Vec<u8>,
) -> Result<Option<Arc<DeviceGroup>>, FfiError> {
    if account_id.len() != 16 {
        return Err(FfiError::Codec("account_id must be 16 bytes".into()));
    }
    let mut acct = [0u8; 16];
    acct.copy_from_slice(&account_id);
    let loaded = MlsDeviceGroup::load(&identity.inner, &acct)?;
    Ok(loaded.map(|inner| {
        Arc::new(DeviceGroup {
            inner: Mutex::new(inner),
        })
    }))
}

// Manual std::fmt::Debug for ProcessedMessage so we can panic-format it in
// tests without uniffi attempting to derive Debug for FFI types.
impl core::fmt::Debug for ProcessedMessage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Application { data, .. } => write!(f, "Application {{ {} bytes }}", data.len()),
            Self::Commit => write!(f, "Commit"),
            Self::Proposal { record } => write!(
                f,
                "Proposal {{ sender_leaf: {:?}, kind: {:?}, stored: {} }}",
                record.sender_leaf, record.kind, record.stored
            ),
            Self::ExternalJoinProposal { .. } => write!(f, "ExternalJoinProposal"),
        }
    }
}

// ============================================================================
// Phase 3a4 — Call signaling + SFrame keying FFI
// ============================================================================

/// Audio / video request flags on a call offer (Phase 3a3).
#[derive(uniffi::Record)]
pub struct FfiCallMediaFlags {
    pub audio: bool,
    pub video: bool,
}

impl FfiCallMediaFlags {
    fn into_inner(self) -> lakko_calls::MediaFlags {
        lakko_calls::MediaFlags {
            audio: self.audio,
            video: self.video,
        }
    }

    fn from_inner(f: lakko_calls::MediaFlags) -> Self {
        Self {
            audio: f.audio,
            video: f.video,
        }
    }
}

/// Terminal reason carried in
/// [`FfiCallSignalingBody::End`].
#[derive(uniffi::Enum)]
pub enum FfiCallEndReason {
    HungUp,
    Busy,
    Declined,
    TimedOut,
    Error,
}

impl FfiCallEndReason {
    fn into_inner(self) -> lakko_calls::EndReason {
        match self {
            Self::HungUp => lakko_calls::EndReason::HungUp,
            Self::Busy => lakko_calls::EndReason::Busy,
            Self::Declined => lakko_calls::EndReason::Declined,
            Self::TimedOut => lakko_calls::EndReason::TimedOut,
            Self::Error => lakko_calls::EndReason::Error,
        }
    }

    fn from_inner(r: lakko_calls::EndReason) -> Self {
        match r {
            lakko_calls::EndReason::HungUp => Self::HungUp,
            lakko_calls::EndReason::Busy => Self::Busy,
            lakko_calls::EndReason::Declined => Self::Declined,
            lakko_calls::EndReason::TimedOut => Self::TimedOut,
            lakko_calls::EndReason::Error => Self::Error,
        }
    }
}

/// Typed body of a call-signaling event (Phase 3a3 +
/// 3c2). Every variant carries a 16-byte `call_id`.
#[derive(uniffi::Enum)]
pub enum FfiCallSignalingBody {
    /// SDP offer from the initiator (1×1).
    Offer {
        call_id: Vec<u8>,
        sdp: String,
        created_at_ms: u64,
        media: FfiCallMediaFlags,
    },
    /// SDP answer from the callee (1×1).
    Answer { call_id: Vec<u8>, sdp: String },
    /// Trickled ICE candidate (1×1).
    IceCandidate {
        call_id: Vec<u8>,
        candidate: String,
        sdp_mid: String,
        sdp_m_line_index: u32,
    },
    /// Terminal event (1×1 hang-up).
    End {
        call_id: Vec<u8>,
        reason: FfiCallEndReason,
    },
    /// Group-call announce (Phase 3c2). Posted by the
    /// initiator to the chat MLS group; observers render a
    /// joinable-call card.
    GroupStart {
        call_id: Vec<u8>,
        initiator_leaf: u32,
        created_at_ms: u64,
        media: FfiCallMediaFlags,
    },
    /// Group-call participant joined the SFU (Phase 3c2).
    GroupJoin {
        call_id: Vec<u8>,
        leaf_index: u32,
        joined_at_ms: u64,
    },
    /// Group-call participant left the SFU (Phase 3c2).
    GroupLeave {
        call_id: Vec<u8>,
        leaf_index: u32,
        reason: FfiCallEndReason,
    },
}

fn body_into_inner(body: FfiCallSignalingBody) -> Result<lakko_calls::SignalingBody, FfiError> {
    let take_id = |bytes: Vec<u8>| -> Result<lakko_calls::CallId, FfiError> {
        if bytes.len() != 16 {
            return Err(FfiError::Codec(format!(
                "call_id length: expected 16, got {}",
                bytes.len()
            )));
        }
        let mut id = [0u8; 16];
        id.copy_from_slice(&bytes);
        Ok(id)
    };
    Ok(match body {
        FfiCallSignalingBody::Offer {
            call_id,
            sdp,
            created_at_ms,
            media,
        } => lakko_calls::SignalingBody::Offer {
            call_id: take_id(call_id)?,
            sdp,
            created_at_ms,
            media: media.into_inner(),
        },
        FfiCallSignalingBody::Answer { call_id, sdp } => lakko_calls::SignalingBody::Answer {
            call_id: take_id(call_id)?,
            sdp,
        },
        FfiCallSignalingBody::IceCandidate {
            call_id,
            candidate,
            sdp_mid,
            sdp_m_line_index,
        } => lakko_calls::SignalingBody::IceCandidate {
            call_id: take_id(call_id)?,
            candidate,
            sdp_mid,
            sdp_m_line_index,
        },
        FfiCallSignalingBody::End { call_id, reason } => lakko_calls::SignalingBody::End {
            call_id: take_id(call_id)?,
            reason: reason.into_inner(),
        },
        FfiCallSignalingBody::GroupStart {
            call_id,
            initiator_leaf,
            created_at_ms,
            media,
        } => lakko_calls::SignalingBody::GroupStart {
            call_id: take_id(call_id)?,
            initiator_leaf,
            created_at_ms,
            media: media.into_inner(),
        },
        FfiCallSignalingBody::GroupJoin {
            call_id,
            leaf_index,
            joined_at_ms,
        } => lakko_calls::SignalingBody::GroupJoin {
            call_id: take_id(call_id)?,
            leaf_index,
            joined_at_ms,
        },
        FfiCallSignalingBody::GroupLeave {
            call_id,
            leaf_index,
            reason,
        } => lakko_calls::SignalingBody::GroupLeave {
            call_id: take_id(call_id)?,
            leaf_index,
            reason: reason.into_inner(),
        },
    })
}

fn body_from_inner(body: lakko_calls::SignalingBody) -> FfiCallSignalingBody {
    match body {
        lakko_calls::SignalingBody::Offer {
            call_id,
            sdp,
            created_at_ms,
            media,
        } => FfiCallSignalingBody::Offer {
            call_id: call_id.to_vec(),
            sdp,
            created_at_ms,
            media: FfiCallMediaFlags::from_inner(media),
        },
        lakko_calls::SignalingBody::Answer { call_id, sdp } => FfiCallSignalingBody::Answer {
            call_id: call_id.to_vec(),
            sdp,
        },
        lakko_calls::SignalingBody::IceCandidate {
            call_id,
            candidate,
            sdp_mid,
            sdp_m_line_index,
        } => FfiCallSignalingBody::IceCandidate {
            call_id: call_id.to_vec(),
            candidate,
            sdp_mid,
            sdp_m_line_index,
        },
        lakko_calls::SignalingBody::End { call_id, reason } => FfiCallSignalingBody::End {
            call_id: call_id.to_vec(),
            reason: FfiCallEndReason::from_inner(reason),
        },
        lakko_calls::SignalingBody::GroupStart {
            call_id,
            initiator_leaf,
            created_at_ms,
            media,
        } => FfiCallSignalingBody::GroupStart {
            call_id: call_id.to_vec(),
            initiator_leaf,
            created_at_ms,
            media: FfiCallMediaFlags::from_inner(media),
        },
        lakko_calls::SignalingBody::GroupJoin {
            call_id,
            leaf_index,
            joined_at_ms,
        } => FfiCallSignalingBody::GroupJoin {
            call_id: call_id.to_vec(),
            leaf_index,
            joined_at_ms,
        },
        lakko_calls::SignalingBody::GroupLeave {
            call_id,
            leaf_index,
            reason,
        } => FfiCallSignalingBody::GroupLeave {
            call_id: call_id.to_vec(),
            leaf_index,
            reason: FfiCallEndReason::from_inner(reason),
        },
    }
}

impl From<lakko_calls::Error> for FfiError {
    fn from(err: lakko_calls::Error) -> Self {
        match err {
            lakko_calls::Error::Exporter(msg) => FfiError::Exporter(msg),
            lakko_calls::Error::Crypto(msg) => FfiError::Codec(msg),
            lakko_calls::Error::CounterExhausted => {
                FfiError::Codec("sframe counter exhausted".into())
            },
        }
    }
}

/// Compose plaintext call-signaling bytes (Phase 3a3). The
/// caller wraps these in an MLS application message via
/// [`Group::create_application_message`].
#[uniffi::export]
pub fn compose_call_signaling(body: FfiCallSignalingBody) -> Result<Vec<u8>, FfiError> {
    let inner = body_into_inner(body)?;
    Ok(lakko_calls::compose_signaling_payload(&inner)?)
}

/// Inverse of [`compose_call_signaling`]. Decodes the
/// plaintext bytes that fell out of
/// [`ProcessedMessage::Application`].
#[uniffi::export]
pub fn parse_call_signaling(plaintext: Vec<u8>) -> Result<FfiCallSignalingBody, FfiError> {
    let body = lakko_calls::parse_signaling_payload(&plaintext)?;
    Ok(body_from_inner(body))
}

/// Phase 3a4 — derive the per-call SFrame seed from a
/// conversation MLS group's exporter (label
/// `lakko/call/v1`). Both sides reach the same 32 bytes at
/// the same epoch with the same `call_id`.
#[uniffi::export]
pub fn sframe_derive_call_key(
    group: &Group,
    identity: &Identity,
    call_id: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let g = group.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
    let key = lakko_calls::CallKey::derive(&g, &identity.inner, &call_id)?;
    Ok(key.as_bytes().to_vec())
}

/// Per-participant SFrame key + nonce salt derived from the
/// 32-byte call key + sender leaf index. Caller maintains
/// the frame counter on the platform side.
#[derive(uniffi::Record)]
pub struct FfiSframeParticipantKey {
    /// 32-byte AEAD key.
    pub key_bytes: Vec<u8>,
    /// 12-byte nonce salt.
    pub nonce_salt: Vec<u8>,
}

/// Derive a per-participant SFrame key + nonce salt for a given
/// `(sender_leaf, stream_index)` stream (ADR-0035). The seal-side derivation:
/// each of a leaf's concurrent streams (audio, camera, screen-share, …) gets a
/// distinct key + nonce salt keyed on the composite KID
/// `(sender_leaf << 8) | stream_index`, so their independent counters cannot
/// reuse a `(key, nonce)` pair — the guarantee the ADR-0034 per-media-TYPE tag
/// could not give two same-kind streams. `stream_index` is a `u8`, so the full
/// `0..=255` range is valid and cannot overflow the KID's 8-bit stream field.
#[uniffi::export]
pub fn sframe_derive_participant_key(
    call_key_seed: Vec<u8>,
    sender_leaf: u32,
    stream_index: u8,
) -> Result<FfiSframeParticipantKey, FfiError> {
    if call_key_seed.len() != 32 {
        return Err(FfiError::Codec("call_key_seed must be 32 bytes".into()));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&call_key_seed);
    let ck = lakko_calls::CallKey::from_seed(seed);
    let pk = ck.participant_key_for(sender_leaf, u32::from(stream_index))?;
    Ok(FfiSframeParticipantKey {
        key_bytes: pk.key_bytes().to_vec(),
        nonce_salt: pk.nonce_salt().to_vec(),
    })
}

/// Open an SFrame frame with the Phase 3c5 overlap window.
/// Tries `current_call_key_seed` first; on AEAD rejection
/// falls back to `previous_call_key_seed` if present
/// (caller passes `None` outside the 1-second overlap
/// window). Returns the plaintext on success.
///
/// Mirrors [`GroupCallSession::try_open_with_overlap`]; the
/// FFI surface is seed-flat so platform code can hold the
/// 32-byte seeds in its own keychain-backed store rather
/// than ferrying a Rust handle.
///
/// `kid` is the composite SFrame key identifier
/// `(sender_leaf << 8) | stream_index` (ADR-0035) the receiver read from the
/// frame's header — the decryptor never needs a separate media discriminator,
/// it re-derives the exact sender-stream key straight from the KID.
#[uniffi::export]
pub fn sframe_open_with_overlap(
    current_call_key_seed: Vec<u8>,
    previous_call_key_seed: Option<Vec<u8>>,
    kid: u64,
    aad: Vec<u8>,
    ciphertext: Vec<u8>,
    frame_counter: u32,
) -> Result<Vec<u8>, FfiError> {
    let take_seed = |bytes: Vec<u8>| -> Result<[u8; 32], FfiError> {
        if bytes.len() != 32 {
            return Err(FfiError::Codec(format!(
                "call_key_seed must be 32 bytes, got {}",
                bytes.len()
            )));
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&bytes);
        Ok(seed)
    };

    let current_seed = take_seed(current_call_key_seed)?;
    let current = lakko_calls::CallKey::from_seed(current_seed);
    let current_key = current.participant_key(kid)?;
    match current_key.open(&aad, &ciphertext, frame_counter) {
        Ok(pt) => return Ok(pt),
        Err(lakko_calls::Error::Crypto(_)) => {
            // Fall through to the overlap-window path.
        },
        Err(other) => return Err(other.into()),
    }

    if let Some(prev_bytes) = previous_call_key_seed {
        let prev_seed = take_seed(prev_bytes)?;
        let previous = lakko_calls::CallKey::from_seed(prev_seed);
        let prev_key = previous.participant_key(kid)?;
        return prev_key
            .open(&aad, &ciphertext, frame_counter)
            .map_err(Into::into);
    }
    Err(FfiError::Codec(
        "frame failed authentication under current key (no overlap window active)".into(),
    ))
}

// ============================================================================
// Phase 3b4 — Call-admission JWT FFI surface
// ============================================================================
//
// The client side of the call-admission JWT flow lets the
// platform (Swift / Kotlin) parse + self-verify a JWT it
// just received from `POST /call/jwt` (caller drives the
// HTTP; the FFI surface is plain bytes / strings ↔ typed
// records). Crypto + wire shape lives in
// `crates/lakko-call-jwt`.

/// Mirror of `lakko_call_jwt::CallRole` — Record-friendly
/// enum.
#[derive(uniffi::Enum)]
pub enum FfiCallRole {
    /// Regular participant.
    Participant,
    /// Publisher-only (Phase 4 broadcasts).
    Publisher,
    /// Subscriber-only (Phase 4 audio-room audience).
    Subscriber,
}

impl FfiCallRole {
    fn from_inner(r: lakko_call_jwt::CallRole) -> Self {
        match r {
            lakko_call_jwt::CallRole::Participant => Self::Participant,
            lakko_call_jwt::CallRole::Publisher => Self::Publisher,
            lakko_call_jwt::CallRole::Subscriber => Self::Subscriber,
        }
    }
}

/// Parsed JWT claims — typed mirror of
/// `lakko_call_jwt::CallJwtClaims`. Returned by
/// [`parse_call_jwt_unverified`] + [`verify_call_jwt_with_jwks`].
#[derive(uniffi::Record)]
pub struct FfiCallJwtClaims {
    /// Issuer; always `"lakko-auth"` on a well-formed JWT.
    pub iss: String,
    /// Audience; always `"lakko-sfu"`.
    pub aud: String,
    /// account_id, 32-char lower-case hex.
    pub sub: String,
    /// call_id, 32-char lower-case hex.
    pub call_id: String,
    /// Participation role.
    pub role: FfiCallRole,
    /// `iat` — issued-at, unix seconds.
    pub iat: u64,
    /// `exp` — expires-at, unix seconds.
    pub exp: u64,
    /// JWS header `kid` — 16 hex chars. Useful for client
    /// dashboards / debug logs.
    pub kid: String,
}

fn claims_to_ffi(claims: lakko_call_jwt::CallJwtClaims, kid: String) -> FfiCallJwtClaims {
    FfiCallJwtClaims {
        iss: claims.iss,
        aud: claims.aud,
        sub: claims.sub,
        call_id: claims.call_id,
        role: FfiCallRole::from_inner(claims.role),
        iat: claims.iat,
        exp: claims.exp,
        kid,
    }
}

impl From<lakko_call_jwt::JwtError> for FfiError {
    fn from(err: lakko_call_jwt::JwtError) -> Self {
        match err {
            lakko_call_jwt::JwtError::Format(msg) => FfiError::Codec(format!("jwt format: {msg}")),
            lakko_call_jwt::JwtError::Signature => FfiError::Codec("jwt signature".into()),
            lakko_call_jwt::JwtError::Expired => FfiError::Codec("jwt expired".into()),
            lakko_call_jwt::JwtError::Encode(msg) => FfiError::Codec(format!("jwt encode: {msg}")),
        }
    }
}

/// Parse a JWT into typed claims WITHOUT verifying the
/// signature. Useful for inspecting role / expiry / kid
/// client-side before presenting the JWT to the SFU.
///
/// **Do NOT** trust the returned claims for any
/// authorisation decision — use [`verify_call_jwt_with_jwks`]
/// for the verifying path.
#[uniffi::export]
pub fn parse_call_jwt_unverified(jwt: String) -> Result<FfiCallJwtClaims, FfiError> {
    let (header, claims) = lakko_call_jwt::parse_unverified(&jwt)?;
    Ok(claims_to_ffi(claims, header.kid))
}

/// Verify a JWT against a JWKS document the client fetched
/// from `GET /.well-known/jwks.json` (typically cached
/// for 24 h). Returns the typed claims on success.
///
/// `now_unix_secs` is injected by the caller so the
/// platform controls the clock source — pass
/// `Date.now() / 1000` on web, `Date().timeIntervalSince1970`
/// on iOS, `System.currentTimeMillis() / 1000` on Android.
///
/// # Errors
///
/// Surfaces every shape failure (JWS format, JWKS no-match,
/// signature, expired, JSON decode) as [`FfiError::Codec`]
/// with a descriptive prefix.
#[uniffi::export]
pub fn verify_call_jwt_with_jwks(
    jwks_doc_bytes: Vec<u8>,
    jwt: String,
    now_unix_secs: u64,
) -> Result<FfiCallJwtClaims, FfiError> {
    let jwks = lakko_call_jwt::parse_jwks(&jwks_doc_bytes)?;
    let claims = lakko_call_jwt::verify_with_jwks(&jwks, &jwt, now_unix_secs)?;
    // Re-derive kid from the JWT header for the FFI shape.
    let (header, _) = lakko_call_jwt::parse_unverified(&jwt)?;
    Ok(claims_to_ffi(claims, header.kid))
}

// ============================================================================
// Phase 3a11 — Call quality metrics FFI
// ============================================================================
//
// MOS estimation + adaptive-bitrate recommendation surfaced
// as plain Records / free functions. Platform layer
// samples `RTCStatsReport` on its own cadence and feeds
// the values in.

/// Audio codec hint for the loss-impairment curve. Mirror
/// of `lakko_calls::AudioCodec`.
#[derive(uniffi::Enum)]
pub enum FfiAudioCodec {
    /// Opus (RFC 6716) — FEC-capable; the default for
    /// Lakko's voice / video calls.
    Opus,
    /// G.711 / ISAC / AAC-LC / any non-Opus. No-FEC
    /// baseline for the impairment math.
    Other,
}

impl FfiAudioCodec {
    fn into_inner(self) -> lakko_calls::AudioCodec {
        match self {
            Self::Opus => lakko_calls::AudioCodec::Opus,
            Self::Other => lakko_calls::AudioCodec::Other,
        }
    }
}

/// Discretised quality bucket. Mirror of
/// `lakko_calls::QualityLevel`.
#[derive(uniffi::Enum)]
pub enum FfiQualityLevel {
    /// MOS ≥ 4.3 — toll-quality.
    Excellent,
    /// 4.0 ≤ MOS < 4.3.
    Good,
    /// 3.6 ≤ MOS < 4.0.
    Fair,
    /// 3.1 ≤ MOS < 3.6.
    Poor,
    /// MOS < 3.1 — near-unusable.
    Bad,
}

impl FfiQualityLevel {
    fn from_inner(l: lakko_calls::QualityLevel) -> Self {
        match l {
            lakko_calls::QualityLevel::Excellent => Self::Excellent,
            lakko_calls::QualityLevel::Good => Self::Good,
            lakko_calls::QualityLevel::Fair => Self::Fair,
            lakko_calls::QualityLevel::Poor => Self::Poor,
            lakko_calls::QualityLevel::Bad => Self::Bad,
        }
    }
}

/// Single network sample. Caller constructs one per
/// `RTCStatsReport` poll.
#[derive(uniffi::Record)]
pub struct FfiQualitySample {
    /// Round-trip time in milliseconds.
    pub rtt_ms: u32,
    /// Inter-arrival jitter in milliseconds.
    pub jitter_ms: u32,
    /// Packet-loss fraction in `[0.0, 100.0]`.
    pub loss_pct: f32,
    /// Codec hint.
    pub codec: FfiAudioCodec,
}

/// Output of [`estimate_call_quality`]. Carries both raw
/// `r_factor` / `mos` and the discretised bucket.
#[derive(uniffi::Record)]
pub struct FfiQualityScore {
    /// E-model R-factor in `[0.0, 100.0]`.
    pub r_factor: f32,
    /// Mean Opinion Score in `[1.0, 4.5]`.
    pub mos: f32,
    /// Discretised bucket.
    pub level: FfiQualityLevel,
}

/// Estimate call quality from a single network sample.
#[uniffi::export]
pub fn estimate_call_quality(sample: FfiQualitySample) -> FfiQualityScore {
    let inner = lakko_calls::QualitySample {
        rtt_ms: sample.rtt_ms,
        jitter_ms: sample.jitter_ms,
        loss_pct: sample.loss_pct,
        codec: sample.codec.into_inner(),
    };
    let score = lakko_calls::estimate_quality(inner);
    FfiQualityScore {
        r_factor: score.r_factor,
        mos: score.mos,
        level: FfiQualityLevel::from_inner(score.level),
    }
}

/// Recommend a new encoder bitrate. Returns the current
/// bitrate unchanged when the sample falls in the "hold"
/// band.
#[uniffi::export]
pub fn recommend_call_bitrate(current_bps: u32, sample: FfiQualitySample) -> u32 {
    let inner = lakko_calls::QualitySample {
        rtt_ms: sample.rtt_ms,
        jitter_ms: sample.jitter_ms,
        loss_pct: sample.loss_pct,
        codec: sample.codec.into_inner(),
    };
    lakko_calls::recommend_bitrate(current_bps, inner)
}

/// Default ring-buffer size for [`FfiCallQualitySampler`].
#[uniffi::export]
pub fn ffi_default_quality_sample_window() -> u32 {
    u32::try_from(lakko_calls::DEFAULT_SAMPLE_WINDOW).unwrap_or(5)
}

/// Maximum ring-buffer size accepted by
/// [`FfiCallQualitySampler::new`].
#[uniffi::export]
pub fn ffi_max_quality_sample_window() -> u32 {
    u32::try_from(lakko_calls::MAX_SAMPLE_WINDOW).unwrap_or(60)
}

/// Stateful rolling-window smoothed quality sampler
/// (Phase 3a11). Mirror of `lakko_calls::QualitySampler` —
/// one per active call session on the platform side.
///
/// `record` pushes a fresh sample and returns the
/// smoothed [`FfiQualityScore`]; `latest` returns the
/// current smoothed score without pushing; `clear` resets
/// the window (on call end / SFU handoff).
#[derive(uniffi::Object)]
pub struct FfiCallQualitySampler {
    inner: std::sync::Mutex<lakko_calls::QualitySampler>,
}

#[uniffi::export]
impl FfiCallQualitySampler {
    /// Construct a sampler with a custom window size.
    /// Sizes outside `[1, MAX_SAMPLE_WINDOW]` are clamped.
    #[uniffi::constructor]
    pub fn new(window_size: u32) -> Self {
        let bounded = usize::try_from(window_size).unwrap_or(lakko_calls::DEFAULT_SAMPLE_WINDOW);
        Self {
            inner: std::sync::Mutex::new(lakko_calls::QualitySampler::new(bounded)),
        }
    }

    /// Effective window size after clamping.
    pub fn window_size(&self) -> u32 {
        let guard = self.inner.lock().expect("FfiCallQualitySampler poisoned");
        u32::try_from(guard.window_size()).unwrap_or(u32::MAX)
    }

    /// Number of samples currently buffered.
    pub fn count(&self) -> u32 {
        let guard = self.inner.lock().expect("FfiCallQualitySampler poisoned");
        u32::try_from(guard.count()).unwrap_or(u32::MAX)
    }

    /// Drop every buffered sample.
    pub fn clear(&self) {
        let mut guard = self.inner.lock().expect("FfiCallQualitySampler poisoned");
        guard.clear();
    }

    /// Push a sample and return the smoothed score.
    pub fn record(&self, sample: FfiQualitySample) -> FfiQualityScore {
        let inner_sample = lakko_calls::QualitySample {
            rtt_ms: sample.rtt_ms,
            jitter_ms: sample.jitter_ms,
            loss_pct: sample.loss_pct,
            codec: sample.codec.into_inner(),
        };
        let mut guard = self.inner.lock().expect("FfiCallQualitySampler poisoned");
        let score = guard.record(inner_sample);
        FfiQualityScore {
            r_factor: score.r_factor,
            mos: score.mos,
            level: FfiQualityLevel::from_inner(score.level),
        }
    }

    /// Latest smoothed score without pushing a new sample.
    /// Returns `None` when the sampler is empty.
    pub fn latest(&self) -> Option<FfiQualityScore> {
        let guard = self.inner.lock().expect("FfiCallQualitySampler poisoned");
        guard.latest().map(|score| FfiQualityScore {
            r_factor: score.r_factor,
            mos: score.mos,
            level: FfiQualityLevel::from_inner(score.level),
        })
    }
}

// ============================================================================
// Phase 3c3-joiner — link-based call invite acceptance FFI
// ============================================================================
//
// Wraps `lakko_calls::accept_call_invite` for Swift /
// Kotlin. The Rust shape returns an owned MLS group + a
// GroupCallSession + Welcome bytes; the FFI shape returns a
// uniffi::Object wrapping the MLS group + flat fields for
// the rest. The call-key seed is surfaced flat so the
// platform can feed it into the existing
// `sframe_derive_participant_key` FFI fn without holding a
// GroupCallSession handle.

/// Result of [`accept_call_invite`]. The owned `Group`
/// handle is the freshly-created MLS group with the
/// joiner as founder and the owner already added; the
/// platform uses it for subsequent MLS ops via the
/// existing `Group` methods. `welcome_for_owner` is the
/// MLS wire bytes the joiner must deliver to the owner
/// over their existing transport (the stranger-inbox
/// routed by `capability.owner_account_id`, typically).
#[derive(uniffi::Object)]
pub struct AcceptedCallInvite {
    /// Fresh MLS group (joiner at leaf 0, owner at leaf 1).
    group: Arc<Group>,
    /// MLS wire-format Welcome bytes for the owner to
    /// run `Group::join_from_welcome` against.
    welcome_for_owner: Vec<u8>,
    /// 16-byte `call_id` extracted from the capability.
    call_id: Vec<u8>,
    /// Joiner's leaf index in the new MLS group — always
    /// 0 for a single-joiner accept per the 3c3-joiner
    /// V1 contract, but exposed for forward-compat.
    own_leaf_index: u32,
    /// 32-byte `CallKey` seed derived via the MLS
    /// exporter against `call_id`. Feed this into the
    /// existing `sframe_derive_participant_key`
    /// free function to get the per-leaf SFrame key.
    call_key_seed: Vec<u8>,
}

#[uniffi::export]
impl AcceptedCallInvite {
    /// Freshly-created MLS group (joiner is founder at
    /// leaf 0). Platform uses this for subsequent MLS
    /// ops + the existing `sframe_derive_call_key` /
    /// `sframe_derive_participant_key` free fns.
    pub fn group(&self) -> Arc<Group> {
        self.group.clone()
    }

    /// MLS wire-format Welcome bytes for the owner.
    /// Platform must deliver these out-of-band.
    pub fn welcome_for_owner(&self) -> Vec<u8> {
        self.welcome_for_owner.clone()
    }

    /// 16-byte `call_id` from the capability.
    pub fn call_id(&self) -> Vec<u8> {
        self.call_id.clone()
    }

    /// Joiner's leaf index (0 for the founder).
    pub fn own_leaf_index(&self) -> u32 {
        self.own_leaf_index
    }

    /// 32-byte `CallKey` seed.
    pub fn call_key_seed(&self) -> Vec<u8> {
        self.call_key_seed.clone()
    }
}

/// Accept a verified call-invite capability. Mirrors
/// [`lakko_calls::accept_call_invite`].
///
/// `capability` must already be `decode_and_verify`'d
/// (use the [`contacts::CallInviteCapability`] constructor).
/// The returned object holds a fresh MLS group + the
/// Welcome bytes the joiner ships to the owner.
///
/// # Errors
///
/// - [`FfiError::Codec`] when the embedded KP is
///   malformed.
/// - [`FfiError::Exporter`] when the call-key derivation
///   fails (practically unreachable).
#[uniffi::export]
pub fn accept_call_invite(
    capability: &contacts::CallInviteCapability,
    joiner_identity: &Identity,
) -> Result<Arc<AcceptedCallInvite>, FfiError> {
    let accepted = lakko_calls::accept_call_invite(&capability.inner, &joiner_identity.inner)?;
    let call_key_seed = accepted.session.call_key().as_bytes().to_vec();
    let call_id = accepted.session.call_id.to_vec();
    let own_leaf_index = accepted.session.own_leaf_index;
    let group = Group {
        inner: Mutex::new(accepted.group),
    };
    Ok(Arc::new(AcceptedCallInvite {
        group: Arc::new(group),
        welcome_for_owner: accepted.welcome_for_owner,
        call_id,
        own_leaf_index,
        call_key_seed,
    }))
}

// ---------------------------------------------------------------------------
// Identity supersession proof (G-4, ADR-0007 §6).
//
// On rotation (e.g. recover-from-words minting a fresh account identity) the
// OLD identity signs a proof authorising the NEW identity as its successor.
// Peers verify it against the old key they already pin and rotate their pin to
// the new key — bridging trust WITHOUT a blind TOFU re-accept (security
// invariant #6: old keys never silently come back).
//
// This is the proof primitive only. The KT-log chain verifier
// (`verify_supersession_chain`) ties a proof to an operator-signed Merkle
// inclusion; it consumes log types that have no FFI wire format yet and lands
// with the transparency service.
// ---------------------------------------------------------------------------

/// Build the [`lakko_crypto::signing::SigningKey`] for an identity from its
/// 32-byte Ed25519 seed (the bytes `Identity::ed25519_seed()` returns).
fn signing_key_from_seed(seed: &[u8]) -> Result<lakko_crypto::signing::SigningKey, FfiError> {
    let arr: [u8; 32] = seed
        .try_into()
        .map_err(|_| FfiError::Identity("ed25519 seed must be 32 bytes".to_string()))?;
    Ok(lakko_crypto::signing::SigningKey::from_seed(&arr))
}

/// Sign an identity supersession proof (G-4): the OLD identity authorises the
/// NEW identity as its successor, valid in `[issued_at, expires_at]` (the
/// verifier additionally caps the window at 30 days). Pass each identity's
/// `ed25519_seed()`; both public keys are derived internally so the proof's
/// embedded `old_pubkey` always matches the signing key. Returns the wire-form
/// proof to publish alongside the rotation.
///
/// # Errors
///
/// [`FfiError::Identity`] if either seed is not exactly 32 bytes.
#[uniffi::export]
pub fn sign_supersession_proof(
    old_ed25519_seed: Vec<u8>,
    new_ed25519_seed: Vec<u8>,
    issued_at_unix_secs: i64,
    expires_at_unix_secs: i64,
) -> Result<Vec<u8>, FfiError> {
    let old_sk = signing_key_from_seed(&old_ed25519_seed)?;
    let new_sk = signing_key_from_seed(&new_ed25519_seed)?;
    let proof = lakko_transparency::SupersessionProof::new(
        old_sk.verifying_key().to_bytes(),
        new_sk.verifying_key().to_bytes(),
        issued_at_unix_secs,
        expires_at_unix_secs,
    );
    Ok(proof.sign(&old_sk).to_vec())
}

/// Verify an identity supersession proof (G-4) against the old key the caller
/// already pins for this account. Returns the authorised NEW public key
/// (32 bytes) the caller should re-pin on success.
///
/// # Errors
///
/// [`FfiError::Identity`] if `trusted_old_pubkey` is not 32 bytes, or the proof
/// is malformed / not signed by the trusted old key / carries an out-of-range
/// (>30-day) window / has expired relative to `now_unix_secs`.
#[uniffi::export]
pub fn verify_supersession(
    proof_bytes: Vec<u8>,
    trusted_old_pubkey: Vec<u8>,
    now_unix_secs: i64,
) -> Result<Vec<u8>, FfiError> {
    let old: [u8; 32] = trusted_old_pubkey
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Identity("trusted old pubkey must be 32 bytes".to_string()))?;
    let new = lakko_transparency::verify_supersession(&proof_bytes, &old, now_unix_secs)
        .map_err(|e| FfiError::Identity(format!("supersession verify: {e}")))?;
    Ok(new.to_vec())
}

/// Verify a complete identity-rotation chain against the KT log (G-30 #2,
/// ADR-0019). Unlike [`verify_supersession`] — which checks only the bundle's
/// standalone proof against the local pin — this anchors the rotation to the
/// operator-signed, append-only log: a rotation is bound to a SINGLE committed
/// version under a no-rollback root, which makes a leaked-old-key equivocation
/// DETECTABLE (two distinct successors cannot both be log-confirmed at one
/// version; the second peer to fetch sees a version-floor or inclusion failure).
///
/// The decomposed fields mirror the JSON a client gathers from
/// `/transparency/{root,latest,proof}` plus the `supersession_proof` carried in
/// the peer's identity bundle and what the peer currently pins. On success the
/// caller re-pins to the returned NEW public key (32 bytes) at `entry_version`.
///
/// `last_seen_log_size` is the client's persisted monotone high-water mark; the
/// chain's no-rollback check reads only the size, so the stale `SignedRoot`'s
/// other fields are reconstructed as zero (never inspected).
///
/// # Errors
///
/// [`FfiError::Identity`] if any fixed-length field is the wrong length
/// (`*_tree_root` / `entry_username_hash` / `pinned_old_pubkey` != 32,
/// `root_signature` != 64, `audit_path` not a multiple of 32), `operator_pubkey`
/// is not a valid Ed25519 key, or any chain gate fails (forged root, rollback,
/// inclusion mismatch, wrong account, non-newer version, or a supersession proof
/// that is absent / not signed by the pinned old key / out-of-window / expired /
/// not authorising exactly the entry's new key).
#[uniffi::export]
#[allow(clippy::too_many_arguments)]
pub fn verify_supersession_chain(
    // The operator key the client pinned out-of-band.
    operator_pubkey: Vec<u8>,
    // The newly-presented signed root (from `/transparency/root`).
    new_tree_root: Vec<u8>,
    new_log_size: u64,
    new_timestamp_unix: u64,
    new_root_signature: Vec<u8>,
    // The new log entry (from `/transparency/latest/:account_key`). The
    // `entry_username_hash` is the account_key the client queried with and is
    // used for both the entry and the pin (the inclusion proof is what binds it
    // to the root).
    entry_username_hash: Vec<u8>,
    entry_identity_pubkey: Vec<u8>,
    entry_version: u64,
    // Its Merkle inclusion proof (from `/transparency/proof/:idx`).
    inclusion_leaf_index: u64,
    inclusion_audit_path: Vec<u8>,
    // The supersession proof authorising the transition (from the peer's
    // identity bundle).
    supersession_proof: Vec<u8>,
    // What the peer currently pins.
    pinned_old_pubkey: Vec<u8>,
    pinned_version: u64,
    last_seen_log_size: u64,
    now_unix_secs: i64,
) -> Result<Vec<u8>, FfiError> {
    let operator =
        lakko_crypto::signing::VerifyingKey::from_bytes(&operator_pubkey).map_err(|_| {
            FfiError::Identity("operator pubkey is not a valid Ed25519 key".to_string())
        })?;
    let new_tree_root: lakko_transparency::TreeHash = new_tree_root
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Identity("new_tree_root must be 32 bytes".to_string()))?;
    let new_root_signature = lakko_crypto::signing::Signature::from_bytes(&new_root_signature)
        .map_err(|_| FfiError::Identity("root_signature must be 64 bytes".to_string()))?;
    let username_hash: [u8; 32] = entry_username_hash
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Identity("entry_username_hash must be 32 bytes".to_string()))?;
    let pinned_old: [u8; 32] = pinned_old_pubkey
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Identity("pinned_old_pubkey must be 32 bytes".to_string()))?;

    if inclusion_audit_path.len() % 32 != 0 {
        return Err(FfiError::Identity(
            "audit_path length must be a multiple of 32 bytes".to_string(),
        ));
    }
    let audit_path: Vec<lakko_transparency::TreeHash> = inclusion_audit_path
        .chunks_exact(32)
        .map(|c| {
            let mut h = [0u8; 32];
            h.copy_from_slice(c);
            h
        })
        .collect();

    let new_root = lakko_transparency::SignedRoot {
        tree_root: new_tree_root,
        log_size: new_log_size,
        timestamp_unix: new_timestamp_unix,
        signature: new_root_signature,
    };
    let new_entry = lakko_transparency::LogEntry {
        username_hash,
        identity_pubkey: entry_identity_pubkey,
        version: entry_version,
    };
    let inclusion = lakko_transparency::InclusionProof {
        leaf_index: inclusion_leaf_index,
        audit_path,
    };
    // The chain's `check_no_rollback` reads only `log_size`; reconstruct the
    // stale root from the client's high-water mark with zeroed other fields
    // (never inspected). A zero signature parses fine — it is never verified.
    let last_seen_root = lakko_transparency::SignedRoot {
        tree_root: [0u8; 32],
        log_size: last_seen_log_size,
        timestamp_unix: 0,
        signature: lakko_crypto::signing::Signature::from_bytes(&[0u8; 64])
            .map_err(|_| FfiError::Identity("zero signature parse".to_string()))?,
    };

    let pinned = lakko_transparency::PinnedIdentity {
        username_hash: &username_hash,
        old_pubkey: &pinned_old,
        version: pinned_version,
        last_seen_root: &last_seen_root,
    };
    let evidence = lakko_transparency::RotationEvidence {
        operator: &operator,
        new_root: &new_root,
        new_entry: &new_entry,
        inclusion: &inclusion,
        supersession_proof: &supersession_proof,
    };

    let rotated = lakko_transparency::verify_supersession_chain(&pinned, &evidence, now_unix_secs)
        .map_err(|e| FfiError::Identity(format!("supersession chain verify: {e}")))?;
    Ok(rotated.new_pubkey.to_vec())
}

/// Sign a genesis / fresh-start KT-log leaf's account-to-key binding with the
/// leaf's OWN identity key (G-30 #2, ADR-0019). Used for the first-ever publish
/// and a lost-everything fresh-start mint (a new key with NO supersession
/// proof). Pass the identity's `ed25519_seed()`; the leaf's pubkey is derived
/// from it internally so the self-signature always matches the embedded key.
/// The signature is version-INDEPENDENT (the version is operator-assigned and
/// unknown to a fresh-start minter). Returns the 64-byte signature to carry as
/// the leaf's `leaf_auth`.
///
/// # Errors
///
/// [`FfiError::Identity`] if the seed is not 32 bytes or `username_hash` is not
/// 32 bytes.
#[uniffi::export]
pub fn sign_leaf_genesis(
    ed25519_seed: Vec<u8>,
    username_hash: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let sk = signing_key_from_seed(&ed25519_seed)?;
    let username_hash: [u8; 32] = username_hash
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Identity("username_hash must be 32 bytes".to_string()))?;
    let pubkey = sk.verifying_key().to_bytes();
    Ok(lakko_transparency::sign_leaf_genesis(&sk, &username_hash, &pubkey).to_vec())
}

/// Verify a genesis self-signature against the embedded `identity_pubkey`
/// (G-30 #2, ADR-0019). A pass proves the holder of `identity_pubkey` authorised
/// this `(username_hash, pubkey)` account-to-key binding.
///
/// # Errors
///
/// [`FfiError::Identity`] if `username_hash` is not 32 bytes, the embedded
/// `identity_pubkey` is not a valid Ed25519 key, `sig` is not 64 bytes, or the
/// signature does not verify.
#[uniffi::export]
pub fn verify_leaf_genesis(
    sig: Vec<u8>,
    username_hash: Vec<u8>,
    identity_pubkey: Vec<u8>,
) -> Result<(), FfiError> {
    let username_hash: [u8; 32] = username_hash
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Identity("username_hash must be 32 bytes".to_string()))?;
    lakko_transparency::verify_leaf_genesis(&sig, &username_hash, &identity_pubkey)
        .map_err(|e| FfiError::Identity(format!("leaf genesis verify: {e}")))
}

/// Verify a witness co-signature (ADR-0020) over a transparency root's
/// `(tree_root, log_size)` against the CLIENT-PINNED `witness_key`. A client
/// requires this AND the operator's own root signature before trusting a root —
/// the defence against an operator split-view (G-30 #1). The `cosig` bytes are
/// the self-describing wire blob the operator serves alongside the root; this
/// confirms the blob is bound to exactly `(expected_tree_root,
/// expected_log_size)` and was signed by the pinned witness.
///
/// # Errors
///
/// [`FfiError::Identity`] if `expected_tree_root` is not 32 bytes, the co-sig is
/// the wrong length / version, its embedded root differs from the expected one,
/// or the signature does not verify under `witness_key`.
#[uniffi::export]
pub fn verify_witness_cosig(
    cosig: Vec<u8>,
    witness_key: Vec<u8>,
    expected_tree_root: Vec<u8>,
    expected_log_size: u64,
) -> Result<(), FfiError> {
    let expected_tree_root: [u8; 32] = expected_tree_root
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Identity("expected_tree_root must be 32 bytes".to_string()))?;
    lakko_transparency::verify_witness_cosig(
        &cosig,
        &witness_key,
        &expected_tree_root,
        expected_log_size,
    )
    .map_err(|e| FfiError::Identity(format!("witness co-sig verify: {e}")))
}

/// Verify a K-of-N witness QUORUM (ADR-0039, G-30 finding-1) over a transparency
/// root's `(tree_root, log_size)`: at least `threshold` DISTINCT client-pinned
/// witnesses must each have co-signed the SAME root. This is the K-of-N
/// generalisation of [`verify_witness_cosig`] — the client pins `N` witness keys
/// (`pinned_keys`) and requires `threshold` (`K`) of them, so an operator can no
/// longer fork the log by colluding with a single witness (it must corrupt `K`).
///
/// The tally counts DISTINCT pinned keys, never co-signatures: each pinned key
/// contributes at most one to the quorum no matter how many supplied `cosigs`
/// verify under it, and a duplicated pinned key is counted once. Since `cosigs`
/// is attacker-controlled (a malicious operator serves them), this defeats
/// replaying one witness's co-signature `K` times. A `cosigs` entry bound to a
/// different root, signed by a non-pinned key, or malformed simply does not count
/// (it is never fatal), reusing every [`verify_witness_cosig`] gate.
///
/// A `threshold` of `0` is a misconfiguration and is rejected fail-closed before
/// any co-signature is examined, so a config bug can never silently disable the
/// split-view defence (invariant #4).
///
/// # Errors
///
/// [`FfiError::Identity`] if `expected_tree_root` is not 32 bytes, `threshold` is
/// `0`, or fewer than `threshold` distinct pinned keys produced a valid
/// co-signature over the expected root.
#[uniffi::export]
pub fn verify_witness_quorum(
    cosigs: Vec<Vec<u8>>,
    pinned_keys: Vec<Vec<u8>>,
    threshold: u32,
    expected_tree_root: Vec<u8>,
    expected_log_size: u64,
) -> Result<(), FfiError> {
    let expected_tree_root: [u8; 32] = expected_tree_root
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Identity("expected_tree_root must be 32 bytes".to_string()))?;
    // Borrow each owned buffer as a slice; `verify_witness_quorum` re-verifies
    // each cosig's length / version / binding, so no length pre-check is needed
    // here (a malformed entry is ignored, never fatal).
    let cosig_refs: Vec<&[u8]> = cosigs.iter().map(Vec::as_slice).collect();
    let key_refs: Vec<&[u8]> = pinned_keys.iter().map(Vec::as_slice).collect();
    lakko_transparency::verify_witness_quorum(
        &cosig_refs,
        &key_refs,
        threshold as usize,
        &expected_tree_root,
        expected_log_size,
    )
    .map_err(|e| FfiError::Identity(format!("witness quorum verify: {e}")))
}

/// Verify an auth-attested genesis attestation's full account / username_hash /
/// pubkey binding against the CLIENT-PINNED auth attest verifying key (ADR-0036,
/// closes the G-30 finding-2 squatting residual). A pass proves the party that
/// OPAQUE-authenticated as `expected_account_id` presented `expected_pubkey` as
/// its genesis identity key (bridged to the KT-log leaf by
/// `expected_username_hash`) — so only the account holder could have minted it.
///
/// This is the CLIENT first-observation path: it checks the signature plus all
/// four bindings but deliberately does NOT enforce freshness (the attestation is
/// a permanent historical artifact; the transparency append gate is the side that
/// bounds the mint→append window). Verify beside the ADR-0019 self-signature.
///
/// # Errors
///
/// [`FfiError::Identity`] if `attest_verifying_key` is not a valid Ed25519 key,
/// `expected_account_id` is not 16 bytes, `expected_username_hash` /
/// `expected_pubkey` are not 32 bytes, the attestation is malformed / the wrong
/// length or version, the signature does not verify, or any of the account /
/// username_hash / pubkey bindings mismatch.
#[uniffi::export]
pub fn verify_genesis_attestation(
    attestation: Vec<u8>,
    attest_verifying_key: Vec<u8>,
    expected_account_id: Vec<u8>,
    expected_username_hash: Vec<u8>,
    expected_pubkey: Vec<u8>,
) -> Result<(), FfiError> {
    let verifying = lakko_crypto::signing::VerifyingKey::from_bytes(&attest_verifying_key)
        .map_err(|_| {
            FfiError::Identity("attest verifying key is not a valid Ed25519 key".to_string())
        })?;
    let expected_account: [u8; 16] = expected_account_id
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Identity("expected_account_id must be 16 bytes".to_string()))?;
    let expected_username_hash: [u8; 32] = expected_username_hash
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Identity("expected_username_hash must be 32 bytes".to_string()))?;
    let expected_pubkey: [u8; 32] = expected_pubkey
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Identity("expected_pubkey must be 32 bytes".to_string()))?;
    lakko_attest::verify_genesis_binding(
        &attestation,
        &verifying,
        &expected_account,
        &expected_username_hash,
        &expected_pubkey,
    )
    .map(|_| ())
    .map_err(|e| FfiError::Identity(format!("genesis attestation verify: {e}")))
}

/// Derive the transparency `username_hash` for an `account_id`
/// (`BLAKE3("lakko/transparency/account/v1" ‖ account_id)`) — the same
/// derivation discovery uses on append. A client computes the
/// `expected_username_hash` it feeds to [`verify_genesis_attestation`] from the
/// target's `account_id` (obtained via the `username → account_id` discovery
/// lookup) without re-implementing BLAKE3. Returns the 32-byte hash.
#[uniffi::export]
#[must_use]
pub fn account_username_hash(account_id: Vec<u8>) -> Vec<u8> {
    lakko_transparency::account_username_hash(&account_id).to_vec()
}

// ---------------------------------------------------------------------------
// Key-transparency inclusion verifier (KT-log S3b).
//
// After fetching `GET /transparency/root` (the operator-signed Merkle root),
// `GET /transparency/latest/:account_key` (the latest entry's
// `{idx, version, identity_pubkey}`) and `GET /transparency/proof/:idx` (the
// audit path), a client calls `verify_log_inclusion` to prove the identity it
// is about to trust is committed by the operator-signed log — NOT just whatever
// the zero-trust server returned. Two gates, BOTH must pass:
//
//   1. `SignedRoot::verify` against the CLIENT-PINNED operator key — a forged
//      root (signed by anyone but the pinned operator) fails here.
//   2. `verify_inclusion` — this exact `(account_key, identity_pubkey, version)`
//      leaf is at `leaf_index` of the tree that root commits to. A different
//      pubkey / version / index / a tampered audit path fails here.
//
// The verdict is bound to the exact tuple the caller cares about, so a malicious
// server cannot substitute a different identity and pass.
//
// The no-rollback (append-only) check stays CLIENT-side: the client compares
// the fetched `log_size` against its last-seen monotone value. This verifier is
// stateless and does not persist or compare any last-seen size.
// ---------------------------------------------------------------------------

/// Verify that `entry_identity_pubkey` (at `entry_version`) for the account-key
/// `entry_username_hash` is committed by the operator-signed transparency log
/// (KT-log S3b). The decomposed fields mirror the JSON a client decodes from
/// `/transparency/{root,latest,proof}`.
///
/// Runs two gates and returns successfully only if BOTH pass: (1) the root is
/// signed by the client-pinned `operator_pubkey`; (2) this exact
/// `(account_key, pubkey, version)` leaf is at `leaf_index` of that root's tree.
///
/// The no-rollback (append-only) check is the CALLER's job: compare the fetched
/// `log_size` against the last-seen monotone value before trusting the result.
/// This function is stateless.
///
/// # Errors
///
/// [`FfiError::Identity`] if any fixed-length field is the wrong length
/// (`tree_root` / `entry_username_hash` != 32, `root_signature` != 64,
/// `audit_path` length not a multiple of 32), `operator_pubkey` is not 1..=64
/// bytes / not a valid Ed25519 key, the root signature does not verify under
/// `operator_pubkey`, or the inclusion proof does not bind the entry to the
/// root.
#[uniffi::export]
#[allow(clippy::too_many_arguments)]
pub fn verify_log_inclusion(
    operator_pubkey: Vec<u8>,
    tree_root: Vec<u8>,
    log_size: u64,
    timestamp_unix: u64,
    root_signature: Vec<u8>,
    leaf_index: u64,
    audit_path: Vec<u8>,
    entry_username_hash: Vec<u8>,
    entry_identity_pubkey: Vec<u8>,
    entry_version: u64,
) -> Result<(), FfiError> {
    // The operator key is client-pinned out-of-band; accept the same 1..=64
    // byte range the log column allows, but it must parse as a valid Ed25519
    // verifying key (32 bytes canonical).
    if operator_pubkey.is_empty() || operator_pubkey.len() > 64 {
        return Err(FfiError::Identity(
            "operator pubkey must be 1..=64 bytes".to_string(),
        ));
    }
    let operator =
        lakko_crypto::signing::VerifyingKey::from_bytes(&operator_pubkey).map_err(|_| {
            FfiError::Identity("operator pubkey is not a valid Ed25519 key".to_string())
        })?;

    let tree_root: lakko_transparency::TreeHash = tree_root
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Identity("tree_root must be 32 bytes".to_string()))?;
    let signature = lakko_crypto::signing::Signature::from_bytes(&root_signature)
        .map_err(|_| FfiError::Identity("root_signature must be 64 bytes".to_string()))?;
    let username_hash: [u8; 32] = entry_username_hash
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Identity("entry_username_hash must be 32 bytes".to_string()))?;

    if audit_path.len() % 32 != 0 {
        return Err(FfiError::Identity(
            "audit_path length must be a multiple of 32 bytes".to_string(),
        ));
    }
    let audit_path: Vec<lakko_transparency::TreeHash> = audit_path
        .chunks_exact(32)
        .map(|c| {
            let mut h = [0u8; 32];
            h.copy_from_slice(c);
            h
        })
        .collect();

    let signed_root = lakko_transparency::SignedRoot {
        tree_root,
        log_size,
        timestamp_unix,
        signature,
    };
    let proof = lakko_transparency::InclusionProof {
        leaf_index,
        audit_path,
    };
    let entry = lakko_transparency::LogEntry {
        username_hash,
        identity_pubkey: entry_identity_pubkey,
        version: entry_version,
    };

    // Gate 1: the root is operator-signed. A forged root fails here.
    signed_root
        .verify(&operator)
        .map_err(|e| FfiError::Identity(format!("signed root verify: {e}")))?;

    // Gate 2: this exact entry is committed by that root.
    lakko_transparency::verify_inclusion(&entry, &proof, &signed_root)
        .map_err(|e| FfiError::Identity(format!("inclusion verify: {e}")))?;

    Ok(())
}

/// Domain separator for the transparency-log account-key. MUST stay
/// byte-identical to the discovery service's `ACCOUNT_KEY_DOMAIN` and the wasm
/// `transparencyAccountKey` so all three derive the same log key.
const TRANSPARENCY_ACCOUNT_KEY_DOMAIN: &str = "lakko/transparency/account/v1";

/// Derive the 32-byte transparency-log account-key for an account id:
/// `BLAKE3(domain || account_id)`. Clients use it to look up + verify a
/// contact's identity in the KT log (KT-log S3). The account id is already
/// public, so the derived key leaks nothing new.
#[uniffi::export]
#[must_use]
pub fn transparency_account_key(account_id: Vec<u8>) -> Vec<u8> {
    let mut input = Vec::with_capacity(TRANSPARENCY_ACCOUNT_KEY_DOMAIN.len() + account_id.len());
    input.extend_from_slice(TRANSPARENCY_ACCOUNT_KEY_DOMAIN.as_bytes());
    input.extend_from_slice(&account_id);
    lakko_crypto::hash::blake3(&input).to_vec()
}

/// ADR-0028 §3 — read a group's current MLS epoch from a wire-form journal
/// message's public framing, without any group secret. The join-ticket joiner
/// (a non-member) calls this on the group journal's latest message to craft an
/// external-join proposal for the right epoch. Returns `None` for non-MLS bytes
/// or an epoch-less variant (Welcome / KeyPackage) — the caller treats that as
/// epoch `0` (a freshly-created group).
#[uniffi::export]
#[must_use]
pub fn epoch_of_message(bytes: Vec<u8>) -> Option<u64> {
    lakko_mls::epoch_of_message(&bytes)
}

/// ADR-0028 §3 — the epoch the group is in AFTER a journal row: the framed
/// epoch, plus one when the row is a Commit (a Commit is framed at the epoch it
/// was created in and moves the group to the next). This, over the journal's
/// last MLS row, is what a joiner puts on its external-join proposal; the
/// framed epoch alone is stale whenever the head is a Commit, and every member
/// then refuses the proposal without telling the joiner. Returns `None`
/// exactly where `epoch_of_message` does.
#[uniffi::export]
#[must_use]
pub fn epoch_after_message(bytes: Vec<u8>) -> Option<u64> {
    lakko_mls::epoch_after_message(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirror of the lakko-mls integration test driven through the FFI
    /// surface. Exercises the same lifecycle one Swift / Kotlin client would.
    #[test]
    fn ffi_three_party_lifecycle() {
        let alice = Identity::new("alice".into()).unwrap();
        let bob = Identity::new("bob".into()).unwrap();
        let charlie = Identity::new("charlie".into()).unwrap();

        let alice_group = Group::create(&alice).unwrap();

        // Alice adds Bob.
        let bob_kp = bob.create_key_package().unwrap();
        let add_bob = alice_group.add_member_from_wire(&alice, bob_kp).unwrap();
        let bob_group = Group::join_from_welcome(add_bob.welcome, &bob).unwrap();

        // Alice → Bob.
        let msg = alice_group
            .create_application_message(&alice, b"hello via ffi".to_vec())
            .unwrap();
        let received = bob_group.process_incoming(&bob, msg).unwrap();
        match received {
            ProcessedMessage::Application {
                data, sender_leaf, ..
            } => {
                assert_eq!(data, b"hello via ffi");
                // ADR-0023 — the sender's leaf is surfaced (Alice's, here).
                assert_eq!(sender_leaf, Some(alice_group.own_leaf_index().unwrap()));
            },
            other => panic!("expected application, got {other:?}"),
        }

        // Alice adds Charlie; Bob processes the commit.
        let charlie_kp = charlie.create_key_package().unwrap();
        let add_charlie = alice_group
            .add_member_from_wire(&alice, charlie_kp)
            .unwrap();
        let bob_post = bob_group
            .process_incoming(&bob, add_charlie.commit)
            .unwrap();
        assert!(matches!(bob_post, ProcessedMessage::Commit));
        let charlie_group = Group::join_from_welcome(add_charlie.welcome, &charlie).unwrap();

        assert_eq!(alice_group.member_count().unwrap(), 3);
        assert_eq!(bob_group.member_count().unwrap(), 3);
        assert_eq!(charlie_group.member_count().unwrap(), 3);

        // Exporter consistency.
        let alice_export = alice_group
            .export_secret(&alice, "lakko/test".into(), b"ctx".to_vec(), 32)
            .unwrap();
        let charlie_export = charlie_group
            .export_secret(&charlie, "lakko/test".into(), b"ctx".to_vec(), 32)
            .unwrap();
        assert_eq!(alice_export, charlie_export);
    }

    // ========================================================================
    // ADR-0045 — proposal + sweep + rank surface
    // ========================================================================

    /// Set up `alice` (owner) + `bob` + `charlie` in one group, returning the
    /// three identities and their three `Group` handles.
    fn adr0045_trio() -> ((Identity, Group), (Identity, Group), (Identity, Group)) {
        let alice = Identity::new("alice".into()).unwrap();
        let bob = Identity::new("bob".into()).unwrap();
        let charlie = Identity::new("charlie".into()).unwrap();

        let alice_g = Group::create(&alice).unwrap();
        let add_bob = alice_g
            .add_member_from_wire(&alice, bob.create_key_package().unwrap())
            .unwrap();
        let bob_g = Group::join_from_welcome(add_bob.welcome, &bob).unwrap();

        let add_charlie = alice_g
            .add_member_from_wire(&alice, charlie.create_key_package().unwrap())
            .unwrap();
        assert!(matches!(
            bob_g
                .process_incoming_verifying_roles(&bob, add_charlie.commit)
                .unwrap(),
            ProcessedMessage::Commit
        ));
        let charlie_g = Group::join_from_welcome(add_charlie.welcome, &charlie).unwrap();

        ((alice, alice_g), (bob, bob_g), (charlie, charlie_g))
    }

    /// ADR-0045 **S7** across the UniFFI boundary: `approveExternalJoin` hands
    /// back a **Proposal**, not a Commit, and the Welcome the joiner needs comes
    /// out of the sweep instead.
    ///
    /// This is the shape every client mirror is built from, so it is asserted
    /// on the FFI types rather than inferred from the core: the return value is
    /// bytes a caller publishes, the epoch does not move, and the joiner opens
    /// the Welcome the *sweep* produced.
    #[test]
    fn approving_an_external_join_yields_a_proposal_and_the_sweep_yields_the_welcome() {
        let ((alice, alice_g), (bob, bob_g), _charlie) = adr0045_trio();
        let dave = Identity::new("dave".into()).unwrap();

        // A stranger asks, against the group's current epoch.
        let request = dave
            .create_external_join_proposal(alice_g.group_id().unwrap(), alice_g.epoch().unwrap())
            .unwrap();
        let seen = alice_g
            .process_incoming_verifying_roles(&alice, request)
            .unwrap();
        let ProcessedMessage::ExternalJoinProposal {
            joiner_signature_key,
            retained,
        } = seen
        else {
            panic!("expected an ExternalJoinProposal, got {seen:?}");
        };
        assert!(retained, "the quarantine had room, so an approve can act");
        assert_eq!(
            alice_g.pending_external_joins().unwrap(),
            vec![joiner_signature_key.clone()],
        );

        // The approve is a proposal: bytes to publish, and no epoch advance.
        let epoch_before = alice_g.epoch().unwrap();
        let approval = alice_g
            .approve_external_join(&alice, joiner_signature_key)
            .unwrap();
        assert!(!approval.is_empty());
        assert_eq!(alice_g.epoch().unwrap(), epoch_before);
        assert_eq!(alice_g.pending_proposals().unwrap().len(), 1);

        // On the wire it is an ordinary admin-authored Add: bob's receipt gate
        // stores it and cannot tell it came from a `/g/` link.
        let received = bob_g
            .process_incoming_verifying_roles(&bob, approval)
            .unwrap();
        let ProcessedMessage::Proposal { record } = received else {
            panic!("expected a Proposal, got {received:?}");
        };
        assert!(record.stored);
        assert!(matches!(record.kind, ProposalKind::Add { .. }));

        // The sweep is what produces the Welcome.
        let sweep = alice_g
            .commit_pending_proposals(&alice)
            .unwrap()
            .expect("a commit was produced");
        assert_eq!(sweep.swept.len(), 1);
        let welcome = sweep
            .welcome
            .expect("a sweep carrying an Add has a Welcome");
        let dave_g = Group::join_from_welcome(welcome, &dave).unwrap();
        assert_eq!(dave_g.epoch().unwrap(), epoch_before + 1);
        assert!(alice_g.pending_external_joins().unwrap().is_empty());
    }

    /// ADR-0045 S7 — a plain member's approve emits nothing at all, rather than
    /// a proposal its peers happen to drop.
    #[test]
    fn a_plain_member_cannot_approve_an_external_join() {
        let (_alice, _bob, (charlie, charlie_g)) = adr0045_trio();
        // `Group::create` seeds the owner-only role table, so charlie — added
        // afterwards and never promoted — is a plain member.
        assert!(matches!(
            charlie_g.own_role(&charlie).unwrap(),
            Some(Role::Member)
        ));

        let dave = Identity::new("dave".into()).unwrap();
        let request = dave
            .create_external_join_proposal(
                charlie_g.group_id().unwrap(),
                charlie_g.epoch().unwrap(),
            )
            .unwrap();
        let seen = charlie_g
            .process_incoming_verifying_roles(&charlie, request)
            .unwrap();
        let ProcessedMessage::ExternalJoinProposal {
            joiner_signature_key,
            ..
        } = seen
        else {
            panic!("expected an ExternalJoinProposal, got {seen:?}");
        };

        let epoch = charlie_g.epoch().unwrap();
        assert!(
            charlie_g
                .approve_external_join(&charlie, joiner_signature_key.clone())
                .is_err(),
            "a plain member must not approve",
        );
        assert_eq!(charlie_g.epoch().unwrap(), epoch);
        assert!(
            charlie_g.pending_proposals().unwrap().is_empty(),
            "a refused approve must publish no proposal and store none",
        );
        assert_eq!(
            charlie_g.pending_external_joins().unwrap(),
            vec![joiner_signature_key],
            "and must leave the request for an admin",
        );
    }

    /// ADR-0045 end to end across the UniFFI boundary: the owner **proposes**
    /// an add (epoch unchanged), a peer receives it as a stored
    /// `ProposalRecord`, the owner sweeps it into one commit, and the peer
    /// merges. The whole point is that `proposeAddMemberFromWire` does not
    /// advance the epoch — only `commitPendingProposals` does.
    #[test]
    fn propose_then_sweep_round_trips_through_the_ffi_types() {
        let ((alice, alice_g), (bob, bob_g), (charlie, charlie_g)) = adr0045_trio();
        let dave = Identity::new("dave".into()).unwrap();
        let dave_key = {
            let kp = dave.create_key_package().unwrap();
            key_package_signature_key(kp.clone()).unwrap()
        };

        let epoch_before = alice_g.epoch().unwrap();
        let proposal = alice_g
            .propose_add_member_from_wire(&alice, dave.create_key_package().unwrap())
            .unwrap();
        assert_eq!(
            alice_g.epoch().unwrap(),
            epoch_before,
            "a proposal must not advance the epoch",
        );

        // The proposer retains its own proposal so its own sweep can see it.
        assert_eq!(alice_g.pending_proposals().unwrap().len(), 1);

        // Bob receives it through the role-verifying gate: authorised (the
        // proposer is the owner) and therefore STORED.
        let received = bob_g
            .process_incoming_verifying_roles(&bob, proposal.clone())
            .unwrap();
        let ProcessedMessage::Proposal { record } = received else {
            panic!("expected a Proposal, got {received:?}");
        };
        assert!(record.stored, "an owner's add proposal must be stored");
        assert_eq!(record.sender_leaf, Some(alice_g.own_leaf_index().unwrap()));
        let ProposalKind::Add { signature_key, .. } = &record.kind else {
            panic!("expected Add, got {:?}", record.kind);
        };
        assert_eq!(signature_key, &dave_key);
        assert_eq!(bob_g.pending_proposals().unwrap().len(), 1);
        assert_eq!(bob_g.epoch().unwrap(), epoch_before);

        // Charlie receives the SAME proposal bytes through the OPEN path. That
        // path has no role table to judge against, so it stores nothing and
        // reports the drop as data — the honest, reachable `stored == false`.
        let charlie_saw = charlie_g.process_incoming(&charlie, proposal).unwrap();
        let ProcessedMessage::Proposal { record: dropped } = charlie_saw else {
            panic!("expected a Proposal");
        };
        assert!(
            !dropped.stored,
            "the open receive path must report the proposal as dropped",
        );
        assert!(
            matches!(dropped.kind, ProposalKind::Add { .. }),
            "a dropped proposal still reports what was asked for",
        );
        assert!(
            charlie_g.pending_proposals().unwrap().is_empty(),
            "a dropped proposal must not enter the pending set",
        );

        // Alice sweeps. One commit, one welcome, one swept record.
        let swept = alice_g
            .commit_pending_proposals(&alice)
            .unwrap()
            .expect("a pending add must produce a commit");
        assert_eq!(swept.swept.len(), 1);
        assert!(matches!(swept.swept[0].kind, ProposalKind::Add { .. }));
        assert!(swept.swept[0].stored);
        let welcome = swept
            .welcome
            .clone()
            .expect("an add sweep carries a welcome");
        assert!(
            alice_g.epoch().unwrap() > epoch_before,
            "the sweep advances"
        );

        assert!(matches!(
            bob_g
                .process_incoming_verifying_roles(&bob, swept.commit)
                .unwrap(),
            ProcessedMessage::Commit
        ));
        assert!(
            bob_g.pending_proposals().unwrap().is_empty(),
            "merging the commit empties the pending set",
        );
        let dave_g = Group::join_from_welcome(welcome, &dave).unwrap();
        assert_eq!(alice_g.member_count().unwrap(), 4);
        assert_eq!(bob_g.member_count().unwrap(), 4);
        assert_eq!(dave_g.member_count().unwrap(), 4);

        // Nothing left to sweep => no commit at all, NOT an empty one.
        assert!(alice_g.commit_pending_proposals(&alice).unwrap().is_none());
    }

    /// The peer-facing rank — the input `BeaconObservation.rank` needs. It must
    /// agree with [`Group::own_committer_rank`] for every member seen from
    /// every vantage point (the rank is a function of the shared, authenticated
    /// role table, so it cannot be observer-relative), and must fail closed to
    /// `None` for a non-member, a wrong-length key, and an open group.
    #[test]
    fn peer_committer_rank_agrees_with_own_rank_from_every_vantage_point() {
        let ((alice, alice_g), (bob, bob_g), (charlie, charlie_g)) = adr0045_trio();
        let owner_key = alice.signature_public_key();

        let promote = alice_g
            .set_group_roles(&alice, owner_key.clone(), vec![bob.signature_public_key()])
            .unwrap();
        for (id, g) in [(&bob, &bob_g), (&charlie, &charlie_g)] {
            assert!(matches!(
                g.process_incoming_verifying_roles(id, promote.clone())
                    .unwrap(),
                ProcessedMessage::Commit
            ));
        }

        for (label, g) in [
            ("alice", &alice_g),
            ("bob", &bob_g),
            ("charlie", &charlie_g),
        ] {
            assert_eq!(
                g.committer_rank(alice.signature_public_key()).unwrap(),
                Some(0),
                "{label} must rank the owner 0",
            );
            assert_eq!(
                g.committer_rank(bob.signature_public_key()).unwrap(),
                Some(1),
                "{label} must rank the sole admin 1",
            );
            assert_eq!(
                g.committer_rank(charlie.signature_public_key()).unwrap(),
                None,
                "{label} must give a plain member NO rank — not rank 0",
            );
        }

        // Each device's peer-derived rank for itself is its own rank.
        for (id, g) in [(&alice, &alice_g), (&bob, &bob_g), (&charlie, &charlie_g)] {
            assert_eq!(
                g.committer_rank(id.signature_public_key()).unwrap(),
                g.own_committer_rank(id).unwrap(),
            );
        }

        // Fail-closed inputs.
        let dave = Identity::new("dave".into()).unwrap();
        assert_eq!(
            alice_g.committer_rank(dave.signature_public_key()).unwrap(),
            None,
            "a non-member has no rank",
        );
        assert_eq!(alice_g.committer_rank(vec![0u8; 31]).unwrap(), None);
        assert_eq!(alice_g.committer_rank(Vec::new()).unwrap(), None);
    }

    /// A rank is a **position** in the role table's canonical order, not a
    /// stable identity — which is why `BeaconObservation.rank` must be derived
    /// when the verdict is taken and never captured at receipt.
    ///
    /// This runs the renumbering that makes a captured rank *incommensurable*
    /// rather than merely stale: promoting a second admin whose key sorts
    /// BEFORE the incumbent moves the incumbent from rank 1 to rank 2, so a
    /// rank captured under the old table names a different position in the new
    /// one. Both directions are exercised, so the test cannot pass by accident
    /// on whichever key happens to sort lower.
    #[test]
    fn a_rank_is_a_position_that_an_admin_change_renumbers() {
        let ((alice, alice_g), (bob, bob_g), (charlie, charlie_g)) = adr0045_trio();
        let owner_key = alice.signature_public_key();
        let bob_key = bob.signature_public_key();
        let charlie_key = charlie.signature_public_key();

        let apply = |wire: Vec<u8>| {
            for (id, g) in [(&bob, &bob_g), (&charlie, &charlie_g)] {
                assert!(matches!(
                    g.process_incoming_verifying_roles(id, wire.clone())
                        .unwrap(),
                    ProcessedMessage::Commit
                ));
            }
        };

        apply(
            alice_g
                .set_group_roles(&alice, owner_key.clone(), vec![bob_key.clone()])
                .unwrap(),
        );
        assert_eq!(alice_g.committer_rank(bob_key.clone()).unwrap(), Some(1));

        apply(
            alice_g
                .set_group_roles(
                    &alice,
                    owner_key.clone(),
                    vec![bob_key.clone(), charlie_key.clone()],
                )
                .unwrap(),
        );

        let bob_now = alice_g.committer_rank(bob_key.clone()).unwrap();
        let charlie_now = alice_g.committer_rank(charlie_key.clone()).unwrap();
        assert_eq!(
            [bob_now, charlie_now]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .len(),
            2,
            "both admins must be ranked",
        );
        assert_ne!(bob_now, charlie_now, "two admins cannot share a rank");
        // Exactly one of them is now rank 1 and the other rank 2 — the table's
        // own canonical (sorted) order decides which, and the loser of that
        // sort has been RENUMBERED away from the rank it held a moment ago.
        let mut ranks = [bob_now, charlie_now];
        ranks.sort_unstable();
        assert_eq!(ranks, [Some(1), Some(2)]);
        if bob_key < charlie_key {
            assert_eq!(bob_now, Some(1));
            assert_eq!(charlie_now, Some(2));
        } else {
            assert_eq!(
                bob_now,
                Some(2),
                "adding an admin that sorts first renumbers the incumbent from 1 to 2",
            );
            assert_eq!(charlie_now, Some(1));
        }
    }

    /// The committer rank must distinguish "owner" (`Some(0)`) from "not in the
    /// committer set" (`None`). Collapsing `None` into `0` would make every
    /// plain member believe it is the owner and sweep.
    #[test]
    fn committer_rank_never_conflates_absent_with_rank_zero() {
        let ((alice, alice_g), (bob, bob_g), (charlie, charlie_g)) = adr0045_trio();

        assert_eq!(
            alice_g.own_committer_rank(&alice).unwrap(),
            Some(0),
            "the owner is rank 0",
        );
        assert_eq!(
            bob_g.own_committer_rank(&bob).unwrap(),
            None,
            "a plain member has NO rank — not rank 0",
        );
        assert_eq!(charlie_g.own_committer_rank(&charlie).unwrap(), None);

        // Promote Bob to admin: he moves from None to Some(1), which is neither
        // the owner's 0 nor his previous absence.
        let owner_key = alice.signature_public_key();
        let promote = alice_g
            .set_group_roles(&alice, owner_key.clone(), vec![bob.signature_public_key()])
            .unwrap();
        for (id, g) in [(&bob, &bob_g), (&charlie, &charlie_g)] {
            assert!(matches!(
                g.process_incoming_verifying_roles(id, promote.clone())
                    .unwrap(),
                ProcessedMessage::Commit
            ));
        }
        assert_eq!(alice_g.own_committer_rank(&alice).unwrap(), Some(0));
        assert_eq!(bob_g.own_committer_rank(&bob).unwrap(), Some(1));
        assert_eq!(
            charlie_g.own_committer_rank(&charlie).unwrap(),
            None,
            "a plain member stays absent while an admin gains a rank",
        );
    }

    /// A `Remove`'s target leaf and an `Add`'s signature key live in **separate
    /// enum variants**, so no caller can read one as the other. This pins the
    /// classification a client's UI branches on.
    #[test]
    fn add_and_remove_payloads_are_not_interchangeable() {
        let ((alice, alice_g), (bob, bob_g), _charlie) = adr0045_trio();
        let dave = Identity::new("dave".into()).unwrap();
        let dave_key = key_package_signature_key(dave.create_key_package().unwrap()).unwrap();

        let add = alice_g
            .propose_add_member_from_wire(&alice, dave.create_key_package().unwrap())
            .unwrap();
        let ProcessedMessage::Proposal { record: add_rec } =
            bob_g.process_incoming_verifying_roles(&bob, add).unwrap()
        else {
            panic!("expected a Proposal");
        };
        match &add_rec.kind {
            ProposalKind::Add { signature_key, .. } => assert_eq!(signature_key, &dave_key),
            other => panic!("an Add must classify as Add, got {other:?}"),
        }

        // Bob proposes his OWN removal — leaving, which needs no role.
        let bob_leaf = bob_g.own_leaf_index().unwrap();
        let leave = bob_g.propose_remove_member(&bob, bob_leaf).unwrap();
        let ProcessedMessage::Proposal { record: rm_rec } = alice_g
            .process_incoming_verifying_roles(&alice, leave)
            .unwrap()
        else {
            panic!("expected a Proposal");
        };
        assert!(rm_rec.stored, "a member may always propose its own removal");
        match &rm_rec.kind {
            ProposalKind::Remove { target_leaf } => assert_eq!(*target_leaf, bob_leaf),
            other => panic!("a Remove must classify as Remove, got {other:?}"),
        }
    }

    /// `isActive` crosses the UniFFI boundary and tracks the eviction.
    ///
    /// The core property — that the accessor and the roster inference never
    /// disagree — is pinned in `lakko-mls`'s `group_membership_state` suite;
    /// what this asserts is the **wiring**, because the failure mode of a
    /// wrongly-plumbed accessor is a composer that stays enabled on a dead
    /// group, which is exactly the gap it exists to close.
    #[test]
    fn is_active_tracks_an_eviction_across_the_ffi_boundary() {
        let ((alice, alice_g), (bob, bob_g), (charlie, charlie_g)) = adr0045_trio();

        assert!(alice_g.is_active().unwrap(), "the owner starts active");
        assert!(charlie_g.is_active().unwrap(), "a member starts active");

        // Charlie leaves: a member may always propose its own removal.
        let charlie_leaf = charlie_g.own_leaf_index().unwrap();
        let leave = charlie_g
            .propose_remove_member(&charlie, charlie_leaf)
            .unwrap();
        alice_g
            .process_incoming_verifying_roles(&alice, leave.clone())
            .unwrap();
        bob_g.process_incoming_verifying_roles(&bob, leave).unwrap();
        // Charlie does not re-process its own proposal — OpenMLS refuses an
        // own message, and `proposeRemoveMember` already queued it locally.

        let sweep = alice_g
            .commit_pending_proposals(&alice)
            .unwrap()
            .expect("a pending removal must produce a commit");

        charlie_g
            .process_incoming_verifying_roles(&charlie, sweep.commit.clone())
            .unwrap();
        bob_g
            .process_incoming_verifying_roles(&bob, sweep.commit)
            .unwrap();

        assert!(
            !charlie_g.is_active().unwrap(),
            "the evicted device must read inactive through the FFI",
        );
        assert!(
            alice_g.is_active().unwrap() && bob_g.is_active().unwrap(),
            "the survivors must stay active",
        );
        // The asymmetry the UI depends on: reads still answer on a dead group.
        assert_eq!(charlie_g.member_count().unwrap(), 2);
        assert!(charlie_g
            .create_application_message(&charlie, b"nope".to_vec())
            .is_err());
    }

    /// A plain member's unauthorised proposal is refused **locally**, before
    /// anything reaches the wire — so a client never publishes a proposal every
    /// peer would drop.
    #[test]
    fn a_plain_member_cannot_mint_an_unauthorised_proposal() {
        let ((alice, alice_g), _bob, (charlie, charlie_g)) = adr0045_trio();
        let dave = Identity::new("dave".into()).unwrap();

        assert!(
            charlie_g
                .propose_add_member_from_wire(&charlie, dave.create_key_package().unwrap())
                .is_err(),
            "a plain member may not propose an Add",
        );
        assert!(
            charlie_g
                .propose_remove_member(&charlie, alice_g.own_leaf_index().unwrap())
                .is_err(),
            "nobody may propose removing the owner",
        );
        assert!(
            charlie_g
                .propose_group_roles(
                    &charlie,
                    charlie.signature_public_key(),
                    vec![alice.signature_public_key()],
                )
                .is_err(),
            "only the owner may propose a role-table rewrite",
        );
        assert!(
            charlie_g.commit_pending_proposals(&charlie).is_err(),
            "a plain member may not sweep",
        );
        assert!(charlie_g.pending_proposals().unwrap().is_empty());
    }

    // ========================================================================
    // ADR-0045 S3b — the staged sweep + ownership transfer
    // ========================================================================

    /// **Won.** Stage leaves the group at the epoch it built against, holding
    /// an un-merged commit; the verdict merge lands it at the next epoch, in
    /// the same state the peer that applied the published commit reached.
    ///
    /// This is the sequence the whole staged pair exists for: stage, publish,
    /// read the journal order, merge.
    #[test]
    fn staging_then_merging_lands_the_sweep() {
        let ((alice, alice_g), (bob, bob_g), _charlie) = adr0045_trio();
        let dave = Identity::new("dave".into()).unwrap();

        // The proposal reaches every member FIRST. A swept commit carries it
        // by REFERENCE, and OpenMLS resolves those references at commit
        // receipt on every member — a peer that never saw the proposal refuses
        // the commit with `InvalidCommit(MissingProposal)`. That is RFC 9420
        // enforcing ADR-0045's "store at receipt" rule, not a test artefact.
        let proposal = alice_g
            .propose_add_member_from_wire(&alice, dave.create_key_package().unwrap())
            .unwrap();
        assert!(matches!(
            bob_g
                .process_incoming_verifying_roles(&bob, proposal)
                .unwrap(),
            ProcessedMessage::Proposal { .. }
        ));
        let epoch_before = alice_g.epoch().unwrap();
        assert!(!alice_g.has_staged_sweep().unwrap());

        // 1. Stage. The epoch does NOT move, and the group now owes a verdict.
        let staged = alice_g
            .stage_pending_proposals(&alice)
            .unwrap()
            .expect("a pending add must produce a staged sweep");
        assert_eq!(
            alice_g.epoch().unwrap(),
            epoch_before,
            "staging must NOT advance the epoch — that is the whole point",
        );
        assert!(
            alice_g.has_staged_sweep().unwrap(),
            "a staged sweep must be reported as awaiting a verdict",
        );
        assert_eq!(staged.swept.len(), 1);
        assert!(matches!(staged.swept[0].kind, ProposalKind::Add { .. }));
        let welcome = staged
            .welcome
            .clone()
            .expect("an add sweep carries a welcome");

        // 2. Publish. Bob applies it and reaches the new epoch alone.
        assert!(matches!(
            bob_g
                .process_incoming_verifying_roles(&bob, staged.commit.clone())
                .unwrap(),
            ProcessedMessage::Commit
        ));
        assert_eq!(
            bob_g.epoch().unwrap(),
            epoch_before + 1,
            "the peer advances on the published commit while the stager has not",
        );

        // 3 + 4. The journal ordered it first, so merge.
        assert_eq!(
            alice_g.merge_staged_sweep(&alice).unwrap(),
            StagedSweepOutcome::Merged,
        );
        assert!(!alice_g.has_staged_sweep().unwrap());
        assert_eq!(alice_g.epoch().unwrap(), bob_g.epoch().unwrap());
        assert!(
            alice_g.pending_proposals().unwrap().is_empty(),
            "a merged sweep empties the proposal store",
        );

        // The Welcome staged before the merge is the real one.
        let dave_g = Group::join_from_welcome(welcome, &dave).unwrap();
        assert_eq!(dave_g.member_count().unwrap(), 4);
        assert_eq!(alice_g.member_count().unwrap(), 4);

        // Merging again is an idempotent no-op — what makes ADR-0040's restart
        // sweep safe to run twice.
        assert_eq!(
            alice_g.merge_staged_sweep(&alice).unwrap(),
            StagedSweepOutcome::NothingStaged,
        );
    }

    /// **Lost.** Abandoning returns the group to the epoch the sweep was built
    /// against and throws no work away — the proposals it selected are still
    /// pending, so the winner (or a later sweep) picks them up.
    #[test]
    fn staging_then_abandoning_rolls_back_and_keeps_the_proposals() {
        let ((alice, alice_g), _bob, _charlie) = adr0045_trio();
        let dave = Identity::new("dave".into()).unwrap();

        alice_g
            .propose_add_member_from_wire(&alice, dave.create_key_package().unwrap())
            .unwrap();
        let epoch_before = alice_g.epoch().unwrap();
        let members_before = alice_g.member_count().unwrap();

        let staged = alice_g.stage_pending_proposals(&alice).unwrap().unwrap();
        assert_eq!(staged.swept.len(), 1);
        assert!(alice_g.has_staged_sweep().unwrap());

        assert_eq!(
            alice_g.abandon_staged_sweep(&alice).unwrap(),
            StagedSweepOutcome::Abandoned,
        );
        assert!(!alice_g.has_staged_sweep().unwrap());
        assert_eq!(
            alice_g.epoch().unwrap(),
            epoch_before,
            "abandoning must leave the group exactly where it was",
        );
        assert_eq!(alice_g.member_count().unwrap(), members_before);
        assert_eq!(
            alice_g.pending_proposals().unwrap().len(),
            1,
            "an abandoned sweep leaves its proposals for the next committer",
        );

        // And the group is free to act again: a second stage now succeeds.
        let restaged = alice_g.stage_pending_proposals(&alice).unwrap().unwrap();
        assert_eq!(restaged.swept.len(), 1);
        assert_eq!(
            alice_g.merge_staged_sweep(&alice).unwrap(),
            StagedSweepOutcome::Merged,
        );
        assert_eq!(alice_g.member_count().unwrap(), members_before + 1);

        // Abandoning again is an idempotent no-op.
        assert_eq!(
            alice_g.abandon_staged_sweep(&alice).unwrap(),
            StagedSweepOutcome::NothingStaged,
        );
    }

    // ========================================================================
    // Typed membership refusals (ADR-0045 §9.1 item 1 / §12 item 6)
    // ========================================================================

    /// Every core [`RefusalReason`] reaches Swift / Kotlin as its **own**
    /// `FfiError` variant, and no two share one.
    ///
    /// This is the property the whole change exists for. `#[uniffi(flat_error)]`
    /// lowers only the variant discriminant and the `Display` string, so the
    /// variant *is* the machine-readable channel — which makes a collision
    /// between two reasons silently unfixable from the client side, and makes
    /// this bijection the thing worth pinning.
    #[test]
    fn every_refusal_reason_gets_its_own_ffi_variant() {
        let mut seen: Vec<FfiRefusalReason> = Vec::new();
        for reason in lakko_mls::RefusalReason::ALL {
            let err = FfiError::from(lakko_mls::Error::refused_for_tests(
                reason,
                "a refusal sentence",
            ));
            let recovered = err
                .refusal_reason()
                .unwrap_or_else(|| panic!("{reason:?} lost its reason at the FFI: {err:?}"));
            assert!(
                !seen.contains(&recovered),
                "{reason:?} shares an FfiError variant with an earlier reason",
            );
            seen.push(recovered);
            assert_eq!(
                membership_refusal_code(recovered),
                reason.code(),
                "the FFI code must be the core code verbatim",
            );
        }
        assert_eq!(seen.len(), lakko_mls::RefusalReason::ALL.len());
    }

    /// The prose survives too. A developer reading a Kotlin stack must still get
    /// the sentence — the typed reason is *additional*, never a replacement.
    #[test]
    fn a_typed_refusal_still_carries_its_sentence() {
        for reason in lakko_mls::RefusalReason::ALL {
            let err = FfiError::from(lakko_mls::Error::refused_for_tests(
                reason,
                "the sentence the core wrote",
            ));
            assert_eq!(
                format!("{err}"),
                "the sentence the core wrote",
                "{reason:?} must lower its detail verbatim",
            );
        }
    }

    /// The three-valued disposition crosses the boundary as data, not as
    /// something a client re-decides from a variant's name — and the
    /// **retryable** set that crosses is exactly the queue-full one.
    #[test]
    fn the_refusal_disposition_crosses_the_boundary() {
        for reason in lakko_mls::RefusalReason::ALL {
            let ffi = FfiRefusalReason::from(reason);
            let expected = match reason.disposition() {
                lakko_mls::RefusalDisposition::RetryLater => FfiRefusalDisposition::RetryLater,
                lakko_mls::RefusalDisposition::DoNotRetry => FfiRefusalDisposition::DoNotRetry,
                lakko_mls::RefusalDisposition::NotPermitted => FfiRefusalDisposition::NotPermitted,
            };
            assert_eq!(membership_refusal_disposition(ffi), expected, "{reason:?}");
            assert_eq!(
                membership_refusal_retry_may_succeed(ffi),
                reason.retry_may_succeed(),
                "{reason:?}",
            );
        }

        let retryable: Vec<FfiRefusalReason> = lakko_mls::RefusalReason::ALL
            .into_iter()
            .map(FfiRefusalReason::from)
            .filter(|r| membership_refusal_retry_may_succeed(*r))
            .collect();
        assert_eq!(
            retryable,
            vec![
                FfiRefusalReason::RoleChangePending,
                FfiRefusalReason::PendingSetFull,
            ],
            "only a full bounded queue may be offered as a retry",
        );

        // The middle value a two-way split would have got wrong.
        assert_eq!(
            membership_refusal_disposition(FfiRefusalReason::SweepAlreadyStaged),
            FfiRefusalDisposition::DoNotRetry,
        );
        assert_eq!(
            membership_refusal_disposition(FfiRefusalReason::AddNotOwnerOrAdmin),
            FfiRefusalDisposition::NotPermitted,
        );
    }

    /// End to end through the real object surface: a plain member's add is
    /// refused with the add-specific variant, and an admin's role change with
    /// the owner-only one — so a client can tell the two §6 strings apart
    /// without looking at a message.
    #[test]
    fn real_refusals_arrive_typed_through_the_group_object() {
        let ((alice, alice_g), (bob, bob_g), (charlie, charlie_g)) = adr0045_trio();
        let promote = alice_g
            .set_group_roles(
                &alice,
                alice.signature_public_key(),
                vec![bob.signature_public_key()],
            )
            .unwrap();
        bob_g
            .process_incoming_verifying_roles(&bob, promote.clone())
            .unwrap();
        charlie_g
            .process_incoming_verifying_roles(&charlie, promote)
            .unwrap();

        let dave = Identity::new("dave".into()).unwrap();
        let err = charlie_g
            .propose_add_member_from_wire(&charlie, dave.create_key_package().unwrap())
            .unwrap_err();
        assert_eq!(
            err.refusal_reason(),
            Some(FfiRefusalReason::AddNotOwnerOrAdmin),
            "a member's add must be typed, got {err:?}",
        );

        let err = bob_g
            .set_group_roles(&bob, alice.signature_public_key(), vec![])
            .unwrap_err();
        assert_eq!(
            err.refusal_reason(),
            Some(FfiRefusalReason::NotOwner),
            "an admin's role change must be typed, got {err:?}",
        );

        // A wire-format failure is NOT a refusal, so a client cannot mistake it
        // for a permission answer.
        let err = alice_g
            .propose_add_member_from_wire(&alice, b"not a key package".to_vec())
            .unwrap_err();
        assert_eq!(err.refusal_reason(), None, "got {err:?}");
    }

    /// A second stage is refused with the **typed** [`FfiError::SweepAlreadyStaged`]
    /// variant, not a message a caller would have to string-match.
    ///
    /// Fail-closed: building a second sweep would silently replace a commit
    /// that may already be on the wire, so the refusal is the correct
    /// behaviour and the caller's job is to reach the outstanding verdict.
    #[test]
    fn a_second_stage_is_refused_with_a_typed_error() {
        let ((alice, alice_g), _bob, _charlie) = adr0045_trio();
        let dave = Identity::new("dave".into()).unwrap();
        let erin = Identity::new("erin".into()).unwrap();

        alice_g
            .propose_add_member_from_wire(&alice, dave.create_key_package().unwrap())
            .unwrap();
        let first = alice_g.stage_pending_proposals(&alice).unwrap().unwrap();

        let err = alice_g.stage_pending_proposals(&alice).unwrap_err();
        assert!(
            matches!(err, FfiError::SweepAlreadyStaged(_)),
            "the second stage must be the TYPED refusal, got {err:?}",
        );

        // A staged sweep also freezes this group's own membership operations,
        // so the client obligation is real: reach the verdict or stay frozen.
        assert!(
            alice_g
                .propose_add_member_from_wire(&alice, erin.create_key_package().unwrap())
                .is_err(),
            "a staged sweep freezes this group's own proposals",
        );

        // Reaching the verdict clears it, and the FIRST staged commit is still
        // the one that was on the wire.
        assert_eq!(
            alice_g.merge_staged_sweep(&alice).unwrap(),
            StagedSweepOutcome::Merged,
        );
        assert!(!first.commit.is_empty());
        assert!(alice_g.stage_pending_proposals(&alice).unwrap().is_none());
    }

    /// `hasStagedSweep` reports honestly on every path, and a verdict on a
    /// group that never staged anything is `NothingStaged` rather than an
    /// error — which is what makes both verdict calls idempotent.
    ///
    /// In particular the **eager** `commitPendingProposals` leaves nothing
    /// staged, so a rollback can never reach a commit it already merged.
    #[test]
    fn has_staged_sweep_reports_honestly_on_every_path() {
        let ((alice, alice_g), _bob, _charlie) = adr0045_trio();
        let dave = Identity::new("dave".into()).unwrap();

        // A fresh group: nothing staged, both verdicts are no-ops.
        assert!(!alice_g.has_staged_sweep().unwrap());
        assert_eq!(
            alice_g.merge_staged_sweep(&alice).unwrap(),
            StagedSweepOutcome::NothingStaged,
        );
        assert_eq!(
            alice_g.abandon_staged_sweep(&alice).unwrap(),
            StagedSweepOutcome::NothingStaged,
        );

        // Nothing pending: staging emits no commit and stages nothing.
        assert!(alice_g.stage_pending_proposals(&alice).unwrap().is_none());
        assert!(
            !alice_g.has_staged_sweep().unwrap(),
            "an empty stage must not leave a verdict owed",
        );

        // The EAGER path merges before it returns, so it leaves nothing staged.
        alice_g
            .propose_add_member_from_wire(&alice, dave.create_key_package().unwrap())
            .unwrap();
        assert!(alice_g.commit_pending_proposals(&alice).unwrap().is_some());
        assert!(
            !alice_g.has_staged_sweep().unwrap(),
            "the eager sweep must leave nothing to adjudicate",
        );
        assert_eq!(
            alice_g.abandon_staged_sweep(&alice).unwrap(),
            StagedSweepOutcome::NothingStaged,
            "abandoning after an eager commit must not undo it",
        );
    }

    /// Ownership transfer round-trips through the FFI with **both**
    /// `OutgoingOwner` variants, and the variant is what decides the outgoing
    /// owner's fate — `Admin` keeps them in the admin set, `Member` does not.
    ///
    /// The variant is a required argument on both entry points; there is no
    /// default for a binding to collapse it to.
    #[test]
    fn ownership_transfer_honours_both_outgoing_owner_variants() {
        for (outgoing, alice_keeps_admin) in
            [(OutgoingOwner::Admin, true), (OutgoingOwner::Member, false)]
        {
            let ((alice, alice_g), (bob, bob_g), (charlie, charlie_g)) = adr0045_trio();
            let alice_key = alice.signature_public_key();
            let bob_key = bob.signature_public_key();

            assert!(matches!(
                alice_g.own_role(&alice).unwrap(),
                Some(Role::Owner)
            ));
            assert!(matches!(bob_g.own_role(&bob).unwrap(), Some(Role::Member)));

            let commit = alice_g
                .transfer_ownership(&alice, bob_key.clone(), outgoing)
                .unwrap();
            for (id, g) in [(&bob, &bob_g), (&charlie, &charlie_g)] {
                assert!(
                    matches!(
                        g.process_incoming_verifying_roles(id, commit.clone())
                            .unwrap(),
                        ProcessedMessage::Commit
                    ),
                    "every peer must accept the owner's transfer commit",
                );
            }

            for g in [&alice_g, &bob_g, &charlie_g] {
                let table = g.group_roles().unwrap().expect("role table");
                assert_eq!(table.owner, bob_key, "{outgoing:?}: bob must be the owner");
                assert_eq!(
                    table.admins.contains(&alice_key),
                    alice_keeps_admin,
                    "{outgoing:?}: the outgoing owner's admin membership must follow the variant",
                );
            }
            assert!(matches!(bob_g.own_role(&bob).unwrap(), Some(Role::Owner)));
            assert!(matches!(
                alice_g.own_role(&alice).unwrap(),
                Some(Role::Admin | Role::Member),
            ));
            assert_eq!(
                alice_g.own_committer_rank(&alice).unwrap().is_some(),
                alice_keeps_admin,
                "{outgoing:?}: a demoted former owner leaves the committer set entirely",
            );

            // The new owner can administer; the former owner can no longer
            // rewrite the table however the variant went.
            assert!(bob_g
                .set_group_roles(&bob, bob_key.clone(), vec![alice_key.clone()])
                .is_ok());
            assert!(
                alice_g
                    .transfer_ownership(&alice, alice_key.clone(), OutgoingOwner::Admin)
                    .is_err(),
                "{outgoing:?}: a transfer is irreversible by the person who made it",
            );
        }
    }

    /// The proposal-path twin: the owner **proposes** a transfer (epoch
    /// unchanged), a peer stores it, and a sweep installs the new owner. The
    /// incoming owner may sweep it themselves once they are an admin.
    #[test]
    fn propose_ownership_transfer_rides_the_proposal_path() {
        let ((alice, alice_g), (bob, bob_g), (charlie, charlie_g)) = adr0045_trio();
        let bob_key = bob.signature_public_key();
        let epoch_before = alice_g.epoch().unwrap();

        let proposal = alice_g
            .propose_ownership_transfer(&alice, bob_key.clone(), OutgoingOwner::Member)
            .unwrap();
        assert_eq!(
            alice_g.epoch().unwrap(),
            epoch_before,
            "a proposal must not advance the epoch",
        );

        for (id, g) in [(&bob, &bob_g), (&charlie, &charlie_g)] {
            let ProcessedMessage::Proposal { record } = g
                .process_incoming_verifying_roles(id, proposal.clone())
                .unwrap()
            else {
                panic!("expected a Proposal");
            };
            assert!(record.stored, "the owner's role change must be stored");
            assert!(matches!(record.kind, ProposalKind::RoleChange));
        }

        // The owner sweeps its own proposal through the staged pair.
        let staged = alice_g.stage_pending_proposals(&alice).unwrap().unwrap();
        for (id, g) in [(&bob, &bob_g), (&charlie, &charlie_g)] {
            assert!(matches!(
                g.process_incoming_verifying_roles(id, staged.commit.clone())
                    .unwrap(),
                ProcessedMessage::Commit
            ));
        }
        assert_eq!(
            alice_g.merge_staged_sweep(&alice).unwrap(),
            StagedSweepOutcome::Merged,
        );

        for g in [&alice_g, &bob_g, &charlie_g] {
            assert_eq!(g.group_roles().unwrap().expect("table").owner, bob_key);
        }
        assert!(matches!(bob_g.own_role(&bob).unwrap(), Some(Role::Owner)));
        assert!(matches!(
            alice_g.own_role(&alice).unwrap(),
            Some(Role::Member)
        ));
    }

    /// C-63 at the FFI: naming an owner that no leaf holds is refused locally
    /// with a clear error, before any MLS state is touched. An owner outside
    /// the group could never be replaced (a role change is owner-only) nor
    /// removed (the owner's leaf is unremovable), so the group would be
    /// permanently un-administrable.
    #[test]
    fn transfer_refuses_a_non_member_and_a_bad_key_length() {
        let ((alice, alice_g), _bob, _charlie) = adr0045_trio();
        let stranger = Identity::new("stranger".into()).unwrap();
        let epoch_before = alice_g.epoch().unwrap();

        for outgoing in [OutgoingOwner::Admin, OutgoingOwner::Member] {
            assert!(
                alice_g
                    .transfer_ownership(&alice, stranger.signature_public_key(), outgoing)
                    .is_err(),
                "{outgoing:?}: an owner outside the group must be refused",
            );
            assert!(alice_g
                .propose_ownership_transfer(&alice, stranger.signature_public_key(), outgoing)
                .is_err());
            // Wrong length never reaches the core.
            assert!(matches!(
                alice_g
                    .transfer_ownership(&alice, vec![0u8; 31], outgoing)
                    .unwrap_err(),
                FfiError::Codec(_),
            ));
            assert!(matches!(
                alice_g
                    .propose_ownership_transfer(&alice, vec![0u8; 33], outgoing)
                    .unwrap_err(),
                FfiError::Codec(_),
            ));
        }

        assert_eq!(
            alice_g.epoch().unwrap(),
            epoch_before,
            "every refusal must leave the group exactly as it was",
        );
        assert!(!alice_g.has_staged_sweep().unwrap());
        assert!(alice_g.pending_proposals().unwrap().is_empty());
    }

    /// A plain member may neither stage a sweep nor transfer ownership.
    #[test]
    fn a_plain_member_can_neither_stage_nor_transfer() {
        let ((alice, alice_g), _bob, (charlie, charlie_g)) = adr0045_trio();

        assert!(
            charlie_g.stage_pending_proposals(&charlie).is_err(),
            "a plain member may not sweep, staged or eager",
        );
        assert!(charlie_g
            .transfer_ownership(
                &charlie,
                charlie.signature_public_key(),
                OutgoingOwner::Admin,
            )
            .is_err());
        assert!(charlie_g
            .propose_ownership_transfer(
                &charlie,
                charlie.signature_public_key(),
                OutgoingOwner::Member,
            )
            .is_err());
        assert!(!charlie_g.has_staged_sweep().unwrap());
        assert_eq!(
            alice_g.group_roles().unwrap().expect("table").owner,
            alice.signature_public_key(),
        );
    }

    /// `Identity::from_seed` is deterministic for the same input
    /// — the OPAQUE-export-key path 1.15d / 1.15e5 rely on for
    /// "same password → same device identity across re-installs."
    /// Verifies both that the constructor accepts a 32-byte seed
    /// and that every public accessor (Ed25519 / X25519 / seed)
    /// produces stable bytes across reconstruction.
    #[test]
    fn identity_from_seed_is_deterministic() {
        let seed = [0x42u8; 32];
        let a = Identity::from_seed(seed.to_vec()).unwrap();
        let b = Identity::from_seed(seed.to_vec()).unwrap();

        assert_eq!(a.signature_public_key(), b.signature_public_key());
        assert_eq!(a.x25519_public_key(), b.x25519_public_key());
        assert_eq!(a.x25519_secret_key(), b.x25519_secret_key());
        assert_eq!(a.ed25519_seed(), b.ed25519_seed());

        // Every key has the expected length.
        assert_eq!(a.signature_public_key().len(), 32);
        assert_eq!(a.x25519_public_key().len(), 32);
        assert_eq!(a.x25519_secret_key().len(), 32);
        assert_eq!(a.ed25519_seed().len(), 32);

        // Note: `ed25519_seed()` returns the HKDF-derived
        // Ed25519 sub-seed, not the input bytes. The input
        // `seed` is hashed through HKDF with separate "info"
        // labels for the Ed25519 and X25519 sub-keys (see
        // `lakko_mls::Identity::from_seed` impl), so the
        // accessor exposes the derived material, not the
        // master. Still useful for the contact-request flow:
        // it's the Ed25519 signing seed `lakko_contacts::compose`
        // demands.
    }

    /// Different seeds produce different keys — sanity check that
    /// the derivation actually depends on the input.
    #[test]
    fn identity_from_seed_distinct_inputs_distinct_outputs() {
        let alice = Identity::from_seed(vec![1u8; 32]).unwrap();
        let bob = Identity::from_seed(vec![2u8; 32]).unwrap();
        assert_ne!(alice.signature_public_key(), bob.signature_public_key());
        assert_ne!(alice.x25519_public_key(), bob.x25519_public_key());
    }

    /// `Identity::snapshot_storage` produces a non-empty blob
    /// on a fresh identity, and the bytes round-trip cleanly
    /// through `restore_storage` on a parallel identity built
    /// from the same seed.
    ///
    /// Driving 1.15f3's persistence path: a Kotlin caller will
    /// build `Identity.fromSeed(exportKey)` post-login,
    /// re-hydrate its OpenMLS provider state via
    /// `restore_storage(snapshotBlob)`, and from that point the
    /// in-memory state has every group + ratchet key the prior
    /// process wrote.
    #[test]
    fn identity_snapshot_round_trips_through_restore() {
        // Build Alice + a Group with Bob — that mutates the
        // provider storage (init keys + group ratchet).
        let alice = Identity::from_seed(vec![0xAAu8; 32]).unwrap();
        let bob = Identity::from_seed(vec![0xBBu8; 32]).unwrap();
        let alice_group = Group::create(&alice).unwrap();
        let bob_kp = bob.create_key_package().unwrap();
        let add = alice_group.add_member_from_wire(&alice, bob_kp).unwrap();
        let _ = add; // Welcome bytes; not used for this test.
        let epoch_after_add = alice_group.epoch().unwrap();

        // Snapshot — must be non-empty.
        let snapshot = alice.snapshot_storage().unwrap();
        assert!(
            !snapshot.is_empty(),
            "snapshot must carry at least the bookkeeping bytes",
        );

        // Build a parallel Alice from the same seed (her
        // public keys agree by construction; the secrets are
        // deterministic from the seed too). Her provider
        // storage starts smaller — no group state on it yet.
        let alice_clone = Identity::from_seed(vec![0xAAu8; 32]).unwrap();
        let empty_snapshot = alice_clone.snapshot_storage().unwrap();
        assert_ne!(
            snapshot, empty_snapshot,
            "fresh identity must have a different storage blob",
        );

        // Restore: the clone now holds the prior provider's
        // state, including the group ratchet that produced
        // `epoch_after_add`. Doesn't throw on well-formed input.
        alice_clone.restore_storage(snapshot.clone()).unwrap();

        // Snapshot the restored identity — must be non-empty
        // (the restored state has every byte the original
        // wrote). We DO NOT assert byte-equality here: the
        // underlying OpenMLS provider uses a HashMap-backed
        // store whose serialisation order is not stable across
        // instances, so two providers holding semantically
        // identical state may emit slightly different blob
        // bytes. The semantic round-trip below is what we
        // actually care about.
        let restored = alice_clone.snapshot_storage().unwrap();
        assert!(!restored.is_empty());

        // Semantic round-trip: snapshot the restored clone,
        // build a SECOND clone, restore that — the
        // double-restored blob must be non-empty (i.e. the
        // restore path is stable under repeated apply).
        let alice_clone2 = Identity::from_seed(vec![0xAAu8; 32]).unwrap();
        alice_clone2.restore_storage(restored).unwrap();
        assert!(!alice_clone2.snapshot_storage().unwrap().is_empty());

        // Sanity: the original Alice still produces an exporter
        // for the group she made — `restore_storage` on a clone
        // doesn't mutate the original.
        let exporter = alice_group
            .export_secret(&alice, "lakko/test".into(), b"ctx".to_vec(), 32)
            .unwrap();
        assert_eq!(exporter.len(), 32);
        let _ = epoch_after_add; // documented in the dance above.
    }

    /// `restore_storage` rejects a truncated / malformed
    /// snapshot rather than corrupting the in-memory state.
    #[test]
    fn identity_restore_rejects_malformed_input() {
        let alice = Identity::from_seed(vec![0x11u8; 32]).unwrap();
        // Empty input has no length prefix to start with —
        // surfaces as the underlying Identity error.
        let err = alice.restore_storage(vec![]).unwrap_err();
        assert!(matches!(err, FfiError::Identity(_)));
    }

    /// `load_group` round-trips a snapshot: create a group +
    /// snapshot the Identity's provider storage + restore into
    /// a parallel Identity + `load_group(group_id)` rebuilds a
    /// handle that produces the same exporter as the original.
    /// Drives 1.15f5's eager-rebuild path through the FFI
    /// boundary.
    #[test]
    fn load_group_round_trips_through_snapshot() {
        // Build Alice, create a group, snapshot her provider
        // storage.
        let alice = Identity::from_seed(vec![0xAAu8; 32]).unwrap();
        let alice_group = Group::create(&alice).unwrap();
        let group_id = alice_group.group_id().unwrap();
        let exporter_before = alice_group
            .export_secret(&alice, "lakko/test".into(), b"ctx".to_vec(), 32)
            .unwrap();
        let snapshot = alice.snapshot_storage().unwrap();

        // Build a parallel Alice from the same seed; her
        // provider has no groups until we restore.
        let alice_clone = Identity::from_seed(vec![0xAAu8; 32]).unwrap();
        // load_group on the un-restored clone returns None —
        // the group doesn't exist in her provider yet.
        let pre_restore = load_group(&alice_clone, group_id.clone()).unwrap();
        assert!(pre_restore.is_none(), "no group before restore");

        // Restore + load.
        alice_clone.restore_storage(snapshot).unwrap();
        let loaded = load_group(&alice_clone, group_id.clone())
            .unwrap()
            .expect("load_group must return Some after restore");

        // The restored handle produces the same exporter the
        // original did — the ratchet state survived the
        // round-trip.
        let exporter_after = loaded
            .export_secret(&alice_clone, "lakko/test".into(), b"ctx".to_vec(), 32)
            .unwrap();
        assert_eq!(exporter_before, exporter_after);

        // `group_id` on the loaded handle matches the original.
        assert_eq!(loaded.group_id().unwrap(), group_id);
    }

    /// `Group::group_id` is stable across epochs — `1.15f2`'s
    /// chat client uses the value to key the DS group journal,
    /// so a wandering id would route messages to a dead
    /// `/ds/group/{...}` slot mid-conversation.
    #[test]
    fn group_id_is_stable_across_epoch_advance() {
        let alice = Identity::new("alice".into()).unwrap();
        let bob = Identity::new("bob".into()).unwrap();

        let alice_group = Group::create(&alice).unwrap();
        let epoch0_id = alice_group.group_id().unwrap();
        assert!(!epoch0_id.is_empty(), "group_id must be non-empty");
        let epoch0_value = alice_group.epoch().unwrap();

        // Add Bob — epoch advances, group_id must NOT.
        let bob_kp = bob.create_key_package().unwrap();
        alice_group.add_member_from_wire(&alice, bob_kp).unwrap();
        let epoch1_id = alice_group.group_id().unwrap();
        let epoch1_value = alice_group.epoch().unwrap();

        assert_ne!(
            epoch0_value, epoch1_value,
            "epoch must have advanced after add_member",
        );
        assert_eq!(epoch0_id, epoch1_id, "group_id must not change with epoch");
    }

    // ============================================================================
    // PublisherGroup FFI surface (G4-2-mls-persistence step 2)
    // ============================================================================

    /// Channel id helper — 16 bytes, deterministic per test
    /// case so a re-run sees the same persistence-key shape
    /// without depending on any RNG.
    fn channel_id_filled(byte: u8) -> Vec<u8> {
        vec![byte; lakko_mls::CHANNEL_ID_LEN]
    }

    #[test]
    fn publisher_group_create_then_channel_seed_is_stable() {
        let alice = Identity::new("alice-publisher".into()).unwrap();
        let cid = channel_id_filled(0xAA);

        let pg = PublisherGroup::create(&alice, cid.clone()).unwrap();
        let seed_1 = pg.channel_seed(&alice).unwrap();
        let seed_2 = pg.channel_seed(&alice).unwrap();
        // Same group, same epoch -> same seed.
        assert_eq!(seed_1, seed_2);
        assert_eq!(seed_1.len(), 32);
        // Channel id round-trips.
        assert_eq!(pg.channel_id().unwrap(), cid);
    }

    #[test]
    fn publisher_group_open_or_create_is_idempotent_across_handles() {
        // The load-bearing FFI test: two consecutive
        // `open_or_create` calls under the SAME identity for
        // the SAME channel id must resolve to the SAME
        // persisted group — i.e. they must produce byte-
        // identical channel seeds. If they don't, we're back
        // to the throwaway-group bug G4-2-mls-persistence
        // closes.
        let alice = Identity::new("alice-persist".into()).unwrap();
        let cid = channel_id_filled(0xBB);

        let first = publisher_group_open_or_create(&alice, cid.clone()).unwrap();
        let seed_first = first.channel_seed(&alice).unwrap();

        let second = publisher_group_open_or_create(&alice, cid.clone()).unwrap();
        let seed_second = second.channel_seed(&alice).unwrap();

        assert_eq!(seed_first, seed_second);
    }

    #[test]
    fn publisher_group_load_returns_null_before_create() {
        let alice = Identity::new("alice-empty".into()).unwrap();
        let cid = channel_id_filled(0xCC);

        let loaded = publisher_group_load(&alice, cid).unwrap();
        assert!(loaded.is_none(), "no group should be persisted yet");
    }

    #[test]
    fn publisher_group_load_returns_group_after_create() {
        let alice = Identity::new("alice-loadback".into()).unwrap();
        let cid = channel_id_filled(0xDD);

        let _created = PublisherGroup::create(&alice, cid.clone()).unwrap();
        let loaded = publisher_group_load(&alice, cid.clone()).unwrap();
        assert!(loaded.is_some(), "load must find the freshly-created group");
        assert_eq!(loaded.unwrap().channel_id().unwrap(), cid);
    }

    #[test]
    fn publisher_group_rejects_wrong_length_channel_id() {
        let alice = Identity::new("alice-badid".into()).unwrap();
        // 8 bytes — half a channel id. Must refuse rather
        // than silently zero-pad (would let two channels
        // collide under the deterministic group id).
        let bad = vec![0u8; 8];
        let create_res = PublisherGroup::create(&alice, bad.clone());
        assert!(create_res.is_err(), "create must refuse short channel id");
        let create_err = create_res.err().unwrap();
        assert!(matches!(
            create_err,
            FfiError::Codec(_) | FfiError::Group(_)
        ));

        let load_res = publisher_group_load(&alice, bad);
        assert!(load_res.is_err(), "load must refuse short channel id");
        let load_err = load_res.err().unwrap();
        assert!(matches!(load_err, FfiError::Codec(_) | FfiError::Group(_)));
    }

    #[test]
    fn publisher_group_rotate_changes_seed() {
        // G4-2-mls-persistence step 4 (FFI) — the load-bearing
        // test: rotating the PublisherGroup MUST produce a
        // distinct exporter output (different epoch under the
        // hood). Mirror of the lakko-mls test of the same
        // shape, but exercised through the UniFFI surface so
        // the iOS / Android composer paths are covered too.
        let alice = Identity::new("alice-rotate".into()).unwrap();
        let cid = channel_id_filled(0xEE);

        let pg = publisher_group_open_or_create(&alice, cid).unwrap();
        let seed_pre = pg.channel_seed(&alice).unwrap();

        let _commit = pg.rotate(&alice).unwrap();
        let seed_post = pg.channel_seed(&alice).unwrap();

        assert_ne!(seed_pre, seed_post);
        assert_eq!(seed_post.len(), 32);
    }

    #[test]
    fn publisher_group_distinct_channels_distinct_seeds() {
        let alice = Identity::new("alice-multichannel".into()).unwrap();
        let cid_a = channel_id_filled(0x11);
        let cid_b = channel_id_filled(0x22);

        let pg_a = publisher_group_open_or_create(&alice, cid_a).unwrap();
        let pg_b = publisher_group_open_or_create(&alice, cid_b).unwrap();
        let seed_a = pg_a.channel_seed(&alice).unwrap();
        let seed_b = pg_b.channel_seed(&alice).unwrap();
        assert_ne!(seed_a, seed_b);
    }

    fn ed25519_pub_of(id: &Identity) -> Vec<u8> {
        signing_key_from_seed(&id.ed25519_seed())
            .unwrap()
            .verifying_key()
            .to_bytes()
            .to_vec()
    }

    #[test]
    fn supersession_proof_roundtrips_through_ffi() {
        let old = Identity::new("super-old".into()).unwrap();
        let new = Identity::new("super-new".into()).unwrap();
        let old_pub = ed25519_pub_of(&old);
        let new_pub = ed25519_pub_of(&new);
        let now = 1_000_000_i64;
        let proof =
            sign_supersession_proof(old.ed25519_seed(), new.ed25519_seed(), now, now + 3600)
                .unwrap();
        let authorised = verify_supersession(proof, old_pub, now + 10).unwrap();
        assert_eq!(authorised, new_pub);
    }

    #[test]
    fn supersession_proof_rejected_under_wrong_old_key() {
        let old = Identity::new("super-old-2".into()).unwrap();
        let new = Identity::new("super-new-2".into()).unwrap();
        let wrong_pub = ed25519_pub_of(&Identity::new("super-wrong".into()).unwrap());
        let now = 2_000_000_i64;
        let proof =
            sign_supersession_proof(old.ed25519_seed(), new.ed25519_seed(), now, now + 3600)
                .unwrap();
        assert!(verify_supersession(proof, wrong_pub, now + 10).is_err());
    }

    #[test]
    fn supersession_proof_rejected_when_expired() {
        let old = Identity::new("super-old-3".into()).unwrap();
        let new = Identity::new("super-new-3".into()).unwrap();
        let old_pub = ed25519_pub_of(&old);
        let now = 3_000_000_i64;
        let proof =
            sign_supersession_proof(old.ed25519_seed(), new.ed25519_seed(), now, now + 60).unwrap();
        assert!(verify_supersession(proof, old_pub, now + 1000).is_err());
    }

    #[test]
    fn supersession_sign_rejects_bad_seed_length() {
        let new = Identity::new("super-new-4".into()).unwrap();
        assert!(sign_supersession_proof(vec![0u8; 31], new.ed25519_seed(), 0, 60).is_err());
    }

    // -- K-of-N witness quorum: verify_witness_quorum (ADR-0039, G-30 finding-1) --

    #[test]
    fn witness_quorum_k_of_n_through_ffi() {
        let tree_root = vec![0xABu8; 32];
        let tree_root_arr = [0xABu8; 32];
        let log_size = 7u64;
        let sks: Vec<lakko_crypto::signing::SigningKey> = [1u8, 2, 3]
            .iter()
            .map(|s| lakko_crypto::signing::SigningKey::from_seed(&[*s; 32]))
            .collect();
        let pinned: Vec<Vec<u8>> = sks
            .iter()
            .map(|sk| sk.verifying_key().to_bytes().to_vec())
            .collect();
        let all_cosigs: Vec<Vec<u8>> = sks
            .iter()
            .map(|sk| lakko_transparency::sign_witness_cosig(sk, &tree_root_arr, log_size).to_vec())
            .collect();

        // Three distinct valid co-sigs satisfy both a 3-of-3 and a 1-of-3 quorum.
        verify_witness_quorum(
            all_cosigs.clone(),
            pinned.clone(),
            3,
            tree_root.clone(),
            log_size,
        )
        .expect("3-of-3 met");
        verify_witness_quorum(
            all_cosigs.clone(),
            pinned.clone(),
            1,
            tree_root.clone(),
            log_size,
        )
        .expect("1-of-3 met");

        // Only two witnesses actually co-signed (K-1) → a threshold of 3 is unmet.
        let two_cosigs = all_cosigs[..2].to_vec();
        assert!(
            verify_witness_quorum(two_cosigs, pinned.clone(), 3, tree_root.clone(), log_size)
                .is_err()
        );

        // Zero threshold is rejected fail-closed even with valid co-sigs present.
        assert!(verify_witness_quorum(
            all_cosigs.clone(),
            pinned.clone(),
            0,
            tree_root.clone(),
            log_size
        )
        .is_err());

        // A short (31-byte) expected_tree_root is refused before any check.
        assert!(verify_witness_quorum(all_cosigs, pinned, 1, vec![0u8; 31], log_size).is_err());
    }

    // -- KT-log S3b: verify_log_inclusion --

    /// Decomposed FFI fields for one entry's inclusion call, mirroring the
    /// JSON a client decodes from `/transparency/{root,latest,proof}`.
    struct DecomposedInclusion {
        operator_pubkey: Vec<u8>,
        tree_root: Vec<u8>,
        log_size: u64,
        timestamp_unix: u64,
        root_signature: Vec<u8>,
        leaf_index: u64,
        audit_path: Vec<u8>,
        username_hash: Vec<u8>,
        identity_pubkey: Vec<u8>,
        version: u64,
    }

    /// Build a 4-entry in-memory log, sign the current root, and decompose
    /// the proof for `idx` into the FFI field shape.
    fn build_inclusion_fixture(idx: u64) -> DecomposedInclusion {
        use lakko_transparency::{InMemoryLog, LogEntry};
        let operator = lakko_crypto::signing::SigningKey::generate();
        let mut log = InMemoryLog::new(operator);
        for i in 0..4u64 {
            log.append(LogEntry {
                username_hash: lakko_crypto::hash::blake3(format!("u{i}").as_bytes()),
                identity_pubkey: vec![0x10 + u8::try_from(i).unwrap(); 32],
                version: 1,
            });
        }
        let signed = log.sign_current_root(1_700_000_000);
        let proof = log.prove_inclusion(idx).expect("proof");
        let entry = log.entry(idx).expect("entry").clone();

        let mut audit_path = Vec::new();
        for h in &proof.audit_path {
            audit_path.extend_from_slice(h);
        }

        DecomposedInclusion {
            operator_pubkey: log.operator_key().to_bytes().to_vec(),
            tree_root: signed.tree_root.to_vec(),
            log_size: signed.log_size,
            timestamp_unix: signed.timestamp_unix,
            root_signature: signed.signature.to_bytes().to_vec(),
            leaf_index: proof.leaf_index,
            audit_path,
            username_hash: entry.username_hash.to_vec(),
            identity_pubkey: entry.identity_pubkey,
            version: entry.version,
        }
    }

    fn call_inclusion(d: &DecomposedInclusion) -> Result<(), FfiError> {
        verify_log_inclusion(
            d.operator_pubkey.clone(),
            d.tree_root.clone(),
            d.log_size,
            d.timestamp_unix,
            d.root_signature.clone(),
            d.leaf_index,
            d.audit_path.clone(),
            d.username_hash.clone(),
            d.identity_pubkey.clone(),
            d.version,
        )
    }

    #[test]
    fn log_inclusion_valid_passes_both_gates() {
        for idx in 0..4 {
            let d = build_inclusion_fixture(idx);
            call_inclusion(&d).expect("valid inclusion verifies");
        }
    }

    #[test]
    fn log_inclusion_flipped_root_signature_fails() {
        let mut d = build_inclusion_fixture(2);
        d.root_signature[0] ^= 0x01;
        assert!(call_inclusion(&d).is_err());
    }

    #[test]
    fn log_inclusion_flipped_audit_path_fails() {
        let mut d = build_inclusion_fixture(2);
        assert!(!d.audit_path.is_empty());
        d.audit_path[0] ^= 0x01;
        assert!(call_inclusion(&d).is_err());
    }

    #[test]
    fn log_inclusion_wrong_operator_key_fails() {
        let mut d = build_inclusion_fixture(1);
        d.operator_pubkey = lakko_crypto::signing::SigningKey::generate()
            .verifying_key()
            .to_bytes()
            .to_vec();
        assert!(call_inclusion(&d).is_err());
    }

    #[test]
    fn log_inclusion_wrong_identity_pubkey_fails() {
        let mut d = build_inclusion_fixture(1);
        d.identity_pubkey = vec![0xFF; 32];
        assert!(call_inclusion(&d).is_err());
    }

    #[test]
    fn log_inclusion_wrong_version_fails() {
        let mut d = build_inclusion_fixture(1);
        d.version = d.version.wrapping_add(1);
        assert!(call_inclusion(&d).is_err());
    }

    #[test]
    fn log_inclusion_malformed_lengths_fail_fast() {
        let base = build_inclusion_fixture(0);

        let mut d = build_inclusion_fixture(0);
        d.tree_root.pop();
        assert!(call_inclusion(&d).is_err(), "31-byte tree_root rejected");

        let mut d = build_inclusion_fixture(0);
        d.root_signature.push(0x00);
        assert!(call_inclusion(&d).is_err(), "65-byte signature rejected");

        let mut d = build_inclusion_fixture(2);
        d.audit_path.push(0x00);
        assert!(
            call_inclusion(&d).is_err(),
            "non-multiple-of-32 audit path rejected"
        );

        let mut d = build_inclusion_fixture(0);
        d.operator_pubkey.clear();
        assert!(call_inclusion(&d).is_err(), "empty operator key rejected");

        let mut d = build_inclusion_fixture(0);
        d.username_hash.pop();
        assert!(
            call_inclusion(&d).is_err(),
            "31-byte username_hash rejected"
        );

        call_inclusion(&base).expect("base fixture still valid");
    }

    // -- G-30 #2 / ADR-0019: verify_supersession_chain --

    /// Decomposed FFI fields for a rotation-chain call: a `(v1 old key) ->
    /// (v2 new key)` log plus the supersession proof + the pin.
    struct DecomposedChain {
        operator_pubkey: Vec<u8>,
        new_tree_root: Vec<u8>,
        new_log_size: u64,
        new_timestamp_unix: u64,
        new_root_signature: Vec<u8>,
        entry_username_hash: Vec<u8>,
        entry_identity_pubkey: Vec<u8>,
        entry_version: u64,
        inclusion_leaf_index: u64,
        inclusion_audit_path: Vec<u8>,
        supersession_proof: Vec<u8>,
        pinned_old_pubkey: Vec<u8>,
        pinned_version: u64,
        last_seen_log_size: u64,
        now_unix_secs: i64,
        new_pubkey: Vec<u8>,
    }

    /// Build a 2-entry log for `alice` — v1 under `old`, v2 under `new` — sign
    /// the root, mint an old->new supersession proof, and decompose into the
    /// chain FFI field shape pinned at v1.
    fn build_chain_fixture() -> DecomposedChain {
        use lakko_transparency::{InMemoryLog, LogEntry, SupersessionProof};
        let operator = lakko_crypto::signing::SigningKey::generate();
        let old_sk = lakko_crypto::signing::SigningKey::generate();
        let new_sk = lakko_crypto::signing::SigningKey::generate();
        let account_key = lakko_crypto::hash::blake3(b"alice");

        let mut log = InMemoryLog::new(operator);
        log.append(LogEntry {
            username_hash: account_key,
            identity_pubkey: old_sk.verifying_key().to_bytes().to_vec(),
            version: 1,
        });
        let v2_idx = log.append(LogEntry {
            username_hash: account_key,
            identity_pubkey: new_sk.verifying_key().to_bytes().to_vec(),
            version: 2,
        });
        let signed = log.sign_current_root(1_700_000_000);
        let proof = log.prove_inclusion(v2_idx).expect("proof");
        let entry = log.entry(v2_idx).expect("entry").clone();

        let issued = 1_700_000_000_i64;
        let expires = issued + 1000;
        let supersession_proof = SupersessionProof::new(
            old_sk.verifying_key().to_bytes(),
            new_sk.verifying_key().to_bytes(),
            issued,
            expires,
        )
        .sign(&old_sk)
        .to_vec();

        let mut audit_path = Vec::new();
        for h in &proof.audit_path {
            audit_path.extend_from_slice(h);
        }

        DecomposedChain {
            operator_pubkey: log.operator_key().to_bytes().to_vec(),
            new_tree_root: signed.tree_root.to_vec(),
            new_log_size: signed.log_size,
            new_timestamp_unix: signed.timestamp_unix,
            new_root_signature: signed.signature.to_bytes().to_vec(),
            entry_username_hash: entry.username_hash.to_vec(),
            entry_identity_pubkey: entry.identity_pubkey,
            entry_version: entry.version,
            inclusion_leaf_index: proof.leaf_index,
            inclusion_audit_path: audit_path,
            supersession_proof,
            pinned_old_pubkey: old_sk.verifying_key().to_bytes().to_vec(),
            pinned_version: 1,
            last_seen_log_size: 1,
            now_unix_secs: issued + 10,
            new_pubkey: new_sk.verifying_key().to_bytes().to_vec(),
        }
    }

    fn call_chain(d: &DecomposedChain) -> Result<Vec<u8>, FfiError> {
        verify_supersession_chain(
            d.operator_pubkey.clone(),
            d.new_tree_root.clone(),
            d.new_log_size,
            d.new_timestamp_unix,
            d.new_root_signature.clone(),
            d.entry_username_hash.clone(),
            d.entry_identity_pubkey.clone(),
            d.entry_version,
            d.inclusion_leaf_index,
            d.inclusion_audit_path.clone(),
            d.supersession_proof.clone(),
            d.pinned_old_pubkey.clone(),
            d.pinned_version,
            d.last_seen_log_size,
            d.now_unix_secs,
        )
    }

    #[test]
    fn chain_valid_rotation_returns_authorised_new_key() {
        let d = build_chain_fixture();
        let got = call_chain(&d).expect("valid rotation chain verifies");
        assert_eq!(got, d.new_pubkey, "re-pin to the authorised new key");
    }

    #[test]
    fn chain_forged_root_signature_fails() {
        let mut d = build_chain_fixture();
        d.new_root_signature[0] ^= 0x01;
        assert!(call_chain(&d).is_err(), "forged root fails gate 1");
    }

    #[test]
    fn chain_wrong_pinned_old_key_fails() {
        let mut d = build_chain_fixture();
        d.pinned_old_pubkey = lakko_crypto::signing::SigningKey::generate()
            .verifying_key()
            .to_bytes()
            .to_vec();
        assert!(
            call_chain(&d).is_err(),
            "a proof not signed by the pinned old key fails"
        );
    }

    #[test]
    fn chain_non_newer_version_fails() {
        let mut d = build_chain_fixture();
        d.pinned_version = 2; // entry is also v2 → not strictly newer
        assert!(call_chain(&d).is_err(), "non-newer version is a downgrade");
    }

    #[test]
    fn chain_rollback_fails() {
        let mut d = build_chain_fixture();
        d.last_seen_log_size = d.new_log_size + 1; // shrinking log
        assert!(
            call_chain(&d).is_err(),
            "a rollback fails the no-rollback gate"
        );
    }

    #[test]
    fn chain_expired_proof_fails() {
        let mut d = build_chain_fixture();
        d.now_unix_secs = 1_700_000_000 + 5000; // past the 1000s window
        assert!(call_chain(&d).is_err(), "an expired proof fails");
    }

    #[test]
    fn chain_malformed_lengths_fail_fast() {
        let mut d = build_chain_fixture();
        d.new_tree_root.pop();
        assert!(call_chain(&d).is_err(), "31-byte tree_root rejected");

        let mut d = build_chain_fixture();
        d.pinned_old_pubkey.pop();
        assert!(call_chain(&d).is_err(), "31-byte pinned key rejected");

        let mut d = build_chain_fixture();
        d.inclusion_audit_path.push(0x00);
        assert!(
            call_chain(&d).is_err(),
            "non-multiple-of-32 audit path rejected"
        );
    }

    // -- G-30 #2 / ADR-0019: sign/verify_leaf_genesis --

    #[test]
    fn leaf_genesis_sign_verify_roundtrips() {
        let id = Identity::new("genesis-1".into()).unwrap();
        let username_hash = vec![0xAB; 32];
        let pubkey = ed25519_pub_of(&id);
        let sig = sign_leaf_genesis(id.ed25519_seed(), username_hash.clone()).unwrap();
        verify_leaf_genesis(sig, username_hash, pubkey).expect("genesis self-sig verifies");
    }

    #[test]
    fn leaf_genesis_wrong_account_key_fails() {
        let id = Identity::new("genesis-2".into()).unwrap();
        let pubkey = ed25519_pub_of(&id);
        let sig = sign_leaf_genesis(id.ed25519_seed(), vec![0xCD; 32]).unwrap();
        // A different account_key → the signed binding differs → fail.
        assert!(verify_leaf_genesis(sig, vec![0x01; 32], pubkey).is_err());
    }

    #[test]
    fn leaf_genesis_wrong_pubkey_fails() {
        let id = Identity::new("genesis-3".into()).unwrap();
        let other = Identity::new("genesis-3b".into()).unwrap();
        let username_hash = vec![0xEF; 32];
        let sig = sign_leaf_genesis(id.ed25519_seed(), username_hash.clone()).unwrap();
        assert!(verify_leaf_genesis(sig, username_hash, ed25519_pub_of(&other)).is_err());
    }

    #[test]
    fn leaf_genesis_sign_rejects_bad_seed() {
        assert!(sign_leaf_genesis(vec![0u8; 31], vec![0u8; 32]).is_err());
    }

    // -- ADR-0036: verify_genesis_attestation / account_username_hash --

    fn signed_genesis_attestation(
        signing: &lakko_crypto::signing::SigningKey,
        account: &[u8; 16],
        username_hash: &[u8; 32],
        pubkey: &[u8; 32],
    ) -> Vec<u8> {
        // Expiry is ignored by the binding (client) path; use the far future.
        lakko_attest::GenesisAttestation::new(*account, *username_hash, *pubkey, i64::MAX)
            .sign(signing)
            .to_vec()
    }

    #[test]
    fn genesis_attestation_verify_roundtrips() {
        let signing = lakko_crypto::signing::SigningKey::from_seed(&[0x11; 32]);
        let verifying = signing.verifying_key().to_bytes().to_vec();
        let account = [0xAA; 16];
        let username_hash = [0xBB; 32];
        let pubkey = [0xCC; 32];
        let wire = signed_genesis_attestation(&signing, &account, &username_hash, &pubkey);
        verify_genesis_attestation(
            wire,
            verifying,
            account.to_vec(),
            username_hash.to_vec(),
            pubkey.to_vec(),
        )
        .expect("genesis attestation verifies");
    }

    #[test]
    fn genesis_attestation_wrong_pubkey_fails() {
        let signing = lakko_crypto::signing::SigningKey::from_seed(&[0x11; 32]);
        let verifying = signing.verifying_key().to_bytes().to_vec();
        let account = [0xAA; 16];
        let username_hash = [0xBB; 32];
        let pubkey = [0xCC; 32];
        let wire = signed_genesis_attestation(&signing, &account, &username_hash, &pubkey);
        // A different expected pubkey → the binding check fails.
        assert!(verify_genesis_attestation(
            wire,
            verifying,
            account.to_vec(),
            username_hash.to_vec(),
            vec![0x00; 32],
        )
        .is_err());
    }

    #[test]
    fn genesis_attestation_wrong_verifying_key_fails() {
        let signing = lakko_crypto::signing::SigningKey::from_seed(&[0x11; 32]);
        let account = [0xAA; 16];
        let username_hash = [0xBB; 32];
        let pubkey = [0xCC; 32];
        let wire = signed_genesis_attestation(&signing, &account, &username_hash, &pubkey);
        let other = lakko_crypto::signing::SigningKey::from_seed(&[0x22; 32])
            .verifying_key()
            .to_bytes()
            .to_vec();
        // Signed by another key → signature does not verify under the pinned key.
        assert!(verify_genesis_attestation(
            wire,
            other,
            account.to_vec(),
            username_hash.to_vec(),
            pubkey.to_vec(),
        )
        .is_err());
    }

    #[test]
    fn genesis_attestation_rejects_malformed_and_bad_lengths() {
        let verifying = lakko_crypto::signing::SigningKey::from_seed(&[0x11; 32])
            .verifying_key()
            .to_bytes()
            .to_vec();
        // Too-short attestation blob.
        assert!(verify_genesis_attestation(
            vec![0u8; 10],
            verifying.clone(),
            vec![0xAA; 16],
            vec![0xBB; 32],
            vec![0xCC; 32],
        )
        .is_err());
        // Wrong-length expected fields.
        assert!(verify_genesis_attestation(
            vec![0u8; 153],
            verifying,
            vec![0xAA; 15],
            vec![0xBB; 32],
            vec![0xCC; 32],
        )
        .is_err());
    }

    #[test]
    fn account_username_hash_matches_transparency() {
        let account_id = vec![0x07; 16];
        assert_eq!(
            account_username_hash(account_id.clone()),
            lakko_transparency::account_username_hash(&account_id).to_vec()
        );
    }
}
