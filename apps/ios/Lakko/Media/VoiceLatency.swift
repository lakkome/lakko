// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0047 §2 — the rules that decide when a voice message's bytes are
/// fetched, when a recorded sample counts, and when a touch on the waveform
/// is a seek.
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
