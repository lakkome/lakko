// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.avatar

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * ADR-0084 — at-rest sealing for everything this device keeps about profile
 * photos: the decrypted avatar files and the blob references (which carry
 * the AEAD keys). AES-256-GCM under a non-exportable Android Keystore key of
 * its own alias, the shape [me.lakko.app.chat.MessageLogStore] uses for the
 * message log: `iv(12) || ciphertext+tag`.
 *
 * A port, so the cache's bookkeeping runs on the host JVM with
 * [PlainAvatarSeal] in place of the Keystore.
 */
public interface AvatarSeal {
    /** `iv || ciphertext`, or `null` when the key is unavailable. */
    public fun seal(plaintext: ByteArray): ByteArray?

    /** The plaintext, or `null` on any failure (corrupt, key rotated). */
    public fun open(sealed: ByteArray): ByteArray?
}

/** Host tests only: no sealing at all. */
public object PlainAvatarSeal : AvatarSeal {
    override fun seal(plaintext: ByteArray): ByteArray = plaintext.copyOf()
    override fun open(sealed: ByteArray): ByteArray = sealed.copyOf()
}

/** The production seal over the Android Keystore. */
public object KeystoreAvatarSeal : AvatarSeal {
    private const val ANDROID_KEYSTORE = "AndroidKeyStore"
    private const val ALIAS = "me.lakko.avatar-wrap.v1"
    private const val TRANSFORM = "AES/GCM/NoPadding"
    private const val IV_LEN = 12
    private const val TAG_BITS = 128

    @Synchronized
    private fun key(): SecretKey {
        val ks = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        (ks.getKey(ALIAS, null) as? SecretKey)?.let { return it }
        val spec = KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
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

    override fun seal(plaintext: ByteArray): ByteArray? = try {
        val cipher = Cipher.getInstance(TRANSFORM)
        cipher.init(Cipher.ENCRYPT_MODE, key())
        val iv = cipher.iv
        if (iv.size != IV_LEN) {
            null
        } else {
            iv + cipher.doFinal(plaintext)
        }
    } catch (_: Throwable) {
        null
    }

    override fun open(sealed: ByteArray): ByteArray? {
        if (sealed.size <= IV_LEN) return null
        return try {
            val cipher = Cipher.getInstance(TRANSFORM)
            cipher.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(TAG_BITS, sealed, 0, IV_LEN))
            cipher.doFinal(sealed, IV_LEN, sealed.size - IV_LEN)
        } catch (_: Throwable) {
            null
        }
    }
}

/** [refs] as a sealed, base64 string for a preferences row, or `null`. */
public fun sealAvatarRefs(seal: AvatarSeal, refs: me.lakko.app.profile.AvatarRefs?): String? {
    val r = refs ?: return null
    val json = kotlinx.serialization.json.Json.encodeToString(me.lakko.app.profile.AvatarRefs.serializer(), r)
    val sealed = seal.seal(json.toByteArray(Charsets.UTF_8)) ?: return null
    return java.util.Base64.getEncoder().encodeToString(sealed)
}

/** The inverse of [sealAvatarRefs]; `null` on any failure. */
public fun openAvatarRefs(seal: AvatarSeal, stored: String?): me.lakko.app.profile.AvatarRefs? {
    val s = stored ?: return null
    return try {
        val plain = seal.open(java.util.Base64.getDecoder().decode(s)) ?: return null
        kotlinx.serialization.json.Json { ignoreUnknownKeys = true }
            .decodeFromString(me.lakko.app.profile.AvatarRefs.serializer(), String(plain, Charsets.UTF_8))
    } catch (_: Throwable) {
        null
    }
}
