// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The chat list's unread badge, from what this device's receive paths apply.
///
/// Before the foreground receive, a chat that was not open was not polled at
/// all, so nothing could be unread in it and the only badge source was the
/// cross-device read cursor (`DeviceGroupSyncRepository.readCursors`), which is
/// absent until some device of this account opens the chat. Now every group is
/// received while the app is foregrounded, so the badge has something to count.
///
/// The rule is deliberately simple:
///
///   * only an incoming chat message counts — edits, reactions, typing, read
///     receipts, commits, call signaling and service envelopes do not;
///   * nothing counts while the chat is open, because it is being read;
///   * opening the chat clears the count.
///
/// In memory only: a relaunch starts at zero.
enum UnreadCounting {

    /// The number of user-visible incoming messages in a batch.
    static func incomingMessages(in events: [IncomingChatEvent]) -> Int {
        events.reduce(0) { acc, event in
            if case .application = event { return acc + 1 }
            return acc
        }
    }

    /// The count after a receive path applied `applied`.
    static func next(current: Int, applied: [IncomingChatEvent], chatOpen: Bool) -> Int {
        guard !chatOpen else { return 0 }
        return current + incomingMessages(in: applied)
    }

    /// What the chat list shows. A read cursor broadcast by any of this
    /// account's devices is the better source when one exists (it knows about
    /// reads on the other devices); without one, the local count.
    static func displayed(local: Int, cursorDerived: Int?) -> Int {
        cursorDerived ?? local
    }
}
