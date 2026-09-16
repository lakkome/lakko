// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// MARK: - Alphabetical grouping

/// The pure rules behind the Contacts list (docs/ux/client-redesign.md
/// section 5.3): the alphabetical sections with their index letters, and the
/// search match. Kept out of the view so a test can pin them.
enum ContactListPresentation {

    /// One contact as the grouping sees it.
    struct Item<ID: Hashable>: Equatable {
        let id: ID
        /// The resolved display name (the list label without the username).
        let name: String
        /// The verified username without `@`, when known.
        let username: String?
        /// `true` when `name` is the neutral placeholder: nothing is known
        /// about the person, so they sort under the catch-all section rather
        /// than under "M" for "Member".
        let isPlaceholder: Bool
    }

    struct LetterSection<ID: Hashable>: Equatable {
        let letter: String
        let ids: [ID]
    }

    /// The catch-all index letter for names that do not start with a letter,
    /// and for placeholders.
    static let otherLetter = "#"

    /// The index letter a name sorts under: its first letter, uppercased
    /// (locale-aware), or ``otherLetter``.
    static func indexLetter(for name: String, isPlaceholder: Bool = false) -> String {
        guard !isPlaceholder else { return otherLetter }
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        // A username-only label is `@alice`: it sorts under A, not under `@`.
        let bare = trimmed.hasPrefix("@") ? String(trimmed.dropFirst()) : trimmed
        guard let first = bare.first, first.isLetter else { return otherLetter }
        return String(first).uppercased()
    }

    /// The sections in display order: letters A to Z (locale order), then
    /// ``otherLetter`` last. Within a section, names sort case-insensitively;
    /// a placeholder sorts after every named person.
    static func sections<ID>(_ items: [Item<ID>]) -> [LetterSection<ID>] {
        var buckets: [String: [Item<ID>]] = [:]
        for item in items {
            buckets[indexLetter(for: item.name, isPlaceholder: item.isPlaceholder), default: []].append(item)
        }
        let letters = buckets.keys.sorted { lhs, rhs in
            if lhs == otherLetter { return false }
            if rhs == otherLetter { return true }
            return lhs.localizedStandardCompare(rhs) == .orderedAscending
        }
        return letters.map { letter in
            let sorted = buckets[letter, default: []].sorted { lhs, rhs in
                if lhs.isPlaceholder != rhs.isPlaceholder { return !lhs.isPlaceholder }
                return sortKey(lhs).localizedCaseInsensitiveCompare(sortKey(rhs)) == .orderedAscending
            }
            return LetterSection(letter: letter, ids: sorted.map(\.id))
        }
    }

    private static func sortKey<ID>(_ item: Item<ID>) -> String {
        let trimmed = item.name.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.hasPrefix("@") ? String(trimmed.dropFirst()) : trimmed
    }

    /// Whether a contact matches the search field: a case-insensitive
    /// substring of the name or of the username (with or without `@`). An
    /// empty query matches everything.
    static func matches<ID>(_ item: Item<ID>, query: String) -> Bool {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return true }
        let bareNeedle = needle.hasPrefix("@") ? String(needle.dropFirst()) : needle
        if item.name.localizedCaseInsensitiveContains(bareNeedle) { return true }
        if let username = item.username, username.localizedCaseInsensitiveContains(bareNeedle) {
            return true
        }
        return false
    }
}

// MARK: - Requests

/// Incoming and sent contact requests, newest first. The mailbox message id is
/// monotone per inbox, so it is the order the requests arrived in.
enum RequestSorting {
    static func newestFirst<T>(_ items: [T], messageId: (T) -> Int64) -> [T] {
        items.sorted { messageId($0) > messageId($1) }
    }
}

// MARK: - Safety number

/// The 60-digit safety number laid out for reading: groups of five digits,
/// three groups per row, so two people can compare it aloud line by line.
enum SafetyNumberFormat {
    static let groupSize = 5
    static let groupsPerRow = 3

    /// The five-digit groups of `number`, whatever spacing it arrived with.
    /// A trailing partial group is kept rather than dropped, so a malformed
    /// input is visible instead of silently shortened.
    static func groups(_ number: String) -> [String] {
        let digits = number.filter(\.isNumber)
        var out: [String] = []
        var index = digits.startIndex
        while index < digits.endIndex {
            let end = digits.index(index, offsetBy: groupSize, limitedBy: digits.endIndex) ?? digits.endIndex
            out.append(String(digits[index..<end]))
            index = end
        }
        return out
    }

    /// ``groups(_:)`` arranged ``groupsPerRow`` to a row.
    static func rows(_ number: String) -> [[String]] {
        let all = groups(number)
        return stride(from: 0, to: all.count, by: groupsPerRow).map {
            Array(all[$0..<min($0 + groupsPerRow, all.count)])
        }
    }
}

// MARK: - Invite links

/// One row of the "Invite links" sheet: the server-authoritative list merged
/// with the links generated in this session that the server list does not
/// show yet (the list is fetched, not pushed).
struct InviteLinkRow: Equatable, Identifiable {
    enum Origin: Equatable {
        /// From `listActiveInvites` — revoked through the slug.
        case server
        /// Generated this session and not (yet) in the server list — revoked
        /// through its raw blob id.
        case session(blobId: Data)
    }

    let slug: String
    let url: String
    /// The user-set label, or empty.
    let label: String
    let createdAt: Date?
    /// UNIX millis; 0 = never expires.
    let expiryMs: UInt64
    /// "1 use left", "3 of 10 left", or `nil` for an unlimited link.
    let usesLeft: String?
    let origin: Origin

    var id: String { slug }

    var title: String { label.isEmpty ? "Invite link" : label }
}

enum InviteLinkRows {

    /// The server list first (it is the truth for uses and revocation), then
    /// session links the server list does not carry, newest first. Expired
    /// links are dropped: the sheet manages links that still work.
    static func merge(
        active: [ActiveInvite],
        sessionGenerated: [GeneratedInvite],
        now: Date
    ) -> [InviteLinkRow] {
        let nowMs = UInt64(max(0, now.timeIntervalSince1970 * 1000))
        var rows: [InviteLinkRow] = active.compactMap { invite in
            guard !isExpired(expiryMs: invite.expiryMs, nowMs: nowMs) else { return nil }
            return InviteLinkRow(
                slug: invite.blobIdSlug,
                url: url(slug: invite.blobIdSlug),
                label: invite.label,
                createdAt: parseCreatedAt(invite.createdAt),
                expiryMs: invite.expiryMs,
                usesLeft: usesLeftCopy(maxUses: invite.maxUses, usedCount: invite.usedCount),
                origin: .server)
        }
        let known = Set(rows.map(\.slug))
        for invite in sessionGenerated where !known.contains(invite.blobIdSlug) {
            let expiry = UInt64(max(0, invite.expiryMs))
            guard !isExpired(expiryMs: expiry, nowMs: nowMs) else { continue }
            rows.append(InviteLinkRow(
                slug: invite.blobIdSlug,
                url: invite.url,
                label: invite.label,
                createdAt: Date(timeIntervalSince1970: TimeInterval(invite.createdAtMs) / 1000),
                expiryMs: expiry,
                usesLeft: usesLeftCopy(
                    maxUses: UInt32(clamping: max(0, invite.maxUses)), usedCount: 0),
                origin: .session(blobId: invite.blobId)))
        }
        return rows.sorted { lhs, rhs in
            (lhs.createdAt ?? .distantPast) > (rhs.createdAt ?? .distantPast)
        }
    }

    static func isExpired(expiryMs: UInt64, nowMs: UInt64) -> Bool {
        expiryMs != 0 && expiryMs <= nowMs
    }

    /// `https://lakko.me/i/<slug>`.
    static func url(slug: String) -> String {
        ContactsRepository.inviteUrlPrefix + slug
    }

    /// Sentinels as `lakko-contacts` defines them: 0 = single use,
    /// `UInt32.max` = unlimited, else N uses. `nil` for unlimited.
    static func usesLeftCopy(maxUses: UInt32, usedCount: UInt32) -> String? {
        if maxUses == UInt32.max { return nil }
        let total = maxUses == 0 ? 1 : Int(maxUses)
        let left = max(0, total - Int(usedCount))
        if total == 1 {
            return left == 1 ? "1 use left" : "Used"
        }
        return "\(left) of \(total) left"
    }

    /// "Expires in 6 days" / "Expires tomorrow" through the relative
    /// formatter; "Never expires" for 0; "Expired" once past.
    static func expiryCopy(expiryMs: UInt64, now: Date) -> String {
        if expiryMs == 0 { return "Never expires" }
        let expiry = Date(timeIntervalSince1970: TimeInterval(expiryMs) / 1000)
        if expiry <= now { return "Expired" }
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .full
        return "Expires \(formatter.localizedString(for: expiry, relativeTo: now))"
    }

    /// The server's `created_at` string: ISO 8601 with or without fractional
    /// seconds, or a MariaDB-style `YYYY-MM-DD HH:MM:SS` (UTC). `nil` for
    /// anything else, so the row simply shows no date.
    static func parseCreatedAt(_ text: String) -> Date? {
        let iso = ISO8601DateFormatter()
        iso.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = iso.date(from: text) { return date }
        iso.formatOptions = [.withInternetDateTime]
        if let date = iso.date(from: text) { return date }
        let sql = DateFormatter()
        sql.locale = Locale(identifier: "en_US_POSIX")
        sql.timeZone = TimeZone(secondsFromGMT: 0)
        sql.dateFormat = "yyyy-MM-dd HH:mm:ss"
        return sql.date(from: text)
    }
}

// MARK: - Devices

/// The row title of a device (docs/ux/client-redesign.md section 5.5): no
/// platform, model or user-set name reaches the client yet, so a device is
/// "This device" or "Device N" — never its key.
enum DeviceRowPresentation {
    static func title(isSelf: Bool, position: Int) -> String {
        isSelf ? "This device" : "Device \(position)"
    }
}
