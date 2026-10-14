// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import SwiftUI

/// The call's settings (`docs/ux/calls.md` section 4.1): which microphone,
/// camera and speaker the call uses.
///
/// The app's own lists, never a system picker and never the system's route
/// sheet. Each section is a group of radio rows, the system default first.
/// A choice applies at once to the live call and holds for the call.
///
/// Which sections and rows exist is ``CallSettingsModel``; what the platform
/// offers and how a choice is applied is ``CallDevices``. This file draws.
struct CallSettingsSheet: View {
    @ObservedObject var state: CallUiState
    @ObservedObject var l10n = Localization.shared
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

            if sections.isEmpty {
                Text(l10n.t("call.settings.no_devices"))
                    .font(.subheadline)
                    .foregroundColor(Color.lakkoTextMuted)
                Spacer(minLength: 0)
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 18) {
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
