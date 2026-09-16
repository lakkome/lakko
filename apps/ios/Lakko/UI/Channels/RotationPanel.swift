// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import SwiftUI

/// Scheduled content-key rotation (ADR-0010 / C-54) as the "Access" card of
/// ``ChannelManageScreen``: one plain status line and a manual refresh.
/// Starts the `RotationCron` heartbeat while the screen is visible.
///
/// ADR-0010 §"PublisherGroup" — the *scheduled* half of content-key rotation
/// (the membership-change half advances automatically on every MLS epoch
/// bump). The pure `channelShouldRotate` policy (default 24 h / 1000 posts)
/// decides *when*; `RotationCron` does *how* — advance the PublisherGroup
/// epoch, derive the new seed, and reseal it to every subscriber via the
/// atomic rotate fanout (the same audited path `CoPublisher.approve` reuses
/// after adding a co-publisher).
///
/// **Fail-closed (invariant #4).** A rotation that fails part-way never
/// exposes plaintext or hands out a bad key: the reseal batch is atomic
/// server-side, a single seal failure aborts the whole batch, and a failed
/// fanout simply leaves subscribers unable to decrypt the *next* post until
/// the next successful reseal — an availability effect, never a downgrade.
/// The `RotationCron` already carries these semantics; this view only drives
/// its lifecycle + surfaces the error.
struct RotationPanel: View {
    let channelId: String
    let client: ChannelsClient
    /// Canonical AccountIdentity (phrase) seed — used to open the channel's
    /// persistent PublisherGroup (same handle ``CoPublishersPanel`` opens).
    let accountIdentitySeed: Data

    @StateObject private var model = RotationPanelModel()

    var body: some View {
        ChannelManageCard(
            title: "Access",
            subtitle: "Someone who leaves cannot read what is posted after the next refresh."
        ) {
            Text(model.status)
                .font(.subheadline)
                .foregroundStyle(Color.lakkoText)
                .fixedSize(horizontal: false, vertical: true)

            Button {
                Task { await model.rotateNow() }
            } label: {
                Label(model.busy ? "Refreshing..." : "Refresh access now",
                      systemImage: "arrow.triangle.2.circlepath")
            }
            .buttonStyle(.bordered)
            .disabled(model.busy || !model.ready)

            if let error = model.error {
                ChannelManageErrorLine(text: error)
            }
        }
        .task {
            await model.load(
                channelId: channelId,
                client: client,
                accountIdentitySeed: accountIdentitySeed
            )
        }
        .onDisappear { model.teardown() }
    }
}

/// Reactive owner of the rotation lifecycle for one manage screen. A
/// reference type (`ObservableObject`) so the long-lived `RotationCron`
/// callbacks can capture it safely — a SwiftUI `View` struct value cannot be
/// mutated from an escaping closure stored on the cron actor.
@MainActor
final class RotationPanelModel: ObservableObject {
    /// The one plain status line (``ChannelsFormatting/accessRefreshLine``).
    @Published var status: String = "Loading..."
    /// Surfaced when opening the group or a manual rotation fails.
    @Published var error: String?
    /// `true` while a manual refresh is in flight.
    @Published var busy = false
    /// `true` once the PublisherGroup + cron are ready (enables the button).
    @Published var ready = false

    private var channelIdHex = ""
    private var client = ChannelsClient()
    private var identity: Identity?
    private var publisherGroup: PublisherGroup?
    private var cron: RotationCron?
    private var cronTask: Task<Void, Never>?

    /// Open the channel's PublisherGroup once, then (re)start the periodic
    /// heartbeat. Idempotent across screen re-appears: the group is opened
    /// only on the first call; the heartbeat is restarted every time (it is
    /// cancelled in `teardown` on disappear).
    func load(
        channelId: String,
        client: ChannelsClient,
        accountIdentitySeed: Data
    ) async {
        self.channelIdHex = channelId
        self.client = client
        if cron == nil {
            do {
                let id = try Identity.fromSeed(seed: accountIdentitySeed)
                let pg = try CoPublisher.openPublisherGroup(
                    identity: id, channelIdHex: channelId)
                identity = id
                publisherGroup = pg
                cron = makeCron(identity: id, publisherGroup: pg)
                ready = true
            } catch {
                NSLog("Channels: open publisher group failed: %@", String(describing: error))
                self.error = LegacyChannelV1.describe(error) { _ in
                    "Could not open this channel's publisher settings. Try again later."
                }
                return
            }
        }
        refreshStatus()
        startPeriodic()
    }

    /// User-initiated rotation — forces a rotation regardless of the schedule
    /// (e.g. after suspecting a subscriber compromise). Drives the same
    /// `runOnce` path the heartbeat uses; a synthetic "never rotated" state
    /// + a 1 ms interval satisfies `channelShouldRotate` so the gate always
    /// fires, then `runOnce` stamps + persists a fresh post-rotation state
    /// via the context's `saveState`.
    func rotateNow() async {
        guard let cron else { return }
        if busy { return }
        busy = true
        error = nil
        defer { busy = false }
        do {
            _ = try await cron.runOnce(
                policy: ChannelRotationPolicy(intervalMs: 1, maxPosts: 1),
                state: ChannelRotationState(lastRotationMs: 0, postsSinceRotation: 0)
            )
            // The `onRotated` callback refreshes the status line on success;
            // refresh here too so a no-subscriber rotation (still bumps the
            // epoch) is reflected immediately.
            refreshStatus()
        } catch {
            NSLog("Channels: manual rotation failed: %@", String(describing: error))
            self.error = "Could not refresh access. Try again."
        }
    }

    /// Cancel the heartbeat when the screen disappears. The PublisherGroup +
    /// cron handles are retained so a re-appear restarts the heartbeat
    /// without re-opening the group.
    func teardown() {
        cronTask?.cancel()
        cronTask = nil
    }

    // MARK: - internals

    private func makeCron(
        identity: Identity,
        publisherGroup: PublisherGroup
    ) -> RotationCron {
        let channelId = self.channelIdHex
        let context = RotationCron.Context(
            publisherGroup: publisherGroup,
            identity: identity,
            channelIdHex: channelId,
            channelsClient: client,
            saveState: { newState in
                ChannelRotationStateStore.save(channelId: channelId, state: newState)
            },
            onRotated: { [weak self] epoch in
                Task { @MainActor in self?.handleRotated(epoch) }
            }
        )
        return RotationCron(context: context)
    }

    private func startPeriodic() {
        guard let cron else { return }
        cronTask?.cancel()
        let channelId = self.channelIdHex
        cronTask = cron.start(
            policy: channelRotationPolicyDefault(),
            loadState: {
                ChannelRotationStateStore.loadOrSeed(
                    channelId: channelId,
                    nowMs: UInt64(Date().timeIntervalSince1970 * 1000)
                )
            },
            onError: { [weak self] err in
                Task { @MainActor in self?.handleError(err) }
            }
        )
    }

    private func handleRotated(_ epoch: UInt32) {
        error = nil
        refreshStatus()
    }

    private func handleError(_ err: Error) {
        // Heartbeat errors are transient (the next tick retries from the new
        // local state); surface the latest so a persistent failure is visible
        // without spamming.
        NSLog("Channels: scheduled rotation check failed: %@", String(describing: err))
        error = "The last automatic refresh did not complete. It retries on its own."
    }

    private func refreshStatus() {
        status = ChannelsFormatting.accessRefreshLine(
            lastRotationMs: ChannelRotationStateStore.load(channelId: channelIdHex)?.lastRotationMs,
            nowMs: Int64(Date().timeIntervalSince1970 * 1000),
            calendar: .current,
            locale: .current
        )
    }
}

/// Per-channel persistence for the rotation state (`lastRotationMs` +
/// `postsSinceRotation`). Non-secret operational metadata — no key material
/// — so plain `UserDefaults` is appropriate (invariant #7 governs key
/// material only; the channel list itself is likewise not SQLCipher-backed
/// on iOS today).
enum ChannelRotationStateStore {
    private static let prefix = "lakko.channels.rotation-state.v1."

    private struct Persisted: Codable {
        let lastRotationMs: UInt64
        let postsSinceRotation: UInt32
    }

    static func load(channelId: String) -> ChannelRotationState? {
        guard
            let data = UserDefaults.standard.data(forKey: prefix + channelId),
            let p = try? JSONDecoder().decode(Persisted.self, from: data)
        else { return nil }
        return ChannelRotationState(
            lastRotationMs: p.lastRotationMs,
            postsSinceRotation: p.postsSinceRotation
        )
    }

    static func save(channelId: String, state: ChannelRotationState) {
        let p = Persisted(
            lastRotationMs: state.lastRotationMs,
            postsSinceRotation: state.postsSinceRotation
        )
        guard let data = try? JSONEncoder().encode(p) else { return }
        UserDefaults.standard.set(data, forKey: prefix + channelId)
    }

    /// Return the persisted state, or seed a genesis `after_rotation(now)`
    /// snapshot when absent — so the first scheduled check on an existing
    /// channel does not fire an immediate rotation (it lands ~one interval
    /// later instead).
    static func loadOrSeed(channelId: String, nowMs: UInt64) -> ChannelRotationState {
        if let existing = load(channelId: channelId) { return existing }
        let seeded = channelRotationStateAfterRotation(nowMs: nowMs)
        save(channelId: channelId, state: seeded)
        return seeded
    }
}
