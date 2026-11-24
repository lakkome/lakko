// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import UIKit
import UserNotifications

/// `docs/ux/group-and-channel-privacy.md` section 8.1 — telling the owner and
/// admins a request to join is waiting: the bar in the conversation, the
/// unread chat-list row, and one notification per request.
///
/// The decisions are pure and live here (``JoinRequestSignals``); the
/// repository feeds them what it read (the retained requests, the role, the
/// two persisted sets) and does what they say. Mirror of the same rule on web
/// and Android.
enum JoinRequestSignals {

    /// When the `Join requests` heading reads `64+`, the bar reads
    /// `64+ requests to join` (the same threshold the members panel uses).
    static let fullThreshold = 64

    /// The role gate: the owner and admins of a group whose membership has
    /// not ended (the gate that shows `Approve` and `Reject`).
    static func signalsApply(role: Role?, membershipEnded: Bool) -> Bool {
        guard !membershipEnded else { return false }
        return role == .owner || role == .admin
    }

    /// What the bar says.
    enum Banner: Equatable {
        /// `{n} request to join` / `{n} requests to join`.
        case count(Int)
        /// `64+ requests to join`.
        case full

        /// The catalog key and the count it is formatted with.
        var key: String {
            switch self {
            case .count: return "chat.join_requests.banner"
            case .full: return "chat.join_requests.banner_full"
            }
        }

        /// The bar's text in the current language.
        var text: String {
            switch self {
            case .count(let n):
                return Localization.text(key, counts: ["count": UInt64(max(0, n))])
            case .full:
                return Localization.text(key)
            }
        }
    }

    /// The bar, or `nil` when there is none: not an owner or admin, the
    /// membership ended, or no retained request is waiting.
    static func banner(role: Role?, membershipEnded: Bool, pendingCount: Int) -> Banner? {
        guard signalsApply(role: role, membershipEnded: membershipEnded), pendingCount > 0 else {
            return nil
        }
        return pendingCount >= fullThreshold ? .full : .count(pendingCount)
    }

    /// The two sets a device keeps per account and group, keyed by the
    /// joiner's signature key (base64).
    struct Sets: Codable, Equatable {
        /// Requests already announced (by notification or by the bar).
        var announced: Set<String> = []
        /// Requests not yet shown on screen.
        var unshown: Set<String> = []

        var isEmpty: Bool { announced.isEmpty && unshown.isEmpty }
    }

    /// What one new request leads to.
    enum Arrival: Equatable {
        /// Already announced on this device: nothing.
        case alreadyAnnounced
        /// The conversation is open in front of the person: the bar shows it.
        case shownByBar
        /// Not shown yet; the group is muted, so no notification.
        case unshownQuiet
        /// Not shown yet; post the notification.
        case notify
    }

    /// A new request event for `joinerKeyB64`. Returns the sets to store and
    /// what to do. A request asked again (already pending) is the same request
    /// and is not announced again.
    static func arrival(
        joinerKeyB64: String, sets: Sets, openAndVisible: Bool, muted: Bool
    ) -> (sets: Sets, arrival: Arrival) {
        guard !sets.announced.contains(joinerKeyB64) else { return (sets, .alreadyAnnounced) }
        var next = sets
        next.announced.insert(joinerKeyB64)
        if openAndVisible { return (next, .shownByBar) }
        next.unshown.insert(joinerKeyB64)
        return (next, muted ? .unshownQuiet : .notify)
    }

    /// Both sets cut to the requests still waiting.
    static func pruned(_ sets: Sets, pending: Set<String>) -> Sets {
        Sets(announced: sets.announced.intersection(pending), unshown: sets.unshown.intersection(pending))
    }

    /// The bar was drawn in the open conversation: every request counts as
    /// shown.
    static func barDrawn(_ sets: Sets) -> Sets {
        Sets(announced: sets.announced, unshown: [])
    }

    /// The requests that add to the chat-list row: the unshown ones still
    /// waiting, and only while the signals apply.
    static func unshownCount(sets: Sets?, pending: Set<String>, signalsApply: Bool) -> Int {
        guard signalsApply, let sets else { return 0 }
        return sets.unshown.intersection(pending).count
    }

    /// The row's unread count: the unread messages plus one per unshown
    /// request.
    static func rowUnread(messages: Int, unshown: Int) -> Int {
        max(0, messages) + max(0, unshown)
    }

    /// The notification body: the joiner's name when known, else the
    /// unnamed form.
    static func notificationBody(name: String?, groupTitle: String) -> String {
        let trimmed = name?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if trimmed.isEmpty {
            return Localization.text("notification.join_request.body_unnamed", texts: ["group": groupTitle])
        }
        return Localization.text(
            "notification.join_request.body", texts: ["name": trimmed, "group": groupTitle])
    }

    /// The name of a request: the account's name when it resolves to more
    /// than a placeholder (a contact, a profile), else the name the request
    /// carried, else the leaf's name when it resolves, else `nil`.
    static func requestName(
        accountKeyName: ResolvedDisplayName?, carried: String?, leafName: ResolvedDisplayName?
    ) -> String? {
        if let accountKeyName, accountKeyName.source != .placeholder { return accountKeyName.name }
        if let carried, !carried.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return carried }
        if let leafName, leafName.source != .placeholder { return leafName.name }
        return nil
    }

    /// One notification per group and joiner.
    static func notificationIdentifier(conversationIdHex: String, joinerKeyB64: String) -> String {
        "lakko.join-request.\(conversationIdHex.lowercased()).\(joinerKeyB64)"
    }
}

/// The two sets of ``JoinRequestSignals``, per account, in `UserDefaults`:
/// one row per account holding every group's sets. Local, never synced; it
/// holds group ids and joiner keys, nothing secret. Not written once the
/// account's session has ended (``AccountStoreGate``); removed with the
/// account by ``AccountLocalDataWipe`` (step `join-request-signals`).
struct JoinRequestSignalStore {
    var defaults: UserDefaults = .standard

    static let keyPrefix = "me.lakko.chat.join-request-signals.v1."

    static func key(accountIdB64: String) -> String { keyPrefix + accountIdB64 }

    /// Every group's sets; empty when there is none or it is unreadable.
    func load(accountIdB64: String) -> [String: JoinRequestSignals.Sets] {
        guard let data = defaults.data(forKey: Self.key(accountIdB64: accountIdB64)),
              let raw = try? JSONDecoder().decode([String: JoinRequestSignals.Sets].self, from: data)
        else { return [:] }
        return raw
    }

    func sets(accountIdB64: String, groupIdHex: String) -> JoinRequestSignals.Sets {
        load(accountIdB64: accountIdB64)[groupIdHex.lowercased()] ?? JoinRequestSignals.Sets()
    }

    /// Store one group's sets; an empty pair removes the group's entry.
    func save(accountIdB64: String, groupIdHex: String, sets: JoinRequestSignals.Sets) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var all = load(accountIdB64: accountIdB64)
        let id = groupIdHex.lowercased()
        guard all[id] != sets else { return }
        if sets.isEmpty { all[id] = nil } else { all[id] = sets }
        if all.isEmpty {
            defaults.removeObject(forKey: Self.key(accountIdB64: accountIdB64))
            return
        }
        guard let data = try? JSONEncoder().encode(all) else { return }
        defaults.set(data, forKey: Self.key(accountIdB64: accountIdB64))
    }

    func forgetGroup(accountIdB64: String, groupIdHex: String) {
        save(accountIdB64: accountIdB64, groupIdHex: groupIdHex, sets: JoinRequestSignals.Sets())
    }

    func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.key(accountIdB64: accountIdB64))
    }
}

/// Which group conversations (or their Group info) are on screen. The chat
/// screen and the info screen register as they appear and disappear.
@MainActor
enum VisibleGroupConversation {
    private static var shown: [String: Int] = [:]

    static func appeared(_ groupIdHex: String) {
        shown[groupIdHex.lowercased(), default: 0] += 1
    }

    static func disappeared(_ groupIdHex: String) {
        let id = groupIdHex.lowercased()
        guard let count = shown[id] else { return }
        shown[id] = count > 1 ? count - 1 : nil
    }

    /// The group is open in front of the person: the app active and the
    /// conversation (or its info) showing.
    static func isOpenAndVisible(_ groupIdHex: String) -> Bool {
        UIApplication.shared.applicationState == .active && shown[groupIdHex.lowercased()] != nil
    }
}

/// Shows a request-to-join notification. Injectable for tests.
protocol JoinRequestNotificationPoster {
    @MainActor func post(conversationIdHex: String, joinerKeyB64: String, title: String, body: String)
}

/// The system notification through `UNUserNotificationCenter`, under the
/// app's existing notification permission (never asked for here); with
/// notifications off the system drops it and the bar and the row remain.
struct SystemJoinRequestNotifier: JoinRequestNotificationPoster {
    func post(conversationIdHex: String, joinerKeyB64: String, title: String, body: String) {
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        content.sound = .default
        content.threadIdentifier = "lakko.join-request.\(conversationIdHex.lowercased())"
        content.userInfo = [JoinRequestNotificationTaps.routeKey: conversationIdHex.lowercased()]
        let request = UNNotificationRequest(
            identifier: JoinRequestSignals.notificationIdentifier(
                conversationIdHex: conversationIdHex, joinerKeyB64: joinerKeyB64),
            content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request) { error in
            if error != nil { NSLog("[chat] join request notification not shown") }
        }
    }
}

/// A tap on a request-to-join notification: the group whose info opens at
/// `Join requests`. The session root takes it, also when the tap launched
/// the app.
@MainActor
final class JoinRequestNotificationTaps: ObservableObject {
    static let shared = JoinRequestNotificationTaps()

    /// The notification's `userInfo` key carrying the conversation id (hex).
    nonisolated static let routeKey = "lakko.join-request.group"

    @Published var openGroupInfoRequested: String?
}
