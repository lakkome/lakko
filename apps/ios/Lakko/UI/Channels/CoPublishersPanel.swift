// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import SwiftUI
import UIKit

/// ADR-0028 OQ3 — owner-side co-publisher card (``ChannelManageScreen``).
/// Holds a long-lived PublisherGroup + Identity (the approve path needs the
/// same handle the poll loop populated the pending set on). Opens both on
/// appear, polls the journal every 5 s, and surfaces the invite link
/// (Copy / Share) and each request with Approve / Reject. The approve path is
/// `CoPublisher.approve`, whose reseal-after-add is untouched.
struct CoPublishersPanel: View {
    let channelId: String
    let client: ChannelsClient
    let accountIdentitySeed: Data

    @State private var identity: Identity?
    @State private var publisherGroup: PublisherGroup?
    @State private var link: String?
    @State private var pending: [Data] = []
    @State private var busy = false
    @State private var error: UiText?
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared
    @State private var copied = false
    @State private var pollTask: Task<Void, Never>?
    /// ADR-0053 — how far the knocks have been read (their own id space).
    @State private var knockCursor: Int64 = 0
    /// ADR-0053 — the same per-group floor on knock reads the chat poll keeps;
    /// the journal itself is still polled every 5 s.
    @State private var knockThrottle = KnockPollThrottle()

    private let ds = DsClient()

    var body: some View {
        ChannelManageCard(
            title: l10n.t("channels.copublishers.title"),
            subtitle: l10n.t("channels.copublishers.desc")
        ) {
            inviteLinkRow

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
        .task { await onAppearLoad() }
        .onDisappear { teardown() }
    }

    /// The invite link as one row: create it, then Copy / Share it.
    @ViewBuilder
    private var inviteLinkRow: some View {
        if let link {
            HStack(spacing: 8) {
                Image(systemName: "link")
                    .foregroundStyle(Color.lakkoTextMuted)
                Text(link)
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoText)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer(minLength: 4)
                Button {
                    UIPasteboard.general.string = link
                    copied = true
                    Task {
                        try? await Task.sleep(nanoseconds: 1_500_000_000)
                        copied = false
                    }
                } label: {
                    Image(systemName: copied ? "checkmark" : "doc.on.doc")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .accessibilityLabel(l10n.t(copied ? "common.copied" : "chat.invite_link.copy"))
                // Copy is the whole of it: a link is never handed to the
                // system's share sheet.
            }
            .padding(10)
            .background(Color.lakkoSurfaceSunken)
            .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.control, style: .continuous))
        } else {
            Button {
                Task { await createLink() }
            } label: {
                Label(l10n.t(busy ? "chats.new_group.creating" : "chat.invite_link.create"), systemImage: "link")
            }
            .buttonStyle(.bordered)
            .disabled(busy || publisherGroup == nil)
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

    private func onAppearLoad() async {
        do {
            let id = try Identity.fromSeed(seed: accountIdentitySeed)
            let pg = try CoPublisher.openPublisherGroup(identity: id, channelIdHex: channelId)
            identity = id
            publisherGroup = pg
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
                try? await Task.sleep(nanoseconds: 5 * 1_000_000_000)
            }
        }
    }

    private func refreshPending() async {
        guard let identity, let publisherGroup else { return }
        do {
            let outcome = try await CoPublisher.pollJournal(
                ds: ds, identity: identity, publisherGroup: publisherGroup,
                knockAfter: knockCursor,
                readKnocks: knockThrottle.take(channelId))
            knockCursor = outcome.knockCursor
            pending = try CoPublisher.pending(publisherGroup: publisherGroup)
        } catch {
            // Best-effort — keep the last good pending list.
        }
    }

    private func createLink() async {
        guard let identity, let publisherGroup else { return }
        busy = true
        error = nil
        defer { busy = false }
        do {
            link = try CoPublisher.createCoPublisherLink(
                identity: identity, publisherGroup: publisherGroup)
        } catch {
            NSLog("Channels: create co-publisher link failed: %@", String(describing: error))
            self.error = .key("chat.error.create_link")
        }
    }

    private func approve(_ ref: Data) async {
        guard let identity, let publisherGroup else { return }
        busy = true
        error = nil
        defer { busy = false }
        do {
            try await CoPublisher.approve(
                ds: ds, channelsClient: client, identity: identity,
                publisherGroup: publisherGroup, proposalRef: ref, channelIdHex: channelId)
            await refreshPending()
        } catch {
            NSLog("Channels: approve co-publisher failed: %@", String(describing: error))
            self.error = .key("channels.copublishers.error.approve")
        }
    }

    private func reject(_ ref: Data) async {
        guard let identity, let publisherGroup else { return }
        busy = true
        error = nil
        defer { busy = false }
        do {
            try CoPublisher.reject(
                identity: identity, publisherGroup: publisherGroup, proposalRef: ref)
            await refreshPending()
        } catch {
            NSLog("Channels: reject co-publisher failed: %@", String(describing: error))
            self.error = .key("channels.copublishers.error.reject")
        }
    }
}
