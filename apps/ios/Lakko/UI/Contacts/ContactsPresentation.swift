// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// MARK: - Alphabetical grouping

/// The pure rules behind the Contacts list (docs/ux/client-redesign.md
/// section 5.3): the alphabetical sections with their index letters, and the
/// search match. Kept out of the view so a test can pin them.
enum ContactListPresentation {

    /// What a row under Sent says: the name the person was found under with
    /// `@username` beneath it; a request sent by username alone has that for
    /// its name and nothing beneath. `nil` for a row that knows neither.
    static func sentRow(_ request: SentContactRequest) -> (name: String?, username: String?) {
        let username = request.recipientUsername.isEmpty ? nil : "@\(request.recipientUsername)"
        guard let name = request.recipientName, !name.isEmpty else { return (username, nil) }
        return (name, name == username ? nil : username)
    }

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
    /// The whole link with its secret, or `nil` when the secret is not on this
    /// device (ADR-0028 amendment 2026-09-28).
    let url: String?
    /// The user-set label, or empty.
    let label: String
    let createdAt: Date?
    /// UNIX millis; 0 = never expires.
    let expiryMs: UInt64
    /// The limit the link was minted with ("single-use", "up to 5 uses",
    /// "unlimited"), in the interface language of the moment the rows were
    /// built.
    let limit: String
    /// How many uses a multi-use link has left, when the use count is known
    /// (the ADR-0079 synced register carries it). `nil` for a single-use or
    /// unlimited link, and when the count is not known: the row then shows
    /// ``limit`` instead.
    var usesLeft: UInt32? = nil
    let origin: Origin

    var id: String { slug }

    /// The label the person gave the link, or `nil`: a link without a label
    /// has no title line, its facts lead the row (no placeholder name).
    var title: String? { label.isEmpty ? nil : label }
}

enum InviteLinkRows {

    /// The links this device keeps a record of (ADR-0078 section 10: there is
    /// no server list), then session links that list does not carry, newest
    /// first. Expired links are dropped: the sheet manages links that still
    /// work.
    static func merge(
        active: [ActiveInvite],
        sessionGenerated: [GeneratedInvite],
        now: Date,
        accountIdB64: String?,
        secrets: InviteLinkSecretStore = .shared
    ) -> [InviteLinkRow] {
        let nowMs = UInt64(max(0, now.timeIntervalSince1970 * 1000))
        var rows: [InviteLinkRow] = active.compactMap { invite in
            guard !isExpired(expiryMs: invite.expiryMs, nowMs: nowMs) else { return nil }
            return InviteLinkRow(
                slug: invite.blobIdSlug,
                url: url(slug: invite.blobIdSlug, accountIdB64: accountIdB64, secrets: secrets),
                label: invite.label,
                createdAt: parseCreatedAt(invite.createdAt),
                expiryMs: invite.expiryMs,
                limit: limitCopy(maxUses: invite.maxUses),
                usesLeft: usesLeft(maxUses: invite.maxUses, used: invite.usedCount),
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
                limit: limitCopy(maxUses: UInt32(clamping: max(0, invite.maxUses))),
                origin: .session(blobId: invite.blobId)))
        }
        return rows.sorted { lhs, rhs in
            (lhs.createdAt ?? .distantPast) > (rhs.createdAt ?? .distantPast)
        }
    }

    static func isExpired(expiryMs: UInt64, nowMs: UInt64) -> Bool {
        expiryMs != 0 && expiryMs <= nowMs
    }

    /// `docs/ux/client-redesign.md` section 5.3 — what a row whose secret
    /// this device does not hold shows in place of Copy and Share (Revoke
    /// stays).
    static var createdElsewhereCopy: String { Localization.text("invite.links.made_elsewhere_hint") }

    /// `https://lakko.me/i/<slug>#<secret>`, or `nil` when this device does
    /// not hold the link's secret for the account.
    static func url(
        slug: String, accountIdB64: String?, secrets: InviteLinkSecretStore = .shared
    ) -> String? {
        guard let accountIdB64,
              let secret = secrets.secret(accountIdB64: accountIdB64, slug: slug) else { return nil }
        return InviteLinkSecret.link(base: ContactsRepository.inviteUrlPrefix + slug, secret: secret)
    }

    /// Uses left of a multi-use link, or `nil` when the link is single-use or
    /// unlimited, or the use count is not known.
    static func usesLeft(maxUses: UInt32, used: UInt32?) -> UInt32? {
        guard let used, maxUses > 1, maxUses != UInt32.max else { return nil }
        return maxUses - min(used, maxUses)
    }

    /// The use fact under a link: "N uses left" when known, else the limit the
    /// link was minted with.
    static func useFact(_ row: InviteLinkRow) -> String {
        if let left = row.usesLeft {
            return Localization.text("invite.links.uses_left", counts: ["count": UInt64(left)])
        }
        return row.limit
    }

    /// Sentinels as `lakko-contacts` defines them: 0 (or 1) = single use,
    /// `UInt32.max` = unlimited, else up to N uses (the web
    /// `describeInviteLimit`).
    static func limitCopy(maxUses: UInt32) -> String {
        if maxUses == UInt32.max { return Localization.text("invite.uses.unlimited") }
        if maxUses <= 1 { return Localization.text("invite.uses.single") }
        return Localization.text("invite.uses.up_to", counts: ["count": UInt64(maxUses)])
    }

    /// "Expires in 3d" / "Expires in 2h" / "Expires in 5m", "Never expires"
    /// for 0, "Expired" once past (the compact facts of the web list).
    static func expiryCopy(expiryMs: UInt64, now: Date) -> String {
        if expiryMs == 0 { return Localization.text("invite.links.never_expires") }
        let expiry = Date(timeIntervalSince1970: TimeInterval(expiryMs) / 1000)
        if expiry <= now { return Localization.text("invite.links.expired") }
        switch remaining(expiry.timeIntervalSince(now)) {
        case .days(let n):
            return Localization.text("invite.links.expires_days", counts: ["count": UInt64(n)])
        case .hours(let n):
            return Localization.text("invite.links.expires_hours", counts: ["count": UInt64(n)])
        case .minutes(let n):
            return Localization.text("invite.links.expires_minutes", counts: ["count": UInt64(n)])
        }
    }

    /// The time left until a link expires, in the one unit a row shows.
    enum Remaining: Equatable {
        case minutes(Int)
        case hours(Int)
        case days(Int)
    }

    /// The rule all three clients share: minutes rounded up while under an
    /// hour; past that, hours rounded half up while under a day; then days
    /// rounded half up. A 7-day link made an hour ago reads 7d, not 6d.
    /// `delta` is in seconds and positive.
    static func remaining(_ delta: TimeInterval) -> Remaining {
        let minutes = max(1, Int((delta / 60).rounded(.up)))
        if minutes < 60 { return .minutes(minutes) }
        let hours = Int((delta / 3_600 + 0.5).rounded(.down))
        if hours < 24 { return .hours(hours) }
        return .days(Int((delta / 86_400 + 0.5).rounded(.down)))
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
/// "This device" or "Device N" — never its key. Read from the catalog in the
/// interface language of the call (ADR-0061); `N` stays in Latin digits.
enum DeviceRowPresentation {
    static func title(isSelf: Bool, position: Int) -> String {
        isSelf
            ? Localization.text("settings.devices.this_device")
            : Localization.text("settings.devices.device_n", texts: ["number": String(position)])
    }

    /// The trailing control of a device row (UX 18.1).
    enum Control: Equatable {
        /// This device: no control (Forget this device is in Settings).
        case none
        /// Signed out at the server, the rest of the removal still
        /// finishing: a "Removing..." label and no Remove control.
        case pending
        /// Signed out at the server, and the owed step waits for the
        /// person's passkey: the label AND `Finish removing`.
        case pendingFinish
        case remove
    }

    static func control(isSelf: Bool, removalPending: Bool, waitingOnPerson: Bool = false) -> Control {
        if isSelf { return .none }
        guard removalPending else { return .remove }
        return waitingOnPerson ? .pendingFinish : .pending
    }
}
