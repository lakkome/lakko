// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import android.content.Context
import android.net.Uri
import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import me.lakko.app.chat.AttachmentBridge
import me.lakko.app.chat.AttachmentBridgeException
import me.lakko.app.chat.AttachmentMediaMeta
import me.lakko.app.l10n.UiText
import me.lakko.app.media.PickedRead
import java.io.File
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong

/**
 * docs/ux/media-messages.md section 11 — the composer trays of this process,
 * one per (account, conversation). The tray belongs to the conversation's
 * draft on this device: leaving the conversation and coming back keeps it
 * while the app runs, and an upload started in it goes on when the screen
 * goes away. Nothing here is written anywhere that outlives the process but
 * the prepared bytes under [TRAY_DIR] in the cache, which are swept at the
 * first use of each process and deleted when an item is removed or sent.
 *
 * Uploads run one at a time across every tray, so each ring is honest.
 */
public object ComposerTrays {
    private const val TAG = "ComposerTray"
    private const val TRAY_DIR = "composer-tray"

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val uploadLock = Mutex()
    private val nextId = AtomicLong(1)
    private val mutable = MutableStateFlow<Map<String, ComposerTray>>(emptyMap())

    /** Every tray, by [key]. */
    public val trays: StateFlow<Map<String, ComposerTray>> = mutable.asStateFlow()

    /** What an item goes up as, once prepared. */
    private class Prepared(
        val file: File,
        val mime: String?,
        val name: String?,
        val visual: me.lakko.app.media.VisualPrep?,
        val audio: me.lakko.app.media.AudioTags?,
        val idleDays: Int?,
    )

    private val prepared = ConcurrentHashMap<Long, Prepared>()
    private val jobs = ConcurrentHashMap<Long, Job>()
    private val owners = ConcurrentHashMap<Long, String>()

    @Volatile
    private var appContext: Context? = null

    @Volatile
    private var swept = false

    /** The tray key of a conversation of an account. */
    public fun key(accountIdB64: String?, conversationId: String): String = "${accountIdB64.orEmpty()}|$conversationId"

    /** The tray of [key] (empty when none). */
    public fun tray(key: String): ComposerTray = mutable.value[key] ?: ComposerTray()

    private fun bind(context: Context): Context {
        val app = context.applicationContext ?: context
        appContext = app
        if (!swept) {
            swept = true
            // A process that died with items in a tray left their bytes here.
            runCatching { File(app.cacheDir, TRAY_DIR).deleteRecursively() }
        }
        return app
    }

    private fun change(key: String, f: (ComposerTray) -> ComposerTray) {
        mutable.update { all ->
            val next = f(all[key] ?: ComposerTray())
            if (next.items.isEmpty() && next.notice == null) all - key else all + (key to next)
        }
    }

    private fun notice(key: String, text: UiText) {
        change(key) { it.copy(notice = TrayNotice(text, System.currentTimeMillis())) }
    }

    /**
     * Add [uris] to the tray of [key]: a paste, a keyboard insertion, the
     * Photo or Video picker, File, Audio, or the camera. Each gets its place
     * at once; what does not fit the limits is left out with the limit line.
     * The bytes are read right away (a pasted URI's read grant may not
     * outlive the call), then prepared and uploaded.
     */
    public fun add(
        context: Context,
        key: String,
        uris: List<Uri>,
        filesOnly: Boolean,
        mimeHints: List<String?> = emptyList(),
        audio: Boolean = false,
        /** ADR-0087 section 13 — a channel file's idle life; `null` in a chat. */
        idleDays: Int? = null,
    ) {
        if (uris.isEmpty()) return
        val app = bind(context)
        val resolver = app.contentResolver
        val mimes = uris.mapIndexed { i, uri ->
            runCatching { resolver.getType(uri) }.getOrNull()?.takeUnless { it.isEmpty() } ?: mimeHints.getOrNull(i)
        }
        val kinds = mimes.map { trayKindOf(it, filesOnly || audio) }
        val admission = tray(key).admit(kinds)
        admission.noticeKey?.let { notice(key, UiText(it)) }
        val ids = admission.admitted.map { index ->
            val id = nextId.getAndIncrement()
            owners[id] = key
            change(key) { it.copy(items = it.items + TrayItem(id = id, kind = kinds[index], name = null)) }
            index to id
        }
        if (ids.isEmpty()) return
        scope.launch(start = CoroutineStart.UNDISPATCHED) {
            // Read every admitted item first, while any grant still holds.
            val reads = withContext(Dispatchers.IO) {
                ids.map { (index, id) ->
                    id to me.lakko.app.media.readPickedUri(app, uris[index], AttachmentBridge.MAX_ATTACHMENT_BYTES)
                }
            }
            val tags = if (audio) {
                withContext(Dispatchers.IO) { ids.associate { (index, id) -> id to me.lakko.app.media.readAudioTags(app, uris[index]) } }
            } else {
                emptyMap()
            }
            for ((id, read) in reads) {
                val hint = mimes[ids.first { it.second == id }.first]
                val job = scope.launch(start = CoroutineStart.LAZY) { prepareAndUpload(app, key, id, read, hint, tags[id], idleDays) }
                jobs[id] = job
                job.start()
            }
        }
    }

    private suspend fun prepareAndUpload(
        app: Context,
        key: String,
        id: Long,
        read: PickedRead,
        mimeHint: String?,
        audio: me.lakko.app.media.AudioTags?,
        idleDays: Int?,
    ) {
        val ok = read as? PickedRead.Ok
        if (ok == null) {
            val failed = read as PickedRead.Failed
            change(key) { it.without(id) }
            notice(
                key,
                if (failed.error is AttachmentBridgeException.TooLarge) {
                    UiText(TRAY_FILE_TOO_BIG_KEY)
                } else {
                    me.lakko.app.media.attachFailureMessage(failed.name, failed.error)
                },
            )
            forget(id)
            return
        }
        val guess = tray(key).item(id)?.kind ?: return
        val payload = ok.payload
        val mime = payload.mime ?: mimeHint
        val p = withContext(Dispatchers.Default) { prepare(app, id, guess, payload.bytes, mime, payload.name, audio, idleDays) }
        if (p == null || tray(key).item(id) == null) {
            p?.prepared?.file?.delete()
            forget(id)
            return
        }
        prepared[id] = p.prepared
        // A video that goes as a file counts against the file limit.
        if (p.kind == TrayKind.FILE && guess != TrayKind.FILE && tray(key).without(id).fileCount >= TRAY_MAX_FILES) {
            change(key) { it.without(id) }
            notice(key, UiText(TRAY_LIMIT_FILES_KEY))
            forget(id)
            return
        }
        change(key) {
            it.update(id) { item ->
                item.copy(
                    kind = p.kind,
                    name = p.prepared.name,
                    size = p.prepared.file.length(),
                    durationMs = p.durationMs,
                    thumb = p.prepared.visual?.thumb,
                    status = TrayStatus.Uploading(null),
                )
            }
        }
        upload(key, id)
    }

    private class PrepResult(val prepared: Prepared, val kind: TrayKind, val durationMs: Long?)

    /** docs/ux/attachments.md section 3 — what an item goes up as. */
    private fun prepare(
        app: Context,
        id: Long,
        guess: TrayKind,
        bytes: ByteArray,
        mime: String?,
        name: String?,
        audio: me.lakko.app.media.AudioTags?,
        idleDays: Int?,
    ): PrepResult? {
        val dir = File(app.cacheDir, TRAY_DIR).apply { mkdirs() }
        var sendBytes = bytes
        var sendMime = mime
        var sendName = name
        var decodedPhoto = false
        var codec: String? = null
        when (guess) {
            TrayKind.PHOTO -> {
                val essence = mime?.substringBefore(';')?.trim()?.lowercase()
                if (essence == "image/gif") {
                    decodedPhoto = android.graphics.BitmapFactory.Options().let { o ->
                        o.inJustDecodeBounds = true
                        android.graphics.BitmapFactory.decodeByteArray(bytes, 0, bytes.size, o)
                        o.outWidth > 0
                    }
                } else {
                    val max = runCatching { uniffi.lakko_ffi_uniffi.photoMaxLongestSidePx().toInt() }.getOrDefault(2560)
                    val jpeg = me.lakko.app.media.reencodePhoto(bytes, max)
                    if (jpeg != null) {
                        decodedPhoto = true
                        sendBytes = jpeg
                        sendMime = "image/jpeg"
                        sendName = (name?.substringBeforeLast('.')?.takeIf { it.isNotBlank() } ?: "photo") + ".jpg"
                    }
                }
            }
            TrayKind.VIDEO -> codec = me.lakko.app.media.videoTrackMime(app, bytes)
            TrayKind.FILE -> Unit
        }
        val kind = trayKindAfterRead(guess, mime, decodedPhoto, codec)
        if (kind == TrayKind.FILE) {
            sendBytes = bytes
            sendMime = mime ?: "application/octet-stream"
            sendName = name
        }
        val file = File(dir, "item-$id")
        try {
            file.writeBytes(sendBytes)
        } catch (e: java.io.IOException) {
            Log.w(TAG, "tray item could not be stored: ${e.javaClass.simpleName}")
            return null
        }
        val visual = if (kind == TrayKind.FILE) {
            null
        } else {
            runCatching { me.lakko.app.media.prepareVisualMedia(sendBytes, sendMime, app.cacheDir) }.getOrNull()
        }
        val duration = if (kind == TrayKind.VIDEO) videoDurationMs(file) else null
        return PrepResult(Prepared(file, sendMime, sendName, visual, audio, idleDays), kind, duration)
    }

    private fun videoDurationMs(file: File): Long? {
        val r = android.media.MediaMetadataRetriever()
        return try {
            r.setDataSource(file.absolutePath)
            r.extractMetadata(android.media.MediaMetadataRetriever.METADATA_KEY_DURATION)?.toLongOrNull()?.takeIf { it > 0 }
        } catch (_: Throwable) {
            null
        } finally {
            runCatching { r.release() }
        }
    }

    private suspend fun upload(key: String, id: Long) {
        val p = prepared[id] ?: return
        try {
            uploadLock.withLock {
                if (tray(key).item(id) == null) return
                val bytes = withContext(Dispatchers.IO) { p.file.readBytes() }
                val thumbRef = (p.visual?.thumb ?: p.audio?.cover?.bytes)?.let { thumb ->
                    try {
                        val w = p.visual?.thumbSize?.width ?: p.audio?.cover?.width
                        val h = p.visual?.thumbSize?.height ?: p.audio?.cover?.height
                        AttachmentBridge.encryptAndUploadThumb(thumb, "image/jpeg", w?.toUInt(), h?.toUInt())
                    } catch (e: kotlinx.coroutines.CancellationException) {
                        throw e
                    } catch (e: Throwable) {
                        // Section 10 — never block a send on a preview.
                        Log.w(TAG, "thumbnail upload failed: ${e.javaClass.simpleName}")
                        null
                    }
                }
                val meta = when {
                    p.audio != null -> AttachmentMediaMeta(kind = me.lakko.app.media.KIND_AUDIO, durationMs = p.audio.durationMs, thumb = thumbRef)
                    p.visual != null && (p.visual.size != null || thumbRef != null) -> AttachmentMediaMeta(
                        width = p.visual.size?.width?.toUInt(),
                        height = p.visual.size?.height?.toUInt(),
                        thumb = thumbRef,
                    )
                    else -> null
                }
                var ref = AttachmentBridge.encryptAndUpload(
                    bytes = bytes,
                    mime = p.mime,
                    name = p.name,
                    media = meta,
                    onProgress = { f -> change(key) { t -> t.update(id) { it.copy(status = TrayStatus.Uploading(f)) } } },
                    idleDays = p.idleDays,
                )
                if (p.audio != null) {
                    ref = ref.copy(title = p.audio.title ?: p.name?.substringBeforeLast('.'), artist = p.audio.artist)
                }
                if (tray(key).item(id) == null) {
                    // Removed while the upload ran: the file was never sent.
                    appContext?.let { me.lakko.app.storage.BlobLifetime.delete(it, listOfNotNull(ref.blobId, ref.thumb?.blobId)) }
                    return
                }
                change(key) { t -> t.update(id) { it.copy(status = TrayStatus.Ready, ref = ref) } }
            }
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Throwable) {
            Log.w(TAG, "tray upload failed: ${e.javaClass.simpleName}")
            change(key) { t -> t.update(id) { it.copy(status = TrayStatus.Failed) } }
            if (me.lakko.app.media.isUploadLimit(e)) notice(key, UiText("chat.attachment.limit.send"))
        }
    }

    /** Section 11 — upload a failed item again. */
    public fun retry(key: String, id: Long) {
        val item = tray(key).item(id) ?: return
        if (item.status != TrayStatus.Failed) return
        if (prepared[id] == null) {
            remove(key, id)
            return
        }
        change(key) { t -> t.update(id) { it.copy(status = TrayStatus.Uploading(null)) } }
        val job = scope.launch(start = CoroutineStart.LAZY) { upload(key, id) }
        jobs[id] = job
        job.start()
    }

    /** Section 11 — the (x) on an item. An upload never sent is deleted
     *  (ADR-0065 section 6). */
    public fun remove(key: String, id: Long) {
        val item = tray(key).item(id) ?: return
        change(key) { it.without(id) }
        jobs.remove(id)?.cancel()
        item.ref?.let { ref ->
            appContext?.let { me.lakko.app.storage.BlobLifetime.delete(it, listOfNotNull(ref.blobId, ref.thumb?.blobId)) }
        }
        forget(id)
    }

    /** The message went with these items: clear the tray, keep the files. */
    public fun sent(key: String, ids: Collection<Long>) {
        change(key) { t -> t.copy(items = t.items.filterNot { it.id in ids }, notice = null) }
        ids.forEach(::forget)
    }

    /** The prepared bytes of a tray item, for the viewer; `null` while being read. */
    public fun localFile(id: Long): File? = prepared[id]?.file?.takeIf { it.isFile }

    /** Sign-out / forget device: every tray goes, unsent uploads deleted. */
    public fun clearAll() {
        val all = mutable.value
        for ((key, t) in all) t.items.forEach { remove(key, it.id) }
        mutable.value = emptyMap()
    }

    private fun forget(id: Long) {
        jobs.remove(id)
        owners.remove(id)
        prepared.remove(id)?.file?.delete()
    }
}
