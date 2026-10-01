// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.anon

import java.io.File
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/**
 * ADR-0054 S5 - the token pools on disk: one JSON file per account under the
 * app's `noBackupFilesDir` (`lakko-anon-tokens/<account>.json`, the account id
 * as URL-safe base64 without padding), so a pool survives a relaunch and a Log
 * out but never reaches a cloud backup or another device. App-private storage,
 * never logged.
 *
 * Tokens are account data (maintainer decision, 2026-10-01): one account's
 * file is never read for another, and [delete] removes one account's file
 * only. Each file also records its owner; a file whose owner is not the
 * account asked for is treated as unreadable.
 *
 * Written through a sibling temp file and a rename, so a crash mid-write
 * leaves the previous pool (or none), never a torn one. An unreadable file is
 * deleted and read as empty: the pool then simply refills.
 */
public class FileAnonTokenStore(private val dir: File) : AnonTokenStore {

    private val json = Json { ignoreUnknownKeys = true }

    /** The file of [accountKey]'s pool. Visible for tests. */
    internal fun fileFor(accountKey: String): File = File(dir, fileName(accountKey))

    override fun load(accountKey: String): PersistedAnonPool? {
        val file = fileFor(accountKey)
        if (!file.exists()) return null
        val decoded = try {
            json.decodeFromString(Wire.serializer(), file.readText(Charsets.UTF_8))
        } catch (_: Exception) {
            file.delete()
            return null
        }
        if (decoded.v != FORMAT_VERSION || decoded.owner != accountKey) {
            file.delete()
            return null
        }
        val byWire = AnonTokenContext.entries.associateBy { it.wire }
        val tokens = decoded.tokens.mapNotNull { t ->
            byWire[t.c]?.let { PooledAnonToken(context = it, epoch = t.e, headerValue = t.h) }
        }
        return PersistedAnonPool(owner = decoded.owner, tokens = tokens)
    }

    override fun save(pool: PersistedAnonPool) {
        val wire = Wire(
            v = FORMAT_VERSION,
            owner = pool.owner,
            tokens = pool.tokens.map { WireToken(c = it.context.wire, e = it.epoch, h = it.headerValue) },
        )
        val file = fileFor(pool.owner)
        dir.mkdirs()
        val tmp = File(dir, file.name + ".tmp")
        tmp.writeText(json.encodeToString(Wire.serializer(), wire), Charsets.UTF_8)
        if (!tmp.renameTo(file)) {
            file.delete()
            if (!tmp.renameTo(file)) {
                tmp.delete()
                throw java.io.IOException("could not replace the token pool file")
            }
        }
    }

    override fun delete(accountKey: String) {
        val file = fileFor(accountKey)
        file.delete()
        File(dir, file.name + ".tmp").delete()
    }

    @Serializable
    private data class Wire(val v: Int, val owner: String, val tokens: List<WireToken>)

    @Serializable
    private data class WireToken(val c: String, val e: Long, val h: String)

    public companion object {
        private const val FORMAT_VERSION: Int = 1

        private const val DIR_NAME: String = "lakko-anon-tokens"

        /** The one device-wide file of builds before 2026-10-01, never read. */
        private const val RETIRED_FILE: String = "pool.json"

        /**
         * The store under an app's `noBackupFilesDir`. Removes the retired
         * device-wide `pool.json` (and its temp file) on the way: it held the
         * tokens of whichever account filled it last and is never read.
         */
        public fun under(noBackupFilesDir: File): FileAnonTokenStore {
            val dir = File(noBackupFilesDir, DIR_NAME)
            File(dir, RETIRED_FILE).delete()
            File(dir, "$RETIRED_FILE.tmp").delete()
            return FileAnonTokenStore(dir)
        }

        /**
         * `<account>.json`: the account id as URL-safe base64 without padding,
         * so it is one path segment and every standard-base64 spelling of the
         * same id lands on the same file. A key that is not base64 is used
         * with its path characters replaced.
         */
        internal fun fileName(accountKey: String): String {
            val bytes = try {
                java.util.Base64.getDecoder().decode(accountKey)
            } catch (_: IllegalArgumentException) {
                try {
                    java.util.Base64.getUrlDecoder().decode(accountKey)
                } catch (_: IllegalArgumentException) {
                    null
                }
            }
            val stem = if (bytes != null && bytes.isNotEmpty()) {
                java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(bytes)
            } else {
                accountKey.replace('/', '_').replace('+', '-').trimEnd('=').ifEmpty { "_" }
            }
            return "$stem.json"
        }
    }
}
