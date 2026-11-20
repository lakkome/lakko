// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.updates

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The supporters list (ADR-0097 section 2, `docs/ux/supporters.md` section 2). */
@OptIn(ExperimentalCoroutinesApi::class)
class SupportersListTest {

    private class MemStore(var list: HeldSupportersList? = null, var at: Long? = null) : SupportersListStore {
        override fun load(): HeldSupportersList? = list
        override fun save(list: HeldSupportersList) {
            this.list = list
        }
        override fun lastFetchAtMs(): Long? = at
        override fun setLastFetchAtMs(at: Long) {
            this.at = at
        }
    }

    private class FakeSource(var verified: HeldSupportersList?, var bytes: ByteArray? = byteArrayOf(1)) :
        SupportersListSource {
        var fetches = 0
        override suspend fun fetch(): ByteArray? {
            fetches += 1
            return bytes
        }
        override fun verify(bytes: ByteArray): HeldSupportersList? = verified
    }

    private fun list(version: Long, vararg names: String) = HeldSupportersList(version, 1, names.toList())

    private fun TestScope.controller(store: MemStore, source: SupportersListSource?, now: Long = 1_000L) =
        SupportersListController(store, source, this, nowMs = { now })

    @Test
    fun `no pinned key fetches nothing and shows no row`() = runTest {
        val store = MemStore(list = list(1, "Ann"))
        val c = controller(store, source = null)
        c.refresh()
        c.refreshIfDue()
        advanceUntilIdle()
        assertNull(c.state.value.held)
        assertFalse(c.state.value.rowVisible)
    }

    @Test
    fun `a verified list shows the row and a higher version replaces it`() = runTest {
        val store = MemStore()
        val source = FakeSource(list(2, "Ann", "Bo"))
        val c = controller(store, source)
        assertFalse(c.state.value.rowVisible)
        c.refresh()
        advanceUntilIdle()
        assertTrue(c.state.value.rowVisible)
        assertEquals(listOf("Ann", "Bo"), c.state.value.held?.names)
        assertEquals(2L, store.list?.listVersion)

        source.verified = list(1, "Old")
        c.refresh()
        advanceUntilIdle()
        assertEquals(listOf("Ann", "Bo"), c.state.value.held?.names)

        source.verified = list(3, "New")
        c.refresh()
        advanceUntilIdle()
        assertEquals(listOf("New"), c.state.value.held?.names)
        assertEquals(3L, store.list?.listVersion)
    }

    @Test
    fun `a list that does not verify is ignored as if absent`() = runTest {
        val store = MemStore(list = list(5, "Ann"))
        val c = controller(store, FakeSource(verified = null))
        c.refresh()
        advanceUntilIdle()
        assertEquals(listOf("Ann"), c.state.value.held?.names)
        assertTrue(c.state.value.failed)
        assertFalse(c.state.value.loading)
    }

    @Test
    fun `an empty list shows no row`() = runTest {
        val c = controller(MemStore(), FakeSource(list(1)))
        c.refresh()
        advanceUntilIdle()
        assertFalse(c.state.value.rowVisible)
    }

    @Test
    fun `a failed fetch with nothing held is the failed state`() = runTest {
        val c = controller(MemStore(), FakeSource(verified = list(1, "x"), bytes = null))
        c.refresh()
        advanceUntilIdle()
        assertNull(c.state.value.held)
        assertTrue(c.state.value.failed)
    }

    @Test
    fun `at launch the list is fetched at most once a day`() = runTest {
        val day = SupportersListController.DAY_MS
        val fresh = FakeSource(list(1, "a"))
        controller(MemStore(at = 1_000L), fresh, now = 1_000L + day - 1).refreshIfDue()
        advanceUntilIdle()
        assertEquals(0, fresh.fetches)
        controller(MemStore(at = 1_000L), fresh, now = 1_000L + day).refreshIfDue()
        advanceUntilIdle()
        assertEquals(1, fresh.fetches)
        controller(MemStore(), fresh).refreshIfDue()
        advanceUntilIdle()
        assertEquals(2, fresh.fetches)
    }

    @Test
    fun `the list lives beside the release notice`() {
        assertEquals(
            "https://lakko.me/.well-known/lakko-supporters",
            supportersListUrl("https://lakko.me/.well-known/lakko-release"),
        )
        assertEquals(
            "https://test.lakko.me/.well-known/lakko-supporters",
            supportersListUrl("https://test.lakko.me/.well-known/lakko-release"),
        )
    }
}
