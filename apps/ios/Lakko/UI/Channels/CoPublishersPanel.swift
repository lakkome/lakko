// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import SwiftUI
import UIKit

/// ADR-0028 OQ3 — owner-side co-publisher card (the Publishers screen).
/// Runs on the durable channel provider (``ChannelPublisherIdentity``): the
/// pending set the poll loop fills is in that provider, so the approve finds
/// it, and every action reopens the group from it rather than keeping a
/// handle. Polls the journal and lists each request that did not come by a
/// publisher link this account named someone with, with Approve / Reject. The approve path is
/// `CoPublisher.approve`, recorded (G-50) before its reseal-after-add runs.
struct CoPublishersPanel: View {
    let channelId: String
    /// G-50 — where the post-approve content seed is written back.
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient
    let accountIdentitySeed: Data

    /// `true` once the PublisherGroup opened.
    @State private var ready = false
    @State private var pending: [Data] = []
    @State private var busy = false
    @State private var error: UiText?
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared
    @State private var pollTask: Task<Void, Never>?
    /// ADR-0053 — how far the knocks have been read (their own id space).
    @State private var knockCursor: Int64 = 0
    @State private var journalCursor: Int64 = 0
    /// ADR-0053 — the same per-group floor on knock reads the chat poll keeps;
    /// the journal is read from its cursor every 10 s while the list is open.
    @State private var knockThrottle = KnockPollThrottle(floorSeconds: KnockPollThrottle.backgroundFloorSeconds)

    private let ds = DsClient()

    var body: some View {
        // UX 15.2 — the copy-paste link is gone (`Add publisher` names the
        // person, and their request is approved without asking); a request
        // that came by any other link still shows here.
        Group {
            if !pending.isEmpty || error != nil {
                card
            }
        }
        .task { await onAppearLoad() }
        .onDisappear { teardown() }
    }

    private var card: some View {
        ChannelManageCard(
            title: l10n.t("channels.copublishers.title"),
            subtitle: l10n.t("channels.copublishers.desc")
        ) {
            if !pending.isEmpty {
                Text(l10n.t(
                    "channels.copublishers.requests",
                    counts: ["count": UInt64(pending.count)]))
                    .font(.subheadline)
                    .fontWeight(.medium)
                    .foregroundStyle(Color.lakkoText)
                    .padding(.top, 4)
                VStack(spacing: 0) {
                    ForEach(Array(pending.enumerated()), id: \.element) { index, ref in
                        requestRow(index: index, ref: ref)
                        if index < pending.count - 1 {
                            Divider().overlay(Color.lakkoBorder)
                        }
                    }
                }
            }

            if let error {
                ChannelManageErrorLine(text: error.text)
            }
        }
    }

    private func requestRow(index: Int, ref: Data) -> some View {
        HStack(spacing: 8) {
            Text(l10n.t("channels.copublishers.request_row"))
                .font(.body)
                .foregroundStyle(Color.lakkoText)
            Spacer()
            Button(l10n.t("chat.members.join.approve")) { Task { await approve(ref) } }
                .buttonStyle(.borderedProminent)
                .tint(Color.lakkoAccent)
                .controlSize(.small)
                .disabled(busy)
            Button(l10n.t("chat.members.join.reject"), role: .destructive) { Task { await reject(ref) } }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(busy)
        }
        .padding(.vertical, 8)
    }

    /// The channel provider identity and the channel's PublisherGroup.
    private func open() throws -> (identity: Identity, group: PublisherGroup) {
        let identity = try ChannelPublisherIdentity.shared.identity(currentSeed: accountIdentitySeed)
        guard let group = try CoPublisher.loadPublisherGroup(identity: identity, channelIdHex: channelId) else {
            throw CoPublisherError.badChannelId(channelId)
        }
        return (identity, group)
    }

    private func onAppearLoad() async {
        do {
            let id = try ChannelPublisherIdentity.shared.identity(currentSeed: accountIdentitySeed)
            let pg = try CoPublisher.openPublisherGroup(
                identity: id, accountIdB64: AcceptedContactsStore.boundAccountIdB64,
                channelIdHex: channelId)
            ChannelPublisherIdentity.shared.persist()
            ready = true
            // ADR-0053 point 2 — claim-on-open (the claim at creation included).
            await CoPublisher.claimJournal(ds: ds, identity: id, publisherGroup: pg)
        } catch {
            NSLog("Channels: open publisher group failed: %@", String(describing: error))
            self.error = UiText {
                LegacyChannelV1.describe(error) { _ in Localization.text("channels.manage.error.open") }
            }
            return
        }
        startPollLoop()
    }

    private func teardown() {
        pollTask?.cancel()
        pollTask = nil
    }

    private func startPollLoop() {
        pollTask?.cancel()
        pollTask = Task { @MainActor in
            while !Task.isCancelled {
                await refreshPending()
                try? await Task.sleep(nanoseconds: 10 * 1_000_000_000)
            }
        }
    }

    private func refreshPending() async {
        guard ready, let opened = try? open() else { return }
        let identity = opened.identity
        let publisherGroup = opened.group
        do {
            let outcome = try await CoPublisher.pollJournal(
                ds: ds, identity: identity, publisherGroup: publisherGroup,
                knockAfter: knockCursor,
                readKnocks: knockThrottle.take(channelId),
                after: journalCursor)
            knockCursor = outcome.knockCursor
            journalCursor = outcome.journalCursor
            ChannelAdmissionSeedShare.rememberRequests(outcome.requests, channelId: channelId)
            if let groupId = try? publisherGroup.groupId() {
                PublisherLinkApproval.remember(outcome.requests, groupId: groupId)
            }
            // The poll applied commits and retained requests in the provider.
            ChannelPublisherIdentity.shared.persist()
            pending = try CoPublisher.pending(publisherGroup: publisherGroup)
        } catch {
            // Best-effort — keep the last good pending list.
        }
    }

    private func approve(_ ref: Data) async {
        guard ready, let accountIdB64 = AcceptedContactsStore.boundAccountIdB64 else { return }
        busy = true
        error = nil
        defer { busy = false }
        do {
            try await CoPublisher.approve(
                context: ChannelPublisherCommits.Context(
                    channelIdHex: channelId, currentSeed: accountIdentitySeed,
                    accountIdB64: accountIdB64, channelsClient: client, ds: ds),
                state: state, proposalRef: ref)
            await refreshPending()
        } catch {
            NSLog("Channels: approve co-publisher failed: %@", String(describing: error))
            self.error = .key("channels.copublishers.error.approve")
        }
    }

    private func reject(_ ref: Data) async {
        guard ready else { return }
        busy = true
        error = nil
        defer { busy = false }
        do {
            let (identity, publisherGroup) = try open()
            try CoPublisher.reject(
                identity: identity, publisherGroup: publisherGroup, proposalRef: ref)
            ChannelPublisherIdentity.shared.persist()
            await refreshPending()
        } catch {
            NSLog("Channels: reject co-publisher failed: %@", String(describing: error))
            self.error = .key("channels.copublishers.error.reject")
        }
    }
}
