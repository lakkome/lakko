// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for ADR-0091 phase 2: a device that fell behind a
//! conversation notices it and comes back.
//!
//! - The shared rules (M4): `conversationBehindVerdict`,
//!   `reentryRequestDue`, `reentryRoute`, `reentryAdmissionVerdict` and their
//!   constants, so no client hand-writes them.
//! - The request envelope (`lakko_contacts::conversation_reentry`) for the
//!   three routes, and its knock-row seal and open (route 3).
//! - The group's knock public key handed to members
//!   (`lakko_contacts::knock_key`).
//!
//! The MLS half (`Identity.createReentryKeyPackage`,
//! `Group.reentryRequestTarget`, `Group.proposeReentryReplacement`,
//! `Group.joinReentry`) is on the objects in `lib.rs`. Mirror of
//! `crates/lakko-ffi-wasm/src/conversation_reentry.rs`.

use lakko_contacts::conversation_reentry::{
    compose_conversation_reentry as rust_compose, decode_conversation_reentry as rust_decode,
    open_reentry_knock as rust_open_knock, seal_reentry_knock as rust_seal_knock,
    ConversationReentry, ConversationReentryError, CONVERSATION_REENTRY_ENVELOPE_MAX_LEN,
    CONVERSATION_REENTRY_ENVELOPE_MIN_LEN,
};
use lakko_contacts::gap_history::{
    gap_history_rendezvous as rust_gap_rendezvous, gap_history_start as rust_gap_start,
    open_gap_history as rust_open_gap, seal_gap_history as rust_seal_gap, GapHistoryError, GapKey,
    OpenedGapHistory, GAP_HISTORY_MAX_BUNDLE_LEN,
};
use lakko_contacts::knock_key::{
    compose_knock_key as rust_compose_knock_key, decode_knock_key as rust_decode_knock_key,
    knock_key_is_newer as rust_knock_key_is_newer, KnockKey, KnockKeyError, KNOCK_KEY_ENVELOPE_LEN,
};
use lakko_crypto::signing::SigningKey;

use crate::{FfiError, GroupKind, Identity, StructuredHistoryBundle, StructuredHistoryGroup};

fn reentry_error(e: ConversationReentryError) -> FfiError {
    FfiError::Contacts(format!("conversationReentry.{}: {e}", e.code()))
}

fn knock_key_error(e: KnockKeyError) -> FfiError {
    FfiError::Contacts(format!("knockKey.{}: {e}", e.code()))
}

fn gap_error(e: GapHistoryError) -> FfiError {
    FfiError::Contacts(format!("gapHistory.{}: {e}", e.code()))
}

fn key32(what: &str, bytes: &[u8]) -> Result<[u8; 32], FfiError> {
    <[u8; 32]>::try_from(bytes).map_err(|_| {
        FfiError::Contacts(format!(
            "badLength: {what} must be 32 bytes, got {}",
            bytes.len()
        ))
    })
}

fn len_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

// ============================================================================
// The shared rules (M4)
// ============================================================================

/// Where a device stands in one conversation (ADR-0091 section 1).
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiBehindState {
    /// Nothing suggests the device is behind.
    InStep,
    /// The device stops acting in the conversation and asks to come back.
    Suspected {
        /// When the suspicion began (ms since the epoch).
        since_ms: u64,
    },
    /// Shown: the composer is replaced by the waiting note.
    Confirmed {
        /// When the suspicion began (ms since the epoch).
        since_ms: u64,
    },
}

impl From<FfiBehindState> for lakko_mls::BehindState {
    fn from(s: FfiBehindState) -> Self {
        match s {
            FfiBehindState::InStep => Self::InStep,
            FfiBehindState::Suspected { since_ms } => Self::Suspected { since_ms },
            FfiBehindState::Confirmed { since_ms } => Self::Confirmed { since_ms },
        }
    }
}

impl From<lakko_mls::BehindState> for FfiBehindState {
    fn from(s: lakko_mls::BehindState) -> Self {
        match s {
            lakko_mls::BehindState::InStep => Self::InStep,
            lakko_mls::BehindState::Suspected { since_ms } => Self::Suspected { since_ms },
            lakko_mls::BehindState::Confirmed { since_ms } => Self::Confirmed { since_ms },
        }
    }
}

/// What one read of a conversation journal showed (ADR-0091 section 1).
#[allow(clippy::struct_excessive_bools)]
#[derive(uniffi::Record, Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfiBehindObservation {
    /// An ADR-0040 outbox entry or a staged sweep awaits a verdict.
    pub own_commit_owed: bool,
    /// The read returned no row and no "more", and the cursor was not held.
    pub drained: bool,
    /// The DS's `latest_epoch` from this read, if it named one.
    pub latest_epoch: Option<u64>,
    /// The group's own epoch.
    pub local_epoch: u64,
    /// Consecutive `401` on reads signed at the current key, clock not
    /// skewed.
    pub refused_reads: u32,
    /// Since the last applied row, a row framed above `local_epoch` failed.
    pub failed_row_above_local: bool,
    /// A publish to this journal was refused `401`, clock not skewed.
    pub publish_refused: bool,
}

impl From<FfiBehindObservation> for lakko_mls::BehindObservation {
    fn from(o: FfiBehindObservation) -> Self {
        Self {
            own_commit_owed: o.own_commit_owed,
            drained: o.drained,
            latest_epoch: o.latest_epoch,
            local_epoch: o.local_epoch,
            refused_reads: o.refused_reads,
            failed_row_above_local: o.failed_row_above_local,
            publish_refused: o.publish_refused,
        }
    }
}

/// The next state after one observation (ADR-0091 section 1): suspected,
/// confirmed with evidence or after 10 minutes, cleared by a drained read
/// at or below the local epoch. An observation that neither suspects nor
/// clears keeps `previous`.
#[uniffi::export]
#[must_use]
pub fn conversation_behind_verdict(
    previous: FfiBehindState,
    observation: FfiBehindObservation,
    now_ms: u64,
) -> FfiBehindState {
    lakko_mls::conversation_behind_verdict(previous.into(), observation.into(), now_ms).into()
}

/// `true` when the device should send (or repeat) its request to come back:
/// as soon as it is behind; 2 minutes after a request that went to another
/// own device (`lastRouteWasOwnDevice`), then no more often than every 30
/// minutes.
#[uniffi::export]
#[must_use]
pub fn reentry_request_due(
    state: FfiBehindState,
    last_sent_ms: Option<u64>,
    last_route_was_own_device: bool,
    now_ms: u64,
) -> bool {
    lakko_mls::reentry_request_due(
        state.into(),
        last_sent_ms,
        last_route_was_own_device,
        now_ms,
    )
}

/// `true` when the device stops committing, sweeping, registering, proposing
/// and sending repeat envelopes in the conversation (C-93).
#[uniffi::export]
#[must_use]
pub fn behind_state_stops_acting(state: FfiBehindState) -> bool {
    lakko_mls::BehindState::from(state).stops_acting()
}

/// The route a request to come back takes (ADR-0091 section 3).
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiReentryRoute {
    /// A DeviceGroup service envelope to another device of the account.
    OwnDevice,
    /// A sealed mailbox row (`MailboxKind.conversationReentry`) to the other
    /// person of a one-to-one.
    Mailbox,
    /// A sealed knock row to the group's knock key.
    Knock,
}

/// Which route applies: another own device in the conversation first, then
/// a one-to-one's mailbox, then the group's knock queue.
/// `ownDeviceUnanswered` is `true` once a request of the current behind
/// episode went by `OwnDevice` and the device is still behind: every later
/// request of the episode takes the conversation's own route.
#[uniffi::export]
#[must_use]
pub fn reentry_route(
    own_device_in_conversation: bool,
    kind: GroupKind,
    own_device_unanswered: bool,
) -> FfiReentryRoute {
    match lakko_mls::reentry_route(
        own_device_in_conversation,
        kind.into(),
        own_device_unanswered,
    ) {
        lakko_mls::ReentryRoute::OwnDevice => FfiReentryRoute::OwnDevice,
        lakko_mls::ReentryRoute::Mailbox => FfiReentryRoute::Mailbox,
        lakko_mls::ReentryRoute::Knock => FfiReentryRoute::Knock,
    }
}

/// What a re-admitter knows about a request beyond what
/// `Group.reentryRequestTarget` checks (ADR-0091 section 4).
#[derive(uniffi::Record, Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfiReentryAdmissionFacts {
    /// The device is still the account's: in this device's own DeviceGroup
    /// roster (route 1), or listed by the account's directory entry and not
    /// tombstoned (routes 2 and 3).
    pub device_still_the_accounts: bool,
    /// When this device last decrypted an application message from the leaf,
    /// sealed at the current epoch, if ever.
    pub last_current_epoch_message_from_leaf_ms: Option<u64>,
    /// This device already handled a re-entry for the leaf at this epoch.
    pub handled_for_leaf_at_this_epoch: bool,
    /// When this device last accepted a re-entry for the leaf, if ever.
    pub last_accepted_for_leaf_ms: Option<u64>,
}

/// A re-admitter's answer to a request (ADR-0091 section 4).
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiReentryAdmission {
    /// Propose the replacement (`Group.proposeReentryReplacement`).
    Admit,
    /// The device is no longer the account's.
    NotTheAccountsDevice,
    /// The leaf wrote at the current epoch within the hour: not behind.
    LeafIsWriting,
    /// A re-entry for this leaf at this epoch was already handled.
    AlreadyHandled,
    /// A re-entry for this leaf was accepted within the hour.
    TooSoon,
}

/// Whether a re-admitter accepts a request whose KeyPackage
/// `Group.reentryRequestTarget` already accepted.
#[uniffi::export]
#[must_use]
pub fn reentry_admission_verdict(
    facts: FfiReentryAdmissionFacts,
    now_ms: u64,
) -> FfiReentryAdmission {
    let facts = lakko_mls::ReentryAdmissionFacts {
        device_still_the_accounts: facts.device_still_the_accounts,
        last_current_epoch_message_from_leaf_ms: facts.last_current_epoch_message_from_leaf_ms,
        handled_for_leaf_at_this_epoch: facts.handled_for_leaf_at_this_epoch,
        last_accepted_for_leaf_ms: facts.last_accepted_for_leaf_ms,
    };
    match lakko_mls::reentry_admission_verdict(facts, now_ms) {
        lakko_mls::ReentryAdmission::Admit => FfiReentryAdmission::Admit,
        lakko_mls::ReentryAdmission::NotTheAccountsDevice => {
            FfiReentryAdmission::NotTheAccountsDevice
        },
        lakko_mls::ReentryAdmission::LeafIsWriting => FfiReentryAdmission::LeafIsWriting,
        lakko_mls::ReentryAdmission::AlreadyHandled => FfiReentryAdmission::AlreadyHandled,
        lakko_mls::ReentryAdmission::TooSoon => FfiReentryAdmission::TooSoon,
    }
}

/// Consecutive refused reads that make a device suspect it is behind (3).
#[uniffi::export]
#[must_use]
pub const fn reentry_refused_reads_to_suspect() -> u32 {
    lakko_mls::REFUSED_READS_TO_SUSPECT
}

/// How long a suspicion holds before it is shown without other evidence
/// (10 minutes). Chosen, not measured.
#[uniffi::export]
#[must_use]
pub const fn reentry_suspicion_confirm_after_ms() -> u64 {
    lakko_mls::SUSPICION_CONFIRM_AFTER_MS
}

/// The re-request interval (30 minutes). Chosen, not measured.
#[uniffi::export]
#[must_use]
pub const fn reentry_request_interval_ms() -> u64 {
    lakko_mls::REENTRY_REQUEST_INTERVAL_MS
}

/// How long a request sent to another own device waits for an answer before
/// the device asks by the conversation's own route (2 minutes). Chosen, not
/// measured.
#[uniffi::export]
#[must_use]
pub const fn reentry_own_device_answer_wait_ms() -> u64 {
    lakko_mls::OWN_DEVICE_ANSWER_WAIT_MS
}

/// How many re-entry requests per conversation keep their KeyPackage
/// private keys (2); forget older ones with `Identity.forgetKeyPackage`.
#[uniffi::export]
#[must_use]
pub fn reentry_requests_kept() -> u32 {
    len_u32(lakko_mls::REENTRY_REQUESTS_KEPT)
}

// ============================================================================
// The request envelope
// ============================================================================

/// A request to be brought back into a conversation.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiConversationReentry {
    /// The 32-byte conversation group id.
    pub group_id: Vec<u8>,
    /// The `latest_epoch` the device read.
    pub epoch_left: u64,
    /// When the device first failed to read a row of the conversation (its
    /// claim; the re-admitter clamps it with `gapHistoryStart`).
    pub gap_from_ms: u64,
    /// The 32-byte public gap key the gap's history is sealed to.
    pub gap_key: Vec<u8>,
    /// The re-entry KeyPackage wire.
    pub key_package: Vec<u8>,
}

impl From<ConversationReentry> for FfiConversationReentry {
    fn from(r: ConversationReentry) -> Self {
        Self {
            group_id: r.group_id.to_vec(),
            epoch_left: r.epoch_left,
            gap_from_ms: r.gap_from_ms,
            gap_key: r.gap_key.to_vec(),
            key_package: r.key_package,
        }
    }
}

/// Compose the request envelope (routes 1 and 2 carry it as is; route 3
/// seals it with `sealReentryKnockRow`). `gap_key` is the public half of the
/// fresh gap key minted for this request (`generateGapKey`); keep its secret
/// with the request record to open the gap's history.
///
/// # Errors
///
/// `FfiError::Contacts` with `conversationReentry.badKeyPackage` or
/// `badLength` (group id, gap key).
#[uniffi::export]
pub fn compose_conversation_reentry_envelope(
    group_id: Vec<u8>,
    epoch_left: u64,
    gap_from_ms: u64,
    gap_key: Vec<u8>,
    key_package: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    rust_compose(&ConversationReentry {
        group_id: key32("group id", &group_id)?,
        epoch_left,
        gap_from_ms,
        gap_key: key32("gap key", &gap_key)?,
        key_package,
    })
    .map_err(reentry_error)
}

// ============================================================================
// The gap's history, sealed to the returning device (section 7, security
// review 2026-11-21)
// ============================================================================

/// A fresh gap key pair. The secret is secret material: keep it with the
/// durable request record, never send it.
#[derive(uniffi::Record, Clone, PartialEq, Eq)]
pub struct FfiGapKey {
    /// The 32-byte X25519 secret.
    pub secret: Vec<u8>,
    /// The 32-byte public key the request carries.
    pub public_key: Vec<u8>,
}

impl core::fmt::Debug for FfiGapKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("FfiGapKey")
            .field("public_key", &self.public_key)
            .finish_non_exhaustive()
    }
}

/// Mint a fresh gap key for one request to come back.
#[uniffi::export]
#[must_use]
pub fn generate_gap_key() -> FfiGapKey {
    let key = GapKey::generate();
    FfiGapKey {
        secret: key.secret().as_bytes().to_vec(),
        public_key: key.public().to_vec(),
    }
}

/// The public key of a stored gap secret.
///
/// # Errors
///
/// `FfiError::Contacts` with `badLength` for a secret that is not 32 bytes.
#[uniffi::export]
pub fn gap_public_key(secret: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    Ok(GapKey::from_secret(&key32("gap secret", &secret)?)
        .public()
        .to_vec())
}

/// The 32-byte DS rendezvous id the gap's history is posted to and read
/// from: derived from the gap public key, no group secret involved.
///
/// # Errors
///
/// `FfiError::Contacts` with `badLength` for a key that is not 32 bytes.
#[uniffi::export]
pub fn gap_history_rendezvous(gap_key: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    Ok(rust_gap_rendezvous(&key32("gap key", &gap_key)?).to_vec())
}

/// Where the re-admitter starts the gap's rows: the later of the request's
/// claim and the first time this device saw the returning leaf in the
/// roster, else its last message from that leaf. `None` when neither is
/// known: send nothing.
#[uniffi::export]
#[must_use]
pub fn gap_history_start(
    claimed_ms: u64,
    leaf_first_seen_ms: Option<u64>,
    leaf_last_write_ms: Option<u64>,
) -> Option<u64> {
    rust_gap_start(claimed_ms, leaf_first_seen_ms, leaf_last_write_ms)
}

/// An opened gap history: the bundle bytes and the 32-byte leaf key that
/// signed them. Keep the bundle only when
/// `Group.gapHistorySignerAccepted(identity, signerLeafKey)` says so.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiOpenedGapHistory {
    /// The leaf signature key the signature verified under.
    pub signer_leaf_key: Vec<u8>,
    /// The bundle bytes.
    pub bundle: Vec<u8>,
}

/// An opened structured gap history and its signer's leaf key.
#[derive(uniffi::Record)]
pub struct FfiOpenedGapHistoryStructured {
    /// The leaf signature key the signature verified under.
    pub signer_leaf_key: Vec<u8>,
    /// The bundle.
    pub bundle: StructuredHistoryBundle,
}

fn signer_of(readmitter: &Identity) -> SigningKey {
    SigningKey::from_seed(&readmitter.inner.ed25519_seed_bytes())
}

/// Seal opaque bundle bytes of the conversation `group_id` to the gap key a
/// request carried, signed by `readmitter`: this device's identity in that
/// conversation (its leaf signing key).
///
/// # Errors
///
/// `FfiError::Contacts` with `gapHistory.badBundle` / `badKey` or
/// `badLength`.
#[uniffi::export]
pub fn seal_gap_history(
    readmitter: &Identity,
    gap_key: Vec<u8>,
    group_id: Vec<u8>,
    bundle: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    rust_seal_gap(
        &signer_of(readmitter),
        &key32("gap key", &gap_key)?,
        &key32("group id", &group_id)?,
        &bundle,
    )
    .map_err(gap_error)
}

fn open_gap(
    gap_secret: &[u8],
    group_id: &[u8],
    sealed: &[u8],
) -> Result<OpenedGapHistory, FfiError> {
    rust_open_gap(
        &key32("gap secret", gap_secret)?,
        &key32("group id", group_id)?,
        sealed,
    )
    .map_err(gap_error)
}

/// Open a sealed gap history with the gap secret, for the conversation
/// `group_id`: verifies the signature, opens the box, and returns the bundle
/// with the signer's leaf key. Keep it only when
/// `Group.gapHistorySignerAccepted(identity, signerLeafKey)` on the group the
/// device joined.
///
/// # Errors
///
/// `FfiError::Contacts` with `gapHistory.badSignature` / `notOpened` /
/// `badLength` / `unknownVersion`.
#[uniffi::export]
pub fn open_gap_history(
    gap_secret: Vec<u8>,
    group_id: Vec<u8>,
    sealed: Vec<u8>,
) -> Result<FfiOpenedGapHistory, FfiError> {
    let opened = open_gap(&gap_secret, &group_id, &sealed)?;
    Ok(FfiOpenedGapHistory {
        signer_leaf_key: opened.signer_leaf_key.to_vec(),
        bundle: opened.bundle,
    })
}

/// `sealGapHistory` over a structured history bundle (the
/// `sealHistoryBundleStructured` records): the bundle is the prost-encoded
/// `HistoryBundle`, sealed to the gap key and signed by `readmitter`. Refuses
/// a bundle above `lakko_mls::history_bundle::MAX_BUNDLE_MESSAGES` messages.
///
/// # Errors
///
/// `FfiError::Codec` for too many messages, `FfiError::Contacts` as
/// `sealGapHistory`.
#[uniffi::export]
pub fn seal_gap_history_structured(
    readmitter: &Identity,
    gap_key: Vec<u8>,
    group_id: Vec<u8>,
    groups: Vec<StructuredHistoryGroup>,
    max_hlc: u64,
) -> Result<Vec<u8>, FfiError> {
    let bundle =
        crate::encode_history_bundle_checked(crate::build_history_bundle_proto(groups, max_hlc))?;
    seal_gap_history(readmitter, gap_key, group_id, bundle)
}

/// Open a structured gap history (`sealGapHistoryStructured`): the bundle
/// and its signer's leaf key. Keep it only when
/// `Group.gapHistorySignerAccepted`.
///
/// # Errors
///
/// As `openGapHistory`; `FfiError::Codec` when the bundle does not decode.
#[uniffi::export]
pub fn open_gap_history_structured(
    gap_secret: Vec<u8>,
    group_id: Vec<u8>,
    sealed: Vec<u8>,
) -> Result<FfiOpenedGapHistoryStructured, FfiError> {
    let opened = open_gap(&gap_secret, &group_id, &sealed)?;
    Ok(FfiOpenedGapHistoryStructured {
        signer_leaf_key: opened.signer_leaf_key.to_vec(),
        bundle: crate::structured_history_bundle_from_bytes(&opened.bundle)?,
    })
}

/// The longest bundle `sealGapHistory` takes.
#[uniffi::export]
#[must_use]
pub fn gap_history_max_bundle_len() -> u32 {
    len_u32(GAP_HISTORY_MAX_BUNDLE_LEN)
}

/// Decode a request envelope (bounded, fail-closed).
///
/// # Errors
///
/// `FfiError::Contacts` with a `conversationReentry.<code>` reason.
#[uniffi::export]
pub fn decode_conversation_reentry_envelope(
    plaintext: Vec<u8>,
) -> Result<FfiConversationReentry, FfiError> {
    rust_decode(&plaintext)
        .map(Into::into)
        .map_err(reentry_error)
}

/// Seal a request envelope as a knock row for the group whose knock key of
/// `knock_version` is `knock_pk` (route 3). Same size and first bytes as a
/// join request.
///
/// # Errors
///
/// `FfiError::Contacts` with a `conversationReentry.<code>` reason.
#[uniffi::export]
pub fn seal_reentry_knock_row(
    knock_pk: Vec<u8>,
    knock_version: u16,
    envelope: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    rust_seal_knock(&key32("knock key", &knock_pk)?, knock_version, &envelope)
        .map_err(reentry_error)
}

/// Open a re-entry knock row with the knock seed of the version it names
/// (`knockRowVersion`). A join request does not open here
/// (`conversationReentry.notOpened`): try `openKnockRow` first.
///
/// # Errors
///
/// `FfiError::Contacts` with a `conversationReentry.<code>` reason.
#[uniffi::export]
pub fn open_reentry_knock_row(
    knock_seed: Vec<u8>,
    row: Vec<u8>,
) -> Result<FfiConversationReentry, FfiError> {
    rust_open_knock(&key32("knock seed", &knock_seed)?, &row)
        .map(Into::into)
        .map_err(reentry_error)
}

/// The shortest well-formed request envelope.
#[uniffi::export]
#[must_use]
pub fn conversation_reentry_envelope_min_len() -> u32 {
    len_u32(CONVERSATION_REENTRY_ENVELOPE_MIN_LEN)
}

/// The longest well-formed request envelope.
#[uniffi::export]
#[must_use]
pub fn conversation_reentry_envelope_max_len() -> u32 {
    len_u32(CONVERSATION_REENTRY_ENVELOPE_MAX_LEN)
}

// ============================================================================
// The knock key handed to members
// ============================================================================

/// A group's knock public key and version.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiKnockKey {
    /// The knock-key version.
    pub knock_version: u16,
    /// The 32-byte X25519 knock public key.
    pub knock_pk: Vec<u8>,
}

/// Compose the knock-key envelope the owner sends in the group, re-sent on
/// a version change and on growth.
///
/// # Errors
///
/// `FfiError::Contacts` with `badLength` when `knock_pk` is not 32 bytes.
#[uniffi::export]
pub fn compose_knock_key_envelope(
    knock_version: u16,
    knock_pk: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    Ok(rust_compose_knock_key(&KnockKey {
        knock_version,
        knock_pk: key32("knock key", &knock_pk)?,
    }))
}

/// Decode a knock-key envelope. Keep it only from the owner or an admin
/// resolved at receipt (C-68), and only when `knockKeyIsNewer`.
///
/// # Errors
///
/// `FfiError::Contacts` with a `knockKey.<code>` reason.
#[uniffi::export]
pub fn decode_knock_key_envelope(plaintext: Vec<u8>) -> Result<FfiKnockKey, FfiError> {
    rust_decode_knock_key(&plaintext)
        .map(|k| FfiKnockKey {
            knock_version: k.knock_version,
            knock_pk: k.knock_pk.to_vec(),
        })
        .map_err(knock_key_error)
}

/// `true` when an incoming knock version replaces the stored one (strictly
/// greater; any version when none is stored).
#[uniffi::export]
#[must_use]
pub fn knock_key_is_newer(incoming_version: u16, stored_version: Option<u16>) -> bool {
    rust_knock_key_is_newer(incoming_version, stored_version)
}

/// The knock-key envelope's one length (60).
#[uniffi::export]
#[must_use]
pub fn knock_key_envelope_len() -> u32 {
    len_u32(KNOCK_KEY_ENVELOPE_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_envelopes_round_trip_through_the_surface() {
        let gap = generate_gap_key();
        let env = compose_conversation_reentry_envelope(
            vec![3; 32],
            9,
            1000,
            gap.public_key.clone(),
            vec![1, 2, 3],
        )
        .expect("compose");
        let back = decode_conversation_reentry_envelope(env.clone()).expect("decode");
        assert_eq!(back.group_id, vec![3; 32]);
        assert_eq!(back.epoch_left, 9);
        assert_eq!(back.gap_key, gap.public_key);
        assert_eq!(back.key_package, vec![1, 2, 3]);
        let seed = lakko_contacts::knock::knock_seed(&[5; 32], 2);
        let pk = lakko_contacts::knock::knock_public_key(&seed);
        let row = seal_reentry_knock_row(pk.to_vec(), 2, env).expect("seal");
        assert_eq!(
            open_reentry_knock_row(seed.to_vec(), row).expect("open"),
            back
        );

        let key = compose_knock_key_envelope(4, vec![9; 32]).expect("compose");
        assert_eq!(
            u32::try_from(key.len()).ok(),
            Some(knock_key_envelope_len())
        );
        let k = decode_knock_key_envelope(key).expect("decode");
        assert_eq!(k.knock_version, 4);
        assert!(knock_key_is_newer(5, Some(4)));
        assert!(!knock_key_is_newer(4, Some(4)));
    }

    /// The gap's history through the surface: sealed to the request's gap
    /// key, opened only with its secret and only for its conversation; the
    /// rendezvous comes from the gap key; the start is clamped.
    #[test]
    fn the_gap_history_is_sealed_to_the_returning_device_only() {
        let gap = generate_gap_key();
        assert_eq!(
            gap_public_key(gap.secret.clone()).expect("public"),
            gap.public_key
        );
        let groups = vec![StructuredHistoryGroup {
            group_id: vec![3; 32],
            messages: vec![crate::StructuredHistoryMessage {
                payload_bytes: vec![1, 2],
                sender_leaf: 4,
                message_id: vec![9],
                received_at_ms: 5,
            }],
            count_capped: false,
        }];
        let readmitter = Identity::from_seed(vec![0x71; 64]).expect("readmitter");
        let impostor = Identity::from_seed(vec![0x72; 64]).expect("impostor");
        let sealed = seal_gap_history_structured(
            &readmitter,
            gap.public_key.clone(),
            vec![3; 32],
            groups,
            7,
        )
        .expect("seal");
        let opened = open_gap_history_structured(gap.secret.clone(), vec![3; 32], sealed.clone())
            .expect("open");
        assert_eq!(opened.bundle.max_hlc, 7);
        assert_eq!(
            opened.bundle.groups[0].messages[0].payload_bytes,
            vec![1, 2]
        );
        let readmitter_leaf = readmitter.inner.signature_public_key().to_vec();
        assert_eq!(opened.signer_leaf_key, readmitter_leaf);
        // The signer is inside the box: not visible on the rendezvous.
        assert!(!sealed.windows(32).any(|w| w == readmitter_leaf.as_slice()));
        // A member who saw the request signs invented messages: it opens and
        // names that member; `Group.gapHistorySignerAccepted` judges it.
        let forged = seal_gap_history(
            &impostor,
            gap.public_key.clone(),
            vec![3; 32],
            b"invented".to_vec(),
        )
        .expect("seal");
        let opened = open_gap_history(gap.secret.clone(), vec![3; 32], forged).expect("open");
        assert_eq!(
            opened.signer_leaf_key,
            impostor.inner.signature_public_key().to_vec()
        );
        let other = generate_gap_key();
        assert!(open_gap_history(other.secret, vec![3; 32], sealed.clone()).is_err());
        assert!(open_gap_history(gap.secret, vec![4; 32], sealed).is_err());
        let r = gap_history_rendezvous(gap.public_key.clone()).expect("rendezvous");
        assert_eq!(r.len(), 32);
        assert_ne!(r, gap_history_rendezvous(other.public_key).expect("r"));
        assert_eq!(gap_history_start(100, Some(500), None), Some(500));
        assert_eq!(gap_history_start(100, None, None), None);
    }

    /// The MLS half end to end through the surface: charlie misses a
    /// commit, mints K, alice proposes the replacement and sweeps, bob
    /// applies it, charlie joins over the stale group and reads the next
    /// message.
    #[test]
    #[allow(clippy::many_single_char_names)]
    fn a_behind_device_comes_back_through_the_surface() {
        use crate::{Group, Identity};
        let alice = Identity::from_seed(vec![0xA1; 64]).expect("alice");
        let bob = Identity::from_seed(vec![0xB1; 64]).expect("bob");
        let charlie = Identity::from_seed(vec![0xC1; 64]).expect("charlie");
        let dave = Identity::from_seed(vec![0xD1; 64]).expect("dave");
        let a = Group::create(&alice, GroupKind::Group).expect("group");
        let add = a
            .add_members_from_wire(
                &alice,
                vec![
                    bob.create_key_package().expect("kp"),
                    charlie.create_key_package().expect("kp"),
                ],
            )
            .expect("add");
        let b = Group::join_from_welcome(add.welcome.clone(), &bob).expect("bob");
        let c = Group::join_from_welcome(add.welcome, &charlie).expect("charlie");

        let p = a
            .propose_add_member_from_wire(&alice, dave.create_key_package().expect("kp"))
            .expect("propose dave");
        b.process_incoming_verifying_roles(&bob, p)
            .expect("bob stores");
        let swept = a
            .commit_pending_proposals(&alice)
            .expect("sweep")
            .expect("commit");
        b.process_incoming_verifying_roles(&bob, swept.commit)
            .expect("bob applies");
        assert!(c.epoch().expect("epoch") < a.epoch().expect("epoch"));

        let gid = a.group_id().expect("gid");
        let k = charlie
            .create_reentry_key_package(gid, a.epoch().expect("epoch"))
            .expect("K");
        let target = a.reentry_request_target(&alice, k.clone()).expect("target");
        assert_eq!(target.signature_key, charlie.signature_public_key());
        let props = a
            .propose_reentry_replacement(&alice, k.clone())
            .expect("pair");
        b.process_incoming_verifying_roles(&bob, props.add)
            .expect("add");
        b.process_incoming_verifying_roles(&bob, props.remove)
            .expect("remove");
        let swept = a
            .commit_pending_proposals(&alice)
            .expect("sweep")
            .expect("commit");
        b.process_incoming_verifying_roles(&bob, swept.commit)
            .expect("bob applies");

        let wrong = Group::join_reentry(
            swept.welcome.clone().expect("welcome"),
            &charlie,
            vec![charlie.create_key_package().expect("kp")],
        );
        assert!(matches!(
            wrong,
            Err(FfiError::MembershipReentryWelcomeRefused(_))
        ));
        let c2 = Group::join_reentry(swept.welcome.expect("welcome"), &charlie, vec![k])
            .expect("charlie is back");
        assert_eq!(c2.epoch().expect("epoch"), a.epoch().expect("epoch"));
        // On the joined group: the owner (alice) and charlie's own account
        // may sign charlie's gap history; bob, a plain member, may not, so a
        // bundle he signs is dropped.
        let accepted = |key: &[u8]| {
            c2.gap_history_signer_accepted(&charlie, key.to_vec())
                .expect("lock")
        };
        assert!(accepted(alice.inner.signature_public_key()));
        assert!(accepted(charlie.inner.signature_public_key()));
        assert!(!accepted(bob.inner.signature_public_key()));
        let gap = generate_gap_key();
        let forged = seal_gap_history(
            &bob,
            gap.public_key.clone(),
            c2.group_id().expect("id"),
            b"invented".to_vec(),
        )
        .expect("seal");
        let opened =
            open_gap_history(gap.secret, c2.group_id().expect("id"), forged).expect("open");
        assert!(!accepted(&opened.signer_leaf_key));
        let msg = a
            .create_application_message(&alice, b"hi".to_vec())
            .expect("send");
        c2.process_incoming_verifying_roles(&charlie, msg)
            .expect("charlie reads");
    }

    #[test]
    fn the_rules_cross_the_surface() {
        let o = FfiBehindObservation {
            own_commit_owed: false,
            drained: true,
            latest_epoch: Some(16),
            local_epoch: 6,
            refused_reads: 0,
            failed_row_above_local: true,
            publish_refused: false,
        };
        let s = conversation_behind_verdict(FfiBehindState::InStep, o, 7);
        assert_eq!(s, FfiBehindState::Confirmed { since_ms: 7 });
        assert!(behind_state_stops_acting(s));
        assert!(reentry_request_due(s, None, false, 7));
        assert!(reentry_request_due(
            s,
            Some(7),
            true,
            7 + reentry_own_device_answer_wait_ms()
        ));
        assert!(!reentry_request_due(
            s,
            Some(7),
            false,
            7 + reentry_own_device_answer_wait_ms()
        ));
        assert_eq!(
            reentry_route(false, GroupKind::Group, false),
            FfiReentryRoute::Knock
        );
        assert_eq!(
            reentry_route(true, GroupKind::Group, false),
            FfiReentryRoute::OwnDevice
        );
        assert_eq!(
            reentry_route(true, GroupKind::Group, true),
            FfiReentryRoute::Knock
        );
        let facts = FfiReentryAdmissionFacts {
            device_still_the_accounts: true,
            last_current_epoch_message_from_leaf_ms: None,
            handled_for_leaf_at_this_epoch: false,
            last_accepted_for_leaf_ms: None,
        };
        assert_eq!(
            reentry_admission_verdict(facts, 1),
            FfiReentryAdmission::Admit
        );
    }
}
