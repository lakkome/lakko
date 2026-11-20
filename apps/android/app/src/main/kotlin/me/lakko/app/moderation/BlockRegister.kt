// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.moderation

import android.content.Context
import android.util.Log
import java.util.Base64
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import uniffi.lakko_ffi_uniffi.BlockPath
import uniffi.lakko_ffi_uniffi.BlockVerdict

/*
 * ADR-0099 section 6, UX `reporting-and-moderation.md` section 1 — the block
 * register.
 *
 * The register itself lives in the account's synced state (`lakko-sync`),
 * folded and carried between the account's own devices by the DeviceGroup
 * sync runtime exactly like the device acknowledgements. This file is the
 * read side every receive path consults, and the one place a screen writes
 * through: the runtime publishes the folded list here after every change and
 * binds the writer while a synced state is open.
 *
 * What a block DOES on each path is never decided here: [BlockRules] asks the
 * shared rule (`blockVerdict` in `lakko-ui-rules`) and the callers act on the
 * verdict.
 */

/** One blocked person: the account signature key (standard base64) and when. */
public data class BlockedPerson(val accountKeyB64: String, val blockedAtMs: Long)

/** The register of one account, most recent block first. */
public data class BlockedState(
    val accountIdB64: String,
    val blocked: List<BlockedPerson>,
) {
    private val keys: Set<String> = blocked.mapTo(HashSet()) { canonicalKey(it.accountKeyB64) }

    /** Whether [accountKeyB64] (any base64 spelling) is blocked. */
    public fun isBlocked(accountKeyB64: String?): Boolean =
        accountKeyB64 != null && canonicalKey(accountKeyB64) in keys
}

/** Standard base64 of an account key given in any base64 spelling. */
internal fun canonicalKey(keyB64: String): String =
    me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(keyB64)

/** The decoded 32-byte account key, or `null` for anything else. */
internal fun accountKeyBytes(keyB64: String): ByteArray? = try {
    Base64.getDecoder().decode(canonicalKey(keyB64)).takeIf { it.size == ACCOUNT_KEY_BYTES }
} catch (_: IllegalArgumentException) {
    null
}

private const val ACCOUNT_KEY_BYTES: Int = 32

public object BlockRegister {
    private const val TAG = "lakko-block"

    private val mutable = MutableStateFlow<BlockedState?>(null)

    /** The signed-in account's register; `null` while no synced state is open. */
    public val state: StateFlow<BlockedState?> = mutable.asStateFlow()

    /** Called by the sync runtime after every change to the folded state. */
    public fun publish(value: BlockedState?) {
        if (mutable.value != value) mutable.value = value
    }

    /**
     * Writes one block or unblock into the synced state and sends it to the
     * account's other devices. Bound by the sync runtime while a synced state
     * is open; returns whether the register changed.
     */
    @Volatile
    public var writer: (suspend (accountKey: ByteArray, blocked: Boolean) -> Boolean)? = null

    /** Whether [accountKeyB64] is blocked by the signed-in account. */
    public fun isBlocked(accountKeyB64: String?): Boolean = state.value?.isBlocked(accountKeyB64) == true

    /**
     * Block or unblock [accountKeyB64]. `false` when no synced state is open
     * or the key is not an account key; the block then did not happen and the
     * caller says nothing changed.
     */
    public suspend fun setBlocked(accountKeyB64: String, blocked: Boolean): Boolean {
        val key = accountKeyBytes(accountKeyB64) ?: return false
        val write = writer ?: return false
        return try {
            write(key, blocked)
            true
        } catch (cause: kotlin.coroutines.cancellation.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            Log.w(TAG, "block not written (${cause.javaClass.simpleName})")
            false
        }
    }

    /**
     * The background paths (the FCM decrypt, a cold ring) may run with no
     * sync runtime open: they read the persisted synced state the way the
     * mute flag is read. Fails toward "not blocked": an unreadable state must
     * not silence a person the user never blocked.
     */
    public fun isBlockedOnDisk(context: Context, accountIdB64: String?, accountKeyB64: String?): Boolean {
        val live = state.value
        if (live != null && accountIdB64 != null && live.accountIdB64 == accountIdB64) {
            return live.isBlocked(accountKeyB64)
        }
        val account = accountIdB64?.takeIf { it.isNotEmpty() } ?: return false
        val key = accountKeyB64?.let(::accountKeyBytes) ?: return false
        return try {
            val blob = me.lakko.app.devices.DeviceGroupSnapshotStore.getSnapshot(
                context = context.applicationContext,
                accountIdB64 = account,
                dir = me.lakko.app.devices.DeviceGroupSnapshotStore.SYNC_STATE_DIR,
            ) ?: return false
            uniffi.lakko_ffi_uniffi.SyncedState.fromBytes(blob).use { it.isBlocked(key) }
        } catch (cause: Throwable) {
            Log.w(TAG, "could not read the block register (${cause.javaClass.simpleName})")
            false
        }
    }
}

/**
 * The shared rule, behind a seam the host suite can replace (it cannot load
 * the native core). Production is always `uniffi.lakko_ffi_uniffi.blockVerdict`.
 */
public object BlockRules {
    @Volatile
    public var verdict: (BlockPath, Boolean) -> BlockVerdict = { path, blocked ->
        uniffi.lakko_ffi_uniffi.blockVerdict(path, blocked)
    }

    /** The verdict for [path], for a person whose blocked state is [blocked]. */
    public fun of(path: BlockPath, blocked: Boolean): BlockVerdict = verdict(path, blocked)
}

/**
 * What a received chat row from [senderKeyB64] comes to: [BlockVerdict.SHOW],
 * [BlockVerdict.DROP] (a one-to-one: never stored, never shown, never
 * notified) or [BlockVerdict.COLLAPSE] (a group: stored, drawn as the muted
 * row). Own rows and unattributed rows are never subject to a block.
 */
public fun receivedMessageVerdict(
    oneToOne: Boolean,
    senderKeyB64: String?,
    isOwn: Boolean,
    isBlocked: (String?) -> Boolean = BlockRegister::isBlocked,
): BlockVerdict {
    if (isOwn || senderKeyB64 == null) return BlockVerdict.SHOW
    val path = if (oneToOne) BlockPath.ONE_TO_ONE_MESSAGE else BlockPath.GROUP_MESSAGE
    // Asked only for a blocked person: the rule's answer for anyone else is
    // `show` on every path, and the host suite has no native core to ask.
    if (!isBlocked(senderKeyB64)) return BlockVerdict.SHOW
    return BlockRules.of(path, true)
}

/**
 * Whether an incoming signal (typing, read receipt, profile update) from
 * [senderKeyB64] in a one-to-one is dropped. Signals in a group are not
 * subject to a block (UX section 1.2 names the one-to-one only).
 */
public fun dropsIncomingSignal(
    oneToOne: Boolean,
    senderKeyB64: String?,
    isBlocked: (String?) -> Boolean = BlockRegister::isBlocked,
): Boolean {
    if (!oneToOne || senderKeyB64 == null) return false
    if (!isBlocked(senderKeyB64)) return false
    return BlockRules.of(BlockPath.INCOMING_SIGNAL, true) == BlockVerdict.DROP
}

/**
 * Whether this device withholds an outgoing signal (typing, read receipt,
 * profile update) in the one-to-one with [peerKeyB64].
 */
public fun withholdsOutgoingSignal(
    oneToOne: Boolean,
    peerKeyB64: String?,
    isBlocked: (String?) -> Boolean = BlockRegister::isBlocked,
): Boolean {
    if (!oneToOne || peerKeyB64 == null) return false
    if (!isBlocked(peerKeyB64)) return false
    return BlockRules.of(BlockPath.OUTGOING_SIGNAL, true) == BlockVerdict.WITHHOLD
}

/** Whether a contact request from [requesterKeyB64] is dropped unseen, whatever route it came by. */
public fun dropsContactRequest(
    requesterKeyB64: String?,
    isBlocked: (String?) -> Boolean = BlockRegister::isBlocked,
): Boolean {
    if (requesterKeyB64 == null) return false
    if (!isBlocked(requesterKeyB64)) return false
    return BlockRules.of(BlockPath.CONTACT_REQUEST, true) == BlockVerdict.DROP
}

/** Whether an incoming call from [callerKeyB64] is declined without a ring and without a missed-call row. */
public fun declinesCallSilently(
    callerKeyB64: String?,
    isBlocked: (String?) -> Boolean = BlockRegister::isBlocked,
): Boolean {
    if (callerKeyB64 == null) return false
    if (!isBlocked(callerKeyB64)) return false
    return BlockRules.of(BlockPath.INCOMING_CALL, true) == BlockVerdict.DECLINE_SILENTLY
}

/** Whether a notification for anything of [senderKeyB64] is suppressed. */
public fun suppressesNotification(
    senderKeyB64: String?,
    isBlocked: (String?) -> Boolean = BlockRegister::isBlocked,
): Boolean {
    if (senderKeyB64 == null) return false
    if (!isBlocked(senderKeyB64)) return false
    return BlockRules.of(BlockPath.NOTIFICATION, true) == BlockVerdict.DROP
}

/** Whether a call tile of [participantKeyB64] carries the muted "Blocked" label. */
public fun labelsCallTile(
    participantKeyB64: String?,
    isBlocked: (String?) -> Boolean = BlockRegister::isBlocked,
): Boolean {
    if (participantKeyB64 == null) return false
    if (!isBlocked(participantKeyB64)) return false
    return BlockRules.of(BlockPath.CALL_TILE, true) == BlockVerdict.LABEL
}
