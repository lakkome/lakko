// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// Settings > About > Supporters (`docs/ux/supporters.md` section 2.2): the
/// thank-you line, then the names in file order, one per line, plain text.
struct SupportersScreen: View {
    @ObservedObject var center: SupportersListCenter
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        List {
            Section {
                Text(l10n.t("supporters.body"))
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            switch center.screenState {
            case .loading:
                Section {
                    HStack {
                        Spacer()
                        ProgressView()
                        Spacer()
                    }
                }
            case .failed:
                Section {
                    Text(l10n.t("supporters.failed"))
                        .foregroundStyle(Color.lakkoText)
                    Button(l10n.t("supporters.retry")) {
                        Task { await center.retry() }
                    }
                    .foregroundStyle(Color.lakkoAccent)
                }
            case .names(let names):
                Section {
                    ForEach(Array(names.enumerated()), id: \.offset) { _, name in
                        Text(verbatim: name)
                            .foregroundStyle(Color.lakkoText)
                    }
                }
            }
        }
        .navigationTitle(l10n.t("supporters.title"))
        .task {
            // Opened before any fetch, nothing held: fetch now.
            if center.held == nil, center.phase != .loading {
                await center.retry()
            }
        }
    }
}
