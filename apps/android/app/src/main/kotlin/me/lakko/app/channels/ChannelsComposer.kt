// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import java.security.SecureRandom
import uniffi.lakko_ffi_uniffi.ChannelSigningKey
import uniffi.lakko_ffi_uniffi.ChannelSubscriberKeypair
import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.Group
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.channelExporterLabel as uniffiChannelExporterLabel
import uniffi.lakko_ffi_uniffi.channelSeedEnvelopeLen as uniffiChannelSeedEnvelopeLen
import uniffi.lakko_ffi_uniffi.channelSeedFromMlsExporterOutput as uniffiChannelSeedFromMlsExporterOutput
import uniffi.lakko_ffi_uniffi.composeChannelPost as uniffiComposeChannelPost
import uniffi.lakko_ffi_uniffi.decodeChannelPost as uniffiDecodeChannelPost
import uniffi.lakko_ffi_uniffi.openSubscriberSeedEnvelope as uniffiOpenSubscriberSeedEnvelope
import uniffi.lakko_ffi_uniffi.sealSubscriberSeedEnvelope as uniffiSealSubscriberSeedEnvelope
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

/**
 * Subscriber-side X25519 keypair (Phase 4b G4-3c). Mirror
 * of the web `ChannelSubscriberKeys` and the iOS
 * `ChannelSubscriberKeys` value type.
 *
 * The public half registers on
 * `POST /v1/channels/:id/subscribers`; the secret half
 * stays on the device + unwraps the per-subscriber HPKE
 * seed envelopes the publisher fans out on every
 * rotation.
 *
 * Persisted in `ChannelsUiState` via Android
 * `SharedPreferences` until the encrypted-at-rest
 * device-group-keyed store ships — same migration path
 * `ChannelPublisherKeys.signingKeySeed` will follow.
 */
public data class ChannelSubscriberKeys(
    /** 32-byte X25519 secret. */
    public val secret: ByteArray,
    /** 32-byte X25519 public. */
    public val publicKey: ByteArray,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is ChannelSubscriberKeys) return false
        return secret.contentEquals(other.secret) &&
            publicKey.contentEquals(other.publicKey)
    }

    override fun hashCode(): Int {
        var result = secret.contentHashCode()
        result = 31 * result + publicKey.contentHashCode()
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

    /**
     * Build the publisher's `(seed, signingKeySeed)` pair
     * for `channelIdHex`, deriving the content-key seed
     * from a fresh MLS **PublisherGroup** exporter (G-27 /
     * ADR-0010 Tier-1).
     *
     * Mirror of
     * `apps/web/src/channels/composer.ts::freshPublisherKeys`
     * and `apps/ios/Lakko/Channels/ChannelsComposer.swift`.
     * The flow is:
     *
     * 1. Build a transient `Group` with `identity` as the
     *    founding member.
     * 2. Call `group.exportSecret(identity, label =
     *    channelExporterLabel(), context = channel_id,
     *    length = 32u)` — produces 32 bytes domain-separated
     *    by `"lakko/channel/v1"` + the 16-byte channel id.
     * 3. Hand the bytes to
     *    `channelSeedFromMlsExporterOutput` so the seed
     *    crosses the FFI boundary through the audit-pack
     *    surface (C-44).
     *
     * The group is throwaway — it lives only for the
     * duration of this call, exactly like the web shim.
     * PublisherGroup persistence + rotation across
     * composer calls lands in a follow-up sub-sprint
     * (working name `G4-2-mls-persistence`).
     *
     * `signingKeySeed` stays randomly minted by the
     * underlying Android NDK CSPRNG — ADR-0010 treats the
     * signing key as a long-lived per-channel publisher
     * identity, not as something derived from the
     * call-time exporter. It belongs in
     * `androidx.security.crypto` once the migration
     * sub-sprint lands.
     */
    public fun freshPublisherKeys(
        identity: Identity,
        channelIdHex: String,
    ): ChannelPublisherKeys {
        val channelId = channelIdFromHex(channelIdHex)

        val exporterOutput: ByteArray = try {
            val group = Group.create(identity)
            try {
                group.exportSecret(
                    identity = identity,
                    label = uniffiChannelExporterLabel(),
                    context = channelId,
                    length = 32u,
                )
            } finally {
                // `Group` is UniFFI-Disposable +
                // AutoCloseable — explicit close() drops
                // the Rust object now rather than waiting
                // on JVM GC.
                group.close()
            }
        } catch (e: FfiException.Channels) {
            throw ChannelsComposerException.Ffi(e.message ?: "PublisherGroup exporter failed")
        }

        val seed: ByteArray = try {
            uniffiChannelSeedFromMlsExporterOutput(exporterOutput)
        } catch (e: FfiException.Channels) {
            throw ChannelsComposerException.Ffi(e.message ?: "channelSeedFromMlsExporterOutput failed")
        }

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
            ChannelSigningKey.fromSeed(keys.signingKeySeed)
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
            // ChannelSigningKey implements AutoCloseable;
            // close() forwards to destroy() which releases
            // the underlying Rust object. The try/finally
            // shape here predates the `use {}` idiom.
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

    // -----------------------------------------------------------------
    // Subscriber-side (G4-3c)
    // -----------------------------------------------------------------

    /** Fresh subscriber X25519 keypair, backed by
     *  `getrandom` on the Rust side. */
    public fun freshSubscriberKeys(): ChannelSubscriberKeys {
        val kp = ChannelSubscriberKeypair.generate()
        try {
            return ChannelSubscriberKeys(
                secret = kp.secret(),
                publicKey = kp.`public`(),
            )
        } finally {
            kp.close()
        }
    }

    /** Publisher-side: seal a 32-byte content-key seed
     *  against a subscriber's X25519 public key. Returns
     *  the 80-byte wire bytes ready for
     *  `ChannelsClient.deliverEnvelope`. */
    public fun sealSubscriberSeedEnvelope(
        subscriberPub: ByteArray,
        channelIdHex: String,
        epochId: UInt,
        seed: ByteArray,
    ): ByteArray {
        val channelId = channelIdFromHex(channelIdHex)
        try {
            return uniffiSealSubscriberSeedEnvelope(
                subscriberPub = subscriberPub,
                channelId = channelId,
                epochId = epochId,
                seed = seed,
            )
        } catch (e: FfiException.Channels) {
            throw ChannelsComposerException.Ffi(e.message ?: "seal_seed failed")
        }
    }

    /** Subscriber-side: open a seed envelope with the
     *  local keypair. Returns the recovered 32-byte
     *  content-key seed — hand it to ``verifyAndDecrypt``
     *  to decode posts on the channel for this epoch. */
    public fun openSubscriberSeedEnvelope(
        keys: ChannelSubscriberKeys,
        channelIdHex: String,
        epochId: UInt,
        wire: ByteArray,
    ): ByteArray {
        val channelId = channelIdFromHex(channelIdHex)
        val kp = try {
            ChannelSubscriberKeypair.fromSecret(keys.secret)
        } catch (e: FfiException.Channels) {
            throw ChannelsComposerException.Ffi(
                e.message ?: "subscriber-keypair construct failed"
            )
        }
        try {
            return uniffiOpenSubscriberSeedEnvelope(
                keypair = kp,
                channelId = channelId,
                epochId = epochId,
                wire = wire,
            )
        } catch (e: FfiException.Channels) {
            throw ChannelsComposerException.Ffi(e.message ?: "open_seed failed")
        } finally {
            kp.close()
        }
    }

    /** Wire byte length of a seed envelope —
     *  `eph_pub(32) + sealed_seed(32) + tag(16) = 80`.
     *  Exposed for UI sanity checks. */
    public fun seedEnvelopeLen(): UInt = uniffiChannelSeedEnvelopeLen()

    /** Decode a base64url-no-pad string to bytes. Returns
     *  `null` on parse failure. Exposed so the
     *  subscriber-side polling loop can hand the envelope
     *  bytes to ``openSubscriberSeedEnvelope`` without
     *  reaching into composer privates. */
    public fun base64UrlDecodeOrNull(s: String): ByteArray? = base64UrlDecode(s)

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

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
