// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Where a person's sound level was read from.
enum VoiceLevelPath: String, Equatable {
    /// The `audioLevel` of the peer connection's statistics: the person's
    /// `inbound-rtp` audio stream, or this device's `media-source`.
    case statsLevel = "stats-level"
    /// The report carried no level for them, or their microphone is off.
    case none
}

/// One person over one interval.
struct VoiceReport: Equatable {
    let id: String
    let isLocal: Bool
    /// Where the last level of the interval came from.
    let path: VoiceLevelPath
    /// The loudest level of the interval, `0...1`; `nil` when none was read.
    let peak: Double?
    /// Whether the person counted as speaking at any reading of the interval.
    let speaking: Bool
    /// The level that counts as sound for this person at the end of the
    /// interval (``NoiseGate``).
    let threshold: Double
}

/// Why a person is on the large tile; the order of the shared rule.
enum StageReason: String, Equatable {
    case pinned
    case speaking
    case recent
    case picture
    case first
}

/// The `voices` line of the call log: for each person where their sound level
/// was read from, how loud they were and whether that counted as speaking;
/// who holds the floor; who is on the large tile and why. The twin of
/// `apps/web/src/calls/voice-diagnostics.ts`.
///
/// A person who speaks and never reaches the large tile is either not
/// measured (`none`), measured below the threshold (a small peak, `no`), or
/// measured and outranked (a pin, or a floor someone else holds).
///
/// One person reads, for example, `a=stats-level:0.031:yes[thr=0.012]`: the
/// path the ring was decided from, the loudest level on it over the
/// interval, whether the ring was on, then the threshold in force. The web
/// line's bracket also carries the raw value of every path it tried; this
/// client has the one path, so the bracket holds the threshold alone.
///
/// Diagnostics only, on this device only; it decides nothing. Pure.
enum VoiceDiagnostics {

    /// A level in the line: three decimals, since speech on a quiet
    /// microphone sits at a few hundredths; nothing read is a dash.
    static func amount(_ level: Double?) -> String {
        level.map { String(format: "%.3f", $0) } ?? "-"
    }

    /// The short name of a person in the line: the SFU peer id, `me` for
    /// this device. Never a display name.
    static func label(_ id: String, isLocal: Bool) -> String {
        if isLocal { return "me" }
        return id.hasPrefix("peer-") ? String(id.dropFirst(5)) : id
    }

    /// Why `stageId` is on the large tile, given what the shared rule was
    /// asked with. Mirrors the rule's order; it explains an answer, it does
    /// not make one.
    static func stageReason<P: GroupLayoutPerson>(
        stageId: String, people: [P], pinnedId: String?, speech: StageSpeech
    ) -> StageReason {
        if pinnedId == stageId { return .pinned }
        if speech.activeSpeakerId == stageId { return .speaking }
        if (speech.lastSpokeAtMs[stageId] ?? 0) > 0 { return .recent }
        if people.contains(where: { $0.id == stageId && $0.camOn }) { return .picture }
        return .first
    }

    /// The line. `stage` is `nil` when no large tile is drawn.
    static func describe(
        _ prefix: String,
        voices: [VoiceReport],
        floorId: String?,
        stage: (id: String, reason: StageReason)?,
        layout: GroupLayoutKind
    ) -> String {
        let labelOf: (String) -> String = { id in
            label(id, isLocal: voices.first(where: { $0.id == id })?.isLocal == true)
        }
        let people = voices.map { voice -> String in
            return "\(label(voice.id, isLocal: voice.isLocal))=\(voice.path.rawValue)"
                + ":\(amount(voice.peak)):\(voice.speaking ? "yes" : "no")"
                + "[thr=\(amount(voice.threshold))]"
        }
        let parts = ["\(prefix) voices"] + people + [
            "floor=\(floorId.map(labelOf) ?? "-")",
            "stage=\(stage.map { "\(labelOf($0.id))(\($0.reason.rawValue))" } ?? "-")",
            "layout=\(layout.rawValue)",
        ]
        return parts.joined(separator: " ")
    }
}
