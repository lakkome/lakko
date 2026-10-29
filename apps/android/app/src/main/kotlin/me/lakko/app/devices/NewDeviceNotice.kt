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
 * section 17.5, amended 2026-10-29): a system notification, a badge on the
 * Settings destination and on the `Devices` row, and highlighted rows in
 * Settings > Devices, on every OTHER device of the account after a device
 * joins it. Seeing it on one device clears it on all of them.
 *
 * The trigger is this account's DeviceGroup roster gaining a device this
 * device does not treat as known. The DeviceGroup is the account's own
 * device list (it is what Settings > Devices draws, row for row), every
 * device of the account applies the Add commit that admits a new one, and no
 * server says anything: the fact is read off the MLS roster this device
 * already verified.
 *
 * Per device, persisted: the devices this device treats as known (seeded
 * with the devices present when this device joined, plus the ones it let in
 * and the ones it saw), the pending ones with the time this device learned
 * of each (the DeviceGroup records no join time), and whether the "removed"
 * line is up. Per account, synced (the `lakko-sync` device-acknowledgement
 * register, UX 17.6): the devices whose warning was seen on some device of
 * the account. A device pending here whose key is in that set is not
 * pending. Never on the device that was added, never on the device that let
 * it in.
 *
 * Everything below the state class is a pure function of its arguments, so
 * the rules are unit-tested on the host JVM without the native core.
 */

/** The notice state of one account on this device. Device keys are the hex
 *  of the device's DeviceGroup signature key. */
@Serializable
public data class NewDeviceNoticeState(
    /** Whether the known set was ever seeded. */
    val seeded: Boolean = false,
    /** Devices this device knows about and has nothing to say on. */
    val known: Set<String> = emptySet(),
    /** Devices learned of here and not yet seen, with when this device
     *  learned of each (unix ms). Read through [effectivePending]: a key the
     *  account acknowledged is not pending whatever this map holds. */
    val pending: Map<String, Long> = emptyMap(),
    /** A device that carried `New` was removed here: the removed line is up. */
    val removedNew: Boolean = false,
)

/** The explanation above the list: how many devices it is about (`title` /
 *  `title_many`) and the most recent join time this device knows. */
public data class NewDeviceExplanation(
    val count: Int,
    val latestAtMs: Long,
)

/** The devices pending here once the account's [acknowledged] set is taken
 *  out (UX 17.6). */
public fun effectivePending(state: NewDeviceNoticeState, acknowledged: Set<String>): Map<String, Long> =
    if (acknowledged.isEmpty()) state.pending else state.pending.filterKeys { it !in acknowledged }

/** Whether the badge on Settings and on the `Devices` row is up. */
public fun attentionVisible(state: NewDeviceNoticeState, acknowledged: Set<String>): Boolean =
    effectivePending(state, acknowledged).isNotEmpty()

/** The explanation for the rows in [highlighted], or `null` when none is. */
public fun newDeviceExplanation(highlighted: Map<String, Long>): NewDeviceExplanation? {
    val latest = highlighted.values.maxOrNull() ?: return null
    return NewDeviceExplanation(count = highlighted.size, latestAtMs = latest)
}

/**
 * The rows highlighted in Settings > Devices: the ones this visit saw
 * ([visit], kept for the rest of the visit so the person sees which row is
 * new) and anything still pending.
 */
public fun highlightedDevices(
    state: NewDeviceNoticeState,
    acknowledged: Set<String>,
    visit: Map<String, Long>?,
): Map<String, Long> = (visit ?: emptyMap()) + effectivePending(state, acknowledged)

/** The account's [acknowledged] set grew: those devices are known here. */
public fun applyAcknowledged(state: NewDeviceNoticeState, acknowledged: Set<String>): NewDeviceNoticeState {
    val seen = state.pending.keys.filter { it in acknowledged }
    if (seen.isEmpty()) return state
    return state.copy(known = state.known + seen, pending = state.pending - seen.toSet())
}

/**
 * The roster of the account's devices as this device now sees it.
 *
 * - Never seeded, or this device has only ever known itself (it founded a
 *   group of one and has now joined the account's real one): every device
 *   present is known. A device never shows the notice about itself or about
 *   the devices that were there when it joined.
 * - Otherwise a key that is neither known nor already pending nor
 *   [acknowledged] by the account (and is not this device) is NEW, learned at
 *   [nowMs]. An acknowledged key in the roster is known: a device learned of
 *   after it was seen elsewhere never shows anything. A pending key that left
 *   the roster is dropped: there is nothing left to decide about it.
 *
 * Returns the next state and the keys that became new in this step (what the
 * system notification is posted for).
 */
public fun observeRoster(
    state: NewDeviceNoticeState,
    roster: Set<String>,
    selfKey: String?,
    acknowledged: Set<String>,
    nowMs: Long,
): Pair<NewDeviceNoticeState, Set<String>> {
    val knowsOnlyItself = (state.known - setOfNotNull(selfKey)).isEmpty() && state.pending.isEmpty()
    if (!state.seeded || knowsOnlyItself) {
        return state.copy(seeded = true, known = state.known + roster, pending = emptyMap()) to emptySet()
    }
    val seenElsewhere = roster.filter { it in acknowledged && it !in state.known }
    val fresh = roster - state.known - state.pending.keys - acknowledged - setOfNotNull(selfKey)
    val pending = state.pending.filterKeys { it in roster } + fresh.associateWith { nowMs }
    val next = applyAcknowledged(state.copy(known = state.known + seenElsewhere, pending = pending), acknowledged)
    return next to fresh
}

/** This device let a device in (the code route or the scan): everything in
 *  [roster] is known. A second alert about the same act a moment later
 *  teaches people to dismiss without reading (UX 17.5). It acknowledges
 *  nothing for the account: the other devices still warn. */
public fun acknowledgeRoster(state: NewDeviceNoticeState, roster: Set<String>): NewDeviceNoticeState =
    state.copy(seeded = true, known = state.known + roster, pending = state.pending - roster)

/** What opening (or looking at) Settings > Devices does. */
public data class DevicesSeen(
    /** The state with every device this visit saw moved to known. */
    val state: NewDeviceNoticeState,
    /** The devices to acknowledge for the account (empty: nothing to send). */
    val toAcknowledge: Set<String>,
    /** The visit's snapshot: what stays highlighted until the screen is left. */
    val visit: Map<String, Long>,
)

/**
 * The person is on Settings > Devices ([visit] is what this visit already
 * saw, empty on entry): every device pending here is seen. It is
 * acknowledged for the whole account, known here, and stays highlighted for
 * the rest of the visit.
 *
 * Only once the account's synced state is [syncReady]: before that the
 * acknowledgement could not be written, so nothing is consumed - the devices
 * stay pending (highlighted, badge up) and are seen as soon as the state is
 * ready while the screen is still open.
 */
public fun seeDevices(
    state: NewDeviceNoticeState,
    acknowledged: Set<String>,
    visit: Map<String, Long>,
    syncReady: Boolean = true,
): DevicesSeen {
    val seen = effectivePending(state, acknowledged)
    if (!syncReady || seen.isEmpty()) return DevicesSeen(state, emptySet(), visit)
    return DevicesSeen(
        state = state.copy(known = state.known + seen.keys, pending = state.pending - seen.keys),
        toAcknowledge = seen.keys,
        visit = visit + seen,
    )
}

/** What happens to the system notification after a change. */
public sealed interface NoticeNotification {
    /** Post (or replace) it, about [count] devices. */
    public data class Post(val count: Int) : NoticeNotification

    /** Withdraw it: nothing is pending. */
    public data object Withdraw : NoticeNotification

    /** Leave it as it is. */
    public data object Keep : NoticeNotification
}

/**
 * The notification after a step in which [fresh] devices became new and the
 * effective pending set is now [pendingAfter]. Posted for a device the moment
 * this device learns of it, in front or not; withdrawn as soon as nothing is
 * pending, whatever device the acknowledgement came from.
 */
public fun noticeNotification(fresh: Set<String>, pendingAfter: Map<String, Long>): NoticeNotification = when {
    pendingAfter.isEmpty() -> NoticeNotification.Withdraw
    fresh.any { it in pendingAfter } -> NoticeNotification.Post(pendingAfter.size)
    else -> NoticeNotification.Keep
}

/**
 * This device removed [deviceKey]. When the row carried `New` ([wasNew],
 * read BEFORE the removal: the roster that follows it drops the key from the
 * pending set by itself), the removed line is shown. The key is not
 * acknowledged: it is gone.
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
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) { storage.set(accountIdB64, JSON.encodeToString(NewDeviceNoticeState.serializer(), state)) }
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

/** What the screens draw, derived from the state, the account's
 *  acknowledged set and the current Devices visit. */
public data class NewDeviceNoticeView(
    /** The badge on Settings and on the `Devices` row. */
    val attention: Boolean = false,
    /** The rows of Settings > Devices that carry the highlight and `New`. */
    val highlighted: Set<String> = emptySet(),
    /** The explanation above the list, on Settings > Devices only. */
    val explanation: NewDeviceExplanation? = null,
    /** The removed line on Settings > Devices. */
    val removedNew: Boolean = false,
)

/**
 * The live notice of the signed-in account, process-wide: the DeviceGroup
 * sync runtime feeds it every roster it sees and the account's acknowledged
 * set, the navigation and Settings draw the badge, Settings > Devices draws
 * the highlight and reports the visit.
 */
public object NewDeviceNotices {
    private val mutableState: MutableStateFlow<NewDeviceNoticeState> = MutableStateFlow(NewDeviceNoticeState())

    /** The bound account's notice state; the default while none is bound. */
    public val state: StateFlow<NewDeviceNoticeState> = mutableState.asStateFlow()

    private val mutableView: MutableStateFlow<NewDeviceNoticeView> = MutableStateFlow(NewDeviceNoticeView())

    /** What the screens draw. */
    public val view: StateFlow<NewDeviceNoticeView> = mutableView.asStateFlow()

    private var store: NewDeviceNoticeStore? = null
    private var boundAccount: String? = null
    private var appContext: Context? = null

    /** The account's acknowledged devices (hex keys), as the synced state
     *  holds them. */
    private var acknowledged: Set<String> = emptySet()

    /** Whether the account's synced state is open (the sync runtime has
     *  reported its acknowledged set at least once). Until then a visit
     *  consumes nothing ([seeDevices]). */
    private var syncReady: Boolean = false

    /** The current visit to Settings > Devices, `null` while the screen is
     *  not open. */
    private var visit: Map<String, Long>? = null

    /** Writes the account's acknowledgement for the given devices and sends
     *  it to the other devices; installed by the sync runtime. */
    private var acknowledgeSink: ((Set<String>) -> Unit)? = null

    /** Bind the account whose DeviceGroup is open. Idempotent. */
    @Synchronized
    public fun bind(context: Context, accountIdB64: String, acknowledge: (Set<String>) -> Unit) {
        appContext = context.applicationContext
        acknowledgeSink = acknowledge
        if (boundAccount == accountIdB64 && store != null) return
        val s = NewDeviceNoticeStore.onDevice(context)
        store = s
        boundAccount = accountIdB64
        acknowledged = emptySet()
        syncReady = false
        mutableState.value = s.load(accountIdB64)
        project()
    }

    /** Sign-out, wipe, or the device was removed: nothing is shown. The
     *  stored row stays for the account's next sign-in (the wipe removes it). */
    @Synchronized
    public fun unbind() {
        boundAccount = null
        store = null
        acknowledgeSink = null
        acknowledged = emptySet()
        syncReady = false
        mutableState.value = NewDeviceNoticeState()
        project()
    }

    private fun project() {
        val state = mutableState.value
        val highlighted = if (visit != null) highlightedDevices(state, acknowledged, visit) else emptyMap()
        mutableView.value = NewDeviceNoticeView(
            attention = attentionVisible(state, acknowledged),
            highlighted = highlighted.keys,
            explanation = newDeviceExplanation(highlighted),
            removedNew = state.removedNew,
        )
    }

    /**
     * Apply [change] to the bound account's state, then, while the person is
     * on Settings > Devices, see whatever is pending (the person is looking
     * at it). Persists, re-projects, posts or withdraws the notification, and
     * returns the devices to acknowledge for the account.
     */
    @Synchronized
    private fun update(
        accountIdB64: String,
        change: (NewDeviceNoticeState) -> Pair<NewDeviceNoticeState, Set<String>>,
    ): Set<String> {
        if (boundAccount != accountIdB64) return emptySet()
        val s = store ?: return emptySet()
        val before = mutableState.value
        val (changed, fresh) = change(before)
        var next = changed
        var toAcknowledge: Set<String> = emptySet()
        val current = visit
        if (current != null) {
            val seen = seeDevices(next, acknowledged, current, syncReady)
            next = seen.state
            visit = seen.visit
            toAcknowledge = seen.toAcknowledge
        }
        if (next != before) {
            try {
                s.save(accountIdB64, next)
            } catch (cause: Throwable) {
                Log.w(TAG, "new-device notice not persisted: ${cause.javaClass.simpleName}")
            }
            mutableState.value = next
        }
        project()
        when (val action = noticeNotification(fresh, effectivePending(next, acknowledged))) {
            is NoticeNotification.Post -> appContext?.let { postNotification(it, accountIdB64, action.count) }
            NoticeNotification.Withdraw -> appContext?.let { withdrawNotification(it, accountIdB64) }
            NoticeNotification.Keep -> Unit
        }
        return toAcknowledge
    }

    private fun send(toAcknowledge: Set<String>) {
        if (toAcknowledge.isEmpty()) return
        val sink = synchronized(this) { acknowledgeSink } ?: return
        sink(toAcknowledge)
    }

    /**
     * The roster as the sync runtime sees it now. Posts the system
     * notification when a device became new, whether the app is in front or
     * not (UX 17.5); a closed app learns of it at its next open.
     */
    public fun onRoster(accountIdB64: String, roster: Set<String>, selfKey: String?) {
        val ack = update(accountIdB64) { current ->
            observeRoster(current, roster, selfKey, acknowledged, System.currentTimeMillis())
        }
        send(ack)
    }

    /**
     * The account's acknowledged set as the synced state now holds it (on
     * bind, after a local acknowledgement, after one received from another
     * device). Those devices stop being pending here: the badge goes, the
     * notification is withdrawn when nothing is left, and their highlight
     * goes unless this visit saw them itself.
     */
    public fun onAcknowledged(accountIdB64: String, acknowledgedKeys: Set<String>) {
        val ack = synchronized(this) {
            if (boundAccount != accountIdB64) return
            // The first report also says the synced state is open: a visit
            // waiting on it is seen now.
            if (syncReady && acknowledgedKeys == acknowledged) {
                project()
                return
            }
            syncReady = true
            acknowledged = acknowledgedKeys
            update(accountIdB64) { applyAcknowledged(it, acknowledgedKeys) to emptySet() }
        }
        send(ack)
    }

    /** This device let a device in: the roster is known (nothing here). */
    public fun onLetIn(accountIdB64: String, roster: Set<String>) {
        val ack = update(accountIdB64) { acknowledgeRoster(it, roster) to emptySet() }
        send(ack)
    }

    /**
     * Settings > Devices opened: every pending device is seen, acknowledged
     * for the account and kept highlighted for this visit. Idempotent while
     * the visit lasts.
     */
    public fun enterDevices() {
        val ack = synchronized(this) {
            val account = boundAccount ?: return
            if (visit == null) visit = emptyMap()
            update(account) { it to emptySet() }
        }
        send(ack)
    }

    /** Settings > Devices left: the highlight and the explanation go. */
    @Synchronized
    public fun leaveDevices() {
        visit = null
        project()
    }

    /** Whether the row of [deviceKey] carries `New` right now (read BEFORE a
     *  removal for the removed line). */
    public fun isHighlighted(deviceKey: String): Boolean = deviceKey in view.value.highlighted

    /** This device removed the device of [deviceKey]; [wasNew] is whether
     *  its row carried `New` before the removal. */
    public fun onRemovedHere(deviceKey: String, wasNew: Boolean) {
        synchronized(this) {
            visit = visit?.let { it - deviceKey }
        }
        boundAccount?.let { account -> update(account) { removedHere(it, deviceKey, wasNew) to emptySet() } }
    }

    /** The removed line was dismissed. */
    public fun dismissRemoved() {
        boundAccount?.let { account -> update(account) { dismissRemovedLine(it) to emptySet() } }
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

    /** Withdraw the account's notification (also on the account's wipe). */
    public fun withdrawNotification(context: Context, accountIdB64: String) {
        try {
            NotificationManagerCompat.from(context).cancel(accountIdB64, NOTIFICATION_ID)
        } catch (cause: Throwable) {
            Log.w(TAG, "new-device notification not withdrawn: ${cause.javaClass.simpleName}")
        }
    }

    /**
     * Post (or replace) the account's notification. One per account: the tag
     * is the account and the id is fixed, so a second device replaces the
     * first notification instead of stacking. Never asks for the permission:
     * without it nothing is shown and the badge is the notice.
     */
    private fun postNotification(context: Context, accountIdB64: String, count: Int) {
        try {
            val strings = me.lakko.app.l10n.Localization.strings.value
            val title = strings[if (count > 1) "devices.added_notice.title_many" else "devices.added_notice.title"]
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
                .setSmallIcon(me.lakko.app.R.drawable.ic_stat_lakko)
                .setContentTitle(title)
                .setContentText(strings["devices.added_notice.notification_body"])
                .setAutoCancel(true)
                .setContentIntent(pending)
                .build()
            NotificationManagerCompat.from(context).notify(accountIdB64, NOTIFICATION_ID, notification)
        } catch (cause: SecurityException) {
            // Android 13+ without the notification permission: the badge is
            // the notice. The permission is never asked for here.
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

/** The signature key a notice key names, or `null` when it is not 32 bytes
 *  of lowercase hex. */
public fun newDeviceKeyBytes(hex: String): ByteArray? {
    if (hex.length != 64) return null
    val out = ByteArray(32)
    for (i in 0 until 32) {
        val hi = Character.digit(hex[2 * i], 16)
        val lo = Character.digit(hex[2 * i + 1], 16)
        if (hi < 0 || lo < 0) return null
        out[i] = ((hi shl 4) or lo).toByte()
    }
    return out
}
