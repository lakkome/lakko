// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/**
 * What the signed-in device does once the scan route (Add a device > Scan a
 * code > Link device) has added the new device (UX `docs/ux/second-device.md`
 * section 2.1, decided 2026-10-29): there is no result line, the screen goes
 * straight back to the Devices list. When this device had no keys to hand
 * over, the existing `device_link.keys_not_sent` text is shown under the
 * Devices list after returning to it, until Add a device is pressed again.
 *
 * Process memory only: the warning is about the scan that just ran and is
 * not kept across a restart. Keyed on the account, so another account
 * signed in on this device never sees it.
 */
public class ScanLinkedNotice {
    private val keysNotSent = MutableStateFlow<String?>(null)

    /** The account (`accountIdB64`) whose last scan added a device without
     *  its keys, or `null`. */
    public val keysNotSentFor: StateFlow<String?> = keysNotSent

    /** A scan added a device for [accountIdB64]; [keysSent] is whether this
     *  device handed over the account's keys. The caller then returns to the
     *  Devices list. */
    public fun linked(accountIdB64: String, keysSent: Boolean) {
        keysNotSent.value = if (keysSent) null else accountIdB64
    }

    /** Whether the Devices list of [accountIdB64] shows the warning. */
    public fun showsKeysNotSent(accountIdB64: String): Boolean = keysNotSent.value == accountIdB64

    /** Add a device was pressed again: the warning goes. */
    public fun clear() {
        keysNotSent.value = null
    }

    public companion object {
        /** The one instance the Devices and Add a device screens share. */
        public val shared: ScanLinkedNotice = ScanLinkedNotice()
    }
}
