// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import SwiftUI

/// ADR-0028 OQ3 / ADR-0087 section 15.2 — the joiner's whole co-publisher
/// join: ask to join through a co-publisher link, wait for the owner's
/// Welcome, mint local publisher keys and add the channel. One path for the
/// publisher card's `Accept` and the link screen.
@MainActor
enum CoPublisherJoin {
    /// Where a join stands, for the progress line.
    enum Stage { case sending, waiting }

    /// Why a join did not finish: the line to show and whether `Try again`
    /// can help.
    struct Failure: Equatable {
        let message: UiText
        let offersRetry: Bool
    }

    /// Poll attempts (every 3 s) before giving up: the web 60 x 3 s window.
    static let pollAttempts = 60
    static let pollIntervalNs: UInt64 = 3 * 1_000_000_000

    /// Run the join. Returns the channel id. Throws ``Failure``.
    static func run(
        link: String, localName: String, state: ChannelsUiState, accountIdentitySeed: Data,
        ds: DsClient = DsClient(), onStage: (Stage) -> Void
    ) async throws -> String {
        // The account this request is made for, before any wait: the
        // co-publisher mark is filed under it, and under no account signed in
        // while the request was out.
        let accountIdB64 = AcceptedContactsStore.boundAccountIdB64
        do {
            onStage(.sending)
            // The durable channel provider (G-50): the joined PublisherGroup
            // lands in it and is snapshotted once the join is done.
            let identity = try ChannelPublisherIdentity.shared.identity(currentSeed: accountIdentitySeed)
            let request = try await CoPublisher.requestToCoPublish(
                ds: ds, identity: identity, accountIdB64: accountIdB64, link: link)
            onStage(.waiting)
            var keys: ChannelPublisherKeys?
            for _ in 0..<pollAttempts where keys == nil {
                try Task.checkCancellation()
                keys = try await CoPublisher.pollForCoPublishWelcome(
                    ds: ds,
                    identity: identity,
                    accountIdB64: accountIdB64,
                    groupId: request.groupId,
                    joinerKey: request.joinerKey,
                    channelIdHex: request.channelIdHex,
                    // G-29 — reuse the persisted signing seed on a re-poll.
                    existingSigningSeed: state.persistedPublisherSigningSeed(channelId: request.channelIdHex)
                )
                if keys == nil {
                    try? await Task.sleep(nanoseconds: pollIntervalNs)
                }
            }
            guard let keys else {
                throw Failure(message: .key("channels.join.no_approval"), offersRetry: true)
            }
            ChannelPublisherIdentity.shared.persist()
            if !state.channels.contains(where: { $0.id == request.channelIdHex }) {
                state.addChannel(.init(
                    id: request.channelIdHex,
                    localName: localName.trimmingCharacters(in: .whitespaces),
                    localDescription: "",
                    addedAtMs: UInt64(Date().timeIntervalSince1970 * 1000)
                ))
            }
            state.setKeys(channelId: request.channelIdHex, keys: keys)
            return request.channelIdHex
        } catch let failure as Failure {
            throw failure
        } catch let err as LegacyChannelError {
            throw Failure(message: UiText { err.message }, offersRetry: false)
        } catch let err as CoPublisherError {
            // Not a co-publisher link at all: the link itself is the problem,
            // which no retry fixes — the invite page's invalid-link copy.
            NSLog("Channels: co-publish link rejected: %@", String(describing: err))
            throw Failure(message: UiText { groupInviteInvalidMessage }, offersRetry: false)
        } catch {
            NSLog("Channels: co-publish request failed: %@", String(describing: error))
            let classified = JoinRequestFailure.classify(error)
            throw Failure(message: UiText { classified.message }, offersRetry: classified.offersRetry)
        }
    }
}

extension CoPublisherJoin.Failure: Error {}

/// Section 15.2 — `Accept` on a publisher card applies with no further step:
/// the request and the join run from the card, which shows their progress and
/// any failure in place. Held app-wide by link, so a card scrolled away and
/// back shows where its join stands.
@MainActor
final class CoPublisherAcceptance: ObservableObject {
    static let shared = CoPublisherAcceptance()

    enum Phase: Equatable {
        case sending
        case waiting
        case failed(CoPublisherJoin.Failure)
    }

    @Published private(set) var phases: [String: Phase] = [:]
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
                let channelId = try await CoPublisherJoin.run(
                    link: key, localName: facts.channelName, state: state, accountIdentitySeed: seed,
                    onStage: { stage in self?.phases[key] = stage == .sending ? .sending : .waiting })
                self?.phases[key] = nil
                self?.running[key] = nil
                self?.openChannel(channelId)
            } catch let failure as CoPublisherJoin.Failure {
                self?.phases[key] = .failed(failure)
                self?.running[key] = nil
            } catch {
                self?.phases[key] = .failed(.init(message: .key("group.join.request_failed"), offersRetry: true))
                self?.running[key] = nil
            }
        }
    }

    /// Forget every join (sign-out, account switch).
    func reset() {
        running.values.forEach { $0.cancel() }
        running = [:]
        phases = [:]
    }
}
