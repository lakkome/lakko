// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.l10n

import java.io.File
import java.io.IOException

/**
 * ADR-0061 section 5 (S6) — the pure decisions behind signed translation
 * updates, kept free of the FFI and of Android so a host JVM test can drive
 * them. The acceptance rules themselves (signature, version above the
 * built-in catalog and the last accepted bundle, `min_release`, per-string
 * fit, never a security-critical key, size bound) live in the Rust core
 * (`l10nAcceptBundle`); this file decides only what to do with its verdict.
 *
 * Every failure falls toward the built-in catalog, silently (UX section 7).
 */

/** A bundle the core accepted. [bundle] is the opaque FFI handle. */
public data class AcceptedBundle<B>(
    val bundle: B,
    val version: ULong,
    val signedAtMillis: Long,
)

/**
 * The core's acceptance check, as a function: `null` when the core refused
 * the bytes (any reason). [lastAccepted] `null` means "no rollback floor".
 */
public fun interface BundleVerifier<B> {
    public fun accept(bytes: ByteArray, lastAccepted: ULong?): AcceptedBundle<B>?
}

/** What is persisted next to the raw bytes. */
public data class StoredBundleMeta(val version: ULong, val signedAtMillis: Long)

/** A persisted bundle: the raw signed bytes and what the core said of them. */
public class StoredBundle(public val bytes: ByteArray, public val meta: StoredBundleMeta)

/** Where an accepted bundle lives between launches. */
public interface TranslationBundleStorage {
    /** The stored bundle, or `null` when none (or unreadable). */
    public fun read(): StoredBundle?

    /** Replace the stored bundle; throws on a write failure. */
    public fun write(bundle: StoredBundle)

    public fun delete()
}

/** At launch: what to do with the stored bundle. */
public sealed interface LaunchOutcome<out B> {
    /** Nothing stored: built-in catalog only. */
    public data object None : LaunchOutcome<Nothing>

    /** The stored bundle still verifies: use it. */
    public data class Use<B>(val accepted: AcceptedBundle<B>) : LaunchOutcome<B>

    /** The stored bundle no longer verifies (or disagrees with its meta): delete it. */
    public data object Discard : LaunchOutcome<Nothing>
}

/**
 * Re-check a stored bundle at launch. It is re-verified with no rollback
 * floor (it is the floor), and used only when the core accepts it AND its
 * version equals the one recorded when it was stored. An app update that
 * raised the built-in version or the release past the bundle makes the core
 * refuse it, and it is discarded.
 */
public fun <B> decideOnLaunch(stored: StoredBundle?, verifier: BundleVerifier<B>): LaunchOutcome<B> {
    if (stored == null) return LaunchOutcome.None
    val accepted = verifier.accept(stored.bytes, null) ?: return LaunchOutcome.Discard
    if (accepted.version != stored.meta.version) return LaunchOutcome.Discard
    return LaunchOutcome.Use(accepted)
}

/** After a download: apply the new bundle, or keep what is in effect. */
public sealed interface DownloadOutcome<out B> {
    public data class Apply<B>(val accepted: AcceptedBundle<B>) : DownloadOutcome<B>

    public data object Keep : DownloadOutcome<Nothing>
}

/**
 * Judge freshly downloaded bytes with the stored version as the rollback
 * floor. Empty or oversized bodies are kept away from the parser.
 */
public fun <B> decideOnDownload(
    bytes: ByteArray,
    storedVersion: ULong?,
    maxBytes: Int,
    verifier: BundleVerifier<B>,
): DownloadOutcome<B> {
    if (bytes.isEmpty() || bytes.size > maxBytes) return DownloadOutcome.Keep
    val accepted = verifier.accept(bytes, storedVersion) ?: return DownloadOutcome.Keep
    return DownloadOutcome.Apply(accepted)
}

/**
 * The fetch cadence, the release notice's (ADR-0038): at most once per
 * [throttleMillis]; never-checked is due; a clock that went backwards is due
 * (rather than silent forever).
 */
public fun isTranslationFetchDue(lastCheckAtMillis: Long?, nowMillis: Long, throttleMillis: Long): Boolean {
    if (lastCheckAtMillis == null) return true
    val elapsed = nowMillis - lastCheckAtMillis
    return elapsed < 0 || elapsed >= throttleMillis
}

/** The meta file format: `v1`, the version, the signing time, one per line. */
internal object StoredBundleMetaCodec {
    private const val TAG = "v1"

    fun encode(meta: StoredBundleMeta): String = "$TAG\n${meta.version}\n${meta.signedAtMillis}\n"

    fun decode(text: String): StoredBundleMeta? {
        val lines = text.trimEnd('\n').split('\n')
        if (lines.size != 3 || lines[0] != TAG) return null
        val version = lines[1].toULongOrNull() ?: return null
        val signedAt = lines[2].toLongOrNull() ?: return null
        return StoredBundleMeta(version, signedAt)
    }
}

/**
 * File storage under [dir] (the app's `noBackupFilesDir`, so a bundle never
 * rides a device backup). The bytes are written before the meta, each through
 * a temporary file and a rename; a bundle whose meta is missing or malformed
 * reads as absent, and the launch check deletes whatever disagrees.
 */
public class FileTranslationBundleStorage(private val dir: File) : TranslationBundleStorage {
    private val bytesFile get() = File(dir, BYTES_NAME)
    private val metaFile get() = File(dir, META_NAME)

    override fun read(): StoredBundle? = try {
        if (!bytesFile.isFile || !metaFile.isFile) {
            null
        } else {
            val meta = StoredBundleMetaCodec.decode(metaFile.readText())
            if (meta == null) null else StoredBundle(bytesFile.readBytes(), meta)
        }
    } catch (_: IOException) {
        null
    }

    override fun write(bundle: StoredBundle) {
        dir.mkdirs()
        // Remove the meta first so a crash between the two renames leaves a
        // bundle that reads as absent, never new bytes under an old version.
        metaFile.delete()
        replace(bytesFile) { it.writeBytes(bundle.bytes) }
        replace(metaFile) { it.writeText(StoredBundleMetaCodec.encode(bundle.meta)) }
    }

    override fun delete() {
        metaFile.delete()
        bytesFile.delete()
    }

    private fun replace(target: File, write: (File) -> Unit) {
        val tmp = File(dir, target.name + ".tmp")
        write(tmp)
        if (!tmp.renameTo(target)) {
            tmp.delete()
            throw IOException("rename failed for ${target.name}")
        }
    }

    private companion object {
        const val BYTES_NAME = "translation-bundle.bin"
        const val META_NAME = "translation-bundle.meta"
    }
}
