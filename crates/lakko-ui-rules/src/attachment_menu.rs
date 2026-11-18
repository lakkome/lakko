// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The paperclip menu (`docs/ux/attachments.md` section 2, ADR-0090
//! section 9): which rows a composer offers, in which order, and which are
//! shown disabled with a reason. Plus the poll editor's anonymity switch
//! (UX section 7.1), the other rule of that screen the clients would
//! otherwise each write.
//!
//! The order is the same in every conversation kind. A row that does not
//! apply is absent, never disabled, except the two the UX names: Camera on a
//! device with no camera (or while a call holds the camera) and Location
//! where the platform has no location service.
//!
//! Not decided here: a composer that is not shown at all (an ended
//! conversation, a closed one-to-one) and the paperclip hidden while the
//! composer edits a message. The client does not call this then.

use crate::conversation::ConversationKind;
use crate::info::ConversationRole;

/// A sent photo is re-encoded as JPEG with its longest side at most this
/// many pixels (UX section 3, ADR-0090 section 9; chosen, not measured).
pub const PHOTO_MAX_LONGEST_SIDE_PX: u32 = 2_560;

/// One row of the menu, in UX order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AttachmentMenuItem {
    /// The system photo picker.
    PhotoOrVideo,
    /// Lakko's capture screen.
    Camera,
    /// The system document picker.
    File,
    /// The system document picker filtered to audio.
    Audio,
    /// The poll editor.
    Poll,
    /// The checklist editor.
    Checklist,
    /// The location screen.
    Location,
    /// The contact picker.
    Contact,
    /// The article editor.
    Article,
}

impl AttachmentMenuItem {
    /// Every item, in menu order.
    pub const ALL: [Self; 9] = [
        Self::PhotoOrVideo,
        Self::Camera,
        Self::File,
        Self::Audio,
        Self::Poll,
        Self::Checklist,
        Self::Location,
        Self::Contact,
        Self::Article,
    ];

    /// Which hairline-separated group the row belongs to: 0 files, 1 things
    /// people answer, 2 things people share, 3 long text. A separator goes
    /// between rows whose sections differ.
    #[must_use]
    pub const fn section(self) -> u8 {
        match self {
            Self::PhotoOrVideo | Self::Camera | Self::File | Self::Audio => 0,
            Self::Poll | Self::Checklist => 1,
            Self::Location | Self::Contact => 2,
            Self::Article => 3,
        }
    }

    /// Stable kebab-case name (the wasm surface's vocabulary). Frozen.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::PhotoOrVideo => "photo-or-video",
            Self::Camera => "camera",
            Self::File => "file",
            Self::Audio => "audio",
            Self::Poll => "poll",
            Self::Checklist => "checklist",
            Self::Location => "location",
            Self::Contact => "contact",
            Self::Article => "article",
        }
    }
}

/// Why a row is shown disabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttachmentMenuDisabled {
    /// `No camera on this device`.
    NoCamera,
    /// `Unavailable during a call` (the call owns the camera).
    CameraInCall,
    /// `Location isn't available on this device`.
    NoLocation,
}

impl AttachmentMenuDisabled {
    /// Stable kebab-case name. Frozen.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::NoCamera => "no-camera",
            Self::CameraInCall => "camera-in-call",
            Self::NoLocation => "no-location",
        }
    }
}

/// One row as the menu shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AttachmentMenuRow {
    /// Which row.
    pub item: AttachmentMenuItem,
    /// Shown disabled with this reason, or enabled when `None`.
    pub disabled: Option<AttachmentMenuDisabled>,
}

/// The menu rows for a composer (UX section 2.4).
///
/// - one-to-one: all but `Poll` (polls are for groups and channels, as in
///   Telegram);
/// - group: all nine, whatever the role (a group of two included);
/// - channel: all nine for the owner and a publisher (`Owner`, `Admin`);
///   none for a subscriber (`Member`), who writes comments, and comments take
///   no attachments. An empty list means no paperclip.
///
/// `has_camera` / `has_location` are the platform's answers;
/// `call_holds_camera` is `true` while a live call owns the camera.
#[must_use]
pub fn attachment_menu(
    kind: ConversationKind,
    role: ConversationRole,
    has_camera: bool,
    has_location: bool,
    call_holds_camera: bool,
) -> Vec<AttachmentMenuRow> {
    if kind == ConversationKind::Channel && role == ConversationRole::Member {
        return Vec::new();
    }
    AttachmentMenuItem::ALL
        .iter()
        .filter(|&&item| !(kind == ConversationKind::Chat && item == AttachmentMenuItem::Poll))
        .map(|&item| {
            let disabled = match item {
                AttachmentMenuItem::Camera if !has_camera => Some(AttachmentMenuDisabled::NoCamera),
                AttachmentMenuItem::Camera if call_holds_camera => {
                    Some(AttachmentMenuDisabled::CameraInCall)
                },
                AttachmentMenuItem::Location if !has_location => {
                    Some(AttachmentMenuDisabled::NoLocation)
                },
                _ => None,
            };
            AttachmentMenuRow { item, disabled }
        })
        .collect()
}

/// What the poll editor shows for anonymity (UX section 7.1, ADR-0090
/// section 3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PollAnonymity {
    /// No switch; the poll is not anonymous (a one-to-one, or a group of
    /// fewer than three accounts, where anonymity means nothing).
    NotOffered,
    /// The switch is shown, on by default (a group of three or more
    /// accounts).
    SwitchDefaultOn,
    /// No switch; the poll is always anonymous (a channel).
    AlwaysAnonymous,
}

/// The fewest accounts a group needs before an anonymous poll is offered.
pub const MIN_ACCOUNTS_FOR_ANONYMOUS_POLL: usize = 3;

/// The anonymity a poll editor offers in a conversation of `kind` with
/// `accounts` distinct accounts (the members' account keys, not leaves).
#[must_use]
pub const fn poll_anonymity(kind: ConversationKind, accounts: usize) -> PollAnonymity {
    match kind {
        ConversationKind::Chat => PollAnonymity::NotOffered,
        ConversationKind::Group if accounts < MIN_ACCOUNTS_FOR_ANONYMOUS_POLL => {
            PollAnonymity::NotOffered
        },
        ConversationKind::Group => PollAnonymity::SwitchDefaultOn,
        ConversationKind::Channel => PollAnonymity::AlwaysAnonymous,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROLES: [ConversationRole; 3] = [
        ConversationRole::Owner,
        ConversationRole::Admin,
        ConversationRole::Member,
    ];

    fn items(rows: &[AttachmentMenuRow]) -> Vec<AttachmentMenuItem> {
        rows.iter().map(|r| r.item).collect()
    }

    #[test]
    fn all_nine_in_order_in_groups_for_any_role() {
        for role in ROLES {
            let rows = attachment_menu(ConversationKind::Group, role, true, true, false);
            assert_eq!(items(&rows), AttachmentMenuItem::ALL.to_vec());
            assert!(rows.iter().all(|r| r.disabled.is_none()));
        }
    }

    #[test]
    fn a_one_to_one_has_every_row_but_poll() {
        let expected: Vec<AttachmentMenuItem> = AttachmentMenuItem::ALL
            .iter()
            .copied()
            .filter(|&i| i != AttachmentMenuItem::Poll)
            .collect();
        for role in ROLES {
            let rows = attachment_menu(ConversationKind::Chat, role, true, true, false);
            assert_eq!(items(&rows), expected);
        }
    }

    #[test]
    fn a_channel_subscriber_has_no_paperclip_and_publishers_have_all_nine() {
        assert!(attachment_menu(
            ConversationKind::Channel,
            ConversationRole::Member,
            true,
            true,
            false
        )
        .is_empty());
        for role in [ConversationRole::Owner, ConversationRole::Admin] {
            let rows = attachment_menu(ConversationKind::Channel, role, true, true, false);
            assert_eq!(items(&rows), AttachmentMenuItem::ALL.to_vec());
        }
    }

    #[test]
    fn camera_and_location_are_disabled_with_a_reason_never_absent() {
        let rows = attachment_menu(
            ConversationKind::Group,
            ConversationRole::Member,
            false,
            false,
            true,
        );
        assert_eq!(rows.len(), 9);
        assert_eq!(rows[1].disabled, Some(AttachmentMenuDisabled::NoCamera));
        assert_eq!(rows[6].disabled, Some(AttachmentMenuDisabled::NoLocation));
        let rows = attachment_menu(
            ConversationKind::Chat,
            ConversationRole::Member,
            true,
            true,
            true,
        );
        assert_eq!(rows[1].disabled, Some(AttachmentMenuDisabled::CameraInCall));
        assert_eq!(rows.iter().filter(|r| r.disabled.is_some()).count(), 1);
    }

    #[test]
    fn sections_are_contiguous_and_ascending() {
        let sections: Vec<u8> = AttachmentMenuItem::ALL
            .iter()
            .map(|i| i.section())
            .collect();
        assert_eq!(sections, vec![0, 0, 0, 0, 1, 1, 2, 2, 3]);
    }

    #[test]
    fn names_are_distinct() {
        let mut names: Vec<&str> = AttachmentMenuItem::ALL.iter().map(|i| i.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 9);
    }

    #[test]
    fn poll_anonymity_follows_the_conversation() {
        assert_eq!(
            poll_anonymity(ConversationKind::Chat, 2),
            PollAnonymity::NotOffered
        );
        assert_eq!(
            poll_anonymity(ConversationKind::Group, 2),
            PollAnonymity::NotOffered
        );
        assert_eq!(
            poll_anonymity(ConversationKind::Group, 3),
            PollAnonymity::SwitchDefaultOn
        );
        assert_eq!(
            poll_anonymity(ConversationKind::Channel, 0),
            PollAnonymity::AlwaysAnonymous
        );
    }
}
