// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.discovery.DiscoveryClient
import uniffi.lakko_ffi_uniffi.FfiCallInviteMedia
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.composeCallInviteCapability
import java.security.SecureRandom

/**
 * Owner-side composer orchestration for link-based call
 * invites (Phase 3c3-composer-helper).
 *
 * Wraps the five-step "create a call link" pipeline into a
 * single suspend function so the UI doesn't have to know
 * about the call_id RNG, the one-time KeyPackage mint, the
 * FFI boundary, the discovery HTTP round-trip, or the URL
 * builder convention.
 *
 * Sibling of `apps/web/src/calls/composer.ts` +
 * `apps/ios/Lakko/Calls/CallInviteComposer.swift` — same
 * pipeline shape, platform-idiomatic surface.
 */
public class CallInviteComposer(
    private val discoveryClient: DiscoveryClient,
    private val secureRandom: SecureRandom = SecureRandom(),
) {

    public sealed class ComposeException(message: String) : Exception(message) {
        public data class WrongOwnerSigningSeedLength(val expected: Int, val got: Int) :
            ComposeException("ownerSigningSeed must be $expected bytes, got $got")

        public data class WrongOwnerAccountIdLength(val expected: Int, val got: Int) :
            ComposeException("ownerAccountId must be $expected bytes, got $got")

        public data class LabelTooLong(val maxBytes: Int, val got: Int) :
            ComposeException("label is $got bytes (UTF-8), max is $maxBytes")

        public object KeyPackageEmpty :
            ComposeException("identity.createKeyPackage() returned 0 bytes")
    }

    /**
     * Result of [composeAndUpload]. The three primary fields
     * are independent — UI typically surfaces [url] in the
     * share sheet, persists [blobId] for the future revoke
     * flow, and keeps [callId] around for the in-call
     * signaling layer.
     */
    public data class Composed(
        /** Raw 32-byte content-addressed `blob_id`. Pass to
         *  [DiscoveryClient.revokeCallInvite] when the owner
         *  cancels the link. */
        val blobId: ByteArray,
        /** The full `<origin>/c/<slug>` URL the owner
         *  shares — base64url-no-pad of [blobId]. */
        val url: String,
        /** Freshly-minted 16-byte `call_id` embedded in the
         *  capability. Stored client-side so a subsequent
         *  call-admission JWT mint binds to the same id. */
        val callId: ByteArray,
        /** Wire-format capability bytes the server now holds.
         *  Returned for the rare case where the UI wants to
         *  drive `CallInviteCapability.decodeAndVerify(...)`
         *  against the same bytes. */
        val capabilityBytes: ByteArray,
    ) {
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is Composed) return false
            return blobId.contentEquals(other.blobId) &&
                url == other.url &&
                callId.contentEquals(other.callId) &&
                capabilityBytes.contentEquals(other.capabilityBytes)
        }

        override fun hashCode(): Int {
            var result = blobId.contentHashCode()
            result = 31 * result + url.hashCode()
            result = 31 * result + callId.contentHashCode()
            result = 31 * result + capabilityBytes.contentHashCode()
            return result
        }
    }

    /**
     * End-to-end "create call link" pipeline.
     *
     * Five steps:
     *   1. Generate a fresh 16-byte random `call_id` via
     *      [SecureRandom].
     *   2. Mint a one-time MLS `KeyPackage` via
     *      `identity.createKeyPackage()` — the joiner
     *      consumes it to bootstrap the call MLS group.
     *   3. `composeCallInviteCapability(...)` via UniFFI —
     *      wire bytes signed under the owner's Ed25519 key.
     *   4. [DiscoveryClient.uploadCallInvite] — pushes the
     *      bytes to the server-side ledger.
     *   5. [composePointerCallInviteUrl] — builds the
     *      `<origin>/c/<slug>` URL.
     *
     * The call must be driven from a background coroutine
     * because the upload step solves a server-issued PoW
     * challenge that can take a few seconds on slow devices.
     */
    public suspend fun composeAndUpload(
        ownerSigningSeed: ByteArray,
        ownerAccountId: ByteArray,
        identity: Identity,
        media: CallMediaPreset,
        expiryPreset: CallInviteExpiryPreset,
        maxUsesPreset: CallInviteMaxUsesPreset,
        label: String,
        origin: String = CALL_INVITE_DEFAULT_ORIGIN,
    ): Composed {
        // ---- 0. Local validation -------------------------------
        if (ownerSigningSeed.size != 32) {
            throw ComposeException.WrongOwnerSigningSeedLength(32, ownerSigningSeed.size)
        }
        if (ownerAccountId.size != 16) {
            throw ComposeException.WrongOwnerAccountIdLength(16, ownerAccountId.size)
        }
        val labelBytes = label.toByteArray(Charsets.UTF_8).size
        if (labelBytes > CALL_INVITE_LABEL_MAX_LEN) {
            throw ComposeException.LabelTooLong(CALL_INVITE_LABEL_MAX_LEN, labelBytes)
        }

        // ---- 1. call_id RNG ------------------------------------
        val callId = ByteArray(16)
        secureRandom.nextBytes(callId)

        // ---- 2. One-time KeyPackage mint -----------------------
        val oneTimeKeyPackage = identity.createKeyPackage()
        if (oneTimeKeyPackage.isEmpty()) {
            throw ComposeException.KeyPackageEmpty
        }

        // ---- 3. Resolve presets to wire-level values -----------
        val nowMs = System.currentTimeMillis()
        val expiryMs = expiryPreset.toMs(nowMs).toULong()
        val maxUses = maxUsesPreset.toValue()
        val mediaFlags = FfiCallInviteMedia(
            audio = true, // every call carries audio
            video = media == CallMediaPreset.AudioVideo,
        )

        // ---- 4. composeCallInviteCapability via UniFFI ---------
        val capabilityBytes = composeCallInviteCapability(
            ownerSigningSeed = ownerSigningSeed,
            ownerAccountId = ownerAccountId,
            callId = callId,
            expiryMs = expiryMs,
            maxUses = maxUses,
            media = mediaFlags,
            oneTimeKeyPackage = oneTimeKeyPackage,
            label = label,
        )

        // ---- 5. uploadCallInvite via HTTP ----------------------
        val blobId = discoveryClient.uploadCallInvite(capabilityBytes)

        // ---- 6. URL build --------------------------------------
        val url = composePointerCallInviteUrl(blobId, origin)

        return Composed(
            blobId = blobId,
            url = url,
            callId = callId,
            capabilityBytes = capabilityBytes,
        )
    }
}
