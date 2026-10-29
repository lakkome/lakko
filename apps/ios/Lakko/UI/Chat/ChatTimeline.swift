// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The pure layout rules of the chat timeline (UX spec section 5.2): where a
/// date separator goes, which consecutive messages form one run, and how a
/// message's time is written in the bubble corner. Clock, calendar and locale
/// are parameters so the tests pin the output.
public enum ChatTimeline {

    /// Consecutive messages from one sender closer than this form one run
    /// (sender name and avatar once, tighter spacing inside).
    public static let runWindowMs: Int64 = 5 * 60 * 1000

    /// One rendered timeline row.
    public struct Entry: Equatable {
        public let message: ChatMessage
        /// The separator to draw ABOVE this message, when it opens a new day.
        public let dateLabel: String?
        /// `true` when this message opens a run (shows the sender's name and
        /// avatar in a group).
        public let startsRun: Bool
        /// `true` when this message closes a run (the bubble after it belongs
        /// to another run or another day).
        public let endsRun: Bool
        /// The bubble-corner time, `nil` when the message carries no
        /// parseable server time.
        public let timeLabel: String?
    }

    /// Whether `next` continues the run `previous` is in: same side and same
    /// author, neither a system row, and less than ``runWindowMs`` apart.
    /// A message without a parseable time never joins a run: the gap cannot
    /// be measured, and grouping across an unknown gap would hide a sender
    /// name that may be needed.
    public static func continuesRun(
        previous: ChatMessage, previousMs: Int64?,
        next: ChatMessage, nextMs: Int64?
    ) -> Bool {
        guard previous.direction != .system, next.direction != .system else { return false }
        guard previous.direction == next.direction else { return false }
        guard previous.authorKeyB64 == next.authorKeyB64 else { return false }
        guard let previousMs, let nextMs else { return false }
        let gap = nextMs - previousMs
        return gap >= 0 && gap < runWindowMs
    }

    /// "Today", "Yesterday", "16 September", or "16 September 2025" for
    /// another year.
    public static func dateLabel(ms: Int64, nowMs: Int64, calendar: Calendar, locale: Locale) -> String {
        let date = Date(timeIntervalSince1970: Double(ms) / 1000)
        let now = Date(timeIntervalSince1970: Double(nowMs) / 1000)
        if calendar.isDate(date, inSameDayAs: now) { return Localization.text("chat.day.today") }
        if let yesterday = calendar.date(byAdding: .day, value: -1, to: now),
           calendar.isDate(date, inSameDayAs: yesterday) {
            return Localization.text("chat.day.yesterday")
        }
        var style = Date.FormatStyle(locale: locale, calendar: calendar, timeZone: calendar.timeZone)
            .day().month(.wide)
        if calendar.component(.year, from: date) != calendar.component(.year, from: now) {
            style = style.year()
        }
        return date.formatted(style)
    }

    /// The bubble-corner time in the device's time style ("14:05" or
    /// "2:05 PM").
    /// What an outgoing bubble's time slot says.
    ///
    /// A message that has not reached the server has no server time to show,
    /// so the slot carries its state instead: it is the same place the eye
    /// already looks for "when", and it means the bubble needs no second
    /// line. "Not sent" is the word the web client and the media bubbles
    /// both use (`docs/ux/media-messages.md` section 3) - two spellings of
    /// the same thing would be one of them being wrong.
    public static func statusLabel(ack: ChatMessage.Ack, time: String?) -> String? {
        switch ack {
        case .pending: return Localization.text("chat.message.sending")
        case .failed: return Localization.text("chat.message.not_sent")
        case .sent: return time
        }
    }

    public static func timeLabel(ms: Int64, calendar: Calendar, locale: Locale) -> String {
        let date = Date(timeIntervalSince1970: Double(ms) / 1000)
        let style = Date.FormatStyle(locale: locale, calendar: calendar, timeZone: calendar.timeZone)
            .hour().minute()
        return date.formatted(style)
    }

    /// Lay the timeline out: date separators, runs, corner times. `parseMs`
    /// turns a message's server `createdAt` into milliseconds (`nil` when it
    /// does not parse; such a message shows no time and no separator of its
    /// own, and never joins a run).
    public static func layout(
        messages: [ChatMessage],
        nowMs: Int64,
        calendar: Calendar,
        locale: Locale,
        parseMs: (String) -> Int64?
    ) -> [Entry] {
        let stamps = messages.map { parseMs($0.createdAt) }
        var entries: [Entry] = []
        entries.reserveCapacity(messages.count)
        var lastDay: Date?
        for (index, message) in messages.enumerated() {
            let ms = stamps[index]
            var dateLabel: String?
            if let ms {
                let day = calendar.startOfDay(for: Date(timeIntervalSince1970: Double(ms) / 1000))
                if lastDay != day {
                    dateLabel = Self.dateLabel(ms: ms, nowMs: nowMs, calendar: calendar, locale: locale)
                    lastDay = day
                }
            }
            let startsRun: Bool
            if index == 0 || dateLabel != nil {
                startsRun = true
            } else {
                startsRun = !continuesRun(
                    previous: messages[index - 1], previousMs: stamps[index - 1],
                    next: message, nextMs: ms)
            }
            let endsRun: Bool
            if index == messages.count - 1 {
                endsRun = true
            } else {
                let nextMs = stamps[index + 1]
                let nextOpensDay: Bool = {
                    guard let ms, let nextMs else { return false }
                    return !calendar.isDate(
                        Date(timeIntervalSince1970: Double(ms) / 1000),
                        inSameDayAs: Date(timeIntervalSince1970: Double(nextMs) / 1000))
                }()
                endsRun = nextOpensDay
                    || !continuesRun(
                        previous: message, previousMs: ms,
                        next: messages[index + 1], nextMs: nextMs)
            }
            entries.append(
                Entry(
                    message: message,
                    dateLabel: dateLabel,
                    startsRun: startsRun,
                    endsRun: endsRun,
                    timeLabel: statusLabel(
                        ack: message.ack,
                        time: ms.map { timeLabel(ms: $0, calendar: calendar, locale: locale) })))
        }
        return entries
    }

    /// The two bookkeeping rows the receive loop files as system rows carry
    /// protocol names and nothing a reader acts on; the timeline does not draw
    /// them (UX spec section 6).
    ///
    /// A commit row that names people (``MembershipRow``) is drawn only in a
    /// group's timeline (`isGroup`, `docs/ux/second-device.md` section 4): a
    /// one-to-one never shows one, and neither does the chat-list preview,
    /// which asks with the default.
    public static func isHiddenSystemRow(_ message: ChatMessage, isGroup: Bool = false) -> Bool {
        guard message.direction == .system else { return false }
        if message.text == MembershipRow.plainCommitText || message.text == "[mls external join]" { return true }
        if message.text.hasPrefix("[mls ") {
            return !(isGroup && MembershipRow.parse(message.text) != nil)
        }
        // `docs/ux/calls.md` section 6 — a call row IS drawn; the sentence is
        // rendered from its parsed marker. One this build cannot read is
        // hidden rather than shown as the raw marker.
        if CallTranscriptRow.isCallRow(message.text) {
            return CallTranscriptRow.parse(message.text) == nil
        }
        return false
    }

    /// The people a membership row names, or `nil` when it is not one.
    public static func membershipRow(_ message: ChatMessage) -> MembershipRow? {
        guard message.direction == .system else { return nil }
        return MembershipRow.parse(message.text)
    }

    /// The finished call this row records, or `nil` when it is not a call row.
    public static func callRow(_ message: ChatMessage) -> CallTranscriptRow? {
        guard message.direction == .system else { return nil }
        return CallTranscriptRow.parse(message.text)
    }
}
