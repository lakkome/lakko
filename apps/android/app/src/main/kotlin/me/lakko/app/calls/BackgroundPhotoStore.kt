// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.util.Log
import java.io.ByteArrayOutputStream
import java.io.File
import java.security.SecureRandom
import me.lakko.app.account.AccountStoreGate
import me.lakko.app.avatar.AvatarSeal
import me.lakko.app.avatar.KeystoreAvatarSeal

/**
 * The person's own background photos (`docs/ux/call-backgrounds.md` section
 * 2.3, ADR-0089 section 5): at most [MAX_PHOTOS], longer side at most
 * [MAX_SIDE] pixels, on this device only.
 *
 * A photo of someone's home is personal data, so each is kept the way this
 * client keeps other per-account files: sealed under the Keystore wrap key
 * ([AvatarSeal]), in the no-backup directory, in a folder of the account,
 * written only through the account store gate (ADR-0077) and removed by
 * the account wipe ([deleteAccount], a step of
 * [me.lakko.app.account.AccountLocalWipe]).
 *
 * A photo's id carries the moment it was added (`t<millis base 36>_<hex>`),
 * which is what orders the strip newest first; the id is what the stored
 * choice names (`photo:<id>`), so it satisfies the core's photo-id rule
 * (`[A-Za-z0-9_-]`, 1 to 64).
 */
internal class BackgroundPhotoStore(
    private val root: File,
    private val seal: AvatarSeal,
    private val now: () -> Long = System::currentTimeMillis,
    private val random: () -> String = { randomHex() },
) {
    /** The ids of [accountIdB64]'s photos, newest first. */
    fun list(accountIdB64: String): List<String> {
        val dir = dirOf(accountIdB64)
        val names = dir.list() ?: return emptyList()
        return names
            .filter { it.endsWith(SUFFIX) }
            .map { it.removeSuffix(SUFFIX) }
            .filter(::isOwnId)
            .sortedWith(compareByDescending<String> { addedAt(it) }.thenByDescending { it })
    }

    /** Whether photo [id] of [accountIdB64] is on disk. */
    fun exists(accountIdB64: String, id: String): Boolean =
        isOwnId(id) && File(dirOf(accountIdB64), id + SUFFIX).isFile

    /**
     * Adds an encoded photo ([jpeg], already at most [MAX_SIDE]) and returns
     * its id, or `null` when it could not be written (the gate refused, the
     * seal failed). With [MAX_PHOTOS] already kept, the oldest one that is
     * not [selectedId] is removed first.
     */
    fun add(accountIdB64: String, jpeg: ByteArray, selectedId: String?): String? {
        val sealed = seal.seal(jpeg) ?: return null
        return AccountStoreGate.write(accountIdB64) {
            val dir = dirOf(accountIdB64)
            if (!dir.isDirectory && !dir.mkdirs()) return@write null
            val held = list(accountIdB64)
            if (held.size >= MAX_PHOTOS) {
                held.asReversed()
                    .filter { it != selectedId }
                    .take(held.size - MAX_PHOTOS + 1)
                    .forEach { File(dir, it + SUFFIX).delete() }
            }
            val id = "t" + java.lang.Long.toString(now(), 36) + "_" + random()
            val file = File(dir, id + SUFFIX)
            val tmp = File(dir, "$id.tmp")
            try {
                tmp.writeBytes(sealed)
                if (!tmp.renameTo(file)) {
                    tmp.delete()
                    return@write null
                }
            } catch (cause: Throwable) {
                tmp.delete()
                Log.w(TAG, "a background photo could not be written: ${cause.javaClass.simpleName}")
                return@write null
            }
            id
        }
    }

    /** Removes photo [id]. */
    fun remove(accountIdB64: String, id: String) {
        if (!isOwnId(id)) return
        AccountStoreGate.write(accountIdB64) {
            File(dirOf(accountIdB64), id + SUFFIX).delete()
        }
    }

    /** The encoded photo [id], opened, or `null` when it is missing or
     *  cannot be opened. */
    fun read(accountIdB64: String, id: String): ByteArray? {
        if (!isOwnId(id)) return null
        val file = File(dirOf(accountIdB64), id + SUFFIX)
        if (!file.isFile) return null
        return try {
            seal.open(file.readBytes())
        } catch (_: Throwable) {
            null
        }
    }

    /** Removes every photo of [accountIdB64] (the account wipe). */
    fun deleteAccount(accountIdB64: String) {
        dirOf(accountIdB64).deleteRecursively()
    }

    private fun dirOf(accountIdB64: String): File = File(root, accountDirName(accountIdB64))

    companion object {
        private const val TAG = "BackgroundPhotos"
        const val DIR = "lakko-call-backgrounds"
        private const val SUFFIX = ".bin"

        /** How many own photos are kept (UX section 2.3). */
        const val MAX_PHOTOS = 12

        /** The longer side of a kept photo, in pixels (UX section 2.3). */
        const val MAX_SIDE = 1920

        private val ID = Regex("t[0-9a-z]{1,13}_[0-9a-f]{1,32}")

        /** Whether [id] is one this store made. */
        fun isOwnId(id: String): Boolean = ID.matches(id)

        /** When photo [id] was added, in milliseconds; 0 when unreadable. */
        fun addedAt(id: String): Long =
            id.removePrefix("t").substringBefore('_').toLongOrNull(36) ?: 0L

        /** The folder name of an account: its id in URL-safe base64. */
        fun accountDirName(accountIdB64: String): String =
            accountIdB64.replace('+', '-').replace('/', '_').trimEnd('=')

        private fun randomHex(): String {
            val bytes = ByteArray(8)
            SecureRandom().nextBytes(bytes)
            return bytes.joinToString("") { "%02x".format(it) }
        }

        /** The store on this device. */
        fun onDevice(context: Context): BackgroundPhotoStore = BackgroundPhotoStore(
            root = File(context.applicationContext.noBackupFilesDir, DIR),
            seal = KeystoreAvatarSeal,
        )

        /**
         * Decodes a picked picture, turned upright and scaled so its longer
         * side is at most [MAX_SIDE], and encodes it as JPEG; `null` when it
         * is not a picture this device can read (UX: "This photo can't be
         * used").
         */
        fun encodePicked(file: File): ByteArray? {
            val decoded = me.lakko.app.media.decodeOrientedBitmap(file, MAX_SIDE) ?: return null
            return try {
                val scaled = scaleToMaxSide(decoded, MAX_SIDE)
                val out = ByteArrayOutputStream()
                if (!scaled.compress(Bitmap.CompressFormat.JPEG, JPEG_QUALITY, out)) null else out.toByteArray()
            } catch (cause: Throwable) {
                Log.w(TAG, "a picked photo could not be encoded: ${cause.javaClass.simpleName}")
                null
            }
        }

        /** An encoded photo decoded for drawing; `null` when unreadable. */
        fun decode(jpeg: ByteArray): Bitmap? = try {
            BitmapFactory.decodeByteArray(jpeg, 0, jpeg.size)
        } catch (_: Throwable) {
            null
        }

        /** A small copy of an encoded photo for the strip. */
        fun decodeThumbnail(jpeg: ByteArray, side: Int): Bitmap? = try {
            val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
            BitmapFactory.decodeByteArray(jpeg, 0, jpeg.size, bounds)
            var sample = 1
            while (minOf(bounds.outWidth, bounds.outHeight) / (sample * 2) >= side) sample *= 2
            BitmapFactory.decodeByteArray(jpeg, 0, jpeg.size, BitmapFactory.Options().apply { inSampleSize = sample })
        } catch (_: Throwable) {
            null
        }

        private const val JPEG_QUALITY = 90

        private fun scaleToMaxSide(bitmap: Bitmap, maxSide: Int): Bitmap {
            val longer = maxOf(bitmap.width, bitmap.height)
            if (longer <= maxSide) return bitmap
            val scale = maxSide.toFloat() / longer
            return Bitmap.createScaledBitmap(
                bitmap,
                (bitmap.width * scale).toInt().coerceAtLeast(1),
                (bitmap.height * scale).toInt().coerceAtLeast(1),
                true,
            )
        }
    }
}

/**
 * The account wipe's step for the camera background (ADR-0089 section 5):
 * every own photo of [accountIdB64] removed, and a stored choice that
 * names one of them set back to no background, so the next account on
 * this device does not inherit a choice of a photo it cannot see.
 */
internal fun wipeBackgroundPhotos(context: Context, accountIdB64: String) {
    CameraBackgroundSetting.attach(context)
    wipeBackgroundPhotosIn(BackgroundPhotoStore.onDevice(context), CameraBackgroundSetting.holder, accountIdB64)
}

/** [wipeBackgroundPhotos] over explicit stores (the host tests). */
internal fun wipeBackgroundPhotosIn(
    store: BackgroundPhotoStore,
    holder: BackgroundChoiceHolder,
    accountIdB64: String,
) {
    val chosen = holder.current()
    val namesOwnPhoto = chosen is BackgroundSelection.Photo && chosen.id in store.list(accountIdB64)
    store.deleteAccount(accountIdB64)
    if (namesOwnPhoto) holder.select(BackgroundSelection.None)
}
