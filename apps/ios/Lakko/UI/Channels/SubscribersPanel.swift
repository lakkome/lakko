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
    /// The canonical AccountIdentity seed: the subscriber list is
    /// owner-signed with the key it derives for this channel (ADR-0059).
    let accountIdentitySeed: Data

    @State private var subscribers: [String]?
    @State private var busyPk: String?
    @State private var error: String?
    private enum Status { case delivered, auto, failed }
    @State private var statusByPk: [String: Status] = [:]
    @State private var pollTask: Task<Void, Never>?

    var body: some View {
        ChannelManageCard(
            title: countedTitle,
            subtitle: "Each subscriber is granted access to new posts automatically."
        ) {
            if let error {
                ChannelManageErrorLine(text: error)
            }
            if let subs = subscribers {
                if subs.isEmpty {
                    Text("No subscribers yet.")
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
                Text("Loading...")
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
        guard let count = subscribers?.count else { return "Subscribers" }
        return "Subscribers (\(count))"
    }

    /// A subscriber is known only by its key, which is not a name; the row
    /// says "Subscriber N" rather than showing the key.
    private func subscriberRow(_ pk: String, ordinal: Int) -> some View {
        HStack(alignment: .center, spacing: 10) {
            Text("Subscriber \(ordinal)")
                .font(.body)
                .foregroundStyle(Color.lakkoText)

            if let st = statusByPk[pk] {
                let (label, color): (String, Color) = {
                    switch st {
                    case .delivered, .auto: return ("Has access", Color.lakkoSuccess)
                    case .failed: return ("Failed", Color.lakkoDanger)
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
                Text(busyPk == pk ? "Granting..." : "Grant access")
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
            let identity = try Identity.fromSeed(seed: accountIdentitySeed)
            let list = try await client.listSubscribers(
                channelIdHex: channelId,
                accountIdentity: identity
            )
            subscribers = list
            state.setSubscriberCount(channelId: channelId, count: list.count)
        } catch {
            NSLog("Channels: subscriber list failed: %@", String(describing: error))
            self.error = "Could not load subscribers. They reload automatically."
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
            error = "This device cannot grant access here."
            return
        }
        if trigger == .manual {
            busyPk = pubkeyHex
            error = nil
        }
        defer { if trigger == .manual { busyPk = nil } }
        do {
            guard let subPub = Data(channelHex: pubkeyHex) else {
                if trigger == .manual { error = "Could not grant access to this subscriber." }
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
                self.error = "Could not grant access. Try again."
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
