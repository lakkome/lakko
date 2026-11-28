// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import kotlinx.coroutines.test.runTest
import me.lakko.app.account.AccountStoreGate
import me.lakko.app.auth.MarkerStorage
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/**
 * ADR-0101 section 7 (rewritten 2026-11-26) - the owned-channels list and
 * its locks, as a device with the keys keeps them and as a reset opens them.
 * The core is behind [OwnedCore] and the storage service behind
 * [OwnedVaults]; fakes stand in (the host JVM loads no native library).
 */
class OwnedChannelsTest {

    @Before
    fun setUp() = AccountStoreGate.resetForTest()

    @After
    fun tearDown() = AccountStoreGate.resetForTest()

    private fun bytes(fill: Int) = ByteArray(32) { fill.toByte() }
    private fun hex(fill: Int) = "%02x".format(fill).repeat(16)

    private fun entry(id: Int, epochs: List<Long> = listOf(1L), admission: List<Int> = listOf(0), readKey: ByteArray? = null) =
        ownedEntryOf(
            channelIdHex = hex(id),
            readKey = readKey,
            admission = admission.map { OwnedAdmission(it, bytes(0x40 + it)) },
            epochs = epochs.map { OwnedEpoch(it, bytes(0x50 + it.toInt())) },
        )

    /** A core whose "encodings" are readable text and whose roots are
     *  simple functions of their inputs. */
    private object Core : OwnedCore {
        override fun passwordLockRoot(exportKey: ByteArray, serverHalf: ByteArray): ByteArray =
            ByteArray(32) { (exportKey[0] + serverHalf[0] + 1).toByte() }
        override fun passkeyLockRoot(prfOutput: ByteArray, serverHalf: ByteArray): ByteArray =
            ByteArray(32) { (prfOutput[0] + serverHalf[0] + 2).toByte() }
        private var next = 0x70
        override fun newListKey(): ByteArray = ByteArray(32) { next.toByte() }.also { next += 1 }
        override fun encodeListKeyLock(listKey: ByteArray): ByteArray = "R:".toByteArray() + listKey
        override fun decodeListKeyLock(head: ByteArray): ByteArray {
            require(head.size == 34 && head.copyOfRange(0, 2).contentEquals("R:".toByteArray())) { "not a lock" }
            return head.copyOfRange(2, 34)
        }
        override fun encodeKeysLock(listKey: ByteArray, serverHalf: ByteArray): ByteArray = "K:".toByteArray() + listKey + serverHalf
        override fun decodeKeysLock(head: ByteArray): Pair<ByteArray, ByteArray> {
            require(head.size == 66 && head.copyOfRange(0, 2).contentEquals("K:".toByteArray())) { "not a keys lock" }
            return head.copyOfRange(2, 34) to head.copyOfRange(34, 66)
        }
        private val lists = HashMap<String, OwnedList>()
        override fun encodeList(list: OwnedList): ByteArray {
            val key = "L:" + ownedListDigest(list)
            lists[key] = list
            return key.toByteArray()
        }
        override fun decodeList(bytes: ByteArray): OwnedList =
            lists[bytes.toString(Charsets.UTF_8)] ?: throw IllegalArgumentException("not a list")
        override fun merge(mine: OwnedList, theirs: OwnedList, deletedHex: List<String>): OwnedList {
            // By channel id; a deleted channel dropped; the longer history of
            // a channel both name kept.
            val byId = LinkedHashMap<String, OwnedEntry>()
            for (c in theirs.channels + mine.channels) {
                if (c.channelIdHex in deletedHex) continue
                val held = byId[c.channelIdHex]
                if (held == null || (c.epochs.maxOfOrNull { it.epoch } ?: -1) >= (held.epochs.maxOfOrNull { it.epoch } ?: -1)) {
                    byId[c.channelIdHex] = c
                }
            }
            return OwnedList(mine.passwordLockRoot, mine.passkeyLocks, byId.values.sortedBy { it.channelIdHex })
        }
    }

    /** The storage service: versioned heads by root. */
    private class Vaults : OwnedVaults {
        val heads = HashMap<String, Pair<Long, ByteArray>>()
        val log = ArrayList<String>()
        var conflictNextListWrite = 0
        var unreadable: String? = null
        override fun of(root: ByteArray): OwnedVault {
            val name = ownedHex(root)
            return object : OwnedVault {
                override suspend fun read(): OwnedRead {
                    if (name == unreadable) return OwnedRead.Unreadable
                    val head = heads[name] ?: return OwnedRead.Missing
                    return OwnedRead.Found(head.first, head.second)
                }
                override suspend fun write(version: Long, plain: ByteArray): OwnedWrite {
                    val isList = plain.toString(Charsets.UTF_8).startsWith("L:")
                    if (isList && conflictNextListWrite > 0) {
                        conflictNextListWrite -= 1
                        return OwnedWrite.CONFLICT
                    }
                    val held = heads[name]
                    if ((held?.first ?: 0L) + 1 != version) return OwnedWrite.CONFLICT
                    heads[name] = version to plain
                    log += "write:${name.take(2)}:" + plain.toString(Charsets.UTF_8).take(2)
                    return OwnedWrite.WRITTEN
                }
                override suspend fun delete() {
                    if (heads.remove(name) != null) log += "delete:${name.take(2)}"
                }
            }
        }
    }

    private val keysRoot = bytes(0x11)
    private val serverHalf = bytes(0x05)
    private val export = bytes(0x20)

    private fun facts(
        channels: List<OwnedEntry> = listOf(entry(1)),
        exportKey: ByteArray? = export,
        heldServerHalf: ByteArray? = serverHalf,
        heldListKey: ByteArray? = null,
        deleted: List<String> = emptyList(),
        newPasskeys: Map<String, ByteArray> = emptyMap(),
        live: Set<String>? = null,
        rotatedAt: Map<String, Long> = emptyMap(),
    ) = OwnedDeviceFacts(keysRoot, exportKey, heldServerHalf, heldListKey, channels, rotatedAt, deleted, newPasskeys, live)

    // ---- the entries -------------------------------------------------------

    @Test
    fun `an entry keeps the newest four epoch seeds and the newest eight admission seeds`() {
        val e = entry(1, epochs = (1L..7L).toList(), admission = (0..9).toList())
        assertEquals(listOf(4L, 5L, 6L, 7L), e.epochs.map { it.epoch })
        assertEquals((2..9).toList(), e.admission.map { it.version })
        // A read key that is not one is left out; an id is lower case.
        assertNull(ownedEntryOf("AB".repeat(16), ByteArray(5), emptyList(), emptyList()).readKey)
        assertEquals("ab".repeat(16), ownedEntryOf("AB".repeat(16), null, emptyList(), emptyList()).channelIdHex)
        // Neither prints a secret.
        assertEquals("OwnedEntry(admission=8, epochs=4, handleSeed=false)", e.toString())
        assertFalse(e.epochs[0].toString().contains(ownedHex(e.epochs[0].seed)))
    }

    @Test
    fun `the list holds at most 64 channels, the most recently rotated ones`() {
        val many = (0 until 70).map { entry(it) }
        val rotated = mapOf(hex(69) to 900L, hex(68) to 800L)
        val kept = ownedEntriesToList(many, rotated).map { it.channelIdHex }
        assertEquals(OWNED_MAX_CHANNELS, kept.size)
        assertTrue(hex(69) in kept && hex(68) in kept)
        // The rest by channel id: the highest ids fall off.
        assertFalse(hex(67) in kept)
        assertEquals(kept.sorted(), kept)
    }

    @Test
    fun `the digest changes with a seed, a root or a channel and not with the order`() {
        val a = OwnedList(bytes(1), mapOf("t" to bytes(2)), listOf(entry(1), entry(2)))
        assertEquals(ownedListDigest(a), ownedListDigest(OwnedList(bytes(1), mapOf("t" to bytes(2)), listOf(entry(2), entry(1)))))
        assertFalse(ownedListDigest(a) == ownedListDigest(OwnedList(bytes(9), mapOf("t" to bytes(2)), a.channels)))
        assertFalse(ownedListDigest(a) == ownedListDigest(OwnedList(bytes(1), emptyMap(), a.channels)))
        assertFalse(ownedListDigest(a) == ownedListDigest(OwnedList(bytes(1), mapOf("t" to bytes(2)), listOf(entry(1), entry(2, epochs = listOf(1L, 2L))))))
        assertEquals("OwnedList(channels=2, passkeyLocks=1)", a.toString())
    }

    // ---- a device with the keys --------------------------------------------

    @Test
    fun `the first pass makes the keys lock, the password lock and the list, each lock before the list names it`() = runTest {
        val vaults = Vaults()
        val done = syncOwnedChannels(facts(), Core, vaults) as OwnedSync.Done
        assertTrue(done.wrote && done.keysLockHolds)
        val passwordRoot = Core.passwordLockRoot(export, serverHalf)
        assertTrue(done.list.passwordLockRoot.contentEquals(passwordRoot))
        // The keys lock (R || S), then the password lock (R), then the list.
        assertEquals(listOf("K:", "R:", "L:"), vaults.log.map { it.removePrefix("write:").substringAfter(":") })
        assertNotNull(vaults.heads[ownedHex(keysRoot)])
        assertNotNull(vaults.heads[ownedHex(passwordRoot)])
        assertTrue(Core.decodeListKeyLock(vaults.heads[ownedHex(passwordRoot)]!!.second).contentEquals(done.listKey))
        assertEquals(listOf(hex(1)), done.list.channels.map { it.channelIdHex })
    }

    @Test
    fun `a pass with nothing new writes nothing`() = runTest {
        val vaults = Vaults()
        syncOwnedChannels(facts(), Core, vaults)
        val before = vaults.log.size
        val again = syncOwnedChannels(facts(heldServerHalf = null), Core, vaults) as OwnedSync.Done
        assertFalse(again.wrote)
        assertEquals(before, vaults.log.size)
    }

    @Test
    fun `without a keys lock and without the server half nothing is made`() = runTest {
        val vaults = Vaults()
        assertEquals(OwnedSync.NoServerHalf, syncOwnedChannels(facts(heldServerHalf = null), Core, vaults))
        assertTrue(vaults.heads.isEmpty())
    }

    @Test
    fun `a keys lock this device's lineage does not open is left alone`() = runTest {
        val vaults = Vaults().apply { unreadable = ownedHex(keysRoot) }
        assertEquals(OwnedSync.NotNow, syncOwnedChannels(facts(), Core, vaults))
        assertTrue(vaults.log.isEmpty())
    }

    @Test
    fun `a changed password makes the new lock, then the list, then deletes the old lock`() = runTest {
        val vaults = Vaults()
        syncOwnedChannels(facts(), Core, vaults)
        val oldRoot = Core.passwordLockRoot(export, serverHalf)
        vaults.log.clear()
        val newExport = bytes(0x30)
        val done = syncOwnedChannels(facts(exportKey = newExport, heldServerHalf = null), Core, vaults) as OwnedSync.Done
        val newRoot = Core.passwordLockRoot(newExport, serverHalf)
        assertTrue(done.list.passwordLockRoot.contentEquals(newRoot))
        assertEquals(
            listOf("write:${ownedHex(newRoot).take(2)}:R:", "write:${ownedHex(done.listKey).take(2)}:L:", "delete:${ownedHex(oldRoot).take(2)}"),
            vaults.log,
        )
        assertNull(vaults.heads[ownedHex(oldRoot)])
    }

    @Test
    fun `a device that does not hold the current password leaves the password lock as it is`() = runTest {
        val vaults = Vaults()
        syncOwnedChannels(facts(), Core, vaults)
        val root = Core.passwordLockRoot(export, serverHalf)
        // A rotation on a device with no export key (or a stale one): the
        // list is written with the new epoch, the lock root carried forward.
        val done = syncOwnedChannels(
            facts(channels = listOf(entry(1, epochs = listOf(1L, 2L))), exportKey = null, heldServerHalf = null),
            Core,
            vaults,
        ) as OwnedSync.Done
        assertTrue(done.wrote)
        assertTrue(done.list.passwordLockRoot.contentEquals(root))
        assertNotNull(vaults.heads[ownedHex(root)])
        assertEquals(listOf(1L, 2L), done.list.channels.single().epochs.map { it.epoch })
    }

    @Test
    fun `a passkey that keeps the chats gets its own lock, and loses it when it is deleted`() = runTest {
        val vaults = Vaults()
        syncOwnedChannels(facts(), Core, vaults)
        val prf = bytes(0x60)
        val added = syncOwnedChannels(
            facts(heldServerHalf = null, newPasskeys = mapOf("aa" to prf)),
            Core,
            vaults,
        ) as OwnedSync.Done
        val passkeyRoot = Core.passkeyLockRoot(prf, serverHalf)
        assertTrue(added.list.passkeyLocks.getValue("aa").contentEquals(passkeyRoot))
        assertTrue(Core.decodeListKeyLock(vaults.heads[ownedHex(passkeyRoot)]!!.second).contentEquals(added.listKey))
        // Still listed while the account's passkeys could not be read.
        val unknown = syncOwnedChannels(facts(heldServerHalf = null), Core, vaults) as OwnedSync.Done
        assertEquals(setOf("aa"), unknown.list.passkeyLocks.keys)
        // The passkey was removed: the list drops it, then its lock goes.
        val removed = syncOwnedChannels(facts(heldServerHalf = null, live = emptySet()), Core, vaults) as OwnedSync.Done
        assertTrue(removed.list.passkeyLocks.isEmpty())
        assertNull(vaults.heads[ownedHex(passkeyRoot)])
    }

    @Test
    fun `another owner device's channels are kept, and a deleted channel leaves the list`() = runTest {
        val vaults = Vaults()
        // Device one knows channels 1 and 2.
        syncOwnedChannels(facts(channels = listOf(entry(1), entry(2))), Core, vaults)
        // Device two knows only channel 3: the merge keeps what it does not know.
        val merged = syncOwnedChannels(facts(channels = listOf(entry(3)), heldServerHalf = null), Core, vaults) as OwnedSync.Done
        assertEquals(listOf(hex(1), hex(2), hex(3)), merged.list.channels.map { it.channelIdHex })
        // Channel 2 is deleted on device one.
        val deleted = syncOwnedChannels(
            facts(channels = listOf(entry(1)), heldServerHalf = null, deleted = listOf(hex(2))),
            Core,
            vaults,
        ) as OwnedSync.Done
        assertEquals(listOf(hex(1), hex(3)), deleted.list.channels.map { it.channelIdHex })
    }

    @Test
    fun `a version conflict reads, merges and writes again, and gives up after three rounds`() = runTest {
        val vaults = Vaults()
        syncOwnedChannels(facts(), Core, vaults)
        vaults.conflictNextListWrite = 2
        val done = syncOwnedChannels(
            facts(channels = listOf(entry(1), entry(4)), heldServerHalf = null),
            Core,
            vaults,
        ) as OwnedSync.Done
        assertEquals(listOf(hex(1), hex(4)), done.list.channels.map { it.channelIdHex })
        vaults.conflictNextListWrite = OWNED_WRITE_ATTEMPTS
        assertEquals(
            OwnedSync.NotNow,
            syncOwnedChannels(facts(channels = listOf(entry(1), entry(4), entry(5)), heldServerHalf = null), Core, vaults),
        )
    }

    // ---- a reset -----------------------------------------------------------

    @Test
    fun `the password and the server half together open the list for a reset - neither does alone`() = runTest {
        val vaults = Vaults()
        val made = syncOwnedChannels(facts(channels = listOf(entry(1), entry(2))), Core, vaults) as OwnedSync.Done
        val opened = openOwnedForReset(Core.passwordLockRoot(export, serverHalf), serverHalf, Core, vaults)!!
        assertTrue(opened.listKey.contentEquals(made.listKey))
        assertEquals(listOf(hex(1), hex(2)), opened.list.channels.map { it.channelIdHex })
        assertFalse(opened.toString().contains(ownedHex(opened.listKey)))
        // Another password, or another server half: no lock is found.
        assertNull(openOwnedForReset(Core.passwordLockRoot(bytes(0x33), serverHalf), serverHalf, Core, vaults))
        assertNull(openOwnedForReset(Core.passwordLockRoot(export, bytes(0x06)), bytes(0x06), Core, vaults))
    }

    @Test
    fun `a passkey that sealed the reset opens the list through its own lock`() = runTest {
        val vaults = Vaults()
        val prf = bytes(0x60)
        syncOwnedChannels(facts(newPasskeys = mapOf("aa" to prf)), Core, vaults)
        // The password was changed by a mailed code since: only the passkey
        // lock opens.
        assertNull(openOwnedForReset(Core.passwordLockRoot(bytes(0x44), serverHalf), serverHalf, Core, vaults))
        assertNotNull(openOwnedForReset(Core.passkeyLockRoot(prf, serverHalf), serverHalf, Core, vaults))
    }

    @Test
    fun `after the reset the same list key goes under the new keys lock`() = runTest {
        val vaults = Vaults()
        syncOwnedChannels(facts(channels = listOf(entry(1))), Core, vaults)
        val opened = openOwnedForReset(Core.passwordLockRoot(export, serverHalf), serverHalf, Core, vaults)!!
        // New keys: a new keys-lock root; the device holds R and S from the
        // lock the reset opened.
        val newKeysRoot = bytes(0x12)
        val after = syncOwnedChannels(
            OwnedDeviceFacts(newKeysRoot, export, opened.serverHalf, opened.listKey, opened.list.channels),
            Core,
            vaults,
        ) as OwnedSync.Done
        assertTrue("the recovery keys stay the same", after.listKey.contentEquals(opened.listKey))
        val (r, s) = Core.decodeKeysLock(vaults.heads[ownedHex(newKeysRoot)]!!.second)
        assertTrue(r.contentEquals(opened.listKey) && s.contentEquals(serverHalf))
        assertTrue(after.keysLockHolds)
    }

    @Test
    fun `taking the channels back keeps only the unanswered ones owed`() = runTest {
        val entries = listOf(entry(1), entry(2), entry(3), entry(4))
        val owed = retakeOwnedChannels(entries) { e ->
            when (e.channelIdHex) {
                hex(1) -> OwnedRetakeOutcome.TAKEN
                hex(2) -> OwnedRetakeOutcome.REFUSED
                hex(3) -> OwnedRetakeOutcome.RETRY
                else -> throw java.io.IOException("offline")
            }
        }
        assertEquals(listOf(hex(3), hex(4)), owed.map { it.channelIdHex })
    }

    // ---- with the account --------------------------------------------------

    @Test
    fun `deleting the account deletes the list and every lock it names`() = runTest {
        val vaults = Vaults()
        syncOwnedChannels(facts(newPasskeys = mapOf("aa" to bytes(0x60))), Core, vaults)
        assertEquals(4, vaults.heads.size)
        deleteOwnedChannels(keysRoot, Core, vaults)
        assertTrue(vaults.heads.isEmpty())
        // Nothing there is not an error.
        deleteOwnedChannels(keysRoot, Core, vaults)
    }

    // ---- what the device keeps ---------------------------------------------

    private class MapStorage : MarkerStorage {
        val rows = HashMap<String, String>()
        override fun get(key: String): String? = rows[key]
        override fun set(key: String, value: String) {
            rows[key] = value
        }
        override fun remove(key: String) {
            rows.remove(key)
        }
    }

    @Test
    fun `the server half is kept only until the keys lock holds it`() {
        val storage = MapStorage()
        val store = OwnedChannelsSecretStore(storage)
        assertNull(store.read("a").serverHalf)
        store.keepServerHalf("a", serverHalf)
        assertTrue(OwnedChannelsSecretStore(storage).read("a").serverHalf!!.contentEquals(serverHalf))
        store.dropSecrets("a")
        assertTrue(storage.rows.isEmpty())
    }

    @Test
    fun `what a reset opened is kept while channels are still owed`() {
        val storage = MapStorage()
        val store = OwnedChannelsSecretStore(storage)
        store.keepRetake("a", bytes(0x70), serverHalf, "L:x".toByteArray(), listOf(hex(1), hex(2)))
        // The keys lock holds R and S, but two channels are still to take back.
        store.dropSecrets("a")
        val held = store.read("a")
        assertEquals(listOf(hex(1), hex(2)), held.owedHex)
        assertTrue(held.listKey!!.contentEquals(bytes(0x70)))
        assertNotNull(held.retakeList)
        store.setOwed("a", listOf(hex(2)))
        assertEquals(listOf(hex(2)), store.read("a").owedHex)
        store.setOwed("a", emptyList())
        assertNull("the opened list goes with the last owed channel", store.read("a").retakeList)
        store.dropSecrets("a")
        assertTrue(storage.rows.isEmpty())
        // A reset that did not happen keeps nothing.
        store.keepRetake("a", bytes(0x70), serverHalf, "L:x".toByteArray(), listOf(hex(1)))
        store.clear("a")
        assertTrue(storage.rows.isEmpty())
    }

    @Test
    fun `a signed-out account's secrets are not written, and the facts digest is per account`() {
        val storage = MapStorage()
        AccountStoreGate.seal("a")
        runCatching { OwnedChannelsSecretStore(storage).keepServerHalf("a", serverHalf) }
        assertTrue(storage.rows.isEmpty())
        AccountStoreGate.resetForTest()
        val facts = OwnedChannelsFactsStore(MapStorage())
        assertNull(facts.digest("a"))
        facts.setDigest("a", "d1")
        assertEquals("d1", facts.digest("a"))
        assertNull(facts.digest("b"))
    }

    // ---- alignment 2026-11-27 ----------------------------------------------

    @Test
    fun `an entry carries the channel's handle write seed and the digest sees it`() {
        val withSeed = ownedEntryOf(hex(1), null, emptyList(), listOf(OwnedEpoch(1L, bytes(0x51))), handleSeed = bytes(0x33))
        assertTrue(withSeed.handleSeed!!.contentEquals(bytes(0x33)))
        // A seed that is not one (wrong length, all zero) is left out.
        assertNull(ownedEntryOf(hex(1), null, emptyList(), emptyList(), handleSeed = ByteArray(16)).handleSeed)
        assertNull(ownedEntryOf(hex(1), null, emptyList(), emptyList(), handleSeed = ByteArray(32)).handleSeed)
        val without = ownedEntryOf(hex(1), null, emptyList(), listOf(OwnedEpoch(1L, bytes(0x51))))
        assertFalse(
            ownedListDigest(OwnedList(OWNED_NO_ROOT, emptyMap(), listOf(withSeed))) ==
                ownedListDigest(OwnedList(OWNED_NO_ROOT, emptyMap(), listOf(without))),
        )
    }

    @Test
    fun `a private channel is retaken by tickets, a public one or an empty entry lets every listed row in`() {
        assertEquals(RetakeAdmission.TICKETS, retakeAdmission(entry(1, admission = listOf(1))))
        assertEquals(RetakeAdmission.EVERYONE_LISTED, retakeAdmission(entry(1, admission = listOf(1), readKey = bytes(0x09))))
        assertEquals(RetakeAdmission.EVERYONE_LISTED, retakeAdmission(entry(1, admission = emptyList())))
    }

    @Test
    fun `a channel retaken by tickets stays judged by tickets when it is marked again`() {
        val store = RecoveredChannelsStore(MapStorage())
        store.mark("a", hex(1), 0L, judgedByTickets = true)
        store.mark("a", hex(1), 5L)
        val held = store.read("a").getValue(hex(1))
        assertEquals(5L, held.throughSeq)
        assertTrue(held.judgedByTickets)
    }

    @Test
    fun `a recall opens the old list through the keys lock`() = runTest {
        val vaults = Vaults()
        val listKey = bytes(0x71)
        vaults.heads[ownedHex(keysRoot)] = 1L to Core.encodeKeysLock(listKey, serverHalf)
        val list = OwnedList(OWNED_NO_ROOT, emptyMap(), listOf(entry(1), entry(2)))
        vaults.heads[ownedHex(listKey)] = 1L to Core.encodeList(list)
        val opened = openOwnedThroughKeysLock(keysRoot, Core, vaults)
        assertNotNull(opened)
        assertTrue(opened!!.listKey.contentEquals(listKey))
        assertTrue(opened.serverHalf.contentEquals(serverHalf))
        assertEquals(listOf(hex(1), hex(2)), opened.list.channels.map { it.channelIdHex })
        // No lock: nothing to retake.
        assertNull(openOwnedThroughKeysLock(bytes(0x12), Core, vaults))
    }

    @Test
    fun `a recall's old keys lock root is kept until the pass opened it`() {
        val storage = MapStorage()
        val store = OwnedChannelsSecretStore(storage)
        store.keepRetakeRoot("a", bytes(0x44))
        assertTrue(store.read("a").retakeRoot!!.contentEquals(bytes(0x44)))
        // The keys lock holding S and R does not drop a root still owed.
        store.dropSecrets("a")
        assertNotNull(OwnedChannelsSecretStore(storage).read("a").retakeRoot)
        store.dropRetakeRoot("a")
        assertNull(store.read("a").retakeRoot)
        assertTrue(storage.rows.isEmpty())
    }

    @Test
    fun `a passkey's lock secret survives until its lock is written`() {
        val storage = MapStorage()
        val store = OwnedChannelsSecretStore(storage)
        store.keepPendingPasskey("a", "aa", bytes(0x61))
        store.keepPendingPasskey("a", "bb", bytes(0x62))
        // A new process reads both back.
        val held = OwnedChannelsSecretStore(storage).pendingPasskeys("a")
        assertEquals(setOf("aa", "bb"), held.keys)
        assertTrue(held.getValue("aa").contentEquals(bytes(0x61)))
        store.dropPendingPasskeys("a", setOf("aa"))
        assertEquals(setOf("bb"), store.pendingPasskeys("a").keys)
        store.dropPendingPasskeys("a", setOf("bb"))
        assertTrue(store.pendingPasskeys("a").isEmpty())
        assertTrue(storage.rows.isEmpty())
    }

    @Test
    fun `a lock that keeps losing its write fails the pass instead of being dropped`() = runTest {
        val vaults = object : OwnedVaults {
            val inner = Vaults()
            override fun of(root: ByteArray): OwnedVault {
                val v = inner.of(root)
                // The passkey lock's vault always loses its write.
                if (ownedHex(root) == ownedHex(Core.passkeyLockRoot(bytes(0x61), serverHalf))) {
                    return object : OwnedVault by v {
                        override suspend fun write(version: Long, plain: ByteArray): OwnedWrite = OwnedWrite.CONFLICT
                    }
                }
                return v
            }
        }
        var threw = false
        try {
            syncOwnedChannels(facts(newPasskeys = mapOf("aa" to bytes(0x61))), Core, vaults)
        } catch (_: OwnedLockNotWritten) {
            threw = true
        }
        assertTrue(threw)
        // The list was not written naming a lock that does not exist.
        val listHeads = vaults.inner.heads.values.count { it.second.toString(Charsets.UTF_8).startsWith("L:") }
        assertEquals(0, listHeads)
    }
}
