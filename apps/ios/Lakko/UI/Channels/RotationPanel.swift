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
    /// ADR-0051 §4 — the publisher's own copy of the metadata record lives
    /// here, and the post-rotation re-issue reads it (and the post-rotation
    /// seed is written back here so the re-issue and every later post seal
    /// under the seed the subscribers were just resealed to).
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient
    /// Canonical AccountIdentity (phrase) seed — used to open the channel's
    /// persistent PublisherGroup (same handle ``CoPublishersPanel`` opens).
    let accountIdentitySeed: Data

    @StateObject private var model = RotationPanelModel()
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        ChannelManageCard(
            title: l10n.t("channels.rotation.title"),
            subtitle: l10n.t("channels.rotation.desc")
        ) {
            Text(model.status.text)
                .font(.subheadline)
                .foregroundStyle(Color.lakkoText)
                .fixedSize(horizontal: false, vertical: true)

            Button {
                Task { await model.rotateNow() }
            } label: {
                Label(l10n.t(model.busy ? "channels.rotation.refreshing" : "channels.rotation.refresh_now"),
                      systemImage: "arrow.triangle.2.circlepath")
            }
            .buttonStyle(.bordered)
            .disabled(model.busy || !model.ready)

            if let error = model.error {
                ChannelManageErrorLine(text: error.text)
            }
        }
        .task {
            await model.load(
                channelId: channelId,
                state: state,
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
    @Published var status: UiText = .key("common.loading")
    /// Surfaced when opening the group or a manual rotation fails.
    @Published var error: UiText?
    /// `true` while a manual refresh is in flight.
    @Published var busy = false
    /// `true` once the PublisherGroup + cron are ready (enables the button).
    @Published var ready = false

    private var channelIdHex = ""
    private var client = ChannelsClient()
    /// ADR-0051 §4 — where the publisher's record and keys live; `nil` only
    /// before `load`.
    private weak var state: ChannelsUiState?
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
        state: ChannelsUiState,
        client: ChannelsClient,
        accountIdentitySeed: Data
    ) async {
        self.channelIdHex = channelId
        self.state = state
        self.client = client
        if cron == nil {
            do {
                let id = try Identity.fromSeed(seed: accountIdentitySeed)
                let pg = try CoPublisher.openPublisherGroup(
                    identity: id, channelIdHex: channelId)
                identity = id
                publisherGroup = pg
                cron = makeCron(identity: id, publisherGroup: pg)
                // ADR-0057, live walk 2026-09-29 — the channel owner claims
                // the publisher journal on open (idempotent; a co-publisher
                // device never claims), so a rotation's epoch registration is
                // not refused on a journal whose claim the DS dropped.
                await CoPublisher.claimJournal(ds: DsClient(), identity: id, publisherGroup: pg)
                ready = true
            } catch {
                NSLog("Channels: open publisher group failed: %@", String(describing: error))
                self.error = UiText {
                    LegacyChannelV1.describe(error) { _ in Localization.text("channels.manage.error.open") }
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
            self.error = .key("channels.rotation.error.refresh")
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
        let accountIdB64 = AcceptedContactsStore.boundAccountIdB64
        let context = RotationCron.Context(
            publisherGroup: publisherGroup,
            identity: identity,
            channelIdHex: channelId,
            channelsClient: client,
            saveState: { newState in
                ChannelRotationStateStore.save(
                    accountIdB64: accountIdB64, channelId: channelId, state: newState)
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
        let accountIdB64 = AcceptedContactsStore.boundAccountIdB64
        cronTask = cron.start(
            policy: channelRotationPolicyDefault(),
            loadState: {
                ChannelRotationStateStore.loadOrSeed(
                    accountIdB64: accountIdB64,
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
        // ADR-0051 §4 — after every rotation the newest epoch must hold a
        // record a late subscriber can read. The rotation advanced the
        // PublisherGroup, so the content seed changed: write the new seed +
        // epoch back first (every later post must seal under the seed the
        // subscribers were just resealed to), then re-post the STORED record
        // with its ORIGINAL stamp — never a fresh clock read, so the re-issue
        // can never out-order an edit made in between.
        guard let state, let identity, let publisherGroup else { return }
        if let old = state.keysByChannel[channelIdHex],
           let newSeed = try? publisherGroup.channelSeed(identity: identity) {
            state.setKeys(
                channelId: channelIdHex,
                keys: ChannelPublisherKeys(seed: newSeed, signingKeySeed: old.signingKeySeed))
        }
        state.setEpoch(channelId: channelIdHex, epoch: epoch)
        guard let record = state.sharedMeta(channelId: channelIdHex) else { return }
        let channelId = channelIdHex
        let client = self.client
        Task { @MainActor in
            do {
                try await ChannelMetaRecord.post(
                    state: state, client: client, channelId: channelId, meta: record)
            } catch {
                NSLog("Channels: post-rotation metadata re-issue failed: %@", String(describing: error))
            }
        }
    }

    private func handleError(_ err: Error) {
        // Heartbeat errors are transient (the next tick retries from the new
        // local state); surface the latest so a persistent failure is visible
        // without spamming.
        NSLog("Channels: scheduled rotation check failed: %@", String(describing: err))
        error = .key("channels.rotation.error.check")
    }

    private func refreshStatus() {
        let last = ChannelRotationStateStore.load(
            accountIdB64: AcceptedContactsStore.boundAccountIdB64,
            channelId: channelIdHex)?.lastRotationMs
        let nowMs = Int64(Date().timeIntervalSince1970 * 1000)
        status = UiText {
            ChannelsFormatting.accessRefreshLine(
                lastRotationMs: last, nowMs: nowMs, calendar: .current,
                locale: Localization.currentLocale)
        }
    }
}

/// Per-channel persistence for the rotation state (`lastRotationMs` +
/// `postsSinceRotation`). Non-secret operational metadata — no key material
/// — so plain `UserDefaults` is appropriate (invariant #7 governs key
/// material only; the channel list itself is likewise not SQLCipher-backed
/// on iOS today).
///
/// Per account (2026-09-30): the state belongs to the account that publishes
/// the channel, so it is filed under that account
/// (`lakko.channels.rotation-state.v2.<account>.<channel>`), the way
/// ``AcceptedContactsStore`` files the contacts. It used to be keyed by the
/// channel alone. No account: nothing is read or written (``loadOrSeed`` then
/// hands back an unpersisted genesis state).
enum ChannelRotationStateStore {
    private static let prefix = "lakko.channels.rotation-state.v2."
    /// The device-wide prefix used before the state was per account. Never
    /// read; removed by ``clear(accountIdB64:defaults:)``.
    static let legacyPrefix = "lakko.channels.rotation-state.v1."

    private struct Persisted: Codable {
        let lastRotationMs: UInt64
        let postsSinceRotation: UInt32
    }

    private static func accountPrefix(_ accountIdB64: String) -> String {
        prefix + accountIdB64 + "."
    }

    static func load(
        accountIdB64: String?,
        channelId: String,
        defaults: UserDefaults = .standard
    ) -> ChannelRotationState? {
        guard
            let accountIdB64,
            let data = defaults.data(forKey: accountPrefix(accountIdB64) + channelId),
            let p = try? JSONDecoder().decode(Persisted.self, from: data)
        else { return nil }
        return ChannelRotationState(
            lastRotationMs: p.lastRotationMs,
            postsSinceRotation: p.postsSinceRotation
        )
    }

    static func save(
        accountIdB64: String?,
        channelId: String,
        state: ChannelRotationState,
        defaults: UserDefaults = .standard
    ) {
        guard let accountIdB64 else { return }
        let p = Persisted(
            lastRotationMs: state.lastRotationMs,
            postsSinceRotation: state.postsSinceRotation
        )
        guard let data = try? JSONEncoder().encode(p) else { return }
        defaults.set(data, forKey: accountPrefix(accountIdB64) + channelId)
    }

    /// Forget-device: drop one account's rotation states (and the old
    /// device-wide rows). Other accounts' stay.
    static func clear(accountIdB64: String, defaults: UserDefaults = .standard) {
        let mine = accountPrefix(accountIdB64)
        for key in defaults.dictionaryRepresentation().keys
        where key.hasPrefix(mine) || key.hasPrefix(legacyPrefix) {
            defaults.removeObject(forKey: key)
        }
    }

    /// Return the persisted state, or seed a genesis `after_rotation(now)`
    /// snapshot when absent — so the first scheduled check on an existing
    /// channel does not fire an immediate rotation (it lands ~one interval
    /// later instead).
    static func loadOrSeed(
        accountIdB64: String?,
        channelId: String,
        nowMs: UInt64,
        defaults: UserDefaults = .standard
    ) -> ChannelRotationState {
        if let existing = load(accountIdB64: accountIdB64, channelId: channelId, defaults: defaults) {
            return existing
        }
        let seeded = channelRotationStateAfterRotation(nowMs: nowMs)
        save(accountIdB64: accountIdB64, channelId: channelId, state: seeded, defaults: defaults)
        return seeded
    }
}
