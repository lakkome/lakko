// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.account

import me.lakko.app.StaleBindingException
import java.util.concurrent.locks.ReentrantReadWriteLock
import kotlin.concurrent.read
import kotlin.concurrent.write

/**
 * Who may write an account's stores on this device, and when.
 *
 * A session's runtimes write its per-account stores from their own threads:
 * the MLS snapshot, the message log, the commit outbox, the push routing
 * rows. Ending the session (sign-out, forget device, another account landing)
 * runs on a different thread and takes none of their locks, so a write that
 * was already past its "which account?" read when the session ended went on
 * to finish afterwards. After a forget-device that is a store of a forgotten
 * account coming back; after a sign-out it is a write nobody asked for.
 *
 * This is a thread race, not work resumed after a suspension, so asking a
 * binding does not close it: the two sides have to exclude each other.
 *
 *  - A store's mutator runs inside [write] (or [writeOrEnd]): shared, so
 *    writers never wait for each other.
 *  - Ending a session runs [seal]: exclusive, so it WAITS for every write in
 *    flight, and from then on the account's writes are refused. The wipe of a
 *    forgotten account runs inside it.
 *  - The account is written again from its next landing on this device
 *    ([open]).
 *
 * Lock order. The gate is the innermost lock a writer takes: inside the
 * per-group mutex and the provider lock, never around either, and OUTSIDE a
 * store's own monitor (the wipe takes the gate and then that monitor to
 * delete, so a writer must not hold the monitor while it waits for the gate).
 * What runs under it is one store write: disk and Keystore, no network, no
 * coroutine suspension and no call back into the code that ends sessions.
 * That is what lets [seal] be called from the main thread: it waits for disk
 * writes in flight, never for a long poll.
 *
 * Process-wide and in memory: a new process starts with every account open,
 * which is right, since no writer of an ended session survives the process.
 */
public object AccountStoreGate {

    @PublishedApi
    internal val lock: ReentrantReadWriteLock = ReentrantReadWriteLock()

    /** Accounts whose session ended and has not landed again. Guarded by [lock]. */
    @PublishedApi
    internal val sealed: HashSet<String> = HashSet()

    /**
     * One key for the spellings an account id is written in (standard and
     * URL-safe base64, padded or not): the runtimes do not agree on one.
     */
    @PublishedApi
    internal fun key(accountIdB64: String): String =
        accountIdB64.replace('-', '+').replace('_', '/').trimEnd('=')

    /**
     * Run one store write for [accountIdB64], unless its session has ended.
     * `null` when it was refused: for a best-effort write, which is then
     * simply not made.
     */
    public inline fun <T : Any> write(accountIdB64: String, write: () -> T): T? =
        lock.read {
            if (key(accountIdB64) in sealed) null else write()
        }

    /**
     * [write] for a write its caller must not go on without (the ratchet
     * state persisted before a ciphertext is published, a commit recorded
     * before it is sent): refused, it ends the caller as work of a session
     * that is over, so nothing is published on the strength of a write that
     * was not made.
     */
    public inline fun <T> writeOrEnd(accountIdB64: String, write: () -> T): T =
        lock.read {
            if (key(accountIdB64) in sealed) throw StaleBindingException()
            write()
        }

    /**
     * The session of [accountIdB64] has ended: wait for its writes in flight,
     * refuse the later ones, and run [during] (the wipe of a forgotten
     * account) while nothing can write.
     */
    public fun <T> seal(accountIdB64: String, during: () -> T): T =
        lock.write {
            if (accountIdB64.isNotEmpty()) sealed.add(key(accountIdB64))
            during()
        }

    /** [seal] with nothing to run: a sign-out, which deletes nothing. */
    public fun seal(accountIdB64: String) {
        seal(accountIdB64) { }
    }

    /** [accountIdB64] landed on this device again: its stores are written. */
    public fun open(accountIdB64: String) {
        lock.write { sealed.remove(key(accountIdB64)) }
    }

    /** Whether writes of [accountIdB64] are refused. Tests and diagnostics. */
    public fun isSealed(accountIdB64: String): Boolean = lock.read { key(accountIdB64) in sealed }

    /** Open every account (a test starts from a new process's state). */
    internal fun resetForTest() {
        lock.write { sealed.clear() }
    }
}
