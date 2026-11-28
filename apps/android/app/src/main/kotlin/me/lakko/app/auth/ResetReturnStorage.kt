// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context

/**
 * ADR-0101 - where the per-account state of reaching an account after a key
 * reset lives on this device: one plain preferences file for public values
 * (a window, a public key, a cursor, what was sent) and one encrypted file
 * for rows that carry secrets (a held channel return carries epoch seeds and
 * an invitation card with a link secret; a returned invitation carries a
 * link secret). Every row is keyed `<prefix><account id>`; Forget device
 * removes every row of the account ([wipe]). Mirror of the web client's
 * `lakko:*` localStorage keys listed in `adr0101-web-port-notes.md`.
 */
public object ResetReturnStorage {
    /** The plain file. */
    public const val PLAIN_PREFS: String = "lakko-reset-return"

    /** The encrypted file. */
    public const val SECRET_PREFS: String = "lakko-reset-return-secret"

    /** Every prefix of the plain file. */
    public val PLAIN_PREFIXES: List<String> = listOf(
        ResetContinuityStore.KEY_PREFIX,
        me.lakko.app.reach.ContinuityTargetStore.KEY_PREFIX,
        CONTINUITY_CURSOR_PREFIX,
        me.lakko.app.channels.SentReturnsStore.KEY_PREFIX,
        me.lakko.app.channels.RecoveryRegisteredStore.KEY_PREFIX,
        me.lakko.app.channels.RecoveryRegisteredStore.LEGACY_KEY_PREFIX,
        me.lakko.app.channels.OwnedChannelsFactsStore.KEY_PREFIX,
        me.lakko.app.chat.HistoryReturnStore.KEY_PREFIX,
        me.lakko.app.channels.RecoveredChannelsStore.KEY_PREFIX,
        me.lakko.app.profile.CarriedProfileStore.KEY_PREFIX,
        me.lakko.app.profile.CarriedProfileStore.NAME_KEY_PREFIX,
        me.lakko.app.chat.DrawnResendStore.KEY_PREFIX,
        me.lakko.app.chat.HistoryServedStore.KEY_PREFIX,
    )

    /** Every prefix of the encrypted file. */
    public val SECRET_PREFIXES: List<String> = listOf(
        me.lakko.app.channels.HeldReturnsStore.KEY_PREFIX,
        me.lakko.app.channels.ReturnedInvitationsStore.KEY_PREFIX,
        me.lakko.app.channels.OwnedChannelsSecretStore.KEY_PREFIX,
        me.lakko.app.channels.OwnedChannelsSecretStore.PASSKEYS_PREFIX,
    )

    /** The continuity mailbox's own cursor, per account. */
    public const val CONTINUITY_CURSOR_PREFIX: String = "continuity-mailbox-cursor:"

    /** The plain file as a [MarkerStorage]. */
    public fun plain(context: Context): MarkerStorage = SharedPrefsMarkerStorage(context, PLAIN_PREFS)

    /** The encrypted file as a [MarkerStorage]. */
    public fun secret(context: Context): MarkerStorage = EncryptedMarkerStorage(context, SECRET_PREFS)

    /** Remove every row of [accountIdB64] from [plain] and [secret]. */
    public fun wipe(plain: MarkerStorage, secret: MarkerStorage, accountIdB64: String) {
        for (p in PLAIN_PREFIXES) runCatching { plain.remove(p + accountIdB64) }
        for (p in SECRET_PREFIXES) runCatching { secret.remove(p + accountIdB64) }
    }

    /** [wipe] over the device's two files. */
    public fun wipeOnDevice(context: Context, accountIdB64: String) {
        wipe(plain(context), secret(context), accountIdB64)
    }

    /** The continuity mailbox cursor of [accountIdB64] (0 when none or damaged). */
    public fun readContinuityCursor(storage: MarkerStorage, accountIdB64: String): Long {
        val raw = runCatching { storage.get(CONTINUITY_CURSOR_PREFIX + accountIdB64) }.getOrNull() ?: return 0L
        return raw.toLongOrNull()?.takeIf { it >= 0L } ?: 0L
    }

    /** Write the cursor. Throws when storage refuses, which holds the cursor. */
    public fun writeContinuityCursor(storage: MarkerStorage, accountIdB64: String, cursor: Long) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            storage.set(CONTINUITY_CURSOR_PREFIX + accountIdB64, cursor.toString())
        }
    }
}
