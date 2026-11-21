// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch

/**
 * Pictures pasted into the chat of a group call (`docs/ux/calls.md` section
 * 4.1.4.1).
 *
 * A picture does not fit the call's sealed relay, so it goes the way a
 * picture goes in a conversation: encrypted on this device with a fresh key,
 * uploaded to the file store as an unreadable blob that lives one idle day,
 * and the call-chat message carries only its address, key, nonce and size.
 * Every receiver downloads it at once and keeps it in memory only.
 *
 * - The picture is sent first, the text written with it second, so the two
 *   arrive in that order.
 * - A picture whose upload failed shows `Not sent.`; `Send again` tries the
 *   upload again with the same ciphertext, or, when the upload finished and
 *   only the message did not go, sends the same message bytes again.
 * - Every blob this device uploaded in the call is deleted when its call ends,
 *   through the owed blob delete (ADR-0065), including one whose upload was
 *   still running then.
 *
 * Pure: no Android, no native library, no network; the work is injected.
 * Used from the call's main-confined scope.
 */

/** Where a picture's ciphertext is and how to open it: the Kotlin twin of the
 *  core's `CallChatPicture`. [key] is secret and never logged. */
public class CallChatPictureRef(
    public val blobId: ByteArray,
    public val key: ByteArray,
    public val nonce: ByteArray,
    /** Plaintext size in bytes. */
    public val size: UInt,
    public val width: UShort,
    public val height: UShort,
) {
    public val pixelSize: CallChatPictureSize get() = CallChatPictureSize(width.toInt(), height.toInt())

    override fun equals(other: Any?): Boolean =
        other is CallChatPictureRef && blobId.contentEquals(other.blobId) && key.contentEquals(other.key) &&
            nonce.contentEquals(other.nonce) && size == other.size && width == other.width && height == other.height

    override fun hashCode(): Int {
        var h = blobId.contentHashCode()
        h = 31 * h + key.contentHashCode()
        h = 31 * h + nonce.contentHashCode()
        h = 31 * h + size.hashCode()
        h = 31 * h + width.hashCode()
        return 31 * h + height.hashCode()
    }

    /** No key, no blob id. */
    override fun toString(): String = "CallChatPictureRef(${width}x$height)"
}

/** A picture encrypted under a fresh key, before its upload. */
public class EncryptedCallChatPicture(
    public val blobId: ByteArray,
    public val key: ByteArray,
    public val nonce: ByteArray,
    public val ciphertext: ByteArray,
)

/** The session's half of the call chat, as the sender needs it. */
public interface CallChatWire {
    /** Number a message: [text], or a picture when `null`. */
    public suspend fun reserve(text: String?): ULong

    /** Picture message [seq] was uploaded. */
    public suspend fun attachPicture(seq: ULong, picture: CallChatPictureRef)

    /** Send message [seq]; `false` when it could not go. */
    public suspend fun transmit(seq: ULong): Boolean
}

/** The work a picture needs, injected so the rule is tested on the host. */
public class CallChatPictureDeps(
    /** Encrypt a prepared JPEG under a fresh key; `null` on failure. */
    public val encrypt: suspend (jpeg: ByteArray) -> EncryptedCallChatPicture?,
    /** Upload a ciphertext with an idle life of one day; `false` on failure. */
    public val put: suspend (picture: EncryptedCallChatPicture) -> Boolean,
    /** Download and decrypt a received picture; `null` when it is gone or
     *  does not open. */
    public val download: suspend (picture: CallChatPictureRef) -> ByteArray?,
    /** Delete these uploaded blobs through the owed delete (kept until the
     *  file store confirms, retried at the next start). */
    public val deleteUploaded: (blobIds: List<ByteArray>) -> Unit,
)

/** The blobs this device uploaded in one call, in upload order. */
public class CallChatUploads {
    private val ids = mutableListOf<ByteArray>()

    /** A blob is about to be uploaded: recorded before the upload, so one
     *  whose answer was lost is still deleted. */
    public fun add(blobId: ByteArray) {
        if (ids.none { it.contentEquals(blobId) }) ids += blobId
    }

    /** Every recorded blob, once: the list is empty after. */
    public fun takeAll(): List<ByteArray> = ids.toList().also { ids.clear() }

    public val count: Int get() = ids.size
}

/**
 * The chat control of a call that carries pictures: texts as before, and
 * pictures uploaded then sent as their own message.
 */
public class CallChatSender(
    private val scope: CoroutineScope,
    private val model: CallChatModel,
    private val wire: CallChatWire,
    private val deps: CallChatPictureDeps,
) : CallChatControl {
    /** An own picture message and how far it got. */
    private class Outgoing(val picture: PreparedCallChatPicture) {
        var encrypted: EncryptedCallChatPicture? = null
        var uploaded: Boolean = false
    }

    private val outgoing = HashMap<ULong, Outgoing>()
    private val inFlight = HashSet<ULong>()
    private val uploads = CallChatUploads()
    private val jobs = mutableListOf<Job>()
    private var ended = false

    override val takesPictures: Boolean get() = true

    override fun send(text: String) {
        launchTask {
            val seq = wire.reserve(text)
            model.addOwn(seq, text, wire.transmit(seq))
        }
    }

    override fun sendPicture(picture: PreparedCallChatPicture, text: String?) {
        launchTask {
            val pictureSeq = wire.reserve(null)
            outgoing[pictureSeq] = Outgoing(picture)
            model.addOwnPicture(pictureSeq, picture.size, picture.jpeg)
            // The text written with it is numbered and shown now, and goes
            // out once the picture has, so the two arrive in that order.
            val textSeq = text?.let { t -> wire.reserve(t).also { model.addOwnPending(it, t) } }
            inFlight += pictureSeq
            val sent = try {
                deliver(pictureSeq)
            } finally {
                inFlight -= pictureSeq
            }
            if (!ended) model.setOwnDelivery(pictureSeq, sent)
            if (textSeq != null && !ended) model.setOwnDelivery(textSeq, wire.transmit(textSeq))
        }
    }

    override fun resend(seq: ULong) {
        if (seq in inFlight) return
        launchTask {
            if (outgoing[seq] != null) {
                inFlight += seq
                model.setOwnSending(seq)
                val sent = try {
                    deliver(seq)
                } finally {
                    inFlight -= seq
                }
                if (!ended) model.setOwnDelivery(seq, sent)
            } else {
                model.setOwnDelivery(seq, wire.transmit(seq))
            }
        }
    }

    /** Upload picture [seq] when it is not uploaded yet, then send its
     *  message. */
    private suspend fun deliver(seq: ULong): Boolean {
        val message = outgoing[seq] ?: return false
        if (!message.uploaded) {
            val encrypted = message.encrypted ?: deps.encrypt(message.picture.jpeg)?.also {
                message.encrypted = it
                uploads.add(it.blobId)
            } ?: return false
            if (ended) return false
            val stored = try {
                deps.put(encrypted)
            } finally {
                // The call ended while it was uploading: it was in the list
                // the end deleted, possibly before the store had it.
                if (ended) deps.deleteUploaded(listOf(encrypted.blobId))
            }
            if (!stored || ended) return false
            message.uploaded = true
            message.encrypted = null
            wire.attachPicture(
                seq,
                CallChatPictureRef(
                    blobId = encrypted.blobId,
                    key = encrypted.key,
                    nonce = encrypted.nonce,
                    size = message.picture.jpeg.size.toUInt(),
                    width = message.picture.width.toUShort(),
                    height = message.picture.height.toUShort(),
                ),
            )
        }
        return wire.transmit(seq)
    }

    /** A verified picture message arrived: shown at once, downloaded now. */
    public fun onReceivedPicture(peerId: String, seq: ULong, picture: CallChatPictureRef, senderName: String) {
        if (ended) return
        val id = model.addReceivedPicture(peerId, seq, picture.pixelSize, senderName) ?: return
        launchTask {
            val bytes = deps.download(picture)
            if (!ended) {
                model.setPicture(id, if (bytes != null) CallChatPictureState.Ready(bytes) else CallChatPictureState.Unavailable)
            }
        }
    }

    /**
     * The call ended for this device: every picture it uploaded in the call
     * is deleted, what is still running stops, and nothing more is shown.
     * Idempotent.
     */
    public fun endCall() {
        if (ended) return
        ended = true
        val ids = uploads.takeAll()
        if (ids.isNotEmpty()) deps.deleteUploaded(ids)
        jobs.toList().forEach { it.cancel() }
        jobs.clear()
        outgoing.clear()
        inFlight.clear()
    }

    private fun launchTask(block: suspend () -> Unit) {
        if (ended) return
        // Kept until the end of the call, which cancels what still runs.
        jobs += scope.launch { block() }
    }
}
