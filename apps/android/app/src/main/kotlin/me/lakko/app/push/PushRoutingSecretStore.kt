// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import android.content.Context
import android.util.Base64
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

/**
 * Encrypted per-`(account, group)` `push_routing_secret` store
 * (ADR-0042 — opaque per-group push routing id).
 *
 * Mirror of:
 *
 *   * `apps/web/src/push/group-routing-store.ts` (the `secrets`
 *     IndexedDB object store)
 *   * `apps/ios/Lakko/Push/PushRoutingSecretStore.swift`
 *
 * ## What's stored
 *
 * The 32-byte `push_routing_secret` — E2EE key material minted
 * ONCE by the ADR-0022 group OWNER and distributed in-band over an
 * MLS application message. It derives the opaque `push_gid`
 * ([uniffi.lakko_ffi_uniffi.derivePushGid]) the client subscribes /
 * notifies under, so it is sensitive and stored **encrypted at
 * rest** — the SAME [EncryptedSharedPreferences] posture as
 * [me.lakko.app.auth.AccountIdentitySeedStore] /
 * [me.lakko.app.push.DevicePushId] (AES-256-GCM value encryption +
 * AES-256-SIV key encryption, master key hardware-backed in the
 * Android Keystore, invariant #7). Foreground-only — the
 * FirebaseMessagingService silent-push path never needs the raw
 * secret (it reads the plaintext reverse index in
 * [PushRouteStore]).
 *
 * ## Storage layout
 *
 * `EncryptedSharedPreferences` named `"lakko-push-routing-secret"`.
 * Keys: `"push-routing-secret-v1:" + accountIdB64 + NUL + groupIdHex`,
 * where NUL is the `0x00` byte (written as the Kotlin escape
 * `\u0000` — see [ROW_SEPARATOR]; a RAW NUL in the source would make
 * git classify this file as binary and render no diff).
 * `accountIdB64` is standard base64 (`A-Za-z0-9+/=`) and
 * `groupIdHex` is lowercase hex — neither alphabet contains a NUL,
 * so the single-NUL separator between them is unambiguous (and lets
 * [clearForAccount] match every group of an account by the
 * `prefix + accountIdB64 + NUL` key prefix).
 *
 * Value: `base64(secret) + ":" + ('1' | '0')` — the 32 raw secret
 * bytes plus the durable **distributed** flag ([Entry.distributed]).
 * Base64 (`A-Za-z0-9+/=`) contains no `':'`, so the suffix is
 * unambiguous. Flag and secret live in ONE row so a single [put]
 * writes both atomically — there is no torn "secret without a flag"
 * state to reconcile.
 *
 * ## Why a distributed flag
 *
 * The owner mints the secret, persists it, then broadcasts the
 * distribution envelope to the group. Persist-before-broadcast is
 * deliberate (minting twice would DIVERGE the group's `push_gid`,
 * which is far worse than broadcasting twice) — but it used to mean a
 * failed broadcast left the secret stored and the owner's mint guard
 * permanently satisfied, so the envelope was never re-sent and every
 * OTHER member deferred forever: the whole group silently lost push,
 * for good. Recording whether the broadcast was confirmed on the wire
 * is what makes that failure recoverable —
 * [me.lakko.app.push.GroupPushRouting.ensure] re-broadcasts a row
 * flagged `distributed = false`.
 *
 * Only a boolean is stored — deliberately NOT an ADR-0040-style
 * outbox holding the envelope bytes. A commit's wire bytes are
 * unrecoverable once lost, so that outbox had to persist them; the
 * distribution envelope is **recomputable at any time** from the
 * stored secret (`composePushRoutingSecret`), so the only thing that
 * cannot be reconstructed is whether it landed.
 *
 * ## Legacy rows
 *
 * A row written before the flag existed carries no `':'` suffix and
 * is read as **`distributed = true`**. Those groups are provisioned
 * and working today; treating them as pending would re-broadcast for
 * every group on the first launch after an upgrade, for no benefit.
 *
 * ## Invariants
 *
 * - #2 / #5 — the raw secret never leaves the device; push-gw only
 *   ever sees the one-way-derived opaque `push_gid`, never this
 *   secret nor the DS `group_id`.
 * - #7 — wrapped under the hardware-backed device master key.
 */
public object PushRoutingSecretStore {

    private const val PREFS_NAME = "lakko-push-routing-secret"
    private const val KEY_PREFIX = "push-routing-secret-v1:"

    /** `push_routing_secret` length pinned by the Rust
     *  `lakko_contacts::push_routing::PUSH_ROUTING_SECRET_LEN`. */
    public const val SECRET_LENGTH: Int = 32

    /**
     * One stored row: the 32-byte `push_routing_secret` plus whether
     * its in-group distribution envelope has been confirmed on the
     * wire.
     *
     * @property distributed `false` means the owner minted + persisted
     *   this secret but the broadcast has NOT yet succeeded, so
     *   [me.lakko.app.push.GroupPushRouting.ensure] must re-broadcast
     *   it (recomposing the envelope from [secret]). A legacy row
     *   written before the flag existed reads as `true`.
     */
    public data class Entry(val secret: ByteArray, val distributed: Boolean) {
        override fun equals(other: Any?): Boolean =
            this === other ||
                (
                    other is Entry &&
                        distributed == other.distributed &&
                        secret.contentEquals(other.secret)
                    )

        override fun hashCode(): Int = 31 * secret.contentHashCode() + distributed.hashCode()
    }

    /**
     * Persist [secret] (32 bytes) for `(accountIdB64, groupIdHex)`
     * together with its [distributed] flag. Overwrites any prior row
     * (secret AND flag, atomically — one value string).
     *
     * Callers: the owner mint persists `distributed = false` (nothing
     * is on the wire yet); a RECEIVED secret persists
     * `distributed = true` (it demonstrably travelled the journal to
     * get here, and a receiver never re-broadcasts).
     *
     * @throws IllegalArgumentException when `secret.size != 32`.
     */
    public fun put(
        context: Context,
        accountIdB64: String,
        groupIdHex: String,
        secret: ByteArray,
        distributed: Boolean,
    ) {
        require(secret.size == SECRET_LENGTH) {
            "push_routing_secret must be $SECRET_LENGTH bytes (got ${secret.size})"
        }
        openPrefs(context)
            .edit()
            .putString(rowKey(accountIdB64, groupIdHex), encodeValue(secret, distributed))
            .apply()
    }

    /**
     * Read the whole row for `(accountIdB64, groupIdHex)`, or `null`
     * when this device holds none (a member that has not yet received
     * the distribution envelope) or the row is malformed.
     */
    public fun entry(
        context: Context,
        accountIdB64: String,
        groupIdHex: String,
    ): Entry? = decodeValue(
        openPrefs(context).getString(rowKey(accountIdB64, groupIdHex), null),
    )

    /**
     * Read the secret for `(accountIdB64, groupIdHex)`, or `null`
     * when this device holds none (a member that has not yet
     * received the distribution envelope) or the row is malformed.
     */
    public fun get(
        context: Context,
        accountIdB64: String,
        groupIdHex: String,
    ): ByteArray? = entry(context, accountIdB64, groupIdHex)?.secret

    /**
     * Has the distribution envelope for the held secret been confirmed
     * on the wire?
     *
     * An ABSENT (or malformed) row answers `true`: there is nothing
     * pending to re-broadcast, and the owner-mint branch — not the
     * re-broadcast branch — is what handles "no secret". A legacy row
     * with no flag suffix likewise answers `true`.
     */
    public fun isDistributed(
        context: Context,
        accountIdB64: String,
        groupIdHex: String,
    ): Boolean = entry(context, accountIdB64, groupIdHex)?.distributed ?: true

    /**
     * Flip the held row's flag to `distributed = true` — called only
     * AFTER a broadcast has actually succeeded. No-op when no row is
     * held, when the row is malformed, or when it is already flagged
     * (so a repeat mark writes nothing).
     */
    public fun markDistributed(
        context: Context,
        accountIdB64: String,
        groupIdHex: String,
    ) {
        val current = entry(context, accountIdB64, groupIdHex) ?: return
        if (current.distributed) return
        put(context, accountIdB64, groupIdHex, current.secret, distributed = true)
    }

    /**
     * Cheap existence check — does this device already hold a
     * `push_routing_secret` for `(accountIdB64, groupIdHex)`? The
     * guard the owner-mint funnel uses to avoid re-minting a secret
     * it already provisioned (a second random mint would diverge).
     */
    public fun has(
        context: Context,
        accountIdB64: String,
        groupIdHex: String,
    ): Boolean = openPrefs(context).contains(rowKey(accountIdB64, groupIdHex))

    /**
     * Erase EVERY secret held for [accountIdB64] (all groups).
     * Called from `ChatRepository.forgetDevice` alongside the other
     * per-account wipes. No-op when absent; best-effort posture
     * (any per-key remove failure is swallowed by the caller).
     */
    public fun clearForAccount(context: Context, accountIdB64: String) {
        val prefs = openPrefs(context)
        val prefix = KEY_PREFIX + accountIdB64 + ROW_SEPARATOR
        val editor = prefs.edit()
        for (key in prefs.all.keys) {
            if (key.startsWith(prefix)) editor.remove(key)
        }
        editor.apply()
    }

    // -----------------------------------------------------------------
    // Row-value codec (PURE — no Android framework, host-JVM testable)
    // -----------------------------------------------------------------

    /** Separates the base64 secret from the flag character in a row
     *  value. Absent from the base64 alphabet (`A-Za-z0-9+/=`), so the
     *  split is unambiguous. */
    private const val FLAG_SEPARATOR = ':'

    private const val FLAG_DISTRIBUTED = '1'
    private const val FLAG_PENDING = '0'

    /** A row value split into its two parts, before the base64 decode. */
    public data class ParsedValue(val encodedSecret: String, val distributed: Boolean)

    /**
     * Render a row value: the already-base64'd secret plus the flag
     * suffix. PURE — the inverse of [parseValue].
     */
    public fun formatValue(encodedSecret: String, distributed: Boolean): String =
        encodedSecret + FLAG_SEPARATOR + (if (distributed) FLAG_DISTRIBUTED else FLAG_PENDING)

    /**
     * Split a stored row value into its base64 secret + distributed
     * flag. PURE — no Android framework, so the LEGACY-row rule below
     * is host-JVM unit-testable.
     *
     *   - `null` in → `null` out (no row held).
     *   - **No `':'` suffix → a LEGACY row → `distributed = true`.**
     *     Rows written before the flag existed belong to groups that
     *     are provisioned and working; reading them as pending would
     *     re-broadcast for every group on the first launch after an
     *     upgrade, for no benefit.
     *   - A `':'` suffix that is not exactly one recognised flag
     *     character → `null` (malformed; the same fail-closed posture
     *     as an undecodable base64 body). This client is the only
     *     writer, so that is corruption, not a format to accept.
     */
    public fun parseValue(raw: String?): ParsedValue? {
        val value = raw ?: return null
        val sep = value.indexOf(FLAG_SEPARATOR)
        if (sep < 0) return ParsedValue(value, distributed = true)
        val distributed = when (value.substring(sep + 1)) {
            FLAG_DISTRIBUTED.toString() -> true
            FLAG_PENDING.toString() -> false
            else -> return null
        }
        return ParsedValue(value.substring(0, sep), distributed)
    }

    // -----------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------

    private fun encodeValue(secret: ByteArray, distributed: Boolean): String =
        formatValue(Base64.encodeToString(secret, Base64.NO_WRAP), distributed)

    private fun decodeValue(raw: String?): Entry? {
        val parsed = parseValue(raw) ?: return null
        val decoded: ByteArray = try {
            Base64.decode(parsed.encodedSecret, Base64.NO_WRAP)
        } catch (_: IllegalArgumentException) {
            return null
        }
        return if (decoded.size == SECRET_LENGTH) {
            Entry(decoded, parsed.distributed)
        } else {
            null
        }
    }

    private const val ROW_SEPARATOR = "\u0000"

    private fun rowKey(accountIdB64: String, groupIdHex: String): String =
        KEY_PREFIX + accountIdB64 + ROW_SEPARATOR + groupIdHex

    private fun openPrefs(context: Context): android.content.SharedPreferences {
        val master = MasterKey.Builder(context.applicationContext)
            .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
            .build()
        return EncryptedSharedPreferences.create(
            context.applicationContext,
            PREFS_NAME,
            master,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    }
}
