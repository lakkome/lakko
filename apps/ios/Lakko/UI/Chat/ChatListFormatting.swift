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

    /// `docs/ux/media-messages.md` section 11 — how many photos, videos and
    /// plain files a message carries (anything else: a voice message, a GIF,
    /// an audio file, keeps its own word).
    public struct AttachmentCounts: Equatable {
        public var photos = 0
        public var videos = 0
        public var files = 0
        public var other = 0

        public var media: Int { photos + videos }
    }

    public static func attachmentCounts(
        _ refs: [AttachmentRef], allowlist: InlineMedia.Allowlist = InlineMedia.coreAllowlist
    ) -> AttachmentCounts {
        var counts = AttachmentCounts()
        for ref in refs {
            let decision = InlineMedia.decision(kind: ref.kind, mime: ref.mime, allowlist: allowlist)
            switch decision.bubble {
            case .photo: counts.photos += 1
            case .video: counts.videos += 1
            case .file where ref.kind == nil && decision.inline == nil: counts.files += 1
            default: counts.other += 1
            }
        }
        return counts
    }

    /// Section 11 — the words a message of photos, videos or files says
    /// without text: `Photo`, `3 photos`, `Video`, `2 videos`, `4 photos and
    /// videos`, `File`, `3 files`. `nil` when the message carries anything
    /// else (its own words apply) or nothing.
    public static func attachmentWords(_ counts: AttachmentCounts) -> String? {
        guard counts.other == 0 else { return nil }
        if counts.media > 0 {
            if counts.photos > 0, counts.videos > 0 {
                return Localization.text("chat.media.photos_and_videos", counts: ["count": UInt64(counts.media)])
            }
            if counts.photos > 0 {
                return Localization.text("chat.media.photos", counts: ["count": UInt64(counts.photos)])
            }
            return Localization.text("chat.media.videos", counts: ["count": UInt64(counts.videos)])
        }
        if counts.files > 0 {
            return Localization.text("chat.media.files", counts: ["count": UInt64(counts.files)])
        }
        return nil
    }

    /// The attachment labels ``preview(text:attachmentLabels:direction:deleted:unsupported:isGroup:senderName:)``
    /// takes: section 11's words for photos, videos and files, else one word
    /// per attachment.
    public static func attachmentLabels(
        _ refs: [AttachmentRef], allowlist: InlineMedia.Allowlist = InlineMedia.coreAllowlist
    ) -> [String] {
        if let words = attachmentWords(attachmentCounts(refs, allowlist: allowlist)) { return [words] }
        return refs.map { mediaLabel(kind: $0.kind, mime: $0.mime, allowlist: allowlist) }
    }

    /// Section 11 — the separator between a files-only message's words and its
    /// text ("2 files · Weekend plans"), the literal web and Android use.
    public static let filesTextSeparator = " · "

    /// Section 11 — a message's text as the row shows it: a message whose
    /// attachments are only plain files says the words, then the text; any
    /// other message keeps its text as it is.
    public static func textWithFileWords(
        _ text: String, refs: [AttachmentRef], allowlist: InlineMedia.Allowlist = InlineMedia.coreAllowlist
    ) -> String {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return text }
        let counts = attachmentCounts(refs, allowlist: allowlist)
        guard counts.files > 0, counts.media == 0, counts.other == 0,
              let words = attachmentWords(counts) else { return text }
        return words + filesTextSeparator + trimmed
    }

    /// Section 11 — the first photo or video of a message (the row's 20 pt
    /// thumbnail), with how many photos and videos it carries (the stack).
    public static func previewMedia(
        _ refs: [AttachmentRef], allowlist: InlineMedia.Allowlist = InlineMedia.coreAllowlist
    ) -> ConversationPreviewMedia? {
        let media = refs.filter {
            let bubble = InlineMedia.decision(kind: $0.kind, mime: $0.mime, allowlist: allowlist).bubble
            return bubble == .photo || bubble == .video
        }
        guard let first = media.first else { return nil }
        return ConversationPreviewMedia(
            thumb: first.thumb,
            isVideo: InlineMedia.decision(kind: first.kind, mime: first.mime, allowlist: allowlist).bubble == .video,
            count: media.count)
    }

    /// ADR-0087 sections 14 and 15.2 — what the row says for a message that
    /// carries a channel invitation card: the invitation, never the `/g/`
    /// link the message's text holds.
    public static func channelInvitePreview(channelName: String, publisher: Bool) -> String {
        Localization.text(
            publisher ? "chats.preview.publisher_invite" : "chats.preview.channel_invite",
            texts: ["channel": channelName])
    }

    /// The second line of a row. `senderName` is the resolved name of the
    /// message's author and is shown as a prefix only in a group; own
    /// messages read "You: ...".
    public static func preview(
        text: String,
        attachmentLabels: [String],
        direction: Direction,
        deleted: Bool,
        unsupported: Bool = false,
        isGroup: Bool,
        senderName: String?
    ) -> String {
        let body: String
        if deleted {
            body = Localization.text("chat.message.deleted")
        } else if unsupported {
            // ADR-0061 section 6 — a message from a newer version of Lakko;
            // its text is empty and the sentence is the client's own.
            body = Localization.text("chat.message.unsupported")
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
