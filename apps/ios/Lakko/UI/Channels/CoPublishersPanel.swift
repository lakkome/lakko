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
    @State private var error: String?
    @State private var copied = false
    @State private var pollTask: Task<Void, Never>?
    /// ADR-0053 — how far the knocks have been read (their own id space).
    @State private var knockCursor: Int64 = 0

    private let ds = DsClient()

    var body: some View {
        ChannelManageCard(
            title: "Co-publishers",
            subtitle: "Anyone with the invite link can ask to publish here. You approve each request."
        ) {
            inviteLinkRow

            if !pending.isEmpty {
                Text("Requests")
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
                ChannelManageErrorLine(text: error)
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
                .accessibilityLabel(copied ? "Copied" : "Copy invite link")
                ShareLink(item: link) {
                    Image(systemName: "square.and.arrow.up")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .accessibilityLabel("Share invite link")
            }
            .padding(10)
            .background(Color.lakkoSurfaceSunken)
            .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.control, style: .continuous))
        } else {
            Button {
                Task { await createLink() }
            } label: {
                Label(busy ? "Creating..." : "Create invite link", systemImage: "link")
            }
            .buttonStyle(.bordered)
            .disabled(busy || publisherGroup == nil)
        }
    }

    private func requestRow(index: Int, ref: Data) -> some View {
        HStack(spacing: 8) {
            Text("Request \(index + 1)")
                .font(.body)
                .foregroundStyle(Color.lakkoText)
            Spacer()
            Button("Approve") { Task { await approve(ref) } }
                .buttonStyle(.borderedProminent)
                .tint(Color.lakkoAccent)
                .controlSize(.small)
                .disabled(busy)
            Button("Reject", role: .destructive) { Task { await reject(ref) } }
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
            self.error = LegacyChannelV1.describe(error) { _ in
                "Could not open this channel's publisher settings. Try again later."
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
                knockAfter: knockCursor)
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
            self.error = "Could not create the link. Try again."
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
            self.error = "Could not approve the request. Try again."
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
            self.error = "Could not reject the request. Try again."
        }
    }
}
