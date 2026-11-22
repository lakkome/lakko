// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import UserNotifications

/// `docs/ux/message-actions.md` section 9 — a notification already on screen
/// for a message that was then deleted is withdrawn.
///
/// A delivered message notification keeps the wake's payload: `p` is the DS
/// id of the row the wake was sent for (the message itself), and the
/// extension sets the thread identifier to the conversation it previewed.
/// A notification is withdrawn only when both match, so one wake's
/// notification about a different message of the same conversation stays.
/// When the conversation moved to a successor group the thread is the
/// conversation id rather than this group's, nothing matches and nothing is
/// withdrawn.
public enum DeletedMessageNotifications {

    /// The identifiers of the delivered notifications for `messageId` in the
    /// conversation threaded as `thread`.
    public static func identifiers(
        _ delivered: [(identifier: String, thread: String, userInfo: [AnyHashable: Any])],
        thread: String,
        messageId: Int64
    ) -> [String] {
        delivered.filter { item in
            guard item.thread.caseInsensitiveCompare(thread) == .orderedSame else { return false }
            return pointer(item.userInfo) == messageId
        }.map(\.identifier)
    }

    /// The wake's `p`, as a number or a decimal string.
    static func pointer(_ userInfo: [AnyHashable: Any]) -> Int64? {
        if let number = userInfo["p"] as? NSNumber { return number.int64Value }
        if let string = userInfo["p"] as? String { return Int64(string) }
        return nil
    }

    /// Withdraw them. Fire-and-forget; nothing is reported.
    public static func withdraw(groupId: Data, messageId: Int64) {
        let thread = groupId.map { String(format: "%02x", $0) }.joined()
        Task {
            let center = UNUserNotificationCenter.current()
            let delivered = await center.deliveredNotifications().map {
                (identifier: $0.request.identifier,
                 thread: $0.request.content.threadIdentifier,
                 userInfo: $0.request.content.userInfo)
            }
            let ids = identifiers(delivered, thread: thread, messageId: messageId)
            guard !ids.isEmpty else { return }
            center.removeDeliveredNotifications(withIdentifiers: ids)
        }
    }
}
