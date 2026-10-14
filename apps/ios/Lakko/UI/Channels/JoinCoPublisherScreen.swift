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
    /// A readable message that is not one of the shared request failures (a
    /// retired-derivation channel, an invalid link, an owner who has not yet
    /// approved), or `nil`.
    @State private var errorMessage: UiText?
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared
    /// ADR-0053 — the shared join-request failure (same copy and Try-again
    /// rule as the group join screen). The raw error is logged, never shown.
    @State private var failure: JoinRequestFailure?

    private let ds = DsClient()

    /// Number of poll attempts (every 3 s) before giving up — mirrors the web
    /// 60 x 3 s window.
    private static let pollAttempts = 60
    private static let pollIntervalNs: UInt64 = 3 * 1_000_000_000

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
                    Text(failure.message)
                        .foregroundStyle(Color.lakkoDanger)
                    if failure.offersRetry {
                        Button(joinTryAgain) {
                            Task { await join() }
                        }
                        .disabled(busy)
                    }
                }
            } else if let errorMessage {
                Section {
                    Text(errorMessage.text)
                        .foregroundStyle(Color.lakkoDanger)
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
        errorMessage = nil
        failure = nil
        status = "group.join.sending"
        // The account this request is made for, before any wait: the
        // co-publisher mark is filed under it, and under no account signed in
        // while the request was out.
        let accountIdB64 = AcceptedContactsStore.boundAccountIdB64
        defer {
            busy = false
            status = nil
        }
        do {
            let identity = try Identity.fromSeed(seed: accountIdentitySeed)
            let request = try await CoPublisher.requestToCoPublish(
                ds: ds, identity: identity, accountIdB64: accountIdB64, link: trimmed)
            status = "channels.join.waiting"
            var keys: ChannelPublisherKeys?
            for _ in 0..<Self.pollAttempts where keys == nil {
                keys = try await CoPublisher.pollForCoPublishWelcome(
                    ds: ds,
                    identity: identity,
                    accountIdB64: accountIdB64,
                    groupId: request.groupId,
                    joinerKey: request.joinerKey,
                    channelIdHex: request.channelIdHex,
                    // G-29 — reuse the persisted signing seed on a re-poll.
                    existingSigningSeed: state.persistedPublisherSigningSeed(
                        channelId: request.channelIdHex)
                )
                if keys == nil {
                    try? await Task.sleep(nanoseconds: Self.pollIntervalNs)
                }
            }
            guard let keys else {
                errorMessage = .key("channels.join.no_approval")
                return
            }
            let name = localName.trimmingCharacters(in: .whitespaces)
            state.addChannel(.init(
                id: request.channelIdHex,
                localName: name,
                localDescription: "",
                addedAtMs: UInt64(Date().timeIntervalSince1970 * 1000)
            ))
            state.setKeys(channelId: request.channelIdHex, keys: keys)
            dismiss()
        } catch let err as LegacyChannelError {
            errorMessage = UiText { err.message }
        } catch let err as CoPublisherError {
            // Not a co-publisher link at all: the link itself is the problem,
            // which no retry fixes — the invite page's invalid-link copy.
            NSLog("Channels: co-publish link rejected: %@", String(describing: err))
            errorMessage = UiText { groupInviteInvalidMessage }
        } catch {
            NSLog("Channels: co-publish request failed: %@", String(describing: error))
            failure = JoinRequestFailure.classify(error)
        }
    }
}
