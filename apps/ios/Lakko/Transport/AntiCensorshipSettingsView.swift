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

    @State private var enabled: Bool

    init(manager: AntiCensorshipManager) {
        self.manager = manager
        self._enabled = State(initialValue: manager.isEnabled)
    }

    var body: some View {
        Form {
            Section {
                Toggle("Hide Lakko from network blocking", isOn: $enabled)
                    .onChange(of: enabled) { _, on in
                        manager.setEnabled(on)
                    }
            } header: {
                Text("Anti-censorship")
            } footer: {
                Text(
                    "When on, Lakko connects in a way that is harder for a network "
                    + "to detect and block. Your messages stay end-to-end encrypted "
                    + "either way."
                )
            }

            Section {
                statusRow
            } header: {
                Text("Status")
            }
        }
        .navigationTitle("Anti-censorship")
        .navigationBarTitleDisplayMode(.inline)
    }

    @ViewBuilder
    private var statusRow: some View {
        switch manager.state {
        case .disabled:
            label("Off", "Direct connection", color: .secondary)
        case .unconfigured:
            label(
                "Not available in this build",
                "This build was made without the setting.",
                color: .lakkoWarning
            )
        case .arming:
            HStack {
                ProgressView()
                Text("Connecting...")
                    .foregroundStyle(.secondary)
            }
        case .armed:
            label("Active", "Connections are hidden from network blocking.", color: .lakkoSuccess)
        case .unavailable(let reason):
            label("Unavailable", reason, color: .red)
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
