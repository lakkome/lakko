// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0047 §1 — the voice-message waveform: at most ``maxBytes`` amplitude
/// bytes carried in the envelope so a recipient can draw the bubble before the
/// audio is downloaded. Mirror of `apps/web/src/media/waveform.ts`.
///
/// The recorder samples a level (0...1) at a steady cadence while capturing;
/// ``downsample(_:buckets:)`` folds that series into the envelope's bytes.
/// Bucketing takes the PEAK of each bucket rather than the mean, so a short
/// loud syllable survives the fold instead of being averaged into the silence
/// around it.
public enum Waveform {

    /// The envelope bound — `lakko-attachments` drops a larger waveform.
    public static let maxBytes = 128

    /// Fold `levels` (non-negative amplitudes, any scale) into at most
    /// `buckets` bytes, normalised so the loudest bucket is 255. `nil` for an
    /// empty series — the envelope field is then simply absent. A silent take
    /// yields all zeros, which is honest: there was nothing to draw.
    public static func downsample(_ levels: [Double], buckets: Int = maxBytes) -> Data? {
        let n = levels.count
        guard n > 0 else { return nil }
        let count = max(1, min(buckets, maxBytes, n))
        var peaks = [Double](repeating: 0, count: count)
        for (i, raw) in levels.enumerated() {
            let v = raw.isFinite && raw > 0 ? raw : 0
            let b = min(count - 1, (i * count) / n)
            if v > peaks[b] { peaks[b] = v }
        }
        let loudest = peaks.max() ?? 0
        // An entirely silent take has no waveform, and saying so is not the
        // same as saying "every bar is zero". ``bars(_:count:)`` floors a zero
        // row at 0.08 and draws an ABSENT waveform at 0.15, so recording the
        // measurement would draw FLATTER than having no measurement at all,
        // which is backwards. At that height a bar is wider than it is tall,
        // which is what turned a waveform into a row of dots. Found on
        // Android, fixed in all three ports.
        guard loudest > 0 else { return nil }
        return Data(peaks.map { p in
            UInt8(max(0, min(255, ((p / loudest) * 255).rounded())))
        })
    }

    /// Resample stored waveform bytes to `bars` display bars (0...1 each), so
    /// every bubble draws the same number of bars whatever the sender produced.
    /// An absent or empty waveform draws a flat, low placeholder so the seek
    /// slider still has a visible track.
    public static func bars(_ waveform: Data?, count bars: Int) -> [Double] {
        let count = max(1, bars)
        guard let waveform, !waveform.isEmpty else {
            return [Double](repeating: 0.15, count: count)
        }
        let bytes = [UInt8](waveform)
        return (0..<count).map { i in
            let src = min(bytes.count - 1, (i * bytes.count) / count)
            // A floor keeps silent stretches visible as a track.
            return max(0.08, Double(bytes[src]) / 255)
        }
    }

    /// `AVAudioRecorder.averagePower(forChannel:)` reports dBFS in
    /// `-160...0`; convert it to a linear amplitude in `0...1` for the meter
    /// and the waveform series. Non-finite input reads as silence.
    public static func linearLevel(decibels: Float) -> Double {
        guard decibels.isFinite else { return 0 }
        let clamped = min(0, max(-160, Double(decibels)))
        return min(1, max(0, pow(10, clamped / 20)))
    }
}
