// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Receives every joined group the user does NOT have open, while the app is
/// in the foreground.
///
/// ## The defect
///
/// Before this, iOS received a group's journal only while that group's chat
/// screen was on screen (its `.task` long poll). `ChatRepository.pollAllGroups`
/// existed but had no production caller, and nothing else polled: the chat
/// list, Home and Contacts screens read the repository's in-memory state and
/// never fetched. So a message, a 1x1 call offer or a group-call announcement
/// for any other chat stayed on the DS until the user opened that chat, and an
/// incoming call rang only if the caller's chat happened to be the one open.
/// (Backgrounded, the NotificationServiceExtension previews a message but never
/// writes MLS state, C-59, so it does not change this.)
///
/// ## Why short polls in a round robin, not a long poll per group
///
/// A long poll per group holds one connection per chat for its whole window,
/// beside the open chat's own long poll, the contacts inbox loop and the
/// DeviceGroup sync loop. The foreground receive instead spends a small, fixed
/// budget:
///
///   * **Short polls** (`waitSecs` 0) — one round trip each.
///   * **Round robin**: one sweep over every eligible group, then a pause of
///     ``Config/intervalMs`` plus or minus ``Config/jitterMs``, so two devices
///     on one network do not poll in lockstep.
///   * **Bounded concurrency** (``Config/concurrency``) whatever the number of
///     groups.
///   * **Immediate re-poll** of a group that just yielded rows, ahead of the
///     rest of the sweep, capped per sweep — a caller's offer followed by its
///     ICE candidates drains without waiting an interval per page, and one busy
///     chat cannot starve the others.
///   * **Failure backoff** per group, so a journal that keeps failing is not
///     retried every sweep.
///
/// A live call's group gets a dedicated long poll for the duration of the call
/// (``Ports/callGroup``) when its chat is not open: for a 1x1 call an answer or
/// ICE candidate delayed by a sweep interval stalls the connection, and for a
/// group call a commit delayed by one leaves media sealed under a key the
/// newest members no longer derive.
///
/// ## Cadence (2026-11-06)
///
/// A round robin every few seconds over every group is what the web client
/// was measured doing on 2026-11-05 (about 2.5 requests a second idle with six
/// groups), which does not scale. The pause is short (``Config/intervalMs``)
/// only while something is known to be arriving: a push wake reached the app
/// in the foreground (a window of ``Config/wakeWindowMs``), or a call is up.
/// Otherwise it is ``Config/idlePushIntervalMs`` when this device holds an
/// APNs token (the push is what brings a message in; the sweep is a safety
/// net) and ``Config/idleNoPushIntervalMs`` when it holds none. A push wake
/// and the return to the foreground end a long pause at once (``nudge()``),
/// so the long pause costs no latency on those paths. The open chat long-polls
/// itself and is not in the round robin. All three intervals chosen, not
/// measured; the web client uses the same three.
///
/// ## Pausing
///
/// Backgrounding pauses the loop between polls; it does not cancel a poll in
/// flight. A poll's tail may be running an ADR-0045 committer sweep, and
/// cancelling one mid-publish would leave a verdict owed for no reason. On
/// returning to the foreground the loop sweeps at once, so an offer that
/// arrived while backgrounded rings if it is still fresh (inside the ring
/// window by its DS row time, ``CallHostRouting/ringWindowMs``).
///
/// ## What this does NOT own
///
/// Correctness under overlap. `poll` is `ChatRepository.pollGroup`, which
/// applies under the per-group commit lock, re-reads the cursor under it, drops
/// a batch whose cursor moved, and persists before advancing (C-67). A
/// background poll racing the open chat's poll or a membership commit is made
/// safe there, not prevented here.
@MainActor
final class ForegroundReceiveScheduler {

    struct Ports {
        /// Every group that may be received, in a stable order. Re-read at the
        /// start of each sweep.
        var groups: @MainActor () -> [String]
        /// `true` for a group this scheduler must not poll right now (its chat
        /// is open and long-polls itself). Re-checked before each poll.
        var isExcluded: @MainActor (_ groupIdHex: String) -> Bool
        /// One poll. Returns `true` when it consumed rows (so more may follow).
        var poll: @MainActor (_ groupIdHex: String, _ waitSecs: Int) async throws -> Bool
        /// The group of a live 1x1 or group call, if any.
        var callGroup: @MainActor () -> String?
        /// Sleep; returns early (without throwing) when the task is cancelled.
        var sleepMs: (_ ms: Int64) async -> Void
        /// `Double.random(in: 0..<1)`-shaped, for the jitter.
        var random: () -> Double
        var nowMs: () -> Int64
        /// One sweep finished (not cancelled): how many polls it made and how
        /// many answered. The Feed's "Couldn't check for new messages." reads
        /// this (``FeedRefreshCenter``). The call loop's long poll does not
        /// report.
        var onSweepDone: @MainActor (_ tally: FeedCheckTally) -> Void = { _ in }
        /// Whether a push wake can reach this device (an APNs token is held).
        /// `nil` keeps the short interval always (the tests' default).
        var pushReachable: (@MainActor () -> Bool)? = nil
    }

    struct Config: Equatable {
        var intervalMs: Int64 = 4_000
        var jitterMs: Int64 = 1_000
        /// Idle, with an APNs token held.
        var idlePushIntervalMs: Int64 = 120_000
        /// Idle, with no APNs token.
        var idleNoPushIntervalMs: Int64 = 30_000
        /// How long a foreground push wake keeps the short interval.
        var wakeWindowMs: Int64 = 30_000
        var concurrency: Int = 2
        var maxImmediateRepolls: Int = 4
        /// The dedicated call-group long poll's window.
        var callLongPollSecs: Int = 25
        /// How often the call loop re-checks for a call when there is none.
        var callIdleCheckMs: Int64 = 1_000
        /// Failure backoff: `base * 2^(failures-1)`, capped.
        var backoffBaseMs: Int64 = 2_000
        var backoffMaxMs: Int64 = 60_000
    }

    private let ports: Ports
    let config: Config

    private(set) var isForeground = false
    private var sweepLoop: Task<Void, Never>?
    private var callLoop: Task<Void, Never>?
    private var foregroundWaiters: [CheckedContinuation<Void, Never>] = []
    private let backoff = GroupBackoff()
    private var nudged = false
    private var wakeWindowUntilMs: Int64 = 0
    /// The groups the last sweep started with; `nil` before the first one.
    private var lastSweepGroups: Set<String>?

    init(ports: Ports, config: Config = Config()) {
        self.ports = ports
        self.config = config
    }

    // MARK: - Lifecycle

    /// Start the loops (idempotent). They run until ``stop()``, parking while
    /// the app is not in the foreground.
    /// The running scheduler, for the push delegate's ``nudge()``.
    static weak var active: ForegroundReceiveScheduler?

    func start() {
        Self.active = self
        if sweepLoop == nil {
            sweepLoop = Task { @MainActor [weak self] in await self?.runSweepLoop() }
        }
        if callLoop == nil {
            callLoop = Task { @MainActor [weak self] in await self?.runCallLoop() }
        }
    }

    /// Stop for good (logout). Cancels the loops; a poll in flight finishes or
    /// throws on its own.
    func stop() {
        if Self.active === self { Self.active = nil }
        sweepLoop?.cancel()
        callLoop?.cancel()
        sweepLoop = nil
        callLoop = nil
        backoff.clear()
        resumeWaiters()
    }

    /// The scene became active (`true`) or left the foreground (`false`).
    func setForeground(_ foreground: Bool) {
        isForeground = foreground
        if foreground {
            nudged = true
            resumeWaiters()
        }
    }

    /// A push wake reached the app in the foreground: sweep now, and keep the
    /// short interval for ``Config/wakeWindowMs``.
    func nudge() {
        wakeWindowUntilMs = ports.nowMs() &+ config.wakeWindowMs
        nudged = true
    }

    /// Whether a group appeared since the last sweep began: a chat this
    /// device just joined (a Welcome, an approved request, a recreated
    /// group). Its first messages and its name arrive in the next minute, and
    /// may not bring a push wake (the device subscribes to its pushes only
    /// once it holds the group's routing secret), so it is swept at once and
    /// the short interval kept for ``Config/wakeWindowMs``. Walk 7
    /// (2026-11-20): a group joined on an iPhone read "No messages yet" under
    /// the creator's name for over 15 s though its first message had arrived.
    func noteGroupsIfNew() -> Bool {
        guard let last = lastSweepGroups else { return false }
        let now = Set(ports.groups())
        guard !now.isSubset(of: last) else { return false }
        nudge()
        return true
    }

    /// The pause before the next sweep, for this moment.
    func currentIntervalMs() -> Int64 {
        guard let pushReachable = ports.pushReachable else { return config.intervalMs }
        if ports.callGroup() != nil || ports.nowMs() < wakeWindowUntilMs { return config.intervalMs }
        return pushReachable() ? config.idlePushIntervalMs : config.idleNoPushIntervalMs
    }

    // MARK: - Loops

    private func runSweepLoop() async {
        while !Task.isCancelled {
            guard isForeground else {
                await waitUntilForeground()
                continue
            }
            nudged = false
            await runSweep()
            if Task.isCancelled { return }
            await pause()
        }
    }

    /// Sleep for the current interval, in slices of at most a second, so a
    /// ``nudge()`` or the return to the foreground ends it early. The slices
    /// count the time they asked for, not a clock.
    private func pause() async {
        let interval = currentIntervalMs()
        let jitter = interval == config.intervalMs ? config.jitterMs : interval / 4
        var remaining = Self.pauseMs(
            config: Config(intervalMs: interval, jitterMs: jitter), random: ports.random())
        if ports.pushReachable == nil {
            await ports.sleepMs(remaining)
            return
        }
        while remaining > 0, !Task.isCancelled, !nudged {
            let slice = min(remaining, 1_000)
            await ports.sleepMs(slice)
            remaining -= slice
            _ = noteGroupsIfNew()
        }
    }

    private func runCallLoop() async {
        while !Task.isCancelled {
            guard isForeground else {
                await waitUntilForeground()
                continue
            }
            guard let group = ports.callGroup(), !ports.isExcluded(group) else {
                await ports.sleepMs(config.callIdleCheckMs)
                continue
            }
            do {
                _ = try await ports.poll(group, config.callLongPollSecs)
            } catch {
                if Task.isCancelled { return }
                await ports.sleepMs(config.backoffBaseMs)
            }
        }
    }

    /// One pass over every eligible group (plus immediate re-polls), with at
    /// most ``Config/concurrency`` polls in flight. Returns when the pass is
    /// done or the task is cancelled; never throws.
    func runSweep() async {
        let groups = ports.groups()
        lastSweepGroups = Set(groups)
        let queue = SweepQueue(groups: groups)
        await withTaskGroup(of: Void.self) { group in
            for _ in 0..<max(1, config.concurrency) {
                group.addTask { @MainActor [weak self] in
                    await self?.worker(queue: queue)
                }
            }
        }
        if !Task.isCancelled { ports.onSweepDone(queue.tally) }
    }

    private func worker(queue: SweepQueue) async {
        while !Task.isCancelled {
            // A backgrounded app stops taking new work; what is in flight ends
            // on its own.
            guard isForeground, let id = queue.next() else { return }
            // The call loop owns the live call's group.
            if ports.isExcluded(id) || id == ports.callGroup() { continue }
            if backoff.isBackedOff(id, nowMs: ports.nowMs()) { continue }
            queue.inFlight.insert(id)
            defer { queue.inFlight.remove(id) }
            queue.tally.tried += 1
            do {
                let yielded = try await ports.poll(id, 0)
                queue.tally.answered += 1
                backoff.succeeded(id)
                let used = queue.repolls[id] ?? 0
                if yielded, used < config.maxImmediateRepolls, !Task.isCancelled {
                    queue.repolls[id] = used + 1
                    queue.pending.insert(id, at: 0)
                }
            } catch {
                if Task.isCancelled { return }
                backoff.failed(id, nowMs: ports.nowMs(), config: config)
            }
        }
    }

    private func waitUntilForeground() async {
        await withCheckedContinuation { continuation in
            if isForeground || Task.isCancelled {
                continuation.resume()
            } else {
                foregroundWaiters.append(continuation)
            }
        }
    }

    private func resumeWaiters() {
        let waiters = foregroundWaiters
        foregroundWaiters.removeAll()
        waiters.forEach { $0.resume() }
    }

    /// The pause after a sweep: `interval +/- jitter`, never negative.
    nonisolated static func pauseMs(config: Config, random: Double) -> Int64 {
        let offset = (random * 2 - 1) * Double(config.jitterMs)
        return max(0, Int64((Double(config.intervalMs) + offset).rounded()))
    }
}

/// One sweep's work list. Main-actor confined, shared by the workers.
@MainActor
private final class SweepQueue {
    var pending: [String]
    var inFlight: Set<String> = []
    var repolls: [String: Int] = [:]
    /// Polls made and polls answered, for ``ForegroundReceiveScheduler/Ports/onSweepDone``.
    var tally = FeedCheckTally()

    init(groups: [String]) {
        pending = groups
    }

    /// The next group not already being polled. The same group is never polled
    /// twice at once: the second poll would only find its cursor moved.
    func next() -> String? {
        guard let index = pending.firstIndex(where: { !inFlight.contains($0) }) else { return nil }
        return pending.remove(at: index)
    }
}

/// Per-group failure backoff. A success clears it.
@MainActor
final class GroupBackoff {
    private var state: [String: (failures: Int, untilMs: Int64)] = [:]

    func failed(_ id: String, nowMs: Int64, config: ForegroundReceiveScheduler.Config) {
        let failures = (state[id]?.failures ?? 0) + 1
        let shift = min(failures - 1, 16)
        let delay = min(config.backoffMaxMs, config.backoffBaseMs << Int64(shift))
        state[id] = (failures, nowMs + delay)
    }

    func succeeded(_ id: String) {
        state.removeValue(forKey: id)
    }

    func isBackedOff(_ id: String, nowMs: Int64) -> Bool {
        guard let entry = state[id] else { return false }
        return nowMs < entry.untilMs
    }

    func clear() {
        state.removeAll()
    }
}

// MARK: - Production ports

extension ForegroundReceiveScheduler {
    static func live(
        repository: ChatRepository,
        callHost: CallHost,
        groupCallHost: GroupCallHost,
        feedRefresh: FeedRefreshCenter
    ) -> ForegroundReceiveScheduler {
        ForegroundReceiveScheduler(ports: Ports(
            groups: { [weak repository] in repository?.receivableGroupIds() ?? [] },
            isExcluded: { [weak repository] groupIdHex in
                repository?.isChatOpen(groupIdHex: groupIdHex) ?? true
            },
            poll: { [weak repository] groupIdHex, waitSecs in
                guard let repository else { return false }
                let before = repository.journalCursor(groupIdHex: groupIdHex)
                try await repository.pollGroup(groupIdHex: groupIdHex, waitSecs: waitSecs)
                return repository.journalCursor(groupIdHex: groupIdHex) != before
            },
            callGroup: { [weak callHost, weak groupCallHost] in
                if let callHost, callHost.hasLiveCall, let group = callHost.activeGroupIdHex {
                    return group
                }
                // A group call's group, so an epoch advance reaches the call's
                // key promptly (G-18 / ADR-0029) while its chat is closed.
                return groupCallHost?.liveGroupIdHex
            },
            sleepMs: { ms in
                try? await Task.sleep(nanoseconds: UInt64(max(0, ms)) * 1_000_000)
            },
            random: { Double.random(in: 0..<1) },
            nowMs: { ChatRepository.nowMs() },
            onSweepDone: { [weak feedRefresh] tally in
                feedRefresh?.record(.groups, tally)
            },
            pushReachable: { PushDeviceTokens.shared.alert != nil }
        ))
    }
}
