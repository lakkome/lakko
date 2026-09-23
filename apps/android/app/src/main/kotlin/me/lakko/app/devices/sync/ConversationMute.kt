// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

import android.content.Context
import android.util.Log
import me.lakko.app.devices.DeviceGroupSnapshotStore

/**
 * Is a conversation muted? Read from the one place mute is already kept.
 *
 * ## Why this exists at all
 *
 * Mute was stored and drawn everywhere — [FlagsState.muted] rides the
 * DeviceGroup journal between a person's devices, it survives a relaunch in
 * the sync-state snapshot, the chat list draws it, and the ADR-0050 pairing
 * bundle carries it to a new device — and not one ring path read it. A muted
 * conversation rang with the device ringtone, vibrated, and took over the
 * screen exactly like an unmuted one (`docs/ux/calls.md` section 2.1: "a muted
 * chat never rings").
 *
 * ## Why it reads the snapshot rather than the repository
 *
 * [DeviceGroupSyncRepository] holds the same flags in memory, but the two ring
 * paths cannot both reach it: the FCM wake runs with no activity and therefore
 * with no bootstrapped repository, and `AppCallHost.attach` happens before
 * `MainActivity` has even constructed one. What both paths CAN reach is the
 * repository's own persisted snapshot, which is the same state — the
 * repository seeds itself from this file on launch and rewrites it whenever
 * the flags move. So this is a second READER, never a second store; nothing
 * here writes.
 *
 * The cost is the repository's 500 ms persist debounce: a mute toggled in the
 * half-second before an offer lands is not seen. That is the right side to be
 * imprecise on — the miss rings a chat the person has just muted, rather than
 * silencing one they have not.
 *
 * ## Which way it fails
 *
 * Toward ringing. No snapshot, an unreadable one, no row for the group: not
 * muted. A person who never muted anything must never lose a call to a store
 * that failed to open, and the failure mode of the opposite choice — a phone
 * that silently stops ringing — is invisible and would be blamed on the call,
 * not on the store.
 */
public object ConversationMute {

    private const val TAG = "lakko-mute"

    /**
     * Whether [groupIdHex] is muted for [accountIdB64].
     *
     * Touches the disk (one small file plus an AES-GCM open), so call it off
     * the main thread. Both ring paths already have a coroutine to do that in.
     */
    public fun isMuted(context: Context, accountIdB64: String?, groupIdHex: String?): Boolean {
        val account = accountIdB64?.takeIf { it.isNotEmpty() } ?: return false
        val key = groupIdHex?.lowercase()?.takeIf { it.isNotEmpty() } ?: return false
        return try {
            val blob = DeviceGroupSnapshotStore.getSnapshot(
                context = context.applicationContext,
                accountIdB64 = account,
                dir = DeviceGroupSnapshotStore.SYNC_STATE_DIR,
            ) ?: return false
            DeviceGroupSyncStateCodec.decode(blob)?.flags?.get(key)?.muted == true
        } catch (cause: Throwable) {
            // Fail toward ringing; see the class documentation.
            Log.w(TAG, "could not read the conversation's mute flag", cause)
            false
        }
    }
}
