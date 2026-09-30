// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import SwiftUI

/// Auto-fanout poll cadence — every 5 s the publisher panel re-fetches the
/// subscriber list, diffs against the already-delivered set, and auto-seals
/// an envelope for each new pubkey. Same cadence the reader-side poll uses.
private let subscribersPollIntervalNs: UInt64 = 5_000_000_000

/// Publisher-side fan-out card (``ChannelManageScreen``): the subscriber
/// list as "Subscriber N" rows with their access state, plus a manual grant.
/// The auto fan-out loop is the one the old detail panel ran.
struct SubscribersPanel: View {
    let channelId: String
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient

    @State private var subscribers: [String]?
    @State private var busyPk: String?
    @State private var error: UiText?
    /// ADR-0058 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared
    private enum Status { case delivered, auto, failed }
    @State private var statusByPk: [String: Status] = [:]
    @State private var pollTask: Task<Void, Never>?

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

            if let st = statusByPk[pk] {
                let (label, color): (String, Color) = {
                    switch st {
                    case .delivered, .auto:
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

    private func refresh() async {
        do {
            let list = try await client.listSubscribers(channelIdHex: channelId)
            subscribers = list
            state.setSubscriberCount(channelId: channelId, count: list.count)
        } catch {
            NSLog("Channels: subscriber list failed: %@", String(describing: error))
            self.error = .key("channels.subscribers.error.load")
        }
    }

    private enum DeliverTrigger { case manual, auto }

    private func deliverTo(
        _ pubkeyHex: String,
        trigger: DeliverTrigger = .manual
    ) async {
        // Manual delivery is one-at-a-time (single busy spinner across the
        // panel). The auto path ignores `busyPk` so a slow manual tap on one
        // subscriber does not stall the polling loop's progress on others.
        if trigger == .manual && busyPk != nil { return }
        guard let keys = state.keysByChannel[channelId] else {
            error = .key("channels.publish.error.not_publisher")
            return
        }
        if trigger == .manual {
            busyPk = pubkeyHex
            error = nil
        }
        defer { if trigger == .manual { busyPk = nil } }
        do {
            guard let subPub = Data(channelHex: pubkeyHex) else {
                if trigger == .manual { error = .key("channels.subscribers.error.deliver") }
                statusByPk[pubkeyHex] = .failed
                return
            }
            let epoch = state.epochByChannel[channelId] ?? 0
            let wire = try ChannelsComposer.sealSubscriberSeedEnvelope(
                subscriberPub: subPub,
                channelIdHex: channelId,
                epochId: epoch,
                seed: keys.seed
            )
            // ADR-0031: file the envelope under the exact epoch we sealed it
            // for, so a catching-up subscriber lands it at the right key in
            // its `epoch -> seed` map.
            try await client.deliverEnvelope(
                channelIdHex: channelId,
                subscriberPubkeyHex: pubkeyHex,
                envelope: wire,
                epoch: epoch
            )
            statusByPk[pubkeyHex] = trigger == .auto ? .auto : .delivered
        } catch {
            NSLog("Channels: grant access failed: %@", String(describing: error))
            if trigger == .manual {
                self.error = ChannelsErrorCopy.text(
                    for: error, prefixKey: "channels.subscribers.error.deliver_prefix",
                    fallbackKey: "channels.subscribers.error.deliver")
            }
            statusByPk[pubkeyHex] = .failed
        }
    }

    /// Start the auto-fanout polling loop. Every `subscribersPollIntervalNs`:
    /// refresh the list, then auto-deliver to any pubkey we have not
    /// successfully delivered to yet. Cancellable via `pollTask?.cancel()`
    /// in `.onDisappear`.
    private func startAutoFanoutLoop() {
        pollTask?.cancel()
        pollTask = Task {
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: subscribersPollIntervalNs)
                if Task.isCancelled { return }
                await refresh()
                guard let list = subscribers else { continue }
                // Sequence the auto-deliveries — UniFFI ChannelSigningKey /
                // ChannelSubscriberKeypair calls each hit the Rust runtime
                // serially anyway, and serial avoids a burst of N parallel
                // seals on a fresh subscriber storm.
                for pk in list {
                    if Task.isCancelled { return }
                    let st = statusByPk[pk]
                    if st == nil || st == .failed {
                        await deliverTo(pk, trigger: .auto)
                    }
                }
            }
        }
    }
}
