// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import UIKit

/// Letting new subscribers in (ADR-0010, ADR-0059 section 4): for a channel
/// this device can post to, read the subscriber list and seal the current
/// epoch's seed to every key that has not had it yet. Mirror of web
/// `channels/subscriber-admission.ts` + `SubscriberAdmission.tsx`.
///
/// One type for both callers, so they share what has been delivered: the
/// session-long loop (``runLoop(channels:accountIdentitySeed:)``), which lets
/// people in while the app is in front whatever screen is showing, and the
/// Manage screen's ``SubscribersPanel``, which shows the same state and polls
/// faster while it is open. Before this the let-in loop lived in the panel
/// alone, so a person who subscribed from Discover waited until the owner
/// happened to open Manage.
///
/// What was delivered is kept in memory per (channel, key, epoch). A relaunch
/// delivers everyone once more, which the server takes as an upsert; a new
/// epoch delivers again, which the rotation fan-out would also do. Every
/// envelope is sealed on this device under the publisher's seed; the server
/// only stores it (invariant #2).
@MainActor
final class SubscriberAdmission: ObservableObject {

    static let shared = SubscriberAdmission()

    /// Between passes over the channels this device posts to: the channel
    /// publisher pass of 60 s (2026-11-06, was 15 s; web and Android use the
    /// same). Chosen, not measured.
    static let intervalSeconds: UInt64 = 60

    /// How one subscriber's envelope stands on this device.
    enum Status: Equatable { case ready, failed }

    struct Entry: Equatable {
        /// The epoch the envelope was sealed for.
        let epoch: UInt32
        let status: Status
    }

    struct ChannelAdmission: Equatable {
        /// The list as last read, `nil` before the first read.
        var subscribers: [String]?
        /// Key -> status at the epoch it was sealed for.
        var status: [String: Entry] = [:]
    }

    enum AdmissionError: Error {
        /// This device holds no publisher keys for the channel.
        case noKeys
        /// The subscriber key is not 32 bytes of hex.
        case badKey
    }

    @Published private(set) var byChannel: [String: ChannelAdmission] = [:]

    private let client: ChannelsClient
    private var inFlight: Set<String> = []

    init(client: ChannelsClient = ChannelsClient()) {
        self.client = client
    }

    func admission(channelId: String) -> ChannelAdmission {
        byChannel[channelId] ?? ChannelAdmission()
    }

    /// The status of `pubkeyHex` at the channel's current epoch: `.ready`
    /// only when delivered at that epoch, `nil` when not yet handled.
    func status(channelId: String, pubkeyHex: String, state: ChannelsUiState) -> Status? {
        guard let entry = byChannel[channelId]?.status[pubkeyHex] else { return nil }
        let epoch = state.epochByChannel[channelId] ?? 0
        if entry.status == .ready && entry.epoch != epoch { return nil }
        return entry.status
    }

    /// Forget everything; on sign-out and at a new session.
    func reset() {
        byChannel = [:]
        inFlight = []
    }

    /// The channel's current epoch: the cached one, else read from the
    /// server and cached. Never guessed: an envelope filed under the wrong
    /// epoch lands the seed at the wrong key of the subscriber's
    /// `epoch -> seed` map (ADR-0031).
    private func currentEpoch(channelId: String, state: ChannelsUiState) async throws -> UInt32 {
        if let epoch = state.epochByChannel[channelId] { return epoch }
        let epoch = try await client.fetchEpoch(channelIdHex: channelId)
        state.setEpoch(channelId: channelId, epoch: epoch)
        return epoch
    }

    private func record(channelId: String, pubkeyHex: String, entry: Entry) {
        var a = byChannel[channelId] ?? ChannelAdmission()
        a.status[pubkeyHex] = entry
        byChannel[channelId] = a
    }

    /// Seal and deliver the current seed to one key. Records the outcome and
    /// rethrows a failure, so a manual retry can word the error.
    func admit(
        channelId: String, pubkeyHex: String, state: ChannelsUiState, accountIdentity: Identity
    ) async throws {
        guard state.keysByChannel[channelId] != nil else { throw AdmissionError.noKeys }
        _ = try await currentEpoch(channelId: channelId, state: state)
        // ADR-0087 section 15 — the seed of an epoch the channel has reached,
        // never the composer's seed under an epoch it does not serve (a
        // co-publisher that applied the owner's commit before the owner's
        // reseal landed); none held: nobody is let in this pass.
        guard let sealing = state.composerSeed(channelId: channelId) else { throw AdmissionError.noKeys }
        let epoch = sealing.epoch
        do {
            guard let subPub = Data(channelHex: pubkeyHex) else { throw AdmissionError.badKey }
            let wire = try ChannelsComposer.sealSubscriberSeedEnvelope(
                subscriberPub: subPub,
                channelIdHex: channelId,
                epochId: epoch,
                seed: sealing.seed
            )
            // ADR-0031: filed under the exact epoch it was sealed for.
            try await client.deliverEnvelope(
                channelIdHex: channelId,
                subscriberPubkeyHex: pubkeyHex,
                envelope: wire,
                epoch: epoch,
                accountIdentity: accountIdentity
            )
            record(channelId: channelId, pubkeyHex: pubkeyHex, entry: Entry(epoch: epoch, status: .ready))
        } catch {
            record(channelId: channelId, pubkeyHex: pubkeyHex, entry: Entry(epoch: epoch, status: .failed))
            throw error
        }
    }

    /// Read the list and let in every key not yet delivered at the current
    /// epoch (or whose last delivery failed). One pass per channel at a time;
    /// a second caller while one runs returns at once. Sequential, so a burst
    /// of new subscribers does not open many Rust calls at once. Throws when
    /// the list cannot be read; a single delivery failure is recorded and the
    /// next pass tries again.
    func admitNewSubscribers(
        channelId: String,
        state: ChannelsUiState,
        accountIdentitySeed: Data
    ) async throws {
        guard !inFlight.contains(channelId), state.keysByChannel[channelId] != nil else { return }
        inFlight.insert(channelId)
        defer { inFlight.remove(channelId) }

        let identity = try Identity.fromSeed(seed: accountIdentitySeed)
        // ADR-0087 section 7 — a private channel lets in only the rows whose
        // sealed ticket names one of its live invite keys.
        if state.extras(channelId).isPublic == false {
            // A co-publisher holding no seed yet lets nobody in (it cannot
            // check a ticket); the owner's device does meanwhile.
            let seeds = ChannelInvites.admissionSeeds(channelIdHex: channelId).map(\.seed)
            if !seeds.isEmpty {
                try await admitPrivate(
                    channelId: channelId, state: state, identity: identity, admissionSeeds: seeds)
            }
            return
        }
        // Owner-signed, with the co-publisher fallback inside the client.
        let list = try await client.listSubscribers(channelIdHex: channelId, accountIdentity: identity)
        state.setSubscriberCount(channelId: channelId, count: list.count)
        var a = byChannel[channelId] ?? ChannelAdmission()
        a.subscribers = list
        byChannel[channelId] = a

        let epoch = try await currentEpoch(channelId: channelId, state: state)
        for pk in list {
            if Task.isCancelled { return }
            if let entry = byChannel[channelId]?.status[pk],
               entry.status == .ready, entry.epoch == epoch { continue }
            do {
                try await admit(channelId: channelId, pubkeyHex: pk, state: state, accountIdentity: identity)
            } catch {
                NSLog("[channels] letting a subscriber in failed")
            }
        }
    }

    /// The session-long loop: once at start, then every
    /// ``intervalSeconds`` while the app is not in the background, a pass
    /// over every channel this device holds publisher keys for (owned or
    /// co-published). The channel state is bound at construction, so the
    /// keys are already loaded. Returns when the calling task is cancelled
    /// (sign-out, account switch).
    /// Section 7 — read the rows a page at a time, open each ticket with the
    /// channel's admission seed, check the invite key it names is live, seal
    /// only then; a row with no valid ticket is removed.
    private func admitPrivate(
        channelId: String, state: ChannelsUiState, identity: Identity, admissionSeeds: [Data]
    ) async throws {
        guard let channelIdBytes = ChannelsClient.hexToData(channelId) else { return }
        let live = Set(try await client.inviteKeys(channelIdHex: channelId, accountIdentity: identity))
        let epoch = try await currentEpoch(channelId: channelId, state: state)
        var after: UInt64 = 0
        var all: [String] = []
        for _ in 0..<200 {
            let page = try await client.subscriberPage(channelIdHex: channelId, after: after, accountIdentity: identity)
            for row in page.rows {
                if Task.isCancelled { return }
                all.append(row.pubkeyHex)
                if let entry = byChannel[channelId]?.status[row.pubkeyHex], entry.status == .ready, entry.epoch == epoch {
                    continue
                }
                // Every held version, newest first: a ticket sealed to an
                // older version (an older link) still opens.
                guard let pk = Data(channelHex: row.pubkeyHex), let ticket = row.sealedTicket,
                      let inviteKey = admissionSeeds.reversed().lazy.compactMap({ seed in
                          try? openChannelAdmissionTicket(
                              admissionSeed: seed, channelId: channelIdBytes, subscriberPk: pk, ticket: ticket)
                      }).first,
                      live.contains(inviteKey)
                else {
                    // A row already let in under an earlier epoch stays: a
                    // revoked link removes nobody (section 7).
                    if byChannel[channelId]?.status[row.pubkeyHex]?.status == .ready { continue }
                    try? await client.removeSubscriber(
                        channelIdHex: channelId, pubkeyHex: row.pubkeyHex, accountIdentity: identity)
                    continue
                }
                do {
                    try await admit(channelId: channelId, pubkeyHex: row.pubkeyHex, state: state, accountIdentity: identity)
                } catch {
                    NSLog("[channels] letting a subscriber in failed")
                }
            }
            guard !page.rows.isEmpty, page.nextAfter > after else { break }
            after = page.nextAfter
        }
        var a = byChannel[channelId] ?? ChannelAdmission()
        a.subscribers = all
        byChannel[channelId] = a
    }

    func runLoop(channels: ChannelsUiState, accountIdentitySeed: () -> Data?) async {
        while !Task.isCancelled {
            if UIApplication.shared.applicationState != .background,
               let seed = accountIdentitySeed() {
                for id in channels.keysByChannel.keys.sorted() {
                    if Task.isCancelled { return }
                    // ADR-0087 section 6 — a public channel's subscribers read
                    // its public seed; nobody reads its subscriber list.
                    if channels.extras(id).isPublic == true { continue }
                    do {
                        try await admitNewSubscribers(
                            channelId: id, state: channels, accountIdentitySeed: seed)
                    } catch {
                        // A channel the server does not answer for this time.
                        NSLog("[channels] subscriber admission failed")
                    }
                }
            }
            try? await Task.sleep(nanoseconds: Self.intervalSeconds * 1_000_000_000)
        }
    }
}
