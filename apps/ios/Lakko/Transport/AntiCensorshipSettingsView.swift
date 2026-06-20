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
    var onBack: () -> Void

    @State private var enabled: Bool

    init(manager: AntiCensorshipManager, onBack: @escaping () -> Void) {
        self.manager = manager
        self.onBack = onBack
        self._enabled = State(initialValue: manager.isEnabled)
    }

    var body: some View {
        Form {
            Section {
                Toggle("Route through obfuscated tunnel", isOn: $enabled)
                    .onChange(of: enabled) { _, on in
                        manager.setEnabled(on)
                    }
            } header: {
                Text("Anti-censorship")
            } footer: {
                Text(
                    "When on, Lakko fetches a signed network-bootstrap config and "
                    + "routes its API calls through a censorship-resistant transport. "
                    + "Your messages stay end-to-end encrypted either way; this only "
                    + "hides the connection metadata from a network that blocks Lakko."
                )
            }

            Section {
                statusRow
            } header: {
                Text("Status")
            }
        }
        .navigationTitle("Anti-censorship")
        .toolbar {
            ToolbarItem(placement: .navigationBarLeading) {
                Button("Back", action: onBack)
            }
        }
    }

    @ViewBuilder
    private var statusRow: some View {
        switch manager.state {
        case .disabled:
            label("Off", "Direct connection", color: .secondary)
        case .unconfigured:
            label(
                "Not available in this build",
                "No bootstrap key was pinned at build time",
                color: .orange
            )
        case .arming:
            HStack {
                ProgressView()
                Text("Fetching bootstrap config…")
                    .foregroundStyle(.secondary)
            }
        case .armed:
            label("Active", "Routing through the obfuscated tunnel", color: .green)
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
