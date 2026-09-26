// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import me.lakko.app.chat.AttachmentMediaMeta
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.lakko_ffi_uniffi.AttachmentRef
import uniffi.lakko_ffi_uniffi.ThumbRef

/**
 * ADR-0047 — outgoing voice messages, video notes and GIFs.
 *
 * A media message is sent as its OWN message the moment it is finished (a
 * released hold, Send from preview, a tapped GIF). Its bubble enters the
 * timeline at once with a determinate upload ring, and a failure leaves it
 * there as `Not sent · Retry` rather than rolling it back (UX spec §2 / §5
 * states). Nothing reaches the other side until [publish] succeeds.
 *
 * A job keeps whatever it already achieved — the bytes, the uploaded poster,
 * the uploaded ref — so a retry RESUMES: a publish that failed after the upload
 * landed does not upload the file again.
 *
 * The three effects are injected so this is host-testable: production passes
 * the attachment bridge (encrypt + upload) and the repository's send.
 */
public class MediaSendQueue(
    private val scope: CoroutineScope,
    private val uploadThumb: suspend (PosterFrame) -> ThumbRef,
    private val uploadMain: suspend (
        bytes: ByteArray,
        mime: String,
        name: String,
        meta: AttachmentMediaMeta,
        onProgress: (Float) -> Unit,
    ) -> AttachmentRef,
    /** Publishes the uploaded ref as its own message, carrying the reply the
     *  job was armed with (ADR-0052), or `null` for an ordinary send. */
    private val publish: suspend (AttachmentRef, me.lakko.app.chat.ReplyRef?) -> Unit,
    private val onLog: (String) -> Unit = {},
) {
    public class Job(
        public val tempId: Long,
        public val kind: String,
        public var mime: String,
        public var name: String,
        public val durationMs: Long?,
        public val waveform: ByteArray?,
        public val width: Int?,
        public val height: Int?,
        public val poster: PosterFrame?,
        /** Loads the bytes and their MIME type (a GIF's type is only known once
         *  the proxy answered). `null` is a failure. */
        public val loadBytes: suspend () -> Pair<ByteArray, String>?,
        /** ADR-0052 — the reply armed when this take was handed over. Taken
         *  at enqueue, so the strip can clear at once and a retry still sends
         *  the take as the reply it was recorded as. */
        public val reply: me.lakko.app.chat.ReplyRef? = null,
    ) {
        internal var bytes: ByteArray? = null
        internal var thumb: ThumbRef? = null
        internal var posterTried: Boolean = false
        internal var uploaded: AttachmentRef? = null
    }

    private val jobs = LinkedHashMap<Long, Job>()
    private val mutableRows = MutableStateFlow<List<PendingMediaRow>>(emptyList())
    public val rows: StateFlow<List<PendingMediaRow>> = mutableRows.asStateFlow()

    private var nextId: Long = 1

    public fun newTempId(): Long = nextId++

    public fun enqueue(job: Job) {
        jobs[job.tempId] = job
        mutableRows.update { it + rowFor(job, progress = 0f, failed = false) }
        run(job)
    }

    /** Resume a FAILED job. A job still in flight is left alone, so a double
     *  tap can never run two uploads of one message. */
    public fun retry(tempId: Long) {
        val job = jobs[tempId] ?: return
        if (mutableRows.value.firstOrNull { it.tempId == tempId }?.failed != true) return
        run(job)
    }

    /** Drop a failed row; nothing was sent, so there is nothing to undo. */
    public fun discard(tempId: Long) {
        jobs.remove(tempId)
        mutableRows.update { rows -> rows.filterNot { it.tempId == tempId } }
    }

    private fun run(job: Job) {
        update(job) { it.copy(progress = 0.02f, failed = false) }
        scope.launch {
            try {
                if (job.uploaded == null) {
                    if (job.bytes == null) {
                        val loaded = job.loadBytes() ?: throw MediaSendFailed("no bytes")
                        job.bytes = loaded.first
                        job.mime = loaded.second
                        if (job.kind == KIND_GIF) job.name = gifNameFor(job.name, loaded.second)
                        update(job) { it.copy(mime = job.mime, previewBytes = if (job.kind == KIND_GIF) loaded.first else it.previewBytes) }
                    }
                    val poster = job.poster
                    if (poster != null && job.thumb == null && !job.posterTried) {
                        job.posterTried = true
                        try {
                            job.thumb = uploadThumb(poster)
                        } catch (e: CancellationException) {
                            throw e
                        } catch (e: Throwable) {
                            // ADR-0047 open question, current answer: a failed
                            // poster upload sends the note without one; the
                            // bubble shows a neutral circle.
                            onLog("video note poster upload failed; sending without a poster")
                        }
                    }
                    val bytes = job.bytes ?: throw MediaSendFailed("no bytes")
                    job.uploaded = uploadMain(bytes, job.mime, job.name, metaFor(job)) { p ->
                        update(job) { it.copy(progress = 0.05f + 0.85f * p.coerceIn(0f, 1f)) }
                    }
                }
                update(job) { it.copy(progress = 0.95f) }
                publish(job.uploaded ?: throw MediaSendFailed("no ref"), job.reply)
                jobs.remove(job.tempId)
                mutableRows.update { rows -> rows.filterNot { it.tempId == job.tempId } }
            } catch (e: CancellationException) {
                throw e
            } catch (e: Throwable) {
                onLog("media send failed: ${e.javaClass.simpleName}")
                update(job) { it.copy(failed = true) }
            }
        }
    }

    private fun metaFor(job: Job): AttachmentMediaMeta = AttachmentMediaMeta(
        kind = job.kind,
        durationMs = job.durationMs?.takeIf { it > 0 }?.coerceAtMost(UInt.MAX_VALUE.toLong())?.toUInt(),
        waveform = job.waveform,
        width = job.width?.takeIf { it in 1..MAX_GIF_DIMENSION }?.toUInt(),
        height = job.height?.takeIf { it in 1..MAX_GIF_DIMENSION }?.toUInt(),
        thumb = job.thumb,
    )

    private fun update(job: Job, change: (PendingMediaRow) -> PendingMediaRow) {
        mutableRows.update { rows -> rows.map { if (it.tempId == job.tempId) change(it) else it } }
    }

    private fun rowFor(job: Job, progress: Float, failed: Boolean) = PendingMediaRow(
        tempId = job.tempId,
        kind = job.kind,
        mime = job.mime,
        name = job.name,
        durationMs = job.durationMs,
        waveform = job.waveform,
        width = job.width,
        height = job.height,
        posterBytes = job.poster?.bytes,
        previewBytes = null,
        progress = progress,
        failed = failed,
    )

    private class MediaSendFailed(message: String) : RuntimeException(message)

    public companion object {
        /** A picked GIF's container is only known once the proxy answered; the
         *  file name follows it so an old client's Save gets the right
         *  extension. */
        public fun gifNameFor(name: String, mime: String): String {
            val base = name.substringBeforeLast('.', name)
            val ext = when (mime) {
                "image/webp" -> "webp"
                "video/mp4" -> "mp4"
                else -> "gif"
            }
            return "$base.$ext"
        }
    }
}

/** What the timeline draws for an outgoing media message still in flight. */
public data class PendingMediaRow(
    val tempId: Long,
    val kind: String,
    val mime: String,
    val name: String,
    val durationMs: Long?,
    val waveform: ByteArray?,
    val width: Int?,
    val height: Int?,
    val posterBytes: ByteArray?,
    val previewBytes: ByteArray?,
    val progress: Float,
    val failed: Boolean,
)
