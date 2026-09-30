// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discover

import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import me.lakko.app.profile.ProfileRepublishScheduler
import uniffi.lakko_ffi_uniffi.FfiSharedChannel
import uniffi.lakko_ffi_uniffi.FfiSharedChannels

/**
 * ADR-0059 section 6 — channels people in your groups follow, the session
 * runtime on Android. Mirror of `apps/web/src/discover/SharedChannelsContext.tsx`.
 *
 * Owns, for the signed-in account:
 *
 *   * the RECEIVE side: a member's list is filed under the member's ACCOUNT
 *     key resolved at receipt (C-68), strictly newer stamp wins
 *     (`sharedChannelsIsNewer`);
 *   * the "Share my channels with this group" switch per group, and the SEND
 *     side: the list is sent when the switch goes on, an empty list when it
 *     goes off, again whenever the set of listed channels this device follows
 *     changes, and re-sent VERBATIM with its original stamp when the group
 *     grows (the profile republish scheduler, throttled + jittered);
 *   * the sealed row both live in ([DiscoverStore]).
 *
 * ## Locking
 *
 * One [mutex] serialises every read-decide-write of the row (C-62
 * sub-invariant 5): the row is one sealed file per account, so a per-group lock
 * would still race on the file. A receive is LAUNCHED from the poll's locked
 * apply region rather than run inside it, because a send holds [mutex] while
 * [broadcast] takes the group lock: running the receive inline (group lock,
 * then [mutex]) would close a cycle. Out-of-order receives are harmless: the
 * strictly-newer rule keeps the newest list whatever the arrival order.
 */
public class SharedChannelsRepository(
    private val account: () -> String?,
    /** Publish [envelope] into the group (the audited control-message send). */
    private val broadcast: suspend (groupIdHex: String, envelope: ByteArray) -> Unit,
    /** The group is held on this device and this device is still a member. */
    private val isLive: (groupIdHex: String) -> Boolean,
    private val scope: CoroutineScope,
    private val compose: (SharedList) -> ByteArray = ::composeSharedList,
    private val isNewer: (ULong, ULong) -> Boolean = { a, b -> uniffi.lakko_ffi_uniffi.sharedChannelsIsNewer(a, b) },
    private val load: (String) -> SharedChannelsRow,
    private val save: (String, SharedChannelsRow) -> Boolean,
    /** Diagnostics; never carries a channel, a handle or a member key. */
    private val log: (String, Throwable?) -> Unit = { m, t -> Log.w(TAG, m, t) },
) {
    private val mutex = Mutex()
    private var boundAccount: String? = null
    private val _row = MutableStateFlow(SharedChannelsRow())

    /** The row of the signed-in account (received lists, switches, what was sent). */
    public val row: StateFlow<SharedChannelsRow> = _row.asStateFlow()

    /**
     * The channels this device could share now, or `null` while the channel
     * list is not loaded for this account (then nothing is re-sent: an empty
     * list read before loading would withdraw every shared channel). Set by the
     * app shell from [me.lakko.app.channels.ChannelsUiState].
     */
    @Volatile
    public var shareable: () -> List<ShareableChannel>? = { null }

    private val growth = ProfileRepublishScheduler(scope = scope, publish = { republishInto(it) })

    /** The row for the current account, loaded on first use. Under [mutex]. */
    private fun current(): Pair<String, SharedChannelsRow>? {
        val acct = account() ?: return null
        if (boundAccount != acct) {
            boundAccount = acct
            _row.value = try {
                load(acct)
            } catch (_: Throwable) {
                SharedChannelsRow()
            }
        }
        return acct to _row.value
    }

    private fun install(acct: String, next: SharedChannelsRow) {
        _row.value = next
        if (!save(acct, next)) log("shared channels row not saved", null)
    }

    /** Load the row for the signed-in account (after sign-in). */
    public suspend fun open() {
        mutex.withLock { current() }
    }

    /** Whether the switch is on in [groupIdHex]. */
    public fun isSharing(groupIdHex: String): Boolean = groupIdHex in _row.value.shareOn

    private fun currentChannels(): List<SharedChannel>? = shareable()?.let(::channelsToShare)

    private suspend fun sendFreshLocked(acct: String, groupIdHex: String, channels: List<SharedChannel>) {
        val held = _row.value
        val list = SharedList(freshListStamp(System.currentTimeMillis(), held.sent[groupIdHex]), channels)
        broadcast(groupIdHex, compose(list))
        install(acct, _row.value.copy(sent = _row.value.sent + (groupIdHex to list)))
    }

    /**
     * Turn the switch on or off: sends the list (or an empty one), then stores
     * the switch. Throws the send failure; the switch does not change.
     */
    public suspend fun setSharing(groupIdHex: String, on: Boolean) {
        mutex.withLock {
            val (acct, _) = current() ?: throw IllegalStateException("Sign in again.")
            if (!isLive(groupIdHex)) throw IllegalStateException("You are no longer in this group.")
            val channels = if (on) currentChannels() ?: emptyList() else emptyList()
            sendFreshLocked(acct, groupIdHex, channels)
            val shareOn = if (on) _row.value.shareOn + groupIdHex else _row.value.shareOn - groupIdHex
            install(acct, _row.value.copy(shareOn = shareOn))
        }
    }

    /** File an incoming list. Launched, never awaited under the group lock. */
    public fun receive(groupIdHex: String, senderKey: String?, list: SharedList) {
        scope.launch {
            try {
                mutex.withLock {
                    val (acct, row) = current() ?: return@withLock
                    val applied = applyIncomingList(row, groupIdHex, senderKey, list, isNewer)
                    if (applied.verdict == SharedChannelsVerdict.ACCEPT) {
                        install(acct, applied.row)
                    } else {
                        log("shared channels list not filed: ${applied.verdict}", null)
                    }
                }
            } catch (cause: Throwable) {
                log("shared channels receive failed", cause)
            }
        }
    }

    /** The group gained a member: schedule the verbatim re-send. */
    public fun groupGrew(groupIdHex: String) {
        if (groupIdHex in _row.value.shareOn) growth.onGrowth(groupIdHex)
    }

    /** Growth: re-send what was last sent, with its ORIGINAL stamp. */
    private suspend fun republishInto(groupIdHex: String): Boolean {
        val sent = mutex.withLock {
            current() ?: return false
            if (groupIdHex !in _row.value.shareOn) return false
            _row.value.sent[groupIdHex]
        } ?: return false
        if (!isLive(groupIdHex)) return false
        return try {
            broadcast(groupIdHex, compose(sent))
            true
        } catch (cause: Throwable) {
            log("shared channels growth re-send failed", cause)
            false
        }
    }

    private var resendJob: Job? = null

    /** A change of the followed listed set (or of the row): re-send, debounced,
     *  to every group the switch is on in whose last list differs. */
    public fun followedSetChanged() {
        resendJob?.cancel()
        resendJob = scope.launch {
            delay(RESEND_DEBOUNCE_MS)
            resendChanged()
        }
    }

    private suspend fun resendChanged() {
        mutex.withLock {
            val (acct, _) = current() ?: return
            val channels = currentChannels() ?: return
            for (groupIdHex in _row.value.shareOn) {
                val sent = _row.value.sent[groupIdHex]
                if (sent != null && sameChannels(sent.channels, channels)) continue
                if (!isLive(groupIdHex)) continue
                try {
                    sendFreshLocked(acct, groupIdHex, channels)
                } catch (cause: Throwable) {
                    log("shared channels re-send failed", cause)
                }
            }
        }
    }

    /** The group was deleted from this device. */
    public fun forgetGroup(groupIdHex: String) {
        scope.launch {
            mutex.withLock {
                val (acct, row) = current() ?: return@withLock
                if (groupIdHex in row.received || groupIdHex in row.sent || groupIdHex in row.shareOn) {
                    install(acct, forgetGroup(row, groupIdHex))
                }
            }
        }
    }

    /** Sign-out / forget device: drop what is in memory (the file is the
     *  caller's to delete). */
    public fun clear() {
        resendJob?.cancel()
        boundAccount = null
        _row.value = SharedChannelsRow()
    }

    private companion object {
        const val TAG = "SharedChannels"

        /** A burst of subscriptions becomes one list (chosen). */
        const val RESEND_DEBOUNCE_MS = 3_000L
    }
}

/** The FFI compose of one list. */
public fun composeSharedList(list: SharedList): ByteArray =
    uniffi.lakko_ffi_uniffi.composeSharedChannelsEnvelope(
        FfiSharedChannels(
            updatedAtMs = list.updatedAtMs,
            channels = list.channels.map {
                FfiSharedChannel(
                    channelId = me.lakko.app.channels.ChannelsClient.channelIdBytes(it.channelIdHex),
                    handle = it.handle,
                    name = it.name,
                )
            },
        ),
    )

/** The FFI decode of one envelope, `null` for a malformed one (dropped). */
public fun decodeSharedListOrNull(bytes: ByteArray): SharedList? =
    try {
        val d = uniffi.lakko_ffi_uniffi.decodeSharedChannelsEnvelope(bytes)
        SharedList(
            updatedAtMs = d.updatedAtMs,
            channels = d.channels.map { c ->
                SharedChannel(c.channelId.joinToString("") { "%02x".format(it) }, c.handle, c.name)
            },
        )
    } catch (_: Throwable) {
        null
    }
