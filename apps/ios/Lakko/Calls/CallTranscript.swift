// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// What a finished call leaves behind in the conversation
/// (`docs/ux/calls.md` section 6).
///
/// ## Why this exists
///
/// A call used to leave no trace at all: it ended, the screen closed, and the
/// conversation looked exactly as it had before anyone picked up. Section 6
/// says every finished call writes a row — `Outgoing call, 12:04`,
/// `Incoming call, 12:04`, `Missed call`, `Declined` — and that tapping it
/// calls back.
///
/// ## Where the row lives
///
/// On the device that was in the call, and nowhere else. A call row is **not**
/// a wire format and never reaches the journal: both devices were in the same
/// call and each writes its own row from what it observed, so there is nothing
/// to agree on and nothing for a server to carry (the two rows differ by
/// design — one is outgoing, the other incoming).
///
/// It is stored as an ordinary system row in the chat timeline, which means it
/// is persisted by the encrypted message log along with every other message
/// and survives a relaunch by exactly the same route. The row's `text` is the
/// ``marker`` — a compact, parseable sentinel in the shape the timeline
/// already uses for its bookkeeping rows (`[mls commit]`) — so the sentence a
/// reader sees is rendered from the parsed outcome rather than frozen into the
/// store. A row written by an older build, or one whose marker no longer
/// parses, is not drawn at all rather than shown as raw text.
public struct CallTranscriptRow: Equatable, Sendable {

    /// Which side placed the call, from the point of view of the device
    /// writing the row.
    /// `Codable` so the flat Calls index (``CallHistoryEntry``) stores the
    /// direction as one of the two words this type already spells, rather
    /// than inventing a second vocabulary for the same fact.
    public enum Direction: String, Codable, Equatable, Sendable {
        case outgoing
        case incoming
    }

    /// How the call finished. One outcome per sentence section 6 lists.
    public enum Outcome: Equatable, Sendable {
        /// Media flowed. Carries how long for.
        case answered(durationMs: Int64)
        /// Nobody picked up, or it was cancelled before anyone could.
        case missed
        /// A person, or their device, refused it.
        case declined
        /// It was answered and no media ever flowed.
        case failed
    }

    /// Why the call ended, as the call session reported it.
    public enum Cause: String, Equatable, Sendable {
        case hungUp
        case busy
        case declined
        case timedOut
        case failed
    }

    public let direction: Direction
    public let outcome: Outcome
    /// Started as a video call: what the row's mark shows and calling back
    /// repeats.
    public let video: Bool

    public init(direction: Direction, outcome: Outcome, video: Bool = false) {
        self.direction = direction
        self.outcome = outcome
        self.video = video
    }

    // MARK: - Outcome

    /// The outcome of a call that has just ended.
    ///
    /// `connectedAtMs` is `nil` for a call that never carried media, which is
    /// the only thing that separates a call from an attempt. A call that DID
    /// connect is always `answered`, however it then ended — hanging up is how
    /// calls normally finish.
    ///
    /// Among calls that never connected, only a refusal is `declined`: a
    /// device answering **busy** refused it just as a person tapping Decline
    /// did. A call somebody answered that never carried media (`accepted`)
    /// is `failed`: nobody missed it. Everything else — the 45-second ring
    /// running out, the caller cancelling — is `missed`, which is the word
    /// section 2.1 uses for both sides of a call nobody answered.
    public static func outcome(
        connectedAtMs: Int64?,
        endedAtMs: Int64,
        cause: Cause?,
        accepted: Bool = false
    ) -> Outcome {
        if let connectedAtMs {
            return .answered(durationMs: max(0, endedAtMs - connectedAtMs))
        }
        switch cause {
        case .declined, .busy:
            return .declined
        case .hungUp, .timedOut, .failed, nil:
            return accepted ? .failed : .missed
        }
    }

    // MARK: - Copy

    /// The sentence the conversation shows, in the interface language
    /// (ADR-0061). The duration stays a clock value (`12:04`).
    public var text: String {
        switch outcome {
        case .answered(let durationMs):
            let key = direction == .outgoing ? "call.log.outgoing_duration" : "call.log.incoming_duration"
            return Localization.text(key, texts: ["duration": Self.durationLabel(ms: durationMs)])
        case .missed:
            return Localization.text("call.log.missed")
        case .declined:
            return Localization.text("call.log.declined")
        case .failed:
            return Localization.text("call.log.failed")
        }
    }

    /// The spoken form (`docs/ux/calls.md` section 8): a duration is announced
    /// as a duration, never read digit by digit, and the row says what a tap
    /// on it does.
    public var accessibilityLabel: String {
        let mode = video ? "video" : "voice"
        let head: String
        switch outcome {
        case .answered(let durationMs):
            let kind = Localization.text(
                "call.log.a11y.\(mode)_\(direction == .outgoing ? "outgoing" : "incoming")")
            head = Localization.text(
                "call.log.a11y.lasted",
                texts: ["head": kind, "duration": Self.spokenDuration(ms: durationMs)])
        case .missed:
            head = Localization.text("call.log.a11y.\(mode)_missed")
        case .declined:
            head = Localization.text("call.log.a11y.\(mode)_declined")
        case .failed:
            head = Localization.text("call.log.failed")
        }
        return Localization.text("call.log.a11y.call_back", texts: ["head": head])
    }

    /// `12:04`; hours are added only when there are any.
    public static func durationLabel(ms: Int64) -> String {
        let total = max(0, ms) / 1000
        let seconds = total % 60
        let minutes = (total / 60) % 60
        let hours = total / 3600
        if hours > 0 {
            return String(format: "%d:%02d:%02d", hours, minutes, seconds)
        }
        return String(format: "%d:%02d", minutes, seconds)
    }

    /// `12 minutes 4 seconds` — what VoiceOver reads instead of "one two colon
    /// zero four", in the interface language.
    public static func spokenDuration(ms: Int64) -> String {
        durationWords(seconds: max(0, ms) / 1000, alwaysSeconds: false)
    }

    /// A number of seconds said in words (the web `durationWords`). Each unit
    /// is its own plural message (`common.duration.hours` / `minutes` /
    /// `seconds`) and the units present are joined pair by pair through
    /// `common.duration.pair`, so no unit word and no word order is written
    /// here. Hours and minutes appear only when they are not zero; seconds
    /// always with `alwaysSeconds`, else only when not zero or when nothing
    /// else would be said.
    public static func durationWords(seconds totalSeconds: Int64, alwaysSeconds: Bool) -> String {
        let total = max(0, totalSeconds)
        let seconds = total % 60
        let minutes = (total / 60) % 60
        let hours = total / 3600
        var parts: [String] = []
        if hours > 0 {
            parts.append(Localization.text("common.duration.hours", counts: ["count": UInt64(hours)]))
        }
        if minutes > 0 {
            parts.append(Localization.text("common.duration.minutes", counts: ["count": UInt64(minutes)]))
        }
        if alwaysSeconds || seconds > 0 || parts.isEmpty {
            parts.append(Localization.text("common.duration.seconds", counts: ["count": UInt64(seconds)]))
        }
        return parts.reduce("") { acc, part in
            acc.isEmpty ? part : Localization.text("common.duration.pair", texts: ["first": acc, "second": part])
        }
    }

    // MARK: - Storage

    /// The prefix every call row's stored text starts with. Nothing else in
    /// the timeline may begin with it.
    public static let markerPrefix = "[call]"

    /// The stored form. Machine-readable on purpose: the sentence is rendered
    /// from the parsed value, so re-wording section 6 never has to migrate
    /// rows already on disk.
    public var marker: String {
        let head = "\(Self.markerPrefix) \(direction.rawValue)"
        let body: String
        switch outcome {
        case .answered(let durationMs): body = "\(head) answered \(max(0, durationMs))"
        case .missed: body = "\(head) missed"
        case .declined: body = "\(head) declined"
        case .failed: body = "\(head) failed"
        }
        return video ? "\(body) \(Self.videoField)" : body
    }

    /// The last field of a video call's marker. A row written before it has
    /// none and reads as a voice call.
    private static let videoField = "video"

    /// `true` when `text` is a stored call row — parseable or not. A row that
    /// claims to be one and is not must be hidden rather than drawn raw.
    public static func isCallRow(_ text: String) -> Bool {
        text.hasPrefix(markerPrefix)
    }

    /// Read a stored marker back. `nil` for anything this build does not
    /// understand, which the timeline then does not draw.
    public static func parse(_ text: String) -> CallTranscriptRow? {
        var fields = text.split(separator: " ", omittingEmptySubsequences: true).map(String.init)
        let video = fields.last == videoField
        if video { fields.removeLast() }
        guard fields.count >= 3, fields[0] == markerPrefix,
              let direction = Direction(rawValue: fields[1])
        else { return nil }
        switch fields[2] {
        case "answered":
            guard fields.count == 4, let ms = Int64(fields[3]), ms >= 0 else { return nil }
            return CallTranscriptRow(direction: direction, outcome: .answered(durationMs: ms), video: video)
        case "missed":
            guard fields.count == 3 else { return nil }
            return CallTranscriptRow(direction: direction, outcome: .missed, video: video)
        case "declined":
            guard fields.count == 3 else { return nil }
            return CallTranscriptRow(direction: direction, outcome: .declined, video: video)
        case "failed":
            guard fields.count == 3 else { return nil }
            return CallTranscriptRow(direction: direction, outcome: .failed, video: video)
        default:
            return nil
        }
    }
}
