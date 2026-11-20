// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The anonymous report ([ADR-0098] section 2).
//!
//! ```text
//! report  = DOMAIN ‖ version(1) ‖ clear_len(2) ‖ clear ‖ sealed
//! clear   = target_kind(1) ‖ target ‖ reason(1) ‖ conversation_kind(1) ‖ evidence_count(1)
//! sealed  = the seal module's layout, purpose Report, outer AAD = clear
//! content = comment_len(2) ‖ comment ‖ count(1) ‖ item × count
//! item    = kind(1) ‖ fields of that kind (below)
//! ```
//!
//! The clear part is what the moderation service groups, counts and ranks
//! cases by. It names the target and the reason, never the reporter. The
//! content (comment and evidence) is sealed to the moderators.
//!
//! [ADR-0098]: ../../../docs/adr/0098-reporting-and-moderation.md

use lakko_crypto::agreement::SecretKey;

use crate::reason::Reason;
use crate::seal::{self, Purpose, Recipient, MODERATOR_ID_LEN};
use crate::target::Target;
use crate::wire::{header, put16, put32, put8, Reader};
use crate::ModerationError;

/// The report's domain separator. Frozen.
pub const REPORT_DOMAIN: &[u8] = b"lakko/moderation/report/v1\0";
/// The only report version.
pub const REPORT_VERSION: u8 = 0x01;
/// The most evidence items a report carries (20, chosen, UX section 2.2).
pub const MAX_EVIDENCE: usize = 20;
/// The longest comment in characters (UX section 2.2).
pub const MAX_COMMENT_CHARS: usize = 500;
/// The longest comment in bytes.
pub const MAX_COMMENT_BYTES: usize = 2000;
/// The longest message plaintext, contact request or post in an item.
pub const MAX_ITEM_BYTES: usize = 64 * 1024;
/// The longest clear part.
const MAX_CLEAR: usize = 1 + 64 + 3;

/// Where the reported thing was seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ConversationKind {
    /// A one-to-one conversation.
    OneToOne = 1,
    /// A group.
    Group = 2,
    /// A channel.
    Channel = 3,
    /// A contact request.
    ContactRequest = 4,
    /// A Discover listing.
    Discover = 5,
    /// A person's card, with nothing included.
    Card = 6,
}

impl ConversationKind {
    /// Decode a wire byte.
    ///
    /// # Errors
    ///
    /// [`ModerationError::UnknownValue`].
    pub const fn from_u8(byte: u8) -> Result<Self, ModerationError> {
        Ok(match byte {
            1 => Self::OneToOne,
            2 => Self::Group,
            3 => Self::Channel,
            4 => Self::ContactRequest,
            5 => Self::Discover,
            6 => Self::Card,
            _ => return Err(ModerationError::UnknownValue("conversation kind")),
        })
    }
}

/// The clear part of a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportClear {
    /// What is reported.
    pub target: Target,
    /// Why.
    pub reason: Reason,
    /// Where it was seen.
    pub conversation_kind: ConversationKind,
    /// How many evidence items the sealed content holds.
    pub evidence_count: u8,
}

impl ReportClear {
    /// Encode.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(MAX_CLEAR);
        self.target.write(&mut out);
        out.push(self.reason.as_u8());
        out.push(self.conversation_kind as u8);
        out.push(self.evidence_count);
        out
    }

    /// Decode, exact.
    ///
    /// # Errors
    ///
    /// Any [`ModerationError`] decode variant.
    pub fn decode(bytes: &[u8]) -> Result<Self, ModerationError> {
        let mut r = Reader::new(bytes);
        let target = Target::read(&mut r)?;
        let reason = Reason::from_u8(r.u8()?)?;
        let conversation_kind = ConversationKind::from_u8(r.u8()?)?;
        let evidence_count = r.u8()?;
        if usize::from(evidence_count) > MAX_EVIDENCE {
            return Err(ModerationError::Malformed("evidence count"));
        }
        r.finish()?;
        Ok(Self {
            target,
            reason,
            conversation_kind,
            evidence_count,
        })
    }
}

/// One piece of evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceItem {
    /// A conversation message: the attested plaintext as received, the
    /// conversation's group id, the sender leaf's signature key and that
    /// leaf's device binding (empty for a leaf that is its account key).
    Message {
        /// The conversation's MLS group id.
        group_id: Vec<u8>,
        /// The decrypted application plaintext, attested wrapper included.
        plaintext: Vec<u8>,
        /// The sender leaf's Ed25519 signature key.
        leaf_key: [u8; 32],
        /// The leaf's device binding bytes (ADR-0050), or empty.
        binding: Vec<u8>,
    },
    /// A contact request as received (signed by its sender's identity key).
    ContactRequest {
        /// The request wire.
        wire: Vec<u8>,
    },
    /// A channel post as received (signed by its publisher).
    ChannelPost {
        /// The channel id.
        channel_id: [u8; 16],
        /// The post wire.
        post: Vec<u8>,
    },
    /// A Discover listing as shown.
    Listing {
        /// The listing wire.
        wire: Vec<u8>,
    },
}

impl EvidenceItem {
    fn write(&self, out: &mut Vec<u8>) -> Result<(), ModerationError> {
        match self {
            Self::Message {
                group_id,
                plaintext,
                leaf_key,
                binding,
            } => {
                if group_id.is_empty() {
                    return Err(ModerationError::Invalid("group id"));
                }
                out.push(1);
                put8(out, group_id, 255)?;
                put32(out, plaintext, MAX_ITEM_BYTES)?;
                out.extend_from_slice(leaf_key);
                put8(out, binding, 255)?;
            },
            Self::ContactRequest { wire } => {
                out.push(2);
                put32(out, wire, MAX_ITEM_BYTES)?;
            },
            Self::ChannelPost { channel_id, post } => {
                out.push(3);
                out.extend_from_slice(channel_id);
                put32(out, post, MAX_ITEM_BYTES)?;
            },
            Self::Listing { wire } => {
                out.push(4);
                put32(out, wire, MAX_ITEM_BYTES)?;
            },
        }
        Ok(())
    }

    fn read(r: &mut Reader<'_>) -> Result<Self, ModerationError> {
        Ok(match r.u8()? {
            1 => {
                let group_id = r.bytes8(255)?.to_vec();
                if group_id.is_empty() {
                    return Err(ModerationError::Malformed("group id"));
                }
                Self::Message {
                    group_id,
                    plaintext: r.bytes32(MAX_ITEM_BYTES)?.to_vec(),
                    leaf_key: r.array()?,
                    binding: r.bytes8(255)?.to_vec(),
                }
            },
            2 => Self::ContactRequest {
                wire: r.bytes32(MAX_ITEM_BYTES)?.to_vec(),
            },
            3 => Self::ChannelPost {
                channel_id: r.array()?,
                post: r.bytes32(MAX_ITEM_BYTES)?.to_vec(),
            },
            4 => Self::Listing {
                wire: r.bytes32(MAX_ITEM_BYTES)?.to_vec(),
            },
            _ => return Err(ModerationError::UnknownValue("evidence kind")),
        })
    }
}

/// The sealed content of a report.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReportContent {
    /// The reporter's comment, possibly empty.
    pub comment: String,
    /// The evidence.
    pub evidence: Vec<EvidenceItem>,
}

fn check_comment(comment: &str) -> Result<(), ModerationError> {
    if comment.len() > MAX_COMMENT_BYTES || comment.chars().count() > MAX_COMMENT_CHARS {
        return Err(ModerationError::Invalid("comment too long"));
    }
    if comment
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(ModerationError::Invalid("comment control character"));
    }
    Ok(())
}

impl ReportContent {
    /// Encode, refusing what [`Self::decode`] refuses.
    ///
    /// # Errors
    ///
    /// [`ModerationError::Invalid`].
    pub fn encode(&self) -> Result<Vec<u8>, ModerationError> {
        check_comment(&self.comment)?;
        if self.evidence.len() > MAX_EVIDENCE {
            return Err(ModerationError::Invalid("too much evidence"));
        }
        let mut out = Vec::new();
        put16(&mut out, self.comment.as_bytes(), MAX_COMMENT_BYTES)?;
        out.push(u8::try_from(self.evidence.len()).map_err(|_| ModerationError::Invalid("count"))?);
        for item in &self.evidence {
            item.write(&mut out)?;
        }
        Ok(out)
    }

    /// Decode, exact.
    ///
    /// # Errors
    ///
    /// Any [`ModerationError`] decode variant.
    pub fn decode(bytes: &[u8]) -> Result<Self, ModerationError> {
        let mut r = Reader::new(bytes);
        let comment = std::str::from_utf8(r.bytes16(MAX_COMMENT_BYTES)?)
            .map_err(|_| ModerationError::Malformed("comment utf-8"))?
            .to_owned();
        check_comment(&comment).map_err(|_| ModerationError::Malformed("comment"))?;
        let count = usize::from(r.u8()?);
        if count > MAX_EVIDENCE {
            return Err(ModerationError::Malformed("evidence count"));
        }
        let mut evidence = Vec::with_capacity(count);
        for _ in 0..count {
            evidence.push(EvidenceItem::read(&mut r)?);
        }
        r.finish()?;
        Ok(Self { comment, evidence })
    }
}

/// Build a report: the clear part from the arguments and the content's
/// evidence count, the content sealed to `recipients`.
///
/// # Errors
///
/// [`ModerationError::Invalid`] for content or recipients the seal refuses.
pub fn seal_report(
    target: Target,
    reason: Reason,
    conversation_kind: ConversationKind,
    content: &ReportContent,
    list_version: u64,
    recipients: &[Recipient],
) -> Result<Vec<u8>, ModerationError> {
    let content_bytes = content.encode()?;
    let clear = ReportClear {
        target,
        reason,
        conversation_kind,
        evidence_count: u8::try_from(content.evidence.len())
            .map_err(|_| ModerationError::Invalid("count"))?,
    }
    .encode();
    let sealed = seal::seal_for_moderators(
        Purpose::Report,
        &clear,
        &content_bytes,
        list_version,
        recipients,
    )?;
    let mut out = Vec::with_capacity(REPORT_DOMAIN.len() + 3 + clear.len() + sealed.len());
    out.extend_from_slice(REPORT_DOMAIN);
    out.push(REPORT_VERSION);
    put16(&mut out, &clear, MAX_CLEAR)?;
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// Split a report into its decoded clear part and its sealed bytes, without
/// opening anything. This is all the moderation service reads.
///
/// # Errors
///
/// Any [`ModerationError`] decode variant.
pub fn split_sealed(report: &[u8]) -> Result<(ReportClear, &[u8]), ModerationError> {
    let mut r = header(report, REPORT_DOMAIN, REPORT_VERSION)?;
    let clear = ReportClear::decode(r.bytes16(MAX_CLEAR)?)?;
    let sealed = r.remaining();
    seal::split(sealed)?;
    Ok((clear, sealed))
}

/// Open a report as one moderator.
///
/// # Errors
///
/// Decode errors, [`ModerationError::CannotOpen`], or
/// [`ModerationError::Invalid`] when the clear evidence count does not match
/// the content.
pub fn open_report(
    report: &[u8],
    moderator_id: &[u8; MODERATOR_ID_LEN],
    secret: &SecretKey,
) -> Result<(ReportClear, ReportContent), ModerationError> {
    let (clear, sealed) = split_sealed(report)?;
    let content = ReportContent::decode(&seal::open_for_moderator(
        Purpose::Report,
        &clear.encode(),
        sealed,
        moderator_id,
        secret,
    )?)?;
    if content.evidence.len() != usize::from(clear.evidence_count) {
        return Err(ModerationError::Invalid("evidence count mismatch"));
    }
    Ok((clear, content))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seal::moderator_report_secret;
    use crate::target::TargetKind;
    use proptest::prelude::*;

    fn moderator() -> (Recipient, SecretKey) {
        let secret = moderator_report_secret(&[1; 32]).unwrap();
        (
            Recipient {
                moderator_id: [1; 16],
                x25519_public: secret.public_key().to_bytes(),
            },
            secret,
        )
    }

    fn content() -> ReportContent {
        ReportContent {
            comment: "they keep writing\nat night".into(),
            evidence: vec![
                EvidenceItem::Message {
                    group_id: vec![9; 32],
                    plaintext: b"hello".to_vec(),
                    leaf_key: [2; 32],
                    binding: vec![3; 137],
                },
                EvidenceItem::ContactRequest { wire: vec![4; 50] },
                EvidenceItem::ChannelPost {
                    channel_id: [5; 16],
                    post: vec![6; 10],
                },
                EvidenceItem::Listing { wire: vec![] },
            ],
        }
    }

    #[test]
    fn a_report_round_trips_for_the_moderator() {
        let (m, secret) = moderator();
        let target = Target::account(&[7; 32]).unwrap();
        let report = seal_report(
            target.clone(),
            Reason::Harassment,
            ConversationKind::OneToOne,
            &content(),
            3,
            &[m],
        )
        .unwrap();
        let (clear, _) = split_sealed(&report).unwrap();
        assert_eq!(clear.target, target);
        assert_eq!(clear.reason, Reason::Harassment);
        assert_eq!(clear.evidence_count, 4);
        let (clear2, opened) = open_report(&report, &[1; 16], &secret).unwrap();
        assert_eq!(clear2, clear);
        assert_eq!(opened, content());
    }

    #[test]
    fn the_clear_part_cannot_be_swapped() {
        let (m, secret) = moderator();
        let report = seal_report(
            Target::account(&[7; 32]).unwrap(),
            Reason::Spam,
            ConversationKind::Group,
            &content(),
            1,
            &[m],
        )
        .unwrap();
        let (clear, _) = split_sealed(&report).unwrap();
        let mut other = clear.clone();
        other.target = Target::account(&[8; 32]).unwrap();
        let other_bytes = other.encode();
        let old = clear.encode();
        let mut swapped = REPORT_DOMAIN.to_vec();
        swapped.push(REPORT_VERSION);
        put16(&mut swapped, &other_bytes, MAX_CLEAR).unwrap();
        let header_len = REPORT_DOMAIN.len() + 1 + 2 + old.len();
        swapped.extend_from_slice(&report[header_len..]);
        assert_eq!(
            open_report(&swapped, &[1; 16], &secret),
            Err(ModerationError::CannotOpen)
        );
    }

    #[test]
    fn bounds_are_enforced() {
        let mut c = ReportContent {
            comment: "x".repeat(MAX_COMMENT_CHARS + 1),
            ..Default::default()
        };
        assert!(c.encode().is_err());
        c.comment = "a\u{0007}b".into();
        assert!(c.encode().is_err());
        c.comment = String::new();
        c.evidence = vec![EvidenceItem::Listing { wire: vec![] }; MAX_EVIDENCE + 1];
        assert!(c.encode().is_err());
        c.evidence = vec![EvidenceItem::Message {
            group_id: vec![],
            plaintext: vec![],
            leaf_key: [0; 32],
            binding: vec![],
        }];
        assert!(c.encode().is_err());
        assert!(ReportClear::decode(&[TargetKind::Channel as u8]).is_err());
    }

    proptest! {
        #[test]
        fn decoders_are_exact_and_canonical(bytes in proptest::collection::vec(any::<u8>(), 0..300)) {
            if let Ok(c) = ReportContent::decode(&bytes) {
                prop_assert_eq!(c.encode().unwrap(), bytes.clone());
            }
            if let Ok(c) = ReportClear::decode(&bytes) {
                prop_assert_eq!(c.encode(), bytes.clone());
            }
            let _ = split_sealed(&bytes);
        }

        #[test]
        fn content_round_trips(comment in "[a-z \n]{0,50}", n in 0usize..5, len in 0usize..40) {
            let c = ReportContent {
                comment,
                evidence: (0..n).map(|i| EvidenceItem::ContactRequest { wire: vec![u8::try_from(i).unwrap(); len] }).collect(),
            };
            prop_assert_eq!(ReportContent::decode(&c.encode().unwrap()).unwrap(), c);
        }
    }
}
