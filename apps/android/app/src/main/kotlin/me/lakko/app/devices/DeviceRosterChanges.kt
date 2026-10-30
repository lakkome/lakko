// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/**
 * "The list follows the account" (`docs/ux/second-device.md` section 3): the
 * account's device list changed on this device, and an open Devices screen
 * reads it again. The screen reads the list off the persisted DeviceGroup
 * snapshot, so this moves only once that snapshot holds the change.
 */
public object DeviceRosterChanges {
    private val mutableRevision = MutableStateFlow(0L)

    /** Moves on every change; the value means nothing else. */
    public val revision: StateFlow<Long> = mutableRevision.asStateFlow()

    public fun note() {
        mutableRevision.update { it + 1 }
    }
}

/**
 * The commit a poll applied reaches the disk only with the batch's persist,
 * and a failed persist is written again with the next batch: the change is
 * announced after the write that holds it, not after the commit.
 */
internal class RosterChangeAfterPersist(private val announce: () -> Unit) {
    private var owed = false

    /** A commit was applied in memory. */
    fun onCommitApplied() {
        owed = true
    }

    /** The handle's snapshot reached the disk. */
    fun onPersisted() {
        if (owed) {
            owed = false
            announce()
        }
    }
}
