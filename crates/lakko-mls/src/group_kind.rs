// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A conversation's kind, carried in the `0xFF03` GroupContext extension
//! ([ADR-0062]).
//!
//! The creator writes the kind into the GroupContext at
//! [`crate::Group::create`]; no commit ever changes it, and every member reads
//! it from there, so every device of every member gives the same answer from
//! the moment it joins. A **one-to-one** keeps its two people: every member's
//! device refuses a commit that changes the set of accounts in it (a person's
//! own devices may still come and go, ADR-0050). A **group** follows the
//! ordinary ADR-0022 / ADR-0045 membership rules.
//!
//! The wire body is exactly two bytes:
//!
//! ```text
//! version: u8   // GROUP_KIND_VERSION; anything else => refuse
//! kind:    u8   // 0x01 one-to-one, 0x02 group; 0x00 and > 0x02 reserved
//! ```
//!
//! Decoding is bounded and fail-closed in the style of the role-table codec:
//! exact length first, then the version, then the kind. Any other input is an
//! error, never a default.
//!
//! `GroupKind` is deliberately not called `ConversationKind`:
//! `lakko-ui-rules::ConversationKind` (`Chat` / `Group` / `Channel`) crosses the
//! same FFI surfaces and names a different thing — a list a row belongs to
//! rather than a membership rule.
//!
//! [ADR-0062]: ../../../docs/adr/0062-conversation-kind.md

use openmls::prelude::{Extension, ExtensionType, Extensions, GroupContext};

use crate::error::{Error, Result};

/// The group-kind GroupContext extension type (ADR-0062). In the MLS
/// private-use range beside `0xFF01` (ADR-0015) and `0xFF02` (ADR-0022), so
/// OpenMLS surfaces it as `Extension::Unknown`.
pub(crate) const GROUP_KIND_EXTENSION_TYPE: u16 = 0xFF03;

/// Current wire version of the `0xFF03` body. An unknown version is refused.
const GROUP_KIND_VERSION: u8 = 1;

/// Exact length of an `0xFF03` body: `version(1) ‖ kind(1)`.
pub const GROUP_KIND_BODY_LEN: usize = 2;

/// Wire byte of [`GroupKind::OneToOne`].
const KIND_ONE_TO_ONE: u8 = 0x01;

/// Wire byte of [`GroupKind::Group`].
const KIND_GROUP: u8 = 0x02;

/// What kind of conversation a [`crate::Group`] is (ADR-0062).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GroupKind {
    /// Two people, always the same two. No commit may remove a person or add a
    /// third; each person's own devices may still join and leave.
    OneToOne,
    /// Any number of people, under the ADR-0022 role table and the ADR-0045
    /// proposal rules. A group with two people is still a group.
    Group,
}

impl GroupKind {
    /// The canonical two-byte `0xFF03` body for this kind.
    #[must_use]
    pub const fn encode(self) -> [u8; GROUP_KIND_BODY_LEN] {
        let kind = match self {
            Self::OneToOne => KIND_ONE_TO_ONE,
            Self::Group => KIND_GROUP,
        };
        [GROUP_KIND_VERSION, kind]
    }

    /// Decode an `0xFF03` body. Fail-closed: exact length, then the version,
    /// then the kind byte; anything else is an error.
    ///
    /// # Errors
    ///
    /// [`Error::Codec`] for a body that is not exactly
    /// [`GROUP_KIND_BODY_LEN`] bytes, carries an unknown version, or names a
    /// reserved kind.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let [version, kind] = bytes else {
            return Err(Error::Codec(format!(
                "group kind body must be {GROUP_KIND_BODY_LEN} bytes, got {}",
                bytes.len()
            )));
        };
        if *version != GROUP_KIND_VERSION {
            return Err(Error::Codec(format!(
                "group kind version {version} is not supported"
            )));
        }
        match *kind {
            KIND_ONE_TO_ONE => Ok(Self::OneToOne),
            KIND_GROUP => Ok(Self::Group),
            other => Err(Error::Codec(format!(
                "group kind byte {other:#04x} is reserved"
            ))),
        }
    }

    /// The `0xFF03` GroupContext extension carrying this kind.
    pub(crate) fn extension(self) -> Extension {
        Extension::Unknown(
            GROUP_KIND_EXTENSION_TYPE,
            openmls::prelude::UnknownExtension(self.encode().to_vec()),
        )
    }
}

/// The group-kind facts one GroupContext extension list carries: the raw
/// `0xFF03` body, and whether `RequiredCapabilities` names `0xFF03`.
///
/// Compared **byte for byte** rather than decoded, because the rule is "no
/// commit changes the kind" (ADR-0062 section 4) and a decoded comparison would
/// let a commit swap one malformed body for another, or drop the extension from
/// a group whose body happened not to decode.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct KindState {
    /// The raw `0xFF03` body, or `None` when the list carries none.
    pub(crate) body: Option<Vec<u8>>,
    /// `true` when the list's `RequiredCapabilities` names `0xFF03`.
    pub(crate) required: bool,
}

impl KindState {
    /// Read the kind facts out of a GroupContext extension list.
    pub(crate) fn of(extensions: &Extensions<GroupContext>) -> Self {
        Self {
            body: extensions
                .unknown(GROUP_KIND_EXTENSION_TYPE)
                .map(|e| e.0.clone()),
            required: extensions.required_capabilities().is_some_and(|r| {
                r.extension_types()
                    .contains(&ExtensionType::Unknown(GROUP_KIND_EXTENSION_TYPE))
            }),
        }
    }

    /// The decoded kind, or `None` when there is no body or it does not decode.
    pub(crate) fn kind(&self) -> Option<GroupKind> {
        self.body.as_deref().and_then(|b| GroupKind::decode(b).ok())
    }

    /// `true` when a GroupContext rewrite from `self` to `after` leaves the
    /// kind as it was: the `0xFF03` body byte-identical (present or absent in
    /// both), and `0xFF03` not dropped from `RequiredCapabilities`.
    pub(crate) fn preserved_by(&self, after: &Self) -> bool {
        self.body == after.body && (!self.required || after.required)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn both_kinds_round_trip() {
        for kind in [GroupKind::OneToOne, GroupKind::Group] {
            assert_eq!(GroupKind::decode(&kind.encode()).expect("decode"), kind);
        }
    }

    #[test]
    fn the_wire_bytes_are_pinned() {
        assert_eq!(GroupKind::OneToOne.encode(), [0x01, 0x01]);
        assert_eq!(GroupKind::Group.encode(), [0x01, 0x02]);
        assert_eq!(GROUP_KIND_EXTENSION_TYPE, 0xFF03);
    }

    #[test]
    fn every_wrong_length_is_refused() {
        for len in [0usize, 1, 3, 4, 64] {
            let mut body = vec![0u8; len];
            if len >= 2 {
                body[0] = GROUP_KIND_VERSION;
                body[1] = KIND_GROUP;
            }
            assert!(
                GroupKind::decode(&body).is_err(),
                "length {len} must be refused"
            );
        }
    }

    #[test]
    fn every_other_version_is_refused() {
        for version in (0u8..=255).filter(|v| *v != GROUP_KIND_VERSION) {
            assert!(GroupKind::decode(&[version, KIND_ONE_TO_ONE]).is_err());
            assert!(GroupKind::decode(&[version, KIND_GROUP]).is_err());
        }
    }

    #[test]
    fn every_reserved_kind_byte_is_refused() {
        for kind in (0u8..=255).filter(|k| *k != KIND_ONE_TO_ONE && *k != KIND_GROUP) {
            assert!(
                GroupKind::decode(&[GROUP_KIND_VERSION, kind]).is_err(),
                "kind byte {kind:#04x} must be refused"
            );
        }
    }

    #[test]
    fn preservation_is_byte_identity_and_never_dropping_the_requirement() {
        let one = KindState {
            body: Some(GroupKind::OneToOne.encode().to_vec()),
            required: true,
        };
        let group = KindState {
            body: Some(GroupKind::Group.encode().to_vec()),
            required: true,
        };
        assert!(one.preserved_by(&one));
        assert!(!one.preserved_by(&group), "changing the kind");
        assert!(
            !one.preserved_by(&KindState {
                body: None,
                required: true
            }),
            "dropping the body"
        );
        assert!(
            !one.preserved_by(&KindState {
                body: one.body.clone(),
                required: false
            }),
            "dropping the requirement"
        );
        let absent = KindState::default();
        assert!(
            absent.preserved_by(&absent),
            "a kind-less group stays kind-less"
        );
        assert!(
            !absent.preserved_by(&one),
            "adding a kind later is a change too"
        );
    }

    proptest! {
        /// Anything that decodes re-encodes to the identical two bytes.
        #[test]
        fn decode_is_canonical(bytes in proptest::collection::vec(any::<u8>(), 0..8)) {
            if let Ok(kind) = GroupKind::decode(&bytes) {
                prop_assert_eq!(kind.encode().to_vec(), bytes);
            }
        }
    }
}
