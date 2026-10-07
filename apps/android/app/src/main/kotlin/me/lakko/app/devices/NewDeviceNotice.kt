// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Build
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import me.lakko.app.auth.MarkerStorage
import me.lakko.app.auth.SharedPrefsMarkerStorage

/*
 * "A device was added to your account" (`docs/ux/identity-recovery.md`
 * section 17.5): shown on every OTHER device of the account after a device
 * joins it.
 *
 * The trigger is this account's DeviceGroup roster gaining a device this
 * device has not acknowledged. The DeviceGroup is the account's own device
 * list (it is what Settings > Devices draws, row for row), every device of
 * the account applies the Add commit that admits a new one, and no server
 * says anything: the fact is read off the MLS roster this device already
 * verified.
 *
 * Per device, persisted: the set of device keys already acknowledged (seeded
 * with the devices present when this device joined), the unacknowledged ones
 * with the time this device learned of each (the DeviceGroup records no join
 * time, so that is the best the notice can say), and whether the "removed"
 * line is up. Never on the device that was added, never on the device that
 * let it in.
 */

/** The notice state of one account on this device. Device keys are the hex
 *  of the device's DeviceGroup signature key. */
@Serializable
public data class NewDeviceNoticeState(
    /** Whether the acknowledged set was ever seeded. */
    val seeded: Boolean = false,
    /** Devices this device knows about and has nothing to say on. */
    val acknowledged: Set<String> = emptySet(),
    /** Unacknowledged new devices and when this device learned of each
     *  (unix ms). Non-empty: the banner is up. */
    val pending: Map<String, Long> = emptyMap(),
    /** A device that carried `New` was removed here: the line that replaces
     *  the banner is up. */
    val removedNew: Boolean = false,
)

/** What the banner says. */
public data class NewDeviceBanner(
    /** How many devices it is about (`title` / `title_many`). */
    val count: Int,
    /** The most recent join time this device knows (unix ms). */
    val latestAtMs: Long,
)

/** The banner for [state], or `null` when there is nothing to say. */
public fun newDeviceBanner(state: NewDeviceNoticeState): NewDeviceBanner? {
    val latest = state.pending.values.maxOrNull() ?: return null
    return NewDeviceBanner(count = state.pending.size, latestAtMs = latest)
}

/** Whether the row of [deviceKey] carries the `New` label. */
public fun isNewDevice(state: NewDeviceNoticeState, deviceKey: String): Boolean = deviceKey in state.pending

/**
 * The roster of the account's devices as this device now sees it.
 *
 * - Never seeded, or this device has only ever known itself (it founded a
 *   group of one and has now joined the account's real one): every device
 *   present is known. A device never shows the notice about itself or about
 *   the devices that were there when it joined.
 * - Otherwise a key that is neither acknowledged nor already pending (and is
 *   not this device) is NEW, learned at [nowMs]. A pending key that left the
 *   roster is dropped: there is nothing left to decide about it.
 *
 * Returns the next state and the keys that became new in this step (what a
 * local notification is posted for).
 */
public fun observeRoster(
    state: NewDeviceNoticeState,
    roster: Set<String>,
    selfKey: String?,
    nowMs: Long,
): Pair<NewDeviceNoticeState, Set<String>> {
    val knowsOnlyItself = (state.acknowledged - setOfNotNull(selfKey)).isEmpty() && state.pending.isEmpty()
    if (!state.seeded || knowsOnlyItself) {
        return state.copy(seeded = true, acknowledged = state.acknowledged + roster, pending = emptyMap()) to emptySet()
    }
    val fresh = roster - state.acknowledged - state.pending.keys - setOfNotNull(selfKey)
    val pending = state.pending.filterKeys { it in roster } + fresh.associateWith { nowMs }
    return state.copy(pending = pending) to fresh
}

/** This device let a device in (the code route or the scan): everything in
 *  [roster] is known. A second alert about the same act a moment later
 *  teaches people to dismiss the banner without reading (UX 17.5). */
public fun acknowledgeRoster(state: NewDeviceNoticeState, roster: Set<String>): NewDeviceNoticeState =
    state.copy(seeded = true, acknowledged = state.acknowledged + roster, pending = state.pending - roster)

/** `Dismiss`: the person decided. The `New` labels go with the banner. */
public fun dismissNotice(state: NewDeviceNoticeState): NewDeviceNoticeState =
    state.copy(acknowledged = state.acknowledged + state.pending.keys, pending = emptyMap())

/**
 * This device removed [deviceKey]. When the row carried `New` ([wasNew],
 * read BEFORE the removal: the roster that follows it drops the key from the
 * pending set by itself), the removed line replaces the banner. The key is
 * not acknowledged: it is gone.
 */
public fun removedHere(state: NewDeviceNoticeState, deviceKey: String, wasNew: Boolean): NewDeviceNoticeState =
    if (wasNew) {
        state.copy(pending = state.pending - deviceKey, removedNew = true)
    } else {
        state
    }

/** The removed line was dismissed (or `Change password` was opened). */
public fun dismissRemovedLine(state: NewDeviceNoticeState): NewDeviceNoticeState = state.copy(removedNew = false)

/** The persisted notice state, one row per account. Plain storage: device
 *  keys are public MLS leaf keys, the rest is flags and times. */
public class NewDeviceNoticeStore(private val storage: MarkerStorage) {
    public fun load(accountIdB64: String): NewDeviceNoticeState {
        val raw = storage.get(accountIdB64) ?: return NewDeviceNoticeState()
        return try {
            JSON.decodeFromString(NewDeviceNoticeState.serializer(), raw)
        } catch (_: Exception) {
            // An unreadable row reads as never seeded: the next roster seeds
            // it and nothing is announced that this device cannot date.
            NewDeviceNoticeState()
        }
    }

    public fun save(accountIdB64: String, state: NewDeviceNoticeState) {
        storage.set(accountIdB64, JSON.encodeToString(NewDeviceNoticeState.serializer(), state))
    }

    public fun deleteAccount(accountIdB64: String) {
        storage.remove(accountIdB64)
    }

    public companion object {
        public const val PREFS_NAME: String = "lakko-new-device-notice"

        private val JSON = Json { ignoreUnknownKeys = true }

        public fun onDevice(context: Context): NewDeviceNoticeStore =
            NewDeviceNoticeStore(SharedPrefsMarkerStorage(context, PREFS_NAME))
    }
}

/**
 * The live notice of the signed-in account, process-wide: the DeviceGroup
 * sync runtime feeds it every roster it sees, the Chats list and Settings >
 * Devices draw it.
 */
public object NewDeviceNotices {
    private val mutableState: MutableStateFlow<NewDeviceNoticeState> = MutableStateFlow(NewDeviceNoticeState())

    /** The bound account's notice state; the default while none is bound. */
    public val state: StateFlow<NewDeviceNoticeState> = mutableState.asStateFlow()

    private var store: NewDeviceNoticeStore? = null
    private var boundAccount: String? = null

    /** Bind the account whose DeviceGroup is open. Idempotent. */
    @Synchronized
    public fun bind(context: Context, accountIdB64: String) {
        if (boundAccount == accountIdB64 && store != null) return
        val s = NewDeviceNoticeStore.onDevice(context)
        store = s
        boundAccount = accountIdB64
        mutableState.value = s.load(accountIdB64)
    }

    /** Sign-out, wipe, or the device was removed: nothing is shown. The
     *  stored row stays for the account's next sign-in (the wipe removes it). */
    @Synchronized
    public fun unbind() {
        boundAccount = null
        store = null
        mutableState.value = NewDeviceNoticeState()
    }

    @Synchronized
    private fun update(accountIdB64: String, change: (NewDeviceNoticeState) -> NewDeviceNoticeState) {
        if (boundAccount != accountIdB64) return
        val s = store ?: return
        val next = change(mutableState.value)
        if (next == mutableState.value) return
        try {
            s.save(accountIdB64, next)
        } catch (cause: Throwable) {
            Log.w(TAG, "new-device notice not persisted: ${cause.javaClass.simpleName}")
        }
        mutableState.value = next
    }

    /**
     * The roster as the sync runtime sees it now. Posts the local
     * notification when a device became new while the app is running but not
     * in front (UX 17.5); a closed app shows the banner at its next open.
     */
    public fun onRoster(context: Context, accountIdB64: String, roster: Set<String>, selfKey: String?) {
        var fresh: Set<String> = emptySet()
        update(accountIdB64) { current ->
            val (next, became) = observeRoster(current, roster, selfKey, System.currentTimeMillis())
            fresh = became
            next
        }
        if (fresh.isNotEmpty() && !me.lakko.app.push.isAppForegrounded(context)) {
            postNotification(context.applicationContext)
        }
    }

    /** This device let a device in: the roster is known (no banner here). */
    public fun onLetIn(accountIdB64: String, roster: Set<String>) {
        update(accountIdB64) { acknowledgeRoster(it, roster) }
    }

    /** `Dismiss` on the banner. */
    public fun dismiss() {
        boundAccount?.let { account -> update(account) { dismissNotice(it) } }
    }

    /** This device removed the device of [deviceKey]; [wasNew] is whether
     *  its row carried `New` before the removal. */
    public fun onRemovedHere(deviceKey: String, wasNew: Boolean) {
        boundAccount?.let { account -> update(account) { removedHere(it, deviceKey, wasNew) } }
    }

    /** The removed line was dismissed. */
    public fun dismissRemoved() {
        boundAccount?.let { account -> update(account) { dismissRemovedLine(it) } }
    }

    private val mutableOpenDevices: MutableStateFlow<Boolean> = MutableStateFlow(false)

    /** The notification was tapped: the app opens Settings > Devices once a
     *  session is on screen, then [consumeOpenDevices]. */
    public val openDevicesRequested: StateFlow<Boolean> = mutableOpenDevices.asStateFlow()

    public fun requestOpenDevices() {
        mutableOpenDevices.value = true
    }

    public fun consumeOpenDevices() {
        mutableOpenDevices.value = false
    }

    /** The intent extra a tap on the notification carries: open Settings >
     *  Devices. */
    public const val EXTRA_OPEN_DEVICES: String = "me.lakko.app.OPEN_DEVICES"

    private fun postNotification(context: Context) {
        try {
            val strings = me.lakko.app.l10n.Localization.strings.value
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                context.getSystemService(NotificationManager::class.java)?.createNotificationChannel(
                    NotificationChannel(
                        CHANNEL_ID,
                        strings["devices.added_notice.title"],
                        NotificationManager.IMPORTANCE_DEFAULT,
                    ),
                )
            }
            val launch = context.packageManager.getLaunchIntentForPackage(context.packageName)
                ?: Intent(context, me.lakko.app.MainActivity::class.java)
            launch.addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
            launch.putExtra(EXTRA_OPEN_DEVICES, true)
            val pending = PendingIntent.getActivity(
                context,
                NOTIFICATION_ID,
                launch,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )
            // It names no device and no place (UX 17.5).
            val notification = NotificationCompat.Builder(context, CHANNEL_ID)
                .setSmallIcon(android.R.drawable.sym_def_app_icon)
                .setContentTitle(strings["devices.added_notice.title"])
                .setContentText(strings["devices.added_notice.notification_body"])
                .setAutoCancel(true)
                .setContentIntent(pending)
                .build()
            NotificationManagerCompat.from(context).notify(NOTIFICATION_ID, notification)
        } catch (cause: SecurityException) {
            // Android 13+ without the notification permission: the banner at
            // the next open is the notice.
            Log.w(TAG, "new-device notification not posted (permission)")
        } catch (cause: Throwable) {
            Log.w(TAG, "new-device notification not posted: ${cause.javaClass.simpleName}")
        }
    }

    private const val TAG: String = "lakko-new-device"
    private const val CHANNEL_ID: String = "lakko-account-security"
    private const val NOTIFICATION_ID: Int = 0x4e44
}

/** The device key the notice uses for a DeviceGroup leaf: lowercase hex of
 *  its signature public key. */
public fun newDeviceKey(signaturePublicKey: ByteArray): String =
    signaturePublicKey.joinToString("") { "%02x".format(it) }
