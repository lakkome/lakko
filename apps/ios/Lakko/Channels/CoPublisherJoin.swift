// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import SwiftUI

/// One publisher request this account sent that has not landed (ADR-0087
/// section 15, ADR-0045 decision 2: a request has no deadline). Mirror of
/// `PendingPublisherRequest` in `apps/web/src/channels/pending-publisher-requests.ts`.
struct PendingPublisherRequest: Codable, Equatable {
    let channelIdHex: String
    /// The PublisherGroup the link named, lower-case hex.
    let groupIdHex: String
    /// base64 of the signature key the knocked KeyPackage belongs to (the
    /// channel provider identity's at the time of the request).
    let joinerKeyB64: String
    /// What the link said, for the channel row once joined.
    let name: String
    let description: String
    let requestedAtMs: Int64
}

/// Publisher requests this account sent and that have not landed.
///
/// `Accept` on a publisher invitation knocks with a KeyPackage and waits for
/// the owner's device. The owner may approve minutes or days later, when its
/// app next runs; the Welcome then lands in this account's mailbox. The record
/// is what lets the mailbox receive loop (``ContactsRepository``) join that
/// Welcome whenever it arrives, after a restart too, whether or not the card
/// or the link screen is still open.
///
/// Per account, in `UserDefaults` behind the account store gate (no secret in
/// it: the KeyPackage's private key is in the channel provider's snapshot,
/// saved before the knock). A record goes once the channel is joined, when
/// the channel is gone, and with the account wipe. A second request for the
/// same channel replaces the first.
enum PendingPublisherRequests {
    static func key(_ accountIdB64: String) -> String { "lakko.channels.pending-publisher.v1.\(accountIdB64)" }

    /// Every pending request of the account; an unreadable row reads as none.
    static func load(accountIdB64: String, defaults: UserDefaults = .standard) -> [PendingPublisherRequest] {
        guard let data = defaults.data(forKey: key(accountIdB64)),
              let list = try? JSONDecoder().decode([PendingPublisherRequest].self, from: data)
        else { return [] }
        return list
    }

    static func isPending(channelIdHex: String, accountIdB64: String, defaults: UserDefaults = .standard) -> Bool {
        load(accountIdB64: accountIdB64, defaults: defaults).contains { $0.channelIdHex == channelIdHex }
    }

    /// Record a request, replacing an earlier one for the same channel.
    /// Throws when it cannot be written (the caller must not knock then).
    static func record(
        _ request: PendingPublisherRequest, accountIdB64: String, defaults: UserDefaults = .standard
    ) throws {
        let rest = load(accountIdB64: accountIdB64, defaults: defaults)
            .filter { $0.channelIdHex != request.channelIdHex }
        try save(rest + [request], accountIdB64: accountIdB64, defaults: defaults)
    }

    /// Drop the request for a channel (joined, or the channel is gone).
    static func drop(channelIdHex: String, accountIdB64: String, defaults: UserDefaults = .standard) {
        let list = load(accountIdB64: accountIdB64, defaults: defaults)
        let rest = list.filter { $0.channelIdHex != channelIdHex }
        guard rest.count != list.count else { return }
        try? save(rest, accountIdB64: accountIdB64, defaults: defaults)
    }

    /// The account wipe.
    static func clear(accountIdB64: String, defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: key(accountIdB64))
    }

    private static func save(
        _ list: [PendingPublisherRequest], accountIdB64: String, defaults: UserDefaults
    ) throws {
        let gate = try AccountStoreGate.enterOrEnd(accountIdB64)
        defer { gate.leave() }
        if list.isEmpty {
            defaults.removeObject(forKey: key(accountIdB64))
        } else {
            defaults.set(try JSONEncoder().encode(list), forKey: key(accountIdB64))
        }
    }
}

/// ADR-0028 OQ3 / ADR-0087 section 15.2 — the joiner's whole co-publisher
/// join: ask to join through a co-publisher link, wait for the owner's
/// Welcome, mint local publisher keys and add the channel. One path for the
/// publisher card's `Accept` and the link screen.
///
/// The REQUEST has no deadline (ADR-0045 decision 2): it is recorded before
/// the knock, the screen waits a while, and when the owner has not approved
/// by then the request stays pending and the mailbox receive loop lands the
/// Welcome whenever it arrives (``landPendingWelcome(_:accountIdB64:identity:state:)``).
@MainActor
enum CoPublisherJoin {
    /// Where a join stands, for the progress line.
    enum Stage { case sending, waiting }

    /// How a run ended.
    enum Outcome: Equatable {
        /// This device publishes to the channel.
        case joined(channelIdHex: String)
        /// Sent and recorded; the owner has not approved yet. Not a failure:
        /// the request stays and lands whenever the Welcome arrives.
        case waiting(channelIdHex: String)
    }

    /// Why a join did not finish: the line to show and whether `Try again`
    /// can help.
    struct Failure: Equatable {
        let message: UiText
        let offersRetry: Bool
        /// What it was, for the card's `Ask again` (UX 15.2.2).
        var kind: Kind = .other

        enum Kind: Equatable {
            /// The link no longer works (revoked, expired, not a co-publisher
            /// link, damaged).
            case linkDead
            /// No connection reached Lakko.
            case offline
            case other
        }

        /// No connection reached the server: the DS or the directory said so.
        static func isOffline(_ error: Error) -> Bool {
            if case .network = error as? DsClientError { return true }
            if case .network = error as? DiscoveryClientError { return true }
            return error is URLError
        }
    }

    /// Poll attempts (every 3 s) a screen waits before it shows the request
    /// as waiting: the web 60 x 3 s window.
    static let pollAttempts = 60
    static let pollIntervalNs: UInt64 = 3 * 1_000_000_000

    /// Run the join. Throws ``Failure``.
    static func run(
        link: String, localName: String, state: ChannelsUiState, accountIdentitySeed: Data,
        ds: DsClient = DsClient(), onStage: (Stage) -> Void
    ) async throws -> Outcome {
        // The account this request is made for, before any wait: the
        // co-publisher mark is filed under it, and under no account signed in
        // while the request was out.
        let accountIdB64 = AcceptedContactsStore.boundAccountIdB64
        let name = localName.trimmingCharacters(in: .whitespaces)
        do {
            onStage(.sending)
            // The durable channel provider (G-50): the joined PublisherGroup
            // lands in it and is snapshotted once the join is done.
            let identity = try ChannelPublisherIdentity.shared.identity(currentSeed: accountIdentitySeed)
            let request = try await CoPublisher.requestToCoPublish(
                ds: ds, identity: identity, accountIdB64: accountIdB64, link: link,
                recordRequest: { request in
                    guard let accountIdB64 else { throw StaleBindingError() }
                    try PendingPublisherRequests.record(
                        PendingPublisherRequest(
                            channelIdHex: request.channelIdHex,
                            groupIdHex: DsClient.pseudonymHex(request.groupId),
                            joinerKeyB64: base64Encode(request.joinerKey),
                            name: name, description: "",
                            requestedAtMs: Int64(Date().timeIntervalSince1970 * 1000)),
                        accountIdB64: accountIdB64)
                })
            onStage(.waiting)
            for _ in 0..<pollAttempts {
                try Task.checkCancellation()
                // Landed by the mailbox receive loop meanwhile.
                if let accountIdB64,
                   !PendingPublisherRequests.isPending(channelIdHex: request.channelIdHex, accountIdB64: accountIdB64),
                   state.keysByChannel[request.channelIdHex] != nil {
                    return .joined(channelIdHex: request.channelIdHex)
                }
                let keys = try? await CoPublisher.pollForCoPublishWelcome(
                    ds: ds,
                    identity: identity,
                    accountIdB64: accountIdB64,
                    groupId: request.groupId,
                    joinerKey: request.joinerKey,
                    channelIdHex: request.channelIdHex,
                    // G-29 — reuse the persisted signing seed on a re-poll.
                    existingSigningSeed: state.persistedPublisherSigningSeed(channelId: request.channelIdHex)
                )
                if let keys {
                    try settle(
                        channelIdHex: request.channelIdHex, name: name, description: "", keys: keys,
                        state: state, accountIdB64: accountIdB64)
                    return .joined(channelIdHex: request.channelIdHex)
                }
                try? await Task.sleep(nanoseconds: pollIntervalNs)
            }
            if let accountIdB64,
               !PendingPublisherRequests.isPending(channelIdHex: request.channelIdHex, accountIdB64: accountIdB64),
               state.keysByChannel[request.channelIdHex] != nil {
                return .joined(channelIdHex: request.channelIdHex)
            }
            return .waiting(channelIdHex: request.channelIdHex)
        } catch let failure as Failure {
            throw failure
        } catch let err as LegacyChannelError {
            throw Failure(message: UiText { err.message }, offersRetry: false)
        } catch let err as CoPublisherError {
            // Not a co-publisher link at all: the link itself is the problem,
            // which no retry fixes — the invite page's invalid-link copy.
            NSLog("Channels: co-publish link rejected: %@", String(describing: err))
            throw Failure(message: UiText { groupInviteInvalidMessage }, offersRetry: false, kind: .linkDead)
        } catch {
            NSLog("Channels: co-publish request failed: %@", String(describing: error))
            if Failure.isOffline(error) {
                throw Failure(message: .key("common.error.offline"), offersRetry: true, kind: .offline)
            }
            let classified = JoinRequestFailure.classify(error)
            throw Failure(message: UiText { classified.message }, offersRetry: classified.offersRetry)
        }
    }

    /// A request landed: the channel and its keys, durably, then the record
    /// goes. Throws when the joined group could not be made durable; the
    /// record then stays.
    static func settle(
        channelIdHex: String, name: String, description: String, keys: ChannelPublisherKeys,
        state: ChannelsUiState, accountIdB64: String?,
        persist: (() throws -> Void)? = nil
    ) throws {
        if !state.channels.contains(where: { $0.id == channelIdHex }) {
            state.addChannel(.init(
                id: channelIdHex,
                localName: name,
                localDescription: description,
                addedAtMs: UInt64(Date().timeIntervalSince1970 * 1000)
            ))
        }
        state.setKeys(channelId: channelIdHex, keys: keys)
        // The joined group and the keys on disk before the record goes (and
        // before the mailbox cursor moves past the Welcome).
        if let persist { try persist() } else { try ChannelPublisherIdentity.shared.persistOrThrow() }
        // ADR-0050 section 2.9 (amended 2026-11-13) — the account's other
        // devices list the channel as a co-publisher's (readers there).
        state.onRelationshipStarted?(channelIdHex)
        if let accountIdB64 {
            PendingPublisherRequests.drop(channelIdHex: channelIdHex, accountIdB64: accountIdB64)
        }
    }

    /// The mailbox receive loop's half of a publisher request (ADR-0045
    /// decision 2: no deadline): try one Welcome from the account mailbox
    /// against every pending request. `welcome` is the Welcome itself (a
    /// `welcome` row's body, or a `welcomeRendezvous` row's body). Returns
    /// the channel it landed, or `nil` when the Welcome is no request's
    /// (nothing changes then). Throws when a landed join could not be made
    /// durable (the caller holds its mailbox cursor).
    static func landPendingWelcome(
        _ welcome: Data, accountIdB64: String, identity: Identity, state: ChannelsUiState,
        join: (Identity, Data, Data, String, Data?) throws -> ChannelPublisherKeys? = { identity, welcome, groupId, channelIdHex, seed in
            try CoPublisher.joinCoPublishWelcome(
                identity: identity, accountIdB64: AcceptedContactsStore.boundAccountIdB64, welcome: welcome,
                groupId: groupId, channelIdHex: channelIdHex, existingSigningSeed: seed)
        },
        persist: (() throws -> Void)? = nil
    ) throws -> String? {
        let pending = PendingPublisherRequests.load(accountIdB64: accountIdB64)
        guard !pending.isEmpty else { return nil }
        let ownKey = base64Encode(identity.signaturePublicKey())
        for request in pending {
            // The Welcome is sealed to the knocked KeyPackage, whose private
            // key is in this identity's provider only when the request was
            // made with it.
            guard request.joinerKeyB64 == ownKey,
                  let groupId = CoPublisher.hexToData(request.groupIdHex) else { continue }
            guard let keys = try? join(
                identity, welcome, groupId, request.channelIdHex,
                state.persistedPublisherSigningSeed(channelId: request.channelIdHex))
            else { continue }
            try settle(
                channelIdHex: request.channelIdHex, name: request.name, description: request.description,
                keys: keys, state: state, accountIdB64: accountIdB64, persist: persist)
            return request.channelIdHex
        }
        return nil
    }
}

extension CoPublisherJoin.Failure: Error {}

/// Section 15.2 — `Accept` on a publisher card applies with no further step:
/// the request and the join run from the card, which shows their progress and
/// any failure in place. Held app-wide by link, so a card scrolled away and
/// back shows where its join stands; a request still pending (recorded, the
/// owner has not approved) shows as waiting after a restart too.
@MainActor
final class CoPublisherAcceptance: ObservableObject {
    static let shared = CoPublisherAcceptance()

    enum Phase: Equatable {
        case sending
        case waiting
        case failed(CoPublisherJoin.Failure)
        /// `Ask again` found the link no longer works; the earlier request
        /// stays, so a late approval still lands.
        case linkDead
    }

    /// What the card in the one-to-one shows.
    enum CardState: Equatable {
        /// This device publishes the channel: `Open`.
        case open
        /// No request yet: `Accept`.
        case accept
        /// Being sent: the waiting line, nothing to press.
        case sending
        /// Sent under a day ago: the waiting line. UX 15.2.2 — `Ask again`
        /// shows once a request has waited a day, at most once per channel
        /// per day (asking restarts it): the group join request's rule,
        /// `RequestsSentStore.mayAskAgain(sentAtMs:nowMs:)`.
        case waiting
        /// Sent a day ago or more: "No answer yet." and `Ask again`.
        case noAnswer
        /// The link no longer works.
        case linkDead
        /// A failure, with `Try again` (or `Ask again` when a request was
        /// sent before) when it can help.
        case failed(CoPublisherJoin.Failure, askAgain: Bool)
    }

    /// The card's state, pure. Never tells of a refusal: an unanswered
    /// request reads as waiting, then as "No answer yet.".
    nonisolated static func cardState(
        isPublisher: Bool, phase: Phase?, requestedAtMs: Int64?, nowMs: Int64
    ) -> CardState {
        if isPublisher { return .open }
        switch phase {
        case .sending?: return .sending
        case .linkDead?: return .linkDead
        case .failed(let failure)?:
            if failure.kind == .linkDead, requestedAtMs != nil { return .linkDead }
            return .failed(failure, askAgain: requestedAtMs != nil)
        case .waiting?, nil:
            guard let requestedAtMs else { return phase == nil ? .accept : .waiting }
            return RequestsSentStore.mayAskAgain(sentAtMs: requestedAtMs, nowMs: nowMs)
                ? .noAnswer : .waiting
        }
    }

    @Published private(set) var phases: [String: Phase] = [:]
    /// Moves when a pending request is recorded or lands, so a card redraws.
    @Published private(set) var pendingRevision = 0
    private var running: [String: Task<Void, Never>] = [:]

    /// Set by the signed-in shell.
    var state: ChannelsUiState?
    var seedProvider: () -> Data? = { nil }
    /// Open a channel this device publishes to.
    var openChannel: (String) -> Void = { _ in }

    /// This device already publishes to the channel.
    func isPublisher(_ channelIdHex: String) -> Bool {
        state?.keysByChannel[channelIdHex] != nil
    }

    /// A request to publish to the channel was sent and waits for the owner.
    func isPending(_ channelIdHex: String) -> Bool {
        requestedAtMs(channelIdHex) != nil
    }

    /// When the pending request for the channel was sent, `nil` when none.
    func requestedAtMs(_ channelIdHex: String) -> Int64? {
        _ = pendingRevision
        guard let account = AcceptedContactsStore.boundAccountIdB64 else { return nil }
        return PendingPublisherRequests.load(accountIdB64: account)
            .first { $0.channelIdHex == channelIdHex }?.requestedAtMs
    }

    /// What the card shows now.
    func cardState(_ facts: CoPublisherCardFacts) -> CardState {
        Self.cardState(
            isPublisher: isPublisher(facts.channelIdHex), phase: phases[facts.link.absoluteString],
            requestedAtMs: requestedAtMs(facts.channelIdHex),
            nowMs: Int64(Date().timeIntervalSince1970 * 1000))
    }

    func accept(_ facts: CoPublisherCardFacts) {
        let key = facts.link.absoluteString
        guard running[key] == nil else { return }
        guard let state, let seed = seedProvider() else {
            phases[key] = .failed(.init(message: .key("group.join.request_failed"), offersRetry: true))
            return
        }
        phases[key] = .sending
        running[key] = Task { [weak self] in
            do {
                let outcome = try await CoPublisherJoin.run(
                    link: key, localName: facts.channelName, state: state, accountIdentitySeed: seed,
                    onStage: { stage in
                        self?.phases[key] = stage == .sending ? .sending : .waiting
                        if stage == .waiting { self?.pendingRevision += 1 }
                    })
                self?.running[key] = nil
                self?.pendingRevision += 1
                switch outcome {
                case .joined(let channelId):
                    self?.phases[key] = nil
                    self?.openChannel(channelId)
                case .waiting:
                    // The request stays; the mailbox loop lands it.
                    self?.phases[key] = .waiting
                }
            } catch let failure as CoPublisherJoin.Failure {
                // `Ask again` on a link that no longer works: the earlier
                // request stays (a late approval still lands).
                let askedBefore = self.map { $0.isPending(facts.channelIdHex) } ?? false
                self?.phases[key] = failure.kind == .linkDead && askedBefore ? .linkDead : .failed(failure)
                self?.running[key] = nil
            } catch {
                self?.phases[key] = .failed(.init(message: .key("group.join.request_failed"), offersRetry: true))
                self?.running[key] = nil
            }
        }
    }

    /// The mailbox receive loop: try one Welcome against this account's
    /// pending publisher requests, under the channel provider identity the
    /// requests are made with. Returns whether it landed one. Throws when a
    /// landed join could not be made durable.
    func landPendingWelcome(_ welcome: Data, accountIdB64: String) throws -> Bool {
        guard !PendingPublisherRequests.load(accountIdB64: accountIdB64).isEmpty else { return false }
        // Requests are pending but the shell is not up (or another account is
        // bound): nothing can be joined now, and the Welcome must be read
        // again rather than passed.
        guard let state, let seed = seedProvider(),
              AcceptedContactsStore.boundAccountIdB64 == accountIdB64,
              let identity = try? ChannelPublisherIdentity.shared.identity(currentSeed: seed)
        else { throw ContactsRepository.PublisherWelcomeNotReady() }
        guard try CoPublisherJoin.landPendingWelcome(
            welcome, accountIdB64: accountIdB64, identity: identity, state: state) != nil
        else { return false }
        pendingRevision += 1
        return true
    }

    /// Forget every join (sign-out, account switch).
    func reset() {
        running.values.forEach { $0.cancel() }
        running = [:]
        phases = [:]
        pendingRevision += 1
    }
}
