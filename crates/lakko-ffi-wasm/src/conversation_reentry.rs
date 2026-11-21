// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for ADR-0091 phase 2: a device that fell behind a
//! conversation notices it and comes back. Mirror of
//! `crates/lakko-ffi-uniffi/src/conversation_reentry.rs`.
//!
//! - The shared rules (M4): `conversationBehindVerdict`,
//!   `reentryRequestDue`, `reentryRoute`, `reentryAdmissionVerdict`.
//! - The request envelope, its knock-row seal and open (route 3).
//! - The group's knock public key handed to members.
//!
//! The MLS half (`Identity.createReentryKeyPackage`,
//! `Group.reentryRequestTarget`, `Group.proposeReentryReplacement`,
//! `Group.joinReentry`) is in `mls.rs`.
//!
//! # Errors
//!
//! Every fallible function throws a string `"<area>.<code>: <reason>"`
//! (`conversationReentry.badKeyPackage`, `knockKey.badLength`, ...).

use lakko_contacts::conversation_reentry::{
    compose_conversation_reentry as rust_compose, decode_conversation_reentry as rust_decode,
    open_reentry_knock as rust_open_knock, seal_reentry_knock as rust_seal_knock,
    ConversationReentry as RustReentry, ConversationReentryError,
    CONVERSATION_REENTRY_ENVELOPE_MAX_LEN, CONVERSATION_REENTRY_ENVELOPE_MIN_LEN,
};
use lakko_contacts::gap_history::{
    gap_history_rendezvous as rust_gap_rendezvous, gap_history_start as rust_gap_start,
    open_gap_history as rust_open_gap, seal_gap_history as rust_seal_gap, GapHistoryError,
    GapKey as RustGapKey, OpenedGapHistory as RustOpened, GAP_HISTORY_MAX_BUNDLE_LEN,
};
use lakko_contacts::knock_key::{
    compose_knock_key as rust_compose_knock_key, decode_knock_key as rust_decode_knock_key,
    knock_key_is_newer as rust_knock_key_is_newer, KnockKey as RustKnockKey, KnockKeyError,
    KNOCK_KEY_ENVELOPE_LEN,
};
use lakko_crypto::signing::SigningKey;
use wasm_bindgen::prelude::*;

use crate::mls::{GroupKind, HistoryGroupInput, Identity, OpenedHistoryBundle};

fn reentry_error(e: ConversationReentryError) -> String {
    format!("conversationReentry.{}: {e}", e.code())
}

fn gap_error(e: GapHistoryError) -> String {
    format!("gapHistory.{}: {e}", e.code())
}

fn knock_key_error(e: KnockKeyError) -> String {
    format!("knockKey.{}: {e}", e.code())
}

fn key32(area: &str, what: &str, bytes: &[u8]) -> Result<[u8; 32], String> {
    <[u8; 32]>::try_from(bytes).map_err(|_| {
        format!(
            "{area}.badLength: {what} must be 32 bytes, got {}",
            bytes.len()
        )
    })
}

fn len_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

// ============================================================================
// The shared rules (M4)
// ============================================================================

/// Which of the three states a [`BehindState`] is.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BehindStateKind {
    /// Nothing suggests the device is behind.
    InStep = 0,
    /// The device stops acting and asks to come back.
    Suspected = 1,
    /// Shown: the composer is replaced by the waiting note.
    Confirmed = 2,
}

/// Where a device stands in one conversation (ADR-0091 section 1).
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BehindState {
    kind: BehindStateKind,
    since_ms: u64,
}

#[wasm_bindgen]
impl BehindState {
    /// Build a state. `sinceMs` is ignored for `InStep`.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(kind: BehindStateKind, since_ms: u64) -> Self {
        Self { kind, since_ms }
    }

    /// The state's kind.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn kind(&self) -> BehindStateKind {
        self.kind
    }

    /// When the suspicion began; `undefined` while in step.
    #[wasm_bindgen(getter, js_name = sinceMs)]
    #[must_use]
    pub fn since_ms(&self) -> Option<u64> {
        match self.kind {
            BehindStateKind::InStep => None,
            _ => Some(self.since_ms),
        }
    }

    /// The device stops committing, sweeping, registering, proposing and
    /// sending repeat envelopes in the conversation (C-93).
    #[wasm_bindgen(getter, js_name = stopsActing)]
    #[must_use]
    pub fn stops_acting(&self) -> bool {
        lakko_mls::BehindState::from(*self).stops_acting()
    }
}

impl From<BehindState> for lakko_mls::BehindState {
    fn from(s: BehindState) -> Self {
        match s.kind {
            BehindStateKind::InStep => Self::InStep,
            BehindStateKind::Suspected => Self::Suspected {
                since_ms: s.since_ms,
            },
            BehindStateKind::Confirmed => Self::Confirmed {
                since_ms: s.since_ms,
            },
        }
    }
}

impl From<lakko_mls::BehindState> for BehindState {
    fn from(s: lakko_mls::BehindState) -> Self {
        match s {
            lakko_mls::BehindState::InStep => Self::new(BehindStateKind::InStep, 0),
            lakko_mls::BehindState::Suspected { since_ms } => {
                Self::new(BehindStateKind::Suspected, since_ms)
            },
            lakko_mls::BehindState::Confirmed { since_ms } => {
                Self::new(BehindStateKind::Confirmed, since_ms)
            },
        }
    }
}

/// The next state after one read of a conversation journal (ADR-0091
/// section 1). `latestEpoch` is `undefined` when the read named none.
#[wasm_bindgen(js_name = conversationBehindVerdict)]
#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
#[must_use]
pub fn conversation_behind_verdict(
    previous: &BehindState,
    own_commit_owed: bool,
    drained: bool,
    latest_epoch: Option<u64>,
    local_epoch: u64,
    refused_reads: u32,
    failed_row_above_local: bool,
    publish_refused: bool,
    now_ms: u64,
) -> BehindState {
    let observation = lakko_mls::BehindObservation {
        own_commit_owed,
        drained,
        latest_epoch,
        local_epoch,
        refused_reads,
        failed_row_above_local,
        publish_refused,
    };
    lakko_mls::conversation_behind_verdict((*previous).into(), observation, now_ms).into()
}

/// `true` when the device should send (or repeat) its request to come back:
/// as soon as it is behind; 2 minutes after a request that went to another
/// own device (`lastRouteWasOwnDevice`), then no more often than every 30
/// minutes.
#[wasm_bindgen(js_name = reentryRequestDue)]
#[must_use]
pub fn reentry_request_due(
    state: &BehindState,
    last_sent_ms: Option<u64>,
    last_route_was_own_device: bool,
    now_ms: u64,
) -> bool {
    lakko_mls::reentry_request_due(
        (*state).into(),
        last_sent_ms,
        last_route_was_own_device,
        now_ms,
    )
}

/// The route a request to come back takes (ADR-0091 section 3).
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReentryRoute {
    /// A DeviceGroup service envelope to another device of the account.
    OwnDevice = 1,
    /// A sealed mailbox row (`MailboxKind.ConversationReentry`) to the other
    /// person of a one-to-one.
    Mailbox = 2,
    /// A sealed knock row to the group's knock key.
    Knock = 3,
}

/// Which route applies: another own device in the conversation first, then
/// a one-to-one's mailbox, then the group's knock queue.
/// `ownDeviceUnanswered` is `true` once a request of the current behind
/// episode went by `OwnDevice` and the device is still behind: every later
/// request of the episode takes the conversation's own route.
#[wasm_bindgen(js_name = reentryRoute)]
#[must_use]
pub fn reentry_route(
    own_device_in_conversation: bool,
    kind: GroupKind,
    own_device_unanswered: bool,
) -> ReentryRoute {
    match lakko_mls::reentry_route(
        own_device_in_conversation,
        kind.into(),
        own_device_unanswered,
    ) {
        lakko_mls::ReentryRoute::OwnDevice => ReentryRoute::OwnDevice,
        lakko_mls::ReentryRoute::Mailbox => ReentryRoute::Mailbox,
        lakko_mls::ReentryRoute::Knock => ReentryRoute::Knock,
    }
}

/// A re-admitter's answer to a request (ADR-0091 section 4).
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReentryAdmission {
    /// Propose the replacement (`Group.proposeReentryReplacement`).
    Admit = 0,
    /// The device is no longer the account's.
    NotTheAccountsDevice = 1,
    /// The leaf wrote at the current epoch within the hour.
    LeafIsWriting = 2,
    /// A re-entry for this leaf at this epoch was already handled.
    AlreadyHandled = 3,
    /// A re-entry for this leaf was accepted within the hour.
    TooSoon = 4,
}

/// Whether a re-admitter accepts a request whose KeyPackage
/// `Group.reentryRequestTarget` already accepted.
#[wasm_bindgen(js_name = reentryAdmissionVerdict)]
#[must_use]
pub fn reentry_admission_verdict(
    device_still_the_accounts: bool,
    last_current_epoch_message_from_leaf_ms: Option<u64>,
    handled_for_leaf_at_this_epoch: bool,
    last_accepted_for_leaf_ms: Option<u64>,
    now_ms: u64,
) -> ReentryAdmission {
    let facts = lakko_mls::ReentryAdmissionFacts {
        device_still_the_accounts,
        last_current_epoch_message_from_leaf_ms,
        handled_for_leaf_at_this_epoch,
        last_accepted_for_leaf_ms,
    };
    match lakko_mls::reentry_admission_verdict(facts, now_ms) {
        lakko_mls::ReentryAdmission::Admit => ReentryAdmission::Admit,
        lakko_mls::ReentryAdmission::NotTheAccountsDevice => ReentryAdmission::NotTheAccountsDevice,
        lakko_mls::ReentryAdmission::LeafIsWriting => ReentryAdmission::LeafIsWriting,
        lakko_mls::ReentryAdmission::AlreadyHandled => ReentryAdmission::AlreadyHandled,
        lakko_mls::ReentryAdmission::TooSoon => ReentryAdmission::TooSoon,
    }
}

/// Consecutive refused reads that make a device suspect it is behind.
#[wasm_bindgen(js_name = reentryRefusedReadsToSuspect)]
#[must_use]
pub fn reentry_refused_reads_to_suspect() -> u32 {
    lakko_mls::REFUSED_READS_TO_SUSPECT
}

/// How long a suspicion holds before it is shown without other evidence.
#[wasm_bindgen(js_name = reentrySuspicionConfirmAfterMs)]
#[must_use]
pub fn reentry_suspicion_confirm_after_ms() -> u64 {
    lakko_mls::SUSPICION_CONFIRM_AFTER_MS
}

/// The re-request interval.
#[wasm_bindgen(js_name = reentryRequestIntervalMs)]
#[must_use]
pub fn reentry_request_interval_ms() -> u64 {
    lakko_mls::REENTRY_REQUEST_INTERVAL_MS
}

/// How long a request sent to another own device waits for an answer before
/// the device asks by the conversation's own route. Chosen, not measured.
#[wasm_bindgen(js_name = reentryOwnDeviceAnswerWaitMs)]
#[must_use]
pub fn reentry_own_device_answer_wait_ms() -> u64 {
    lakko_mls::OWN_DEVICE_ANSWER_WAIT_MS
}

/// How many re-entry requests per conversation keep their KeyPackage keys.
#[wasm_bindgen(js_name = reentryRequestsKept)]
#[must_use]
pub fn reentry_requests_kept() -> u32 {
    len_u32(lakko_mls::REENTRY_REQUESTS_KEPT)
}

// ============================================================================
// The request envelope
// ============================================================================

/// A request to be brought back into a conversation.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationReentry {
    inner: RustReentry,
}

#[wasm_bindgen]
impl ConversationReentry {
    /// The 32-byte conversation group id.
    #[wasm_bindgen(getter, js_name = groupId)]
    #[must_use]
    pub fn group_id(&self) -> Vec<u8> {
        self.inner.group_id.to_vec()
    }

    /// The `latest_epoch` the device read.
    #[wasm_bindgen(getter, js_name = epochLeft)]
    #[must_use]
    pub fn epoch_left(&self) -> u64 {
        self.inner.epoch_left
    }

    /// When the device first failed to read a row of the conversation (its
    /// claim; the re-admitter clamps it with `gapHistoryStart`).
    #[wasm_bindgen(getter, js_name = gapFromMs)]
    #[must_use]
    pub fn gap_from_ms(&self) -> u64 {
        self.inner.gap_from_ms
    }

    /// The 32-byte public gap key the gap's history is sealed to.
    #[wasm_bindgen(getter, js_name = gapKey)]
    #[must_use]
    pub fn gap_key(&self) -> Vec<u8> {
        self.inner.gap_key.to_vec()
    }

    /// The re-entry KeyPackage wire.
    #[wasm_bindgen(getter, js_name = keyPackage)]
    #[must_use]
    pub fn key_package(&self) -> Vec<u8> {
        self.inner.key_package.clone()
    }
}

/// Compose the request envelope. `gap_key` is the public half of the fresh
/// gap key minted for this request (`generateGapKey`); keep its secret with
/// the request record to open the gap's history.
///
/// # Errors
///
/// `conversationReentry.badKeyPackage`, or `badLength` for the group id or
/// the gap key.
#[wasm_bindgen(js_name = composeConversationReentryEnvelope)]
pub fn compose_conversation_reentry_envelope(
    group_id: &[u8],
    epoch_left: u64,
    gap_from_ms: u64,
    gap_key: &[u8],
    key_package: &[u8],
) -> Result<Vec<u8>, String> {
    rust_compose(&RustReentry {
        group_id: key32("conversationReentry", "group id", group_id)?,
        epoch_left,
        gap_from_ms,
        gap_key: key32("conversationReentry", "gap key", gap_key)?,
        key_package: key_package.to_vec(),
    })
    .map_err(reentry_error)
}

// ============================================================================
// The gap's history, sealed to the returning device (section 7, security
// review 2026-11-21)
// ============================================================================

/// A fresh gap key pair. The secret is secret material: keep it with the
/// durable request record, never send it.
#[wasm_bindgen]
pub struct GapKey {
    secret: Vec<u8>,
    public_key: Vec<u8>,
}

#[wasm_bindgen]
impl GapKey {
    /// The 32-byte X25519 secret.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn secret(&self) -> Vec<u8> {
        self.secret.clone()
    }

    /// The 32-byte public key the request carries.
    #[wasm_bindgen(getter, js_name = publicKey)]
    #[must_use]
    pub fn public_key(&self) -> Vec<u8> {
        self.public_key.clone()
    }
}

/// Mint a fresh gap key for one request to come back.
#[wasm_bindgen(js_name = generateGapKey)]
#[must_use]
pub fn generate_gap_key() -> GapKey {
    let key = RustGapKey::generate();
    GapKey {
        secret: key.secret().as_bytes().to_vec(),
        public_key: key.public().to_vec(),
    }
}

/// The public key of a stored gap secret.
///
/// # Errors
///
/// `gapHistory.badLength` for a secret that is not 32 bytes.
#[wasm_bindgen(js_name = gapPublicKey)]
pub fn gap_public_key(secret: &[u8]) -> Result<Vec<u8>, String> {
    Ok(
        RustGapKey::from_secret(&key32("gapHistory", "gap secret", secret)?)
            .public()
            .to_vec(),
    )
}

/// The 32-byte DS rendezvous id the gap's history is posted to and read
/// from: derived from the gap public key, no group secret involved.
///
/// # Errors
///
/// `gapHistory.badLength` for a key that is not 32 bytes.
#[wasm_bindgen(js_name = gapHistoryRendezvous)]
pub fn gap_history_rendezvous(gap_key: &[u8]) -> Result<Vec<u8>, String> {
    Ok(rust_gap_rendezvous(&key32("gapHistory", "gap key", gap_key)?).to_vec())
}

/// Where the re-admitter starts the gap's rows: the later of the request's
/// claim and the first time this device saw the returning leaf in the
/// roster, else its last message from that leaf. `undefined` when neither is
/// known: send nothing.
#[wasm_bindgen(js_name = gapHistoryStart)]
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
#[wasm_bindgen]
pub struct OpenedGapHistory {
    signer_leaf_key: Vec<u8>,
    bundle: Vec<u8>,
}

#[wasm_bindgen]
impl OpenedGapHistory {
    /// The leaf signature key the signature verified under.
    #[wasm_bindgen(getter, js_name = signerLeafKey)]
    #[must_use]
    pub fn signer_leaf_key(&self) -> Vec<u8> {
        self.signer_leaf_key.clone()
    }

    /// The bundle bytes.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn bundle(&self) -> Vec<u8> {
        self.bundle.clone()
    }
}

/// An opened structured gap history and its signer's leaf key.
#[wasm_bindgen]
pub struct OpenedGapHistoryStructured {
    signer_leaf_key: Vec<u8>,
    bundle: Option<OpenedHistoryBundle>,
}

#[wasm_bindgen]
impl OpenedGapHistoryStructured {
    /// The leaf signature key the signature verified under.
    #[wasm_bindgen(getter, js_name = signerLeafKey)]
    #[must_use]
    pub fn signer_leaf_key(&self) -> Vec<u8> {
        self.signer_leaf_key.clone()
    }

    /// The bundle; `undefined` after the first call.
    #[wasm_bindgen(js_name = takeBundle)]
    pub fn take_bundle(&mut self) -> Option<OpenedHistoryBundle> {
        self.bundle.take()
    }
}

fn signer_of(readmitter: &Identity) -> SigningKey {
    SigningKey::from_seed(&readmitter.mls_identity().ed25519_seed_bytes())
}

/// Seal opaque bundle bytes of the conversation `group_id` to the gap key a
/// request carried, signed by `readmitter`: this device's identity in that
/// conversation (its leaf signing key).
///
/// # Errors
///
/// A `gapHistory.<code>` string.
#[wasm_bindgen(js_name = sealGapHistory)]
pub fn seal_gap_history(
    readmitter: &Identity,
    gap_key: &[u8],
    group_id: &[u8],
    bundle: &[u8],
) -> Result<Vec<u8>, String> {
    rust_seal_gap(
        &signer_of(readmitter),
        &key32("gapHistory", "gap key", gap_key)?,
        &key32("gapHistory", "group id", group_id)?,
        bundle,
    )
    .map_err(gap_error)
}

fn open_gap(gap_secret: &[u8], group_id: &[u8], sealed: &[u8]) -> Result<RustOpened, String> {
    rust_open_gap(
        &key32("gapHistory", "gap secret", gap_secret)?,
        &key32("gapHistory", "group id", group_id)?,
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
/// A `gapHistory.<code>` string (`badSignature`, `notOpened`, ...).
#[wasm_bindgen(js_name = openGapHistory)]
pub fn open_gap_history(
    gap_secret: &[u8],
    group_id: &[u8],
    sealed: &[u8],
) -> Result<OpenedGapHistory, String> {
    let opened = open_gap(gap_secret, group_id, sealed)?;
    Ok(OpenedGapHistory {
        signer_leaf_key: opened.signer_leaf_key.to_vec(),
        bundle: opened.bundle,
    })
}

/// `sealGapHistory` over a structured history bundle (the
/// `HistoryGroupInput`s `sealHistoryBundleStructured` takes): the bundle is
/// the prost-encoded `HistoryBundle`, sealed to the gap key and signed by
/// `readmitter`. Refuses more than 1000 messages.
///
/// # Errors
///
/// A `gapHistory.<code>` string, or `gapHistory.badBundle` for too many
/// messages.
#[wasm_bindgen(js_name = sealGapHistoryStructured)]
pub fn seal_gap_history_structured(
    readmitter: &Identity,
    gap_key: &[u8],
    group_id: &[u8],
    groups: Vec<HistoryGroupInput>,
    max_hlc: u64,
) -> Result<Vec<u8>, String> {
    use prost::Message as _;
    let bundle = crate::mls::build_history_bundle_proto(groups, max_hlc);
    let total: usize = bundle.groups.iter().map(|g| g.messages.len()).sum();
    if total > lakko_mls::history_bundle::MAX_BUNDLE_MESSAGES {
        return Err(format!(
            "gapHistory.badBundle: more than {} messages",
            lakko_mls::history_bundle::MAX_BUNDLE_MESSAGES
        ));
    }
    let mut bytes = Vec::with_capacity(bundle.encoded_len());
    bundle
        .encode(&mut bytes)
        .map_err(|e| format!("gapHistory.badBundle: {e}"))?;
    seal_gap_history(readmitter, gap_key, group_id, &bytes)
}

/// Open a structured gap history (`sealGapHistoryStructured`): the bundle
/// (`takeBundle()`) and its signer's leaf key. Keep it only when
/// `gapHistorySignerAccepted`.
///
/// # Errors
///
/// A `gapHistory.<code>` string; `gapHistory.badBundle` when the bundle does
/// not decode.
#[wasm_bindgen(js_name = openGapHistoryStructured)]
pub fn open_gap_history_structured(
    gap_secret: &[u8],
    group_id: &[u8],
    sealed: &[u8],
) -> Result<OpenedGapHistoryStructured, String> {
    use prost::Message as _;
    let opened = open_gap(gap_secret, group_id, sealed)?;
    let bundle =
        lakko_proto::mls_app::history_bundle::v1::HistoryBundle::decode(opened.bundle.as_slice())
            .map_err(|e| format!("gapHistory.badBundle: {e}"))?;
    Ok(OpenedGapHistoryStructured {
        signer_leaf_key: opened.signer_leaf_key.to_vec(),
        bundle: Some(crate::mls::opened_history_bundle(bundle)),
    })
}

/// The longest bundle `sealGapHistory` takes.
#[wasm_bindgen(js_name = gapHistoryMaxBundleLen)]
#[must_use]
pub fn gap_history_max_bundle_len() -> u32 {
    len_u32(GAP_HISTORY_MAX_BUNDLE_LEN)
}

/// Decode a request envelope (bounded, fail-closed).
///
/// # Errors
///
/// A `conversationReentry.<code>` string.
#[wasm_bindgen(js_name = decodeConversationReentryEnvelope)]
pub fn decode_conversation_reentry_envelope(
    plaintext: &[u8],
) -> Result<ConversationReentry, String> {
    rust_decode(plaintext)
        .map(|inner| ConversationReentry { inner })
        .map_err(reentry_error)
}

/// Seal a request envelope as a knock row (route 3).
///
/// # Errors
///
/// A `conversationReentry.<code>` string.
#[wasm_bindgen(js_name = sealReentryKnockRow)]
pub fn seal_reentry_knock_row(
    knock_pk: &[u8],
    knock_version: u16,
    envelope: &[u8],
) -> Result<Vec<u8>, String> {
    rust_seal_knock(
        &key32("conversationReentry", "knock key", knock_pk)?,
        knock_version,
        envelope,
    )
    .map_err(reentry_error)
}

/// Open a re-entry knock row with the knock seed of the version it names.
/// A join request does not open here (`conversationReentry.notOpened`).
///
/// # Errors
///
/// A `conversationReentry.<code>` string.
#[wasm_bindgen(js_name = openReentryKnockRow)]
pub fn open_reentry_knock_row(
    knock_seed: &[u8],
    row: &[u8],
) -> Result<ConversationReentry, String> {
    rust_open_knock(
        &key32("conversationReentry", "knock seed", knock_seed)?,
        row,
    )
    .map(|inner| ConversationReentry { inner })
    .map_err(reentry_error)
}

/// The shortest well-formed request envelope.
#[wasm_bindgen(js_name = conversationReentryEnvelopeMinLen)]
#[must_use]
pub fn conversation_reentry_envelope_min_len() -> u32 {
    len_u32(CONVERSATION_REENTRY_ENVELOPE_MIN_LEN)
}

/// The longest well-formed request envelope.
#[wasm_bindgen(js_name = conversationReentryEnvelopeMaxLen)]
#[must_use]
pub fn conversation_reentry_envelope_max_len() -> u32 {
    len_u32(CONVERSATION_REENTRY_ENVELOPE_MAX_LEN)
}

// ============================================================================
// The knock key handed to members
// ============================================================================

/// A group's knock public key and version.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KnockKey {
    inner: RustKnockKey,
}

#[wasm_bindgen]
impl KnockKey {
    /// The knock-key version.
    #[wasm_bindgen(getter, js_name = knockVersion)]
    #[must_use]
    pub fn knock_version(&self) -> u16 {
        self.inner.knock_version
    }

    /// The 32-byte X25519 knock public key.
    #[wasm_bindgen(getter, js_name = knockPk)]
    #[must_use]
    pub fn knock_pk(&self) -> Vec<u8> {
        self.inner.knock_pk.to_vec()
    }
}

/// Compose the knock-key envelope the owner sends in the group.
///
/// # Errors
///
/// `knockKey.badLength` when `knockPk` is not 32 bytes.
#[wasm_bindgen(js_name = composeKnockKeyEnvelope)]
pub fn compose_knock_key_envelope(knock_version: u16, knock_pk: &[u8]) -> Result<Vec<u8>, String> {
    Ok(rust_compose_knock_key(&RustKnockKey {
        knock_version,
        knock_pk: key32("knockKey", "knock key", knock_pk)?,
    }))
}

/// Decode a knock-key envelope. Keep it only from the owner or an admin
/// resolved at receipt, when `knockKeyIsNewer`.
///
/// # Errors
///
/// A `knockKey.<code>` string.
#[wasm_bindgen(js_name = decodeKnockKeyEnvelope)]
pub fn decode_knock_key_envelope(plaintext: &[u8]) -> Result<KnockKey, String> {
    rust_decode_knock_key(plaintext)
        .map(|inner| KnockKey { inner })
        .map_err(knock_key_error)
}

/// `true` when an incoming knock version replaces the stored one.
#[wasm_bindgen(js_name = knockKeyIsNewer)]
#[must_use]
pub fn knock_key_is_newer(incoming_version: u16, stored_version: Option<u16>) -> bool {
    rust_knock_key_is_newer(incoming_version, stored_version)
}

/// The knock-key envelope's one length.
#[wasm_bindgen(js_name = knockKeyEnvelopeLen)]
#[must_use]
pub fn knock_key_envelope_len() -> u32 {
    len_u32(KNOCK_KEY_ENVELOPE_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_envelopes_and_rules_cross_the_surface() {
        let gap = generate_gap_key();
        let env =
            compose_conversation_reentry_envelope(&[3; 32], 9, 1000, &gap.public_key(), &[1, 2, 3])
                .expect("compose");
        let back = decode_conversation_reentry_envelope(&env).expect("decode");
        assert_eq!(back.group_id(), vec![3; 32]);
        assert_eq!(back.epoch_left(), 9);
        assert_eq!(back.gap_key(), gap.public_key());
        let readmitter = Identity::from_seed(&[0x71; 64]).expect("readmitter");
        let impostor = Identity::from_seed(&[0x72; 64]).expect("impostor");
        let sealed =
            seal_gap_history(&readmitter, &gap.public_key(), &[3; 32], b"bundle").expect("seal");
        let opened = open_gap_history(&gap.secret(), &[3; 32], &sealed).expect("open");
        assert_eq!(opened.bundle(), b"bundle");
        let readmitter_leaf = readmitter.mls_identity().signature_public_key().to_vec();
        assert_eq!(opened.signer_leaf_key(), readmitter_leaf);
        assert!(!sealed.windows(32).any(|w| w == readmitter_leaf.as_slice()));
        let forged =
            seal_gap_history(&impostor, &gap.public_key(), &[3; 32], b"invented").expect("seal");
        let opened = open_gap_history(&gap.secret(), &[3; 32], &forged).expect("open");
        assert_eq!(
            opened.signer_leaf_key(),
            impostor.mls_identity().signature_public_key().to_vec()
        );
        assert!(open_gap_history(&generate_gap_key().secret(), &[3; 32], &sealed).is_err());
        assert_eq!(
            gap_history_rendezvous(&gap.public_key()).expect("r").len(),
            32
        );
        assert_eq!(gap_history_start(100, None, Some(300)), Some(300));
        let seed = lakko_contacts::knock::knock_seed(&[5; 32], 2);
        let pk = lakko_contacts::knock::knock_public_key(&seed);
        let row = seal_reentry_knock_row(&pk, 2, &env).expect("seal");
        assert_eq!(open_reentry_knock_row(&seed, &row).expect("open"), back);
        let key = compose_knock_key_envelope(4, &[9; 32]).expect("compose");
        assert_eq!(
            decode_knock_key_envelope(&key)
                .expect("decode")
                .knock_version(),
            4
        );
        assert!(compose_knock_key_envelope(4, &[9; 31])
            .expect_err("short")
            .starts_with("knockKey.badLength"));

        let s = conversation_behind_verdict(
            &BehindState::new(BehindStateKind::InStep, 0),
            false,
            true,
            Some(16),
            6,
            0,
            true,
            false,
            7,
        );
        assert_eq!(s.kind(), BehindStateKind::Confirmed);
        assert_eq!(s.since_ms(), Some(7));
        assert!(s.stops_acting());
        assert!(reentry_request_due(&s, None, false, 7));
        assert!(reentry_request_due(
            &s,
            Some(7),
            true,
            7 + reentry_own_device_answer_wait_ms()
        ));
        assert_eq!(
            reentry_route(false, GroupKind::OneToOne, false),
            ReentryRoute::Mailbox
        );
        assert_eq!(
            reentry_route(true, GroupKind::OneToOne, true),
            ReentryRoute::Mailbox
        );
        assert_eq!(
            reentry_admission_verdict(true, None, false, None, 1),
            ReentryAdmission::Admit
        );
    }
}
