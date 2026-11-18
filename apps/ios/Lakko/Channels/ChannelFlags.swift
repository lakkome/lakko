// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// A channel row's pin, mute and archive (`docs/ux/group-and-channel-privacy.md`
/// section 12: a channel's row in Chats behaves as a group's).
///
/// They are the account's synced flags (ADR-0058), the register a chat or a
/// group uses, so they reach the account's other devices the same way. The
/// register is keyed on bytes: the conversation's MLS group id (32 bytes) for
/// a chat or a group, the channel id (16 bytes) for a channel. The lengths
/// differ, so a channel's flags are never read as a group's. Web and Android
/// key a channel the same way.
enum ChannelFlags {
    /// The key of a channel's flags in ``DeviceGroupSyncRepository/flags``:
    /// the channel id in lower-case hex, the form every key there is in.
    static func key(channelIdHex: String) -> String {
        channelIdHex.lowercased()
    }
}
