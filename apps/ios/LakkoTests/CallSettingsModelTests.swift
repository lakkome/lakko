// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import XCTest
@testable import Lakko

/// `docs/ux/calls.md` section 4.1 — the call's settings sheet: which sections
/// and rows exist for a given list of devices and a given selection. The same
/// list as the web `call-settings.test.ts`.
final class CallSettingsModelTests: XCTestCase {

    private typealias Device = CallUiState.CallDevice

    private let devices: [CallUiState.CallDevice] = [
        Device(id: "mic-1", label: "iPhone Microphone", kind: .audioInput),
        Device(id: "mic-2", label: "AirPods", kind: .audioInput),
        Device(id: "cam-1", label: "Front Camera", kind: .videoInput),
        Device(id: "out-1", label: "Speaker", kind: .audioOutput),
        Device(id: "out-2", label: "  ", kind: .audioOutput),
    ]

    private func sections(
        _ devices: [CallUiState.CallDevice],
        _ settings: CallUiState.CallSettings = .default,
        output: Bool = true
    ) -> [CallSettingsModel.Section] {
        CallSettingsModel.sections(
            devices: devices, settings: settings, canSelectAudioOutput: output)
    }

    func testItListsMicrophoneCameraAndSpeakerEachStartingWithTheSystemDefault() {
        let all = sections(devices)
        XCTAssertEqual(all.map(\.kind), [.microphone, .camera, .speaker])
        XCTAssertEqual(all.map(\.title), ["Microphone", "Camera", "Speaker"])
        for section in all {
            XCTAssertEqual(
                section.options.first,
                CallSettingsModel.Option(deviceId: nil, label: "System default", selected: true))
        }
    }

    func testEveryDeviceOfAKindIsARowInTheOrderReported() {
        XCTAssertEqual(sections(devices)[0].options.map(\.deviceId), [nil, "mic-1", "mic-2"])
        XCTAssertEqual(
            sections(devices)[0].options.map(\.label),
            ["System default", "iPhone Microphone", "AirPods"])
    }

    func testItMarksTheChosenDeviceAndNothingElse() {
        let mic = sections(devices, CallUiState.CallSettings(selectedAudioInput: "mic-2"))[0]
        XCTAssertEqual(mic.options.filter(\.selected).map(\.deviceId), ["mic-2"])
        // A choice in one section leaves the others on the system default.
        let camera = sections(devices, CallUiState.CallSettings(selectedAudioInput: "mic-2"))[1]
        XCTAssertEqual(camera.options.filter(\.selected).map(\.deviceId), [nil])
    }

    /// A headset was unplugged: the call is in fact on the system default,
    /// and the list says so.
    func testAChoiceThatIsNoLongerReportedReadsAsTheSystemDefault() {
        let mic = sections(devices, CallUiState.CallSettings(selectedAudioInput: "unplugged"))[0]
        XCTAssertEqual(mic.options.filter(\.selected).map(\.deviceId), [nil])
    }

    /// A section exists only where choosing does something.
    func testThereIsNoSpeakerSectionWhereTheAppCannotMoveTheSound() {
        XCTAssertEqual(sections(devices, output: false).map(\.kind), [.microphone, .camera])
    }

    func testThereIsNoSectionForAKindWithNoDevice() {
        let onlyMics = devices.filter { $0.kind == .audioInput }
        XCTAssertEqual(sections(onlyMics).map(\.kind), [.microphone])
        XCTAssertEqual(sections([]), [], "and with no section at all the sheet says so")
    }

    func testADeviceWithNoNameIsNumbered() {
        let speaker = sections(devices)[2]
        XCTAssertEqual(
            speaker.options.map { $0.label.withoutIsolates },
            ["System default", "Speaker", "Device 2"])
    }

    func testADeviceWithNoIdIsNotARow() {
        let nameless = [Device(id: "", label: "Ghost", kind: .audioInput)]
        XCTAssertEqual(sections(nameless), [])
    }

    // MARK: - What is worth offering

    /// One microphone, or one camera, is not a choice: its only row would be
    /// the system default under another name.
    func testASingleMicrophoneOrCameraIsNotOffered() {
        let one = [
            Device(id: "mic-1", label: "iPhone Microphone", kind: .audioInput),
            Device(id: "cam-1", label: "Front Camera", kind: .videoInput),
        ]
        XCTAssertEqual(CallSettingsModel.choosable(one), [])
        XCTAssertEqual(sections(CallSettingsModel.choosable(one)), [])
    }

    func testTwoOfAKindAreOfferedAndTheLoudspeakerAlwaysIs() {
        let reported = [
            Device(id: "mic-1", label: "iPhone Microphone", kind: .audioInput),
            Device(id: "mic-2", label: "AirPods", kind: .audioInput),
            Device(id: "cam-1", label: "Front Camera", kind: .videoInput),
            Device(id: "out-1", label: "Speaker", kind: .audioOutput),
        ]
        XCTAssertEqual(
            CallSettingsModel.choosable(reported).map(\.id), ["mic-1", "mic-2", "out-1"])
        XCTAssertEqual(
            sections(CallSettingsModel.choosable(reported)).map(\.kind), [.microphone, .speaker])
    }

    func testFrontAndBackCamerasAreAChoice() {
        let cameras = [
            Device(id: "front", label: "Front Camera", kind: .videoInput),
            Device(id: "back", label: "Back Camera", kind: .videoInput),
        ]
        XCTAssertEqual(
            sections(CallSettingsModel.choosable(cameras)).first?.options.map(\.deviceId),
            [nil, "front", "back"])
    }

    // MARK: - A choice holds for the call

    @MainActor
    func testAChoiceHoldsForTheCallAndTheNextCallStartsFromTheDefaults() {
        let state = CallUiState()
        state.setAvailableDevices(devices)
        state.setSelectedAudioInput("mic-2")
        state.setSelectedVideoInput("cam-1")
        state.setSelectedAudioOutput("out-1")
        XCTAssertEqual(state.settings.selectedAudioInput, "mic-2")
        XCTAssertEqual(state.settings.selectedVideoInput, "cam-1")
        XCTAssertEqual(state.settings.selectedAudioOutput, "out-1")
        state.reset()
        XCTAssertEqual(state.settings, .default)
    }
}

/// Where a call's sound comes out, as rules (`docs/ux/calls.md` section 4.1).
final class CallOutputRulesTests: XCTestCase {

    /// A group call, and any call with video, plays on the loudspeaker unless
    /// a headset or a Bluetooth device is connected. `.defaultToSpeaker` is
    /// exactly that: the loudspeaker only when nothing else is connected.
    func testALoudspeakerDefaultAddsTheOptionThatYieldsToAHeadset() {
        let base: AVAudioSession.CategoryOptions = [.allowBluetoothHFP]
        let loud = CallAudioSession.callCategoryOptions(base: base, loudspeakerByDefault: true)
        XCTAssertTrue(loud.contains(.defaultToSpeaker))
        XCTAssertTrue(loud.contains(.allowBluetoothHFP), "a Bluetooth device still takes over")
    }

    /// A one-to-one voice call starts on the earpiece, as it always has.
    func testAVoiceCallKeepsTheEarpiece() {
        let base: AVAudioSession.CategoryOptions = [.allowBluetoothHFP, .defaultToSpeaker]
        let quiet = CallAudioSession.callCategoryOptions(base: base, loudspeakerByDefault: false)
        XCTAssertFalse(quiet.contains(.defaultToSpeaker))
        XCTAssertTrue(quiet.contains(.allowBluetoothHFP))
    }

    /// The loudspeaker is a row in the settings only where choosing it would
    /// change something.
    func testTheLoudspeakerIsOfferedOnlyWhereItIsARealAlternative() {
        // A voice call on a phone: the default is the earpiece.
        XCTAssertTrue(
            CallDevices.loudspeakerIsAChoice(
                alreadyChosen: false, defaultsToLoudspeaker: false,
                externalOutputConnected: false, hasEarpiece: true))
        // A group or video call with nothing connected: it already is the
        // loudspeaker.
        XCTAssertFalse(
            CallDevices.loudspeakerIsAChoice(
                alreadyChosen: false, defaultsToLoudspeaker: true,
                externalOutputConnected: false, hasEarpiece: true))
        // A tablet has no earpiece to choose against.
        XCTAssertFalse(
            CallDevices.loudspeakerIsAChoice(
                alreadyChosen: false, defaultsToLoudspeaker: false,
                externalOutputConnected: false, hasEarpiece: false))
        // A headset took over: the loudspeaker is the way back.
        XCTAssertTrue(
            CallDevices.loudspeakerIsAChoice(
                alreadyChosen: false, defaultsToLoudspeaker: true,
                externalOutputConnected: true, hasEarpiece: false))
        // Once chosen it stays listed, so it can be un-chosen.
        XCTAssertTrue(
            CallDevices.loudspeakerIsAChoice(
                alreadyChosen: true, defaultsToLoudspeaker: true,
                externalOutputConnected: false, hasEarpiece: false))
    }
}
