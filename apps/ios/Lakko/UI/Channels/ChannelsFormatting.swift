// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import SwiftUI

/// The pure text rules behind the Channels screens (UX spec section 5.4):
/// what a channel row shows, and how a post's time reads. Everything takes
/// its clock, calendar and locale as parameters so the tests pin the strings
/// without depending on the device.
public enum ChannelsFormatting {

    // MARK: Row

    /// What one channel row renders. Built once per body pass from the
    /// shared record, the local handle, the cached posts and the cached
    /// subscriber count. The same shape as the web `channelRowModel`.
    public struct RowModel: Equatable {
        /// ADR-0051 §5 — the shared name, else the local name, else the bare
        /// handle, else "Channel".
        public let title: String
        /// "@handle", or `nil` when this device knows no handle.
        public let handleLabel: String?
        /// "12 subscribers", "1 subscriber", or `nil` when the count is
        /// unknown (a reader never learns it).
        public let subscribersLabel: String?
        /// The newest post's time in the list vocabulary
        /// (``ChatListFormatting/lastMessageTime``), `nil` with no posts.
        public let timeLabel: String?
        /// The second line: `@handle · N subscribers` (the handle omitted when
        /// the title already IS the handle, so it never shows twice), else
        /// whichever of the two is known, else "Your channel" / "Subscribed".
        public let secondLine: String

        public init(
            title: String,
            handleLabel: String?,
            subscribersLabel: String?,
            timeLabel: String?,
            secondLine: String
        ) {
            self.title = title
            self.handleLabel = handleLabel
            self.subscribersLabel = subscribersLabel
            self.timeLabel = timeLabel
            self.secondLine = secondLine
        }
    }

    /// ADR-0051 §5 — the channel title precedence, shared with the reader
    /// header: the shared name, else the local name, else the bare handle,
    /// else "Channel".
    public static func title(sharedName: String, localName: String, handle: String) -> String {
        let shared = sharedName.trimmingCharacters(in: .whitespacesAndNewlines)
        if !shared.isEmpty { return shared }
        let local = localName.trimmingCharacters(in: .whitespacesAndNewlines)
        if !local.isEmpty { return local }
        let bare = bareHandle(handle)
        return bare.isEmpty ? "Channel" : bare
    }

    /// The handle without whitespace and without any leading `@`.
    public static func bareHandle(_ handle: String) -> String {
        String(handle.trimmingCharacters(in: .whitespacesAndNewlines).drop(while: { $0 == "@" }))
    }

    /// "1 subscriber" / "12 subscribers".
    public static func subscriberCountLabel(_ count: Int) -> String {
        count == 1 ? "1 subscriber" : "\(count) subscribers"
    }

    public static func rowModel(
        sharedName: String = "",
        name: String,
        handle: String,
        isOwner: Bool = false,
        subscriberCount: Int?,
        newestPostMs: Int64?,
        nowMs: Int64,
        calendar: Calendar,
        locale: Locale
    ) -> RowModel {
        let title = Self.title(sharedName: sharedName, localName: name, handle: handle)
        let bare = bareHandle(handle)
        let handleLabel = bare.isEmpty ? nil : "@\(bare)"
        let subscribersLabel = subscriberCount.map(subscriberCountLabel)
        let timeLabel = newestPostMs.map {
            ChatListFormatting.lastMessageTime(
                ms: $0, nowMs: nowMs, calendar: calendar, locale: locale)
        }
        // Never the handle twice: when the title fell through to the handle,
        // the second line carries only the count / the role.
        let handleOnSecondLine = title == bare ? nil : handleLabel
        var parts = [handleOnSecondLine, subscribersLabel].compactMap { $0 }
        if parts.isEmpty { parts = [isOwner ? "Your channel" : "Subscribed"] }
        return RowModel(
            title: title,
            handleLabel: handleLabel,
            subscribersLabel: subscribersLabel,
            timeLabel: timeLabel,
            secondLine: parts.joined(separator: " \u{00B7} ")
        )
    }

    /// The reader header's one-line status (UX spec §3.2 / §4): the handle
    /// when the title is not already the handle, else the role.
    public static func headerStatus(title: String, handle: String, isOwner: Bool) -> String {
        let bare = bareHandle(handle)
        if !bare.isEmpty, title != bare { return "@\(bare)" }
        return isOwner ? "Your channel" : "Subscribed"
    }

    /// The newest post's server time among `posts`, `nil` when empty.
    public static func newestPostMs(_ postTimesMs: [UInt64]) -> Int64? {
        guard let max = postTimesMs.max() else { return nil }
        return Int64(clamping: max)
    }

    /// Case- and diacritic-insensitive match of the search text against the
    /// row's title, handle and second line. An empty query matches everything.
    public static func matches(query: String, row: RowModel) -> Bool {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
            .drop(while: { $0 == "@" })
        guard !needle.isEmpty else { return true }
        let options: String.CompareOptions = [.caseInsensitive, .diacriticInsensitive]
        let haystacks = [row.title, row.handleLabel ?? "", row.secondLine]
        return haystacks.contains { $0.range(of: needle, options: options) != nil }
    }

    // MARK: Relative time

    /// How long ago something happened, in one short phrase: "Just now",
    /// "5 min ago", "2 h ago", "Yesterday", then the short date ("16 Sep",
    /// with the year when it differs). Used on post cards and the access
    /// refresh line; the list rows use the chat-list vocabulary instead.
    public static func relativeLabel(
        ms: Int64, nowMs: Int64, calendar: Calendar, locale: Locale
    ) -> String {
        let deltaS = max(0, (nowMs - ms) / 1000)
        if deltaS < 60 { return "Just now" }
        if deltaS < 3600 { return "\(deltaS / 60) min ago" }
        let date = Date(timeIntervalSince1970: Double(ms) / 1000)
        let now = Date(timeIntervalSince1970: Double(nowMs) / 1000)
        if calendar.isDate(date, inSameDayAs: now) { return "\(deltaS / 3600) h ago" }
        if let yesterday = calendar.date(byAdding: .day, value: -1, to: now),
           calendar.isDate(date, inSameDayAs: yesterday) {
            return "Yesterday"
        }
        var style = Date.FormatStyle(locale: locale, calendar: calendar, timeZone: calendar.timeZone)
        if calendar.component(.year, from: date) == calendar.component(.year, from: now) {
            style = style.day().month(.abbreviated)
        } else {
            style = style.day().month(.abbreviated).year()
        }
        return date.formatted(style)
    }

    /// The access-refresh status as one plain line (UX spec section 5.4).
    /// `lastRotationMs == nil` or `0` reads as never refreshed.
    public static func accessRefreshLine(
        lastRotationMs: UInt64?, nowMs: Int64, calendar: Calendar, locale: Locale
    ) -> String {
        guard let last = lastRotationMs, last > 0 else {
            return "Access refreshes automatically; not refreshed yet."
        }
        let ago = relativeLabel(
            ms: Int64(clamping: last), nowMs: nowMs, calendar: calendar, locale: locale)
        let when = ago == "Just now" ? "just now" : ago
        return "Access last refreshed \(when); refreshes automatically."
    }
}

// MARK: - Shared views

/// "Not available" plus one plain-language reason. Never the key material,
/// the epoch, or the failure class the decrypt path reported.
struct UnavailablePostView: View {
    let reason: String

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text("Not available")
                .font(.body)
                .foregroundStyle(Color.lakkoText)
            Text(reason)
                .font(.footnote)
                .foregroundStyle(Color.lakkoTextMuted)
        }
        .accessibilityElement(children: .combine)
    }
}

/// The reasons a post can be unreadable, in the words the card shows.
enum UnavailablePostReason {
    /// The post predates this device's access (older key window).
    static let beforeAccess = "Published before you subscribed."
    /// The seed for the post's epoch is held but the post did not verify.
    static let notVerified = "Could not be verified on this device."
}

// MARK: - hex / base64url helpers shared by the Channels UI

extension Data {
    /// Decode lowercase hex (a subscriber public key's wire form) into
    /// bytes. `nil` on any failure.
    init?(channelHex hex: String) {
        let chars = Array(hex)
        guard chars.count % 2 == 0 else { return nil }
        var out = Data(capacity: chars.count / 2)
        var i = 0
        while i < chars.count {
            guard let byte = UInt8(String(chars[i...(i + 1)]), radix: 16)
            else { return nil }
            out.append(byte)
            i += 2
        }
        self = out
    }

    /// Decode a base64url-no-pad string into `Data`. Local copy of
    /// `ChannelsComposer`'s private helper so the reader screen does not
    /// reach into composer internals.
    init?(base64URLNoPadSubscriber s: String) {
        var std = s
            .replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        let pad = (4 - (std.count % 4)) % 4
        std.append(String(repeating: "=", count: pad))
        guard let data = Data(base64Encoded: std) else { return nil }
        self = data
    }
}
