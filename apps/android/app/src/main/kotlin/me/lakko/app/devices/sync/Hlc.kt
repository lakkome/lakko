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
}
