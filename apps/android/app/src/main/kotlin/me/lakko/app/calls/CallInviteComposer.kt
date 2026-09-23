// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.discovery.DiscoveryClient
import uniffi.lakko_ffi_uniffi.FfiCallInviteMedia
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.composeCallInviteCapability

/**
 * Owner-side composer for the invite link of a call that is already happening
 * (ADR-0032, `docs/ux/call-links.md` sections 3 and 4).
 *
 * Wraps the four-step pipeline into a single suspend function so the UI does
 * not have to know about the one-time KeyPackage mint, the FFI boundary, the
 * discovery round trip or the URL convention.
 *
 * The `call_id` is an **argument**, not something minted here: the link must
 * lead into the call the person is sitting in, and that call already has an id.
 * The four fields the old form asked about are filled from the section-4 values
 * in `CallInviteLink.kt` instead of being asked.
 *
 * Sibling of `apps/web/src/calls/composer.ts` +
 * `apps/ios/Lakko/Calls/CallInviteComposer.swift` — same pipeline shape,
 * platform-idiomatic surface.
 */
public class CallInviteComposer(
    private val discoveryClient: DiscoveryClient,
) {

    public sealed class ComposeException(message: String) : Exception(message) {
        public data class WrongOwnerSigningSeedLength(val expected: Int, val got: Int) :
            ComposeException("ownerSigningSeed must be $expected bytes, got $got")

        public data class WrongOwnerAccountIdLength(val expected: Int, val got: Int) :
            ComposeException("ownerAccountId must be $expected bytes, got $got")

        public data class WrongCallIdLength(val expected: Int, val got: Int) :
            ComposeException("callId must be $expected bytes, got $got")

        public object KeyPackageEmpty :
            ComposeException("identity.createKeyPackage() returned 0 bytes")
    }

    /**
     * Result of [composeAndUpload]. The fields are independent — the UI hands
     * [url] to the share sheet, and [blobId] is what a future revoke would name.
     */
    public data class Composed(
        /** Raw 32-byte content-addressed `blob_id`. */
        val blobId: ByteArray,
        /** The full `<origin>/c/<slug>` URL the owner shares. */
        val url: String,
        /** Wire-format capability bytes the server now holds. */
        val capabilityBytes: ByteArray,
    ) {
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is Composed) return false
            return blobId.contentEquals(other.blobId) &&
                url == other.url &&
                capabilityBytes.contentEquals(other.capabilityBytes)
        }

        override fun hashCode(): Int {
            var result = blobId.contentHashCode()
            result = 31 * result + url.hashCode()
            result = 31 * result + capabilityBytes.contentHashCode()
            return result
        }
    }

    /**
     * Mint the invite link for the call [callId] names.
     *
     * Four steps:
     *   1. Mint a one-time MLS `KeyPackage`. ADR-0032 makes the owner the sole
     *      founder, so this one goes unused — the wire format still carries the
     *      field, and a well-formed capability is what the joiner verifies.
     *   2. `composeCallInviteCapability(...)` via UniFFI — wire bytes signed
     *      under the owner's Ed25519 key, with the section-4 values.
     *   3. [DiscoveryClient.uploadCallInvite] — pushes the bytes to the ledger.
     *   4. [composePointerCallInviteUrl] — builds the `<origin>/c/<slug>` URL.
     *
     * Must be driven from a background coroutine: the upload solves a
     * server-issued proof-of-work challenge that takes seconds on slow devices.
     */
    public suspend fun composeAndUpload(
        ownerSigningSeed: ByteArray,
        ownerAccountId: ByteArray,
        identity: Identity,
        callId: ByteArray,
        nowMs: Long = System.currentTimeMillis(),
        origin: String = CALL_INVITE_DEFAULT_ORIGIN,
    ): Composed {
        // ---- 0. Local validation -------------------------------
        if (ownerSigningSeed.size != 32) {
            throw ComposeException.WrongOwnerSigningSeedLength(32, ownerSigningSeed.size)
        }
        if (ownerAccountId.size != 16) {
            throw ComposeException.WrongOwnerAccountIdLength(16, ownerAccountId.size)
        }
        if (callId.size != CALL_ID_LEN) {
            throw ComposeException.WrongCallIdLength(CALL_ID_LEN, callId.size)
        }

        // ---- 1. One-time KeyPackage mint -----------------------
        val oneTimeKeyPackage = identity.createKeyPackage()
        if (oneTimeKeyPackage.isEmpty()) {
            throw ComposeException.KeyPackageEmpty
        }

        // ---- 2. composeCallInviteCapability via UniFFI ---------
        val capabilityBytes = composeCallInviteCapability(
            ownerSigningSeed = ownerSigningSeed,
            ownerAccountId = ownerAccountId,
            callId = callId,
            expiryMs = callInviteLinkExpiryMs(nowMs).toULong(),
            maxUses = callInviteLinkMaxUses(),
            media = FfiCallInviteMedia(audio = true, video = CALL_INVITE_LINK_MEDIA_VIDEO),
            oneTimeKeyPackage = oneTimeKeyPackage,
            label = CALL_INVITE_LINK_LABEL,
        )

        // ---- 3. uploadCallInvite via HTTP ----------------------
        val blobId = discoveryClient.uploadCallInvite(capabilityBytes)

        // ---- 4. URL build --------------------------------------
        return Composed(
            blobId = blobId,
            url = composePointerCallInviteUrl(blobId, origin),
            capabilityBytes = capabilityBytes,
        )
    }
}
