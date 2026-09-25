// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0047 §2 — the rules that decide when a voice message's bytes are
/// fetched, when a recorded sample counts, when the microphone is giving
/// nothing, and when a touch on the waveform is a seek.
///
/// They live here, as pure functions over values, because each one was a
/// condition buried in a view or a recorder callback where nothing could
/// run it. The leading silence a listener hears is made of two of them.

/// When the recorder is allowed to write.
public enum VoiceCapture {

    /// How long the audio input is given to settle before the first sample is
    /// written to the file.
    ///
    /// Recording opens with a category change to `.playAndRecord` and a
    /// session activation, which reconfigure the audio route; the input does
    /// not deliver usable frames the instant that call returns. Starting the
    /// encoder there writes the settle window INTO the file, and since the
    /// take's duration is the recorder's own clock, it is counted as well —
    /// so every message opens on silence the sender never spoke.
    ///
    /// The recorder therefore schedules its start this far ahead on the audio
    /// device clock, which leaves the window outside the file entirely rather
    /// than trimming it afterwards.
    ///
    /// The value is CHOSEN, not measured: no device was available to time the
    /// route change on. It is small enough that a speaker who begins the
    /// instant they press loses nothing audible, and the floor it must clear
    /// is only that it be shorter than ``RecordingMachine/minTakeMs``. Tune it
    /// against a real device before trusting the number.
    public static let inputSettleSeconds: TimeInterval = 0.12

    /// Whether a metering tick belongs to the take.
    ///
    /// The level timer runs from the moment the recorder is armed, which is
    /// before the scheduled start; a tick taken then would prepend bars to the
    /// waveform that no sample in the file corresponds to. The recorder's own
    /// clock stays at zero until it actually captures, so it is the gate.
    public static func recordsLevel(recordedSeconds: TimeInterval) -> Bool {
        recordedSeconds > 0
    }
}

/// UX spec §2 "Hearing nothing" — when the measured input level has stayed at
/// zero for ``warnAfterMs``, the recording bar says `No sound from the
/// microphone` in place of its hint, and clears the moment any level is
/// measured.
///
/// The rule lives here, as a fold over samples with no clock and no recorder in
/// it, for the reason the spec gives: silence was invisible from inside the app.
/// A chain of voice messages recorded on an emulator whose microphone toggle was
/// off arrived as digital silence and nothing anywhere said so; the cause was
/// found by measuring the file with an external tool. Every client already
/// samples the level to draw the live meter, so the signal the warning needs was
/// one each of them was holding and none of them read. Put the same condition
/// inline in the bar and it is a condition nothing can run.
///
/// What it is NOT: a loudness threshold. The take is never blocked and never
/// discarded on this account — a quiet room is not an error, and refusing to
/// send would be the client overruling the person about their own message — so
/// the warning is only ever a warning, and a person who speaks softly must never
/// see it. ``levelFloor`` is what keeps that true.
public enum MicrophoneSilence {

    /// The one wording, so the bar and the accessibility label cannot drift.
    public static let warningCopy = "No sound from the microphone"

    /// §2 — how long the level must stay at zero before the bar says so.
    ///
    /// The spec's number, shared by all three clients so the same room produces
    /// the same warning on each: below about a second a microphone that is merely
    /// slow to warm up would be accused of being dead, and past three the person
    /// has already committed to the take.
    public static let warnAfterMs: Int64 = 2000

    /// The level at or below which the meter reported NOTHING, rather than
    /// something quiet.
    ///
    /// "Zero" is the spec's word, and it is not exact on this platform:
    /// `AVAudioRecorder.averagePower` reports `-160` dBFS for digital silence,
    /// which ``Waveform/linearLevel(decibels:)`` turns into `1e-8` rather than
    /// `0`, so an exact-zero test would never fire here at all. Android's
    /// integer `getMaxAmplitude` does return exactly `0`. The three clients
    /// therefore share one floor over the one 0...1 linear scale instead of each
    /// asking its own meter its own question.
    ///
    /// `1e-6` is -120 dBFS, and it sits in a gap two orders of magnitude wide on
    /// either side:
    ///
    ///   * a hundred times ABOVE the `1e-8` this platform's meter reports for
    ///     silence, so a dead microphone lands under it;
    ///   * thirty times BELOW one least-significant bit of 16-bit PCM
    ///     (`1 / 32767` = `3.05e-5`, -90.3 dBFS), the quietest thing either
    ///     platform's recorder can represent as non-silence.
    ///
    /// So nothing that can be heard, and nothing a 16-bit converter can encode
    /// as anything but silence, can sit beneath it: the floor separates "the
    /// meter reported its own silence" from "one bit of real signal", never a
    /// soft voice from a loud one.
    ///
    /// CHOSEN from those two ranges, not measured — no device was available to
    /// read a real microphone's noise floor on. If a real device turns out to
    /// report something above this from a dead input, this is the number to
    /// revisit, not the two seconds.
    public static let levelFloor: Double = 1e-6

    /// Did this sample hear anything at all? A non-finite reading counts as
    /// silence rather than as sound: `nan > floor` is already `false`, and a
    /// meter reporting nonsense has not heard anything either.
    public static func hasSound(level: Double) -> Bool { level > levelFloor }

    /// How long the current unbroken run of silence has been going, and whether
    /// it has earned the warning yet. One per take, folded forward by
    /// ``MicrophoneSilence/observe(_:level:atMs:)``.
    public struct Watch: Equatable {
        /// When the current run of no-sound samples began, in the take's own
        /// elapsed milliseconds; `nil` before the first sample and after
        /// anything was heard.
        public var silentSinceMs: Int64?
        /// Whether the bar should be saying ``warningCopy`` right now. Read
        /// straight off the fold rather than recomputed, so exactly one place
        /// decides it.
        public var warns: Bool

        public init(silentSinceMs: Int64? = nil, warns: Bool = false) {
            self.silentSinceMs = silentSinceMs
            self.warns = warns
        }
    }

    /// Fold one level sample, taken at `atMs` of the take's elapsed time, into
    /// `watch`.
    ///
    /// Anything heard resets the run outright — §2's "clears the moment any
    /// level is measured" — so the warning describes the present rather than
    /// passing a verdict on the take: a microphone unmuted mid-recording, or a
    /// person who simply had not started speaking, must not carry an accusation
    /// through the rest of it.
    ///
    /// A run is timed from the first silent sample, so a take that opens with
    /// sound and goes quiet later is measured from when it went quiet. `atMs`
    /// runs off the recorder's own monotonic clock; a value below where the run
    /// began reads as no time passed, which is the safe way round — it delays
    /// the warning rather than raising a false one.
    public static func observe(_ watch: Watch, level: Double, atMs: Int64) -> Watch {
        guard !hasSound(level: level) else { return Watch() }
        let since = watch.silentSinceMs ?? atMs
        return Watch(silentSinceMs: since, warns: atMs - since >= warnAfterMs)
    }
}

/// When a voice bubble warms its bytes.
public enum VoicePrefetch {

    /// Whether a bubble that just came into view should fetch and decrypt its
    /// audio now.
    ///
    /// Every other media bubble already does this on appear — the photo, the
    /// GIF, the video note's poster. The voice bubble was the one that fetched
    /// nothing until it was pressed, so the first press paid a storage round
    /// trip and an AEAD open before a single sample could be decoded, and the
    /// message appeared to begin with a second of nothing.
    ///
    /// An outgoing take still in the send queue is skipped: its bytes are
    /// already on disk under the local key, and there is no blob to fetch.
    public static func shouldWarm(pending: Bool) -> Bool { !pending }
}

/// When a touch on the waveform is a seek.
public enum VoiceSeekGate {

    /// UIKit's default long-press threshold, which is what opens the message's
    /// context menu.
    public static let longPressSeconds: TimeInterval = 0.5

    /// Whether a touch that has just lifted off the waveform should move the
    /// playhead.
    ///
    /// The waveform seeks on touch-up, and the message's context menu opens on
    /// a long press over the same pixels. A press held long enough to open the
    /// menu is not a seek — the person was reaching for React or Delete — so
    /// its lift must not also jump the playhead. Whether the system cancels
    /// the drag when it opens the menu is its business; this refuses either
    /// way, so the outcome does not rest on that arbitration.
    public static func acceptsSeek(pressSeconds: TimeInterval) -> Bool {
        pressSeconds < longPressSeconds
    }
}
