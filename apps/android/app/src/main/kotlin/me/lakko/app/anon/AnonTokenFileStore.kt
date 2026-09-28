// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.anon

import java.io.File
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/**
 * ADR-0054 S5 - the token pool on disk: one JSON file under the app's
 * `noBackupFilesDir`, so it survives a relaunch but never reaches a cloud
 * backup or another device. App-private storage, never logged.
 *
 * Written through a sibling temp file and a rename, so a crash mid-write
 * leaves the previous pool (or none), never a torn one. An unreadable file is
 * deleted and read as empty: the pool then simply refills.
 */
public class FileAnonTokenStore(private val file: File) : AnonTokenStore {

    private val json = Json { ignoreUnknownKeys = true }

    override fun load(): PersistedAnonPool? {
        if (!file.exists()) return null
        val decoded = try {
            json.decodeFromString(Wire.serializer(), file.readText(Charsets.UTF_8))
        } catch (_: Exception) {
            file.delete()
            return null
        }
        if (decoded.v != FORMAT_VERSION) {
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
        file.parentFile?.mkdirs()
        val tmp = File(file.parentFile, file.name + ".tmp")
        tmp.writeText(json.encodeToString(Wire.serializer(), wire), Charsets.UTF_8)
        if (!tmp.renameTo(file)) {
            file.delete()
            if (!tmp.renameTo(file)) {
                tmp.delete()
                throw java.io.IOException("could not replace the token pool file")
            }
        }
    }

    override fun clear() {
        file.delete()
        File(file.parentFile, file.name + ".tmp").delete()
    }

    @Serializable
    private data class Wire(val v: Int, val owner: String, val tokens: List<WireToken>)

    @Serializable
    private data class WireToken(val c: String, val e: Long, val h: String)

    public companion object {
        private const val FORMAT_VERSION: Int = 1

        /** The file under an app's `noBackupFilesDir`. */
        public fun under(noBackupFilesDir: File): FileAnonTokenStore =
            FileAnonTokenStore(File(File(noBackupFilesDir, "lakko-anon-tokens"), "pool.json"))
    }
}
