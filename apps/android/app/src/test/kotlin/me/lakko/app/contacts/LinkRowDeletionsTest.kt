// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import android.content.SharedPreferences
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.yield
import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.discovery.DiscoveryHttpError
import me.lakko.app.discovery.DiscoveryNetworkError
import me.lakko.app.discovery.RevokeInviteResult
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0079 section 4 - the ledger holds one row per invite link, and only
 * the device that revokes the link deletes it, once. The old Android code
 * sent a second `DELETE /discovery/invite/{id}` from the post-poll sweep for
 * every revoked link it held a record of (404 on the device that revoked it,
 * and on every other device that minted it); these tests count the ledger
 * calls.
 */
class LinkRowDeletionsTest {

    private val account = "YWNjb3VudA=="
    private val blobId = ByteArray(6) { (it + 1).toByte() }
    private val slug = base64UrlEncodeNoPad(blobId)
    private val seed = ByteArray(32) { 7 }

    /** A ledger that counts its `DELETE`s. */
    private class Ledger {
        val calls = AtomicInteger(0)
        var answer: () -> RevokeInviteResult = { RevokeInviteResult.Revoked }
        var gate: CompletableDeferred<Unit>? = null
        val entered = CompletableDeferred<Unit>()
        val delete = LinkRowDeleter.Delete { _, s ->
            assertEquals(32, s.size)
            calls.incrementAndGet()
            entered.complete(Unit)
            gate?.await()
            answer()
        }
    }

    private fun networkDown(): RevokeInviteResult =
        throw DiscoveryNetworkError("/discovery/invite (DELETE)", java.io.IOException("no route"))

    @Test
    fun `a link revoked on another device sends the ledger nothing and drops local traces`() = runBlocking {
        val deleter = LinkRowDeleter(InMemoryOwedLinkRowDeletionStore())
        val ledger = Ledger()
        val forgotten = mutableListOf<String>()
        // This device minted the link (it holds a seed); another device of the
        // account revoked it, and the register carries the seed.
        repeat(3) {
            LinkRowSweep.run(
                accountIdB64 = account,
                revoked = listOf(LinkRowSweep.RevokedLink(blobId, seed.copyOf())),
                ownSeed = { seed.copyOf() },
                deleter = deleter,
                delete = ledger.delete,
                forget = { forgotten += base64UrlEncodeNoPad(it) },
            )
        }
        assertEquals("the revoking device deleted the row; this one must not", 0, ledger.calls.get())
        assertTrue(forgotten.isNotEmpty())
        assertTrue(forgotten.all { it == slug })
        assertTrue(deleter.owed.slugs(account).isEmpty())
    }

    @Test
    fun `the revoking device deletes once and later sweeps send nothing`() = runBlocking {
        val deleter = LinkRowDeleter(InMemoryOwedLinkRowDeletionStore())
        val ledger = Ledger()
        var registerRevoked = false
        val outcome = LinkRowRevoke.run(
            accountIdB64 = account, blobId = blobId, recordSeed = seed.copyOf(), ownSeed = seed.copyOf(),
            revokeRegister = {
                assertTrue("owed is marked before the register changes", deleter.owed.isOwed(account, slug))
                registerRevoked = true
            },
            deleter = deleter, delete = ledger.delete, forget = { },
        )
        assertTrue(registerRevoked)
        assertEquals(LinkRowDeleter.Outcome.Deleted(RevokeInviteResult.Revoked), outcome)
        repeat(3) {
            LinkRowSweep.run(
                account, listOf(LinkRowSweep.RevokedLink(blobId, seed.copyOf())), { seed.copyOf() },
                deleter, ledger.delete, { },
            )
        }
        assertEquals(1, ledger.calls.get())
        assertFalse(deleter.owed.isOwed(account, slug))
    }

    @Test
    fun `a sweep during the revoking DELETE joins it instead of sending a second`() = runBlocking {
        val deleter = LinkRowDeleter(InMemoryOwedLinkRowDeletionStore())
        val ledger = Ledger().apply { gate = CompletableDeferred() }
        val revoke = async {
            LinkRowRevoke.run(
                account, blobId, seed.copyOf(), seed.copyOf(), { }, deleter, ledger.delete, { },
            )
        }
        ledger.entered.await()
        val sweep = async {
            LinkRowSweep.run(
                account, listOf(LinkRowSweep.RevokedLink(blobId, seed.copyOf())), { seed.copyOf() },
                deleter, ledger.delete, { },
            )
        }
        repeat(10) { yield() }
        ledger.gate!!.complete(Unit)
        revoke.await()
        sweep.await()
        assertEquals(1, ledger.calls.get())
        assertFalse(deleter.owed.isOwed(account, slug))
    }

    @Test
    fun `an unreachable ledger keeps the deletion owed and one later sweep finishes it`() = runBlocking {
        val deleter = LinkRowDeleter(InMemoryOwedLinkRowDeletionStore())
        val ledger = Ledger().apply { answer = { networkDown() } }
        var forgotten = 0
        val outcome = LinkRowRevoke.run(
            account, blobId, seed.copyOf(), null, { }, deleter, ledger.delete, { forgotten += 1 },
        )
        assertEquals(LinkRowDeleter.Outcome.Unreachable, outcome)
        assertTrue(deleter.owed.isOwed(account, slug))
        assertEquals("local traces stay until the ledger answers", 0, forgotten)

        val down = LinkRowSweep.run(
            account, listOf(LinkRowSweep.RevokedLink(blobId, seed.copyOf())), { null },
            deleter, ledger.delete, { forgotten += 1 },
        )
        assertEquals(1, down.unreachable)
        assertTrue(deleter.owed.isOwed(account, slug))

        // The ledger answers "no row" (another call got through): done.
        ledger.answer = { RevokeInviteResult.NotFoundOrNotOwner }
        LinkRowSweep.run(
            account, listOf(LinkRowSweep.RevokedLink(blobId, seed.copyOf())), { null },
            deleter, ledger.delete, { forgotten += 1 },
        )
        assertFalse(deleter.owed.isOwed(account, slug))
        val callsWhenDone = ledger.calls.get()
        assertEquals(3, callsWhenDone)
        assertTrue(forgotten >= 1)

        LinkRowSweep.run(
            account, listOf(LinkRowSweep.RevokedLink(blobId, seed.copyOf())), { null },
            deleter, ledger.delete, { },
        )
        assertEquals("nothing more once the ledger answered", callsWhenDone, ledger.calls.get())
    }

    @Test
    fun `an owed deletion is retried even before the register is open`() = runBlocking {
        val store = InMemoryOwedLinkRowDeletionStore()
        store.set(account, slug, true)
        val deleter = LinkRowDeleter(store)
        val ledger = Ledger()
        LinkRowSweep.run(account, emptyList(), { seed.copyOf() }, deleter, ledger.delete, { })
        assertEquals(1, ledger.calls.get())
        assertFalse(store.isOwed(account, slug))
    }

    @Test
    fun `a refusal other than the network keeps the deletion owed`() = runBlocking {
        val deleter = LinkRowDeleter(InMemoryOwedLinkRowDeletionStore())
        val ledger = Ledger().apply {
            answer = { throw DiscoveryHttpError(503, "/discovery/invite", "", code = null, retryAfterSecs = null) }
        }
        val thrown = runCatching {
            LinkRowRevoke.run(account, blobId, seed.copyOf(), null, { }, deleter, ledger.delete, { })
        }.exceptionOrNull()
        assertTrue(thrown is DiscoveryHttpError)
        assertTrue(deleter.owed.isOwed(account, slug))
    }

    @Test
    fun `a failed register revoke clears the mark and sends nothing`() = runBlocking {
        val deleter = LinkRowDeleter(InMemoryOwedLinkRowDeletionStore())
        val ledger = Ledger()
        val thrown = runCatching {
            LinkRowRevoke.run(
                account, blobId, seed.copyOf(), null, { throw IllegalStateException("register") },
                deleter, ledger.delete, { },
            )
        }.exceptionOrNull()
        assertTrue(thrown is IllegalStateException)
        assertFalse(deleter.owed.isOwed(account, slug))
        assertEquals(0, ledger.calls.get())
    }

    @Test
    fun `a seedless record is deleted once by the device holding its own seed`() = runBlocking {
        val deleter = LinkRowDeleter(InMemoryOwedLinkRowDeletionStore())
        val ledger = Ledger()
        var ownRecord: ByteArray? = seed.copyOf()
        val seedless = listOf(LinkRowSweep.RevokedLink(blobId, ByteArray(0)))
        repeat(3) {
            LinkRowSweep.run(
                account, seedless, { ownRecord?.copyOf() }, deleter, ledger.delete,
                { ownRecord = null },
            )
        }
        assertEquals(1, ledger.calls.get())
        assertFalse(deleter.owed.isOwed(account, slug))

        // A device with no record of its own sends nothing.
        val other = LinkRowDeleter(InMemoryOwedLinkRowDeletionStore())
        val otherLedger = Ledger()
        LinkRowSweep.run(account, seedless, { null }, other, otherLedger.delete, { })
        assertEquals(0, otherLedger.calls.get())
    }

    @Test
    fun `a second revoke while the first runs joins it`() = runBlocking {
        val deleter = LinkRowDeleter(InMemoryOwedLinkRowDeletionStore())
        val ledger = Ledger().apply { gate = CompletableDeferred() }
        val first = async { LinkRowRevoke.run(account, blobId, seed.copyOf(), null, { }, deleter, ledger.delete, { }) }
        ledger.entered.await()
        val second = async { LinkRowRevoke.run(account, blobId, seed.copyOf(), null, { }, deleter, ledger.delete, { }) }
        repeat(10) { yield() }
        ledger.gate!!.complete(Unit)
        assertEquals(LinkRowDeleter.Outcome.Deleted(RevokeInviteResult.Revoked), first.await())
        assertEquals(LinkRowDeleter.Outcome.Deleted(RevokeInviteResult.Revoked), second.await())
        assertEquals(1, ledger.calls.get())
    }

    @Test
    fun `the owed list is kept per account in shared preferences and cleared on forget`() {
        val prefs = MemoryPrefs()
        val store = SharedPrefsOwedLinkRowDeletionStore { prefs }
        store.set(account, slug, true)
        store.set(account, "other", true)
        store.set("b3RoZXI=", slug, true)
        val reopened = SharedPrefsOwedLinkRowDeletionStore { prefs }
        assertEquals(setOf(slug, "other"), reopened.slugs(account))
        reopened.set(account, "other", false)
        assertEquals(setOf(slug), reopened.slugs(account))
        reopened.clearForAccount(account)
        assertTrue(reopened.slugs(account).isEmpty())
        assertEquals("another account's list stays", setOf(slug), reopened.slugs("b3RoZXI="))
    }

    /** A minimal in-memory [SharedPreferences]. */
    private class MemoryPrefs : SharedPreferences {
        val map = HashMap<String, Any?>()
        override fun getAll(): MutableMap<String, *> = HashMap(map)
        override fun getString(key: String, defValue: String?): String? = map[key] as? String ?: defValue
        @Suppress("UNCHECKED_CAST")
        override fun getStringSet(key: String, defValues: MutableSet<String>?): MutableSet<String>? =
            (map[key] as? Set<String>)?.toMutableSet() ?: defValues
        override fun getInt(key: String, defValue: Int): Int = map[key] as? Int ?: defValue
        override fun getLong(key: String, defValue: Long): Long = map[key] as? Long ?: defValue
        override fun getFloat(key: String, defValue: Float): Float = map[key] as? Float ?: defValue
        override fun getBoolean(key: String, defValue: Boolean): Boolean = map[key] as? Boolean ?: defValue
        override fun contains(key: String): Boolean = key in map
        override fun registerOnSharedPreferenceChangeListener(l: SharedPreferences.OnSharedPreferenceChangeListener) {}
        override fun unregisterOnSharedPreferenceChangeListener(l: SharedPreferences.OnSharedPreferenceChangeListener) {}
        override fun edit(): SharedPreferences.Editor = object : SharedPreferences.Editor {
            val pending = HashMap<String, Any?>()
            val removed = HashSet<String>()
            override fun putString(key: String, value: String?) = apply { pending[key] = value }
            override fun putStringSet(key: String, values: MutableSet<String>?) = apply { pending[key] = values?.toSet() }
            override fun putInt(key: String, value: Int) = apply { pending[key] = value }
            override fun putLong(key: String, value: Long) = apply { pending[key] = value }
            override fun putFloat(key: String, value: Float) = apply { pending[key] = value }
            override fun putBoolean(key: String, value: Boolean) = apply { pending[key] = value }
            override fun remove(key: String) = apply { removed += key }
            override fun clear() = apply { removed += map.keys }
            override fun commit(): Boolean {
                apply()
                return true
            }
            override fun apply() {
                removed.forEach { map.remove(it) }
                map.putAll(pending)
            }
        }
    }
}
