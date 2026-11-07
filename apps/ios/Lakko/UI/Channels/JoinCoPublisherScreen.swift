// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import SwiftUI

/// ADR-0028 OQ3 — joiner side: paste a co-publisher invite link, ask to
/// join, wait until the owner approves, then mint local publisher keys and
/// open the channel. Mirror of the web `onJoinAsCoPublisher` flow.
struct JoinCoPublisherScreen: View {
    @ObservedObject var state: ChannelsUiState
    let accountIdentitySeed: Data
    @Environment(\.dismiss) private var dismiss

    @State private var link: String = ""
    @State private var localName: String = ""
    @State private var busy = false
    /// The catalog key of the progress line, rendered when shown.
    @State private var status: String?
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared
    /// ADR-0053 — the join failure (same copy and Try-again rule as the
    /// group join screen). The raw error is logged, never shown.
    @State private var failure: CoPublisherJoin.Failure?

    private let ds = DsClient()

    var body: some View {
        Form {
            Section {
                TextField(l10n.t("channels.join.link_label"), text: $link, axis: .vertical)
                    .lineLimit(2...4)
                    .autocorrectionDisabled()
                    .textInputAutocapitalization(.never)
                TextField(l10n.t("channels.join.name_label"), text: $localName)
            } footer: {
                Text(l10n.t("channels.join.subtitle"))
            }
            if let status {
                Section {
                    HStack(spacing: 10) {
                        ProgressView()
                        Text(l10n.t(status))
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                }
            }
            if let failure {
                Section {
                    Text(failure.message.text)
                        .foregroundStyle(Color.lakkoDanger)
                    if failure.offersRetry {
                        Button(joinTryAgain) {
                            Task { await join() }
                        }
                        .disabled(busy)
                    }
                }
            }
        }
        .scrollContentBackground(.hidden)
        .background(Color.lakkoBackground)
        .navigationTitle(l10n.t("channels.join.title"))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button(l10n.t(busy ? "channels.join.requesting" : "group.join.request")) {
                    Task { await join() }
                }
                .disabled(busy || link.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
    }

    private func join() async {
        if busy { return }
        let trimmed = link.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty { return }
        busy = true
        failure = nil
        defer {
            busy = false
            status = nil
        }
        do {
            _ = try await CoPublisherJoin.run(
                link: trimmed, localName: localName, state: state, accountIdentitySeed: accountIdentitySeed,
                ds: ds, onStage: { stage in
                    status = stage == .sending ? "group.join.sending" : "channels.join.waiting"
                })
            dismiss()
        } catch let failed as CoPublisherJoin.Failure {
            failure = failed
        } catch {
            failure = .init(message: .key("group.join.request_failed"), offersRetry: true)
        }
    }
}
