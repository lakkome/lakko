// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0052 decision 3 / UX spec section 4.1 "Growth" — when this device
/// RE-SENDS a conversation's shared pins.
///
/// A pin itself is sent by the pin path (awaited, applied locally only after
/// the send). This type owns the one other trigger: **the conversation gained
/// a member**. A newcomer cannot read the epoch a pin was sent in, so a device
/// that may set shared pins re-sends the CURRENT pinned set — every envelope
/// with its ORIGINAL stamp verbatim, never a fresh clock read, so a re-send
/// can never out-order an unpin that landed in between (`pinIsNewer` is
/// strictly greater, so on everyone who already holds the pin the re-send is
/// `acceptIdentical`). Unpins are not re-sent: a newcomer holds nothing to
/// undo.
///
/// The throttle and the jitter are ``ProfilePublisher``'s, as for the group
/// name and for the same reason: every existing member sees the same add at
/// the same moment, so an unjittered re-send is a synchronised burst. At most
/// one re-send per group per ``ProfilePublisher/minIntervalMs``, delayed by a
/// uniform `0...maxJitterMs`; a second growth while one is scheduled coalesces
/// into it; the scheduled re-send is skipped when a pin went out to that group
/// after the growth was noted.
@MainActor
final class PinPublisher {

    struct Ports {
        /// Seal and publish `bytes` as an MLS application message in the group.
        var send: @MainActor (_ groupIdHex: String, _ bytes: Data) async throws -> Void
        /// The group's current PINNED envelopes with their STORED stamps, or
        /// an empty list when this device holds none, may not set shared pins
        /// there, or can no longer send into it.
        var envelopes: @MainActor (_ groupIdHex: String) -> [Data]
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

    /// Record that a pin went out through the pin path, so a growth noted
    /// before it does not re-send on top of it.
    func notePublished(groupIdHex: String) {
        publishSequence &+= 1
        lastPublishSequence[groupIdHex] = publishSequence
        lastPublishedAtMs[groupIdHex] = ports.nowMs()
    }

    /// Re-send the stored pins into `groupIdHex` now, one envelope each, in
    /// order. Best-effort: a failed send is logged and the rest still go; the
    /// next growth re-sends again.
    func publishNow(groupIdHex: String) async {
        let envelopes = ports.envelopes(groupIdHex)
        guard !envelopes.isEmpty else { return }
        var anySent = false
        for bytes in envelopes {
            do {
                try await ports.send(groupIdHex, bytes)
                anySent = true
            } catch {
                // ADR-0091 section 2 — refused while behind: not a failure.
                if !ConversationReentryRules.isBehindRefusal(error) {
                    NSLog("ADR-0052 pin re-send failed: \(error)")
                }
            }
        }
        if anySent { notePublished(groupIdHex: groupIdHex) }
    }

    /// The conversation gained a member. Schedules the throttled, jittered
    /// re-send, or coalesces into the one already scheduled. Returns the task
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
