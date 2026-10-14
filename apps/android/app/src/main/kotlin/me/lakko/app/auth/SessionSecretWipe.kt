// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import me.lakko.app.StaleBindingException
import uniffi.lakko_ffi_uniffi.Identity

/**
 * The wipe of a session's seeds at sign-out, and the one way to build an
 * identity from a seed that the wipe cannot tear.
 *
 * `AuthRepository` zeroes the signed-in account's seeds IN PLACE the moment
 * the session ends, and the same arrays are what work still in flight for
 * that session holds. Work that asked its binding a moment before the wipe
 * goes on to read zeroes, or bytes half zeroed, and builds an identity from
 * them: a key that belongs to nobody, acting under the account's id and its
 * bearer token.
 *
 * Handing each piece of work its own copy would close that, and would also
 * leave as many un-wiped copies of the seed on the heap as there is work in
 * flight. So the arrays stay shared and wiped when they are now, and the two
 * sides take one lock instead: [wipe] zeroes under it, and [use] reads under
 * it and refuses a seed that has been wiped. A reader therefore sees the seed
 * whole (and the core has copied it before the wipe can start) or is told the
 * session has ended.
 *
 * An all-zero array IS a wiped one: a seed is the output of a key-derivation
 * function and is all zero with probability 2^-512.
 */
public object SessionSecretWipe {
    private val lock = Any()

    /** Zero every array in [secrets], excluding any [use] in progress. */
    internal fun wipe(vararg secrets: ByteArray) {
        synchronized(lock) { secrets.forEach { it.fill(0) } }
    }

    /**
     * Run [read] over [seed] while no wipe can touch it.
     *
     * @throws StaleBindingException when [seed] was wiped: the session it
     *   belonged to has ended.
     */
    public fun <T> use(seed: ByteArray, read: (ByteArray) -> T): T =
        synchronized(lock) {
            if (seed.all { it == 0.toByte() }) throw StaleBindingException()
            read(seed)
        }

    /** The account identity of a session seed that may be wiped under the
     *  caller: `Identity.fromSeed`, or [StaleBindingException]. */
    public fun identity(accountIdentitySeed: ByteArray): Identity = use(accountIdentitySeed, Identity::fromSeed)
}
