// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for reporting ([ADR-0098]) on iOS and Android.
//!
//! Mirror of `crates/lakko-ffi-wasm/src/moderation.rs`, minus the
//! moderators' panel (web only):
//!
//! - [`attest_message`]: wrap an outgoing chat envelope with the leaf's
//!   signature (section 1);
//! - [`unwrap_attested`] and [`message_attestation`]: the receive path. The
//!   client hands the unwrapped envelope to `parse_body_envelope` and keeps
//!   the attestation with the message;
//! - [`moderation_account_target`], [`report_reason_codes`],
//!   [`seal_moderation_report`]: the report sheet;
//! - [`seal_moderation_appeal`]: a suspended person's appeal (section 4);
//! - [`moderation_bucket_prefix`] and [`moderation_account_standing`]: the
//!   suspension mark, from a snapshot bucket answer checked against the
//!   pinned moderation log key (section 5, step 4);
//! - [`moderation_channel_bucket_prefix`] and
//!   [`moderation_channel_standing`]: the same for a channel's removal
//!   (step 5);
//! - [`moderation_prove_account_standing`] and
//!   [`moderation_prove_channel_standing`]: the proof a client needs before
//!   it acts on a suspended or removed standing: the moderator's signed
//!   statement behind the snapshot entry (security fix F2).
//!
//! Every refusal is [`FfiError::Moderation`] with the message
//! `"<code>: <reason>"`.
//!
//! [ADR-0098]: ../../../docs/adr/0098-reporting-and-moderation.md

use std::sync::Arc;

use lakko_attachments::attested;
use lakko_moderation::moderators::ModeratorList;
use lakko_moderation::report::{seal_report, ConversationKind, EvidenceItem, ReportContent};
use lakko_moderation::snapshot::{
    account_prefix_hex, channel_prefix_hex, check_account, check_target, prove_standing,
    SnapshotError, Standing, TargetStanding,
};
use lakko_moderation::target::{account_target_hash, Target, TargetKind};
use lakko_moderation::{ModerationError, Reason};

use crate::{FfiError, Identity};

fn err(e: &ModerationError) -> FfiError {
    FfiError::Moderation(format!("{}: {e}", e.code()))
}

fn attest_err(e: &attested::AttestError) -> FfiError {
    FfiError::Moderation(format!("{}: {e}", e.code()))
}

/// What a message's signature showed (ADR-0098 section 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiAttestation {
    /// The signature verified under the sender's leaf key.
    Verified,
    /// The wrapper was there but did not verify, or was malformed.
    Failed,
    /// The message carried no wrapper.
    Absent,
}

/// Wrap the outgoing chat `envelope` for the conversation `group_id` with a
/// signature by `identity`'s conversation leaf key. Service envelopes are
/// refused: only chat messages are attested.
///
/// # Errors
///
/// [`FfiError::Moderation`] (`bad-group-id`, `not-a-chat-envelope`).
#[uniffi::export]
pub fn attest_message(
    identity: Arc<Identity>,
    group_id: Vec<u8>,
    envelope: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let seed = identity.inner.ed25519_seed_bytes();
    attested::attest(
        &seed,
        Some(identity.inner.signature_public_key()),
        &group_id,
        &envelope,
    )
    .map_err(|e| attest_err(&e))
}

/// The envelope inside an attested plaintext, or the plaintext unchanged
/// when it carries no wrapper or a malformed one (a malformed wrapper then
/// classifies as a service envelope and is dropped).
#[uniffi::export]
#[must_use]
pub fn unwrap_attested(plaintext: Vec<u8>) -> Vec<u8> {
    match attested::split(&plaintext) {
        Ok((_, envelope)) => envelope.to_vec(),
        Err(_) => plaintext,
    }
}

/// What the signature of a received plaintext shows, checked against the
/// sender leaf key resolved at receipt.
#[uniffi::export]
#[must_use]
pub fn message_attestation(
    plaintext: Vec<u8>,
    group_id: Vec<u8>,
    sender_leaf_key: Vec<u8>,
) -> FfiAttestation {
    match lakko_attachments::parse_attested_body(&plaintext, &group_id, &sender_leaf_key).1 {
        lakko_attachments::Attestation::Verified => FfiAttestation::Verified,
        lakko_attachments::Attestation::Failed => FfiAttestation::Failed,
        lakko_attachments::Attestation::Absent => FfiAttestation::Absent,
    }
}

/// The 32-byte report target of an account, from its account key.
///
/// # Errors
///
/// [`FfiError::Moderation`] (`invalid`) for a key that is not 32 bytes.
#[uniffi::export]
pub fn moderation_account_target(account_key: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    if account_key.len() != 32 {
        return Err(err(&ModerationError::Invalid("account key length")));
    }
    Ok(account_target_hash(&account_key).to_vec())
}

/// The report reasons' wire bytes, in the order the sheet lists them.
#[uniffi::export]
#[must_use]
pub fn report_reason_codes() -> Vec<u8> {
    Reason::ALL.iter().map(|r| r.as_u8()).collect()
}

/// The stable name of a reason byte (`spam`, `child-safety`, ...), or `None`.
#[uniffi::export]
#[must_use]
pub fn report_reason_name(code: u8) -> Option<String> {
    Reason::from_u8(code).ok().map(|r| r.name().to_owned())
}

/// Verify a moderator list fetched from the moderation service under the
/// pinned `root_key` and return its version. A client keeps the highest
/// version it has seen and refuses a lower one (ADR-0098 section 3).
///
/// # Errors
///
/// [`FfiError::Moderation`] when the list does not verify.
#[uniffi::export]
pub fn moderator_list_version(moderator_list: Vec<u8>, root_key: Vec<u8>) -> Result<u64, FfiError> {
    let root: [u8; 32] = root_key
        .try_into()
        .map_err(|_| err(&ModerationError::Invalid("root key length")))?;
    ModeratorList::verify(&moderator_list, &root)
        .map(|l| l.list_version)
        .map_err(|e| err(&e))
}

/// One piece of evidence a report carries.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiEvidenceItem {
    /// A conversation message as received: the attested plaintext, the
    /// conversation's group id, the sender leaf key and the leaf's device
    /// binding (`Group.deviceBindingAt`).
    Message {
        /// The conversation's group id.
        group_id: Vec<u8>,
        /// The decrypted plaintext, wrapper included.
        plaintext: Vec<u8>,
        /// The sender leaf's signature key (32 bytes).
        leaf_key: Vec<u8>,
        /// The leaf's binding bytes, or empty.
        binding: Vec<u8>,
    },
    /// A contact request as received.
    ContactRequest {
        /// The request wire.
        wire: Vec<u8>,
    },
    /// A channel post as received.
    ChannelPost {
        /// The channel id (16 bytes).
        channel_id: Vec<u8>,
        /// The post wire.
        post: Vec<u8>,
    },
    /// A Discover listing as shown.
    Listing {
        /// The listing wire.
        wire: Vec<u8>,
    },
}

fn item(i: FfiEvidenceItem) -> Result<EvidenceItem, FfiError> {
    let bad = |what| err(&ModerationError::Invalid(what));
    Ok(match i {
        FfiEvidenceItem::Message {
            group_id,
            plaintext,
            leaf_key,
            binding,
        } => EvidenceItem::Message {
            group_id,
            plaintext,
            leaf_key: leaf_key.try_into().map_err(|_| bad("leaf key length"))?,
            binding,
        },
        FfiEvidenceItem::ContactRequest { wire } => EvidenceItem::ContactRequest { wire },
        FfiEvidenceItem::ChannelPost { channel_id, post } => EvidenceItem::ChannelPost {
            channel_id: channel_id
                .try_into()
                .map_err(|_| bad("channel id length"))?,
            post,
        },
        FfiEvidenceItem::Listing { wire } => EvidenceItem::Listing { wire },
    })
}

/// Build a report: the moderator list is verified under the pinned
/// `root_key`, and the comment and evidence are sealed to its moderators.
/// The result is the body of `POST /moderation/report`.
///
/// `target_kind`: 1 account, 2 group handle, 3 channel, 4 name.
/// `conversation_kind`: 1 one-to-one, 2 group, 3 channel, 4 contact request,
/// 5 Discover, 6 a card.
///
/// # Errors
///
/// [`FfiError::Moderation`] with the codes of `ModerationError`.
#[uniffi::export]
#[allow(clippy::too_many_arguments)]
pub fn seal_moderation_report(
    target_kind: u8,
    target: Vec<u8>,
    reason: u8,
    conversation_kind: u8,
    comment: String,
    evidence: Vec<FfiEvidenceItem>,
    moderator_list: Vec<u8>,
    root_key: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let root: [u8; 32] = root_key
        .try_into()
        .map_err(|_| err(&ModerationError::Invalid("root key length")))?;
    let list = ModeratorList::verify(&moderator_list, &root).map_err(|e| err(&e))?;
    let kind = TargetKind::from_u8(target_kind).map_err(|e| err(&e))?;
    if kind == TargetKind::Blob {
        return Err(err(&ModerationError::Invalid("a blob is not reported")));
    }
    let content = ReportContent {
        comment,
        evidence: evidence.into_iter().map(item).collect::<Result<_, _>>()?,
    };
    seal_report(
        Target::new(kind, &target).map_err(|e| err(&e))?,
        Reason::from_u8(reason).map_err(|e| err(&e))?,
        ConversationKind::from_u8(conversation_kind).map_err(|e| err(&e))?,
        &content,
        list.list_version,
        &list.recipients(),
    )
    .map_err(|e| err(&e))
}

/// Seal a suspended person's appeal: the moderator list is verified under
/// the pinned `root_key`, and the trimmed `text` (1 to 1,000 characters) is
/// sealed to its current moderators. The result is the `sealed_b64` of
/// `POST /account/suspension/appeal`.
///
/// # Errors
///
/// [`FfiError::Moderation`] with the codes of `ModerationError` (`invalid`
/// for an empty or over-long text or a bad root key, `bad-signature` for a
/// list that does not verify).
#[uniffi::export]
pub fn seal_moderation_appeal(
    text: String,
    moderator_list: Vec<u8>,
    root_key: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let root: [u8; 32] = root_key
        .try_into()
        .map_err(|_| err(&ModerationError::Invalid("root key length")))?;
    let list = ModeratorList::verify(&moderator_list, &root).map_err(|e| err(&e))?;
    lakko_moderation::appeal::seal_appeal(&text, &list).map_err(|e| err(&e))
}

// ---------------------------------------------------------------------------
// The suspension snapshot (section 5, step 4).
// ---------------------------------------------------------------------------

fn snapshot_err(e: &SnapshotError) -> FfiError {
    FfiError::Moderation(format!("{}: {e}", e.code()))
}

fn account_key_32(account_key: &[u8]) -> Result<[u8; 32], FfiError> {
    account_key
        .try_into()
        .map_err(|_| err(&ModerationError::Invalid("account key length")))
}

fn ms_i64(ms: u64) -> i64 {
    i64::try_from(ms).unwrap_or(i64::MAX)
}

/// An account's standing, read from a checked snapshot bucket answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct ModerationStanding {
    /// Whether the account is suspended at the time given.
    pub suspended: bool,
    /// The end of the suspension (unix ms); `None` for no end, or when not
    /// suspended.
    pub until_ms: Option<i64>,
    /// The snapshot's period.
    pub period: u64,
    /// Until when the answer may be reused for the same prefix (unix ms).
    pub reuse_until_ms: i64,
    /// The moderation log index of the statement behind the target's
    /// snapshot entry, when the bucket holds one: the entry a client fetches
    /// (`GET /moderation/log/entry/{logIndex}?size={log_size}`) for
    /// [`moderation_prove_account_standing`]. `None` when not suspended and
    /// no entry is held.
    pub log_index: Option<u64>,
}

/// The four lower-case hex characters of an account's snapshot bucket, for
/// `GET /moderation/snapshot/latest/bucket/{prefix}`.
///
/// # Errors
///
/// [`FfiError::Moderation`] (`invalid`) for a key that is not 32 bytes.
#[uniffi::export]
pub fn moderation_bucket_prefix(account_key: Vec<u8>) -> Result<String, FfiError> {
    Ok(account_prefix_hex(&account_key_32(&account_key)?))
}

/// Check a snapshot bucket answer (the route's JSON body) against the pinned
/// moderation log operator key and, when `witness_keys` is not empty, a
/// quorum of `witness_threshold` of them; then read `account_key`'s
/// standing at `now_ms`.
///
/// # Errors
///
/// [`FfiError::Moderation`] with the codes of
/// `lakko_moderation::snapshot::SnapshotError` (`too-large`, `malformed`,
/// `bad-key`, `bad-root-signature`, `witness-quorum`,
/// `snapshot-not-included`, `bucket-not-included`, `wrong-period`,
/// `wrong-prefix`, `stale`, `malformed-snapshot`), or `invalid` for a key
/// that is not 32 bytes.
#[uniffi::export]
pub fn moderation_account_standing(
    answer_json: String,
    account_key: Vec<u8>,
    log_operator_key: Vec<u8>,
    witness_keys: Vec<Vec<u8>>,
    witness_threshold: u32,
    now_ms: i64,
) -> Result<ModerationStanding, FfiError> {
    let account = account_key_32(&account_key)?;
    let operator: [u8; 32] = log_operator_key
        .as_slice()
        .try_into()
        .map_err(|_| snapshot_err(&SnapshotError::BadKey))?;
    let keys: Vec<&[u8]> = witness_keys.iter().map(Vec::as_slice).collect();
    let threshold = usize::try_from(witness_threshold).unwrap_or(usize::MAX);
    let got = check_account(
        answer_json.as_bytes(),
        &account,
        &operator,
        &keys,
        threshold,
        now_ms,
    )
    .map_err(|e| snapshot_err(&e))?;
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

fn channel_id_16(channel_id: &[u8]) -> Result<[u8; 16], FfiError> {
    channel_id
        .try_into()
        .map_err(|_| err(&ModerationError::Invalid("channel id length")))
}

/// The four lower-case hex characters of a channel's snapshot bucket (the
/// first two bytes of its 16-byte id), for
/// `GET /moderation/snapshot/latest/bucket/{prefix}`.
///
/// # Errors
///
/// [`FfiError::Moderation`] (`invalid`) for an id that is not 16 bytes.
#[uniffi::export]
pub fn moderation_channel_bucket_prefix(channel_id: Vec<u8>) -> Result<String, FfiError> {
    Ok(channel_prefix_hex(&channel_id_16(&channel_id)?))
}

/// Check a snapshot bucket answer as [`moderation_account_standing`] does,
/// then read whether the channel `channel_id` was removed at `now_ms`:
/// `suspended` is true for a removed channel, and `until_ms` is always
/// `None` (a removal has no end).
///
/// # Errors
///
/// The codes of [`moderation_account_standing`]; `invalid` for an id that
/// is not 16 bytes.
#[uniffi::export]
pub fn moderation_channel_standing(
    answer_json: String,
    channel_id: Vec<u8>,
    log_operator_key: Vec<u8>,
    witness_keys: Vec<Vec<u8>>,
    witness_threshold: u32,
    now_ms: i64,
) -> Result<ModerationStanding, FfiError> {
    let channel = channel_id_16(&channel_id)?;
    let operator: [u8; 32] = log_operator_key
        .as_slice()
        .try_into()
        .map_err(|_| snapshot_err(&SnapshotError::BadKey))?;
    let keys: Vec<&[u8]> = witness_keys.iter().map(Vec::as_slice).collect();
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
    .map_err(|e| snapshot_err(&e))?;
    Ok(standing_record(got))
}

fn root_key_32(moderation_root_key: &[u8]) -> Result<[u8; 32], FfiError> {
    moderation_root_key
        .try_into()
        .map_err(|_| snapshot_err(&SnapshotError::BadKey))
}

/// Prove an account's suspended standing from the moderator's signed
/// statement behind it. `answer_json` is the bucket answer already checked
/// with [`moderation_account_standing`]; `entry_json` is the answer of
/// `GET /moderation/log/entry/{logIndex}?size={log_size}` (the standing's
/// `log_index` and the answer head's `log_size`); `moderator_list` is the
/// root-signed list (`GET /moderation/moderators`), verified here under the
/// pinned `moderation_root_key`. A client marks, drops, hides or refuses
/// anything on a suspended standing only after this returns.
///
/// # Errors
///
/// [`FfiError::Moderation`] with the codes of
/// `lakko_moderation::snapshot::SnapshotError`: `no-entry`,
/// `not-suspended`, `entry-mismatch`, `entry-not-included`,
/// `not-a-statement`, `bad-list`, `not-authorised`, `bad-statement`,
/// `wrong-action`, `wrong-target`, `until-mismatch`, the shape codes of
/// [`moderation_account_standing`], `bad-key` for a root key that is not 32
/// bytes, `invalid` for an account key that is not 32 bytes.
#[uniffi::export]
pub fn moderation_prove_account_standing(
    answer_json: String,
    entry_json: String,
    moderator_list: Vec<u8>,
    moderation_root_key: Vec<u8>,
    account_key: Vec<u8>,
    now_ms: i64,
) -> Result<(), FfiError> {
    let account = account_key_32(&account_key)?;
    let root = root_key_32(&moderation_root_key)?;
    prove_standing(
        answer_json.as_bytes(),
        entry_json.as_bytes(),
        &moderator_list,
        &root,
        TargetKind::Account,
        &account_target_hash(&account),
        now_ms,
    )
    .map_err(|e| snapshot_err(&e))
}

/// Prove a channel's removal from the moderator's signed statement behind
/// it, as [`moderation_prove_account_standing`] does for an account.
///
/// # Errors
///
/// The codes of [`moderation_prove_account_standing`]; `invalid` for an id
/// that is not 16 bytes.
#[uniffi::export]
pub fn moderation_prove_channel_standing(
    answer_json: String,
    entry_json: String,
    moderator_list: Vec<u8>,
    moderation_root_key: Vec<u8>,
    channel_id: Vec<u8>,
    now_ms: i64,
) -> Result<(), FfiError> {
    let channel = channel_id_16(&channel_id)?;
    let root = root_key_32(&moderation_root_key)?;
    prove_standing(
        answer_json.as_bytes(),
        entry_json.as_bytes(),
        &moderator_list,
        &root,
        TargetKind::Channel,
        &channel,
        now_ms,
    )
    .map_err(|e| snapshot_err(&e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_moderation::moderators::{ModeratorEntry, ModeratorKey};
    use lakko_moderation::report::open_report;
    use lakko_moderation::seal::moderator_report_secret;

    #[test]
    fn a_report_built_here_opens_for_the_moderator() {
        let secret = moderator_report_secret(&[1; 32]).unwrap();
        let root = [42u8; 32];
        let list = ModeratorList {
            list_version: 1,
            issued_at_ms: 1,
            rp_id: "m.example".into(),
            origin: "https://m.example".into(),
            entries: vec![ModeratorEntry {
                moderator_id: [1; 16],
                name: "m".into(),
                key: ModeratorKey::Ed25519([1; 32]),
                credential_id: vec![1],
                x25519_public: secret.public_key().to_bytes(),
                added_at_ms: 0,
                revoked_at_ms: 0,
            }],
        }
        .sign(&root)
        .unwrap();
        let root_pub = lakko_crypto::signing::SigningKey::from_seed(&root)
            .verifying_key()
            .to_bytes()
            .to_vec();
        let target = moderation_account_target(vec![7; 32]).unwrap();
        let report = seal_moderation_report(
            1,
            target.clone(),
            1,
            6,
            "spam".into(),
            vec![FfiEvidenceItem::ContactRequest { wire: vec![1, 2] }],
            list.clone(),
            root_pub.clone(),
        )
        .unwrap();
        assert_eq!(
            moderator_list_version(list.clone(), root_pub.clone()).unwrap(),
            1
        );
        assert!(moderator_list_version(list.clone(), vec![0; 32]).is_err());
        let appeal =
            seal_moderation_appeal(" please ".into(), list.clone(), root_pub.clone()).unwrap();
        assert_eq!(
            lakko_moderation::appeal::open_appeal(&appeal, &[1; 16], &secret).unwrap(),
            "please"
        );
        assert!(seal_moderation_appeal("please".into(), list.clone(), vec![0; 32]).is_err());
        assert!(seal_moderation_appeal("  ".into(), list.clone(), root_pub.clone()).is_err());
        let (clear, content) = open_report(&report, &[1; 16], &secret).unwrap();
        assert_eq!(clear.target.bytes(), target.as_slice());
        assert_eq!(content.comment, "spam");
        assert!(
            seal_moderation_report(1, target, 1, 6, String::new(), vec![], list, vec![0; 32])
                .is_err()
        );
    }

    #[test]
    fn the_receive_helpers_agree_with_the_core() {
        let identity = Arc::new(Identity {
            inner: lakko_mls::Identity::from_seed(&[5; 64]).unwrap(),
        });
        let env = lakko_attachments::compose("hi", Vec::new()).unwrap();
        let wire = attest_message(identity.clone(), b"g".to_vec(), env.clone()).unwrap();
        assert_eq!(unwrap_attested(wire.clone()), env);
        assert_eq!(unwrap_attested(env.clone()), env);
        let key = identity.inner.signature_public_key().to_vec();
        assert_eq!(
            message_attestation(wire.clone(), b"g".to_vec(), key.clone()),
            FfiAttestation::Verified
        );
        assert_eq!(
            message_attestation(wire, b"h".to_vec(), key.clone()),
            FfiAttestation::Failed
        );
        assert_eq!(
            message_attestation(env, b"g".to_vec(), key),
            FfiAttestation::Absent
        );
        assert_eq!(report_reason_codes(), vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(report_reason_name(6).as_deref(), Some("child-safety"));
        assert_eq!(report_reason_name(0), None);
    }

    #[test]
    fn the_snapshot_check_is_wired() {
        use lakko_crypto::signing::SigningKey;
        use lakko_moderation::snapshot::{build_snapshot, BucketAnswer};
        use lakko_transparency::merkle::hash_leaf;
        use lakko_transparency::SignedRoot;

        let account = vec![7u8; 32];
        let built_at = 1_800_000_000_000u64;
        let snapshot = build_snapshot(&[], built_at, 86_400).unwrap();
        let leaf = snapshot.root().log_leaf();
        let tree_root = hash_leaf(&leaf);
        let op = SigningKey::from_seed(&[11; 32]);
        let head = SignedRoot {
            tree_root,
            log_size: 1,
            timestamp_unix: 1,
            signature: op.sign(&SignedRoot::signing_payload(&tree_root, 1, 1)),
        };
        let prefix = moderation_bucket_prefix(account.clone()).unwrap();
        let answer = BucketAnswer::new(
            &snapshot,
            u16::from_str_radix(&prefix, 16).unwrap(),
            0,
            &[],
            &head,
            &[],
        );
        let json = serde_json::to_string(&answer).unwrap();
        let op_pub = op.verifying_key().to_bytes().to_vec();
        let got = moderation_account_standing(
            json.clone(),
            account.clone(),
            op_pub.clone(),
            vec![],
            1,
            1_800_000_001_000,
        )
        .unwrap();
        assert!(!got.suspended);
        assert_eq!(got.until_ms, None);
        assert_eq!(got.reuse_until_ms, 1_800_086_400_000);
        let msg = |e: FfiError| match e {
            FfiError::Moderation(m) => m,
            _ => String::new(),
        };
        assert!(msg(moderation_account_standing(
            json.clone(),
            account.clone(),
            op_pub.clone(),
            vec![vec![1; 32]],
            1,
            1_800_000_001_000
        )
        .unwrap_err())
        .starts_with("witness-quorum: "));
        assert!(msg(moderation_account_standing(
            "{}".into(),
            account.clone(),
            op_pub,
            vec![],
            1,
            0
        )
        .unwrap_err())
        .starts_with("malformed: "));
        assert!(msg(
            moderation_account_standing(json, account, vec![1; 31], vec![], 1, 0).unwrap_err()
        )
        .starts_with("bad-key: "));
        assert!(moderation_bucket_prefix(vec![1; 31]).is_err());
    }

    #[test]
    fn a_removed_channel_reads_removed() {
        use lakko_crypto::signing::SigningKey;
        use lakko_moderation::snapshot::{build_snapshot, BucketAnswer, LOG_LEAF_STATEMENT};
        use lakko_moderation::statement::{reason_commitment, Action, SignedStatement, Statement};
        use lakko_moderation::webauthn::Assertion;
        use lakko_transparency::merkle::{audit_path, root as merkle_root};
        use lakko_transparency::SignedRoot;

        let channel = vec![0x3cu8; 16];
        let statement = Statement {
            statement_id: [1; 16],
            action: Action::ChannelRemove,
            target: Target::new(TargetKind::Channel, &channel).unwrap(),
            reason_commitment: reason_commitment(Reason::Spam, &[1; 16]),
            until_ms: 0,
            issued_at_ms: 10,
            moderator_id: [2; 16],
            list_version: 1,
        };
        let mut leaf = vec![LOG_LEAF_STATEMENT];
        leaf.extend_from_slice(
            &SignedStatement {
                statement: statement.encode(),
                assertion: Assertion {
                    authenticator_data: vec![1],
                    client_data_json: vec![2],
                    signature: vec![3],
                },
            }
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
        let prefix = moderation_channel_bucket_prefix(channel.clone()).unwrap();
        assert_eq!(prefix, "3c3c");
        let answer = BucketAnswer::new(
            &snapshot,
            u16::from_str_radix(&prefix, 16).unwrap(),
            1,
            &audit_path(&log, 1),
            &head,
            &[],
        );
        let json = serde_json::to_string(&answer).unwrap();
        let op_pub = op.verifying_key().to_bytes().to_vec();
        let got = moderation_channel_standing(
            json.clone(),
            channel.clone(),
            op_pub.clone(),
            vec![],
            1,
            1_800_000_001_000,
        )
        .unwrap();
        assert!(got.suspended);
        assert_eq!(got.until_ms, None);
        let mut other = channel;
        other[15] ^= 1;
        assert!(
            !moderation_channel_standing(
                json.clone(),
                other,
                op_pub.clone(),
                vec![],
                1,
                1_800_000_001_000
            )
            .unwrap()
            .suspended
        );
        assert!(moderation_channel_standing(json, vec![1; 32], op_pub, vec![], 1, 0).is_err());
        assert!(moderation_channel_bucket_prefix(vec![1; 15]).is_err());
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn a_suspension_is_proven_from_its_signed_statement() {
        use base64::engine::general_purpose::STANDARD as B64;
        use base64::Engine as _;
        use lakko_crypto::signing::SigningKey;
        use lakko_moderation::moderators::ModeratorEntry;
        use lakko_moderation::snapshot::{
            build_snapshot, BucketAnswer, LogEntryAnswer, LOG_LEAF_STATEMENT,
        };
        use lakko_moderation::software_authenticator::SoftwareAuthenticator;
        use lakko_moderation::statement::{reason_commitment, Action, Statement};
        use lakko_transparency::merkle::{audit_path, root as merkle_root};
        use lakko_transparency::SignedRoot;

        let root_seed = [42u8; 32];
        let root_pub = SigningKey::from_seed(&root_seed)
            .verifying_key()
            .to_bytes()
            .to_vec();
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
        let account = vec![7u8; 32];
        let built_at = 1_800_000_000_000u64;
        let until = built_at + 50_000;
        let statement = Statement {
            statement_id: [1; 16],
            action: Action::Suspend,
            target: Target::account(&[7; 32]).unwrap(),
            reason_commitment: reason_commitment(Reason::Spam, &[1; 16]),
            until_ms: until,
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
        let prefix = moderation_bucket_prefix(account.clone()).unwrap();
        let answer = serde_json::to_string(&BucketAnswer::new(
            &snapshot,
            u16::from_str_radix(&prefix, 16).unwrap(),
            1,
            &audit_path(&log, 1),
            &head,
            &[],
        ))
        .unwrap();
        let now = 1_800_000_001_000;
        let standing = moderation_account_standing(
            answer.clone(),
            account.clone(),
            op.verifying_key().to_bytes().to_vec(),
            vec![],
            1,
            now,
        )
        .unwrap();
        assert!(standing.suspended);
        assert_eq!(standing.log_index, Some(0));
        let entry = |index: u64| {
            serde_json::to_string(&LogEntryAnswer {
                index,
                leaf_b64: B64.encode(&log[0]),
                audit_path_b64: audit_path(&log, 0).iter().map(|h| B64.encode(h)).collect(),
                log_size: 2,
            })
            .unwrap()
        };
        moderation_prove_account_standing(
            answer.clone(),
            entry(0),
            list_bytes.clone(),
            root_pub.clone(),
            account.clone(),
            now,
        )
        .unwrap();
        let msg = |e: FfiError| match e {
            FfiError::Moderation(m) => m,
            _ => String::new(),
        };
        assert!(msg(moderation_prove_account_standing(
            answer.clone(),
            entry(1),
            list_bytes.clone(),
            root_pub.clone(),
            account.clone(),
            now,
        )
        .unwrap_err())
        .starts_with("entry-mismatch: "));
        assert!(msg(moderation_prove_account_standing(
            answer.clone(),
            entry(0),
            list_bytes.clone(),
            vec![1; 32],
            account.clone(),
            now,
        )
        .unwrap_err())
        .starts_with("bad-list: "));
        assert!(msg(moderation_prove_account_standing(
            answer.clone(),
            entry(0),
            list_bytes.clone(),
            vec![1; 31],
            account.clone(),
            now,
        )
        .unwrap_err())
        .starts_with("bad-key: "));
        // The account's own bucket answer does not prove a channel.
        assert!(moderation_prove_channel_standing(
            answer,
            entry(0),
            list_bytes,
            root_pub,
            vec![7; 16],
            now,
        )
        .is_err());
    }
}
