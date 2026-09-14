// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The per-group serializing chain every MLS-mutating region of
/// ``ChatRepository`` runs inside, plus the one signal that says a group's MLS
/// state has gone quiet.
///
/// The chain itself is exactly the construction ``ChatRepository`` has used
/// since ADR-0040 (G-35) / G-38 — see `ChatRepository.withCommitLock` for WHY
/// it exists, what runs inside it and the deadlock audit. It moved into its own
/// type for one reason: the app-level call host must observe it, and an
/// observable queue is easier to test on its own than through a bootstrapped
/// repository.
///
/// ## Idle observation
///
/// Every region that can move a group's epoch — the poll's apply half (whether
/// the open chat's long poll drove it, the foreground receive for chats that are
/// not open, or the dedicated call-group long poll), the membership create
/// paths, the ADR-0045 sweep and its adjudication, and the ADR-0040 F1 reset
/// that replaces a group with its durable snapshot — runs inside this chain. A
/// DRAINED chain is therefore the one moment that can say "the epoch may have
/// changed, and nothing is half-way through changing it", whatever screen is
/// showing. The group call host rotates a live call's SFrame key there
/// (G-18 / ADR-0029); before, only the open chat's view model noticed an
/// advance, so leaving the chat froze the call key.
///
/// ## Only on drain, never between two queued regions
///
/// Between two queued regions the next one may already have eager-merged a
/// commit it has not published yet (ADR-0040). Reporting there would let an
/// observer read an epoch that is still in flight and may be rolled back by the
/// F1 reset. On drain the state is whatever the last region left behind; the
/// regions before it need no report of their own because the drain covers them.
///
/// ## Contract for observers
///
///   * Called synchronously on the main actor, AFTER the chain entry has been
///     removed, so an observer that queues a new region starts a fresh chain
///     rather than nesting in the one that drained.
///   * An observer must not take this lock on every call, or its own drain
///     reports again forever. Reading group state (an epoch, an exporter
///     secret) needs no lock: those UniFFI calls are synchronous and run between
///     regions on the main actor.
///
/// Same contract as `apps/web/src/mls/group-lock.ts` `onGroupMlsIdle`.
@MainActor
final class GroupMlsLock {

    /// Per-group chain tail. SERIALIZING, never coalescing: a successor waits
    /// and then runs in full against freshly-read state.
    private var chain: [String: Task<Void, Never>] = [:]

    private var observers: [UUID: @MainActor (String) -> Void] = [:]

    init() {}

    /// Observe every drain. Keep the returned token to stop observing.
    @discardableResult
    func observeIdle(_ observer: @escaping @MainActor (_ groupIdHex: String) -> Void) -> UUID {
        let token = UUID()
        observers[token] = observer
        return token
    }

    func removeIdleObserver(_ token: UUID) {
        observers.removeValue(forKey: token)
    }

    /// `true` while any region for `groupIdHex` is running or queued.
    func isBusy(groupIdHex: String) -> Bool {
        chain[groupIdHex] != nil
    }

    /// Run `body` as the sole in-flight MLS-mutating section for one group.
    func run<T>(
        groupIdHex: String,
        _ body: @escaping @MainActor () async throws -> T
    ) async throws -> T {
        // Read the tail and replace it with NO `await` in between: `@MainActor`
        // cannot interleave another caller across a straight-line region, so two
        // callers can never observe the SAME predecessor and run beside each
        // other.
        let predecessor = chain[groupIdHex]
        let run = Task { @MainActor () async throws -> T in
            // Wait for everything already queued for this group. A predecessor
            // that threw must not block its successor, hence the discarded
            // result on a `Never`-failing erased tail.
            _ = await predecessor?.value
            return try await body()
        }
        // The erased tail completes exactly when `run` does, so a successor
        // chaining on it observes this section's writes.
        let tail = Task { @MainActor in _ = try? await run.value }
        chain[groupIdHex] = tail
        // Tail-guarded release — a successor that queued behind this section is
        // the tail now, and dropping it would let the caller after it start
        // beside it. Runs on both the success and the throw path. Only the
        // release that empties the chain reports the drain.
        defer {
            if chain[groupIdHex] == tail {
                chain.removeValue(forKey: groupIdHex)
                notifyIdle(groupIdHex)
            }
        }
        return try await run.value
    }

    private func notifyIdle(_ groupIdHex: String) {
        // A snapshot, so an observer that removes itself (or another) while
        // being notified does not disturb this pass.
        for observer in Array(observers.values) {
            observer(groupIdHex)
        }
    }
}
