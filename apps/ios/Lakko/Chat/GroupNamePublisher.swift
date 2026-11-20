// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0051 §3 — when this device RE-SENDS a group's shared name.
///
/// The name itself is sent by the rename path (awaited, so a failed send is
/// surfaced and the local title does not change) and right after the creating
/// commit. This type owns the third trigger only: **the group gained a
/// member**. A newcomer cannot read the epoch the name was sent in, so an
/// owner or admin that holds the name re-sends it — with the ORIGINAL stamp
/// verbatim, never a fresh clock read, so the re-send can never out-order a
/// rename that landed in between (ADR-0044's lesson; `groupNameIsNewer` is
/// strictly greater, so the re-send is `acceptIdentical` on everyone who
/// already holds it).
///
/// The throttle and the jitter are ``ProfilePublisher``'s, for the same
/// reason: every existing member sees the same add at the same moment, so an
/// unjittered re-send is a synchronised burst. At most one per group per
/// ``ProfilePublisher/minIntervalMs``, delayed by a uniform
/// `0...maxJitterMs`; a second growth while one is scheduled coalesces into
/// it; the scheduled re-send is skipped when a send to that group completed
/// after the growth was noted (that send was sealed at an epoch that already
/// includes the newcomer).
///
/// The send is the same locked application-message path the other service
/// envelopes take, so the name can never straddle an epoch change.
@MainActor
final class GroupNamePublisher {

    struct Ports {
        /// Seal and publish `bytes` as an MLS application message in the group.
        var send: @MainActor (_ groupIdHex: String, _ bytes: Data) async throws -> Void
        /// The group's current name envelope with its STORED stamp, or `nil`
        /// when this device holds no name for the group, is not an owner or
        /// admin of it, or can no longer send into it. Per group, unlike the
        /// profile publisher's one own-profile envelope.
        var envelope: @MainActor (_ groupIdHex: String) -> Data?
        var nowMs: @MainActor () -> Int64
        /// A uniform value in `0...ProfilePublisher.maxJitterMs`.
        var jitterMs: @MainActor () -> Int64
        var sleep: @MainActor (_ ms: Int64) async -> Void
    }

    private let ports: Ports
    private var lastPublishedAtMs: [String: Int64] = [:]
    private var publishSequence: UInt64 = 0
    private var lastPublishSequence: [String: UInt64] = [:]
    private var scheduled: [String: Task<Void, Never>] = [:]

    init(ports: Ports) {
        self.ports = ports
    }

    /// Record that a name went out through another path (create / rename), so
    /// a growth noted before it does not re-send on top of it.
    func notePublished(groupIdHex: String) {
        publishSequence &+= 1
        lastPublishSequence[groupIdHex] = publishSequence
        lastPublishedAtMs[groupIdHex] = ports.nowMs()
    }

    /// Re-send the stored name into `groupIdHex` now. Best-effort: a failed
    /// send is logged and forgotten — the next growth re-sends again.
    func publishNow(groupIdHex: String) async {
        guard let bytes = ports.envelope(groupIdHex) else { return }
        do {
            try await ports.send(groupIdHex, bytes)
            notePublished(groupIdHex: groupIdHex)
        } catch {
            // ADR-0091 section 2 — refused while behind: not a failure.
            guard !ConversationReentryRules.isBehindRefusal(error) else { return }
            NSLog("ADR-0051 group name re-send failed: \(error)")
        }
    }

    /// The group gained a member. Schedules the throttled, jittered re-send,
    /// or coalesces into the one already scheduled. Returns the scheduled task
    /// so a caller (a test) can await it.
    @discardableResult
    func noteGrowth(groupIdHex: String) -> Task<Void, Never>? {
        if let pending = scheduled[groupIdHex] { return pending }
        let delay = ProfilePublisher.growthDelayMs(
            nowMs: ports.nowMs(),
            lastPublishedAtMs: lastPublishedAtMs[groupIdHex],
            jitterMs: min(max(ports.jitterMs(), 0), ProfilePublisher.maxJitterMs)
        )
        let sequenceAtNote = lastPublishSequence[groupIdHex] ?? 0
        let task = Task { @MainActor [weak self] in
            guard let self else { return }
            await self.ports.sleep(delay)
            self.scheduled[groupIdHex] = nil
            guard !Task.isCancelled else { return }
            if (self.lastPublishSequence[groupIdHex] ?? 0) > sequenceAtNote { return }
            await self.publishNow(groupIdHex: groupIdHex)
        }
        scheduled[groupIdHex] = task
        return task
    }

    /// Cancel everything scheduled and forget all timing (account switch).
    func reset() {
        scheduled.values.forEach { $0.cancel() }
        scheduled = [:]
        lastPublishedAtMs = [:]
        lastPublishSequence = [:]
    }
}
