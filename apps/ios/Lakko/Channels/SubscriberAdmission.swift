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

    /// Between passes over the channels this device posts to (chosen, the
    /// web client's 15 s). The Manage screen polls faster while open.
    static let intervalSeconds: UInt64 = 15

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
    func admit(channelId: String, pubkeyHex: String, state: ChannelsUiState) async throws {
        guard let keys = state.keysByChannel[channelId] else { throw AdmissionError.noKeys }
        let epoch = try await currentEpoch(channelId: channelId, state: state)
        do {
            guard let subPub = Data(channelHex: pubkeyHex) else { throw AdmissionError.badKey }
            let wire = try ChannelsComposer.sealSubscriberSeedEnvelope(
                subscriberPub: subPub,
                channelIdHex: channelId,
                epochId: epoch,
                seed: keys.seed
            )
            // ADR-0031: filed under the exact epoch it was sealed for.
            try await client.deliverEnvelope(
                channelIdHex: channelId,
                subscriberPubkeyHex: pubkeyHex,
                envelope: wire,
                epoch: epoch
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
                try await admit(channelId: channelId, pubkeyHex: pk, state: state)
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
    func runLoop(channels: ChannelsUiState, accountIdentitySeed: () -> Data?) async {
        while !Task.isCancelled {
            if UIApplication.shared.applicationState != .background,
               let seed = accountIdentitySeed() {
                for id in channels.keysByChannel.keys.sorted() {
                    if Task.isCancelled { return }
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
