// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.serialization.json.Json
import java.io.File
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * The feed's two per-account stores on disk (ADR-0061 sections 3 and 6): the
 * channel-post log and the saved feeds. Same posture as
 * [me.lakko.app.chat.MessageLogStore] — the channel-post log holds decrypted
 * post text — so both are sealed under a dedicated Keystore-resident
 * AES-256-GCM key and written under `noBackupFilesDir`, so a cloud-backup
 * restore does not carry them to another device.
 *
 * ```text
 *   noBackupFilesDir/lakko-feed/
 *     channel-posts/<accountIdB64>.bin   12-byte IV | ciphertext | tag
 *     feeds/<accountIdB64>.bin
 * ```
 *
 * Best-effort: a Keystore or file failure reads as empty and writes as
 * `false`; the feed works without either (it just starts empty).
 */
public object FeedSealedStore {
    private const val AES_GCM_NOPAD = "AES/GCM/NoPadding"
    private const val GCM_IV_LEN = 12
    private const val GCM_TAG_BITS = 128
    private const val ANDROID_KEYSTORE = "AndroidKeyStore"

    /** Own alias, versioned, so rotating it touches nothing else. */
    public const val WRAP_KEY_ALIAS: String = "me.lakko.feed-wrap.v1"

    private const val ROOT_DIR = "lakko-feed"
    private const val POSTS_DIR = "channel-posts"
    private const val FEEDS_DIR = "feeds"

    private val json = Json {
        ignoreUnknownKeys = true
        explicitNulls = false
    }

    private val postsVersion = MutableStateFlow(0L)

    /** Bumped on every channel-post log write, so the feed re-reads it. */
    public val channelPostsChanged: StateFlow<Long> = postsVersion.asStateFlow()

    private val lock = Any()

    public fun loadChannelPostLog(context: Context, accountIdB64: String): ChannelPostLog {
        val text = read(context, POSTS_DIR, accountIdB64) ?: return ChannelPostLog()
        val raw = try {
            json.decodeFromString(ChannelPostLog.serializer(), text)
        } catch (_: Throwable) {
            null
        }
        return sanitizeChannelPostLog(raw) ?: ChannelPostLog()
    }

    public fun saveChannelPostLog(context: Context, accountIdB64: String, log: ChannelPostLog): Boolean {
        val ok = write(context, POSTS_DIR, accountIdB64, json.encodeToString(ChannelPostLog.serializer(), log))
        if (ok) postsVersion.update { it + 1 }
        return ok
    }

    public fun loadFeedState(context: Context, accountIdB64: String): FeedState {
        val text = read(context, FEEDS_DIR, accountIdB64) ?: return FeedState()
        val wire = try {
            json.decodeFromString(FeedStateWire.serializer(), text)
        } catch (_: Throwable) {
            null
        }
        return decodeFeedState(wire)
    }

    public fun saveFeedState(context: Context, accountIdB64: String, state: FeedState): Boolean =
        write(context, FEEDS_DIR, accountIdB64, json.encodeToString(FeedStateWire.serializer(), encodeFeedState(state)))

    /** Forget device: both stores for the account. The wrap key stays (it is
     *  shared by every account on this device). */
    public fun deleteAccount(context: Context, accountIdB64: String) {
        synchronized(lock) {
            file(context, POSTS_DIR, accountIdB64).delete()
            file(context, FEEDS_DIR, accountIdB64).delete()
            file(context, FeedReadingStore.DIR, accountIdB64).delete()
        }
        FeedReadingStore.forget(accountIdB64)
        postsVersion.update { it + 1 }
    }

    /** Read a sealed file under [dir] (also used by the ADR-0059 Discover
     *  store, which keeps its own directories under the same root and key). */
    internal fun read(context: Context, dir: String, accountIdB64: String): String? {
        synchronized(lock) {
        val f = file(context, dir, accountIdB64)
        if (!f.exists()) return null
        try {
            val blob = f.readBytes()
            if (blob.size <= GCM_IV_LEN) return null
            val cipher = Cipher.getInstance(AES_GCM_NOPAD)
            cipher.init(
                Cipher.DECRYPT_MODE,
                wrapKey(),
                GCMParameterSpec(GCM_TAG_BITS, blob.copyOfRange(0, GCM_IV_LEN)),
            )
            return String(cipher.doFinal(blob.copyOfRange(GCM_IV_LEN, blob.size)), Charsets.UTF_8)
        } catch (_: Throwable) {
            return null
        }
        }
    }

    /** Seal and write [text] under [dir]; `false` on any failure. */
    internal fun write(context: Context, dir: String, accountIdB64: String, text: String): Boolean {
        synchronized(lock) {
        try {
            val cipher = Cipher.getInstance(AES_GCM_NOPAD)
            cipher.init(Cipher.ENCRYPT_MODE, wrapKey())
            val iv = cipher.iv
            if (iv.size != GCM_IV_LEN) return false
            val ct = cipher.doFinal(text.toByteArray(Charsets.UTF_8))
            val target = file(context, dir, accountIdB64)
            val tmp = File(target.parentFile, target.name + ".tmp")
            tmp.writeBytes(iv + ct)
            if (!tmp.renameTo(target)) {
                target.writeBytes(iv + ct)
                tmp.delete()
            }
            return true
        } catch (_: Throwable) {
            return false
        }
        }
    }

    private fun wrapKey(): SecretKey {
        val ks = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        (ks.getKey(WRAP_KEY_ALIAS, null) as? SecretKey)?.let { return it }
        val spec = KeyGenParameterSpec.Builder(
            WRAP_KEY_ALIAS,
            KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
        )
            .setKeySize(256)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setRandomizedEncryptionRequired(true)
            .setUserAuthenticationRequired(false)
            .build()
        val kg = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEYSTORE)
        kg.init(spec)
        return kg.generateKey()
    }

    /** Delete one sealed file under [dir]. */
    internal fun delete(context: Context, dir: String, accountIdB64: String) {
        synchronized(lock) { file(context, dir, accountIdB64).delete() }
    }

    private fun file(context: Context, dir: String, accountIdB64: String): File {
        val d = File(File(context.noBackupFilesDir, ROOT_DIR), dir)
        if (!d.exists()) d.mkdirs()
        return File(d, accountIdB64.replace("/", "_") + ".bin")
    }
}

/**
 * The feed's per-device conveniences: the selected feed and whether the
 * "this device" banner was dismissed. Plain preferences — neither is a
 * secret, and losing them only resets the switcher to Latest.
 */
public object FeedPrefs {
    private const val FILE = "lakko.feed"
    private const val KEY_SELECTION = "selection"
    private const val KEY_BANNER = "device-banner-dismissed"

    public fun selection(context: Context): String? =
        context.getSharedPreferences(FILE, Context.MODE_PRIVATE).getString(KEY_SELECTION, null)

    public fun setSelection(context: Context, value: String) {
        context.getSharedPreferences(FILE, Context.MODE_PRIVATE).edit().putString(KEY_SELECTION, value).apply()
    }

    public fun bannerDismissed(context: Context): Boolean =
        context.getSharedPreferences(FILE, Context.MODE_PRIVATE).getBoolean(KEY_BANNER, false)

    public fun dismissBanner(context: Context) {
        context.getSharedPreferences(FILE, Context.MODE_PRIVATE).edit().putBoolean(KEY_BANNER, true).apply()
    }

    public fun clear(context: Context) {
        context.getSharedPreferences(FILE, Context.MODE_PRIVATE).edit().clear().apply()
    }
}
