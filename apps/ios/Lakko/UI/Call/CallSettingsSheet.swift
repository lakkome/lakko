// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import SwiftUI
import UIKit

/// The call's settings (`docs/ux/calls.md` section 4.1): which microphone,
/// camera and speaker the call uses.
///
/// The app's own lists, never a system picker and never the system's route
/// sheet. Each section is a group of radio rows, the system default first.
/// A choice applies at once to the live call and holds for the call.
///
/// Which sections and rows exist is ``CallSettingsModel``; what the platform
/// offers and how a choice is applied is ``CallDevices``. This file draws.
///
/// A group or link call also carries, at the top, the name this person shows
/// in the call (``CallNameEditor``); a one-to-one call has none.
///
/// Above the device lists: first `Sound`, the noise suppression switch
/// (``CallNoiseSuppressionSection``, section 4.1.3), then the camera
/// background (``CallBackgroundSection``, `docs/ux/call-backgrounds.md`).
struct CallSettingsSheet: View {
    @ObservedObject var state: CallUiState
    @ObservedObject var l10n = Localization.shared
    /// Turn the call's camera on, as the camera button does (the background
    /// section's `Turn on camera without background`).
    var onCameraOn: () -> Void = {}
    let onDone: () -> Void

    /// Whether the loudspeaker is a real alternative right now.
    @State private var canSelectAudioOutput = false

    private var sections: [CallSettingsModel.Section] {
        CallSettingsModel.sections(
            devices: state.settings.availableDevices,
            settings: state.settings,
            canSelectAudioOutput: canSelectAudioOutput)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(l10n.t("call.settings.title"))
                .font(.title3)
                .fontWeight(.semibold)
                .foregroundColor(Color.lakkoText)
                .accessibilityAddTraits(.isHeader)

            if state.callNameAvailable {
                CallNameEditor(state: state)
            }

            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    // ADR-0088 — shown whether or not a device list is.
                    CallNoiseSuppressionSection()
                    // ADR-0089 — above the device lists; absent on a device
                    // that cannot run a background at all.
                    if CameraBackground.shared.isAvailable {
                        CallBackgroundSection(state: state, onCameraOn: onCameraOn)
                    }
                    if sections.isEmpty {
                        Text(l10n.t("call.settings.no_devices"))
                            .font(.subheadline)
                            .foregroundColor(Color.lakkoTextMuted)
                    } else {
                        ForEach(sections) { section in
                            sectionView(section)
                        }
                    }
                }
            }

            Button(action: onDone) {
                Text(l10n.t("common.done"))
                    .font(.headline)
                    .foregroundColor(Color.lakkoAccentForeground)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 12)
                    .background(Color.lakkoAccent)
                    .clipShape(
                        RoundedRectangle(cornerRadius: LakkoRadius.control, style: .continuous))
            }
            .buttonStyle(.plain)
        }
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(Color.lakkoBackground.ignoresSafeArea())
        .presentationDetents([.medium, .large])
        .onAppear(perform: refresh)
        // A headset plugged in or taken out changes what there is to choose.
        .onReceive(
            NotificationCenter.default.publisher(for: AVAudioSession.routeChangeNotification)
                .receive(on: DispatchQueue.main)
        ) { _ in refresh() }
    }

    private func sectionView(_ section: CallSettingsModel.Section) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(section.title)
                .font(.footnote.weight(.semibold))
                .foregroundColor(Color.lakkoTextMuted)
                .accessibilityAddTraits(.isHeader)
            VStack(spacing: 0) {
                ForEach(section.options) { option in
                    Button {
                        choose(section.kind, option.deviceId)
                    } label: {
                        HStack(spacing: 12) {
                            Text(option.label)
                                .font(.body)
                                .foregroundColor(Color.lakkoText)
                                .lineLimit(1)
                                .truncationMode(.tail)
                            Spacer(minLength: 0)
                            if option.selected {
                                Image(systemName: "checkmark")
                                    .font(.body.weight(.semibold))
                                    .foregroundColor(Color.lakkoAccent)
                            }
                        }
                        .padding(.horizontal, 12)
                        .frame(minHeight: CallScreenPresentation.minimumTouchTarget)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityAddTraits(option.selected ? .isSelected : [])
                }
            }
            .background(Color.lakkoSurfaceSunken)
            .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card, style: .continuous))
        }
    }

    /// What there is to choose, as the platform reports it now, and what was
    /// chosen for this call.
    private func refresh() {
        state.setAvailableDevices(CallDevices.available())
        state.setSelectedAudioInput(CallDevices.chosenMicrophone)
        state.setSelectedVideoInput(CallDevices.chosenCamera)
        state.setSelectedAudioOutput(CallDevices.chosenSpeaker)
        canSelectAudioOutput = CallDevices.canSelectAudioOutput(
            chosen: state.settings.selectedAudioOutput)
    }

    /// Apply a choice to the live call. The selection moves only when the
    /// platform accepted it, so the list never claims a route the device
    /// refused.
    private func choose(_ kind: CallSettingsModel.SectionKind, _ id: String?) {
        switch kind {
        case .microphone:
            CallDevices.selectMicrophone(id)
            refresh()
        case .speaker:
            CallDevices.selectSpeaker(id)
            refresh()
        case .camera:
            Task {
                await CallDevices.selectCamera(id)
                refresh()
            }
        }
    }
}

/// `Sound`: the noise suppression switch with its hint (`docs/ux/calls.md`
/// section 4.1.3). The platform's switch, as on any settings page; its spoken
/// name is the label, its state on / off. Stored on this device only and
/// applied to a running call at once.
struct CallNoiseSuppressionSection: View {
    @ObservedObject var setting = NoiseSuppressionSetting.shared
    @ObservedObject var l10n = Localization.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(l10n.t("call.settings.sound"))
                .font(.footnote.weight(.semibold))
                .foregroundColor(Color.lakkoTextMuted)
                .accessibilityAddTraits(.isHeader)
            Toggle(isOn: Binding(get: { setting.isOn }, set: { setting.set($0) })) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(l10n.t("call.settings.noise_suppression"))
                        .font(.body)
                        .foregroundColor(Color.lakkoText)
                    Text(l10n.t("call.settings.noise_suppression_hint"))
                        .font(.footnote)
                        .foregroundColor(Color.lakkoTextMuted)
                }
            }
            .tint(Color.lakkoAccent)
            .accessibilityLabel(l10n.t("call.settings.noise_suppression"))
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .frame(minHeight: CallScreenPresentation.minimumTouchTarget)
            .background(Color.lakkoSurfaceSunken)
            .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card, style: .continuous))
        }
    }
}

/// The name this person shows in this call (`docs/ux/calls.md` section 4.1):
/// one field, its placeholder the profile name, and a Save that applies at
/// once. Empty goes back to the profile name. The profile never changes.
struct CallNameEditor: View {
    @ObservedObject var state: CallUiState
    @ObservedObject var l10n = Localization.shared
    @State private var text = ""

    private var verdict: CallNameField.Verdict {
        CallNameField.verdict(text: text, current: state.ownCallName)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(l10n.t("call.settings.call_name.label"))
                .font(.footnote.weight(.semibold))
                .foregroundColor(Color.lakkoTextMuted)
                .accessibilityAddTraits(.isHeader)
            HStack(spacing: 8) {
                TextField(state.callNameDefault, text: $text)
                    .textFieldStyle(.roundedBorder)
                    .textInputAutocapitalization(.words)
                    .submitLabel(.done)
                    .onSubmit(save)
                    .accessibilityLabel(l10n.t("call.settings.call_name.label"))
                Button(action: save) {
                    Text(l10n.t("call.settings.call_name.save"))
                        .font(.body.weight(.semibold))
                        .frame(minHeight: CallScreenPresentation.minimumTouchTarget)
                }
                .buttonStyle(.borderless)
                .disabled(!canSave)
            }
            if case .invalid(let reason) = verdict {
                Text(reason)
                    .font(.footnote)
                    .foregroundColor(Color.lakkoDanger)
            }
            Text(l10n.t("call.settings.call_name.hint"))
                .font(.footnote)
                .foregroundColor(Color.lakkoTextMuted)
        }
        .onAppear { text = state.ownCallName }
    }

    private var canSave: Bool {
        if case .save = verdict { return true }
        return false
    }

    private func save() {
        guard case .save(let name) = verdict else { return }
        state.saveCallName(name)
        text = name
        let spoken = name.isEmpty
            ? l10n.t("call.settings.call_name.a11y_cleared")
            : l10n.t("call.settings.call_name.a11y_saved", texts: ["name": name])
        UIAccessibility.post(notification: .announcement, argument: spoken)
    }
}
