// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import me.lakko.app.account.AccountStoreGate

/**
 * The key writes of ONE sign-in route, refused only when the account was
 * wiped after the route began (ADR-0077 decision 4, G-48). Mirror of the iOS
 * `SignInKeyWrites`.
 *
 * A sign-in route (a password, a passkey, a recovery, the phrase prompt, a
 * pairing, a device restore, a key reset) writes the device's keys for an
 * account after one or more round trips: the device root key, the export key,
 * the account identity seed, the device identity seed and its binding, the
 * session record. Forget device, Delete account and the other wipes run
 * between those round trips. A key written after such a wipe would put the
 * forgotten account's key back on the device.
 *
 * The account is usually not landed yet when these writes run: it may still
 * be sealed by an earlier sign-out in this process, or marked wiped by an
 * earlier Forget device, and a sign-in after either must write. So the
 * question is not "is the account sealed?" but "was it wiped since this route
 * began?" ([AccountStoreGate.WipeMark]). The route takes the mark at its
 * START, before its first round trip, and hands it to every write below; the
 * same mark gates the route's phrase row ([me.lakko.app.backup.DevicePhrase]).
 *
 * Each write runs inside the gate, so a wipe that starts while it is in
 * flight waits for it and then deletes what it wrote; what runs inside is one
 * store write (disk and Keystore), never a round trip.
 */
internal object SignInKeyWrites {

    /**
     * A write the route must not go on without (the device root key, the
     * session record): refused, it throws
     * [me.lakko.app.StaleBindingException], a cancellation, and the route
     * stops. An error the write itself throws is passed on.
     */
    inline fun <T> required(since: AccountStoreGate.WipeMark, accountIdB64: String, write: () -> T): T =
        AccountStoreGate.writeOrEndUnlessWipedSince(accountIdB64, since, write)

    /**
     * A best-effort write (a cache the route can do without): refused, it is
     * not made and `false` comes back. An error the write itself throws is
     * passed on, for the caller's own best-effort handling.
     */
    inline fun ifNotWiped(since: AccountStoreGate.WipeMark, accountIdB64: String, write: () -> Unit): Boolean =
        AccountStoreGate.writeUnlessWipedSince(accountIdB64, since) {
            write()
            true
        } ?: false

    /** `true` when a write for [accountIdB64] would be refused now. */
    fun isWiped(since: AccountStoreGate.WipeMark, accountIdB64: String): Boolean =
        AccountStoreGate.isWipedSince(accountIdB64, since)
}
