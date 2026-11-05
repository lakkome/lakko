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
///   * §3.1 — the composer's right-hand button is ONE button answering TWO
///     gestures, and which one it was is decided on the GESTURE, not on what
///     the take turned out to be. A touch that goes down and comes up within
///     ``holdToRecordMs`` without travelling past ``tapSlopPoints`` is a TAP:
///     it switches mic / camera and records nothing — it never opens the
///     microphone, never raises a permission prompt and never shows the
///     denied card. A touch still down at ``holdToRecordMs`` is a HOLD, and
///     only then does the permission decision run and a take begin.
///   * Release sends (§2.3). Slide left past ``cancelSlidePoints`` cancels
///     (§2.4). Slide up past ``lockSlidePoints`` locks (§2.5).
///   * A take shorter than ``minTakeMs`` is discarded with the `Hold to
///     record` hint (§2.8). It is NOT a mode switch: by then the press had
///     already passed the hold threshold and opened the microphone, so it is
///     the accidental-press case, not the tap.
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
    /// §3.1 — how long a finger must stay down before the press is a HOLD
    /// rather than a tap. Short enough that a real hold does not feel
    /// delayed, long enough that a deliberate tap never opens the microphone;
    /// the take's own clock starts here, so it does not eat into
    /// ``minTakeMs``.
    public static let holdToRecordMs: Int64 = 200
    /// §3.1 — how far a finger may drift and still be a tap. Anything further
    /// is a gesture, not a tap, so releasing it switches nothing.
    public static let tapSlopPoints: Double = 10
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
        case .pressing(let kind, let at, let permission, let moved):
            return fromPressing(kind: kind, at: at, permission: permission, moved: moved, event)
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
            // §3.1 — a press is not yet a take. Nothing is decided here: not
            // permission, not capture, not the denied card. The finger has to
            // stay down for ``holdToRecordMs`` first, and a tap that lifts
            // before then only switches the mode.
            return RecStep(
                .pressing(kind: kind, at: at, permission: permission, moved: false),
                [.armHold]
            )
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

    /// §3.1 — the window in which the one button is still both gestures.
    ///
    /// The tap / hold verdict is taken here, on the GESTURE, and never on what
    /// the take turned out to be: a release inside the window is a tap and a
    /// mode switch, and everything that would have opened the microphone
    /// waits for ``RecEvent/holdElapsed(at:)``.
    private static func fromPressing(
        kind: CaptureKind,
        at: Int64,
        permission: PermissionHint,
        moved: Bool,
        _ event: RecEvent
    ) -> RecStep {
        switch event {
        case .move(let dx, let dy):
            // A finger that has travelled is no longer a tap, however soon it
            // lifts. The take still waits for the hold threshold, so a flick
            // that both starts and ends inside the window records nothing and
            // switches nothing.
            // Per axis, not by distance, because that is how the web machine
            // reads its identical `TAP_SLOP_PX` and a diagonal drift must not
            // be a tap on one client and a gesture on another.
            let travelled = moved || abs(dx) > tapSlopPoints || abs(dy) > tapSlopPoints
            return RecStep(.pressing(kind: kind, at: at, permission: permission, moved: travelled))
        case .release:
            // The tap, and the ONLY thing that switches the mode.
            return RecStep(.idle, moved ? [] : [.toggleMode])
        case .holdElapsed(let now):
            // A hold: now the permission decision runs, exactly as the press
            // used to do it, and the take's clock starts from here.
            switch permission {
            case .denied:
                return RecStep(.idle, [.showDenied(kind)])
            case .granted:
                return RecStep(
                    .recording(kind: kind, startedAt: now, dx: 0, dy: 0),
                    [.startCapture(kind)]
                )
            case .prompt:
                return RecStep(.requesting(kind: kind), [.requestPermission(kind)])
            }
        case .cancel, .captureFailed, .revoked:
            return RecStep(.idle)
        default:
            return RecStep(.pressing(kind: kind, at: at, permission: permission, moved: moved))
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
                // §3.1 — the accidental press, NOT a tap: the hold threshold
                // had already been passed and the microphone opened, so this
                // discards with the hint and switches nothing.
                return RecStep(.idle, [.finish(.discard), .hint(.holdToRecord)])
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

    /// The TOTAL duration of a recording or received media message: rounds UP
    /// to the next whole second, so any non-zero duration below one second
    /// reads `0:01` instead of `0:00`. Elapsed / playback-position counters
    /// keep `formatClock` (rounded down). Display only — never the wire value.
    public static func formatTotalClock(ms: Int64) -> String {
        guard ms > 0 else { return formatClock(ms: 0) }
        let (quotient, remainder) = ms.quotientAndRemainder(dividingBy: 1000)
        return formatClock(ms: (remainder == 0 ? quotient : quotient + 1) * 1000)
    }

    /// Spoken duration for an elapsed time or a playback position, rounded
    /// DOWN like ``formatClock(ms:)``: "12 seconds", "1 minute 5 seconds".
    /// The visible time and the spoken label use one rounding rule, the
    /// visible one (base walk 2026-11-04).
    public static func spokenDuration(ms: Int64) -> String {
        spoken(seconds: max(0, ms) / 1000)
    }

    /// Spoken TOTAL duration of a recording or received media message,
    /// rounded UP like ``formatTotalClock(ms:)``, so a 3.2 s message shown as
    /// `0:04` is also read as "4 seconds".
    public static func spokenTotalDuration(ms: Int64) -> String {
        guard ms > 0 else { return spoken(seconds: 0) }
        let (quotient, remainder) = ms.quotientAndRemainder(dividingBy: 1000)
        return spoken(seconds: remainder == 0 ? quotient : quotient + 1)
    }

    private static func spoken(seconds totalSeconds: Int64) -> String {
        let total = max(0, totalSeconds)
        let minutes = total / 60
        let seconds = total % 60
        let s = Localization.text("common.duration.seconds", counts: ["count": UInt64(seconds)])
        if minutes == 0 { return s }
        let m = Localization.text("common.duration.minutes", counts: ["count": UInt64(minutes)])
        return seconds == 0
            ? m : Localization.text("common.duration.minutes_seconds", texts: ["minutes": m, "seconds": s])
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
        case .holdToRecord: return Localization.text("chat.record.hint_hold")
        case .cancelled: return Localization.text("chat.record.cancelled")
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
    /// §3.1 — a finger is down and it is not yet known whether this is a tap
    /// (switch the mode) or a hold (record). `permission` is what the platform
    /// said when the finger landed, carried so the hold can act on it without
    /// the press having asked anything; `moved` records that the finger has
    /// travelled past ``RecordingMachine/tapSlopPoints``, which disqualifies
    /// the tap. NOTHING is captured in this state.
    case pressing(kind: CaptureKind, at: Int64, permission: PermissionHint, moved: Bool)
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
        case .pressing(let k, _, _, _), .requesting(let k), .recording(let k, _, _, _),
             .locked(let k, _), .finalizing(let k), .preview(let k, _):
            return k
        }
    }

    /// A finger is down on the record button, whether or not a take has begun.
    /// The composer keeps the button mounted through this, or SwiftUI would
    /// drop the gesture that is deciding tap from hold.
    public var isPressed: Bool {
        if case .pressing = self { return true }
        return false
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
    /// §3.1 — the finger has been down for ``RecordingMachine/holdToRecordMs``,
    /// so the press is a hold and the take may begin. `at` becomes the take's
    /// start, so the ``RecordingMachine/minTakeMs`` floor measures the
    /// recording rather than the finger.
    case holdElapsed(at: Int64)
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
    /// §3.1 — start the clock that turns this press into a hold. The host
    /// arms it once per press and must let it fire only while the press is
    /// still down.
    case armHold
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
