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
 * [me.lakko.app.push.PushDeviceTokens] (AES-256-GCM value encryption +
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
 * Value: `base64(secret) + ":" + ('1' | '0') + ":" + <decimal mint epoch>`
 * — the 32 raw secret bytes, the durable **distributed** flag
 * ([Entry.distributed]), and the ADR-0044 **mint epoch**
 * ([Entry.mintEpoch]). Base64 (`A-Za-z0-9+/=`) contains no `':'`, so
 * the suffixes are unambiguous. All three live in ONE row so a single
 * [put] writes them atomically — there is no torn "secret without a
 * flag" (or "secret without its stamp") state to reconcile.
 *
 * ## Why the mint epoch is STORED, not just carried on the wire
 *
 * ADR-0044. The v2 distribution envelope carries an 8-byte `mint_epoch`
 * the minter stamps from `Group.epoch()` at MINT time, and the receive
 * gate orders envelopes by the pair `(mintEpoch, secretBytes)`. That
 * order is only sound if the stamp **travels with the secret for that
 * secret's whole life**: every re-broadcast recomposes the envelope from
 * the STORED secret, so it must replay the epoch the secret was MINTED
 * under, never the current one. A re-stamp at send time would make a
 * stale secret out-rank the live one and reproduce the very F-A replay
 * (G-36 residual 9) the order exists to close — with the ordering gate
 * fully in place and working as specified.
 *
 * So the epoch is durable per-`(account, group)` state and lives here,
 * beside the flag. It is a monotone counter, not key material (the whole
 * row is encrypted at rest regardless), and ADR-0044 explicitly notes it
 * may be logged.
 *
 * The atomicity is load-bearing for a SECOND reason: because the flag
 * and the secret are replaced by one write, applying an accepted
 * DIFFERENT secret also clears a stale `pending` re-broadcast in the
 * same stroke — see
 * [me.lakko.app.push.GroupPushRouting.receiveGated].
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
 * The flag is per-GROUP, not per-member, so it is also what a failed
 * **redistribute** must rewind ([markPending]). `redistribute` fires
 * after an add-member commit to hand the joiner the secret; if that
 * send fails while the row is already flagged distributed (the mint
 * DID land), nothing would ever re-send it and the newly-added member
 * — the only one missing the secret — defers push permanently: the
 * same terminal bug, scoped to one member instead of the whole group.
 * Flipping the row back to pending puts it back in the sweep's hands.
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
 * A row written before the ADR-0044 epoch existed (one `':'` suffix,
 * the flag alone) is read as **`mintEpoch = 0`** ([LEGACY_MINT_EPOCH])
 * — the LOWEST position in the total order, so any authorised stamped
 * mint supersedes a legacy secret. That is the direction that
 * converges: the alternative (guessing "current") would let a legacy
 * secret out-rank a live rotation, which is the F-A shape itself.
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
     * ADR-0044 — the mint epoch a row written before the stamp existed
     * is read as: `0`, the LOWEST position in the
     * `(mintEpoch, secretBytes)` total order.
     *
     * Deliberately the floor rather than a guess at the current epoch:
     * any authorised stamped mint then supersedes a legacy secret,
     * which is the direction that converges. Guessing "current" would
     * let a legacy secret out-rank a live rotation.
     */
    public const val LEGACY_MINT_EPOCH: ULong = 0uL

    /**
     * One stored row: the 32-byte `push_routing_secret`, whether its
     * in-group distribution envelope has been confirmed on the wire,
     * and the MLS group epoch the secret was MINTED under.
     *
     * @property distributed `false` means a send of this secret's
     *   distribution envelope has NOT been confirmed on the wire — the
     *   owner minted + persisted it and the broadcast failed, or a
     *   post-add-member re-send failed on ANY holder's device — so
     *   [me.lakko.app.push.GroupPushRouting.ensure] must re-broadcast
     *   it (recomposing the envelope from [secret]). A legacy row
     *   written before the flag existed reads as `true`.
     * @property mintEpoch ADR-0044 — the `Group.epoch()` value the
     *   MINTER stamped this secret with, to be replayed VERBATIM by
     *   every re-broadcast of it (never re-stamped at send time — see
     *   the class doc). A row written before the stamp existed reads as
     *   [LEGACY_MINT_EPOCH].
     */
    public data class Entry(
        val secret: ByteArray,
        val distributed: Boolean,
        val mintEpoch: ULong,
    ) {
        override fun equals(other: Any?): Boolean =
            this === other ||
                (
                    other is Entry &&
                        distributed == other.distributed &&
                        mintEpoch == other.mintEpoch &&
                        secret.contentEquals(other.secret)
                    )

        override fun hashCode(): Int {
            var result = secret.contentHashCode()
            result = 31 * result + distributed.hashCode()
            result = 31 * result + mintEpoch.hashCode()
            return result
        }
    }

    /**
     * Persist [secret] (32 bytes) for `(accountIdB64, groupIdHex)`
     * together with its [distributed] flag and its ADR-0044
     * [mintEpoch]. Overwrites any prior row (all three atomically — one
     * value string).
     *
     * Callers: the owner mint persists `distributed = false` (nothing
     * is on the wire yet); a RECEIVED secret persists
     * `distributed = true` (it demonstrably travelled the journal to
     * get here, and a receiver never re-broadcasts).
     *
     * [mintEpoch] is the epoch the secret was MINTED under — on a mint /
     * rotation the `Group.epoch()` read at mint time; on a RECEIVE the
     * epoch the accepted envelope CARRIED, never the receiver's current
     * one. The stamp identifies the secret, not the moment it was
     * applied, so a later re-broadcast from this device replays the
     * minter's stamp rather than inventing a fresher one.
     *
     * @throws IllegalArgumentException when `secret.size != 32`.
     */
    public fun put(
        context: Context,
        accountIdB64: String,
        groupIdHex: String,
        secret: ByteArray,
        distributed: Boolean,
        mintEpoch: ULong,
    ) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            require(secret.size == SECRET_LENGTH) {
                "push_routing_secret must be $SECRET_LENGTH bytes (got ${secret.size})"
            }
            openPrefs(context)
                .edit()
                .putString(
                    rowKey(accountIdB64, groupIdHex),
                    encodeValue(secret, distributed, mintEpoch),
                )
                .apply()
        }
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
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            val current = entry(context, accountIdB64, groupIdHex) ?: return
            if (current.distributed) return
            // ADR-0044 — the stamp is REPLAYED, never refreshed: this write
            // flips a flag, it does not mint anything.
            put(
                context,
                accountIdB64,
                groupIdHex,
                current.secret,
                distributed = true,
                mintEpoch = current.mintEpoch,
            )
        }
    }

    /**
     * Flip the held row's flag back to `distributed = false` — called
     * when a distribution broadcast FAILED, so THIS device's next
     * [me.lakko.app.push.GroupPushRouting.ensure] re-sends the envelope.
     * The exact inverse of [markDistributed].
     *
     * "This device" is deliberate, and is why the rewind is worth
     * anything: ADR-0022 lets an ADMIN add members, so the device that
     * observed the failure is often not the owner, and
     * [me.lakko.app.push.GroupPushRouting.decideEnsureAction] therefore
     * arms the re-send for ANY holder. Re-sending a secret already held
     * needs no rank (the receive gate accepts byte-identical bytes from
     * any sender); only minting does.
     *
     * The caller is [me.lakko.app.push.GroupPushRouting.redistribute]:
     * the mint's own failure already leaves the row pending, but a failed
     * post-add-member re-send on a row that was ALREADY distributed had
     * nothing to rewind, so the joiner never learned the secret.
     *
     * No-op when no row is held, when the row is malformed, or when it is
     * already pending (so a repeat mark writes nothing). Rewrites the
     * secret and the flag in ONE value string, exactly like [put] — there
     * is no window in which the flag is lost while the secret survives
     * (such a torn row would read back as legacy, i.e. distributed, which
     * is precisely the bug).
     */
    public fun markPending(
        context: Context,
        accountIdB64: String,
        groupIdHex: String,
    ) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            val current = entry(context, accountIdB64, groupIdHex) ?: return
            if (!current.distributed) return
            // ADR-0044 — the stamp is REPLAYED, never refreshed (see
            // [markDistributed]).
            put(
                context,
                accountIdB64,
                groupIdHex,
                current.secret,
                distributed = false,
                mintEpoch = current.mintEpoch,
            )
        }
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

    /** Separates the base64 secret from the flag character, and the flag
     *  from the ADR-0044 mint epoch, in a row value. Absent from the
     *  base64 alphabet (`A-Za-z0-9+/=`) and from a decimal integer, so
     *  the split is unambiguous. */
    private const val FIELD_SEPARATOR = ':'

    private const val FLAG_DISTRIBUTED = '1'
    private const val FLAG_PENDING = '0'

    /** A row value split into its parts, before the base64 decode. */
    public data class ParsedValue(
        val encodedSecret: String,
        val distributed: Boolean,
        val mintEpoch: ULong,
    )

    /**
     * Render a row value: the already-base64'd secret, the flag, and the
     * ADR-0044 mint epoch as an unsigned decimal. PURE — the inverse of
     * [parseValue].
     */
    public fun formatValue(
        encodedSecret: String,
        distributed: Boolean,
        mintEpoch: ULong,
    ): String =
        encodedSecret +
            FIELD_SEPARATOR + (if (distributed) FLAG_DISTRIBUTED else FLAG_PENDING) +
            FIELD_SEPARATOR + mintEpoch.toString()

    /**
     * Split a stored row value into its base64 secret + distributed flag
     * + ADR-0044 mint epoch. PURE — no Android framework, so the two
     * LEGACY-row rules below are host-JVM unit-testable.
     *
     * Three accepted shapes, one per schema generation:
     *
     * | shape | written by | read as |
     * |---|---|---|
     * | `body` | pre-ADR-0042-flag | `distributed = true`, `mintEpoch = 0` |
     * | `body:flag` | ADR-0042 / ADR-0043 | flag, `mintEpoch = 0` |
     * | `body:flag:epoch` | ADR-0044 | flag, epoch |
     *
     *   - `null` in → `null` out (no row held).
     *   - **No suffix → a LEGACY row → `distributed = true`.** Rows
     *     written before the flag existed belong to groups that are
     *     provisioned and working; reading them as pending would
     *     re-broadcast for every group on the first launch after an
     *     upgrade, for no benefit.
     *   - **No epoch field → `mintEpoch = 0`** ([LEGACY_MINT_EPOCH]),
     *     the lowest position in the ADR-0044 order, so any authorised
     *     stamped mint supersedes it.
     *   - Anything else — an unrecognised flag character, a
     *     non-decimal / out-of-`u64`-range epoch, or a fourth field →
     *     `null` (malformed; the same fail-closed posture as an
     *     undecodable base64 body). This client is the only writer, so
     *     that is corruption, not a format to accept.
     */
    public fun parseValue(raw: String?): ParsedValue? {
        val value = raw ?: return null
        val parts = value.split(FIELD_SEPARATOR)
        return when (parts.size) {
            1 -> ParsedValue(parts[0], distributed = true, mintEpoch = LEGACY_MINT_EPOCH)
            2 -> ParsedValue(
                parts[0],
                distributed = parseFlag(parts[1]) ?: return null,
                mintEpoch = LEGACY_MINT_EPOCH,
            )
            3 -> ParsedValue(
                parts[0],
                distributed = parseFlag(parts[1]) ?: return null,
                // `toULongOrNull` rejects a sign, a non-digit, and any
                // value past u64::MAX — all corruption, all fail-closed.
                mintEpoch = parts[2].toULongOrNull() ?: return null,
            )
            else -> null
        }
    }

    /** One flag character, or `null` when it is neither. */
    private fun parseFlag(raw: String): Boolean? = when (raw) {
        FLAG_DISTRIBUTED.toString() -> true
        FLAG_PENDING.toString() -> false
        else -> null
    }

    // -----------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------

    private fun encodeValue(
        secret: ByteArray,
        distributed: Boolean,
        mintEpoch: ULong,
    ): String = formatValue(Base64.encodeToString(secret, Base64.NO_WRAP), distributed, mintEpoch)

    private fun decodeValue(raw: String?): Entry? {
        val parsed = parseValue(raw) ?: return null
        val decoded: ByteArray = try {
            Base64.decode(parsed.encodedSecret, Base64.NO_WRAP)
        } catch (_: IllegalArgumentException) {
            return null
        }
        return if (decoded.size == SECRET_LENGTH) {
            Entry(decoded, parsed.distributed, parsed.mintEpoch)
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
