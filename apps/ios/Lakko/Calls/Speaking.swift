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
///    silence. It may come on for a cough. "Sound" is a level well above the
///    person's own noise floor (``NoiseGate``), not above one fixed number.
///  - **the floor**: the one person the large tile follows. A person takes it
///    after 1.5 s of continuous speech, and keeps it while they keep
///    speaking, so a cough never moves a tile and two people talking over
///    each other do not make the stage flip between them.
///
/// Every number is chosen, not measured, and is the same number as in
/// `apps/web/src/calls/speaking.ts`.
///
/// Pure and clock-free: the caller supplies the level and the time, so the
/// whole rule runs in a test.
enum Speaking {
    /// The threshold a person starts with, before anything is known about
    /// the noise around their microphone. `0...1`.
    static let level: Double = 0.02
    /// Sound counts as speech when it is this many times the person's noise
    /// floor (about 9.5 dB above it).
    ///
    /// Why the threshold follows the person instead of being one number: on
    /// a real call (2026-10-11) a phone's microphone delivered speech at
    /// 0.01 to 0.05, while a laptop in a room idled at 0.01 to 0.02. No
    /// single number separates the two; each person's own floor does.
    static let speechOverNoise: Double = 3
    /// The threshold never goes below this, however quiet the floor.
    static let levelMin: Double = 0.005
    /// Nor above this, however loud the room: a floor learned wrong must not
    /// shut a person out for good.
    static let levelMax: Double = 0.05
    /// The floor climbs towards steady sound by this factor a second
    /// (doubling in about seven seconds), so a fan switched on stops
    /// counting as speech and a sentence does not.
    static let noiseRisePerSecond: Double = 1.1
    /// The floor drops to at most this fraction of itself per reading of
    /// the web client, so one torn reading does not empty it. The web
    /// client reads every ``noiseFallSampleMs``; this client reads at
    /// another interval, so the rule is applied per unit of time, as the
    /// Android client does: over `t` milliseconds the floor may fall to
    /// `noiseFallPerSample ^ (t / noiseFallSampleMs)` of itself. After any
    /// stretch of time the three clients' floors have been allowed to fall
    /// by the same factor.
    static let noiseFallPerSample: Double = 0.5
    /// The reading interval ``noiseFallPerSample`` is stated for.
    static let noiseFallSampleMs: Double = 100
    /// A reading below this says nothing about the room: it is a muted
    /// sender or a stream that is not flowing (digital silence), not a quiet
    /// room.
    static let noiseEvidenceMin: Double = 1e-4
    /// Sound for this long turns the ring on.
    static let ringOnMs: UInt64 = 200
    /// Silence for this long turns it off.
    static let ringOffMs: UInt64 = 800
    /// Continuous speech for this long takes the floor.
    static let floorOnsetMs: UInt64 = 1_500
}

/// One person's noise floor and the threshold that follows from it; the twin
/// of `NoiseGate` in `apps/web/src/calls/speaking.ts`.
///
/// The floor falls quickly to the quietest recent reading and rises slowly
/// towards whatever is steadily there. Pure: the caller supplies the time.
struct NoiseGate: Equatable {
    private var floor = Speaking.level / Speaking.speechOverNoise
    private var lastAtMs: UInt64?

    init() {}

    /// A level at or above this counts as sound, now.
    var threshold: Double {
        min(Speaking.levelMax, max(Speaking.levelMin, floor * Speaking.speechOverNoise))
    }

    /// Fold one reading in. Digital silence and non-numbers change nothing.
    mutating func update(_ level: Double, nowMs: UInt64) {
        // `nil` for the first reading: no time lies before it.
        let elapsed: UInt64?
        if let last = lastAtMs {
            elapsed = nowMs > last ? min(1_000, nowMs - last) : 0
        } else {
            elapsed = nil
        }
        lastAtMs = nowMs
        guard level.isFinite, level >= Speaking.noiseEvidenceMin else { return }
        if level < floor {
            // The first reading may fall as far as one reading of the web
            // client does.
            let fallMs = elapsed.map { Double($0) } ?? Speaking.noiseFallSampleMs
            let keep = pow(Speaking.noiseFallPerSample, fallMs / Speaking.noiseFallSampleMs)
            floor = max(level, floor * keep)
        } else {
            floor = min(
                level, floor * pow(Speaking.noiseRisePerSecond, Double(elapsed ?? 0) / 1_000))
        }
    }
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
        /// What counts as sound for this person.
        var gate = NoiseGate()
    }

    private var voices: [String: Voice] = [:]
    private var floor: String?
    private var lastSpoke: [String: UInt64] = [:]

    init() {}

    /// One level reading for one person. `nil` is "could not be read": it
    /// counts as silence for the ring and teaches the noise floor nothing.
    mutating func sample(_ id: String, level: Double?, nowMs: UInt64) {
        var voice = voices[id] ?? Voice()
        // Judged against the floor as it stood before this reading.
        let threshold = voice.gate.threshold
        if let level { voice.gate.update(level, nowMs: nowMs) }
        if let level, level.isFinite, level >= threshold {
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
        guard let voice = voices[id] else { return }
        // What was learned about the room is kept across a mute.
        var cleared = Voice()
        cleared.gate = voice.gate
        voices[id] = cleared
    }

    /// What counts as sound for `id` now (for the diagnostics line).
    func threshold(of id: String) -> Double {
        voices[id]?.gate.threshold ?? Speaking.level
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
