// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import me.lakko.app.chat.AttachmentBridge
import java.io.File
import java.util.UUID

/**
 * Decrypted-media files for ONE open chat screen.
 *
 * `MediaPlayer` and `ImageDecoder` want a file (or a descriptor), so a voice
 * message, video note or GIF is written to disk after it is downloaded and
 * AEAD-opened. That plaintext must not outlive the chat that opened it — the
 * Android analogue of the web client's object-URL registry — so every file
 * lives under a per-session directory this class owns:
 *
 * ```text
 *   cacheDir/chat-media/<session uuid>/<entry key>/<sanitized name>
 * ```
 *
 *   * The name is the SENDER-chosen attachment name, so it goes through
 *     [AttachmentBridge.safeFileName], sits in its own per-entry directory (so
 *     two messages both named `voice.m4a` cannot collide), and is refused if it
 *     still resolves outside that directory.
 *   * [dispose] (chat closed) deletes the whole session directory and refuses
 *     later writes: a download that resolves after the screen left deletes its
 *     own file at once, so an in-flight fetch cannot leak plaintext past the
 *     view that asked for it.
 *   * [sweepStale] removes session directories left behind by a process that
 *     died before its screen could dispose, called when a chat opens.
 *
 * Nothing Android-specific is used, so the bookkeeping is host-testable.
 */
public class ChatMediaCache(root: File) {

    private val sessionDir: File = File(File(root, ROOT_DIR), UUID.randomUUID().toString())
    private val files = LinkedHashMap<String, File>()
    private var disposed = false

    @Synchronized
    public fun get(key: String): File? = files[key]?.takeIf { it.exists() }

    @get:Synchronized
    public val size: Int get() = files.size

    @get:Synchronized
    public val isDisposed: Boolean get() = disposed

    /**
     * Write [bytes] under [key] with a sanitized form of [name] and return the
     * file, or `null` when the cache was already disposed (the bytes are then
     * not kept) or the write failed. Replacing an existing [key] deletes the
     * file it replaces.
     */
    @Synchronized
    public fun put(key: String, name: String?, fallbackName: String, bytes: ByteArray): File? {
        if (disposed) return null
        val entryDir = File(sessionDir, entryDirName(key))
        val safe = AttachmentBridge.safeFileName(name, fallbackName)
        return try {
            entryDir.mkdirs()
            val out = File(entryDir, safe)
            if (out.canonicalFile.parentFile != entryDir.canonicalFile) return null
            out.writeBytes(bytes)
            val previous = files.put(key, out)
            if (previous != null && previous != out) previous.delete()
            out
        } catch (_: Throwable) {
            null
        }
    }

    /** Register a file the caller already wrote inside [newEntryFile]'s
     *  directory (a finished recording). Same dispose rules as [put]. */
    @Synchronized
    public fun adopt(key: String, file: File): File? {
        if (disposed) {
            file.delete()
            return null
        }
        val previous = files.put(key, file)
        if (previous != null && previous != file) previous.delete()
        return file
    }

    /** A fresh path under this session for the caller to write into (a
     *  recording in progress). Not registered until [adopt]; deleted by
     *  [dispose] either way, because it is inside the session directory. */
    @Synchronized
    public fun newEntryFile(key: String, name: String): File? {
        if (disposed) return null
        val entryDir = File(sessionDir, entryDirName(key))
        entryDir.mkdirs()
        val out = File(entryDir, AttachmentBridge.safeFileName(name, "take"))
        return out.takeIf { it.canonicalFile.parentFile == entryDir.canonicalFile }
    }

    @Synchronized
    public fun remove(key: String) {
        val file = files.remove(key) ?: return
        file.delete()
        file.parentFile?.delete()
    }

    /** Delete every file this session wrote and refuse later writes.
     *  Idempotent. */
    @Synchronized
    public fun dispose() {
        disposed = true
        files.clear()
        sessionDir.deleteRecursively()
    }

    public companion object {
        public const val ROOT_DIR: String = "chat-media"

        /** A directory name for a cache key: keys are hex blob ids or local
         *  take ids, but anything outside `[A-Za-z0-9_-]` is replaced so a key
         *  can never become a path. */
        internal fun entryDirName(key: String): String {
            val cleaned = key.map { c ->
                if (c.isLetterOrDigit() && c.code < 128 || c == '-' || c == '_') c else '_'
            }.joinToString("").take(96)
            return cleaned.ifEmpty { "entry" }
        }

        /** Remove session directories under [root] other than [keep]'s own. */
        public fun sweepStale(root: File, keep: ChatMediaCache? = null) {
            val base = File(root, ROOT_DIR)
            val children = base.listFiles() ?: return
            for (child in children) {
                if (keep != null && child == keep.sessionDir) continue
                child.deleteRecursively()
            }
        }
    }
}

/**
 * UX spec §8 — the local-only "played" marker for voice messages and video
 * notes, never sent (it is not a read receipt). Bounded so a long-lived device
 * does not grow the set forever: the oldest marks fall off first, which at
 * worst shows an old message's unplayed dot again.
 */
public class PlayedMediaSet(private val max: Int = MAX_PLAYED_MARKS) {
    private val keys = LinkedHashSet<String>()

    @Synchronized
    public fun isPlayed(key: String): Boolean = key in keys

    /** Returns `true` when [key] was newly marked. */
    @Synchronized
    public fun markPlayed(key: String): Boolean {
        if (!keys.add(key)) return false
        while (keys.size > max) keys.remove(keys.first())
        return true
    }

    @Synchronized
    public fun snapshot(): List<String> = keys.toList()

    /** One key per line; unknown or malformed lines are skipped. */
    @Synchronized
    public fun encode(): String = keys.joinToString("\n")

    @Synchronized
    public fun seed(stored: String?) {
        if (stored == null) return
        for (line in stored.split('\n')) {
            val k = line.trim()
            if (k.isNotEmpty() && k.length <= 128 && k.all { it.isLetterOrDigit() || it == ':' }) {
                markPlayed(k)
            }
        }
    }

    public companion object {
        public const val MAX_PLAYED_MARKS: Int = 2000
    }
}
