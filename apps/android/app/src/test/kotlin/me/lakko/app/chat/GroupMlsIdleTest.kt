// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The per-group lock's drain report ([GroupLocks], [DrainNotifyingMutex]): the
 * signal the app-level group-call host rotates a call key on (G-18 /
 * ADR-0029). The property that matters is that it fires once the group's work
 * has SETTLED — never between two queued regions, where the next region may
 * already hold an eagerly merged, unpublished commit (ADR-0040).
 */
@OptIn(ExperimentalCoroutinesApi::class)
class GroupMlsIdleTest {

    @Test
    fun `a region that runs alone reports one drain for its group`() = runTest {
        val drains = mutableListOf<String>()
        val locks = GroupLocks(onDrain = { drains += it })
        locks.forGroup("aa").withLock { assertTrue(locks.isBusy("aa")) }
        assertEquals(listOf("aa"), drains)
        assertFalse(locks.isBusy("aa"))
    }

    @Test
    fun `queued regions report once, after the last, never between them`() = runTest {
        val drains = mutableListOf<String>()
        val locks = GroupLocks(onDrain = { drains += it })
        val lock = locks.forGroup("aa")
        val firstRelease = CompletableDeferred<Unit>()
        val secondRelease = CompletableDeferred<Unit>()
        val order = mutableListOf<String>()
        launch { lock.withLock { order += "first"; firstRelease.await() } }
        runCurrent()
        launch { lock.withLock { order += "second"; secondRelease.await() } }
        runCurrent()

        firstRelease.complete(Unit)
        runCurrent()
        assertEquals(listOf("first", "second"), order)
        assertEquals("the holder's release with a region queued is not a drain", emptyList<String>(), drains)
        assertTrue(locks.isBusy("aa"))

        secondRelease.complete(Unit)
        runCurrent()
        assertEquals(listOf("aa"), drains)
        assertFalse(locks.isBusy("aa"))
    }

    @Test
    fun `groups drain independently`() = runTest {
        val drains = mutableListOf<String>()
        val locks = GroupLocks(onDrain = { drains += it })
        val hold = CompletableDeferred<Unit>()
        launch { locks.forGroup("aa").withLock { hold.await() } }
        runCurrent()
        locks.forGroup("bb").withLock { }
        assertEquals(listOf("bb"), drains)
        hold.complete(Unit)
        runCurrent()
        assertEquals(listOf("bb", "aa"), drains)
    }

    @Test
    fun `a waiter cancelled before it acquired leaves the count balanced`() = runTest {
        val drains = mutableListOf<String>()
        val locks = GroupLocks(onDrain = { drains += it })
        val lock = locks.forGroup("aa")
        val hold = CompletableDeferred<Unit>()
        launch { lock.withLock { hold.await() } }
        runCurrent()
        val waiter = launch { lock.withLock { } }
        runCurrent()
        waiter.cancel()
        runCurrent()
        assertTrue("the holder still holds it", locks.isBusy("aa"))
        assertEquals(emptyList<String>(), drains)

        hold.complete(Unit)
        runCurrent()
        assertEquals(listOf("aa"), drains)
        assertFalse(locks.isBusy("aa"))
    }

    @Test
    fun `a failed tryLock is not a drain, and a successful one reports on unlock`() {
        val drains = mutableListOf<String>()
        val mutex = DrainNotifyingMutex(onDrain = { drains += "x" })
        assertTrue(mutex.tryLock())
        assertFalse(mutex.tryLock())
        assertEquals(emptyList<String>(), drains)
        assertTrue(mutex.isBusy)
        mutex.unlock()
        assertEquals(listOf("x"), drains)
        assertFalse(mutex.isBusy)
    }

    @Test
    fun `a throwing listener never breaks the lock`() = runTest {
        val locks = GroupLocks(onDrain = { throw IllegalStateException("listener") })
        locks.forGroup("aa").withLock { }
        locks.forGroup("aa").withLock { }
        assertFalse(locks.isBusy("aa"))
    }
}
