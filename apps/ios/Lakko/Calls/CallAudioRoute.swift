// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import Foundation

/// Where a call's sound comes out (`docs/ux/calls.md` section 4.1).
///
/// The Speaker button did not exist on this screen at all, although it is the
/// control people reach for most on a voice call: a phone held away from the ear
/// with nothing to press is the commonest complaint about a call UI. This is the
/// platform half of it.
///
/// It is deliberately thin. WebRTC owns the `AVAudioSession` configuration for
/// the duration of a call, so the only thing worth doing here is asking the
/// session to override the output port and then reporting **what the route
/// actually is** — never what was asked for. A device that refuses (a wired
/// headset is plugged in, CallKit moved the call to a car) leaves the button
/// showing the truth rather than a lie.
enum CallAudioRoute {

    /// Move the sound to the loudspeaker, or back to whatever the system would
    /// otherwise pick. Returns the route as it stands afterwards.
    @discardableResult
    static func setSpeakerOn(_ on: Bool) -> Bool {
        let session = AVAudioSession.sharedInstance()
        do {
            try session.overrideOutputAudioPort(on ? .speaker : .none)
        } catch {
            NSLog("CallAudioRoute: output override refused: %@", String(describing: error))
        }
        return isSpeakerOn()
    }

    /// `true` when the current route's output is the built-in loudspeaker.
    static func isSpeakerOn() -> Bool {
        AVAudioSession.sharedInstance().currentRoute.outputs.contains {
            $0.portType == .builtInSpeaker
        }
    }
}
