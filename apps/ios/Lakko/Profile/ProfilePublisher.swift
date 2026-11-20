// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0048 §2 — when this device publishes its own profile into a group.
///
/// Three triggers, and they are not equal:
///
///   * **Joining a group** (a Welcome join, founding a chat, accepting a
///     request): publish now. A group this device just entered has never heard
///     its name.
///   * **Changing the profile**: publish now, into every group. Holding a
///     change back would leave peers on the old name for no gain.
///   * **The group gained a member**: republish, but throttled and jittered
///     per group. A newcomer cannot read earlier epochs, so it would otherwise
///     never learn existing names — and yet every existing member sees the same
///     add at the same moment, so an unjittered republish is a synchronised
///     burst of one message per member.
///
/// ## The growth republish
///
/// At most one per group per ``minIntervalMs``, delayed by a uniform
/// `0...maxJitterMs` on top. A second growth while one is scheduled coalesces
/// into it (the one republish carries the current profile to every newcomer).
/// When the scheduled republish fires it is SKIPPED if any publish to that
/// group completed after the growth was noted: the per-group send path is
/// serialised with the receive path that applied the add, so such a publish
/// was sealed at an epoch that already includes the newcomer.
///
/// The send is the same locked application-message path the other service
/// envelopes use (committer beacon, push routing secret), so a profile can
/// never straddle an epoch change.
@MainActor
final class ProfilePublisher {
    /// Minimum spacing between two publishes to one group caused by growth.
    nonisolated static let minIntervalMs: Int64 = 30_000
    /// Upper bound of the uniform jitter added to a growth republish.
    nonisolated static let maxJitterMs: Int64 = 10_000

    struct Ports {
        /// Seal and publish `bytes` as an MLS application message in the group.
        var send: @MainActor (_ groupIdHex: String, _ bytes: Data) async throws -> Void
        /// The current own-profile envelope for one group, or `nil` when there
        /// is nothing to publish (no active account). Per group because profile
        /// v3 carries that conversation's reach grants (ADR-0060).
        var envelope: @MainActor (_ groupIdHex: String) -> Data?
        /// Whether this device can still send into the group (not evicted).
        var canSend: @MainActor (_ groupIdHex: String) -> Bool
        var nowMs: @MainActor () -> Int64
        /// A uniform value in `0...maxJitterMs`.
        var jitterMs: @MainActor () -> Int64
        var sleep: @MainActor (_ ms: Int64) async -> Void
    }

    private let ports: Ports
    /// Wall-clock time of the last completed publish, per group.
    private var lastPublishedAtMs: [String: Int64] = [:]
    /// Monotone counter stamped on each completed publish, per group — the
    /// "did anything go out after the growth" test uses this, not the clock, so
    /// two events in one millisecond still order.
    private var publishSequence: UInt64 = 0
    private var lastPublishSequence: [String: UInt64] = [:]
    private var scheduled: [String: Task<Void, Never>] = [:]
    /// Groups where this device owes its profile to someone who cannot have
    /// it yet: a growth was seen and the republish has not gone out, or a
    /// publish failed. Cleared by the next publish that lands.
    private var owed: Set<String> = []

    init(ports: Ports) {
        self.ports = ports
    }

    /// What one publish did.
    enum Outcome: Equatable {
        /// The envelope went out.
        case sent
        /// There was nothing to send: the device can no longer send into the
        /// group, or there is no profile to publish.
        case skipped
        /// The send was attempted and did not land.
        case failed
    }

    /// Publish into `groupIdHex` now (join / change). Best-effort: a failed
    /// send is logged and forgotten — the next trigger carries the profile.
    /// The outcome is returned for the one caller that must not forget
    /// (``publishToAllReporting(groupIdHexes:)``).
    @discardableResult
    func publishNow(groupIdHex: String) async -> Outcome {
        guard ports.canSend(groupIdHex), let bytes = ports.envelope(groupIdHex) else { return .skipped }
        do {
            try await ports.send(groupIdHex, bytes)
            publishSequence &+= 1
            lastPublishSequence[groupIdHex] = publishSequence
            lastPublishedAtMs[groupIdHex] = ports.nowMs()
            owed.remove(groupIdHex)
            return .sent
        } catch {
            // ADR-0091 section 2 — a refusal while behind is the stop list
            // working: owed as before, not logged.
            if !ConversationReentryRules.isBehindRefusal(error) {
                NSLog("ADR-0048 profile publish failed: \(error)")
            }
            // Owed: the next message this device sends in the group carries
            // the profile ahead of it (``publishOwedBeforeSend(groupIdHex:)``).
            owed.insert(groupIdHex)
            return .failed
        }
    }

    /// ADR-0074 section 3.5a rule 3 — publish into every group in
    /// `groupIdHexes` now and say which sends did NOT land, so a caller that
    /// owes the re-send (the rotating device, whose new reach grants peers
    /// have no other way to learn) can keep it owed. A group this device can
    /// no longer send into is not a failure.
    func publishToAllReporting(groupIdHexes: [String]) async -> [String] {
        var failed: [String] = []
        for groupIdHex in groupIdHexes where await publishNow(groupIdHex: groupIdHex) == .failed {
            failed.append(groupIdHex)
        }
        return failed
    }

    /// Publish into every group in `groupIdHexes` now (profile change).
    func publishToAll(groupIdHexes: [String]) async {
        for groupIdHex in groupIdHexes {
            await publishNow(groupIdHex: groupIdHex)
        }
    }

    /// The group gained a member. Schedules the throttled, jittered republish,
    /// or coalesces into the one already scheduled. Returns the scheduled task
    /// so a caller (a test) can await it.
    @discardableResult
    func noteGrowth(groupIdHex: String) -> Task<Void, Never>? {
        owed.insert(groupIdHex)
        if let pending = scheduled[groupIdHex] { return pending }
        let delay = Self.growthDelayMs(
            nowMs: ports.nowMs(),
            lastPublishedAtMs: lastPublishedAtMs[groupIdHex],
            jitterMs: min(max(ports.jitterMs(), 0), Self.maxJitterMs)
        )
        let sequenceAtNote = lastPublishSequence[groupIdHex] ?? 0
        let task = Task { @MainActor [weak self] in
            guard let self else { return }
            await self.ports.sleep(delay)
            // A cancelled republish (an owed profile went out ahead of a
            // message) leaves the slot alone: a newer one may hold it.
            guard !Task.isCancelled else { return }
            self.scheduled[groupIdHex] = nil
            if (self.lastPublishSequence[groupIdHex] ?? 0) > sequenceAtNote { return }
            await self.publishNow(groupIdHex: groupIdHex)
        }
        scheduled[groupIdHex] = task
        return task
    }

    /// Whether this device owes `groupIdHex` its profile (a growth not yet
    /// answered, or a publish that failed).
    func isOwed(groupIdHex: String) -> Bool { owed.contains(groupIdHex) }

    /// Called before this device sends its own message into `groupIdHex`: if
    /// the profile is owed there, publish it now, with no throttle and no
    /// jitter, so the newcomer has the name before the message that needs
    /// it. The scheduled republish is cancelled (this publish answers it). A
    /// failed publish stays owed and does not stop the message. The jittered
    /// republish stays the path when this device sends nothing.
    func publishOwedBeforeSend(groupIdHex: String) async {
        guard owed.contains(groupIdHex) else { return }
        scheduled.removeValue(forKey: groupIdHex)?.cancel()
        await publishNow(groupIdHex: groupIdHex)
    }

    /// Cancel everything scheduled and forget all timing (account switch).
    func reset() {
        scheduled.values.forEach { $0.cancel() }
        scheduled = [:]
        owed = []
        lastPublishedAtMs = [:]
        lastPublishSequence = [:]
    }

    /// How long to wait before a growth republish: whatever remains of the
    /// ``minIntervalMs`` since the last publish (nothing when there was none),
    /// plus the jitter.
    nonisolated static func growthDelayMs(nowMs: Int64, lastPublishedAtMs: Int64?, jitterMs: Int64) -> Int64 {
        let throttle: Int64
        if let last = lastPublishedAtMs {
            // Clamped above too: a wall clock that stepped backwards must not
            // turn the throttle into an arbitrarily long silence.
            throttle = min(max(0, last &+ minIntervalMs &- nowMs), minIntervalMs)
        } else {
            throttle = 0
        }
        return throttle &+ jitterMs
    }
}

/// Did the group GAIN a member between two roster snapshots?
///
/// A set difference over member signature keys, never a count: a poll batch can
/// carry a Remove and an Add together, leaving the count unchanged while a
/// newcomer who has never heard anyone's name is now in the group. An
/// unreadable snapshot (`nil`) is never evidence of growth.
func membershipGrew(previousMemberKeys: [String]?, currentMemberKeys: [String]?) -> Bool {
    guard let previous = previousMemberKeys, let current = currentMemberKeys else { return false }
    return !Set(current).subtracting(previous).isEmpty
}
