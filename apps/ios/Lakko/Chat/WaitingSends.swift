// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation

/// `docs/ux/connection-state.md` section 7 — the rules for a message that did
/// not leave because of the network: it waits, and goes by itself when the
/// servers answer again. Pure.
enum WaitingSends {
    /// The automatic sends a waiting message gets before it shows
    /// `Not sent · Retry` (section 7.2). Chosen, not measured.
    static let maxAutoAttempts = 5

    /// When a send fails for the network while the state still reads online
    /// (another request got an answer in between), the pass runs this long
    /// after. Chosen, not measured; the same value as web and Android.
    static let waitingWhileOnlineRecheckMs: UInt64 = 3_000

    /// Whether a failed send got no HTTP answer at all (section 2), looking
    /// through the wrappers the chat layer and the DS client put around it. A
    /// cancellation is not one; an HTTP status (a refusal, a suspended
    /// account), a membership refusal, the ADR-0091 "behind" refusal, a
    /// compose failure and a changed poll ring are not either.
    static func isNetworkFailure(_ error: Error) -> Bool {
        var current: Error = error
        for _ in 0..<8 {
            if ConnectionState.isNetworkFailure(current) { return true }
            if let ds = current as? DsClientError, case .network(_, let underlying) = ds {
                current = underlying
                continue
            }
            if let chat = current as? ChatError, case .wrapped(_, let underlying) = chat {
                current = underlying
                continue
            }
            if let storage = current as? StorageClientError, case .network(_, let underlying) = storage {
                current = underlying
                continue
            }
            if let bridge = current as? AttachmentBridgeError, case .upload(let underlying) = bridge {
                current = underlying
                continue
            }
            // Section 7.3 — a GIF fetch that got no answer.
            if let gif = current as? OutboxGifFetchError { return gif.isNetwork }
            return false
        }
        return false
    }

    /// The state of a waiting row whose automatic send failed. `attempts` is
    /// how many automatic sends it has had, this one included.
    static func ackAfterAutomaticFailure(network: Bool, attempts: Int) -> ChatMessage.Ack {
        guard network else { return .failed }
        return attempts >= maxAutoAttempts ? .failed : .waiting
    }

    /// Whether a message being sent now must join the queue instead of being
    /// published: the conversation holds another waiting message, or its pass
    /// is running.
    static func mustQueueBehind(waitingLocalIds: [Int64], excluding localId: Int64, passRunning: Bool) -> Bool {
        passRunning || waitingLocalIds.contains { $0 != localId }
    }
}

/// Runs the passes of section 7.2: per conversation, oldest first, one at a
/// time, stopping at the first message that fails again for the network.
/// One pass per conversation at a time; a message is tried at most once per
/// pass, so a pass always ends.
@MainActor
final class WaitingSendRunner {
    /// What one send of a waiting message came to.
    enum Outcome: Equatable {
        /// It went (or was taken over by another queue).
        case sent
        /// No answer again: the pass stops, the rest keep waiting.
        case network
        /// Refused for any other reason: it shows `Not sent`, the pass goes on.
        case refused
    }

    struct Ports {
        /// The conversation's oldest waiting message not in `excluding`.
        var nextWaiting: @MainActor (_ conversation: String, _ excluding: Set<Int64>) -> Int64?
        /// Send it once, settling the row.
        var attempt: @MainActor (_ conversation: String, _ localId: Int64) async -> Outcome
    }

    private let ports: Ports
    private(set) var running: Set<String> = []

    init(ports: Ports) {
        self.ports = ports
    }

    func isRunning(_ conversation: String) -> Bool { running.contains(conversation) }

    /// Start the conversation's pass, unless one is running (a running pass
    /// re-reads the waiting messages after every send, so it picks up what
    /// was queued meanwhile). Returns the started pass, for tests.
    @discardableResult
    func run(_ conversation: String) -> Task<Void, Never>? {
        guard !running.contains(conversation) else { return nil }
        running.insert(conversation)
        let ports = ports
        return Task { @MainActor [weak self] in
            var tried: Set<Int64> = []
            while let localId = ports.nextWaiting(conversation, tried) {
                tried.insert(localId)
                if await ports.attempt(conversation, localId) == .network { break }
            }
            self?.running.remove(conversation)
        }
    }
}

extension ChatRepository {
    func waitingSendPorts() -> WaitingSendRunner.Ports {
        WaitingSendRunner.Ports(
            nextWaiting: { [weak self] conversation, excluding in
                self?.groups[conversation]?.waitingLocalIds.first { !excluding.contains($0) }
            },
            attempt: { [weak self] conversation, localId in
                guard let self else { return .refused }
                return await self.attemptWaitingSend(groupIdHex: conversation, localId: localId)
            }
        )
    }

    /// Section 7.2 — follow the connection state: entering online from
    /// Waiting or Connecting starts a pass in every conversation that holds
    /// a waiting message. The OS signal alone never does.
    func watchConnection(_ phases: AnyPublisher<ConnectionPhase, Never>) {
        var previous: ConnectionPhase?
        connectionWatch = phases
            .receive(on: DispatchQueue.main)
            .sink { [weak self] phase in
                MainActor.assumeIsolated {
                    defer { previous = phase }
                    guard phase == .online, let before = previous, before != .online else { return }
                    self?.runAllWaitingSends()
                }
            }
    }

    /// Section 7.2 — a first send or a tap just left `groupIdHex`'s message
    /// waiting. The failure normally turns the state to waiting and the pass
    /// runs on the way back to online; if the state already reads online,
    /// look again shortly.
    func recheckWaitingIfOnline(groupIdHex: String) {
        guard connectionPhaseNow() == .online else { return }
        Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: WaitingSends.waitingWhileOnlineRecheckMs * 1_000_000)
            guard let self, self.connectionPhaseNow() == .online else { return }
            self.waitingSends.run(groupIdHex)
        }
    }

    /// Start the pass of every conversation that holds a waiting message.
    @discardableResult
    func runAllWaitingSends() -> [Task<Void, Never>] {
        groups.keys.sorted().compactMap { hex in
            guard groups[hex]?.waitingLocalIds.isEmpty == false else { return nil }
            return waitingSends.run(hex)
        }
    }

    /// Start one conversation's pass now (a tap on a waiting message).
    @discardableResult
    func runWaitingSends(groupIdHex: String) -> Task<Void, Never>? {
        waitingSends.run(groupIdHex)
    }

    /// Section 7.2 — put the row being sent at the end of the queue when the
    /// conversation holds waiting messages or its pass runs. Returns whether
    /// it did. The pass is started when the servers are answering (otherwise
    /// the next transition to online starts it).
    func queueBehindWaiting(groupIdHex: String, record: GroupRecord, localId: Int64) -> Bool {
        guard WaitingSends.mustQueueBehind(
            waitingLocalIds: record.waitingLocalIds, excluding: localId,
            passRunning: waitingSends.isRunning(groupIdHex))
        else { return false }
        record.markWaiting(localId: localId, resetAttempts: true)
        state = state.with(threads: buildThreadList())
        if connectionPhaseNow() == .online || waitingSends.isRunning(groupIdHex) {
            waitingSends.run(groupIdHex)
        }
        return true
    }

    /// One automatic send of a waiting row. Never throws: the row is settled
    /// here (sent, waiting again, or `Not sent` after the last attempt or a
    /// refusal).
    func attemptWaitingSend(groupIdHex: String, localId: Int64) async -> WaitingSendRunner.Outcome {
        guard let record = groups[groupIdHex],
              let row = record.messages.first(where: { $0.localId == localId }),
              row.ack == .waiting
        else { return .refused }
        // Section 7.3 — a media message's upload is the first part of its
        // send, and counts as the attempt.
        var ready = row
        if !row.outboxMedia.isEmpty {
            record.noteAutoSendAttempt(localId: localId)
            do {
                ready = try await uploadOutboxMediaIfNeeded(groupIdHex: groupIdHex, localId: localId)
            } catch {
                return settleAutomaticFailure(groupIdHex: groupIdHex, localId: localId, error: error)
            }
        }
        // ADR-0073 R.7 case 1 — a one-to-one whose successor does not exist
        // yet holds the message instead; the held queue sends it.
        if sendRoute(groupIdHex: groupIdHex) == .hold {
            let held = holdMessage(
                groupIdHex: groupIdHex, text: ready.text, attachments: ready.attachments,
                reply: ready.reply, forward: ready.forward,
                content: ready.content.flatMap(ContentModel.content), existingLocalId: localId)
            if held { return .sent }
            groups[groupIdHex]?.markFailed(localId: localId)
            state = state.with(threads: buildThreadList())
            return .refused
        }
        guard let live = groups[groupIdHex] else { return .refused }
        if row.outboxMedia.isEmpty { live.noteAutoSendAttempt(localId: localId) }
        do {
            try await publishUnsentRow(groupIdHex: groupIdHex, record: live, row: ready)
            return .sent
        } catch {
            return settleAutomaticFailure(groupIdHex: groupIdHex, localId: localId, error: error)
        }
    }

    /// An automatic send did not go: waiting again until the last attempt,
    /// `Not sent` after it or on a refusal.
    func settleAutomaticFailure(groupIdHex: String, localId: Int64, error: Error) -> WaitingSendRunner.Outcome {
        let network = WaitingSends.isNetworkFailure(error)
        guard let record = groups[groupIdHex],
              let row = record.messages.first(where: { $0.localId == localId && $0.messageId <= 0 })
        else { return network ? .network : .refused }
        switch WaitingSends.ackAfterAutomaticFailure(network: network, attempts: row.autoSendAttempts) {
        case .waiting: record.markWaiting(localId: localId, resetAttempts: false)
        default: record.markFailed(localId: localId)
        }
        state = state.with(threads: buildThreadList())
        NSLog("[chat] waiting message not sent (%@)", network ? "network" : "refused")
        return network ? .network : .refused
    }
}
