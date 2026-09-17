// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import Foundation

/// ADR-0047 / UX spec §3 — the pure decisions behind a video note bubble's
/// tap, kept apart from `VideoNotePlayerModel` so they can be tested without
/// an `AVPlayer`.
enum VideoNotePlayback {

    enum Mode: Equatable {
        /// The poster, nothing playing.
        case poster
        /// Autoplay: muted and looping.
        case muted
        /// Playing (or paused) with sound after a tap.
        case sound
    }

    enum TapAction: Equatable {
        /// A start is already under way; a second tap must not build a
        /// second player.
        case ignore
        case pause
        case resume
        /// Download if needed, wait until the item is ready, then play with
        /// sound from the start.
        case start
    }

    static func tapAction(mode: Mode, paused: Bool, starting: Bool, hasPlayer: Bool) -> TapAction {
        if starting { return .ignore }
        if mode == .sound, hasPlayer { return paused ? .resume : .pause }
        return .start
    }

    enum Readiness: Equatable {
        case ready
        case failed
        case timedOut
    }

    /// Classifies the result of waiting for `AVPlayerItem.status` to leave
    /// `.unknown`; `nil` means the wait timed out.
    static func readiness(_ status: AVPlayerItem.Status?) -> Readiness {
        switch status {
        case .readyToPlay: return .ready
        case .failed: return .failed
        case .unknown, .none: return .timedOut
        @unknown default: return .failed
        }
    }

    /// Whether a playback that was asked to start and then waited for
    /// `timeControlStatus` to reach `.playing` must be reported as failed.
    /// `observed` is `nil` when the wait timed out. A user pause during the
    /// wait, or a teardown, is not a failure.
    static func startFailed(observed: AVPlayer.TimeControlStatus?, stillWanted: Bool) -> Bool {
        guard stillWanted else { return false }
        return observed != .playing
    }
}

/// Waits for a key-value-observable property to satisfy a predicate, with a
/// timeout. Returns the accepted value, or `nil` on timeout.
@MainActor
enum KeyValueWait {

    static func first<Object: NSObject, Value>(
        of object: Object,
        _ keyPath: KeyPath<Object, Value>,
        timeout: Duration,
        where accept: @escaping (Value) -> Bool
    ) async -> Value? {
        let current = object[keyPath: keyPath]
        if accept(current) { return current }
        let once = ResumeOnce<Value>()
        return await withCheckedContinuation { continuation in
            once.continuation = continuation
            once.observation = object.observe(keyPath, options: [.new]) { observed, _ in
                let value = observed[keyPath: keyPath]
                guard accept(value) else { return }
                Task { @MainActor in once.resume(value) }
            }
            once.timeout = Task { @MainActor in
                try? await Task.sleep(for: timeout)
                once.resume(nil)
            }
            // The value may have changed between the first read and the
            // observation being installed.
            let again = object[keyPath: keyPath]
            if accept(again) { once.resume(again) }
        }
    }
}

@MainActor
private final class ResumeOnce<Value> {
    var continuation: CheckedContinuation<Value?, Never>?
    var observation: NSKeyValueObservation?
    var timeout: Task<Void, Never>?

    func resume(_ value: Value?) {
        guard let continuation else { return }
        self.continuation = nil
        observation?.invalidate()
        observation = nil
        timeout?.cancel()
        timeout = nil
        continuation.resume(returning: value)
    }
}
