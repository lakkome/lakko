// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import WebRTC

/// Lakko's capture noise suppressor on iOS (ADR-0088, `docs/ux/calls.md`
/// section 4.1.3). The processor itself, its threads and its failure rules
/// are in `CaptureNoiseProcessor.{h,mm}`; this file owns the one process-wide
/// processor, the audio processing module the shared peer connection factory
/// is built with, and the switch.
enum CaptureNoiseSuppression {
    /// The one capture post-processing delegate. Held here strongly for the
    /// life of the process: the SDK holds its delegates weakly.
    static let processor = LakkoCaptureNoiseProcessor(
        engine: LakkoNativeNoiseEngine(),
        log: { line in NSLog("[lakko.noise] %@", line) })

    /// The audio processing module `WebRTCPeerConnection.sharedFactory` is
    /// built with: libwebrtc's default module with `processor` as its
    /// capture post-processing stage. Called once per process.
    static func audioProcessingModule() -> RTCDefaultAudioProcessingModule {
        // The initialiser that takes the delegate also takes an
        // `RTCAudioProcessingConfig`, a class the framework does not export a
        // header for, so Swift does not see it; the delegate property sets
        // the same adapter before the module is used.
        let module = RTCDefaultAudioProcessingModule()
        module.capturePostProcessingDelegate = processor
        NSLog("[lakko.noise] noise suppression hook installed (libwebrtc capture post-processing)")
        return module
    }

    /// The platform processing the module runs under, for the arm line. The
    /// shared factory never bypasses the voice-processing I/O unit.
    static let platformEffects =
        "platform voice-processing I/O (echo cancellation, noise suppression, gain control)"
}

/// The `Noise suppression` switch of the call's Settings sheet
/// (`docs/ux/calls.md` section 4.1.3). On by default; this device only, in
/// the app's own defaults (not per account: it is about this phone's
/// microphone); never sent to anyone and not synced. A change applies to a
/// running call at once.
final class NoiseSuppressionSetting: ObservableObject {
    static let shared = NoiseSuppressionSetting(defaults: .standard)

    static let defaultsKey = "lakko.call.noiseSuppression"

    private let defaults: UserDefaults
    private let apply: (Bool) -> Void

    /// The switch, for the sheet.
    @Published private(set) var isOn: Bool

    init(
        defaults: UserDefaults,
        apply: @escaping (Bool) -> Void = { CaptureNoiseSuppression.processor.setEnabled($0) }
    ) {
        self.defaults = defaults
        self.apply = apply
        self.isOn = Self.stored(in: defaults)
    }

    /// The stored choice, on by default. Safe from any thread.
    static func stored(in defaults: UserDefaults) -> Bool {
        defaults.object(forKey: defaultsKey) as? Bool ?? true
    }

    /// The choice a starting call's microphone gets.
    var currentChoice: Bool { Self.stored(in: defaults) }

    /// Sets the switch: stored, shown, and applied to a running call.
    func set(_ on: Bool) {
        defaults.set(on, forKey: Self.defaultsKey)
        isOn = on
        apply(on)
    }
}
