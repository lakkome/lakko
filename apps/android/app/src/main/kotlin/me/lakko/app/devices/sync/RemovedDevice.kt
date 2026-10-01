// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

import kotlinx.coroutines.CancellationException
import me.lakko.app.l10n.UiText

/**
 * "This device was removed" (docs/ux/second-device.md section 3.1).
 *
 * Mirror of the web `apps/web/src/devices/sync/removed-device.ts`.
 *
 * Another device of the account removed this one: once this device applies
 * that commit its DeviceGroup is inactive (`DeviceGroup.isActive()` is
 * `false`), frozen for good, and nothing new decrypts. The sync runtime then
 * stops polling, persists nothing more, and the signed-in root shows one
 * full-screen state instead of the app.
 *
 * The decisions live here, apart from [DeviceGroupSyncRepository], so a host
 * JVM test runs them without a Delivery Service or the native library.
 */
internal object RemovedDevice {

    /** The copy of the removed-device screen, as catalog messages (ADR-0061). */
    val TITLE: UiText = UiText("devices.removed.title")

    val BODY: UiText = UiText("devices.removed.body")

    val SIGN_OUT: UiText = UiText("devices.removed.sign_out")

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
     * Whether the ADR-0057 fork repair may run on this handle. The repair
     * re-founds the account's DeviceGroup, so it runs only on a group known to
     * be active: never on a removed device (it is not forked, it is out), and
     * not on a handle whose state could not be read.
     */
    fun forkRepairAllowed(active: Boolean?): Boolean = active == true
}

/** The outcome of [RemovedDevice.pollStepAfterBatch]. */
internal enum class PollStep {
    /** Keep polling. */
    Continue,

    /** This device was removed: stop the poll loop for good this session. */
    StopRemoved,
}
