// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// ADR-0026 Phase 4 — the anti-censorship settings surface.
///
/// One toggle that arms / disarms the obfuscated tunnel, plus a live status
/// line driven by ``AntiCensorshipManager/state``. Flipping the toggle persists
/// the preference and re-runs the boot-arm routine (fetch + verify the signed
/// bootstrap config), so the user sees the outcome (armed / unavailable /
/// not configured) immediately.
struct AntiCensorshipSettingsView: View {
    @ObservedObject var manager: AntiCensorshipManager
    @ObservedObject private var l10n = Localization.shared

    @State private var enabled: Bool

    init(manager: AntiCensorshipManager) {
        self.manager = manager
        self._enabled = State(initialValue: manager.isEnabled)
    }

    var body: some View {
        Form {
            Section {
                Toggle(l10n.t("settings.anti_censorship.toggle"), isOn: $enabled)
                    .onChange(of: enabled) { _, on in
                        manager.setEnabled(on)
                    }
            } header: {
                Text(l10n.t("settings.anti_censorship.title"))
            } footer: {
                Text(l10n.t("settings.anti_censorship.footer"))
            }

            Section {
                statusRow
            } header: {
                Text(l10n.t("settings.anti_censorship.status"))
            }
        }
        .navigationTitle(l10n.t("settings.anti_censorship.title"))
        .navigationBarTitleDisplayMode(.inline)
    }

    @ViewBuilder
    private var statusRow: some View {
        switch manager.state {
        case .disabled:
            label(l10n.t("common.off"), l10n.t("settings.anti_censorship.status.direct"), color: .secondary)
        case .unconfigured:
            label(
                l10n.t("settings.anti_censorship.status.unconfigured"),
                l10n.t("settings.anti_censorship.status.unconfigured.sub"),
                color: .lakkoWarning
            )
        case .arming:
            HStack {
                ProgressView()
                Text(l10n.t("common.connecting"))
                    .foregroundStyle(.secondary)
            }
        case .armed:
            label(
                l10n.t("settings.anti_censorship.status.active"),
                l10n.t("settings.anti_censorship.status.active.sub"),
                color: .lakkoSuccess
            )
        case .unavailable:
            // The failure itself is in the log (ADR-0061: never its prose).
            label(
                l10n.t("settings.anti_censorship.status.unavailable"),
                l10n.t("settings.anti_censorship.status.unavailable.sub"),
                color: .red
            )
        }
    }

    private func label(_ title: String, _ subtitle: String, color: Color) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).foregroundStyle(color)
            Text(subtitle)
                .font(.footnote)
                .foregroundStyle(.secondary)
        }
    }
}
