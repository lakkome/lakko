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
    /// What the let-in pass does with one row (ADR-0087 section 7).
    enum LetIn: Equatable { case admit, remove, leave }

    /// The let-in verdict of one row, the web rule (`subscriber-admission.ts`)
    /// with this device's mark (``ChannelExtras/admittedThroughSeq``):
    /// - a row at or below the mark came in before the channel went private
    ///   or was let in by an earlier pass: a reader, let in (UX 4.1:
    ///   "Everyone subscribed stays");
    /// - a row whose ticket names a live invite key: let in;
    /// - this device's own key: left alone (a publisher's own card key);
    /// - any other row, on a device that set the mark itself: removed;
    /// - on a device that did not (a co-publisher, another device of the
    ///   owner), left alone: it cannot tell a reader from before from a row
    ///   with no ticket. No ticket alone never lets a row in.
    nonisolated static func letInVerdict(
        seq: UInt64, ticketLive: Bool, mark: UInt64, markSet: Bool, isOwnKey: Bool
    ) -> LetIn {
        if seq <= mark { return .admit }
        if ticketLive { return .admit }
        if isOwnKey { return .leave }
        return markSet ? .remove : .leave
    }

    /// A row this device never removes: its own subscriber key, or the
    /// account's reader key it registered (2026-11-13).
    nonisolated static func isOwnRow(_ pubkeyHex: String, ownKeyHex: String?, accountReaderHex: String?) -> Bool {
        let row = pubkeyHex.lowercased()
        return row == ownKeyHex || row == accountReaderHex
    }

    /// The mark after a pass judged rows (in row order, each with its
    /// verdict): on a device that set the mark, past every row judged; on
    /// any other, only past the rows let in before the first one left alone,
    /// so a row it left is never read as a reader, and never resealed to.
    nonisolated static func advancedMark(
        _ mark: UInt64, markSet: Bool, judged: [(seq: UInt64, verdict: LetIn)]
    ) -> UInt64 {
        var through = mark
        for row in judged.sorted(by: { $0.seq < $1.seq }) {
            if !markSet, row.verdict != .admit { break }
            through = max(through, row.seq)
        }
        return through
    }

    /// Section 4.1 / ADR-0087 section 7 — the channel goes private (or its
    /// owner mints its first admission seed): the rows of today stay readers.
    /// Raise this device's mark to the newest row, the web
    /// `repairPrivateChannelAccess` and type-change step.
    static func markCurrentRowsAsReaders(
        channelId: String, state: ChannelsUiState, client: ChannelsClient, accountIdentity: Identity
    ) async throws {
        var through = state.extras(channelId).admittedThroughSeq ?? 0
        var after = through
        for _ in 0..<10_000 {
            let page = try await client.subscriberPage(channelIdHex: channelId, after: after, accountIdentity: accountIdentity)
            guard let last = page.rows.last else { break }
            through = max(through, last.seq)
            guard page.nextAfter > after else { break }
            after = page.nextAfter
        }
        let raised = through
        state.updateExtras(channelId) {
            $0.admittedThroughSeq = max($0.admittedThroughSeq ?? 0, raised)
            $0.admissionMarkSet = true
            $0.subscriberRowsReadSeq = max($0.subscriberRowsReadSeq ?? 0, raised)
        }
    }

    /// Section 7 — read the rows a page at a time, open each ticket with the
    /// channel's admission seed, check the invite key it names is live, and
    /// apply ``letInVerdict(seq:ticketLive:mark:isOwnKey:)``: seal to the rows
    /// let in, remove the rest on a device holding the mark, then move the
    /// mark past the rows judged.
    private func admitPrivate(
        channelId: String, state: ChannelsUiState, identity: Identity, admissionSeeds: [Data]
    ) async throws {
        guard let channelIdBytes = ChannelsClient.hexToData(channelId) else { return }
        let live = Set(try await client.inviteKeys(channelIdHex: channelId, accountIdentity: identity))
        let epoch = try await currentEpoch(channelId: channelId, state: state)
        let ownKeyHex = state.subscriberKeysByChannel[channelId]
            .flatMap { try? ChannelSubscriberKeypair.fromSecret(secret: $0.secret).`public`() }
            .map { ChannelsComposer.bytesToHex($0).lowercased() }
        // 2026-11-13: the account's reader key this device registered is its
        // own too (its other devices read with it): never removed here.
        let accountReaderHex = state.extras(channelId).readerKeyRegisteredHex?.lowercased()
        let mark = state.extras(channelId).admittedThroughSeq ?? 0
        let markSet = state.extras(channelId).admissionMarkSet == true
        var judged: [(seq: UInt64, verdict: LetIn)] = []
        var stopped = false
        var after: UInt64 = 0
        var all: [String] = []
        // ADR-0090 security pass — the newest row this device read itself:
        // an owner's mark counts only up to it.
        var readThrough: UInt64 = 0
        pages: for _ in 0..<200 {
            let page = try await client.subscriberPage(channelIdHex: channelId, after: after, accountIdentity: identity)
            for row in page.rows {
                if Task.isCancelled { return }
                all.append(row.pubkeyHex)
                readThrough = max(readThrough, row.seq)
                if let entry = byChannel[channelId]?.status[row.pubkeyHex], entry.status == .ready, entry.epoch == epoch {
                    judged.append((row.seq, .admit))
                    continue
                }
                // Every held version, newest first: a ticket sealed to an
                // older version (an older link) still opens.
                var ticketLive = false
                if let pk = Data(channelHex: row.pubkeyHex), let ticket = row.sealedTicket,
                   let inviteKey = admissionSeeds.reversed().lazy.compactMap({ seed in
                       try? openChannelAdmissionTicket(
                           admissionSeed: seed, channelId: channelIdBytes, subscriberPk: pk, ticket: ticket)
                   }).first {
                    ticketLive = live.contains(inviteKey)
                }
                let verdict = Self.letInVerdict(
                    seq: row.seq, ticketLive: ticketLive, mark: mark, markSet: markSet,
                    isOwnKey: Self.isOwnRow(
                        row.pubkeyHex, ownKeyHex: ownKeyHex, accountReaderHex: accountReaderHex))
                switch verdict {
                case .admit:
                    do {
                        try await admit(channelId: channelId, pubkeyHex: row.pubkeyHex, state: state, accountIdentity: identity)
                    } catch {
                        NSLog("[channels] letting a subscriber in failed")
                    }
                case .leave:
                    break
                case .remove:
                    do {
                        try await client.removeSubscriber(
                            channelIdHex: channelId, pubkeyHex: row.pubkeyHex, accountIdentity: identity)
                    } catch {
                        // The mark stops before this row: a row not removed
                        // is judged again, never read as a reader.
                        NSLog("[channels] a row with no valid ticket was not removed yet")
                        stopped = true
                        break pages
                    }
                }
                judged.append((row.seq, verdict))
            }
            guard !page.rows.isEmpty, page.nextAfter > after else { break }
            after = page.nextAfter
        }
        let raised = Self.advancedMark(mark, markSet: markSet, judged: judged)
        if raised != mark {
            state.updateExtras(channelId) { $0.admittedThroughSeq = max($0.admittedThroughSeq ?? 0, raised) }
        }
        if readThrough > (state.extras(channelId).subscriberRowsReadSeq ?? 0) {
            state.updateExtras(channelId) { $0.subscriberRowsReadSeq = readThrough }
        }
        ChannelDeviceAdmission.applyReceivedMark(channelId: channelId, state: state)
        if !stopped {
            var a = byChannel[channelId] ?? ChannelAdmission()
            a.subscribers = all
            byChannel[channelId] = a
        }
    }

    /// The holds of the admission pass (2026-11-20), apart from the
    /// recorded commits' so a transient failure here never delays those.
    static let retry = PublisherCommitRetry()

    func runLoop(channels: ChannelsUiState, accountIdentitySeed: () -> Data?) async {
        while !Task.isCancelled {
            if UIApplication.shared.applicationState != .background,
               let seed = accountIdentitySeed() {
                for id in channels.keysByChannel.keys.sorted() {
                    if Task.isCancelled { return }
                    // ADR-0087 section 6 — a public channel's subscribers read
                    // its public seed; nobody reads its subscriber list.
                    if channels.extras(id).isPublic == true { continue }
                    // 2026-11-20 — a refused or failing channel waits out its
                    // hold (a refusal: until the next start) instead of being
                    // read again every pass.
                    guard let account = AcceptedContactsStore.boundAccountIdB64,
                          Self.retry.mayAttempt(accountIdB64: account, channelIdHex: id) else { continue }
                    do {
                        try await admitNewSubscribers(
                            channelId: id, state: channels, accountIdentitySeed: seed)
                        Self.retry.noteSuccess(accountIdB64: account, channelIdHex: id)
                    } catch {
                        let hold = Self.retry.noteFailure(accountIdB64: account, channelIdHex: id, error: error)
                        NSLog("[channels] subscriber admission failed (%@); %@",
                              PublisherCommitFailure.logLabel(error),
                              hold.refused ? "not tried again until the next start" : "tried again after a wait")
                    }
                }
            }
            try? await Task.sleep(nanoseconds: Self.intervalSeconds * 1_000_000_000)
        }
    }
}
