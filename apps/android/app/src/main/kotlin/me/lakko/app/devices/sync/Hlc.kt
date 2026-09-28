// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

/**
 * Per-process monotonic Hybrid Logical Clock generator for
 * cross-device payloads (Phase 2a3e2b-Android).
 *
 * Mirror of:
 *
 *   * `apps/web/src/devices/sync/hlc.ts`
 *   * `apps/ios/Lakko/Devices/Sync/Hlc.swift`
 *
 * Combines wall-clock milliseconds with a logical counter to
 * defeat ties inside a single millisecond and to survive
 * backward clock jumps:
 *
 * ```
 *   hlc = wall_millis * 1000 + counter
 * ```
 *
 * Strictly monotonic across every call inside the process —
 * if the wall clock hasn't advanced since the last mint, the
 * counter increments instead.
 */
public object Hlc {

    private val lock = Any()

    @Volatile
    private var lastWallMs: ULong = 0u

    @Volatile
    private var counter: ULong = 0u

    /** Mint a strictly-greater HLC value than every prior call
     *  in this process. */
    public fun mint(): ULong = synchronized(lock) {
        val wallMs = System.currentTimeMillis().toULong()
        if (wallMs > lastWallMs) {
            lastWallMs = wallMs
            counter = 0u
        } else {
            counter += 1u
        }
        lastWallMs * 1000u + counter
    }

    /**
     * Make every later [mint] strictly greater than [value].
     *
     * Called after the synced state is seeded from disk or a backup
     * (ADR-0058): a stamp this device wrote in an earlier run, or one a
     * sibling wrote with a clock ahead of ours, must never out-rank the next
     * local edit, or that edit would lose to the stale write it replaces.
     */
    public fun advancePast(value: ULong): Unit = synchronized(lock) {
        val current = lastWallMs * 1000u + counter
        if (value < current) return@synchronized
        lastWallMs = value / 1000u
        counter = value % 1000u
    }
}
