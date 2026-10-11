// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Who is speaking in a group call (`docs/ux/group-call-grid.md` sections 5.2
/// and 7.3).
///
/// Worked out on this device from audio it has already received, decrypted
/// and decoded, so the call server never learns it. Two outputs with two
/// different timings, because they answer two different questions:
///
///  - **the ring** on a tile: on after 200 ms of sound, off after 800 ms of
///    silence. It may come on for a cough.
///  - **the floor**: the one person the large tile follows. A person takes it
///    after 1.5 s of continuous speech, and keeps it while they keep
///    speaking, so a cough never moves a tile and two people talking over
///    each other do not make the stage flip between them.
///
/// Every number is chosen, not measured, and is the same four numbers as
/// `apps/web/src/calls/speaking.ts`.
///
/// Pure and clock-free: the caller supplies the level and the time, so the
/// whole rule runs in a test.
enum Speaking {
    /// A level at or above this counts as sound. `0...1`.
    static let level: Double = 0.02
    /// Sound for this long turns the ring on.
    static let ringOnMs: UInt64 = 200
    /// Silence for this long turns it off.
    static let ringOffMs: UInt64 = 800
    /// Continuous speech for this long takes the floor.
    static let floorOnsetMs: UInt64 = 1_500
}

/// What the tracker reports after a sample.
struct SpeakingSnapshot: Equatable {
    /// Everyone whose ring is on.
    let speakingIds: Set<String>
    /// The one person holding the floor, or `nil`.
    let activeSpeakerId: String?
    /// When each person last held the floor.
    let lastSpokeAtMs: [String: UInt64]
}

/// Follows the people of one call.
struct SpeakingTracker {

    private struct Voice {
        /// Since when the level has been above the threshold, or `nil`.
        var soundSince: UInt64?
        /// Since when it has been below, or `nil`.
        var silenceSince: UInt64?
        /// The ring.
        var speaking = false
        /// Since when the ring has been on without a break, or `nil`.
        var speakingSince: UInt64?
        /// The last reading with sound while the ring was on, or `nil`.
        var lastSoundAt: UInt64?
    }

    private var voices: [String: Voice] = [:]
    private var floor: String?
    private var lastSpoke: [String: UInt64] = [:]

    init() {}

    /// One level reading for one person.
    mutating func sample(_ id: String, level: Double, nowMs: UInt64) {
        var voice = voices[id] ?? Voice()
        if level.isFinite, level >= Speaking.level {
            voice.silenceSince = nil
            let since = voice.soundSince ?? nowMs
            voice.soundSince = since
            if !voice.speaking, nowMs >= since, nowMs - since >= Speaking.ringOnMs {
                voice.speaking = true
                voice.speakingSince = since
            }
            if voice.speaking { voice.lastSoundAt = nowMs }
        } else {
            voice.soundSince = nil
            let since = voice.silenceSince ?? nowMs
            voice.silenceSince = since
            if voice.speaking, nowMs >= since, nowMs - since >= Speaking.ringOffMs {
                voice.speaking = false
                voice.speakingSince = nil
                voice.lastSoundAt = nil
            }
        }
        voices[id] = voice
    }

    /// The person is muted: their ring goes off at once and they give up the
    /// floor at the next snapshot. A muted person is never speaking, whatever
    /// a level reading says.
    mutating func silence(_ id: String) {
        guard voices[id] != nil else { return }
        voices[id] = Voice()
    }

    /// Forget people who are no longer in the call.
    mutating func keepOnly(_ ids: Set<String>) {
        voices = voices.filter { ids.contains($0.key) }
        if let floor, !ids.contains(floor) { self.floor = nil }
        lastSpoke = lastSpoke.filter { ids.contains($0.key) }
    }

    /// The state after the samples so far.
    ///
    /// `candidates` are the people who can hold the floor, in roster order,
    /// which breaks the tie when two reach the onset in the same reading.
    /// This device's own person is NOT one of them: the floor exists for the
    /// large tile, which never shows the own tile while anyone else is
    /// present, and a floor held by the own microphone (the person talking,
    /// or the room heard through it) kept everyone else from taking it, so
    /// the large tile never moved to the person speaking. The ring is for
    /// everyone sampled, the own tile included.
    mutating func snapshot(candidates: [String], nowMs: UInt64) -> SpeakingSnapshot {
        let speakingIds = Set(voices.filter { $0.value.speaking }.keys)
        let voices = self.voices
        // The ring is on: enough to keep a floor already held.
        let stillSpeaking: (String) -> Bool = { voices[$0]?.speaking == true }
        // Enough to take the floor: the speech itself has lasted the onset.
        // The time the ring stays on after the last sound does not count, or
        // a 0.8-second sound would grow into a 1.5-second one by standing
        // still.
        let earned: (String) -> Bool = { id in
            guard let voice = voices[id], voice.speaking, let since = voice.speakingSince,
                  let last = voice.lastSoundAt, last >= since
            else { return false }
            return last - since >= Speaking.floorOnsetMs
        }
        // Whoever holds the floor keeps it while they keep speaking;
        // otherwise it goes to the first candidate in roster order who has
        // earned it. When the holder stops and nobody has, the floor is empty
        // and `lastSpokeAtMs` still names them, which is what keeps the large
        // tile on them.
        if let holder = floor, candidates.contains(holder), stillSpeaking(holder) {
            // Kept.
        } else {
            floor = candidates.first(where: earned)
        }
        if let floor { lastSpoke[floor] = nowMs }
        return SpeakingSnapshot(
            speakingIds: speakingIds, activeSpeakerId: floor, lastSpokeAtMs: lastSpoke)
    }
}
