// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Where each received row meets a block (UX `reporting-and-moderation.md`
/// section 1.2). The verdict itself is `blockVerdict(path:blocked:)` in
/// `crates/lakko-ui-rules`; this file only names the path a row is on and
/// applies what the rule answers.
enum BlockRouting {

    /// The path an event from the other person of a one-to-one is on.
    static func oneToOnePath(of event: IncomingChatEvent) -> BlockPath {
        switch event {
        case .callSignaling:
            return .incomingCall
        case .op(_, let op, _, _, _, _, _):
            switch op {
            case .typing, .readReceipt: return .incomingSignal
            case .edit, .delete, .reaction: return .oneToOneMessage
            }
        case .profile, .profileRequest:
            return .incomingSignal
        default:
            return .oneToOneMessage
        }
    }

    /// Whether a verdict means the row is not kept at all.
    static func drops(_ verdict: BlockVerdict) -> Bool {
        verdict == .drop || verdict == .declineSilently
    }

    /// The events of a one-to-one batch that survive a block of their
    /// sender, with their senders kept aligned. An event with no sender (an
    /// MLS handshake row) is always kept: it was already applied by the core,
    /// and skipping it here would only hide it. `senders` must be aligned
    /// with `events`; when it is not (a producer that resolved none),
    /// nothing is dropped.
    static func keptInOneToOne(
        _ events: [IncomingChatEvent],
        senders: [MemberIdentity?],
        isBlocked: (MemberIdentity) -> Bool
    ) -> (events: [IncomingChatEvent], senders: [MemberIdentity?]) {
        guard senders.count == events.count else { return (events, senders) }
        var keptEvents: [IncomingChatEvent] = []
        var keptSenders: [MemberIdentity?] = []
        for (event, sender) in zip(events, senders) {
            if let sender,
               drops(blockVerdict(path: oneToOnePath(of: event), blocked: isBlocked(sender))) {
                continue
            }
            keptEvents.append(event)
            keptSenders.append(sender)
        }
        return (keptEvents, keptSenders)
    }
}
