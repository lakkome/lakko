// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import me.lakko.app.transparency.LogVerifyStatus

/*
 * ADR-0101 sections 1 to 5 - the RESET side of reaching an account after its
 * key reset: what the resetting device keeps so its conversations, groups,
 * channels, profile and history come back by themselves. Mirror of
 * `apps/web/src/auth/reset-continuity.ts`.
 *
 * After the attested publish ([KeyResetOwed]) the reset owes one more step,
 * the announcement, kept in its own record ([ResetContinuity.announced]) so a
 * failure here never withholds the new words the way an owed publish does;
 * it is retried at every session landing:
 *
 *   1. claim the CONTINUITY mailbox (`MailboxKey.continuityFromAccountIdentity`,
 *      claimed with `dsContinuityMailboxSigner`), never the real one;
 *   2. register on it the one reach key a peer that accepted the reset can
 *      derive, `continuityReachSeed(account id, key generation)`, expiring
 *      with the window;
 *   3. sign the binding and upload it to discovery beside the bundle.
 *
 * The window is counted from the attestation's `signed_at`, the instant every
 * peer counts the continuity key's usability from, so the reset device and
 * its peers agree on when it ends.
 *
 * ## The previous own key
 *
 * A returned profile or channel is accepted silently only when it carries a
 * signature by the account key the person had BEFORE the reset. The KT log
 * serves an account's latest entry only, so the previous key is read BEFORE
 * the reset is sent ([previousKeyStanding]); a recall knows it outright (the
 * device held the old seed). When it cannot be established nothing returned
 * is accepted silently.
 *
 * Public values only (a key, an expiry); no seed is stored here.
 */

/** The window (ADR-0101 section 2; chosen, not measured). Equal to the core's
 *  `continuityWindowMs()`. */
public const val RESET_CONTINUITY_WINDOW_MS: Long = 30L * 24 * 60 * 60 * 1000

/** The binding's expiry for a reset signed at [signedAtMs]. */
public fun resetContinuityExpiryMs(signedAtMs: Long): Long = signedAtMs + RESET_CONTINUITY_WINDOW_MS

/** The record one account keeps on the resetting device. */
@Serializable
public data class ResetContinuity(
    public val v: Int = 1,
    /** Unix ms: the continuity mailbox is read strictly before it. */
    public val expiresAtMs: Long,
    /** Standard base64 of the 32-byte account key of before the reset, or
     *  `null` when it could not be established. */
    public val previousKeyB64: String? = null,
    /** The generation the reset moved the account to (decimal string). */
    public val keyGeneration: String,
    /** The announcement (claim, reach key, binding) landed. */
    public val announced: Boolean = false,
) {
    /** The record with the announcement marked done. */
    public fun withAnnounced(): ResetContinuity = copy(announced = true)
}

/** Whether the announcement is still owed at [nowMs] (never after the
 *  window: a binding for a past window is refused by everyone). */
public fun announcementOwed(record: ResetContinuity?, nowMs: Long): Boolean =
    record != null && !record.announced && nowMs < record.expiresAtMs

/** Whether the continuity mailbox is read at [nowMs]. */
public fun continuityMailboxReadable(expiresAtMs: Long?, nowMs: Long): Boolean =
    expiresAtMs != null && nowMs < expiresAtMs

/**
 * The previous own key a device may rely on, from what it read before the
 * reset was sent:
 *
 * | directory served | log verdict  | previous key           |
 * |------------------|--------------|------------------------|
 * | none             | -            | none                   |
 * | key K            | CONFIRMED    | K                      |
 * | key K            | OFF          | K (no log pinned)      |
 * | key K            | anything else| none                   |
 * | key K == new key | -            | none (nothing changed) |
 */
public fun previousKeyStanding(servedKeyB64: String?, logStatus: LogVerifyStatus?, newKeyB64: String): String? {
    if (servedKeyB64 == null || servedKeyB64 == newKeyB64) return null
    return if (logStatus == LogVerifyStatus.CONFIRMED || logStatus == LogVerifyStatus.OFF) servedKeyB64 else null
}

/** The per-account row of [ResetContinuity] (plain: public values only). */
public class ResetContinuityStore(private val storage: MarkerStorage) {

    /** The record, or `null` (absent or damaged). */
    public fun load(accountIdB64: String): ResetContinuity? {
        val raw = runCatching { storage.get(KEY_PREFIX + accountIdB64) }.getOrNull() ?: return null
        val parsed = runCatching { json.decodeFromString(ResetContinuity.serializer(), raw) }.getOrNull() ?: return null
        if (parsed.v != 1 || !parsed.keyGeneration.matches(DECIMAL)) return null
        return parsed
    }

    /** Write (replace) the record. Never throws. */
    public fun save(accountIdB64: String, record: ResetContinuity) {
        try {
            me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
                storage.set(KEY_PREFIX + accountIdB64, json.encodeToString(ResetContinuity.serializer(), record))
            }
        } catch (_: Throwable) {
            // Storage blocked: the read loop then asks the directory instead.
        }
    }

    /** Forget it. */
    public fun clear(accountIdB64: String) {
        runCatching { storage.remove(KEY_PREFIX + accountIdB64) }
    }

    public companion object {
        public const val KEY_PREFIX: String = "reset-continuity:"
        private val DECIMAL = Regex("^[0-9]+$")
        private val json = Json { ignoreUnknownKeys = true; encodeDefaults = true }

        /** The device's store. */
        public fun onDevice(context: android.content.Context): ResetContinuityStore =
            ResetContinuityStore(ResetReturnStorage.plain(context))
    }
}
