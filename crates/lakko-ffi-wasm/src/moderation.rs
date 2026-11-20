// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm surface for reporting and moderation ([ADR-0099]).
//!
//! The client half mirrors `crates/lakko-ffi-uniffi/src/moderation.rs`
//! (attest, unwrap, attestation, the report builder, the appeal seal). The moderators' panel
//! half is web only: verify the moderator list, derive the moderator's
//! report key from the security key's PRF output, open a report and check
//! its evidence, and build a statement whose challenge the security key
//! signs.
//!
//! Both halves also read the suspension mark from a snapshot bucket answer
//! (`moderationBucketPrefix`, `moderationAccountStanding`, section 5, step
//! 4), and a channel's removal (`moderationChannelBucketPrefix`,
//! `moderationChannelStanding`, step 5), and prove a suspended or removed
//! standing from the moderator's signed statement behind it before acting
//! on it (`moderationProveAccountStanding`, `moderationProveChannelStanding`,
//! security fix F2). The panel also opens an appeal
//! (`openModerationAppeal`).
//!
//! A refusal throws an `Error` whose message is `"<code>: <reason>"` with
//! the codes of `lakko_moderation::ModerationError`,
//! `lakko_moderation::snapshot::SnapshotError` or
//! `lakko_attachments::AttestError`.
//!
//! [ADR-0099]: ../../../docs/adr/0099-reporting-and-moderation.md

use lakko_attachments::attested;
use lakko_moderation::evidence::{verify_message_evidence, EvidenceVerdict};
use lakko_moderation::moderators::ModeratorList;
use lakko_moderation::report::{
    open_report, seal_report, ConversationKind, EvidenceItem, ReportClear, ReportContent,
};
use lakko_moderation::seal::{moderator_report_secret, MODERATOR_ID_LEN, MODERATOR_PRF_INPUT};
use lakko_moderation::snapshot::{
    account_prefix_hex, channel_prefix_hex, check_account, check_target, prove_standing,
    SnapshotError, Standing, TargetStanding,
};
use lakko_moderation::statement::{reason_commitment, Action, SignedStatement, Statement};
use lakko_moderation::target::{account_target_hash, Target, TargetKind};
use lakko_moderation::webauthn::Assertion;
use lakko_moderation::{ModerationError, Reason};
use wasm_bindgen::prelude::*;

use crate::mls::Identity;

fn code(e: &ModerationError) -> String {
    format!("{}: {e}", e.code())
}

fn js(message: String) -> JsError {
    JsError::new(&message)
}

fn fixed<const N: usize>(bytes: &[u8], what: &'static str) -> Result<[u8; N], String> {
    bytes
        .try_into()
        .map_err(|_| code(&ModerationError::Invalid(what)))
}

// ---------------------------------------------------------------------------
// The client half.
// ---------------------------------------------------------------------------

/// Wrap the outgoing chat `envelope` for the conversation `groupId` with a
/// signature by `identity`'s conversation leaf key.
///
/// # Errors
///
/// `bad-group-id`, `not-a-chat-envelope`.
#[wasm_bindgen(js_name = attestMessage)]
pub fn attest_message(
    identity: &Identity,
    group_id: &[u8],
    envelope: &[u8],
) -> Result<Vec<u8>, JsError> {
    let id = identity.mls_identity();
    attested::attest(
        &id.ed25519_seed_bytes(),
        Some(id.signature_public_key()),
        group_id,
        envelope,
    )
    .map_err(|e| js(format!("{}: {e}", e.code())))
}

/// The envelope inside an attested plaintext, or the plaintext unchanged.
#[wasm_bindgen(js_name = unwrapAttested)]
#[must_use]
pub fn unwrap_attested(plaintext: &[u8]) -> Vec<u8> {
    attested::split(plaintext).map_or_else(|_| plaintext.to_vec(), |(_, env)| env.to_vec())
}

/// What a received plaintext's signature shows: `"verified"`, `"failed"`
/// or `"absent"`.
#[wasm_bindgen(js_name = messageAttestation)]
#[must_use]
pub fn message_attestation(plaintext: &[u8], group_id: &[u8], sender_leaf_key: &[u8]) -> String {
    match lakko_attachments::parse_attested_body(plaintext, group_id, sender_leaf_key).1 {
        lakko_attachments::Attestation::Verified => "verified",
        lakko_attachments::Attestation::Failed => "failed",
        lakko_attachments::Attestation::Absent => "absent",
    }
    .to_owned()
}

/// The 32-byte report target of an account.
///
/// # Errors
///
/// `invalid` for a key that is not 32 bytes.
#[wasm_bindgen(js_name = moderationAccountTarget)]
pub fn moderation_account_target(account_key: &[u8]) -> Result<Vec<u8>, JsError> {
    let key = fixed::<32>(account_key, "account key length").map_err(js)?;
    Ok(account_target_hash(&key).to_vec())
}

/// The report reasons' wire bytes, in sheet order.
#[wasm_bindgen(js_name = reportReasonCodes)]
#[must_use]
pub fn report_reason_codes() -> Vec<u8> {
    Reason::ALL.iter().map(|r| r.as_u8()).collect()
}

/// The stable name of a reason byte, or `undefined`.
#[wasm_bindgen(js_name = reportReasonName)]
#[must_use]
pub fn report_reason_name(code: u8) -> Option<String> {
    Reason::from_u8(code).ok().map(|r| r.name().to_owned())
}

/// Builds a report: the clear fields at construction, evidence added one
/// item at a time, then [`ReportBuilder::seal`].
#[wasm_bindgen]
pub struct ReportBuilder {
    target_kind: u8,
    target: Vec<u8>,
    reason: u8,
    conversation_kind: u8,
    comment: String,
    evidence: Vec<EvidenceItem>,
}

#[wasm_bindgen]
impl ReportBuilder {
    /// `targetKind`: 1 account, 2 group handle, 3 channel, 4 name.
    /// `conversationKind`: 1 one-to-one, 2 group, 3 channel, 4 contact
    /// request, 5 Discover, 6 a card.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(
        target_kind: u8,
        target: &[u8],
        reason: u8,
        conversation_kind: u8,
        comment: String,
    ) -> Self {
        Self {
            target_kind,
            target: target.to_vec(),
            reason,
            conversation_kind,
            comment,
            evidence: Vec::new(),
        }
    }

    /// Add a message as received, with the sender leaf's key and binding.
    ///
    /// # Errors
    ///
    /// `invalid` for a leaf key that is not 32 bytes.
    #[wasm_bindgen(js_name = addMessage)]
    pub fn add_message(
        &mut self,
        group_id: &[u8],
        plaintext: &[u8],
        leaf_key: &[u8],
        binding: &[u8],
    ) -> Result<(), JsError> {
        self.evidence.push(EvidenceItem::Message {
            group_id: group_id.to_vec(),
            plaintext: plaintext.to_vec(),
            leaf_key: fixed(leaf_key, "leaf key length").map_err(js)?,
            binding: binding.to_vec(),
        });
        Ok(())
    }

    /// Add a contact request as received.
    #[wasm_bindgen(js_name = addContactRequest)]
    pub fn add_contact_request(&mut self, wire: &[u8]) {
        self.evidence.push(EvidenceItem::ContactRequest {
            wire: wire.to_vec(),
        });
    }

    /// Add a channel post as received.
    ///
    /// # Errors
    ///
    /// `invalid` for a channel id that is not 16 bytes.
    #[wasm_bindgen(js_name = addChannelPost)]
    pub fn add_channel_post(&mut self, channel_id: &[u8], post: &[u8]) -> Result<(), JsError> {
        self.evidence.push(EvidenceItem::ChannelPost {
            channel_id: fixed(channel_id, "channel id length").map_err(js)?,
            post: post.to_vec(),
        });
        Ok(())
    }

    /// Add a Discover listing as shown.
    #[wasm_bindgen(js_name = addListing)]
    pub fn add_listing(&mut self, wire: &[u8]) {
        self.evidence.push(EvidenceItem::Listing {
            wire: wire.to_vec(),
        });
    }

    /// Verify the moderator list under the pinned `rootKey` and seal. The
    /// result is the body of `POST /moderation/report`.
    ///
    /// # Errors
    ///
    /// The codes of `ModerationError`.
    pub fn seal(&self, moderator_list: &[u8], root_key: &[u8]) -> Result<Vec<u8>, JsError> {
        self.seal_core(moderator_list, root_key).map_err(js)
    }
}

impl ReportBuilder {
    fn seal_core(&self, moderator_list: &[u8], root_key: &[u8]) -> Result<Vec<u8>, String> {
        let root = fixed::<32>(root_key, "root key length")?;
        let list = ModeratorList::verify(moderator_list, &root).map_err(|e| code(&e))?;
        let kind = TargetKind::from_u8(self.target_kind).map_err(|e| code(&e))?;
        if kind == TargetKind::Blob {
            return Err(code(&ModerationError::Invalid("a blob is not reported")));
        }
        seal_report(
            Target::new(kind, &self.target).map_err(|e| code(&e))?,
            Reason::from_u8(self.reason).map_err(|e| code(&e))?,
            ConversationKind::from_u8(self.conversation_kind).map_err(|e| code(&e))?,
            &ReportContent {
                comment: self.comment.clone(),
                evidence: self.evidence.clone(),
            },
            list.list_version,
            &list.recipients(),
        )
        .map_err(|e| code(&e))
    }
}

/// Seal a suspended person's appeal (ADR-0099 section 4): the moderator list
/// is verified under the pinned `rootKey`, and the trimmed `text` (1 to 1,000
/// characters) is sealed to its current moderators. The result is the
/// `sealed_b64` of `POST /account/suspension/appeal`.
///
/// # Errors
///
/// The codes of `ModerationError` (`invalid` for an empty or over-long text
/// or a bad root key, `bad-signature` for a list that does not verify).
#[wasm_bindgen(js_name = sealModerationAppeal)]
pub fn seal_moderation_appeal(
    text: &str,
    moderator_list: &[u8],
    root_key: &[u8],
) -> Result<Vec<u8>, JsError> {
    seal_appeal_core(text, moderator_list, root_key).map_err(js)
}

fn seal_appeal_core(text: &str, moderator_list: &[u8], root_key: &[u8]) -> Result<Vec<u8>, String> {
    let root = fixed::<32>(root_key, "root key length")?;
    let list = ModeratorList::verify(moderator_list, &root).map_err(|e| code(&e))?;
    lakko_moderation::appeal::seal_appeal(text, &list).map_err(|e| code(&e))
}

// ---------------------------------------------------------------------------
// The moderators' panel.
// ---------------------------------------------------------------------------

/// A moderator list verified under the pinned root.
#[wasm_bindgen]
pub struct VerifiedModeratorList {
    inner: ModeratorList,
}

#[wasm_bindgen]
impl VerifiedModeratorList {
    /// The list version.
    #[wasm_bindgen(getter, js_name = listVersion)]
    #[must_use]
    pub fn list_version(&self) -> u64 {
        self.inner.list_version
    }

    /// The panel's WebAuthn RP id.
    #[wasm_bindgen(getter, js_name = rpId)]
    #[must_use]
    pub fn rp_id(&self) -> String {
        self.inner.rp_id.clone()
    }

    /// The panel's origin.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn origin(&self) -> String {
        self.inner.origin.clone()
    }

    /// How many moderators the list holds.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn count(&self) -> u32 {
        u32::try_from(self.inner.entries.len()).unwrap_or(u32::MAX)
    }

    /// The `i`-th moderator id.
    #[wasm_bindgen(js_name = moderatorId)]
    #[must_use]
    pub fn moderator_id(&self, i: u32) -> Option<Vec<u8>> {
        self.entry(i).map(|e| e.moderator_id.to_vec())
    }

    /// The `i`-th moderator's name.
    #[must_use]
    pub fn name(&self, i: u32) -> Option<String> {
        self.entry(i).map(|e| e.name.clone())
    }

    /// The `i`-th moderator's WebAuthn credential id (for `allowCredentials`).
    #[wasm_bindgen(js_name = credentialId)]
    #[must_use]
    pub fn credential_id(&self, i: u32) -> Option<Vec<u8>> {
        self.entry(i).map(|e| e.credential_id.clone())
    }

    /// When the `i`-th moderator was removed (unix ms), `0` when active.
    #[wasm_bindgen(js_name = revokedAtMs)]
    #[must_use]
    pub fn revoked_at_ms(&self, i: u32) -> Option<u64> {
        self.entry(i).map(|e| e.revoked_at_ms)
    }
}

impl VerifiedModeratorList {
    fn entry(&self, i: u32) -> Option<&lakko_moderation::ModeratorEntry> {
        self.inner.entries.get(usize::try_from(i).ok()?)
    }
}

/// Verify a moderator list under the pinned root key.
///
/// # Errors
///
/// The codes of `ModerationError`.
#[wasm_bindgen(js_name = verifyModeratorList)]
pub fn verify_moderator_list(
    bytes: &[u8],
    root_key: &[u8],
) -> Result<VerifiedModeratorList, JsError> {
    let root = fixed::<32>(root_key, "root key length").map_err(js)?;
    ModeratorList::verify(bytes, &root)
        .map(|inner| VerifiedModeratorList { inner })
        .map_err(|e| js(code(&e)))
}

/// The bytes a moderator's security key evaluates the WebAuthn PRF over, at
/// enrolment and at the panel's sign-in (`prf: { eval: { first: <these> } }`).
/// Its output, 32 bytes, goes to `moderatorReportPublicKey`,
/// `openModerationReport` and `openModerationAppeal`.
#[wasm_bindgen(js_name = moderatorPrfInput)]
#[must_use]
pub fn moderator_prf_input() -> Vec<u8> {
    MODERATOR_PRF_INPUT.to_vec()
}

/// The moderator's X25519 report public key from their security key's
/// 32-byte PRF output (what the offline tool lists for them).
///
/// # Errors
///
/// `invalid` for a PRF output that is not 32 bytes.
#[wasm_bindgen(js_name = moderatorReportPublicKey)]
pub fn moderator_report_public_key(prf_output: &[u8]) -> Result<Vec<u8>, JsError> {
    moderator_report_secret(prf_output)
        .map(|s| s.public_key().to_bytes().to_vec())
        .map_err(|e| js(code(&e)))
}

/// A report a moderator opened.
#[wasm_bindgen]
pub struct OpenedReport {
    clear: ReportClear,
    content: ReportContent,
}

#[wasm_bindgen]
impl OpenedReport {
    /// The target kind byte.
    #[wasm_bindgen(getter, js_name = targetKind)]
    #[must_use]
    pub fn target_kind(&self) -> u8 {
        self.clear.target.kind().as_u8()
    }

    /// The target bytes.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn target(&self) -> Vec<u8> {
        self.clear.target.bytes().to_vec()
    }

    /// The reason byte.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn reason(&self) -> u8 {
        self.clear.reason.as_u8()
    }

    /// The conversation kind byte.
    #[wasm_bindgen(getter, js_name = conversationKind)]
    #[must_use]
    pub fn conversation_kind(&self) -> u8 {
        self.clear.conversation_kind as u8
    }

    /// The reporter's comment.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn comment(&self) -> String {
        self.content.comment.clone()
    }

    /// How many evidence items.
    #[wasm_bindgen(getter, js_name = evidenceCount)]
    #[must_use]
    pub fn evidence_count(&self) -> u32 {
        u32::try_from(self.content.evidence.len()).unwrap_or(u32::MAX)
    }

    /// The `i`-th item's kind: `"message"`, `"contact-request"`,
    /// `"channel-post"` or `"listing"`.
    #[wasm_bindgen(js_name = evidenceKind)]
    #[must_use]
    pub fn evidence_kind(&self, i: u32) -> Option<String> {
        self.item(i).map(|item| {
            match item {
                EvidenceItem::Message { .. } => "message",
                EvidenceItem::ContactRequest { .. } => "contact-request",
                EvidenceItem::ChannelPost { .. } => "channel-post",
                EvidenceItem::Listing { .. } => "listing",
            }
            .to_owned()
        })
    }

    /// The `i`-th item's bytes to show: a message's envelope (to parse with
    /// `parseBodyEnvelope`), or the wire of the other kinds.
    #[wasm_bindgen(js_name = evidenceContent)]
    #[must_use]
    pub fn evidence_content(&self, i: u32) -> Option<Vec<u8>> {
        self.item(i).map(|item| match item {
            EvidenceItem::Message { plaintext, .. } => unwrap_attested(plaintext),
            EvidenceItem::ContactRequest { wire } | EvidenceItem::Listing { wire } => wire.clone(),
            EvidenceItem::ChannelPost { post, .. } => post.clone(),
        })
    }

    /// The account key the `i`-th item verifies to, or `undefined` when it
    /// does not verify. Compare with `moderationAccountTarget(key)` against
    /// the case's target to mark it "Written by this account".
    #[wasm_bindgen(js_name = evidenceAuthor)]
    #[must_use]
    pub fn evidence_author(&self, i: u32) -> Option<Vec<u8>> {
        match verify_message_evidence(self.item(i)?, lakko_mls::account_key_of_credential) {
            EvidenceVerdict::Verified { account_key } => Some(account_key.to_vec()),
            EvidenceVerdict::NotVerified(_) => None,
        }
    }

    /// Whether the `i`-th item verifies as written by the report's target.
    #[wasm_bindgen(js_name = evidenceIsByTarget)]
    #[must_use]
    pub fn evidence_is_by_target(&self, i: u32) -> bool {
        self.item(i).is_some_and(|item| {
            verify_message_evidence(item, lakko_mls::account_key_of_credential)
                .is_by(&self.clear.target)
        })
    }
}

impl OpenedReport {
    fn item(&self, i: u32) -> Option<&EvidenceItem> {
        self.content.evidence.get(usize::try_from(i).ok()?)
    }
}

fn open_core(
    report: &[u8],
    moderator_id: &[u8],
    prf_output: &[u8],
) -> Result<OpenedReport, String> {
    let id = fixed::<MODERATOR_ID_LEN>(moderator_id, "moderator id length")?;
    let secret = moderator_report_secret(prf_output).map_err(|e| code(&e))?;
    let (clear, content) = open_report(report, &id, &secret).map_err(|e| code(&e))?;
    Ok(OpenedReport { clear, content })
}

fn open_appeal_core(
    sealed: &[u8],
    moderator_id: &[u8],
    prf_output: &[u8],
) -> Result<String, String> {
    let id = fixed::<MODERATOR_ID_LEN>(moderator_id, "moderator id length")?;
    let secret = moderator_report_secret(prf_output).map_err(|e| code(&e))?;
    lakko_moderation::appeal::open_appeal(sealed, &id, &secret).map_err(|e| code(&e))
}

/// Open a suspended person's appeal as the moderator `moderatorId`, whose
/// report key comes from the security key's 32-byte PRF output. Returns the
/// appeal text.
///
/// # Errors
///
/// The codes of `ModerationError` (`cannot-open` when it is not sealed to
/// this moderator, `malformed` for framing or a text that breaks the appeal
/// rules), `invalid` for an id that is not 16 bytes.
#[wasm_bindgen(js_name = openModerationAppeal)]
pub fn open_moderation_appeal(
    sealed: &[u8],
    moderator_id: &[u8],
    prf_output: &[u8],
) -> Result<String, JsError> {
    open_appeal_core(sealed, moderator_id, prf_output).map_err(js)
}

/// Open a report as the moderator `moderatorId`, whose report key comes
/// from the security key's 32-byte PRF output.
///
/// # Errors
///
/// The codes of `ModerationError` (`cannot-open` when it is not sealed to
/// this moderator).
#[wasm_bindgen(js_name = openModerationReport)]
pub fn open_moderation_report(
    report: &[u8],
    moderator_id: &[u8],
    prf_output: &[u8],
) -> Result<OpenedReport, JsError> {
    open_core(report, moderator_id, prf_output).map_err(js)
}

/// A statement ready for the moderator's security key.
#[wasm_bindgen]
pub struct ModerationStatement {
    bytes: Vec<u8>,
    challenge: [u8; 32],
}

#[wasm_bindgen]
impl ModerationStatement {
    /// The encoded statement.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        self.bytes.clone()
    }

    /// The WebAuthn challenge to sign.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn challenge(&self) -> Vec<u8> {
        self.challenge.to_vec()
    }
}

#[allow(clippy::too_many_arguments)]
fn statement_core(
    statement_id: &[u8],
    action: u8,
    target_kind: u8,
    target: &[u8],
    reason: u8,
    reason_salt: &[u8],
    until_ms: u64,
    issued_at_ms: u64,
    moderator_id: &[u8],
    list_version: u64,
) -> Result<ModerationStatement, String> {
    let reason = Reason::from_u8(reason).map_err(|e| code(&e))?;
    let salt = fixed::<16>(reason_salt, "reason salt length")?;
    let statement = Statement {
        statement_id: fixed(statement_id, "statement id length")?,
        action: Action::from_u8(action).map_err(|e| code(&e))?,
        target: Target::new(
            TargetKind::from_u8(target_kind).map_err(|e| code(&e))?,
            target,
        )
        .map_err(|e| code(&e))?,
        reason_commitment: reason_commitment(reason, &salt),
        until_ms,
        issued_at_ms,
        moderator_id: fixed(moderator_id, "moderator id length")?,
        list_version,
    };
    let bytes = statement.encode_checked().map_err(|e| code(&e))?;
    Ok(ModerationStatement {
        challenge: statement.challenge(),
        bytes,
    })
}

/// Build a statement. `statementId` and `reasonSalt` are 16 random bytes
/// the panel draws; the salt goes to the applying service beside the
/// signed statement, never into the log.
///
/// # Errors
///
/// The codes of `ModerationError`.
#[wasm_bindgen(js_name = buildModerationStatement)]
#[allow(clippy::too_many_arguments)]
pub fn build_moderation_statement(
    statement_id: &[u8],
    action: u8,
    target_kind: u8,
    target: &[u8],
    reason: u8,
    reason_salt: &[u8],
    until_ms: u64,
    issued_at_ms: u64,
    moderator_id: &[u8],
    list_version: u64,
) -> Result<ModerationStatement, JsError> {
    statement_core(
        statement_id,
        action,
        target_kind,
        target,
        reason,
        reason_salt,
        until_ms,
        issued_at_ms,
        moderator_id,
        list_version,
    )
    .map_err(js)
}

/// Encode a statement with the security key's assertion over its challenge.
///
/// # Errors
///
/// `invalid` for a field over its bound.
#[wasm_bindgen(js_name = encodeSignedStatement)]
pub fn encode_signed_statement(
    statement: &[u8],
    authenticator_data: &[u8],
    client_data_json: &[u8],
    signature: &[u8],
) -> Result<Vec<u8>, JsError> {
    SignedStatement {
        statement: statement.to_vec(),
        assertion: Assertion {
            authenticator_data: authenticator_data.to_vec(),
            client_data_json: client_data_json.to_vec(),
            signature: signature.to_vec(),
        },
    }
    .encode()
    .map_err(|e| js(code(&e)))
}

// ---------------------------------------------------------------------------
// The suspension snapshot (section 5, step 4).
// ---------------------------------------------------------------------------

fn snapshot_code(e: &SnapshotError) -> String {
    format!("{}: {e}", e.code())
}

fn ms_i64(ms: u64) -> i64 {
    i64::try_from(ms).unwrap_or(i64::MAX)
}

/// An account's standing, read from a checked snapshot bucket answer.
#[wasm_bindgen]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModerationStanding {
    suspended: bool,
    until_ms: Option<i64>,
    period: u64,
    reuse_until_ms: i64,
    log_index: Option<u64>,
}

#[wasm_bindgen]
impl ModerationStanding {
    /// Whether the account is suspended at the time given.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn suspended(&self) -> bool {
        self.suspended
    }

    /// The end of the suspension (unix ms); `undefined` for no end, or when
    /// not suspended.
    #[wasm_bindgen(getter, js_name = untilMs)]
    #[must_use]
    pub fn until_ms(&self) -> Option<i64> {
        self.until_ms
    }

    /// The snapshot's period.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn period(&self) -> u64 {
        self.period
    }

    /// Until when the answer may be reused for the same prefix (unix ms).
    #[wasm_bindgen(getter, js_name = reuseUntilMs)]
    #[must_use]
    pub fn reuse_until_ms(&self) -> i64 {
        self.reuse_until_ms
    }

    /// The moderation log index of the statement behind the target's
    /// snapshot entry, when the bucket holds one: the entry to fetch
    /// (`GET /moderation/log/entry/{logIndex}?size={log_size}`) for
    /// `moderationProveAccountStanding`. `undefined` when no entry is held.
    #[wasm_bindgen(getter, js_name = logIndex)]
    #[must_use]
    pub fn log_index(&self) -> Option<u64> {
        self.log_index
    }
}

fn bucket_prefix_core(account_key: &[u8]) -> Result<String, String> {
    Ok(account_prefix_hex(&fixed::<32>(
        account_key,
        "account key length",
    )?))
}

fn standing_core(
    answer_json: &str,
    account_key: &[u8],
    log_operator_key: &[u8],
    witness_keys: &[u8],
    witness_threshold: u32,
    now_ms: i64,
) -> Result<ModerationStanding, String> {
    let account = fixed::<32>(account_key, "account key length")?;
    let operator: [u8; 32] = log_operator_key
        .try_into()
        .map_err(|_| snapshot_code(&SnapshotError::BadKey))?;
    if witness_keys.len() % 32 != 0 {
        return Err(code(&ModerationError::Invalid("witness keys length")));
    }
    let keys: Vec<&[u8]> = witness_keys.chunks_exact(32).collect();
    let threshold = usize::try_from(witness_threshold).unwrap_or(usize::MAX);
    let got = check_account(
        answer_json.as_bytes(),
        &account,
        &operator,
        &keys,
        threshold,
        now_ms,
    )
    .map_err(|e| snapshot_code(&e))?;
    Ok(standing_record(got))
}

fn standing_record(got: TargetStanding) -> ModerationStanding {
    let (suspended, until_ms) = match got.standing {
        Standing::NotSuspended => (false, None),
        Standing::Suspended { until_ms } => (true, until_ms.map(ms_i64)),
    };
    ModerationStanding {
        suspended,
        until_ms,
        period: got.period,
        reuse_until_ms: ms_i64(got.reuse_until_ms),
        log_index: got.log_index,
    }
}

fn channel_prefix_core(channel_id: &[u8]) -> Result<String, String> {
    Ok(channel_prefix_hex(&fixed::<16>(
        channel_id,
        "channel id length",
    )?))
}

fn channel_standing_core(
    answer_json: &str,
    channel_id: &[u8],
    log_operator_key: &[u8],
    witness_keys: &[u8],
    witness_threshold: u32,
    now_ms: i64,
) -> Result<ModerationStanding, String> {
    let channel = fixed::<16>(channel_id, "channel id length")?;
    let operator: [u8; 32] = log_operator_key
        .try_into()
        .map_err(|_| snapshot_code(&SnapshotError::BadKey))?;
    if witness_keys.len() % 32 != 0 {
        return Err(code(&ModerationError::Invalid("witness keys length")));
    }
    let keys: Vec<&[u8]> = witness_keys.chunks_exact(32).collect();
    let threshold = usize::try_from(witness_threshold).unwrap_or(usize::MAX);
    let got = check_target(
        answer_json.as_bytes(),
        TargetKind::Channel,
        &channel,
        &operator,
        &keys,
        threshold,
        now_ms,
    )
    .map_err(|e| snapshot_code(&e))?;
    Ok(standing_record(got))
}

/// The four lower-case hex characters of a channel's snapshot bucket (the
/// first two bytes of its 16-byte id).
///
/// # Errors
///
/// `invalid` for an id that is not 16 bytes.
#[wasm_bindgen(js_name = moderationChannelBucketPrefix)]
pub fn moderation_channel_bucket_prefix(channel_id: &[u8]) -> Result<String, JsError> {
    channel_prefix_core(channel_id).map_err(js)
}

/// Check a snapshot bucket answer as `moderationAccountStanding` does, then
/// read whether the channel `channelId` was removed at `nowMs`: `suspended`
/// is true for a removed channel, `untilMs` always `undefined`.
///
/// # Errors
///
/// The codes of `moderationAccountStanding`; `invalid` for an id that is
/// not 16 bytes.
#[wasm_bindgen(js_name = moderationChannelStanding)]
pub fn moderation_channel_standing(
    answer_json: &str,
    channel_id: &[u8],
    log_operator_key: &[u8],
    witness_keys: &[u8],
    witness_threshold: u32,
    now_ms: i64,
) -> Result<ModerationStanding, JsError> {
    channel_standing_core(
        answer_json,
        channel_id,
        log_operator_key,
        witness_keys,
        witness_threshold,
        now_ms,
    )
    .map_err(js)
}

/// The four lower-case hex characters of an account's snapshot bucket, for
/// `GET /moderation/snapshot/latest/bucket/{prefix}`.
///
/// # Errors
///
/// `invalid` for a key that is not 32 bytes.
#[wasm_bindgen(js_name = moderationBucketPrefix)]
pub fn moderation_bucket_prefix(account_key: &[u8]) -> Result<String, JsError> {
    bucket_prefix_core(account_key).map_err(js)
}

/// Check a snapshot bucket answer (the route's JSON body) against the pinned
/// moderation log operator key and, when `witnessKeys` (flat-concatenated
/// 32-byte keys) is not empty, a quorum of `witnessThreshold` of them; then
/// read `accountKey`'s standing at `nowMs`.
///
/// # Errors
///
/// The codes of `SnapshotError` (`too-large`, `malformed`, `bad-key`,
/// `bad-root-signature`, `witness-quorum`, `snapshot-not-included`,
/// `bucket-not-included`, `wrong-period`, `wrong-prefix`, `stale`,
/// `malformed-snapshot`), or `invalid` for a key or key list of the wrong
/// length.
#[wasm_bindgen(js_name = moderationAccountStanding)]
pub fn moderation_account_standing(
    answer_json: &str,
    account_key: &[u8],
    log_operator_key: &[u8],
    witness_keys: &[u8],
    witness_threshold: u32,
    now_ms: i64,
) -> Result<ModerationStanding, JsError> {
    standing_core(
        answer_json,
        account_key,
        log_operator_key,
        witness_keys,
        witness_threshold,
        now_ms,
    )
    .map_err(js)
}

fn prove_core(
    answer_json: &str,
    entry_json: &str,
    moderator_list: &[u8],
    moderation_root_key: &[u8],
    kind: TargetKind,
    target: &[u8],
    now_ms: i64,
) -> Result<(), String> {
    let root: [u8; 32] = moderation_root_key
        .try_into()
        .map_err(|_| snapshot_code(&SnapshotError::BadKey))?;
    prove_standing(
        answer_json.as_bytes(),
        entry_json.as_bytes(),
        moderator_list,
        &root,
        kind,
        target,
        now_ms,
    )
    .map_err(|e| snapshot_code(&e))
}

/// Prove an account's suspended standing from the moderator's signed
/// statement behind it. `answerJson` is the bucket answer already checked
/// with `moderationAccountStanding`; `entryJson` is the answer of
/// `GET /moderation/log/entry/{logIndex}?size={log_size}` (the standing's
/// `logIndex` and the answer head's `log_size`); `moderatorList` is the
/// root-signed list (`GET /moderation/moderators`), verified here under the
/// pinned `moderationRootKey`. Act on a suspended standing only after this
/// returns.
///
/// # Errors
///
/// The codes of `SnapshotError`: `no-entry`, `not-suspended`,
/// `entry-mismatch`, `entry-not-included`, `not-a-statement`, `bad-list`,
/// `not-authorised`, `bad-statement`, `wrong-action`, `wrong-target`,
/// `until-mismatch`, the shape codes of `moderationAccountStanding`,
/// `bad-key` for a root key that is not 32 bytes, `invalid` for an account
/// key that is not 32 bytes.
#[wasm_bindgen(js_name = moderationProveAccountStanding)]
pub fn moderation_prove_account_standing(
    answer_json: &str,
    entry_json: &str,
    moderator_list: &[u8],
    moderation_root_key: &[u8],
    account_key: &[u8],
    now_ms: i64,
) -> Result<(), JsError> {
    let account = fixed::<32>(account_key, "account key length").map_err(js)?;
    prove_core(
        answer_json,
        entry_json,
        moderator_list,
        moderation_root_key,
        TargetKind::Account,
        &account_target_hash(&account),
        now_ms,
    )
    .map_err(js)
}

/// Prove a channel's removal from the moderator's signed statement behind
/// it, as `moderationProveAccountStanding` does for an account.
///
/// # Errors
///
/// The codes of `moderationProveAccountStanding`; `invalid` for an id that
/// is not 16 bytes.
#[wasm_bindgen(js_name = moderationProveChannelStanding)]
pub fn moderation_prove_channel_standing(
    answer_json: &str,
    entry_json: &str,
    moderator_list: &[u8],
    moderation_root_key: &[u8],
    channel_id: &[u8],
    now_ms: i64,
) -> Result<(), JsError> {
    let channel = fixed::<16>(channel_id, "channel id length").map_err(js)?;
    prove_core(
        answer_json,
        entry_json,
        moderator_list,
        moderation_root_key,
        TargetKind::Channel,
        &channel,
        now_ms,
    )
    .map_err(js)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_moderation::moderators::{ModeratorEntry, ModeratorKey};

    #[test]
    fn the_prf_input_is_the_frozen_constant() {
        assert_eq!(
            moderator_prf_input(),
            b"lakko/moderation/report-key/v1".to_vec()
        );
    }

    #[test]
    fn a_built_report_opens_in_the_panel() {
        let prf = [3u8; 32];
        let root = [42u8; 32];
        let list = ModeratorList {
            list_version: 2,
            issued_at_ms: 1,
            rp_id: "m.example".into(),
            origin: "https://m.example".into(),
            entries: vec![ModeratorEntry {
                moderator_id: [1; 16],
                name: "m".into(),
                key: ModeratorKey::Ed25519([1; 32]),
                credential_id: vec![1],
                x25519_public: moderator_report_secret(&prf)
                    .unwrap()
                    .public_key()
                    .to_bytes(),
                added_at_ms: 0,
                revoked_at_ms: 0,
            }],
        }
        .sign(&root)
        .unwrap();
        let root_pub = lakko_crypto::signing::SigningKey::from_seed(&root)
            .verifying_key()
            .to_bytes();
        let mut b = ReportBuilder::new(3, &[9; 16], 2, 3, "posts".into());
        b.evidence.push(EvidenceItem::Listing { wire: vec![5] });
        let report = b.seal_core(&list, &root_pub).unwrap();
        let opened = open_core(&report, &[1; 16], &prf).unwrap();
        assert_eq!(opened.target_kind(), 3);
        assert_eq!(opened.comment(), "posts");
        assert_eq!(opened.evidence_kind(0).as_deref(), Some("listing"));
        assert_eq!(opened.evidence_author(0), None);
        assert!(open_core(&report, &[2; 16], &prf).is_err());

        let appeal = seal_appeal_core(" please ", &list, &root_pub).unwrap();
        assert_eq!(
            lakko_moderation::appeal::open_appeal(
                &appeal,
                &[1; 16],
                &moderator_report_secret(&prf).unwrap()
            )
            .unwrap(),
            "please"
        );
        assert_eq!(open_appeal_core(&appeal, &[1; 16], &prf).unwrap(), "please");
        assert!(open_appeal_core(&appeal, &[2; 16], &prf)
            .unwrap_err()
            .starts_with("cannot-open: "));
        assert!(open_appeal_core(&appeal, &[1; 15], &prf)
            .unwrap_err()
            .starts_with("invalid: "));
        assert!(seal_appeal_core("please", &list, &[0; 32]).is_err());
        assert!(seal_appeal_core("", &list, &root_pub)
            .unwrap_err()
            .starts_with("invalid: "));
    }

    #[test]
    fn a_statement_builds_and_checks_its_rules() {
        let s = statement_core(&[1; 16], 1, 1, &[2; 32], 1, &[3; 16], 0, 10, &[4; 16], 1).unwrap();
        let decoded = Statement::decode(&s.bytes).unwrap();
        assert_eq!(decoded.challenge(), s.challenge);
        assert!(decoded.reason_matches(Reason::Spam, &[3; 16]));
        assert!(statement_core(&[1; 16], 9, 1, &[2; 32], 1, &[3; 16], 0, 10, &[4; 16], 1).is_err());
    }

    #[test]
    fn the_snapshot_check_is_wired() {
        use lakko_crypto::signing::SigningKey;
        use lakko_moderation::snapshot::{build_snapshot, BucketAnswer};
        use lakko_transparency::merkle::hash_leaf;
        use lakko_transparency::SignedRoot;

        let account = [7u8; 32];
        let snapshot = build_snapshot(&[], 1_800_000_000_000, 86_400).unwrap();
        let leaf = snapshot.root().log_leaf();
        let tree_root = hash_leaf(&leaf);
        let op = SigningKey::from_seed(&[11; 32]);
        let head = SignedRoot {
            tree_root,
            log_size: 1,
            timestamp_unix: 1,
            signature: op.sign(&SignedRoot::signing_payload(&tree_root, 1, 1)),
        };
        let prefix = bucket_prefix_core(&account).unwrap();
        let answer = BucketAnswer::new(
            &snapshot,
            u16::from_str_radix(&prefix, 16).unwrap(),
            0,
            &[],
            &head,
            &[],
        );
        let json = String::from_utf8(answer.to_json().unwrap()).unwrap();
        let op_pub = op.verifying_key().to_bytes();
        let got = standing_core(&json, &account, &op_pub, &[], 1, 1_800_000_001_000).unwrap();
        assert!(!got.suspended());
        assert_eq!(got.until_ms(), None);
        assert_eq!(got.reuse_until_ms(), 1_800_086_400_000);
        assert!(
            standing_core(&json, &account, &op_pub, &[1; 32], 1, 1_800_000_001_000)
                .unwrap_err()
                .starts_with("witness-quorum: ")
        );
        assert!(standing_core(&json, &account, &op_pub, &[1; 31], 1, 0)
            .unwrap_err()
            .starts_with("invalid: "));
        assert!(standing_core("{}", &account, &op_pub, &[], 1, 0)
            .unwrap_err()
            .starts_with("malformed: "));
        assert!(bucket_prefix_core(&[1; 31]).is_err());

        // A channel in the same empty snapshot: not removed.
        let channel = [9u8; 16];
        let cprefix = channel_prefix_core(&channel).unwrap();
        assert_eq!(cprefix, "0909");
        let answer = BucketAnswer::new(
            &snapshot,
            u16::from_str_radix(&cprefix, 16).unwrap(),
            0,
            &[],
            &head,
            &[],
        );
        let json = String::from_utf8(answer.to_json().unwrap()).unwrap();
        let got =
            channel_standing_core(&json, &channel, &op_pub, &[], 1, 1_800_000_001_000).unwrap();
        assert!(!got.suspended());
        assert!(channel_standing_core(&json, &[9; 15], &op_pub, &[], 1, 0)
            .unwrap_err()
            .starts_with("invalid: "));
        assert!(channel_prefix_core(&[1; 17]).is_err());
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn a_suspension_is_proven_from_its_signed_statement() {
        use base64::engine::general_purpose::STANDARD as B64;
        use base64::Engine as _;
        use lakko_crypto::signing::SigningKey;
        use lakko_moderation::snapshot::{build_snapshot, BucketAnswer, LOG_LEAF_STATEMENT};
        use lakko_moderation::software_authenticator::SoftwareAuthenticator;
        use lakko_transparency::merkle::{audit_path, root as merkle_root};
        use lakko_transparency::SignedRoot;

        let root_seed = [42u8; 32];
        let root_pub = SigningKey::from_seed(&root_seed).verifying_key().to_bytes();
        let authenticator = SoftwareAuthenticator::from_seed(&[9; 32]);
        let list = ModeratorList {
            list_version: 1,
            issued_at_ms: 1,
            rp_id: "m.example".into(),
            origin: "https://m.example".into(),
            entries: vec![ModeratorEntry {
                moderator_id: [1; 16],
                name: "m".into(),
                key: authenticator.moderator_key(),
                credential_id: vec![1],
                x25519_public: [1; 32],
                added_at_ms: 0,
                revoked_at_ms: 0,
            }],
        };
        let list_bytes = list.sign(&root_seed).unwrap();
        let channel = [0x3cu8; 16];
        let statement = Statement {
            statement_id: [1; 16],
            action: Action::ChannelRemove,
            target: Target::new(TargetKind::Channel, &channel).unwrap(),
            reason_commitment: reason_commitment(Reason::Spam, &[1; 16]),
            until_ms: 0,
            issued_at_ms: 10,
            moderator_id: [1; 16],
            list_version: 1,
        };
        let mut leaf = vec![LOG_LEAF_STATEMENT];
        leaf.extend_from_slice(
            &authenticator
                .sign_statement(&list, &statement)
                .unwrap()
                .encode()
                .unwrap(),
        );
        let built_at = 1_800_000_000_000u64;
        let snapshot = build_snapshot(&[leaf.as_slice()], built_at, 86_400).unwrap();
        let log = vec![leaf, snapshot.root().log_leaf()];
        let tree_root = merkle_root(&log);
        let op = SigningKey::from_seed(&[11; 32]);
        let head = SignedRoot {
            tree_root,
            log_size: 2,
            timestamp_unix: 1,
            signature: op.sign(&SignedRoot::signing_payload(&tree_root, 2, 1)),
        };
        let json = String::from_utf8(
            BucketAnswer::new(&snapshot, 0x3c3c, 1, &audit_path(&log, 1), &head, &[])
                .to_json()
                .unwrap(),
        )
        .unwrap();
        let op_pub = op.verifying_key().to_bytes();
        let now = 1_800_000_001_000;
        let got = channel_standing_core(&json, &channel, &op_pub, &[], 1, now).unwrap();
        assert!(got.suspended());
        assert_eq!(got.log_index(), Some(0));
        let path: Vec<String> = audit_path(&log, 0)
            .iter()
            .map(|h| format!("\"{}\"", B64.encode(h)))
            .collect();
        let entry = format!(
            r#"{{"index":0,"leaf_b64":"{}","audit_path_b64":[{}],"log_size":2}}"#,
            B64.encode(&log[0]),
            path.join(",")
        );
        let prove = |root: &[u8], target: &[u8]| {
            prove_core(
                &json,
                &entry,
                &list_bytes,
                root,
                TargetKind::Channel,
                target,
                now,
            )
        };
        assert_eq!(prove(&root_pub, &channel), Ok(()));
        assert!(prove(&[1; 32], &channel)
            .unwrap_err()
            .starts_with("bad-list: "));
        assert!(prove(&[1; 31], &channel)
            .unwrap_err()
            .starts_with("bad-key: "));
        let mut other = channel;
        other[15] ^= 1;
        assert!(prove(&root_pub, &other)
            .unwrap_err()
            .starts_with("no-entry: "));
    }
}
