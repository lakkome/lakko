// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.anon

import java.io.File
import java.nio.file.Files
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Test

/** ADR-0054 S5 - the token pool file under `noBackupFilesDir`. */
class AnonTokenFileStoreTest {

    private val dir: File = Files.createTempDirectory("anon-store").toFile()

    @After
    fun tearDown() {
        dir.deleteRecursively()
    }

    @Test
    fun roundTripsThePool() {
        val store = FileAnonTokenStore.under(dir)
        val pool = PersistedAnonPool(
            owner = "acct",
            tokens = listOf(
                PooledAnonToken(AnonTokenContext.DISCOVERY_LOOKUP, 10, "a"),
                PooledAnonToken(AnonTokenContext.PUSH_REGISTER, 11, "b"),
            ),
        )
        store.save(pool)
        assertEquals(pool, FileAnonTokenStore.under(dir).load())
        assertEquals(listOf("pool.json"), File(dir, "lakko-anon-tokens").list()!!.toList())
    }

    @Test
    fun clearDeletesTheFile() {
        val store = FileAnonTokenStore.under(dir)
        store.save(PersistedAnonPool("acct", emptyList()))
        store.clear()
        assertNull(store.load())
        assertFalse(File(File(dir, "lakko-anon-tokens"), "pool.json").exists())
    }

    @Test
    fun anUnreadableFileReadsAsEmptyAndIsRemoved() {
        val file = File(File(dir, "lakko-anon-tokens").apply { mkdirs() }, "pool.json")
        file.writeText("not json")
        assertNull(FileAnonTokenStore.under(dir).load())
        assertFalse(file.exists())
    }

    @Test
    fun anUnknownContextIsSkipped() {
        val file = File(File(dir, "lakko-anon-tokens").apply { mkdirs() }, "pool.json")
        file.writeText("""{"v":1,"owner":"acct","tokens":[{"c":"future","e":1,"h":"x"},{"c":"ds-knock","e":2,"h":"y"}]}""")
        val loaded = FileAnonTokenStore.under(dir).load()!!
        assertEquals(listOf(PooledAnonToken(AnonTokenContext.DS_KNOCK, 2, "y")), loaded.tokens)
    }
}
