// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import me.lakko.app.l10n.UiText
import uniffi.lakko_ffi_uniffi.FfiCallEndReason
import uniffi.lakko_ffi_uniffi.FfiCallSignalingBody

/**
 * Ending a group call for everyone (`docs/ux/calls.md` section 4.1.2). The
 * Kotlin twin of `apps/web/src/calls/end-for-everyone.ts`.
 *
 * The person who started a group or link call gets a choice behind `End`:
 * leave, or end the call for every person in it. The end travels as an
 * ordinary `CallEnd` with the reason [FfiCallEndReason.ENDED_FOR_ALL], inside
 * the conversation's encryption (the call's own group for a link call), and a
 * device takes itself out of the call only when the MLS-authenticated sender is
 * that call's starter. The same message from anyone else is an ordinary end
 * notice and takes nobody out. The server is never asked.
 *
 * The rules are pure: the FFI types they read are plain Kotlin classes, so the
 * host unit suite drives them without the UniFFI `.so`.
 */

/**
 * Whether pressing `End` opens the choice rather than ending the call.
 *
 * Only for the starter, and only while someone else is in the call: alone,
 * both rows would do the same thing.
 */
public fun endOpensMenu(isStarter: Boolean, othersInCall: Int): Boolean = isStarter && othersInCall > 0

/**
 * Whether a received end takes this device out of the call: the reason says
 * "for everyone" AND it comes from the call's starter. Anything less is not
 * enough: an unknown starter, an unresolved sender, or anyone else.
 *
 * @param senderPersonKey the sender's person key, resolved from the MLS sender
 *   at receipt (C-68); `null` when it could not be resolved. Never a leaf
 *   index, never a field of the body.
 * @param starterPersonKey the person who started the call, as this device
 *   recorded it; `null` when it does not know.
 */
public fun endsTheCallForMe(
    reason: FfiCallEndReason,
    senderPersonKey: String?,
    starterPersonKey: String?,
): Boolean =
    reason == FfiCallEndReason.ENDED_FOR_ALL &&
        !senderPersonKey.isNullOrEmpty() &&
        starterPersonKey != null &&
        senderPersonKey == starterPersonKey

/** The line that tells the others why their call closed. */
public fun endedForEveryoneLine(starterName: String?): UiText {
    val name = starterName?.trim().orEmpty()
    return if (name.isNotEmpty()) {
        UiText("call.ended.for_all_by", texts = mapOf("name" to name))
    } else {
        UiText("call.ended.for_all")
    }
}

// ---- Chat group call --------------------------------------------------------

/**
 * Whether the account behind [ownPersonKey] started the recorded call
 * [callIdHex]. Account-level: any device of the starter that is in the call.
 * A record that is gone, names another call, or carries no starter (stored
 * before the starter was recorded) answers `false`.
 */
internal fun groupCallStartedBy(record: GroupCallRecord?, callIdHex: String, ownPersonKey: String?): Boolean {
    if (record == null || record.callIdHex != callIdHex) return false
    val starter = record.initiatorPersonKey
    return !starter.isNullOrEmpty() && starter == ownPersonKey
}

/**
 * The starter whose end-for-everyone [body] is, for the call [record] is
 * about; `null` when the body takes nobody out of a call.
 *
 * Read BEFORE the End is folded into the ledger, which closes the record and
 * with it the only place the starter is written down.
 *
 * @param senderPersonKey the row's MLS sender resolved to a person at receipt.
 */
internal fun groupCallEndedForEveryoneBy(
    record: GroupCallRecord?,
    body: FfiCallSignalingBody,
    senderPersonKey: String?,
): String? {
    if (body !is FfiCallSignalingBody.End || record == null) return null
    if (record.callIdHex != callIdHexOf(body.callId)) return null
    val starter = record.initiatorPersonKey
    return starter?.takeIf { endsTheCallForMe(body.reason, senderPersonKey, it) }
}

// ---- Link call --------------------------------------------------------------

/**
 * Whether a member's sealed application message, once parsed, is "this call
 * has been ended for everyone". States an intent only; the caller checks who
 * sent it. `null` is a plaintext that is not call signaling at all.
 */
internal fun isEndForEveryone(body: FfiCallSignalingBody?, callId: ByteArray): Boolean =
    body is FfiCallSignalingBody.End &&
        body.reason == FfiCallEndReason.ENDED_FOR_ALL &&
        body.callId.contentEquals(callId)

/**
 * Whether the MLS-authenticated sender of a link call's application message is
 * the link's owner: the sender leaf's signature key equals the owner key the
 * SIGNED capability names, the same key the post-join owner-membership check
 * uses. An unresolved leaf is nobody.
 */
internal fun linkCallSenderIsOwner(senderLeafKeyHex: String?, ownerKeyHex: String): Boolean =
    !senderLeafKeyHex.isNullOrEmpty() && ownerKeyHex.isNotEmpty() && senderLeafKeyHex == ownerKeyHex

// ---- The notice -------------------------------------------------------------

/** How long "{name} ended the call for everyone" stays on screen. */
public const val ENDED_FOR_EVERYONE_NOTICE_MS: Long = 6_000L

/**
 * The one line that says a call this device was in was ended for everyone,
 * held for [holdMs] after it happened. One per process, shared by the group
 * and the link call (there is one call slot), and drawn above every screen.
 */
public class EndedForEveryoneNotice(
    private val scope: CoroutineScope,
    private val holdMs: Long = ENDED_FOR_EVERYONE_NOTICE_MS,
) {
    private val mutableLine = MutableStateFlow<UiText?>(null)

    /** The line to show, or `null`. */
    public val line: StateFlow<UiText?> = mutableLine.asStateFlow()

    private var clear: Job? = null

    /** Show the line for [starterName] (`null` when the device cannot name
     *  the starter). A second end inside the window restarts it. */
    public fun show(starterName: String?) {
        showLine(endedForEveryoneLine(starterName))
    }

    /** Show [line]: why a call closed that this device did not hang up. */
    public fun showLine(line: UiText) {
        mutableLine.value = line
        clear?.cancel()
        clear = scope.launch {
            delay(holdMs)
            mutableLine.value = null
        }
    }

    /** Sign-out: nothing of the previous session stays on screen. */
    public fun reset() {
        clear?.cancel()
        clear = null
        mutableLine.value = null
    }
}
