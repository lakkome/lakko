// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

import kotlinx.coroutines.CancellationException

/**
 * "This device was removed" (docs/ux/second-device.md section 3.1).
 *
 * Mirror of the web `apps/web/src/devices/sync/removed-device.ts`.
 *
 * Another device of the account removed this one: once this device applies
 * that commit its DeviceGroup is inactive (`DeviceGroup.isActive()` is
 * `false`), frozen for good, and nothing new decrypts. The sync runtime then
 * stops polling, persists nothing more, and the app root shows one
 * full-screen state instead of the app and wipes the account's data by
 * itself (ADR-0074 section 2.3; the screen and its copy are
 * `KeysResetSignedOutScreen` and `RemovalCopy`).
 *
 * The decisions live here, apart from [DeviceGroupSyncRepository], so a host
 * JVM test runs them without a Delivery Service or the native library.
 */
internal object RemovedDevice {

    /**
     * Whether the DeviceGroup handle reports this device still in the
     * account's DeviceGroup; `null` when the read itself fails (a poisoned
     * lock, a destroyed handle). A failed read is not proof of removal.
     */
    fun readActive(read: () -> Boolean): Boolean? =
        try {
            read()
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            null
        }

    /** Only an explicit `false` is a removal: the screen must never show on
     *  a read that failed. */
    fun isRemoved(active: Boolean?): Boolean = active == false

    /** What the poll loop does after applying a batch (or after opening a
     *  handle): stop for good once the device is removed, otherwise go on. */
    fun pollStepAfterBatch(active: Boolean?): PollStep =
        if (isRemoved(active)) PollStep.StopRemoved else PollStep.Continue

    /**
     * Whether a device that is behind may drop its DeviceGroup state and
     * rejoin through a sibling (ADR-0081 decision 4): only on a group known to
     * be active — never on a removed device (it is not behind, it is out), and
     * not on a handle whose state could not be read.
     */
    fun rejoinAllowed(active: Boolean?): Boolean = active == true
}

/** The outcome of [RemovedDevice.pollStepAfterBatch]. */
internal enum class PollStep {
    /** Keep polling. */
    Continue,

    /** This device was removed: stop the poll loop for good this session. */
    StopRemoved,
}
