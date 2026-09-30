// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Which of the chat header's two buttons asked for the call
/// (`docs/ux/calls.md` section 3).
public enum CallButtonKind: String, Equatable, Sendable {
    /// The phone button.
    case voice
    /// The camera button.
    case video
}

/// What a call button opens (`docs/ux/calls.md` sections 3.1 and 3.2).
///
/// One case, and that is the decision: every call button dials. The enum is
/// kept so the call sites keep asking one question in one place, and so a
/// future pre-call surface has somewhere to be added. `cameraCheck` was the
/// other case until 2026-09-21.
public enum CallStartSurface: Equatable, Sendable {
    /// No screen at all before the call: dial now with these media flags, and
    /// let the call screen open on `Calling...`.
    case dial(video: Bool)
}

/// The one place that decides what a call button opens and what the offer then
/// carries (`docs/ux/calls.md` sections 3.1, 3.2 and 3.3).
///
/// ## Why this exists, and what changed
///
/// It was written to stop the button's choice being dropped on the way down —
/// the phone button used to open a pre-join with the camera already live, so the
/// callee was told "Video call" for a call made with a voice. That half still
/// holds: ``offeredMedia(micOn:cameraOn:)`` and ``kindLabel(video:)`` are the
/// rule that the word on the callee's screen is derived from the flags on the
/// wire, and cannot disagree with them.
///
/// What the redesign removed is the pre-join itself. Asking "check your
/// microphone" before a voice call is a settings page in front of a phone call,
/// and no messenger does it. So:
///
///   * the **phone button dials immediately** — there is nothing to decide, and
///     every control that could change the call is on the call screen anyway;
///   * the **video button dials too**, carrying the camera. It opened a camera
///     check until 2026-09-21 — a full-screen self-view with a Call button —
///     on the ground that a person about to be seen deserves to know what they
///     look like. That reason is met without the second press: the self-view is
///     on the calling screen, so the caller sees themselves while it rings;
///   * a device with **no camera** answers the video button with a voice call
///     and says so in one line, rather than dialling a video call it cannot
///     make.
public enum CallMediaChoice {

    /// The button for a `video:` flag, for the call sites that still speak in
    /// booleans (the toolbar, the contact screen, the group-call host).
    public static func button(video: Bool) -> CallButtonKind {
        video ? .video : .voice
    }

    /// What the tapped button opens.
    ///
    /// - Parameter hasCamera: whether this device has a usable camera at all
    ///   (``LocalCameraCapture/hasUsableCamera()``).
    public static func startSurface(
        for button: CallButtonKind, hasCamera: Bool
    ) -> CallStartSurface {
        switch button {
        case .voice:
            return .dial(video: false)
        case .video:
            return .dial(video: hasCamera)
        }
    }

    /// The one line a video call downgraded to a voice call says, or `nil` when
    /// nothing was downgraded. Never a reason to refuse the call: a phone with
    /// no camera can still talk.
    public static func noCameraNotice(
        for button: CallButtonKind, hasCamera: Bool
    ) -> String? {
        guard button == .video, !hasCamera else { return nil }
        return Localization.text("call.start.no_camera")
    }

    /// The media flags the offer carries: exactly what the buttons said when the
    /// call was placed — never what the header button asked for. A voice call
    /// dials `(audio: true, video: false)`; the camera check's two round buttons
    /// decide a video call's flags.
    public static func offeredMedia(micOn: Bool, cameraOn: Bool) -> (audio: Bool, video: Bool) {
        (audio: micOn, video: cameraOn)
    }

    /// The one line the callee's screen shows for those flags. Derived from the
    /// flags on the offer, so it cannot disagree with them.
    public static func kindLabel(video: Bool) -> String {
        Localization.text(video ? "call.kind.video" : "call.kind.voice")
    }
}
