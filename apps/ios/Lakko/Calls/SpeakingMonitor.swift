// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The sound levels of one reading, as the peer connection's own statistics
/// report them (`docs/ux/group-call-grid.md` section 7.3).
///
/// Every value is a level the WebRTC engine measured on THIS device: the
/// remote ones from audio already received, decrypted and decoded, the local
/// one from the microphone. Nothing here was sent by the call server, and
/// nothing here is sent anywhere.
struct CallAudioLevels: Equatable {
    /// This device's own microphone (`media-source`, audio), or `nil` when
    /// the report carried none.
    var local: Double?
    /// The level of each other person's audio, keyed by SFU peer id (the
    /// stream id the SFU pins on what it forwards, and the `"peer-<id>"` of
    /// the roster). A person the report said nothing about is absent.
    var remoteByPeerId: [String: Double]

    static let none = CallAudioLevels(local: nil, remoteByPeerId: [:])
}

/// One entry of a statistics report, as ``CallAudioLevelStats`` reads it.
/// `RTCStatistics` in production; a plain value in a test.
struct CallStatEntry {
    let type: String
    let values: [String: NSObject]
}

/// Reads sound levels out of a WebRTC statistics report.
///
/// Pure, so the field names it depends on are pinned by a test rather than
/// only by a call on a device.
enum CallAudioLevelStats {

    /// The levels of one report.
    ///
    /// - `inbound-rtp` entries of kind `audio` carry `audioLevel` (`0...1`)
    ///   and `trackIdentifier`, the id of the receiving track; the caller's
    ///   `peerIdByTrackId` says whose audio that track carries.
    /// - the `media-source` entry of kind `audio` carries the local
    ///   microphone's `audioLevel`.
    ///
    /// An entry without a level, or an inbound stream whose track is not in
    /// the map, contributes nothing: an unknown level is never a zero.
    static func levels(
        from entries: [CallStatEntry], peerIdByTrackId: [String: String]
    ) -> CallAudioLevels {
        var out = CallAudioLevels.none
        for entry in entries {
            // `kind` is the current spelling; `mediaType` is the older one and
            // some builds still emit only that.
            let kind = (entry.values["kind"] as? String) ?? (entry.values["mediaType"] as? String)
            guard kind == "audio",
                  let level = (entry.values["audioLevel"] as? NSNumber)?.doubleValue,
                  level.isFinite
            else { continue }
            switch entry.type {
            case "media-source":
                out.local = max(out.local ?? 0, level)
            case "inbound-rtp":
                guard let trackId = entry.values["trackIdentifier"] as? String,
                      let peerId = peerIdByTrackId[trackId]
                else { continue }
                out.remoteByPeerId[peerId] = max(out.remoteByPeerId[peerId] ?? 0, level)
            default:
                continue
            }
        }
        return out
    }
}

/// Reads the sound level of each person's audio in a group call and feeds
/// ``SpeakingTracker``; the twin of `apps/web/src/calls/speaking-monitor.ts`.
///
/// The levels come from the peer connection's statistics, read about seven
/// times a second: each other person's from the `audioLevel` of the
/// `inbound-rtp` stream of the receiver that decodes their audio, this
/// device's own from its audio `media-source`. The web client measures with
/// an analyser on the played (or captured) stream first and with the energy
/// counters of the receiver's statistics second; the app gets no decoded
/// samples from this binding, so the statistics are the one source here.
/// Like the web client, it never lets a level the sender wrote into the
/// packet header (a receiver's synchronization sources) decide anything.
///
/// A person the report carries no level for is sampled as "could not be
/// read": it counts as silence for the ring (none is drawn, and the large
/// tile falls back to the roster order) and teaches that person's noise
/// floor nothing. A muted person's level is not read at all.
///
/// What counts as sound follows each person's own noise floor
/// (``NoiseGate``), with the web client's constants. The level itself is not
/// the web client's: the statistics' `audioLevel` is whatever window the
/// engine keeps (the web client notes libwebrtc reports a decaying peak, not
/// a root mean square, and does not let it decide there). Here it is the
/// only source, so the threshold follows the floor of that same instrument.
/// The floor's fall is bounded per reading, and a reading here is every
/// 150 ms against the web client's 100 ms, so the floor follows a room that
/// went quiet in half as many more milliseconds; the rise is per second and
/// is the same.
///
/// Group calls only: a one-to-one has one voice.
@MainActor
final class GroupSpeakingMonitor {

    /// How often the levels are read.
    static let sampleIntervalMs: UInt64 = 150

    private let ui: CallUiState
    private let nowMs: () -> UInt64
    private var tracker = SpeakingTracker()
    private var task: Task<Void, Never>?
    /// What each person's level was since the last `voices` line.
    private var report: [String: VoiceNote] = [:]
    private var readingsSinceReport = 0

    /// How many readings one `voices` line covers (about two seconds).
    static let reportEveryReadings = 13

    private struct VoiceNote {
        var measured = false
        var peak: Double?
        var speaking = false
    }

    /// Where a reading comes from. Set once the call's session exists; `nil`
    /// yields no readings.
    var source: (@MainActor () async -> CallAudioLevels?)?

    init(
        ui: CallUiState,
        nowMs: @escaping () -> UInt64 = { UInt64(max(0, Date().timeIntervalSince1970 * 1000)) }
    ) {
        self.ui = ui
        self.nowMs = nowMs
    }

    /// Start reading. Idempotent.
    func start() {
        guard task == nil else { return }
        task = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: Self.sampleIntervalMs * 1_000_000)
                guard !Task.isCancelled, let self else { return }
                guard let levels = await self.source?() else { continue }
                guard !Task.isCancelled else { return }
                self.ingest(levels, nowMs: self.nowMs())
            }
        }
    }

    /// Stop reading and clear every ring. Idempotent.
    func stop() {
        task?.cancel()
        task = nil
        tracker = SpeakingTracker()
        report = [:]
        readingsSinceReport = 0
        ui.setSpeech(speakingIds: [], speech: .none)
    }

    /// Fold one reading into the call's state: the ring on every speaking
    /// tile, and who holds the floor.
    ///
    /// A muted person is never speaking: their level is not read at all
    /// while their microphone is off, this device's own included (a muted
    /// microphone still measures the room). A person the report carried no
    /// level for is sampled with no level, as on the web client: silence for
    /// the ring, so one whose source went away goes off instead of standing,
    /// and nothing for the noise floor.
    ///
    /// This device's own person is sampled for its ring and is never a
    /// candidate for the floor (``SpeakingTracker/snapshot(candidates:nowMs:)``).
    func ingest(_ levels: CallAudioLevels, nowMs: UInt64) {
        let people = ui.participants
        let order = people.map(\.id)
        tracker.keepOnly(Set(order))
        for person in people {
            guard person.micOn else {
                tracker.silence(person.id)
                report[person.id] = nil
                continue
            }
            let level: Double?
            if person.isLocal {
                level = levels.local
            } else {
                level = levels.remoteByPeerId[Self.peerId(ofParticipant: person.id)]
            }
            var note = report[person.id] ?? VoiceNote()
            note.measured = level != nil
            if let level { note.peak = max(note.peak ?? 0, level) }
            report[person.id] = note
            tracker.sample(person.id, level: level, nowMs: nowMs)
        }
        let candidates = people.filter { !$0.isLocal }.map(\.id)
        let snapshot = tracker.snapshot(candidates: candidates, nowMs: nowMs)
        for id in snapshot.speakingIds { report[id]?.speaking = true }
        ui.setSpeech(
            speakingIds: snapshot.speakingIds,
            speech: StageSpeech(
                activeSpeakerId: snapshot.activeSpeakerId,
                lastSpokeAtMs: snapshot.lastSpokeAtMs))
        readingsSinceReport += 1
        if readingsSinceReport >= Self.reportEveryReadings {
            readingsSinceReport = 0
            logVoices(people: people, floorId: snapshot.activeSpeakerId)
        }
    }

    /// The `voices` line of the call log, for the readings since the last
    /// one (``VoiceDiagnostics``).
    private func logVoices(people: [CallUiState.Participant], floorId: String?) {
        let voices = people.map { person -> VoiceReport in
            let note = report[person.id]
            return VoiceReport(
                id: person.id, isLocal: person.isLocal,
                path: note?.measured == true ? .statsLevel : .none,
                peak: note?.peak, speaking: note?.speaking ?? false,
                threshold: tracker.threshold(of: person.id))
        }
        report = [:]
        let layout = GroupCallLayout.kind(
            choice: ui.layoutChoice, pinnedId: ui.pinnedId, people: people)
        var stage: (id: String, reason: StageReason)?
        if layout == .focus,
            let person = GroupCallLayout.stagePerson(
                SharedGroupCallRules(), people: people, pinnedId: ui.pinnedId, speech: ui.speech) {
            stage = (
                id: person.id,
                reason: VoiceDiagnostics.stageReason(
                    stageId: person.id, people: people, pinnedId: ui.pinnedId, speech: ui.speech))
        }
        NSLog(
            "%@",
            VoiceDiagnostics.describe(
                "call media interval", voices: voices, floorId: floorId, stage: stage,
                layout: layout))
    }

    /// The level that counts as sound for a person of the roster now.
    func threshold(ofParticipant id: String) -> Double {
        tracker.threshold(of: id)
    }

    /// The SFU peer id behind a roster id (`"peer-<peerId>"`).
    static func peerId(ofParticipant id: String) -> String {
        id.hasPrefix(participantPrefix) ? String(id.dropFirst(participantPrefix.count)) : id
    }

    private static let participantPrefix = "peer-"
}
