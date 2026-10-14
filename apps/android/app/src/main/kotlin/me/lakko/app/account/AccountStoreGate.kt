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
    public inline fun <T> write(accountIdB64: String, write: () -> T): T? =
        lock.read {
            if (refused(accountIdB64)) null else write()
        }

    /**
     * Whether a write of [accountIdB64] is refused now. Call under [lock].
     * The thread that is sealing is the wipe itself (it holds the exclusive
     * lock): a store that deletes by writing an empty row must not be told
     * its account is gone.
     */
    @PublishedApi
    internal fun refused(accountIdB64: String): Boolean =
        key(accountIdB64) in sealed && !lock.isWriteLockedByCurrentThread

    /**
     * [write] for a write its caller must not go on without (the ratchet
     * state persisted before a ciphertext is published, a commit recorded
     * before it is sent): refused, it ends the caller as work of a session
     * that is over, so nothing is published on the strength of a write that
     * was not made.
     */
    public inline fun <T> writeOrEnd(accountIdB64: String, write: () -> T): T =
        lock.read {
            if (refused(accountIdB64)) throw StaleBindingException()
            write()
        }

    /**
     * One session of an account as the gate saw it open, for a writer that
     * holds state of its own for that session (its own MLS provider handle)
     * and writes the WHOLE of it. Such a writer is not stopped by a seal that
     * was lifted again: the same account signed out and in, the gate is open,
     * and what the old writer holds is the previous session's state. It
     * captures this when it starts and presents it with every write.
     */
    public class Session internal constructor(
        @PublishedApi internal val accountIdB64: String,
        @PublishedApi internal val sealsSeen: Long,
    )

    /** How often each account was sealed. Guarded by [lock]. */
    @PublishedApi
    internal val seals: HashMap<String, Long> = HashMap()

    /** The session of [accountIdB64] that is open now. */
    public fun session(accountIdB64: String): Session =
        lock.read { Session(accountIdB64, seals[key(accountIdB64)] ?: 0L) }

    /**
     * [writeOrEnd] for the writer of one [session]: refused as well when the
     * account was sealed since the session was captured, even if it has been
     * opened again.
     */
    public inline fun <T> writeOrEnd(session: Session, write: () -> T): T =
        lock.read {
            val current = seals[key(session.accountIdB64)] ?: 0L
            if (current != session.sealsSeen || refused(session.accountIdB64)) throw StaleBindingException()
            write()
        }

    /**
     * The session of [accountIdB64] has ended: wait for its writes in flight,
     * refuse the later ones, and run [during] (the wipe of a forgotten
     * account) while nothing can write.
     */
    public fun <T> seal(accountIdB64: String, during: () -> T): T =
        lock.write {
            if (accountIdB64.isNotEmpty()) {
                val account = key(accountIdB64)
                // Counted once per ended session, not per call: the runtimes
                // each seal the account on the way out.
                if (sealed.add(account)) seals[account] = (seals[account] ?: 0L) + 1L
            }
            during()
        }

    /** [seal] with nothing to run: a sign-out, which deletes nothing. */
    public fun seal(accountIdB64: String) {
        seal(accountIdB64) { }
    }

    /** [accountIdB64] landed on this device again: its stores are written. */
    public fun open(accountIdB64: String) {
        lock.write {
            sealed.remove(key(accountIdB64))
            wiped.remove(key(accountIdB64))
        }
    }

    /**
     * The accounts whose local data a wipe removed in this process (Forget
     * device, the key-reset wipe, an expired guest), until their next
     * landing ([open]).
     *
     * A few rows are written for an account that is signed OUT, by design:
     * the guess budget of the three-word route, the logout the server was
     * not told about, the recall record a superseded device keeps. A sealed
     * account must still take those writes, so they ask [writeUnlessWiped]
     * instead of [write]: refused only once the account's data was wiped,
     * which is what stops a late write from putting a row back.
     */
    @PublishedApi
    internal val wiped: HashSet<String> = HashSet()

    /** A wipe of [accountIdB64] is running or has run. Called by the wipe
     *  itself, as its first step. */
    public fun markWiped(accountIdB64: String) {
        if (accountIdB64.isEmpty()) return
        lock.write {
            val account = key(accountIdB64)
            wiped.add(account)
            wipeCount += 1
            lastWipe[account] = wipeCount
        }
    }

    /**
     * A point in this process's sequence of wipes, taken when a piece of
     * sign-in work begins ([wipeMark]).
     *
     * A sign-in writes some of its account's rows BEFORE the account lands
     * (the recovery phrase is the one that matters), so neither [write] nor
     * [writeUnlessWiped] is the right question for them: the account may
     * still be sealed from a sign-out in this process, and it may even be
     * marked wiped by a Forget device that came BEFORE this sign-in, which is
     * exactly the case of signing back in to a forgotten account. What such a
     * write must not do is outlive a wipe that came AFTER its work began:
     * that would put a forgotten account's row back. [writeUnlessWipedSince]
     * asks that and nothing else. Mirror of the iOS `AccountStoreGate.WipeMark`.
     *
     * Not per account, so it can be taken before the work knows which
     * account it is for (a sign-in by name learns the account id from the
     * server's answer).
     */
    public class WipeMark internal constructor(@PublishedApi internal val wipesSeen: Long)

    /** How many wipes ran in this process. Guarded by [lock]. */
    @PublishedApi
    internal var wipeCount: Long = 0L

    /** The [wipeCount] of each account's most recent wipe. Never cleared by
     *  [open]: a landing does not make a wipe that came after a mark go away.
     *  Guarded by [lock]. */
    @PublishedApi
    internal val lastWipe: HashMap<String, Long> = HashMap()

    /** The wipes so far, for [writeUnlessWipedSince]. */
    public fun wipeMark(): WipeMark = lock.read { WipeMark(wipeCount) }

    /** Whether [accountIdB64] was wiped after [since] was taken. Call under
     *  [lock]. The wipe's own thread is let through, as in [write]. */
    @PublishedApi
    internal fun wipedSince(accountIdB64: String, since: WipeMark): Boolean =
        (lastWipe[key(accountIdB64)] ?: 0L) > since.wipesSeen && !lock.isWriteLockedByCurrentThread

    /**
     * Run [write] unless [accountIdB64] was wiped after [since] was taken;
     * `null` when refused. A sign-out, and a wipe that came before [since],
     * refuse nothing here ([WipeMark]).
     */
    public inline fun <T> writeUnlessWipedSince(accountIdB64: String, since: WipeMark, write: () -> T): T? =
        lock.read {
            if (wipedSince(accountIdB64, since)) null else write()
        }

    public fun isWiped(accountIdB64: String): Boolean = lock.read { key(accountIdB64) in wiped }

    /** Run [write] unless [accountIdB64] was wiped; `null` when refused. The
     *  wipe's own thread is let through, as in [write]. */
    public inline fun <T> writeUnlessWiped(accountIdB64: String, write: () -> T): T? =
        lock.read {
            if (key(accountIdB64) in wiped && !lock.isWriteLockedByCurrentThread) null else write()
        }

    /** Whether writes of [accountIdB64] are refused. Tests and diagnostics. */
    public fun isSealed(accountIdB64: String): Boolean = lock.read { key(accountIdB64) in sealed }

    /** Open every account (a test starts from a new process's state). */
    internal fun resetForTest() {
        lock.write {
            sealed.clear()
            seals.clear()
            wiped.clear()
            lastWipe.clear()
            wipeCount = 0L
        }
    }
}
