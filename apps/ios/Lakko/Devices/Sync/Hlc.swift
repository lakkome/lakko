// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Per-process monotonic Hybrid Logical Clock generator for
/// cross-device payloads (Phase 2a3e2b-iOS).
///
/// Mirror of `apps/web/src/devices/sync/hlc.ts`. Combines
/// wall-clock milliseconds with a logical counter to defeat
/// ties inside a single millisecond and to survive backward
/// clock jumps:
///
/// ```text
///   hlc = wall_millis * 1000 + counter
/// ```
///
/// Strictly monotonic across every call inside the process —
/// if the wall clock hasn't advanced since the last mint, the
/// counter increments instead.
public enum Hlc {

    // Synchronisation: actor-isolated state would force every
    // mint to await on the executor. The HLC is on the hot path
    // of the composer keystroke flow, so we trade actor
    // isolation for a plain `os_unfair_lock` (NSLock here for
    // portability outside iOS).
    private static let lock = NSLock()
    nonisolated(unsafe) private static var lastWallMs: UInt64 = 0
    nonisolated(unsafe) private static var counter: UInt64 = 0

    /// Mint a strictly-greater HLC value than every prior call
    /// in this process.
    public static func mint() -> UInt64 {
        lock.lock()
        defer { lock.unlock() }
        let wallMs = UInt64(Date().timeIntervalSince1970 * 1000)
        if wallMs > lastWallMs {
            lastWallMs = wallMs
            counter = 0
        } else {
            counter += 1
        }
        return lastWallMs &* 1000 &+ counter
    }

    /// Move the clock past `hlc`, so the next ``mint()`` is strictly greater.
    ///
    /// The receive half of a hybrid logical clock. Needed since ADR-0058: a
    /// state restored from disk or a backup, or folded from another device
    /// whose wall clock runs ahead, carries stamps this process never minted,
    /// and a local write stamped below one of them would lose to it.
    public static func observe(_ hlc: UInt64) {
        lock.lock()
        defer { lock.unlock() }
        let wallMs = hlc / 1000
        let count = hlc % 1000
        if wallMs > lastWallMs {
            lastWallMs = wallMs
            counter = count
        } else if wallMs == lastWallMs, count > counter {
            counter = count
        }
    }
}
