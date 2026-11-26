// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import kotlinx.serialization.builtins.serializer

/*
 * ADR-0073 R.4 and ADR-0101 sections 1 to 6 - reaching an account after its
 * key reset, the PEER side. Mirror of `apps/web/src/reach/continuity.ts`.
 *
 * A reset changes every reach key and the mailbox (they derive from the
 * account seed), and the new identity knows none of its former contacts, so
 * nobody holds a grant for its new mailbox. ADR-0101: the resetting device
 * announces a second mailbox, the CONTINUITY mailbox, in a 105-byte binding
 * signed by its new identity key, served beside its identity bundle for a
 * window; it registers on that mailbox only the continuity reach key
 * `continuityReachSeed(account id, key generation)`, which a peer that
 * ACCEPTED the reset (ADR-0073 R.2) derives too. So the peer:
 *
 *   1. verifies the binding under the attested new key it pinned, at its own
 *      clock ([peerContinuityTarget]), and keeps the verified mailbox key with
 *      its expiry ([ContinuityTargetStore]);
 *   2. writes the successor's Welcome (and anything else it owes the person)
 *      to `mailboxAddress(continuity mailbox key)`, sealed to that key and
 *      signed with the continuity reach key, after every ordinary grant it
 *      holds ([continuityDest]);
 *   3. stops at expiry: a binding past it is never used.
 */

/** How long the continuity key stays usable, from the reset's `signed_at`
 *  (decision D2; chosen, not measured). Equal to the core's
 *  `continuityWindowMs()`. */
public const val CONTINUITY_KEY_TTL_MS: Long = 30L * 24 * 60 * 60 * 1000

/** Whether a PEER would still sign a write with the continuity key. */
public fun continuityKeyUsable(signedAtMs: Long, nowMs: Long): Boolean = nowMs < signedAtMs + CONTINUITY_KEY_TTL_MS

/** The binding's fixed length: `version(1) || mailbox_pk(32) || expires_at_ms(8 BE) || sig(64)`. */
public const val CONTINUITY_BINDING_LEN: Int = 105

/** The binding's `expires_at_ms`, read off the layout (NOT a verification),
 *  or `null` for a wrong length or a value past [Long.MAX_VALUE]. */
public fun continuityBindingExpiryMs(binding: ByteArray): Long? {
    if (binding.size != CONTINUITY_BINDING_LEN) return null
    var v = 0L
    for (i in 33 until 41) v = (v shl 8) or (binding[i].toLong() and 0xff)
    return v.takeIf { it >= 0L }
}

/** A verified continuity mailbox. */
@kotlinx.serialization.Serializable
public data class ContinuityTarget(
    /** Standard base64 of the 32-byte continuity mailbox public key. */
    val mailboxPkB64: String,
    /** Unix ms; the target is not used at or after it. */
    val expiresAtMs: Long,
)

/**
 * The continuity mailbox a served binding names, verified under the attested
 * new key [attestedKey] at [nowMs], or `null` (absent, malformed, signed by
 * another key, expired). [verify] is the core's `verifyContinuityBinding`:
 * returns the 32-byte mailbox key, throws on any refusal. Never throws.
 */
public fun peerContinuityTarget(
    bindingB64: String?,
    attestedKey: ByteArray,
    nowMs: Long,
    verify: (binding: ByteArray, identityPk: ByteArray, nowMs: Long) -> ByteArray,
): ContinuityTarget? {
    if (bindingB64.isNullOrEmpty()) return null
    val binding = try {
        java.util.Base64.getDecoder().decode(bindingB64)
    } catch (_: IllegalArgumentException) {
        return null
    }
    val expiresAtMs = continuityBindingExpiryMs(binding) ?: return null
    if (expiresAtMs <= nowMs) return null
    return try {
        val pk = verify(binding, attestedKey, nowMs)
        if (pk.size != 32) null else ContinuityTarget(java.util.Base64.getEncoder().encodeToString(pk), expiresAtMs)
    } catch (_: Throwable) {
        null
    }
}

/** Whether a held target may still be written to at [nowMs]. */
public fun continuityTargetLive(target: ContinuityTarget?, nowMs: Long): Boolean =
    target != null && nowMs < target.expiresAtMs

/**
 * ADR-0101 section 4 - whether the continuity mailbox reaches the new
 * identity: a verified, unexpired binding AND the accepted reset's
 * continuity key still usable (two windows, two clocks: the binding's own
 * expiry, the attestation's `signed_at`).
 */
public fun continuityReachable(target: ContinuityTarget?, resetSignedAtMs: Long?, nowMs: Long): Boolean =
    resetSignedAtMs != null && continuityTargetLive(target, nowMs) && continuityKeyUsable(resetSignedAtMs, nowMs)

/**
 * The verified continuity mailboxes this account keeps for its reset
 * contacts, per signed-in account: new account key (standard base64) ->
 * [ContinuityTarget]. Public values only; expired entries are dropped on
 * every write.
 */
public class ContinuityTargetStore(private val storage: me.lakko.app.auth.MarkerStorage) {
    private val json = kotlinx.serialization.json.Json { ignoreUnknownKeys = true }
    private val serializer = kotlinx.serialization.builtins.MapSerializer(
        String.serializer(),
        ContinuityTarget.serializer(),
    )

    private fun loadAll(account: String): MutableMap<String, ContinuityTarget> {
        val raw = runCatching { storage.get(KEY_PREFIX + account) }.getOrNull() ?: return mutableMapOf()
        return runCatching { json.decodeFromString(serializer, raw).toMutableMap() }.getOrElse { mutableMapOf() }
    }

    /** Keep (or, with `null`, forget) the target of [newKeyB64]. Never throws. */
    public fun remember(account: String, newKeyB64: String, target: ContinuityTarget?, nowMs: Long) {
        val all = loadAll(account)
        all.entries.removeAll { it.value.expiresAtMs <= nowMs }
        if (target == null) all.remove(newKeyB64) else all[newKeyB64] = target
        try {
            me.lakko.app.account.AccountStoreGate.write(account) {
                storage.set(KEY_PREFIX + account, json.encodeToString(serializer, all))
            }
        } catch (_: Throwable) {
            // Storage blocked: the next reachability check fetches it again.
        }
    }

    /** The live target held for [newKeyB64], or `null`. */
    public fun targetOf(account: String, newKeyB64: String, nowMs: Long): ContinuityTarget? =
        loadAll(account)[newKeyB64]?.takeIf { continuityTargetLive(it, nowMs) }

    /** Wipe. */
    public fun clear(account: String) {
        runCatching { storage.remove(KEY_PREFIX + account) }
    }

    public companion object {
        public const val KEY_PREFIX: String = "continuity-targets:"

        public fun onDevice(context: android.content.Context): ContinuityTargetStore =
            ContinuityTargetStore(me.lakko.app.auth.ResetReturnStorage.plain(context))
    }
}

/**
 * ADR-0101 section 4 - where a write to the reset person [currentKey] goes
 * through the continuity mailbox, or `null`: only when this device ACCEPTED
 * the reset that pinned it to [currentKey] ([accepted]), the reset's
 * continuity key is still usable, and a live verified target is kept
 * ([targetPk]). [signerOf] turns the continuity reach seed into the signer
 * (the seed is wiped after). Never throws.
 */
public fun continuityDest(
    accepted: me.lakko.app.contacts.AcceptedResetOfKey?,
    targetPk: ByteArray?,
    nowMs: Long,
    seedOf: (accountId: ByteArray, generation: Long) -> ByteArray,
    signerOf: (seed: ByteArray) -> me.lakko.app.ds.DsReachAuth,
): me.lakko.app.contacts.MailboxDest? {
    val a = accepted ?: return null
    val pk = targetPk ?: return null
    if (pk.size != 32 || !continuityKeyUsable(a.signedAtMs, nowMs)) return null
    return try {
        val accountId = java.util.Base64.getDecoder().decode(a.accountIdB64)
        val seed = seedOf(accountId, a.keyGeneration)
        try {
            me.lakko.app.contacts.MailboxDest(pk.copyOf(), signerOf(seed))
        } finally {
            seed.fill(0)
        }
    } catch (_: Throwable) {
        null
    }
}

/**
 * ADR-0101 section 4 - fetch a reset person's identity bundle and keep the
 * continuity mailbox its binding names, verified under the key this device
 * ACCEPTED for the account ([attestedKeyB64], the accepted reset's pin). The
 * one fetch the peer side runs before writing to a continuity mailbox. A
 * failed fetch keeps what is held; a bundle naming another key forgets it.
 * Mirror of `apps/web/src/reach/continuity-target.ts`. Never throws but for
 * cancellation.
 */
public suspend fun refreshContinuityTarget(
    fetch: suspend (accountId: ByteArray) -> me.lakko.app.discovery.IdentityFetchResponse?,
    store: ContinuityTargetStore,
    ownAccountIdB64: String,
    accountIdB64: String,
    attestedKeyB64: String,
    nowMs: Long,
    verify: (binding: ByteArray, identityPk: ByteArray, nowMs: Long) -> ByteArray = { b, k, n ->
        uniffi.lakko_ffi_uniffi.verifyContinuityBinding(b, k, n.toULong())
    },
): ContinuityTarget? {
    return try {
        val bundle = fetch(java.util.Base64.getDecoder().decode(accountIdB64))
        if (bundle == null || bundle.signature_pubkey != attestedKeyB64) {
            store.remember(ownAccountIdB64, attestedKeyB64, null, nowMs)
            return null
        }
        val target = peerContinuityTarget(
            bundle.continuity_binding_b64,
            java.util.Base64.getDecoder().decode(attestedKeyB64),
            nowMs,
            verify,
        )
        store.remember(ownAccountIdB64, attestedKeyB64, target, nowMs)
        target
    } catch (cancel: kotlinx.coroutines.CancellationException) {
        throw cancel
    } catch (_: Throwable) {
        store.targetOf(ownAccountIdB64, attestedKeyB64, nowMs)
    }
}

/** How a successor's Welcomes went ([deliverSuccessorWelcomes]). */
public enum class WelcomeDelivery {
    /** Every person's mailbox took the Welcome. */
    DELIVERED,

    /** At least one person's mailbox could not be written: no verified
     *  binding is held for them, or no grant they accept (the new identity of
     *  an accepted reset whose continuity mailbox is not known or no longer
     *  live, ADR-0101). The Welcome stays owed. */
    UNREACHABLE,
}

/**
 * Deliver a successor group's Welcome to each person in [targets], once per
 * mailbox. Every mailbox is named first: when [mailboxOf] cannot name one
 * (no verified binding is held for that person) NOTHING is written and the
 * result is [WelcomeDelivery.UNREACHABLE]; nothing is ever addressed to a
 * mailbox derived from the account id or the identity key. A write that
 * [isUnreachable] classifies (no grant accepted) stops the loop with the same
 * result; any other failure propagates. Never "delivered" for a person
 * nobody wrote to (2026-11-25: a successor counted delivered with its
 * Welcome skipped became the conversation and every message sent there was
 * read by nobody). Idempotent on the wire: a second delivery is a duplicate
 * the recipient's join ignores.
 */
public suspend fun deliverSuccessorWelcomes(
    targets: List<ByteArray>,
    mailboxOf: (ByteArray) -> ByteArray?,
    publish: suspend (mailbox: ByteArray) -> Unit,
    isUnreachable: (Throwable) -> Boolean = { it is me.lakko.app.contacts.MailboxUnknown },
): WelcomeDelivery {
    val mailboxes = ArrayList<ByteArray>(targets.size)
    for (accountKey in targets) {
        val mailbox = mailboxOf(accountKey)
        if (mailbox == null) return WelcomeDelivery.UNREACHABLE
        mailboxes += mailbox
    }
    val written = HashSet<String>()
    for (mailbox in mailboxes) {
        if (!written.add(mailbox.joinToString("") { "%02x".format(it) })) continue
        try {
            publish(mailbox)
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (cause: Throwable) {
            if (isUnreachable(cause)) return WelcomeDelivery.UNREACHABLE
            throw cause
        }
    }
    return WelcomeDelivery.DELIVERED
}
