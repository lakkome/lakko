// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0047 / UX spec §2-§3, §6 — the hold-to-record gesture as a pure state
/// machine. The iOS mirror of `apps/web/src/media/recording-machine.ts`.
///
/// Every rule about WHEN a take starts, is sent, is locked, is cancelled or is
/// thrown away lives here, as `reduce(state, event) -> (state, effects)`, so
/// it runs under XCTest with no microphone, camera or gesture. The SwiftUI glue
/// (`MediaRecordingController`) only translates the gesture, timer and
/// capture callbacks into events and performs the effects this returns.
///
/// The rules, and where they come from:
///
///   * Press and hold starts recording immediately, once permission is known
///     to be granted (§2 flow 1, §6).
///   * Release sends (§2.3). Slide left past ``cancelSlidePoints`` cancels
///     (§2.4). Slide up past ``lockSlidePoints`` locks (§2.5).
///   * A take shorter than ``minTakeMs`` is discarded with the `Hold to
///     record` hint (§2.8), and that short tap toggles mic / camera (§1).
///   * An accessibility activation (VoiceOver double-tap, no hold) starts a
///     LOCKED recording — the single-activation path §7 requires.
///   * The cap (``voiceCapMs`` 15 min, ``videoNoteCapMs`` 60 s) stops the take
///     into preview (§2.7, §3.3).
///   * The hold that raised a permission PROMPT does not record (§6). iOS
///     reports "not determined" exactly, so unlike the web machine there is no
///     timing inference: a press that had to ask never starts a take.
///   * Permission revoked mid-take discards the take and shows the denied card.
public enum RecordingMachine {

    /// §2.7 — the voice cap.
    public static let voiceCapMs: Int64 = 15 * 60 * 1000
    /// §3.3 — the video-note cap.
    public static let videoNoteCapMs: Int64 = 60 * 1000
    /// §2.8 — shorter takes are discarded.
    public static let minTakeMs: Int64 = 500
    /// §2.4 — how far left a held finger travels before the take is cancelled.
    public static let cancelSlidePoints: Double = 80
    /// §2.5 — how far up a held finger travels before the take locks.
    public static let lockSlidePoints: Double = 60

    public static func cap(for kind: CaptureKind) -> Int64 {
        kind == .voice ? voiceCapMs : videoNoteCapMs
    }

    public static func reduce(_ state: RecState, _ event: RecEvent) -> RecStep {
        switch state {
        case .idle:
            return fromIdle(event)
        case .requesting(let kind):
            return fromRequesting(kind: kind, event)
        case .recording(let kind, let startedAt, _, _):
            return fromRecording(state, kind: kind, startedAt: startedAt, event)
        case .locked(let kind, let startedAt):
            return fromLocked(state, kind: kind, startedAt: startedAt, event)
        case .finalizing(let kind):
            return fromFinalizing(state, kind: kind, event)
        case .preview:
            return fromPreview(state, event)
        }
    }

    private static func fromIdle(_ event: RecEvent) -> RecStep {
        switch event {
        case .press(let kind, let at, let permission):
            switch permission {
            case .denied:
                return RecStep(.idle, [.showDenied(kind)])
            case .granted:
                return RecStep(
                    .recording(kind: kind, startedAt: at, dx: 0, dy: 0),
                    [.startCapture(kind)]
                )
            case .prompt:
                return RecStep(.requesting(kind: kind), [.requestPermission(kind)])
            }
        case .activate(let kind, let at, let permission):
            switch permission {
            case .denied:
                return RecStep(.idle, [.showDenied(kind)])
            case .granted:
                return RecStep(.locked(kind: kind, startedAt: at), [.startCapture(kind)])
            case .prompt:
                return RecStep(.requesting(kind: kind), [.requestPermission(kind)])
            }
        default:
            return RecStep(.idle)
        }
    }

    private static func fromRequesting(kind: CaptureKind, _ event: RecEvent) -> RecStep {
        switch event {
        case .permissionResult(let granted):
            // §6: the gesture that raised the prompt does not record, granted
            // or not. A denial shows the card; a grant simply returns to idle
            // so the NEXT hold records.
            return granted ? RecStep(.idle) : RecStep(.idle, [.showDenied(kind)])
        case .cancel:
            // Nothing is recording yet; a grant still on its way must not
            // start one, and `permissionResult` is ignored in idle.
            return RecStep(.idle)
        default:
            return RecStep(.requesting(kind: kind))
        }
    }

    private static func fromRecording(
        _ state: RecState,
        kind: CaptureKind,
        startedAt: Int64,
        _ event: RecEvent
    ) -> RecStep {
        switch event {
        case .move(let dx, let dy):
            if dx <= -cancelSlidePoints {
                return RecStep(.idle, [.finish(.discard), .hint(.cancelled)])
            }
            if dy <= -lockSlidePoints {
                return RecStep(.locked(kind: kind, startedAt: startedAt))
            }
            return RecStep(.recording(kind: kind, startedAt: startedAt, dx: dx, dy: dy))
        case .release(let at):
            if at - startedAt < minTakeMs {
                return RecStep(.idle, [.finish(.discard), .hint(.holdToRecord), .toggleMode])
            }
            return RecStep(.idle, [.finish(.send)])
        case .tick(let at):
            if at - startedAt >= cap(for: kind) {
                return RecStep(.finalizing(kind: kind), [.finish(.preview)])
            }
            return RecStep(state)
        case .cancel:
            return RecStep(.idle, [.finish(.discard), .hint(.cancelled)])
        case .delete, .captureFailed:
            return RecStep(.idle, [.finish(.discard)])
        case .revoked:
            return RecStep(.idle, [.finish(.discard), .showDenied(kind)])
        default:
            return RecStep(state)
        }
    }

    private static func fromLocked(
        _ state: RecState,
        kind: CaptureKind,
        startedAt: Int64,
        _ event: RecEvent
    ) -> RecStep {
        switch event {
        case .stop(let at):
            if at - startedAt < minTakeMs {
                return RecStep(.idle, [.finish(.discard), .hint(.holdToRecord)])
            }
            return RecStep(.finalizing(kind: kind), [.finish(.preview)])
        case .tick(let at):
            if at - startedAt >= cap(for: kind) {
                return RecStep(.finalizing(kind: kind), [.finish(.preview)])
            }
            return RecStep(state)
        case .delete, .cancel, .captureFailed:
            return RecStep(.idle, [.finish(.discard)])
        case .revoked:
            return RecStep(.idle, [.finish(.discard), .showDenied(kind)])
        default:
            return RecStep(state)
        }
    }

    private static func fromFinalizing(
        _ state: RecState,
        kind: CaptureKind,
        _ event: RecEvent
    ) -> RecStep {
        switch event {
        case .previewReady(let durationMs):
            if durationMs < minTakeMs {
                return RecStep(.idle, [.discardPreview, .hint(.holdToRecord)])
            }
            return RecStep(.preview(kind: kind, durationMs: durationMs))
        case .previewFailed, .captureFailed:
            return RecStep(.idle)
        case .revoked:
            return RecStep(.idle, [.discardPreview, .showDenied(kind)])
        default:
            return RecStep(state)
        }
    }

    private static func fromPreview(_ state: RecState, _ event: RecEvent) -> RecStep {
        switch event {
        case .send:
            return RecStep(.idle, [.sendPreview])
        case .delete, .cancel:
            return RecStep(.idle, [.discardPreview])
        default:
            return RecStep(state)
        }
    }

    // MARK: - Formatting

    /// `0:07`, `1:02`, `15:00` — the recording bar and bubble duration format.
    public static func formatClock(ms: Int64) -> String {
        let total = max(0, ms / 1000)
        let minutes = total / 60
        let seconds = total % 60
        return "\(minutes):" + (seconds < 10 ? "0\(seconds)" : "\(seconds)")
    }

    /// Spoken duration for accessibility labels: "12 seconds", "1 minute 5 seconds".
    public static func spokenDuration(ms: Int64) -> String {
        let total = max(0, Int64((Double(ms) / 1000).rounded()))
        let minutes = total / 60
        let seconds = total % 60
        let s = "\(seconds) \(seconds == 1 ? "second" : "seconds")"
        if minutes == 0 { return s }
        let m = "\(minutes) \(minutes == 1 ? "minute" : "minutes")"
        return seconds == 0 ? m : "\(m) \(s)"
    }
}

/// The two recordable kinds. The raw value is the `AttachmentRef.kind` string.
public enum CaptureKind: String, Equatable, Sendable {
    case voice
    case videoNote = "video_note"
}

/// What the platform says about the permission(s) a kind needs, before any
/// capture is attempted. `prompt` means the system will ask.
public enum PermissionHint: Equatable {
    case granted
    case prompt
    case denied
}

public enum RecordingHint: Equatable {
    case holdToRecord
    case cancelled

    public var copy: String {
        switch self {
        case .holdToRecord: return "Hold to record"
        case .cancelled: return "Cancelled"
        }
    }
}

public enum FinishDisposition: Equatable {
    /// Upload and send the take now.
    case send
    /// Stop the capture and show the preview.
    case preview
    /// Stop the capture and throw the take away.
    case discard
}

public enum RecState: Equatable {
    case idle
    /// Waiting for the system permission prompt the press raised.
    case requesting(kind: CaptureKind)
    /// A finger is down and the take is running. `dx` / `dy` are the offset
    /// from the press point (negative = left / up) for the slide feedback.
    case recording(kind: CaptureKind, startedAt: Int64, dx: Double, dy: Double)
    case locked(kind: CaptureKind, startedAt: Int64)
    /// Waiting for the recorder to hand over the finished take.
    case finalizing(kind: CaptureKind)
    case preview(kind: CaptureKind, durationMs: Int64)

    public var kind: CaptureKind? {
        switch self {
        case .idle: return nil
        case .requesting(let k), .recording(let k, _, _, _), .locked(let k, _),
             .finalizing(let k), .preview(let k, _):
            return k
        }
    }

    /// Is a capture running in this state?
    public var isCapturing: Bool {
        switch self {
        case .recording, .locked: return true
        default: return false
        }
    }

    /// The recording start time, while capturing.
    public var startedAt: Int64? {
        switch self {
        case .recording(_, let s, _, _), .locked(_, let s): return s
        default: return nil
        }
    }
}

public enum RecEvent: Equatable {
    case press(kind: CaptureKind, at: Int64, permission: PermissionHint)
    /// Offset of the held finger from the press point, in points.
    case move(dx: Double, dy: Double)
    case release(at: Int64)
    /// VoiceOver / Switch Control activation of the record button.
    case activate(kind: CaptureKind, at: Int64, permission: PermissionHint)
    case permissionResult(granted: Bool)
    case tick(at: Int64)
    case stop(at: Int64)
    case delete
    /// The gesture was cancelled by the system, or a call started.
    case cancel
    case send
    case revoked
    case captureFailed
    case previewReady(durationMs: Int64)
    case previewFailed
}

public enum RecEffect: Equatable {
    case requestPermission(CaptureKind)
    case startCapture(CaptureKind)
    case finish(FinishDisposition)
    case hint(RecordingHint)
    case showDenied(CaptureKind)
    case toggleMode
    case sendPreview
    case discardPreview
}

public struct RecStep: Equatable {
    public let state: RecState
    public let effects: [RecEffect]

    public init(_ state: RecState, _ effects: [RecEffect] = []) {
        self.state = state
        self.effects = effects
    }
}
