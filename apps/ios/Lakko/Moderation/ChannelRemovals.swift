// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import os

/// ADR-0089 step 5 / UX `reporting-and-moderation.md` section 3 ("A channel
/// that was removed") — the channels this device was told are removed.
///
/// The channels service answers every route of a channel moderators removed
/// with `410 {"code":"channel-removed"}`. Unlike `channel-deleted`, nothing
/// ends here: the channel stays in the list, the posts this device holds stay
/// readable, and only the write controls go. Which line is shown is decided
/// by the moderation log alone (C-92): ``Status/removed`` only after
/// ``SuspensionMarks/channelCheck(_:)`` verified the removal, else
/// ``Status/unavailable``.
///
/// A removed channel's requests are held on the device (``ChannelRemovalGate``)
/// until the standing's reuse period ends, so the network is asked once per
/// period. A channel that answers normally again (a restore) is forgotten:
/// everything comes back with no trace. Memory only.
@MainActor
public final class ChannelRemovals: ObservableObject {

    public static let shared: ChannelRemovals = ChannelRemovals(
        gate: .shared,
        check: { await SuspensionMarks.shared.channelCheck($0) },
        forget: { SuspensionMarks.shared.forgetChannel($0) })

    /// What a removed channel shows.
    public enum Status: Equatable {
        /// The removal is being looked up in the moderation log.
        case checking
        /// The log verified the removal: `suspended.channel_removed`.
        case removed
        /// Not verified (no pin, no network, an answer that does not
        /// verify): `channels.unavailable`.
        case unavailable

        /// The catalog key of the line the conversation and the info show.
        public var lineKey: String {
            switch self {
            case .removed: return "suspended.channel_removed"
            case .checking, .unavailable: return "channels.unavailable"
            }
        }
    }

    /// Channels the service answered `channel-removed` for, by lower-case
    /// channel id hex.
    @Published public private(set) var statuses: [String: Status] = [:]

    private let gate: ChannelRemovalGate
    private let check: @MainActor (Data) async -> (standing: SuspensionMarks.Standing, recheckAtMs: Int64)
    private let forget: @MainActor (Data) -> Void
    private var checking: Set<String> = []
    private var generation: UInt64 = 0

    init(
        gate: ChannelRemovalGate,
        check: @escaping @MainActor (Data) async -> (standing: SuspensionMarks.Standing, recheckAtMs: Int64),
        forget: @escaping @MainActor (Data) -> Void
    ) {
        self.gate = gate
        self.check = check
        self.forget = forget
    }

    /// The status of `channelId` (hex), `nil` while it answers normally.
    public func status(_ channelId: String) -> Status? { statuses[channelId.lowercased()] }

    /// Whether `channelId` is held as removed: write controls are not offered.
    public func isRemoved(_ channelId: String) -> Bool { status(channelId) != nil }

    /// The service answered `410 channel-removed` for `channelId`: ask the
    /// log once, then hold the channel's requests until the answer's reuse
    /// period ends.
    public func noteRemoved(_ channelId: String) {
        let id = channelId.lowercased()
        if statuses[id] == nil { statuses[id] = .checking }
        guard !checking.contains(id), let bytes = ChannelsClient.hexToData(id), bytes.count == 16 else { return }
        checking.insert(id)
        let started = generation
        Task { @MainActor in
            let verdict = await check(bytes)
            guard generation == started else { return }
            checking.remove(id)
            // Answered normally while the log was asked: nothing to show.
            guard statuses[id] != nil else { return }
            let next: Status = verdict.standing.isSuspended ? .removed : .unavailable
            if statuses[id] != next { statuses[id] = next }
            gate.hold(id, untilMs: verdict.recheckAtMs)
            Self.log.info("channel removal checked")
        }
    }

    /// `channelId` answered normally: a restore leaves no trace.
    public func noteAnswered(_ channelId: String) {
        let id = channelId.lowercased()
        gate.release(id)
        guard statuses[id] != nil else { return }
        statuses[id] = nil
        if let bytes = ChannelsClient.hexToData(id) { forget(bytes) }
    }

    /// Drop everything held (sign-out, account switch).
    public func reset() {
        generation &+= 1
        checking = []
        gate.releaseAll()
        if !statuses.isEmpty { statuses = [:] }
    }

    private static let log = Logger(subsystem: "me.lakko.app", category: "channel-removals")
}

/// The network half of ``ChannelRemovals``: every channels request passes
/// through ``perform(_:send:)``. A request for a channel held as removed is
/// answered on the device with the same `channel-removed` error, without
/// reaching the network, until the hold ends; a `410 channel-removed` answer
/// starts a hold, any success for the channel ends it. Thread-safe.
public final class ChannelRemovalGate: @unchecked Sendable {

    public static let shared: ChannelRemovalGate = ChannelRemovalGate(
        onRemoved: { id in Task { @MainActor in ChannelRemovals.shared.noteRemoved(id) } },
        onAnswered: { id in Task { @MainActor in ChannelRemovals.shared.noteAnswered(id) } })

    private let lock = NSLock()
    /// Channel id hex -> when its requests go out again (unix ms).
    private var holds: [String: Int64] = [:]
    private let onRemoved: @Sendable (String) -> Void
    private let onAnswered: @Sendable (String) -> Void
    private let nowMs: @Sendable () -> Int64

    init(
        onRemoved: @escaping @Sendable (String) -> Void,
        onAnswered: @escaping @Sendable (String) -> Void,
        nowMs: @escaping @Sendable () -> Int64 = { Int64(Date().timeIntervalSince1970 * 1000) }
    ) {
        self.onRemoved = onRemoved
        self.onAnswered = onAnswered
        self.nowMs = nowMs
    }

    /// Whether requests for `channelId` are held now.
    public func isHeld(_ channelId: String) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard let until = holds[channelId.lowercased()] else { return false }
        return nowMs() < until
    }

    /// Hold `channelId`'s requests until `untilMs`.
    public func hold(_ channelId: String, untilMs: Int64) {
        lock.lock()
        holds[channelId.lowercased()] = untilMs
        lock.unlock()
    }

    /// End `channelId`'s hold; returns whether one was there.
    @discardableResult
    public func release(_ channelId: String) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return holds.removeValue(forKey: channelId.lowercased()) != nil
    }

    public func releaseAll() {
        lock.lock()
        holds = [:]
        lock.unlock()
    }

    /// Send `request` through `send` unless its channel is held.
    func perform(
        _ request: URLRequest,
        send: (URLRequest) async throws -> (Data, URLResponse)
    ) async throws -> (Data, URLResponse) {
        guard let id = Self.channelId(of: request.url) else { return try await send(request) }
        if isHeld(id) {
            throw ChannelsApiError(
                status: 410, code: ChannelsApiError.channelRemovedCode, detail: "held on this device")
        }
        let (data, response) = try await send(request)
        if let http = response as? HTTPURLResponse {
            if http.statusCode == 410, Self.isRemovedAnswer(data) {
                // A provisional hold until the log answers.
                hold(id, untilMs: nowMs() + SuspensionMarks.failureBackoffMs)
                onRemoved(id)
            } else if (200..<300).contains(http.statusCode) {
                // A hold stays recorded after it ends, so the one check of
                // the period that answers normally finds it here; a channel
                // never held costs no hop to the main actor.
                if release(id) { onAnswered(id) }
            }
        }
        return (data, response)
    }

    /// The 32-hex channel id a channels request names
    /// (`.../v1/channels/{id}...`), lower-cased; `nil` for any other route.
    static func channelId(of url: URL?) -> String? {
        guard let path = url?.path, let range = path.range(of: "/v1/channels/") else { return nil }
        let rest = path[range.upperBound...]
        let candidate = rest.prefix { $0 != "/" }
        guard candidate.count == 32, candidate.allSatisfy(\.isHexDigit) else { return nil }
        return candidate.lowercased()
    }

    /// Whether a 410 body is `{"code":"channel-removed", ...}`.
    static func isRemovedAnswer(_ data: Data) -> Bool {
        struct Envelope: Decodable { let code: String }
        return (try? JSONDecoder().decode(Envelope.self, from: data))?.code == ChannelsApiError.channelRemovedCode
    }
}

/// ADR-0089 step 5 — what a removed channel's info offers, decided here so
/// the tests can run it: the publisher tools, the invitations and the reads
/// the service no longer answers go, and so does Report; the `@name`, the
/// description, the members-can-find-me switch and Unsubscribe stay. The owner's rename
/// becomes the plain description, and the owner or a publisher, who has no
/// Unsubscribe, gets the same leave control (it ends the channel on every
/// device of the account, without asking the service); Delete stays hidden.
public enum ChannelRemovalRules {
    /// The only blocks a removed channel's info keeps.
    static let kept: Set<InfoSection> = [.handle, .channelDescription, .membersCanFindMe, .unsubscribe]

    public static func infoSections(_ sections: [InfoSection], removed: Bool) -> [InfoSection] {
        guard removed else { return sections }
        var out: [InfoSection] = []
        for section in sections {
            if section == .renameGroup, !sections.contains(.channelDescription) {
                out.append(.channelDescription)
            } else if kept.contains(section) {
                out.append(section)
            }
        }
        if !out.contains(.unsubscribe) { out.append(.unsubscribe) }
        return out
    }
}
