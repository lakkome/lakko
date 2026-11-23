// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Recent calls, across every conversation, newest first.
///
/// `docs/ux/navigation.md` sections 5 and 6.2. A call belongs to a
/// conversation, and that is where its row is written (``CallTranscriptRow``,
/// a system row in the chat timeline). The Calls screen wants the opposite
/// cut: one sequence in time order regardless of who the call was with, since
/// a person looking for a call remembers roughly when, not which thread it
/// belonged to.
///
/// Two requirements the spec states, and what they mean here.
///
/// **It must be an index, not a scan.** Deriving this list by walking every
/// conversation's transcript on every open gets worse with use — it is
/// proportional to everything the device has ever said, to show a screen whose
/// content is proportional to how often the person calls. So the row is
/// written into ``CallHistoryStore`` at the moment the transcript row is, and
/// the screen reads that.
///
/// **It must hold the parts, not the sentence.** A list sorts and filters on
/// an outcome and a timestamp; a row that stored the finished words ("Missed
/// call") could not be sorted on and would have to be migrated the day the
/// wording changed. So the entry carries the parts and the words come back out
/// of ``CallTranscriptRow`` — the same type the conversation renders — so the
/// two surfaces can never word one call two ways.
///
/// Everything in this file is pure and takes its clock, calendar and locale as
/// parameters, so the day boundaries can be pinned by a test without waiting
/// for midnight or depending on the device's region.

/// How a call finished, as the index stores it.
///
/// The three cases of ``CallTranscriptRow/Outcome`` with the duration lifted
/// out, because the duration is a separate column the list sorts and the
/// outcome is a value it filters.
public enum CallHistoryOutcome: String, Codable, Equatable, Sendable {
    case answered
    case missed
    case declined
    case failed
}

/// Which screen a row opens, as the index stores it. Only the two MLS kinds
/// can hold a call; a channel is a broadcast and has none.
///
/// `noConversation` is a call that belongs to no conversation
/// (`docs/ux/navigation.md` section 5.1.1): one started with `New call`, or
/// one this device joined from a call link. Its row opens nothing.
public enum CallHistoryKind: String, Codable, Equatable, Sendable {
    case chat
    case group
    case noConversation = "no_conversation"
}

/// One call, reduced to its parts.
public struct CallHistoryEntry: Codable, Equatable, Identifiable, Sendable {
    /// Unique within the index. The start time and the conversation are not
    /// enough on their own — two calls can begin in the same millisecond after
    /// a restore rounded them — and a list that reshuffles on a refresh reads
    /// as a bug and is impossible to report.
    public let id: String
    /// The conversation the call belongs to: a group id hex. `nil` for a call
    /// with no conversation (section 5.1.1).
    public let conversationId: String?
    public let kind: CallHistoryKind
    /// A call with no conversation: the call's id, hex, which is what makes it
    /// one row per call on this device. `nil` for a conversation's call.
    public let callIdHex: String?
    public let direction: CallTranscriptRow.Direction
    public let outcome: CallHistoryOutcome
    /// Whether the call was placed or accepted with the camera on, so calling
    /// back re-dials in the same mode.
    public let video: Bool
    public let startedAtMs: Int64
    /// How long media flowed. `0` for a call that never connected.
    public let durationMs: Int64

    public init(
        id: String,
        conversationId: String,
        kind: CallHistoryKind,
        direction: CallTranscriptRow.Direction,
        outcome: CallHistoryOutcome,
        video: Bool,
        startedAtMs: Int64,
        durationMs: Int64
    ) {
        self.id = id
        self.conversationId = conversationId
        self.kind = kind
        self.callIdHex = nil
        self.direction = direction
        self.outcome = outcome
        self.video = video
        self.startedAtMs = startedAtMs
        self.durationMs = max(0, durationMs)
    }

    private init(
        callIdHex: String,
        direction: CallTranscriptRow.Direction,
        video: Bool,
        startedAtMs: Int64,
        durationMs: Int64
    ) {
        self.id = Self.noConversationId(callIdHex: callIdHex)
        self.conversationId = nil
        self.kind = .noConversation
        self.callIdHex = callIdHex
        self.direction = direction
        // Such a call never rings, so it is never missed or declined: the
        // device was in it, or there is no row at all.
        self.outcome = .answered
        self.video = video
        self.startedAtMs = startedAtMs
        self.durationMs = max(0, durationMs)
    }

    /// A call with no conversation (`docs/ux/navigation.md` section 5.1.1):
    /// `outgoing` when this device started it with `New call`, `incoming` when
    /// it joined from a link.
    public static func noConversation(
        callIdHex: String,
        direction: CallTranscriptRow.Direction,
        video: Bool,
        startedAtMs: Int64,
        durationMs: Int64
    ) -> CallHistoryEntry {
        CallHistoryEntry(
            callIdHex: callIdHex, direction: direction,
            video: video, startedAtMs: startedAtMs, durationMs: durationMs)
    }

    /// The index id of a call with no conversation: the call id alone, so the
    /// same call is filed once on this device whoever writes it.
    public static func noConversationId(callIdHex: String) -> String {
        "call.\(callIdHex.lowercased())"
    }

    /// The parts assembled back into the conversation's own row, which is
    /// where the wording lives (`Outgoing call, 12:04` / `Incoming call,
    /// 12:04` / `Missed call` / `Declined`). Rendering the sentence from here
    /// rather than storing it is what keeps the Calls screen and the
    /// conversation from wording one call two ways.
    public var transcriptRow: CallTranscriptRow {
        switch outcome {
        case .answered:
            return CallTranscriptRow(direction: direction, outcome: .answered(durationMs: durationMs))
        case .missed:
            return CallTranscriptRow(direction: direction, outcome: .missed)
        case .declined:
            return CallTranscriptRow(direction: direction, outcome: .declined)
        case .failed:
            return CallTranscriptRow(direction: direction, outcome: .failed)
        }
    }

    /// The parts as the index stores them, from a finished call's row.
    public static func outcome(of row: CallTranscriptRow) -> (CallHistoryOutcome, Int64) {
        switch row.outcome {
        case .answered(let durationMs): return (.answered, max(0, durationMs))
        case .missed: return (.missed, 0)
        case .declined: return (.declined, 0)
        case .failed: return (.failed, 0)
        }
    }

    /// A call row of a conversation's log, as the index files it: keyed on
    /// the conversation, the row's time (when the call ended) and the
    /// direction. `nil` for a row that is not a readable call row or whose
    /// time cannot be read: a list ordered by time has no place for it.
    ///
    /// Used by the one-time fill from past calls and for the call rows a
    /// history bundle brings, which arrive after that fill has run; both key
    /// a row the same way, so a row filed by one is replaced, not duplicated,
    /// by the other.
    static func fromLogRow(
        conversationId: String, kind: CallHistoryKind, text: String, createdAt: String
    ) -> CallHistoryEntry? {
        guard let row = CallTranscriptRow.parse(text),
              let at = ChatRepository.parseDsTimestampMs(createdAt) else { return nil }
        let (outcome, durationMs) = Self.outcome(of: row)
        return CallHistoryEntry(
            id: "\(conversationId).\(at).\(row.direction.rawValue)",
            conversationId: conversationId,
            kind: kind,
            direction: row.direction,
            outcome: outcome,
            video: row.video,
            startedAtMs: at,
            durationMs: durationMs)
    }

    /// A missed or declined call is the one outcome the reader may want to act
    /// on, so the row carries the danger tint — the same rule the conversation
    /// already applies.
    public var isUnanswered: Bool { outcome != .answered }
}

/// A run of calls that happened on the same calendar day.
public struct CallHistoryDay: Equatable, Identifiable {
    /// `YYYY-MM-DD` in the viewer's own zone; the key, not the label.
    public let day: String
    public let rows: [CallHistoryEntry]

    public var id: String { day }
}

public enum CallHistory {

    /// How many rows the screen keeps. Older calls stay in their conversation,
    /// where they were written.
    public static let maxRows = 300

    /// Newest first, with a total order.
    ///
    /// Ties break on the id so the order never depends on the order the store
    /// happened to yield: two calls that started in the same millisecond would
    /// otherwise swap places on an unrelated refresh.
    public static func ordered(
        _ entries: [CallHistoryEntry], limit: Int = CallHistory.maxRows
    ) -> [CallHistoryEntry] {
        let sorted = entries.sorted { lhs, rhs in
            if lhs.startedAtMs != rhs.startedAtMs { return lhs.startedAtMs > rhs.startedAtMs }
            return lhs.id < rhs.id
        }
        return limit >= 0 ? Array(sorted.prefix(limit)) : sorted
    }

    /// Group an already-ordered list into days.
    ///
    /// The day is computed in the VIEWER's zone rather than UTC: a call at
    /// 00:30 local is today to the person who made it, and a list that files
    /// it under yesterday because UTC says so is wrong in a way nobody would
    /// think to report.
    public static func byDay(
        _ rows: [CallHistoryEntry], calendar: Calendar
    ) -> [CallHistoryDay] {
        var days: [CallHistoryDay] = []
        var currentDay: String?
        var currentRows: [CallHistoryEntry] = []
        for row in rows {
            let day = localDay(ms: row.startedAtMs, calendar: calendar)
            if day != currentDay {
                if let currentDay { days.append(CallHistoryDay(day: currentDay, rows: currentRows)) }
                currentDay = day
                currentRows = []
            }
            currentRows.append(row)
        }
        if let currentDay { days.append(CallHistoryDay(day: currentDay, rows: currentRows)) }
        return days
    }

    /// `YYYY-MM-DD` in the viewer's zone.
    public static func localDay(ms: Int64, calendar: Calendar) -> String {
        let date = Date(timeIntervalSince1970: Double(ms) / 1000)
        let parts = calendar.dateComponents([.year, .month, .day], from: date)
        let year = parts.year ?? 0
        let month = parts.month ?? 0
        let day = parts.day ?? 0
        return String(format: "%04d-%02d-%02d", year, month, day)
    }

    /// The heading for a day of calls: `Today`, `Yesterday`, or the date — the
    /// same three the chat timeline uses.
    ///
    /// `now` is a parameter rather than read here so the boundary can be tested
    /// without waiting for midnight.
    public static func dayLabel(
        _ day: String, now: Date, calendar: Calendar, locale: Locale
    ) -> String {
        let nowMs = Int64(now.timeIntervalSince1970 * 1000)
        if day == localDay(ms: nowMs, calendar: calendar) { return Localization.text("chat.day.today") }
        if let yesterday = calendar.date(byAdding: .day, value: -1, to: now),
           day == localDay(ms: Int64(yesterday.timeIntervalSince1970 * 1000), calendar: calendar) {
            return Localization.text("chat.day.yesterday")
        }
        let fields = day.split(separator: "-").compactMap { Int($0) }
        guard fields.count == 3 else { return day }
        var components = DateComponents()
        components.year = fields[0]
        components.month = fields[1]
        components.day = fields[2]
        guard let date = calendar.date(from: components) else { return day }
        var style = Date.FormatStyle(locale: locale, calendar: calendar, timeZone: calendar.timeZone)
        if calendar.component(.year, from: date) == calendar.component(.year, from: now) {
            style = style.day().month(.wide)
        } else {
            style = style.day().month(.wide).year()
        }
        return date.formatted(style)
    }

    /// The second line of a Calls row: the outcome in the words the
    /// conversation already uses, and the time of day.
    ///
    /// `Outgoing call, 12:04` already carries the DURATION as its trailing
    /// field, so the clock time is appended rather than substituted — the two
    /// answer different questions ("how long" and "when").
    public static func rowSubtitle(
        _ entry: CallHistoryEntry, calendar: Calendar, locale: Locale
    ) -> String {
        let clock = timeOfDay(ms: entry.startedAtMs, calendar: calendar, locale: locale)
        return Localization.joinList([entry.transcriptRow.text, clock])
    }

    /// What VoiceOver reads for a row: the conversation, then the call, then
    /// what a tap does — the duration spoken as a duration rather than read
    /// digit by digit (`docs/ux/calls.md` section 8), through the same
    /// ``CallTranscriptRow/spokenDuration(ms:)`` the conversation uses.
    ///
    /// It does NOT reuse ``CallTranscriptRow/accessibilityLabel`` whole,
    /// because that one ends by promising a tap calls back and here a tap
    /// opens the conversation — the call-back is the separate trailing button.
    public static func rowAccessibilityLabel(
        _ entry: CallHistoryEntry, title: String, calendar: Calendar, locale: Locale
    ) -> String {
        let clock = timeOfDay(ms: entry.startedAtMs, calendar: calendar, locale: locale)
        let video = entry.video
        let outgoing = entry.direction == .outgoing
        let head: String
        switch entry.outcome {
        case .answered:
            let key: String
            switch (video, outgoing) {
            case (true, true): key = "call.log.a11y.video_outgoing"
            case (true, false): key = "call.log.a11y.video_incoming"
            case (false, true): key = "call.log.a11y.voice_outgoing"
            case (false, false): key = "call.log.a11y.voice_incoming"
            }
            head = Localization.text(
                "call.log.a11y.lasted",
                texts: [
                    "head": Localization.text(key),
                    "duration": CallTranscriptRow.spokenDuration(ms: entry.durationMs),
                ])
        case .missed:
            head = Localization.text(video ? "call.log.a11y.video_missed" : "call.log.a11y.voice_missed")
        case .declined:
            head = Localization.text(video ? "call.log.a11y.video_declined" : "call.log.a11y.voice_declined")
        case .failed:
            head = entry.transcriptRow.text
        }
        return Localization.joinList([title, head, clock])
    }

    /// What VoiceOver reads for the trailing button, which is a second target
    /// with a second job and so needs its own label.
    public static func callBackAccessibilityLabel(_ entry: CallHistoryEntry) -> String {
        Localization.text(entry.video ? "call.history.call_back_video_a11y" : "call.history.call_back_a11y")
    }

    /// What a Calls row is (`docs/ux/navigation.md` sections 5.1 and 5.1.1).
    public enum RowShape: Equatable {
        /// A conversation's call: avatar and title of the conversation, the
        /// row opens it, and a trailing button calls back in the same mode.
        case conversation(groupIdHex: String, kind: CallHistoryKind)
        /// A call with no conversation: the group-call glyph, `title`, no
        /// call-back button, and the row is not a tap target (an ended link
        /// call cannot be joined again and there is nothing to open).
        case noConversation(title: String)
    }

    /// The one decision the Calls screen draws a row from.
    ///
    /// A conversation kind with no conversation id cannot open anything, so
    /// it is drawn as a call with no conversation rather than as a row whose
    /// tap goes nowhere.
    public static func rowShape(_ entry: CallHistoryEntry) -> RowShape {
        switch entry.kind {
        case .chat, .group:
            if let groupIdHex = entry.conversationId {
                return .conversation(groupIdHex: groupIdHex, kind: entry.kind)
            }
            return .noConversation(title: noConversationTitle)
        case .noConversation:
            return .noConversation(title: noConversationTitle)
        }
    }

    /// The title of a call with no conversation: always `Group call`, the
    /// words the call screen uses. A call has no name of its own (`Your name
    /// in this call` names a person, not the call).
    public static var noConversationTitle: String {
        Localization.text("call.label.group")
    }

    /// The direction glyph at the leading edge, so the direction does not rest
    /// on the wording alone. The same three the conversation's own call row
    /// uses.
    static func directionGlyph(_ entry: CallHistoryEntry) -> LkIcon {
        switch entry.outcome {
        case .answered:
            return entry.direction == .outgoing ? .phoneOutgoing : .phoneIncoming
        case .missed, .declined, .failed:
            return .phoneDown
        }
    }

    static func timeOfDay(ms: Int64, calendar: Calendar, locale: Locale) -> String {
        let date = Date(timeIntervalSince1970: Double(ms) / 1000)
        let style = Date.FormatStyle(locale: locale, calendar: calendar, timeZone: calendar.timeZone)
            .hour().minute()
        return date.formatted(style)
    }
}

/// A call that has just finished, as the call hosts report it.
///
/// One value rather than a widening argument list, and the reason is the
/// spec's (section 6.2): the index holds the PARTS, and the hosts are the only
/// place that knows them. Whether the camera was on and when the call started
/// cannot be read back once the session is gone, so they travel out with the
/// row or they are lost.
public struct FinishedCall: Equatable, Sendable {
    /// The conversation the call belongs to.
    public let groupIdHex: String
    /// The row the conversation shows.
    public let row: CallTranscriptRow
    /// The call was placed or accepted with the camera on, so calling back
    /// re-dials in the same mode (`docs/ux/navigation.md` section 5.1).
    public let video: Bool
    /// When the call started — which is when it began ringing or dialling, not
    /// when media connected. A missed call has no connect time and still
    /// belongs on the list at the moment it happened.
    public let startedAtMs: Int64
    /// The call's 16-byte id, which numbers the conversation's row
    /// (``CallTranscriptRow/logMessageId(callId:)``); `nil` when unknown.
    public let callId: Data?

    public init(
        groupIdHex: String, row: CallTranscriptRow, video: Bool, startedAtMs: Int64, callId: Data? = nil
    ) {
        self.groupIdHex = groupIdHex
        self.row = row
        self.video = video
        self.startedAtMs = startedAtMs
        self.callId = callId
    }
}
