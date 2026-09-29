// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.discovery.DiscoveryNetworkError
import uniffi.lakko_ffi_uniffi.CallInviteCapability
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
     * Result of [composeAndUpload]. The UI hands [url] to the share sheet;
     * [blobId] is what a future revoke would name, `null` when the upload
     * failed on transport and [url] is the inline form.
     */
    public data class Composed(
        /** Raw 6-byte server-assigned `blob_id`, or `null` for an inline link. */
        val blobId: ByteArray?,
        /** The full `<origin>/c/<slug>#<secret>` URL the owner shares. */
        val url: String,
        /** Plaintext capability wire (never uploaded; the server holds only
         *  the sealed blob). */
        val capabilityBytes: ByteArray,
    ) {
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is Composed) return false
            val otherId = other.blobId
            val sameId = if (blobId == null) otherId == null else otherId != null && blobId.contentEquals(otherId)
            return sameId &&
                url == other.url &&
                capabilityBytes.contentEquals(other.capabilityBytes)
        }

        override fun hashCode(): Int {
            var result = blobId?.contentHashCode() ?: 0
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
     *      under the owner's Ed25519 key, with the section-4 values, sealed
     *      under a fresh link secret (the `InviteLinkMint`).
     *   3. [DiscoveryClient.uploadCallInvite] — pushes the sealed blob and its
     *      clear fields to the ledger. The secret never leaves the device.
     *   4. [composePointerCallInviteUrl] — builds `<origin>/c/<slug>#<secret>`;
     *      when the upload fails on transport, [composeInlineCallInviteUrl]
     *      carries the sealed blob in the URL instead.
     *
     * Must be driven from a background coroutine: the upload is a network
     * round trip.
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
        val expiryMs = callInviteLinkExpiryMs(nowMs)
        val maxUses = callInviteLinkMaxUses()
        val mint = composeCallInviteCapability(
            ownerSigningSeed = ownerSigningSeed,
            ownerAccountId = ownerAccountId,
            callId = callId,
            expiryMs = expiryMs.toULong(),
            maxUses = maxUses,
            media = FfiCallInviteMedia(audio = true, video = CALL_INVITE_LINK_MEDIA_VIDEO),
            oneTimeKeyPackage = oneTimeKeyPackage,
            label = CALL_INVITE_LINK_LABEL,
        )
        // The plaintext wire is the capability the joiner opens to; kept
        // for the caller, never uploaded.
        val capabilityBytes = CallInviteCapability.openLink(mint.sealed, mint.linkSecret).use { it.wire() }

        // ---- 3. uploadCallInvite via HTTP ----------------------
        val blobId = try {
            discoveryClient.uploadCallInvite(
                sealed = mint.sealed,
                issuerPubkey = mint.issuerPubkey,
                expiryMs = expiryMs,
                maxUses = maxUses.toLong(),
            )
        } catch (e: DiscoveryNetworkError) {
            // Transport failure only: the link still works inline. Any HTTP
            // answer (401, 400, 429) propagates.
            null
        }

        // ---- 4. URL build --------------------------------------
        val url = if (blobId != null) {
            composePointerCallInviteUrl(blobId, mint.linkSecret, origin)
        } else {
            composeInlineCallInviteUrl(mint.sealed, mint.linkSecret, origin)
        }
        return Composed(
            blobId = blobId,
            url = url,
            capabilityBytes = capabilityBytes,
        )
    }
}
