// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The check that a reported message was written by an account ([ADR-0089]
//! section 1, UX section 5.3).
//!
//! A message item verifies when:
//!
//! 1. its attested wrapper verifies under the leaf key it names, in the
//!    group it names;
//! 2. the leaf resolves to an account key through its device binding (or is
//!    a Phase 1 leaf whose key is the account key).
//!
//! The binding check is ADR-0050's `account_key_of_credential`, which lives
//! in `lakko-mls`. It is passed in rather than depended on, so this crate,
//! which every applying service links, does not pull the MLS stack in. The
//! FFI layer that runs this check in the moderators' browser passes the real
//! one.
//!
//! What a verified item proves: the account's device signed this envelope
//! for this conversation. It does not prove when, that it was delivered, or
//! what else was said around it.
//!
//! Contact requests, channel posts and listings carry their own signatures;
//! their check is not in this module yet, and they report
//! [`EvidenceVerdict::NotVerified`].
//!
//! [ADR-0089]: ../../../docs/adr/0089-reporting-and-moderation.md

use lakko_attachments::attested;

use crate::report::EvidenceItem;
use crate::target::Target;

/// The outcome of checking one evidence item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceVerdict {
    /// Written by the account with this key.
    Verified {
        /// The author's account key.
        account_key: [u8; 32],
    },
    /// Not shown as written by anyone.
    NotVerified(&'static str),
}

impl EvidenceVerdict {
    /// Whether this verdict says the item was written by `target`'s account.
    #[must_use]
    pub fn is_by(&self, target: &Target) -> bool {
        match self {
            Self::Verified { account_key } => {
                Target::account(account_key).is_ok_and(|t| &t == target)
            },
            Self::NotVerified(_) => false,
        }
    }
}

/// Check one evidence item. `resolve(binding, leaf_key)` maps a leaf's
/// credential bytes and key to its account key, or `None`.
pub fn verify_message_evidence<F>(item: &EvidenceItem, resolve: F) -> EvidenceVerdict
where
    F: Fn(&[u8], &[u8]) -> Option<[u8; 32]>,
{
    match item {
        EvidenceItem::Message {
            group_id,
            plaintext,
            leaf_key,
            binding,
        } => {
            if attested::verify(plaintext, group_id, leaf_key).is_err() {
                return EvidenceVerdict::NotVerified("signature");
            }
            resolve(binding, leaf_key).map_or(
                EvidenceVerdict::NotVerified("device binding"),
                |account_key| EvidenceVerdict::Verified { account_key },
            )
        },
        EvidenceItem::ContactRequest { .. }
        | EvidenceItem::ChannelPost { .. }
        | EvidenceItem::Listing { .. } => EvidenceVerdict::NotVerified("not checked"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::signing::SigningKey;

    const LEAF_SEED: [u8; 32] = [3; 32];
    const ACCOUNT: [u8; 32] = [8; 32];

    fn leaf() -> [u8; 32] {
        SigningKey::from_seed(&LEAF_SEED).verifying_key().to_bytes()
    }

    fn resolver(binding: &[u8], leaf_key: &[u8]) -> Option<[u8; 32]> {
        (binding == b"ok" && leaf_key == leaf()).then_some(ACCOUNT)
    }

    fn item(group: &[u8], signed_group: &[u8], binding: &[u8]) -> EvidenceItem {
        let env = lakko_attachments::compose("hi", Vec::new()).unwrap();
        EvidenceItem::Message {
            group_id: group.to_vec(),
            plaintext: attested::attest(&LEAF_SEED, None, signed_group, &env).unwrap(),
            leaf_key: leaf(),
            binding: binding.to_vec(),
        }
    }

    #[test]
    fn a_signed_bound_message_names_its_account() {
        let v = verify_message_evidence(&item(b"g", b"g", b"ok"), resolver);
        assert_eq!(
            v,
            EvidenceVerdict::Verified {
                account_key: ACCOUNT
            }
        );
        assert!(v.is_by(&Target::account(&ACCOUNT).unwrap()));
        assert!(!v.is_by(&Target::account(&[9; 32]).unwrap()));
    }

    #[test]
    fn a_wrong_group_or_binding_is_not_verified() {
        assert_eq!(
            verify_message_evidence(&item(b"g", b"h", b"ok"), resolver),
            EvidenceVerdict::NotVerified("signature")
        );
        assert_eq!(
            verify_message_evidence(&item(b"g", b"g", b"no"), resolver),
            EvidenceVerdict::NotVerified("device binding")
        );
        let plain = EvidenceItem::Message {
            group_id: b"g".to_vec(),
            plaintext: lakko_attachments::compose("hi", Vec::new()).unwrap(),
            leaf_key: leaf(),
            binding: b"ok".to_vec(),
        };
        assert_eq!(
            verify_message_evidence(&plain, resolver),
            EvidenceVerdict::NotVerified("signature")
        );
    }
}
