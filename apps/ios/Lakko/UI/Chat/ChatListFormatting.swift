// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The pure text rules behind a conversation row (UX spec section 5.1): the
/// time on the first line, the preview on the second, and the search match.
/// Everything takes its clock, calendar and locale as parameters so the
/// tests pin the strings without depending on the device.
public enum ChatListFormatting {

    // MARK: Time

    /// How old a message is, relative to `nowMs`, in the three bands the row
    /// renders differently.
    public enum TimeBucket: Equatable {
        /// Same calendar day: the time of day.
        case today
        /// Within the last six days: the short weekday.
        case thisWeek
        /// Older, same year: day and short month.
        case sameYear
        /// Another year: day, short month and year.
        case otherYear
    }

    public static func bucket(ms: Int64, nowMs: Int64, calendar: Calendar) -> TimeBucket {
        let date = Date(timeIntervalSince1970: Double(ms) / 1000)
        let now = Date(timeIntervalSince1970: Double(nowMs) / 1000)
        if calendar.isDate(date, inSameDayAs: now) { return .today }
        let startOfToday = calendar.startOfDay(for: now)
        if let weekAgo = calendar.date(byAdding: .day, value: -6, to: startOfToday), date >= weekAgo,
           date < startOfToday {
            return .thisWeek
        }
        if calendar.component(.year, from: date) == calendar.component(.year, from: now) {
            return .sameYear
        }
        return .otherYear
    }

    /// The right-aligned time on a row's first line: "14:05" (in the
    /// device's time style) today, "Tue" this week, "16 Sep" older, "16 Sep
    /// 2025" in another year.
    public static func lastMessageTime(
        ms: Int64, nowMs: Int64, calendar: Calendar, locale: Locale
    ) -> String {
        let date = Date(timeIntervalSince1970: Double(ms) / 1000)
        var style = Date.FormatStyle(locale: locale, calendar: calendar, timeZone: calendar.timeZone)
        switch bucket(ms: ms, nowMs: nowMs, calendar: calendar) {
        case .today:
            style = style.hour().minute()
        case .thisWeek:
            style = style.weekday(.abbreviated)
        case .sameYear:
            style = style.day().month(.abbreviated)
        case .otherYear:
            style = style.day().month(.abbreviated).year()
        }
        return date.formatted(style)
    }

    // MARK: Preview

    /// The one-word label for a media message without text.
    public static func mediaLabel(
        kind: String?, mime: String?, allowlist: InlineMedia.Allowlist = InlineMedia.coreAllowlist
    ) -> String {
        let decision = InlineMedia.decision(kind: kind, mime: mime, allowlist: allowlist)
        switch decision.bubble {
        case .voice: return Localization.text("chat.media.voice_message")
        case .videoNote: return Localization.text("chat.media.video_message")
        case .gif: return Localization.text("chat.media.gif")
        case .photo: return Localization.text("chat.media.photo")
        case .video: return Localization.text("chat.media.video")
        case .file:
            switch decision.inline {
            case .image?: return Localization.text("chat.media.photo")
            case .video?: return Localization.text("chat.media.video")
            case .audio?: return Localization.text("chat.media.audio")
            case nil: return Localization.text("chat.media.file")
            @unknown default: return Localization.text("chat.media.file")
            }
        }
    }

    /// The second line of a row. `senderName` is the resolved name of the
    /// message's author and is shown as a prefix only in a group; own
    /// messages read "You: ...".
    public static func preview(
        text: String,
        attachmentLabels: [String],
        direction: Direction,
        deleted: Bool,
        isGroup: Bool,
        senderName: String?
    ) -> String {
        let body: String
        if deleted {
            body = Localization.text("chat.message.deleted")
        } else if let call = CallTranscriptRow.parse(text) {
            // `docs/ux/calls.md` section 6 — the list shows the call the way
            // the conversation does, never the stored marker.
            body = call.text
        } else {
            let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty {
                body = trimmed.replacingOccurrences(of: "\n", with: " ")
            } else if let first = attachmentLabels.first {
                body = attachmentLabels.count > 1
                    ? Localization.text(
                        "chat.media.and_more",
                        texts: ["label": first],
                        counts: ["count": UInt64(attachmentLabels.count - 1)])
                    : first
            } else {
                body = ""
            }
        }
        switch direction {
        case .outgoing:
            return Localization.text("chats.preview.own", texts: ["text": body])
        case .incoming:
            if isGroup, let senderName, !senderName.isEmpty {
                return Localization.text("chats.preview.sender", texts: ["name": senderName, "text": body])
            }
            return body
        case .system:
            return body
        }
    }

    // MARK: Search

    /// Case- and diacritic-insensitive match of the search text against the
    /// row's title and preview. An empty query matches everything.
    public static func matches(query: String, title: String, preview: String) -> Bool {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return true }
        let options: String.CompareOptions = [.caseInsensitive, .diacriticInsensitive]
        return title.range(of: needle, options: options) != nil
            || preview.range(of: needle, options: options) != nil
    }
}
