// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.anon

import java.io.File
import java.nio.file.Files
import java.util.Base64
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Test

/** ADR-0054 S5 - the per-account token pool files under `noBackupFilesDir`. */
class AnonTokenFileStoreTest {

    private val dir: File = Files.createTempDirectory("anon-store").toFile()
    private val alice = Base64.getEncoder().encodeToString(ByteArray(16) { 0x0A })
    private val bob = Base64.getEncoder().encodeToString(ByteArray(16) { 0x0B })

    @After
    fun tearDown() {
        dir.deleteRecursively()
    }

    @Test
    fun roundTripsOneAccountsPool() {
        val store = FileAnonTokenStore.under(dir)
        val pool = PersistedAnonPool(
            owner = alice,
            tokens = listOf(
                PooledAnonToken(AnonTokenContext.DISCOVERY_LOOKUP, 10, "a"),
                PooledAnonToken(AnonTokenContext.PUSH_REGISTER, 11, "b"),
            ),
        )
        store.save(pool)
        assertEquals(pool, FileAnonTokenStore.under(dir).load(alice))
        val expected = Base64.getUrlEncoder().withoutPadding().encodeToString(ByteArray(16) { 0x0A }) + ".json"
        assertEquals(listOf(expected), File(dir, "lakko-anon-tokens").list()!!.toList())
    }

    @Test
    fun twoAccountsKeepSeparateFilesAndDeleteRemovesOnlyOne() {
        val store = FileAnonTokenStore.under(dir)
        val a = PersistedAnonPool(alice, listOf(PooledAnonToken(AnonTokenContext.DS_KNOCK, 1, "a")))
        val b = PersistedAnonPool(bob, listOf(PooledAnonToken(AnonTokenContext.DS_KNOCK, 1, "b")))
        store.save(a)
        store.save(b)
        assertEquals(a, store.load(alice))
        assertEquals(b, store.load(bob))
        store.delete(alice)
        assertNull(store.load(alice))
        assertEquals(b, store.load(bob))
        assertEquals(1, File(dir, "lakko-anon-tokens").list()!!.size)
    }

    @Test
    fun aFileWhoseOwnerIsAnotherAccountReadsAsEmpty() {
        val store = FileAnonTokenStore.under(dir)
        store.save(PersistedAnonPool(bob, listOf(PooledAnonToken(AnonTokenContext.DS_KNOCK, 1, "b"))))
        // Bob's file renamed to Alice's name: never handed to Alice.
        store.fileFor(bob).renameTo(store.fileFor(alice))
        assertNull(store.load(alice))
        assertFalse(store.fileFor(alice).exists())
    }

    @Test
    fun theRetiredDeviceWideFileIsDeleted() {
        val retired = File(File(dir, "lakko-anon-tokens").apply { mkdirs() }, "pool.json")
        retired.writeText("""{"v":1,"owner":"$alice","tokens":[]}""")
        val store = FileAnonTokenStore.under(dir)
        assertFalse(retired.exists())
        assertNull(store.load(alice))
    }

    @Test
    fun anUnreadableFileReadsAsEmptyAndIsRemoved() {
        val store = FileAnonTokenStore.under(dir)
        val file = store.fileFor(alice).apply { parentFile?.mkdirs() }
        file.writeText("not json")
        assertNull(store.load(alice))
        assertFalse(file.exists())
    }

    @Test
    fun anUnknownContextIsSkipped() {
        val store = FileAnonTokenStore.under(dir)
        val file = store.fileFor(alice).apply { parentFile?.mkdirs() }
        file.writeText("""{"v":1,"owner":"$alice","tokens":[{"c":"future","e":1,"h":"x"},{"c":"ds-knock","e":2,"h":"y"}]}""")
        val loaded = store.load(alice)!!
        assertEquals(listOf(PooledAnonToken(AnonTokenContext.DS_KNOCK, 2, "y")), loaded.tokens)
    }
}
