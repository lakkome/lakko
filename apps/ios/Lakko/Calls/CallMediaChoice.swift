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

/// The one place that decides what a call button asks for, what the pre-join
/// opens with, and what the offer then carries (`docs/ux/calls.md` section 3).
///
/// ## Why this exists
///
/// The header's two buttons already passed `video:` down, and every rung under
/// them dropped it: `ChatViewModel.startCall` discarded its own parameters
/// (`_ = (audio, video)`), `CallHost.startOutgoingCall` never took one, and the
/// pre-join screen hard-coded its camera toggle ON. So the phone button opened
/// the pre-join with the camera live, Join offered `video: true`, and the
/// callee was told "Video call" for a call the caller had asked to make with
/// their voice — the exact thing section 3 forbids ("the word on the callee's
/// screen matches what the caller chose, always").
///
/// The rule is two-sided and both halves live here:
///
///   * the **button seeds** the pre-join camera — off for voice, on for video;
///   * the **toggles decide** the offer — whatever the user left them at when
///     they tapped Join is what goes on the wire, so a voice call whose camera
///     the user turned on IS a video call, honestly announced as one.
public enum CallMediaChoice {

    /// The button for a `video:` flag, for the call sites that still speak in
    /// booleans (the toolbar, the group-call host).
    public static func button(video: Bool) -> CallButtonKind {
        video ? .video : .voice
    }

    /// The camera state the pre-join screen opens with. A voice call never
    /// turns the camera on.
    public static func prejoinCameraOn(for button: CallButtonKind) -> Bool {
        switch button {
        case .voice: return false
        case .video: return true
        }
    }

    /// The media flags the offer carries: exactly what the pre-join toggles
    /// said when Join was tapped — never what the button asked for.
    public static func offeredMedia(micOn: Bool, cameraOn: Bool) -> (audio: Bool, video: Bool) {
        (audio: micOn, video: cameraOn)
    }

    /// The one line the callee's screen shows for those flags. Derived from
    /// the flags on the offer, so it cannot disagree with them.
    public static func kindLabel(video: Bool) -> String {
        video ? "Video call" : "Voice call"
    }
}
