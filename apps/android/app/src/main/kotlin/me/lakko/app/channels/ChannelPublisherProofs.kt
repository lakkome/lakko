// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import java.util.concurrent.ConcurrentHashMap
import uniffi.lakko_ffi_uniffi.ChannelCoPublisherSigner
import uniffi.lakko_ffi_uniffi.ChannelOwnerSigner
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.channelCopublisherProofHeaderName
import uniffi.lakko_ffi_uniffi.channelCopublisherSigner
import uniffi.lakko_ffi_uniffi.channelOwnerProofHeaderName

/**
 * ADR-0059 section 9 (G-43) — the publisher-signed channel writes.
 *
 * `services/channels` refuses publish, deliver, rotate, rotate/stage and
 * rotate/commit (and the subscriber list) without a proof signed by the
 * channel's owner key or by a co-publisher key the owner appointed. Every
 * refusal is `401 {"code":"owner-proof-required"}`.
 */

/** What this account is to a channel it publishes to. */
public enum class ChannelPublisherRole {
    /** It created the channel and derives the key registered for it. */
    OWNER,

    /** The owner appointed its co-publisher key after approving it. */
    COPUBLISHER,
}

/** Signs publisher requests for one (account, channel). A fresh proof per
 *  call: each nonce is single use on the server. */
public interface ChannelPublisherProofs {
    /** Unique to the (account, channel) pair: the owner verifying key hex the
     *  account derives for the channel. Keys the [ChannelPublisherRoles] cache. */
    public val roleKey: String

    /** `(header name, header value)` for one request under [role]. [path] is
     *  the service path (`/v1/channels/...`, never the prefix nginx strips),
     *  [query] the exact query sent without `?`, [body] the exact bytes sent. */
    public fun proofHeader(
        role: ChannelPublisherRole,
        method: String,
        path: String,
        query: String,
        body: ByteArray,
    ): Pair<String, String>
}

/** The production [ChannelPublisherProofs]: both signers derived once from
 *  the account identity, which is not needed afterwards. Close after use. */
public class ChannelPublisherSigners private constructor(
    private val owner: ChannelOwnerSigner,
    private val copublisher: ChannelCoPublisherSigner,
) : ChannelPublisherProofs, AutoCloseable {

    override val roleKey: String = owner.verifyingKey().joinToString("") { "%02x".format(it) }

    override fun proofHeader(
        role: ChannelPublisherRole,
        method: String,
        path: String,
        query: String,
        body: ByteArray,
    ): Pair<String, String> = when (role) {
        ChannelPublisherRole.OWNER ->
            channelOwnerProofHeaderName() to owner.sign(method, path, query, body)
        ChannelPublisherRole.COPUBLISHER ->
            channelCopublisherProofHeaderName() to copublisher.sign(method, path, query, body)
    }

    override fun close() {
        owner.close()
        copublisher.close()
    }

    public companion object {
        /** Derive both signers of [channelIdHex] for [accountIdentity]. */
        public fun derive(accountIdentity: Identity, channelIdHex: String): ChannelPublisherSigners {
            val owner = ChannelsClient.ownerSigner(accountIdentity, channelIdHex)
            val copublisher = try {
                channelCopublisherSigner(accountIdentity, ChannelsClient.channelIdBytes(channelIdHex))
            } catch (t: Throwable) {
                owner.close()
                throw t
            }
            return ChannelPublisherSigners(owner, copublisher)
        }

        /** Derive under the shared publisher identity's lock, then release it,
         *  so the network calls that follow do not hold the lock. */
        public suspend fun derive(
            publisher: ChannelPublisherIdentity,
            channelIdHex: String,
        ): ChannelPublisherSigners = publisher.withIdentity(persist = false) { derive(it, channelIdHex) }
    }
}

/**
 * The role per (account, channel), in memory only: nothing on the device
 * records it, so it is learned once per process — from creating the channel,
 * from a completed co-publisher join, or from the first accepted signed
 * request — and a large stage chunk is never uploaded twice to find out.
 */
public class ChannelPublisherRoles {
    private val byKey = ConcurrentHashMap<String, ChannelPublisherRole>()

    public operator fun get(roleKey: String): ChannelPublisherRole? = byKey[roleKey]

    public fun remember(roleKey: String, role: ChannelPublisherRole) {
        byKey[roleKey] = role
    }

    public fun forget(roleKey: String) {
        byKey.remove(roleKey)
    }

    /** Drop every cached role (sign-out, tests). */
    public fun clear() {
        byKey.clear()
    }

    public companion object {
        /** The process-wide cache every [ChannelsClient] uses by default. */
        public val shared: ChannelPublisherRoles = ChannelPublisherRoles()

        /** Record [role] for (account, channel) in [shared] — on creation, or
         *  once a co-publisher join completed. */
        public fun rememberFor(accountIdentity: Identity, channelIdHex: String, role: ChannelPublisherRole) {
            ChannelsClient.ownerSigner(accountIdentity, channelIdHex).use { owner ->
                shared.remember(owner.verifyingKey().joinToString("") { "%02x".format(it) }, role)
            }
        }
    }
}

/** The code `services/channels` answers every refused publisher proof with. */
public const val PUBLISHER_REFUSED: String = "owner-proof-required"
