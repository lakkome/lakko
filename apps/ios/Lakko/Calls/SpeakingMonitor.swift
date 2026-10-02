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
/// times a second. A report that carries no level for someone simply yields no
/// sample for them: no ring is drawn and the large tile falls back to the
/// roster order, which is the call as it was before this existed. A level is
/// never guessed.
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
        ui.setSpeech(speakingIds: [], speech: .none)
    }

    /// Fold one reading into the call's state: the ring on every speaking
    /// tile, and who holds the floor.
    func ingest(_ levels: CallAudioLevels, nowMs: UInt64) {
        let people = ui.participants
        let order = people.map(\.id)
        tracker.keepOnly(Set(order))
        for person in people {
            let level: Double?
            if person.isLocal {
                level = levels.local
            } else {
                level = levels.remoteByPeerId[Self.peerId(ofParticipant: person.id)]
            }
            guard let level else { continue }
            tracker.sample(person.id, level: level, nowMs: nowMs)
        }
        let snapshot = tracker.snapshot(order: order, nowMs: nowMs)
        ui.setSpeech(
            speakingIds: snapshot.speakingIds,
            speech: StageSpeech(
                activeSpeakerId: snapshot.activeSpeakerId,
                lastSpokeAtMs: snapshot.lastSpokeAtMs))
    }

    /// The SFU peer id behind a roster id (`"peer-<peerId>"`).
    static func peerId(ofParticipant id: String) -> String {
        id.hasPrefix(participantPrefix) ? String(id.dropFirst(participantPrefix.count)) : id
    }

    private static let participantPrefix = "peer-"
}
