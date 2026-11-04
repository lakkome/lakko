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
///   * opening the chat clears the count;
///   * a message that came with the pairing history never counts
///     (`docs/ux/second-device.md` section 2, ``PairingHistory``).
///
/// The count holds the message ids, not a number: a row a pairing bundle turns
/// out to carry leaves it by id.
///
/// In memory only: a relaunch starts at zero.
enum UnreadCounting {

    /// The ids of the user-visible incoming messages in a batch.
    static func incomingMessageIds(in events: [IncomingChatEvent]) -> [Int64] {
        events.compactMap { event in
            switch event {
            case .application(let messageId, _, _, _, _, _, _): return messageId
            case .unsupported(let messageId, _, _, _): return messageId
            default: return nil
            }
        }
    }

    /// The number of user-visible incoming messages in a batch.
    static func incomingMessages(in events: [IncomingChatEvent]) -> Int {
        incomingMessageIds(in: events).count
    }

    /// The unread ids after a receive path applied `applied`. `syncedThrough`
    /// is the group's pairing-history floor.
    static func next(
        current: Set<Int64>, applied: [IncomingChatEvent], chatOpen: Bool, syncedThrough: Int64? = nil
    ) -> Set<Int64> {
        guard !chatOpen else { return [] }
        return current.union(
            incomingMessageIds(in: applied).filter {
                !PairingHistory.isHistory(messageId: $0, floor: syncedThrough)
            })
    }

    /// What the chat list shows. A read cursor broadcast by any of this
    /// account's devices is the better source when one exists (it knows about
    /// reads on the other devices); without one, the local count.
    static func displayed(local: Int, cursorDerived: Int?) -> Int {
        cursorDerived ?? local
    }

    /// A read cursor's message id on the wire: 8 big-endian bytes, the shape
    /// every platform writes.
    static func cursorBytes(_ messageId: Int64) -> Data {
        var out = Data(count: 8)
        var v = UInt64(bitPattern: messageId)
        for i in (0..<8).reversed() {
            out[i] = UInt8(v & 0xff)
            v >>= 8
        }
        return out
    }
}
