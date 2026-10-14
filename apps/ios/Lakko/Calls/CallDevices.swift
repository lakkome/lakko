// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import Foundation
import UIKit
import WebRTC

/// The microphones, cameras and outputs a call can choose between on this
/// platform, and the act of choosing one (`docs/ux/calls.md` section 4.1).
///
/// ## What iOS lets an app choose by itself
///
/// - **Microphone**: any input the audio session reports (the built-in
///   microphone, a wired headset's, a Bluetooth device's). On iOS an input
///   and its output are one route, so choosing a headset's microphone moves
///   the sound to that headset too.
/// - **Camera**: any camera the device has.
/// - **Speaker**: one thing only, the built-in loudspeaker (an output port
///   override), as an alternative to whatever the system picks. A particular
///   Bluetooth or AirPlay output, a wired pair of headphones with no
///   microphone, and the earpiece on a call that defaults to the loudspeaker
///   cannot be chosen from inside an app: the system offers those only
///   through its own route sheet, which a Lakko screen does not open.
///
/// Every setter goes through `RTCAudioSession`'s lock, as
/// ``CallAudioSession`` explains.
@MainActor
enum CallDevices {

    /// The one output the app can route to by itself.
    static let loudspeakerId = "builtin-loudspeaker"

    /// What the person chose for the call that is running, so the choice
    /// holds for the call whichever screen is showing it (a call put away and
    /// brought back draws a fresh screen). `nil` is the system default.
    private(set) static var chosenMicrophone: String?
    private(set) static var chosenSpeaker: String?
    static var chosenCamera: String? { LocalCameraCapture.preferredDeviceId }

    // MARK: - What there is

    /// Everything the platform reports, before the "is this a choice at all"
    /// rule (``CallSettingsModel/choosable(_:)``).
    static func reported() -> [CallUiState.CallDevice] {
        var out: [CallUiState.CallDevice] = []
        for input in AVAudioSession.sharedInstance().availableInputs ?? [] {
            out.append(
                CallUiState.CallDevice(id: input.uid, label: input.portName, kind: .audioInput))
        }
        for camera in RTCCameraVideoCapturer.captureDevices() {
            out.append(
                CallUiState.CallDevice(
                    id: camera.uniqueID, label: camera.localizedName, kind: .videoInput))
        }
        out.append(
            CallUiState.CallDevice(
                id: loudspeakerId, label: Localization.text("call.control.speaker"),
                kind: .audioOutput))
        return out
    }

    /// The devices the settings sheet lists.
    static func available() -> [CallUiState.CallDevice] {
        CallSettingsModel.choosable(reported())
    }

    /// Whether choosing the loudspeaker would change anything right now.
    ///
    /// - Parameter chosen: the output already chosen for this call, if any.
    static func canSelectAudioOutput(chosen: String?) -> Bool {
        let session = AVAudioSession.sharedInstance()
        let externalConnected = (session.availableInputs ?? []).contains {
            $0.portType != .builtInMic
        } || session.currentRoute.outputs.contains {
            $0.portType != .builtInSpeaker && $0.portType != .builtInReceiver
        }
        return loudspeakerIsAChoice(
            alreadyChosen: chosen != nil,
            defaultsToLoudspeaker: session.categoryOptions.contains(.defaultToSpeaker),
            externalOutputConnected: externalConnected,
            hasEarpiece: UIDevice.current.userInterfaceIdiom == .phone)
    }

    /// Whether the loudspeaker is a real alternative to the system's own
    /// choice. With a headset or a Bluetooth device connected it always is.
    /// With nothing connected it is one only where the default is the
    /// earpiece: a phone on a voice call. A call that already plays on the
    /// loudspeaker with nothing else connected has nothing to choose.
    nonisolated static func loudspeakerIsAChoice(
        alreadyChosen: Bool,
        defaultsToLoudspeaker: Bool,
        externalOutputConnected: Bool,
        hasEarpiece: Bool
    ) -> Bool {
        if alreadyChosen || externalOutputConnected { return true }
        return hasEarpiece && !defaultsToLoudspeaker
    }

    // MARK: - Choosing

    /// Use the microphone `id` names, or the system's own choice for `nil`.
    /// Returns whether the platform accepted it.
    @discardableResult
    static func selectMicrophone(_ id: String?) -> Bool {
        let port = id.flatMap { uid in
            AVAudioSession.sharedInstance().availableInputs?.first { $0.uid == uid }
        }
        if id != nil, port == nil { return false }
        let applied = configure("preferred input") { session in
            if let port {
                try session.setPreferredInput(port)
            } else {
                // `RTCAudioSession` has no way to clear the preference; the
                // session it wraps does, and its lock is held here.
                try session.session.setPreferredInput(nil)
            }
        }
        if applied { chosenMicrophone = id }
        return applied
    }

    /// Send the sound to the loudspeaker (`loudspeakerId`), or back to the
    /// system's own choice for `nil`. Returns whether the platform accepted
    /// it.
    @discardableResult
    static func selectSpeaker(_ id: String?) -> Bool {
        let applied = configure("output override") {
            try $0.overrideOutputAudioPort(id == loudspeakerId ? .speaker : .none)
        }
        if applied { chosenSpeaker = id }
        return applied
    }

    /// Use the camera `id` names, or the default (front) for `nil`. Applies
    /// at once when the camera is on, and holds for when it is turned on.
    static func selectCamera(_ id: String?) async {
        LocalCameraCapture.preferredDeviceId = id
        await LocalCameraCapture.active?.use(deviceId: id)
    }

    /// The call is over: the next one starts from the system's defaults.
    static func resetForNextCall() {
        LocalCameraCapture.preferredDeviceId = nil
        // Only what was changed is put back: the session of a call that is
        // over is already on its way down, and there is nothing to undo on a
        // call where nothing was chosen.
        if chosenMicrophone != nil { selectMicrophone(nil) }
        if chosenSpeaker != nil { selectSpeaker(nil) }
        chosenMicrophone = nil
        chosenSpeaker = nil
    }

    private static func configure(
        _ what: String, _ change: (RTCAudioSession) throws -> Void
    ) -> Bool {
        let session = RTCAudioSession.sharedInstance()
        session.lockForConfiguration()
        defer { session.unlockForConfiguration() }
        do {
            try change(session)
            return true
        } catch {
            NSLog("CallDevices: %@ refused: %@", what, String(describing: error))
            return false
        }
    }
}
