// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import SwiftUI

/// While Manage is open the list and its access words are re-read every
/// 5 s, faster than the session-long ``SubscriberAdmission`` loop (15 s); both
/// run the same pass over the same shared state.
private let subscribersPollIntervalNs: UInt64 = 5_000_000_000

/// Publisher-side fan-out card (``ChannelManageScreen``): the subscriber
/// list as "Subscriber N" rows with their access state, plus a manual grant.
/// Letting people in is ``SubscriberAdmission``, shared with the app-wide
/// loop, so a new subscriber is let in while the app is open whatever screen
/// is showing; this card shows that state and polls it faster.
struct SubscribersPanel: View {
    let channelId: String
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient
    /// The canonical AccountIdentity seed: the subscriber list is
    /// owner-signed with the key it derives for this channel (ADR-0059).
    let accountIdentitySeed: Data

    @ObservedObject private var admission = SubscriberAdmission.shared
    @State private var busyPk: String?
    @State private var error: UiText?
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared
    @State private var pollTask: Task<Void, Never>?

    private var subscribers: [String]? {
        admission.admission(channelId: channelId).subscribers
    }

    var body: some View {
        ChannelManageCard(
            title: countedTitle,
            subtitle: l10n.t("channels.subscribers.desc")
        ) {
            if let error {
                ChannelManageErrorLine(text: error.text)
            }
            if let subs = subscribers {
                if subs.isEmpty {
                    Text(l10n.t("channels.subscribers.empty"))
                        .font(.subheadline)
                        .foregroundStyle(Color.lakkoTextMuted)
                } else {
                    VStack(spacing: 0) {
                        ForEach(Array(subs.enumerated()), id: \.element) { index, pk in
                            subscriberRow(pk, ordinal: index + 1)
                            if index < subs.count - 1 {
                                Divider().overlay(Color.lakkoBorder)
                            }
                        }
                    }
                }
            } else {
                Text(l10n.t("common.loading"))
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
        }
        .task {
            await refresh()
            startAutoFanoutLoop()
        }
        .onDisappear { pollTask?.cancel() }
    }

    private var countedTitle: String {
        let title = l10n.t("channels.subscribers.title")
        guard let count = subscribers?.count else { return title }
        return l10n.t("common.sentence_pair", texts: [
            "first": title,
            "second": l10n.t("channels.subscribers.count", counts: ["count": UInt64(count)]),
        ])
    }

    /// A subscriber is known only by its key, which is not a name; the row
    /// says "Subscriber N" rather than showing the key.
    private func subscriberRow(_ pk: String, ordinal: Int) -> some View {
        HStack(alignment: .center, spacing: 10) {
            Text(l10n.t("channels.subscribers.row", texts: ["number": String(ordinal)]))
                .font(.body)
                .foregroundStyle(Color.lakkoText)

            if let st = admission.status(channelId: channelId, pubkeyHex: pk, state: state) {
                let (label, color): (String, Color) = {
                    switch st {
                    case .ready:
                        return (l10n.t("channels.subscribers.status.ready"), Color.lakkoSuccess)
                    case .failed:
                        return (l10n.t("channels.subscribers.status.not_ready"), Color.lakkoDanger)
                    }
                }()
                Text(label)
                    .font(.caption)
                    .foregroundStyle(color)
            }

            Spacer(minLength: 4)

            Button {
                Task { await deliverTo(pk) }
            } label: {
                Text(l10n.t(busyPk == pk ? "channels.subscribers.retrying" : "common.retry"))
                    .font(.footnote)
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(busyPk == pk)
        }
        .padding(.vertical, 10)
        .accessibilityElement(children: .combine)
    }

    /// One shared admission pass: re-read the list and let in every key not
    /// yet ready at the current epoch.
    private func refresh() async {
        do {
            try await admission.admitNewSubscribers(
                channelId: channelId,
                state: state,
                accountIdentitySeed: accountIdentitySeed
            )
            error = nil
        } catch {
            // The panel was left mid-load: not a failure of the list.
            if Cancellation.endedByCancellation(error) { return }
            NSLog("Channels: subscriber list failed: %@", String(describing: error))
            self.error = .key("channels.subscribers.error.load")
        }
    }

    /// Manual grant for one subscriber, through the shared single-key admit.
    /// One at a time (a single busy spinner across the panel).
    private func deliverTo(_ pubkeyHex: String) async {
        if busyPk != nil { return }
        busyPk = pubkeyHex
        error = nil
        defer { busyPk = nil }
        do {
            let identity = try Identity.fromSeed(seed: accountIdentitySeed)
            try await admission.admit(
                channelId: channelId, pubkeyHex: pubkeyHex, state: state, accountIdentity: identity)
        } catch SubscriberAdmission.AdmissionError.noKeys {
            error = .key("channels.publish.error.not_publisher")
        } catch SubscriberAdmission.AdmissionError.badKey {
            error = .key("channels.subscribers.error.deliver")
        } catch {
            NSLog("Channels: grant access failed: %@", String(describing: error))
            self.error = ChannelsErrorCopy.text(
                for: error, prefixKey: "channels.subscribers.error.deliver_prefix",
                fallbackKey: "channels.subscribers.error.deliver")
        }
    }

    /// Re-run the shared pass every `subscribersPollIntervalNs` while the
    /// card is on screen. Cancelled in `.onDisappear`.
    private func startAutoFanoutLoop() {
        pollTask?.cancel()
        pollTask = Task {
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: subscribersPollIntervalNs)
                if Task.isCancelled { return }
                await refresh()
            }
        }
    }
}
