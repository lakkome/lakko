// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The call settings sheet: which microphone, camera and speaker the call
/// uses (`docs/ux/calls.md` section 4.1).
///
/// The sheet is the app's own list, never a system picker and never the
/// system's route sheet. Each section is a group of radio rows: the system
/// default first, then every device the call can choose.
///
/// A section exists only where choosing does something. The speaker section
/// needs a platform that lets the app move a call's sound by itself; without
/// it the section is absent rather than a list whose rows change nothing. A
/// section with no device reported is absent too.
///
/// Pure: the screen passes the devices in and draws the answer. The twin of
/// `apps/web/src/calls/call-settings.ts`.
enum CallSettingsModel {

    enum SectionKind: String, CaseIterable, Equatable, Sendable {
        case microphone
        case camera
        case speaker

        /// The devices this section lists.
        var deviceKind: CallUiState.CallDevice.Kind {
            switch self {
            case .microphone: return .audioInput
            case .camera: return .videoInput
            case .speaker: return .audioOutput
            }
        }

        var title: String {
            switch self {
            case .microphone: return Localization.text("call.settings.microphone")
            case .camera: return Localization.text("call.control.camera")
            case .speaker: return Localization.text("call.control.speaker")
            }
        }
    }

    /// One row of a section. An `id` of `nil` is the system default.
    struct Option: Equatable, Identifiable {
        let deviceId: String?
        let label: String
        let selected: Bool

        var id: String { deviceId ?? "" }
    }

    struct Section: Equatable, Identifiable {
        let kind: SectionKind
        let title: String
        let options: [Option]

        var id: String { kind.rawValue }
    }

    /// The sections of the sheet, in order: microphone, camera, speaker.
    ///
    /// A device with no name is numbered. A stored choice that is no longer
    /// reported (a headset was unplugged) reads as the system default, which
    /// is what the call is in fact using.
    static func sections(
        devices: [CallUiState.CallDevice],
        settings: CallUiState.CallSettings,
        canSelectAudioOutput: Bool
    ) -> [Section] {
        var out: [Section] = []
        for kind in SectionKind.allCases {
            if kind == .speaker, !canSelectAudioOutput { continue }
            let listed = devices.filter { $0.kind == kind.deviceKind && !$0.id.isEmpty }
            if listed.isEmpty { continue }
            let stored = chosen(kind, in: settings)
            let chosen = listed.contains(where: { $0.id == stored }) ? stored : nil
            var options = [
                Option(
                    deviceId: nil,
                    label: Localization.text("call.settings.system_default"),
                    selected: chosen == nil)
            ]
            for (index, device) in listed.enumerated() {
                let name = device.label.trimmingCharacters(in: .whitespacesAndNewlines)
                options.append(
                    Option(
                        deviceId: device.id,
                        label: name.isEmpty
                            ? Localization.text(
                                "call.settings.unnamed_device",
                                texts: [
                                    "n": (index + 1).formatted(
                                        .number.locale(Localization.currentLocale))
                                ])
                            : name,
                        selected: chosen == device.id))
            }
            out.append(Section(kind: kind, title: kind.title, options: options))
        }
        return out
    }

    private static func chosen(
        _ kind: SectionKind, in settings: CallUiState.CallSettings
    ) -> String? {
        switch kind {
        case .microphone: return settings.selectedAudioInput
        case .camera: return settings.selectedVideoInput
        case .speaker: return settings.selectedAudioOutput
        }
    }

    /// The devices worth offering, out of everything the platform reports.
    ///
    /// A section with nothing to choose is absent: one microphone, or one
    /// camera, is not a choice (its only row would be the system default
    /// under another name), so a kind with a single input is dropped. An
    /// output is different: the one output the app can route to by itself is
    /// a real alternative to the system's own choice, so it is kept.
    static func choosable(_ devices: [CallUiState.CallDevice]) -> [CallUiState.CallDevice] {
        devices.filter { device in
            switch device.kind {
            case .audioInput, .videoInput:
                return devices.filter { $0.kind == device.kind }.count > 1
            case .audioOutput:
                return true
            }
        }
    }
}
