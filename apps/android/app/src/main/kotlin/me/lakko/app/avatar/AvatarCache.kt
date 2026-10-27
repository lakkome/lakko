// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.avatar

import java.io.File

/**
 * ADR-0084 §3 / UX spec §7 — the per-device cache of decrypted profile
 * photos and videos, keyed by blob id.
 *
 *   * **On disk, sealed.** Each entry is one file `<blob id hex>.bin` under
 *     [dir] holding the decrypted bytes sealed under the device's avatar key
 *     ([AvatarSeal]); a blob already held is never fetched again.
 *   * **Failures wait a minute.** A download or a decode that failed is
 *     marked; [mayTry] says no for [RETRY_AFTER_MS] after it, so a list that
 *     redraws does not refetch on every frame, and the next display after the
 *     minute tries again (UX §7).
 *   * **Memory.** The last [memoryEntries] opened entries stay in memory, so
 *     scrolling a list does not unseal the same file on every bind.
 *
 * No Android type: the bookkeeping runs on the host JVM.
 */
public class AvatarCache(
    private val dir: File,
    private val seal: AvatarSeal,
    private val now: () -> Long = System::currentTimeMillis,
    private val memoryEntries: Int = 64,
) {
    private val memory = object : LinkedHashMap<String, ByteArray>(16, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, ByteArray>?): Boolean =
            size > memoryEntries
    }
    private val failures = HashMap<String, Long>()

    private fun file(blobIdHex: String): File? {
        if (!isBlobIdHex(blobIdHex)) return null
        return File(dir, "$blobIdHex.bin")
    }

    /** The decrypted bytes held for [blobIdHex], or `null`. */
    @Synchronized
    public fun get(blobIdHex: String): ByteArray? {
        memory[blobIdHex]?.let { return it }
        val f = file(blobIdHex) ?: return null
        if (!f.exists()) return null
        val opened = try {
            seal.open(f.readBytes())
        } catch (_: Throwable) {
            null
        }
        if (opened == null) {
            // Unreadable (key rotated, corrupt): drop it so the next display
            // fetches it again.
            f.delete()
            return null
        }
        memory[blobIdHex] = opened
        return opened
    }

    /** Whether [blobIdHex] is held (memory or disk), without opening it. */
    @Synchronized
    public fun contains(blobIdHex: String): Boolean =
        memory.containsKey(blobIdHex) || file(blobIdHex)?.exists() == true

    /** Keep [bytes] for [blobIdHex]. Returns `false` when it could not be
     *  sealed or written (the bytes are then kept in memory only). */
    @Synchronized
    public fun put(blobIdHex: String, bytes: ByteArray): Boolean {
        val f = file(blobIdHex) ?: return false
        memory[blobIdHex] = bytes
        failures.remove(blobIdHex)
        val sealed = seal.seal(bytes) ?: return false
        return try {
            dir.mkdirs()
            val tmp = File(dir, "$blobIdHex.tmp")
            tmp.writeBytes(sealed)
            tmp.renameTo(f)
        } catch (_: Throwable) {
            false
        }
    }

    /** May a download of [blobIdHex] start now: nothing failed for it in the
     *  last [RETRY_AFTER_MS]. */
    @Synchronized
    public fun mayTry(blobIdHex: String): Boolean {
        val at = failures[blobIdHex] ?: return true
        if (now() - at >= RETRY_AFTER_MS) {
            failures.remove(blobIdHex)
            return true
        }
        return false
    }

    /** A download or decode of [blobIdHex] failed. */
    @Synchronized
    public fun markFailed(blobIdHex: String) {
        failures[blobIdHex] = now()
    }

    /** Forget everything (forget device). */
    @Synchronized
    public fun clear() {
        memory.clear()
        failures.clear()
        dir.deleteRecursively()
    }

    public companion object {
        /** UX §7: a failed photo is tried again on the next display after a
         *  minute. */
        public const val RETRY_AFTER_MS: Long = 60_000L

        /** The cache directory name under `noBackupFilesDir`. */
        public const val DIR: String = "avatars"

        /** The directory the decrypted profile video is written to for
         *  playback (cacheDir); emptied at sign-out and forget device. */
        public const val PLAY_DIR: String = "avatar-play"

        internal fun isBlobIdHex(s: String): Boolean =
            s.length == 64 && s.all { it in '0'..'9' || it in 'a'..'f' }
    }
}
