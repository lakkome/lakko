// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0067 section 9 / UX 19.2, 19.5, 19.6 — what the account's own devices
// are told: the password was changed by email, the account was opened by
// email, a recovery email was added / replaced / removed, a recovery phrase
// was saved. The server keeps the list (`GET /account/notices`), since the
// device that acted may hold no keys; each device keeps a seen cursor and
// shows a notice once.
//
// Pure: the kinds, what each one says, which of a fetched page are shown,
// and how a device recognises its own action.

public enum AccountNoticeKind: String, CaseIterable, Equatable, Sendable {
    case passwordChangedByEmail = "password-changed-by-email"
    case accountOpenedByEmail = "account-opened-by-email"
    case emailAdded = "recovery-email-added"
    case emailReplaced = "recovery-email-replaced"
    case emailRemoved = "recovery-email-removed"
    case phraseSaved = "recovery-phrase-saved"
}

/// One notice as the server lists it. No address, no device detail.
public struct AccountNotice: Equatable, Sendable, Identifiable {
    public let id: UInt64
    /// The wire kind; ``known`` is `nil` for one this build does not know.
    public let kind: String
    public let atMs: Int64

    public init(id: UInt64, kind: String, atMs: Int64) {
        self.id = id
        self.kind = kind
        self.atMs = atMs
    }

    public var known: AccountNoticeKind? { AccountNoticeKind(rawValue: kind) }
}

public struct AccountNoticePage: Equatable, Sendable {
    public let notices: [AccountNotice]
    public let nextAfter: UInt64
    public let more: Bool

    public init(notices: [AccountNotice], nextAfter: UInt64, more: Bool) {
        self.notices = notices
        self.nextAfter = nextAfter
        self.more = more
    }
}

/// What one notice says (UX 19.2, 19.5, 19.6).
public struct AccountNoticeCopy: Equatable {
    public let titleKey: String
    /// A second line taking `{date}`, or `nil` for the one-line kinds.
    public let bodyKey: String?

    public static func of(_ kind: AccountNoticeKind) -> AccountNoticeCopy {
        switch kind {
        case .passwordChangedByEmail:
            return AccountNoticeCopy(
                titleKey: "notice.password_by_email.title", bodyKey: "notice.password_by_email.body")
        case .accountOpenedByEmail:
            return AccountNoticeCopy(
                titleKey: "notice.opened_by_email.title", bodyKey: "notice.opened_by_email.body")
        case .emailAdded: return AccountNoticeCopy(titleKey: "notice.email_added", bodyKey: nil)
        case .emailReplaced: return AccountNoticeCopy(titleKey: "notice.email_replaced", bodyKey: nil)
        case .emailRemoved: return AccountNoticeCopy(titleKey: "notice.email_removed", bodyKey: nil)
        case .phraseSaved: return AccountNoticeCopy(titleKey: "notice.phrase_saved", bodyKey: nil)
        }
    }

    public static let wasMeKey = "notice.was_me"
    public static let wasNotMeKey = "notice.was_not_me"
}

/// What a device asks `GET /account/notices` for.
public enum NoticeQuery: Hashable, Sendable {
    /// `after=<id>`: everything after the stored cursor.
    case after(UInt64)
    /// `since_ms=<unix ms>`: a device that never kept a cursor.
    case sinceMs(Int64)

    /// How far back a device without a cursor asks.
    public static let lookbackMs: Int64 = 30 * 24 * 60 * 60 * 1000

    /// The first request of a landing: after the cursor when there is one,
    /// else the last 30 days.
    public static func first(cursor: UInt64?, nowMs: Int64) -> NoticeQuery {
        cursor.map(NoticeQuery.after) ?? .sinceMs(nowMs - lookbackMs)
    }

    /// The query string.
    public var queryItem: String {
        switch self {
        case .after(let id): return "after=\(id)"
        case .sinceMs(let ms): return "since_ms=\(ms)"
        }
    }
}

/// Which notices of a fetch a device shows, pure.
public enum NoticeSelection {

    /// How far a notice's server time may lie from the moment this device
    /// performed the same kind of action and still be read as that action:
    /// ten minutes.
    public static let ownActionWindowMs: Int64 = 10 * 60 * 1000

    public struct Outcome: Equatable {
        /// The notices to show, oldest first, one per kind (the newest).
        public let shown: [AccountNotice]
        /// The cursor to store: nothing at or below it is fetched again.
        public let cursor: UInt64
        /// The "I did this" markers that suppressed a notice: each one is
        /// spent on exactly one notice and is cleared afterwards.
        public let spentMarkers: [String]
    }

    /// Whether `notice` is this device's own action.
    public static func isOwn(_ notice: AccountNotice, ownActions: [String: Int64]) -> Bool {
        guard let didAtMs = ownActions[notice.kind] else { return false }
        return abs(notice.atMs - didAtMs) <= ownActionWindowMs
    }

    /// - Parameters:
    ///   - fetched: every notice the server listed for the query.
    ///   - cursor: the stored seen cursor, or `nil` on a device without one.
    ///   - ownActions: notice kind -> when this device did that (unix ms).
    public static func select(
        fetched: [AccountNotice], cursor: UInt64?, ownActions: [String: Int64]
    ) -> Outcome {
        let floor = cursor ?? 0
        let fresh = fetched.filter { cursor == nil || $0.id > floor }.sorted { $0.id < $1.id }
        var markers = ownActions
        var spent: [String] = []
        var newestOfKind: [AccountNoticeKind: AccountNotice] = [:]
        for notice in fresh {
            // A kind this build does not know is skipped (and counted as seen).
            guard let kind = notice.known else { continue }
            // The device's own action: one marker, one notice.
            if isOwn(notice, ownActions: markers) {
                markers[notice.kind] = nil
                spent.append(notice.kind)
                continue
            }
            newestOfKind[kind] = notice
        }
        return Outcome(
            shown: newestOfKind.values.sorted { $0.id < $1.id },
            cursor: max(floor, fresh.last?.id ?? floor),
            spentMarkers: spent)
    }
}

/// What the `It was not me` screen offers, in order (UX 19.6), pure.
public enum SecureAccountActions: Equatable {
    case newPassword
    case removeEmail
    case savePhrase

    public static func offered(_ status: RecoveryEmailStatus?) -> [SecureAccountActions] {
        var actions: [SecureAccountActions] = [.newPassword]
        if status?.enabled != false { actions.append(.removeEmail) }
        if status?.phraseSaved != true { actions.append(.savePhrase) }
        return actions
    }
}
