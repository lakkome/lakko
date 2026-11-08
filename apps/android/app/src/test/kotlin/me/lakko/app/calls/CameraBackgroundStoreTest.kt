// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import java.io.File
import java.nio.file.Files
import me.lakko.app.account.AccountStoreGate
import me.lakko.app.avatar.AvatarSeal
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/**
 * The camera background's stores (`docs/ux/call-backgrounds.md` sections
 * 2.3 and 8, ADR-0089 section 5): the stored choice, what a selected photo
 * that is gone is drawn as, the twelve-photo limit, the account store gate
 * and the wipe.
 *
 * The stored text is read by the Rust core; the host JVM cannot run it, so
 * [RuleCodec] answers by the core's documented rule and the tests check
 * what the client hands it.
 */
class CameraBackgroundStoreTest {

    /** The core's reading rule: empty or `none` -> none, the known forms,
     *  anything else -> blur. Records what it was asked. */
    private class RuleCodec : BackgroundChoiceCodec {
        val asked = mutableListOf<String>()
        override fun read(stored: String): BackgroundSelection {
            asked += stored
            return when {
                stored.isEmpty() || stored == "none" -> BackgroundSelection.None
                stored == "blur" -> BackgroundSelection.Blur(false)
                stored == "strong-blur" -> BackgroundSelection.Blur(true)
                stored.startsWith("builtin:") -> BackgroundSelection.Builtin(stored.removePrefix("builtin:"))
                stored.startsWith("photo:") -> BackgroundSelection.Photo(stored.removePrefix("photo:"))
                else -> BackgroundSelection.Blur(false)
            }
        }
        override fun write(selection: BackgroundSelection): String = when (selection) {
            BackgroundSelection.None -> "none"
            is BackgroundSelection.Blur -> if (selection.strong) "strong-blur" else "blur"
            is BackgroundSelection.Builtin -> "builtin:${selection.id}"
            is BackgroundSelection.Photo -> "photo:${selection.id}"
        }
    }

    private class MemoryStorage(var text: String? = null, var throwOnRead: Boolean = false) : BackgroundChoiceStorage {
        override fun read(): String? {
            if (throwOnRead) throw IllegalStateException("unreadable")
            return text
        }
        override fun write(text: String) {
            this.text = text
        }
    }

    private object XorSeal : AvatarSeal {
        override fun seal(plaintext: ByteArray): ByteArray = plaintext.map { (it.toInt() xor 0x5a).toByte() }.toByteArray()
        override fun open(sealed: ByteArray): ByteArray = seal(sealed)
    }

    private lateinit var root: File
    private var clock = 1_000_000L
    private var counter = 0
    private val account = "AAECAwQFBgcICQoLDA0ODw=="
    private val other = "EBESExQVFhcYGRobHB0eHw=="

    private fun store() = BackgroundPhotoStore(
        root = root,
        seal = XorSeal,
        now = { clock },
        random = { "%08x".format(++counter) },
    )

    @Before
    fun setUp() {
        root = Files.createTempDirectory("lakko-backgrounds").toFile()
        AccountStoreGate.resetForTest()
    }

    @After
    fun tearDown() {
        root.deleteRecursively()
        AccountStoreGate.resetForTest()
    }

    // ---- the stored choice ------------------------------------------------

    @Test
    fun `nothing stored reads as no background`() {
        val holder = BackgroundChoiceHolder(RuleCodec()) { MemoryStorage(null) }
        assertEquals(BackgroundSelection.None, holder.current())
    }

    @Test
    fun `an unreadable stored text reads as blur`() {
        val holder = BackgroundChoiceHolder(RuleCodec()) { MemoryStorage("photo") }
        assertEquals(BackgroundSelection.Blur(false), holder.current())
    }

    @Test
    fun `storage that cannot be read reads as blur, not as no background`() {
        val codec = RuleCodec()
        val holder = BackgroundChoiceHolder(codec) { MemoryStorage(throwOnRead = true) }
        assertEquals(BackgroundSelection.Blur(false), holder.current())
        // The client handed the core something that is neither empty nor `none`.
        assertTrue(codec.asked.none { it.isEmpty() || it == "none" })
    }

    @Test
    fun `a choice is stored, survives a new process and bumps the generation`() {
        val storage = MemoryStorage()
        val holder = BackgroundChoiceHolder(RuleCodec()) { storage }
        val before = holder.generation
        holder.select(BackgroundSelection.Builtin("dusk"))
        assertEquals("builtin:dusk", storage.text)
        assertEquals(before + 1, holder.generation)
        holder.select(BackgroundSelection.Builtin("dusk"))
        assertEquals(before + 2, holder.generation)
        val next = BackgroundChoiceHolder(RuleCodec()) { storage }
        assertEquals(BackgroundSelection.Builtin("dusk"), next.current())
    }

    @Test
    fun `no storage yet reads again once there is one`() {
        var storage: BackgroundChoiceStorage? = null
        val holder = BackgroundChoiceHolder(RuleCodec()) { storage }
        assertEquals(BackgroundSelection.None, holder.current())
        storage = MemoryStorage("strong-blur")
        assertEquals(BackgroundSelection.Blur(true), holder.current())
    }

    @Test
    fun `a selected photo that is missing is drawn as blur`() {
        val look = backgroundLookFor(BackgroundSelection.Photo("t1_aa")) { false }
        assertEquals(BackgroundLook.Blur(strong = false), look)
    }

    @Test
    fun `a selected photo whose check throws is drawn as blur`() {
        val look = backgroundLookFor(BackgroundSelection.Photo("t1_aa")) { throw IllegalStateException("io") }
        assertEquals(BackgroundLook.Blur(strong = false), look)
    }

    @Test
    fun `a present photo and the other choices are drawn as chosen`() {
        assertEquals(BackgroundLook.Photo("t1_aa"), backgroundLookFor(BackgroundSelection.Photo("t1_aa")) { true })
        assertEquals(BackgroundLook.Blur(true), backgroundLookFor(BackgroundSelection.Blur(true)) { false })
        assertEquals(BackgroundLook.Builtin("night"), backgroundLookFor(BackgroundSelection.Builtin("night")) { false })
        assertNull(backgroundLookFor(BackgroundSelection.None) { true })
    }

    // ---- own photos ---------------------------------------------------------

    private fun addAt(store: BackgroundPhotoStore, at: Long, selected: String? = null): String {
        clock = at
        return store.add(account, byteArrayOf(1, 2, 3), selected)!!
    }

    @Test
    fun `photos are listed newest first and read back`() {
        val store = store()
        val first = addAt(store, 1_000)
        val second = addAt(store, 2_000)
        assertEquals(listOf(second, first), store.list(account))
        assertTrue(store.list(account).all { BackgroundPhotoStore.isOwnId(it) })
        assertEquals(listOf<Byte>(1, 2, 3), store.read(account, first)!!.toList())
        // Sealed on disk, not the plain bytes.
        val onDisk = root.walkTopDown().first { it.isFile }.readBytes()
        assertFalse(onDisk.contentEquals(byteArrayOf(1, 2, 3)))
    }

    @Test
    fun `a thirteenth photo removes the oldest one`() {
        val store = store()
        val ids = (1..12).map { addAt(store, it * 1_000L) }
        val added = addAt(store, 13_000)
        val held = store.list(account)
        assertEquals(12, held.size)
        assertFalse(ids.first() in held)
        assertTrue(added in held)
    }

    @Test
    fun `a thirteenth photo keeps the selected one even when it is the oldest`() {
        val store = store()
        val ids = (1..12).map { addAt(store, it * 1_000L) }
        addAt(store, 13_000, selected = ids[0])
        val held = store.list(account)
        assertEquals(12, held.size)
        assertTrue(ids[0] in held)
        assertFalse(ids[1] in held)
    }

    @Test
    fun `photos of a signed-out account are not written`() {
        val store = store()
        AccountStoreGate.seal(account)
        assertNull(store.add(account, byteArrayOf(1), null))
        assertTrue(store.list(account).isEmpty())
    }

    @Test
    fun `the wipe removes the account's photos and a choice naming one of them`() {
        val store = store()
        val mine = addAt(store, 1_000)
        clock = 2_000
        val theirs = store.add(other, byteArrayOf(9), null)!!
        val storage = MemoryStorage()
        val holder = BackgroundChoiceHolder(RuleCodec()) { storage }
        holder.select(BackgroundSelection.Photo(mine))

        wipeBackgroundPhotosIn(store, holder, account)

        assertTrue(store.list(account).isEmpty())
        assertFalse(File(root, BackgroundPhotoStore.accountDirName(account)).exists())
        assertEquals(listOf(theirs), store.list(other))
        assertEquals(BackgroundSelection.None, holder.current())
        assertEquals("none", storage.text)
    }

    @Test
    fun `the wipe keeps a choice that names no photo of the account`() {
        val store = store()
        addAt(store, 1_000)
        val holder = BackgroundChoiceHolder(RuleCodec()) { MemoryStorage() }
        holder.select(BackgroundSelection.Blur(strong = true))
        wipeBackgroundPhotosIn(store, holder, account)
        assertEquals(BackgroundSelection.Blur(true), holder.current())
    }
}
