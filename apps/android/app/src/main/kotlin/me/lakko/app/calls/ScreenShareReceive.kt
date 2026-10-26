// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import uniffi.lakko_ffi_uniffi.CallCurrentScreenShare
import uniffi.lakko_ffi_uniffi.CallScreenShareState
import uniffi.lakko_ffi_uniffi.VerifiedCallScreenShare

/**
 * Receiving a screen share (ADR-0083, `docs/ux/screen-share.md` section 5).
 *
 * This client never shares a screen. It receives the signed share states of
 * the call (over the sealed SFU relay in a group call, over the conversation
 * in a one-to-one), verifies each against the call's MLS roster, and folds
 * the verified ones through the shared `CallScreenShareState`: one share per
 * call, the same one on every device. What is here is the client's side of
 * that: holding the fold, telling the UI when the current share changed, and
 * the pure rules the screen draws from (which tile, which announcement, which
 * layout).
 *
 * The fold itself is the core's (`lakko-calls::screen_share`) and is never
 * re-implemented here. It is reached through [ScreenShareFold] because the
 * host JVM cannot load the native library: production passes
 * [NativeScreenShareFold], the unit suite a fake.
 */

/** What the UI knows of the current share. */
public data class ScreenShareView(
    /** The share's sequence number. */
    val seq: ULong,
    /** The sharer's leaf signature key, lower-case hex. */
    val signerKeyHex: String,
    /** The sharer's SFU peer id (`p2p` on a one-to-one). */
    val peerId: String,
    /** The share carries sound. */
    val withSound: Boolean,
    /** The share is this device's own (never on Android, which does not
     *  share; kept so the shape matches the fold's answer). */
    val isOwn: Boolean,
)

/** The core fold, as this client uses it. */
public interface ScreenShareFold {
    /** Fold one verified state; `true` when the current share changed. */
    public fun observe(state: VerifiedCallScreenShare): Boolean

    /** The participant with [signerKey] left: its share ends. */
    public fun participantLeft(signerKey: ByteArray): Boolean

    /** The current share, or `null`. */
    public fun current(): CallCurrentScreenShare?

    /** Release the native object. */
    public fun close()
}

/** The shared fold (`CallScreenShareState`). One per call. */
public class NativeScreenShareFold(ownKey: ByteArray) : ScreenShareFold {
    private val state = CallScreenShareState(ownKey)

    override fun observe(state: VerifiedCallScreenShare): Boolean = this.state.observe(state)

    override fun participantLeft(signerKey: ByteArray): Boolean = state.participantLeft(signerKey)

    override fun current(): CallCurrentScreenShare? = state.current()

    override fun close() {
        state.close()
    }
}

/** The view of a current share. */
public fun screenShareViewOf(current: CallCurrentScreenShare): ScreenShareView =
    ScreenShareView(
        seq = current.seq,
        signerKeyHex = hexOf(current.signerKey),
        peerId = current.peerId,
        withSound = current.withSound,
        isOwn = current.isOwn,
    )

/**
 * Holds one call's fold and says when its current share changed.
 *
 * [onChange] is called with the new current share (or `null`) after each
 * change, and only then. Not thread-safe: a session calls it from its one
 * main-confined scope.
 */
public class ScreenShareReceiver(
    private val fold: ScreenShareFold,
    private val onChange: (ScreenShareView?) -> Unit,
) {
    /** The current share as last read. */
    public var current: ScreenShareView? = null
        private set

    private var closed = false

    /**
     * A received state, already verified (`verifyCallScreenShare`), or `null`
     * when the verification refused it: then nothing changes. Returns `true`
     * when the current share changed.
     */
    public fun receive(verified: VerifiedCallScreenShare?): Boolean {
        if (closed || verified == null) return false
        try {
            fold.observe(verified)
        } catch (_: Throwable) {
            return false
        }
        return refresh()
    }

    /** The participant with [signerKey] left the call. */
    public fun participantLeft(signerKey: ByteArray): Boolean {
        if (closed) return false
        try {
            fold.participantLeft(signerKey)
        } catch (_: Throwable) {
            return false
        }
        return refresh()
    }

    /** The call ended: no share, and the fold is released. */
    public fun close() {
        if (closed) return
        closed = true
        fold.close()
        if (current != null) {
            current = null
            onChange(null)
        }
    }

    private fun refresh(): Boolean {
        // Read after every state, not only after one the fold calls a change:
        // the current share is the fold's answer, whatever it said.
        val next = try {
            fold.current()?.let(::screenShareViewOf)
        } catch (_: Throwable) {
            current
        }
        // A change the fold reports with the same current share (the stop of
        // a superseded one) is no change on screen.
        if (next == current) return false
        current = next
        onChange(next)
        return true
    }
}

/** The tile id of a share (beside its sharer's own tile, `peer-<peerId>`). */
public fun screenShareTileId(peerId: String): String = "$SCREEN_TILE_PREFIX$peerId"

/** Whether a roster id names a share tile. */
public fun isScreenShareTileId(id: String): Boolean = id.startsWith(SCREEN_TILE_PREFIX)

private const val SCREEN_TILE_PREFIX: String = "screen-"

/** What the screen reader is told when the current share changes (section
 *  5.3): the catalog key and the sharer to name. */
public data class ScreenShareAnnouncement(val key: String, val peerId: String)

/**
 * The polite announcement for a change from [before] to [after], or `null`.
 *
 * A new share is announced as started (a replacement too: someone else now
 * shares); a share that ended with nothing after it as stopped. A change of
 * nothing but the share's own details says nothing.
 */
public fun screenShareAnnouncement(before: ScreenShareView?, after: ScreenShareView?): ScreenShareAnnouncement? =
    when {
        after != null && (before == null || !sameShare(before, after)) ->
            ScreenShareAnnouncement("call.screen_share.started", after.peerId)
        after == null && before != null ->
            ScreenShareAnnouncement("call.screen_share.stopped", before.peerId)
        else -> null
    }

/** Whether two views are the same share (`(seq, signer)`, ADR-0083 section 2). */
public fun sameShare(a: ScreenShareView, b: ScreenShareView): Boolean =
    a.seq == b.seq && a.signerKeyHex == b.signerKeyHex

/**
 * The group layout across a share (`docs/ux/screen-share.md` section 5.2).
 *
 * [choice] is the person's layout choice now; [saved] is what it was before
 * the first share began, held while a share is live. A first share switches
 * to Focus and remembers the choice; a share that replaces another changes
 * nothing; the end of the last share puts the remembered choice back.
 */
public data class ShareLayout(val choice: GroupLayoutKind?, val saved: SavedChoice?)

/** A remembered layout choice (which may itself be "none made"). */
public data class SavedChoice(val choice: GroupLayoutKind?)

public fun shareLayoutTransition(
    before: ScreenShareView?,
    after: ScreenShareView?,
    choice: GroupLayoutKind?,
    saved: SavedChoice?,
): ShareLayout = when {
    before == null && after != null -> ShareLayout(GroupLayoutKind.FOCUS, saved ?: SavedChoice(choice))
    // The remembered choice, "none made" included; without one, as it is.
    before != null && after == null -> ShareLayout(if (saved != null) saved.choice else choice, null)
    else -> ShareLayout(choice, saved)
}

/**
 * Whether the stage offers the share with a chip (section 5.2): a share is
 * live, someone is pinned, and the stage is not the share.
 */
public fun shareChipShown(shareTileId: String?, pinnedId: String?, stageId: String?): Boolean =
    shareTileId != null && pinnedId != null && stageId != shareTileId

private fun hexOf(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it.toInt() and 0xff) }

/** Lower-case hex to bytes, or `null` for anything that is not. */
internal fun bytesOfHex(hex: String): ByteArray? {
    if (hex.length % 2 != 0) return null
    val out = ByteArray(hex.length / 2)
    for (i in out.indices) {
        val hi = Character.digit(hex[2 * i], 16)
        val lo = Character.digit(hex[2 * i + 1], 16)
        if (hi < 0 || lo < 0) return null
        out[i] = ((hi shl 4) or lo).toByte()
    }
    return out
}
