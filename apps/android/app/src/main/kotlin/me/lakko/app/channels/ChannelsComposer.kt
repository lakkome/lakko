// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import java.security.SecureRandom
import uniffi.lakko_ffi_uniffi.ChannelSigningKey
import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.composeChannelPost as uniffiComposeChannelPost
import uniffi.lakko_ffi_uniffi.decodeChannelPost as uniffiDecodeChannelPost
import uniffi.lakko_ffi_uniffi.newChannelSeedForTest as uniffiNewChannelSeedForTest
import uniffi.lakko_ffi_uniffi.verifyAndDecryptChannelPost as uniffiVerifyAndDecryptChannelPost

/**
 * Local publisher keys for a single channel. Mirror of
 * `apps/web/src/channels/composer.ts::ChannelPublisherKeys`
 * and the iOS `ChannelPublisherKeys` value type.
 *
 * Persisted in-memory in `ChannelsUiState`; a follow-up
 * sprint moves the `signingKeySeed` into the device
 * Keystore via `androidx.security.crypto`.
 */
public data class ChannelPublisherKeys(
    /** 32-byte content-key seed. **Demo only** —
     *  production seeds come from PublisherGroup
     *  `mls_exporter("lakko/channel/v1", channel_id, 32)`
     *  once the MLS-per-channel keying lands. */
    public val seed: ByteArray,
    /** 32-byte Ed25519 signing-key seed. */
    public val signingKeySeed: ByteArray,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is ChannelPublisherKeys) return false
        return seed.contentEquals(other.seed) &&
            signingKeySeed.contentEquals(other.signingKeySeed)
    }

    override fun hashCode(): Int {
        var result = seed.contentHashCode()
        result = 31 * result + signingKeySeed.contentHashCode()
        return result
    }
}

/** Errors the composer surfaces. Same shape as the iOS /
 *  web sides — UI branches on the variant, never on the
 *  message string. */
public sealed class ChannelsComposerException(message: String) : Exception(message) {
    public class BadChannelIdHex(message: String) : ChannelsComposerException(message)
    public class BadBase64(message: String) : ChannelsComposerException(message)
    /** Wrapped UniFFI `FfiException.Channels` — the
     *  underlying detail is in the message. */
    public class Ffi(message: String) : ChannelsComposerException(message)
}

/**
 * Thin Kotlin shim around the UniFFI channels surface.
 * Same six entry points the web + iOS shims expose.
 */
public object ChannelsComposer {

    private val rng = SecureRandom()

    /** Fresh (seed, signingKeySeed) pair backed by
     *  `getrandom` on the Rust side (Android NDK uses
     *  /dev/urandom). */
    public fun freshPublisherKeys(): ChannelPublisherKeys {
        val seed = uniffiNewChannelSeedForTest()
        val signer = ChannelSigningKey.generate()
        try {
            return ChannelPublisherKeys(
                seed = seed,
                signingKeySeed = signer.toSeed(),
            )
        } finally {
            signer.close()
        }
    }

    /** Compose a signed encrypted ChannelPost. Returns
     *  the wire bytes ready for
     *  `ChannelsClient.publishPost`. */
    public fun compose(
        keys: ChannelPublisherKeys,
        channelIdHex: String,
        epochId: UInt,
        messageCounter: ULong,
        plaintext: ByteArray,
    ): ByteArray {
        val channelId = channelIdFromHex(channelIdHex)
        val signer = try {
            ChannelSigningKey(keys.signingKeySeed)
        } catch (e: FfiException.Channels) {
            throw ChannelsComposerException.Ffi(e.message ?: "signing-key construct failed")
        }
        try {
            return uniffiComposeChannelPost(
                seed = keys.seed,
                channelId = channelId,
                epochId = epochId,
                signingKey = signer,
                messageCounter = messageCounter,
                plaintext = plaintext,
            )
        } catch (e: FfiException.Channels) {
            throw ChannelsComposerException.Ffi(e.message ?: "compose failed")
        } finally {
            signer.close()
        }
    }

    /** Verify + decrypt a base64url-encoded ChannelPost
     *  (the form `ChannelsClient.fetchPosts` returns).
     *  Returns the recovered plaintext bytes. */
    public fun verifyAndDecrypt(
        seed: ByteArray,
        channelIdHex: String,
        postWireB64Url: String,
    ): ByteArray {
        val channelId = channelIdFromHex(channelIdHex)
        val wire = base64UrlDecode(postWireB64Url)
            ?: throw ChannelsComposerException.BadBase64(postWireB64Url)
        val post = try {
            uniffiDecodeChannelPost(wire)
        } catch (e: FfiException.Channels) {
            throw ChannelsComposerException.Ffi(e.message ?: "decode failed")
        }
        try {
            return uniffiVerifyAndDecryptChannelPost(
                seed = seed,
                channelId = channelId,
                post = post,
            )
        } catch (e: FfiException.Channels) {
            throw ChannelsComposerException.Ffi(e.message ?: "verifyAndDecrypt failed")
        } finally {
            post.close()
        }
    }

    /** 32 hex chars → 16 bytes. */
    public fun channelIdFromHex(hex: String): ByteArray {
        if (hex.length != 32) {
            throw ChannelsComposerException.BadChannelIdHex(
                "channel id must be 32 hex chars (got ${hex.length})"
            )
        }
        val bytes = ByteArray(16)
        for (i in 0 until 16) {
            val pair = hex.substring(i * 2, i * 2 + 2)
            val byte = pair.toIntOrNull(16) ?: throw ChannelsComposerException.BadChannelIdHex(
                "invalid hex byte '$pair' at position ${i * 2}"
            )
            bytes[i] = byte.toByte()
        }
        return bytes
    }

    /** Hex-encode bytes (lowercase, no separators). */
    public fun bytesToHex(bytes: ByteArray): String {
        val sb = StringBuilder(bytes.size * 2)
        for (b in bytes) {
            sb.append(String.format("%02x", b.toInt() and 0xFF))
        }
        return sb.toString()
    }

    /** Generate 16 fresh bytes + hex-render for use as a
     *  new channel id. Backed by `SecureRandom` — Android's
     *  CSPRNG. */
    public fun freshChannelIdHex(): String {
        val bytes = ByteArray(16)
        rng.nextBytes(bytes)
        return bytesToHex(bytes)
    }

    private fun base64UrlDecode(s: String): ByteArray? {
        // Re-pad + canonicalise to standard base64 so
        // `java.util.Base64` accepts it.
        val std = s.replace('-', '+').replace('_', '/')
        val pad = (4 - (std.length % 4)) % 4
        val padded = std + "=".repeat(pad)
        return runCatching { java.util.Base64.getDecoder().decode(padded) }.getOrNull()
    }
}
