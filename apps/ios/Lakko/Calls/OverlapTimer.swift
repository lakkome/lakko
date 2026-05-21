// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// 1-second SFrame overlap-window timer (Phase 3c5 — iOS
/// mirror of `apps/web/src/calls/overlap-timer.ts` and
/// `apps/android/.../calls/OverlapTimer.kt`).
///
/// The Rust core
/// (`crates/lakko-calls/src/group_call.rs::GroupCallSession`)
/// is wall-clock-free by design: on every `refresh_call_key`
/// it stashes the prior CallKey into `previous_call_key`,
/// and the caller must invoke `drop_previous_call_key`
/// after a ~1-second overlap window. This class is the
/// **platform-side timer** that owns that schedule on iOS.
///
/// Usage:
///
/// ```swift
/// let timer = OverlapTimer()
/// // Right after refresh_call_key():
/// timer.arm { callSession.dropPreviousCallKey() }
/// // ...the closure fires ~1 s later.
///
/// // To cancel before firing (e.g. session torn down):
/// timer.cancel()
/// ```
///
/// Properties (mirror of the web helper):
///
///   * **Single-shot.** Arming twice without cancellation
///     replaces the prior callback (the newer rotation
///     subsumes the older); the first callback never
///     fires.
///   * **Idempotent cancel.** `cancel()` on a disarmed
///     timer is a no-op.
///   * **Safe across destroy.** The underlying `Task`
///     captures `self` weakly; if the `OverlapTimer` is
///     released before the schedule fires, the callback is
///     silently dropped. Caller should still call
///     `cancel()` explicitly during teardown so the task
///     doesn't outlive the call session.
///
/// Closes part of G-18 (known-gaps): "Overlap-window timer
/// lives in the platform layer". The Rust side stays
/// wall-clock-free by design; this helper is the canonical
/// iOS-side schedule. Wiring it into a live call flow
/// lands with the SFU integration in 3b.
///
/// See:
///   * `docs/security/calls.md` C-9 — overlap window
///     invariant.
///   * `docs/security/known-gaps.md` G-18 — what this
///     helper snaps.
public final class OverlapTimer: @unchecked Sendable {

    /// Default overlap window — 1 second per ADR-0008.
    public static let defaultOverlap: TimeInterval = 1.0

    /// Don't accept anything below 50 ms (would skip too
    /// many in-flight frames) or above 10 s (would extend
    /// the seed-boundary forward-secrecy window
    /// unreasonably). Symmetric with the web helper's
    /// `MIN_OVERLAP_TTL_MS` / `MAX_OVERLAP_TTL_MS`.
    public static let minOverlap: TimeInterval = 0.05
    public static let maxOverlap: TimeInterval = 10.0

    /// Lifecycle state — useful for diagnostics + tests.
    public enum State: Equatable {
        case disarmed
        case armed
        case fired
    }

    public let ttl: TimeInterval

    private let stateLock = NSLock()
    private var task: Task<Void, Never>?
    private var currentState: State = .disarmed

    public init(ttl: TimeInterval = OverlapTimer.defaultOverlap) {
        if !ttl.isFinite {
            self.ttl = OverlapTimer.defaultOverlap
        } else {
            self.ttl = min(max(ttl, OverlapTimer.minOverlap), OverlapTimer.maxOverlap)
        }
    }

    deinit {
        task?.cancel()
    }

    /// Current state. Reads behind the same lock that
    /// `arm` / `cancel` write under, so the answer is
    /// consistent across concurrent observers.
    public var state: State {
        stateLock.lock()
        defer { stateLock.unlock() }
        return currentState
    }

    /// `true` iff a callback is scheduled and hasn't fired
    /// or been cancelled.
    public var isArmed: Bool {
        stateLock.lock()
        defer { stateLock.unlock() }
        return currentState == .armed
    }

    /// Schedule `callback` to fire after `ttl` seconds.
    /// Replaces any prior schedule — the first callback
    /// will never fire. Caller invokes this immediately
    /// after `refresh_call_key` on the Rust core.
    ///
    /// The callback runs on a detached `Task`; if it needs
    /// `@MainActor` semantics the caller wraps it in `Task
    /// { @MainActor in ... }` inside the closure.
    public func arm(_ callback: @escaping @Sendable () -> Void) {
        stateLock.lock()
        // Cancel any prior task before installing the new one.
        task?.cancel()
        currentState = .armed
        let nanos = UInt64((ttl * 1_000_000_000).rounded())
        stateLock.unlock()

        let newTask = Task.detached { [weak self] in
            try? await Task.sleep(nanoseconds: nanos)
            if Task.isCancelled { return }
            // Run the callback FIRST, then update state, so
            // a re-entrant arm() inside the callback
            // observes the prior shot as completed.
            callback()
            guard let self = self else { return }
            self.stateLock.lock()
            // Only flip to .fired if no newer arm()
            // re-installed a task in the meantime.
            if self.currentState == .armed {
                self.currentState = .fired
            }
            self.task = nil
            self.stateLock.unlock()
        }

        stateLock.lock()
        task = newTask
        stateLock.unlock()
    }

    /// Cancel the pending callback, if any. No-op when
    /// disarmed.
    public func cancel() {
        stateLock.lock()
        task?.cancel()
        task = nil
        currentState = .disarmed
        stateLock.unlock()
    }
}
