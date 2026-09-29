// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.discovery.ConsumeInviteResult
import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.discovery.DiscoveryHttpError
import uniffi.lakko_ffi_uniffi.AcceptedCallInvite
import uniffi.lakko_ffi_uniffi.CallInviteCapability
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.acceptCallInvite

/**
 * Joiner-side acceptor orchestration for link-based call
 * invites (Phase 3c3-acceptor-helper).
 *
 * Symmetric sibling of [CallInviteComposer] — collapses the
 * seven-step joiner pipeline into a single discriminated
 * result so the `/c/<slug>` landing screen can render the
 * right outcome copy without re-implementing the dispatch:
 *
 *   1. [parseCallInviteSlug] — pointer vs. inline — and the link
 *      secret from the `#fragment` (missing / malformed = damaged,
 *      before any request).
 *   2. (pointer branch) [DiscoveryClient.fetchCallInvite] —
 *      sealed blob or 404.
 *   3. [CallInviteCapability.openLink] — AEAD-open under the link
 *      secret + local Ed25519 signature check (a substituted blob
 *      fails the open: damaged).
 *   4. [CallInviteCapability.isValidNow] — expiry filter.
 *   5. (pointer branch) [DiscoveryClient.consumeCallInvite]
 *      — atomic counter bump + per-joiner dedup tag.
 *   6. [acceptCallInvite] — bootstrap a fresh MLS group with
 *      joiner as founder + owner added via the embedded KP.
 *   7. Return [AcceptedCallInvite.welcomeForOwner] for the
 *      platform to ferry back to the owner.
 *
 * Sibling of `apps/web/src/calls/acceptor.ts` +
 * `apps/ios/Lakko/Calls/CallInviteAcceptor.swift` — same
 * pipeline shape, platform-idiomatic surface.
 */
public class CallInviteAcceptor(
    private val discoveryClient: DiscoveryClient,
    /** The short-link widths; the core's by default, passed in by a host
     *  JVM test that has no native library. */
    private val lengths: me.lakko.app.discovery.InviteLinkLengths =
        me.lakko.app.discovery.InviteLinkLengths.core,
) {

    /**
     * Discriminated outcome. Every variant carries a non-
     * failure payload OR a stable error tag — the landing
     * screen branches exhaustively without try/catch.
     */
    public sealed class Result {
        public data class Accepted(
            val capabilityBytes: ByteArray,
            val ownerAccountId: ByteArray,
            val callId: ByteArray,
            val ownerFingerprint: String,
            val welcomeForOwner: ByteArray,
            /** 32-byte SFrame call-key seed derived via the
             *  MLS exporter (`mls_exporter("lakko/call/v1",
             *  call_id, 32)`). UI hands it to
             *  [uniffi.lakko_ffi_uniffi.sframeDeriveParticipantKey]
             *  to seal the joiner's first frame before the
             *  owner has even joined. */
            val callKeySeed: ByteArray,
        ) : Result() {
            override fun equals(other: Any?): Boolean {
                if (this === other) return true
                if (other !is Accepted) return false
                return capabilityBytes.contentEquals(other.capabilityBytes) &&
                    ownerAccountId.contentEquals(other.ownerAccountId) &&
                    callId.contentEquals(other.callId) &&
                    ownerFingerprint == other.ownerFingerprint &&
                    welcomeForOwner.contentEquals(other.welcomeForOwner) &&
                    callKeySeed.contentEquals(other.callKeySeed)
            }

            override fun hashCode(): Int {
                var result = capabilityBytes.contentHashCode()
                result = 31 * result + ownerAccountId.contentHashCode()
                result = 31 * result + callId.contentHashCode()
                result = 31 * result + ownerFingerprint.hashCode()
                result = 31 * result + welcomeForOwner.contentHashCode()
                result = 31 * result + callKeySeed.contentHashCode()
                return result
            }
        }

        /** `(blob_id, joiner_tag)` PK on the server already
         *  had a row — counter NOT bumped. UI surfaces
         *  "you're already in this call" + opens the
         *  existing call group rather than re-bootstrapping. */
        public data class AlreadyJoined(val capabilityBytes: ByteArray) : Result() {
            override fun equals(other: Any?): Boolean {
                if (this === other) return true
                if (other !is AlreadyJoined) return false
                return capabilityBytes.contentEquals(other.capabilityBytes)
            }

            override fun hashCode(): Int = capabilityBytes.contentHashCode()
        }

        public data object Expired : Result()
        public data object Exhausted : Result()
        public data object NotFound : Result()
        public data class MalformedSlug(val reason: String) : Result()

        /** The link's `#fragment` is missing or malformed, or the sealed
         *  blob does not open under it (sealed-link amendment of
         *  2026-09-28). Never retried without the secret. */
        public data object Damaged : Result()

        /** The discovery service answered `429` on fetch or consume. The
         *  link may be fine; the UI says to try again later. */
        public data object RateLimited : Result()

        /** ADR-0054: today's allowance of invite-consume tokens is used up
         *  (`ACTION_ALLOWANCE_USED_COPY`). */
        public data object AllowanceUsed : Result()
        public data class SignatureFailure(val reason: String) : Result()
        public data class Failed(val message: String) : Result()
    }

    /**
     * End-to-end "join call link" pipeline.
     *
     * Inline-capability slugs (censored-region fallback)
     * skip the fetch + consume steps entirely — consume on
     * an inline capability has no server-side row to bump.
     * The acceptor surfaces this asymmetry as
     * [Result.Accepted] with `capabilityBytes` populated;
     * the `welcomeForOwner` still needs ferrying via
     * whatever transport the user has.
     *
     * @param slug             The `<slug>` segment from a
     *                         `/c/<slug>#<fragment>` URL.
     * @param fragment         The URL fragment (text after `#`), the
     *                         link secret; `null` when the link had none.
     * @param joinerAccountId  Joiner's 16-byte UUIDv7
     *                         account_id — folded into the
     *                         per-joiner dedup tag locally
     *                         before posting to consume.
     *                         Never crosses the wire in
     *                         plaintext.
     * @param joinerIdentity   Joiner's MLS identity handle —
     *                         drives [acceptCallInvite] to
     *                         bootstrap the fresh call MLS
     *                         group.
     * @param nowMs            UNIX millis the acceptor uses
     *                         for the
     *                         [CallInviteCapability.isValidNow]
     *                         check. Defaults to "now".
     */
    public suspend fun acceptFromSlug(
        slug: String,
        fragment: String?,
        joinerAccountId: ByteArray,
        joinerIdentity: Identity,
        nowMs: Long = System.currentTimeMillis(),
    ): Result {
        // ---- 1. Parse slug -----------------------------------
        val parsed = try {
            parseCallInviteSlug(slug, lengths)
        } catch (e: CallInviteUrlException) {
            return Result.MalformedSlug(reason = e.message ?: e::class.simpleName ?: "parse error")
        }
        val linkSecret = me.lakko.app.discovery.inviteLinkSecretOrNull(fragment, lengths)
            ?: return Result.Damaged

        // ---- 2. Pointer branch: fetch bytes -----------------
        val sealed: ByteArray
        var blobId: ByteArray? = null
        when (parsed) {
            is ParsedCallInvite.Pointer -> {
                blobId = parsed.blobId
                try {
                    val fetched = discoveryClient.fetchCallInvite(parsed.blobId)
                        ?: return Result.NotFound
                    sealed = fetched
                } catch (e: DiscoveryHttpError) {
                    if (e.status == 404) return Result.NotFound
                    if (me.lakko.app.discovery.isDiscoveryRateLimited(e)) return Result.RateLimited
                    return Result.Failed(message = e.message ?: e.toString())
                } catch (e: Exception) {
                    return Result.Failed(message = e.message ?: e.toString())
                }
            }
            is ParsedCallInvite.Inline -> {
                sealed = parsed.sealed
            }
        }

        // ---- 3. openLink ------------------------------------
        val capability: CallInviteCapability = try {
            CallInviteCapability.openLink(sealed, linkSecret)
        } catch (e: Exception) {
            if (me.lakko.app.discovery.isInviteLinkDamaged(e)) return Result.Damaged
            return Result.SignatureFailure(reason = e.message ?: e.toString())
        }
        // The joiner tag is computed over the PLAINTEXT capability.
        val capabilityBytes = capability.wire()

        // ---- 4. Expiry filter --------------------------------
        if (!capability.isValidNow(nowMs.toULong())) {
            return Result.Expired
        }

        // ---- 5. Pointer branch: consume ----------------------
        val effectiveBlobId = blobId
        if (effectiveBlobId != null) {
            val consumeResult = try {
                discoveryClient.consumeCallInvite(
                    blobId = effectiveBlobId,
                    capabilityBytes = capabilityBytes,
                    joinerAccountId = joinerAccountId,
                )
            } catch (e: Exception) {
                if (me.lakko.app.discovery.isDiscoveryRateLimited(e)) return Result.RateLimited
                // ADR-0054 - the consume spends an `invite-consume` token.
                if (me.lakko.app.anon.isAnonTokenQuotaExhausted(e)) return Result.AllowanceUsed
                return Result.Failed(message = e.message ?: e.toString())
            }
            when (consumeResult) {
                is ConsumeInviteResult.NotFound -> return Result.NotFound
                is ConsumeInviteResult.Exhausted -> return Result.Exhausted
                is ConsumeInviteResult.AlreadyConsumed ->
                    return Result.AlreadyJoined(capabilityBytes = capabilityBytes)
                is ConsumeInviteResult.Consumed -> {
                    // Fall through to accept.
                }
            }
        }

        // ---- 6. acceptCallInvite (MLS bootstrap) -------------
        val accepted: AcceptedCallInvite = try {
            acceptCallInvite(capability, joinerIdentity)
        } catch (e: Exception) {
            return Result.Failed(message = "accept_call_invite: ${e.message ?: e.toString()}")
        }

        // ---- 7. Done -----------------------------------------
        return Result.Accepted(
            capabilityBytes = capabilityBytes,
            ownerAccountId = capability.ownerAccountId(),
            callId = capability.callId(),
            ownerFingerprint = capability.ownerFingerprint(),
            welcomeForOwner = accepted.welcomeForOwner(),
            callKeySeed = accepted.callKeySeed(),
        )
    }
}
