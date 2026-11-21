// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * The chat of a group call (`docs/ux/calls.md` section 4.1.4,
 * `lakko_calls::call_chat`): the protocol half. The Kotlin twin of the web
 * `calls/call-chat.ts` and built the way [CallNameChannel] is.
 *
 * A message rides the sealed SFU relay (ADR-0078 section 2), signed with the
 * sender's MLS leaf key, and a receiver takes it only from the peer whose
 * verified announcement named that leaf key. Nothing is stored: the channel
 * lives with the call session and is cleared when the call ends for this
 * device.
 *
 * - This device numbers its messages from 1 for the life of its SFU peer id.
 *   A message that could not be sent keeps its number and its bytes, and
 *   `Send again` sends exactly those bytes.
 * - A message from a peer whose announcement has not verified yet is held (at
 *   most [MAX_PENDING_PER_PEER] per peer, the oldest dropped) and read once
 *   the announcement verifies; one from a peer never verified is never shown.
 * - Each `(peer, seq)` is shown once: a relay frame re-sent after a reconnect
 *   must not show twice.
 * - Unlike the call name, a message is not re-sent on re-announcement.
 *
 * Pure: no native library, no network, so the rule is tested on the host JVM.
 * Not thread-safe: the session uses it from its main-confined scope.
 */

/** A received message after verification. */
public data class VerifiedCallChatView(
    /** The message's sequence number, per sender. */
    val seq: ULong,
    /** The text; empty for a picture. */
    val text: String,
    /** The picture (section 4.1.4.1), for a picture message. */
    val picture: CallChatPictureRef? = null,
)

/** What the channel is plugged into. */
public class CallChatChannelDeps(
    /** This device's signed message as `selfPeerId`, numbered `seq`, or
     *  `null` when it cannot make one yet (no group, no peer id). */
    public val compose: (selfPeerId: String, seq: ULong, text: String) -> ByteArray?,
    /** A message from `from`, verified against the leaf key its announcement
     *  verified with, or `null` on any refusal. */
    public val verify: (from: String, signerKey: ByteArray, bytes: ByteArray) -> VerifiedCallChatView?,
    /** Seal and send a relay plaintext to the whole call; `false` when it
     *  could not (no relay key, no socket). */
    public val sendSealed: (plaintext: ByteArray) -> Boolean,
    /** This device's SFU peer id, once the SFU's Welcome named it. */
    public val selfPeerId: () -> String?,
    /** A verified message from [peerId], shown once. */
    public val onMessage: (peerId: String, seq: ULong, text: String) -> Unit,
    /** A message did not verify: one log line, no text, no ids. */
    public val onRejected: () -> Unit = {},
    /** This device's signed picture message (section 4.1.4.1) as
     *  `selfPeerId`, numbered `seq`, or `null` when it cannot make one yet. */
    public val composePicture: (selfPeerId: String, seq: ULong, picture: CallChatPictureRef) -> ByteArray? =
        { _, _, _ -> null },
    /** A verified picture message from [peerId], shown once. */
    public val onPicture: (peerId: String, seq: ULong, picture: CallChatPictureRef) -> Unit = { _, _, _ -> },
)

/** What happened to a message this device wrote. */
public data class CallChatSendResult(
    /** The message's number; `Send again` names it. */
    val seq: ULong,
    /** Whether it went to the socket. */
    val sent: Boolean,
)

/** The chat state of one SFU call session. */
public class CallChatChannel(private val deps: CallChatChannelDeps) {
    /** A message this device wrote: a text, or a picture whose reference
     *  is attached once its upload finished. */
    private class Outgoing(val text: String?, var picture: CallChatPictureRef?, var wire: ByteArray?)

    private var ownSeq: ULong = 0uL
    private val outgoing = HashMap<ULong, Outgoing>()
    private val peerKeys = HashMap<String, ByteArray>()
    private val pending = HashMap<String, ArrayDeque<ByteArray>>()
    private val shown = HashSet<Pair<String, ULong>>()

    /** Write [text] (already passed the core's text rule): numbered, signed
     *  and sent. The message is kept, sent or not, so it can be sent again. */
    public fun send(text: String): CallChatSendResult {
        val seq = reserve(text)
        return CallChatSendResult(seq, resend(seq))
    }

    /**
     * Number a message without sending it: [text], or a picture when
     * [text] is `null` (its reference is attached with [attachPicture] once
     * uploaded). Pictures and texts share one numbering per sender.
     */
    public fun reserve(text: String?): ULong {
        ownSeq += 1uL
        outgoing[ownSeq] = Outgoing(text, picture = null, wire = null)
        return ownSeq
    }

    /** Picture message [seq] was uploaded: what it carries. A text, or a
     *  picture already attached, is left as it is. */
    public fun attachPicture(seq: ULong, picture: CallChatPictureRef) {
        val message = outgoing[seq] ?: return
        if (message.text == null && message.picture == null) message.picture = picture
    }

    /** Send message [seq] again: the same bytes, the same number. `false`
     *  when it still could not go, or there is no such message. */
    public fun resend(seq: ULong): Boolean {
        val message = outgoing[seq] ?: return false
        return transmit(seq, message)
    }

    private fun transmit(seq: ULong, message: Outgoing): Boolean {
        val wire = message.wire ?: run {
            val self = deps.selfPeerId() ?: return false
            val text = message.text
            val picture = message.picture
            val composed = when {
                text != null -> deps.compose(self, seq, text)
                picture != null -> deps.composePicture(self, seq, picture)
                // A picture still uploading has nothing to send yet.
                else -> null
            }
            composed?.also { message.wire = it } ?: return false
        }
        return deps.sendSealed(wire)
    }

    /** [peerId]'s announcement verified with [signerKey]: what it wrote
     *  before is read now, in the order it arrived. */
    public fun onPeerIdentified(peerId: String, signerKey: ByteArray) {
        peerKeys[peerId] = signerKey
        val held = pending.remove(peerId) ?: return
        for (bytes in held) onReceived(peerId, bytes)
    }

    /** A sealed chat message from [peerId] opened. */
    public fun onReceived(peerId: String, bytes: ByteArray) {
        val key = peerKeys[peerId]
        if (key == null) {
            val queue = pending.getOrPut(peerId) { ArrayDeque() }
            if (queue.size >= MAX_PENDING_PER_PEER) queue.removeFirst()
            queue.addLast(bytes)
            return
        }
        val verified = deps.verify(peerId, key, bytes)
        if (verified == null) {
            deps.onRejected()
            return
        }
        if (!shown.add(peerId to verified.seq)) return
        val picture = verified.picture
        if (picture != null) {
            deps.onPicture(peerId, verified.seq, picture)
        } else {
            deps.onMessage(peerId, verified.seq, verified.text)
        }
    }

    /** [peerId] left. What it wrote and was shown stays; what was held for
     *  it, never verified, goes. */
    public fun onPeerLeft(peerId: String) {
        peerKeys.remove(peerId)
        pending.remove(peerId)
    }

    /** The call is over for this device: everything goes. */
    public fun clear() {
        ownSeq = 0uL
        outgoing.clear()
        peerKeys.clear()
        pending.clear()
        shown.clear()
    }

    public companion object {
        /** Messages held per peer before its announcement verifies. */
        public const val MAX_PENDING_PER_PEER: Int = 50
    }
}
