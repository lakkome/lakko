// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import android.content.Context
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.json.Json
import me.lakko.app.auth.MarkerStorage

/*
 * ADR-0101 section 8 - where a profile received in a conversation goes, and
 * the profile coming back after this account's key reset. Mirror of
 * `apps/web/src/profile/profile-signer.ts` and `returned-profile.ts`.
 *
 * Profile v6 is signed by its author's account identity key. A profile in a
 * conversation is normally the sender's own: it is filed under the sender's
 * key (resolved at receipt, C-68) only when its signature verifies under that
 * key. After this account's key reset, a peer that returns a conversation
 * also posts there the newest profile it holds for THIS person, signed by
 * this account's previous key: that one is offered as the own profile coming
 * back ([offerReturnedOwnProfile]), never filed under the peer. Anything else
 * is dropped: a profile signed by someone who did not send it names nobody.
 *
 * A returned own profile is kept only when it verifies under the account key
 * of BEFORE the reset ([me.lakko.app.auth.ResetContinuity.previousKeyB64]);
 * the newest stamp wins; the name and the photo references are adopted (the
 * `@username` belongs to the account and never left) and republished under
 * the new key, everywhere, as after a restore.
 */

/** One envelope held for a person, as received, with its stamp. */
public class HeldEnvelope(public val envelope: ByteArray, public val updatedAtMs: ULong)

/** What to do with one received profile. */
public enum class ProfileRoute {
    /** Filed under the sender. */
    FILE,

    /** This account's own profile coming back after its reset. */
    RETURNED_OWN,

    /** Dropped. */
    DROP,
}

/**
 * | verifies under the sender | verifies under this account's previous key | route        |
 * |---------------------------|--------------------------------------------|--------------|
 * | yes                       | -                                          | FILE         |
 * | no                        | yes                                        | RETURNED_OWN |
 * | no                        | no                                         | DROP         |
 */
public fun routeReceivedProfile(verifiesUnderSender: Boolean, verifiesUnderPreviousOwnKey: Boolean): ProfileRoute =
    when {
        verifiesUnderSender -> ProfileRoute.FILE
        verifiesUnderPreviousOwnKey -> ProfileRoute.RETURNED_OWN
        else -> ProfileRoute.DROP
    }

/** Whether [envelope] verifies under [keyB64] through [verify] (the core's
 *  `verifyProfileEnvelope`, which throws when it does not). Never throws. */
public fun profileVerifiesUnder(
    verify: (envelope: ByteArray, identityPk: ByteArray) -> Unit,
    envelope: ByteArray,
    keyB64: String?,
): Boolean {
    if (keyB64 == null) return false
    return try {
        verify(envelope, java.util.Base64.getDecoder().decode(keyB64))
        true
    } catch (_: Throwable) {
        false
    }
}

/** The core's verifier as [profileVerifiesUnder] takes it. */
public val NATIVE_PROFILE_VERIFIER: (ByteArray, ByteArray) -> Unit = { envelope, key ->
    uniffi.lakko_ffi_uniffi.verifyProfileEnvelope(envelope, key)
}

/** Whether a verified returned profile stamped [candidate] is adopted over a
 *  held stamp [held]: strictly newer, and not zero. */
public fun adoptsReturnedProfile(candidate: ULong, held: ULong): Boolean = candidate > held && candidate > 0uL

/**
 * The newest envelope held under any of a person's keys (a reset person's
 * retired keys; newest stamp wins, a tie keeps the first key's), or `null`.
 */
public fun newestHeldEnvelope(keysB64: List<String>, heldEnvelopeOf: (keyB64: String) -> HeldEnvelope?): ByteArray? {
    var best: HeldEnvelope? = null
    for (k in keysB64) {
        val held = heldEnvelopeOf(k) ?: continue
        if (best == null || held.updatedAtMs > best.updatedAtMs) best = held
    }
    return best?.envelope
}

/**
 * The successors the reset person's profile was already carried into, per
 * signed-in account (plain: group ids). At most 512 kept.
 */
public class CarriedProfileStore(private val storage: MarkerStorage) {
    private val json = Json { ignoreUnknownKeys = true }
    private val serializer = ListSerializer(String.serializer())

    private fun load(account: String): List<String> {
        val raw = runCatching { storage.get(KEY_PREFIX + account) }.getOrNull() ?: return emptyList()
        return runCatching { json.decodeFromString(serializer, raw) }.getOrElse { emptyList() }
    }

    /** Whether it was carried into [successorGroupIdHex]. */
    public fun carriedInto(account: String, successorGroupIdHex: String): Boolean =
        successorGroupIdHex in load(account)

    /** Record that it was. Never throws. */
    public fun markCarried(account: String, successorGroupIdHex: String) {
        val held = load(account).toMutableList()
        if (successorGroupIdHex !in held) held += successorGroupIdHex
        try {
            me.lakko.app.account.AccountStoreGate.write(account) {
                storage.set(KEY_PREFIX + account, json.encodeToString(serializer, held.takeLast(MAX)))
            }
        } catch (_: Throwable) {
            // Carried again: the receiver keeps only a strictly newer stamp.
        }
    }

    public companion object {
        public const val KEY_PREFIX: String = "continuity-profile-carried:"
        private const val MAX: Int = 512

        public fun onDevice(context: Context): CarriedProfileStore =
            CarriedProfileStore(me.lakko.app.auth.ResetReturnStorage.plain(context))
    }
}

/** ADR-0101 section 8 - what runs once a returned own profile was adopted:
 *  republish everywhere. Installed by `MainActivity`. */
public object ReturnedOwnProfile {
    @Volatile
    public var onAdopted: (() -> Unit)? = null
}

/**
 * Offer one returned own profile envelope for [accountId] (standard base64
 * [accountIdB64]). Adopted only when this device recorded a previous key for
 * the reset, the envelope verifies under it and is newer than the name held
 * ([adoptOwnProfileFromDevice]); then republished. Never throws.
 */
public fun offerReturnedOwnProfile(context: Context, accountId: ByteArray, envelope: ByteArray): Boolean {
    return try {
        val accountIdB64 = java.util.Base64.getEncoder().encodeToString(accountId)
        val previous = me.lakko.app.auth.ResetContinuityStore.onDevice(context).load(accountIdB64)?.previousKeyB64
            ?: return false
        val profile = uniffi.lakko_ffi_uniffi.verifyProfileEnvelope(envelope, java.util.Base64.getDecoder().decode(previous))
        val adopted = OwnProfileStore.update(context, accountId) { held ->
            if (!adoptsReturnedProfile(profile.updatedAtMs, (held?.nameUpdatedAtMs ?: 0L).toULong())) {
                null
            } else {
                adoptOwnProfileFromDevice(held, profile, accountId)
            }
        }
        if (adopted) ReturnedOwnProfile.onAdopted?.invoke()
        adopted
    } catch (_: Throwable) {
        false
    }
}

/**
 * A profile received in a conversation from [senderKeyB64], routed by its
 * signer: filed through [ingest] when the sender signed it, offered as this
 * account's own when its previous key did, dropped otherwise. [ingest] is
 * called synchronously. Never throws.
 */
public fun fileReceivedProfile(
    context: Context?,
    accountId: ByteArray?,
    senderKeyB64: String?,
    envelope: ByteArray?,
    ingest: () -> Unit,
    verify: (ByteArray, ByteArray) -> Unit = NATIVE_PROFILE_VERIFIER,
    previousOwnKeyB64: String? = context?.let { ctx ->
        accountId?.let { me.lakko.app.auth.ResetContinuityStore.onDevice(ctx).load(java.util.Base64.getEncoder().encodeToString(it))?.previousKeyB64 }
    },
) {
    if (envelope == null || senderKeyB64 == null) return
    try {
        when (
            routeReceivedProfile(
                verifiesUnderSender = profileVerifiesUnder(verify, envelope, senderKeyB64),
                verifiesUnderPreviousOwnKey = profileVerifiesUnder(verify, envelope, previousOwnKeyB64),
            )
        ) {
            ProfileRoute.FILE -> ingest()
            ProfileRoute.RETURNED_OWN -> if (context != null && accountId != null) {
                offerReturnedOwnProfile(context, accountId, envelope)
            }
            ProfileRoute.DROP -> Unit
        }
    } catch (_: Throwable) {
        // Dropped.
    }
}
