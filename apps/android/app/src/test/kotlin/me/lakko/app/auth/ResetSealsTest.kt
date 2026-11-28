// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.coroutines.test.runTest
import me.lakko.app.account.AccountStoreGate
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Test

/**
 * ADR-0101 section 10 - the seal announcement an account publishes, and
 * whether a reset made now would be sealed (UX 15.1c). The core is behind
 * [SealCore]; a fake stands in (the host JVM loads no native library).
 */
class ResetSealsTest {

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

    @Before
    fun setUp() = AccountStoreGate.resetForTest()

    @After
    fun tearDown() = AccountStoreGate.resetForTest()

    private fun password(pk: String) = SealEntryView(SealKindName.PASSWORD, SEAL_TAG_NONE_HEX, pk)
    private fun phrase(pk: String) = SealEntryView(SealKindName.PHRASE, SEAL_TAG_NONE_HEX, pk)
    private fun passkey(tag: String, pk: String = "pk-$tag") = SealEntryView(SealKindName.PASSKEY, tag, pk)

    private fun view(vararg entries: SealEntryView, generation: Long = 1, seq: Long = 3, issuedAtMs: Long = 1_000) =
        SealAnnouncementView(generation, seq, entries.toList(), issuedAtMs)

    // ---- what the announcement should hold ---------------------------------

    @Test
    fun `nothing can be announced without a password seal from somewhere`() {
        assertNull(desiredSealEntries(null, SealMaterial(passwordPkB64 = null, phrasePkB64 = "ph")))
        // The announced one is carried forward by a device that never held
        // the password.
        assertEquals(
            listOf(password("pw")),
            desiredSealEntries(view(password("pw")), SealMaterial(passwordPkB64 = null, phrasePkB64 = null)),
        )
    }

    @Test
    fun `a saved phrase is never un-saved, and this device's replaces nothing it does not hold`() {
        val current = view(password("pw"), phrase("ph"))
        // A device without the words keeps the announced phrase seal.
        assertEquals(
            listOf(password("pw"), phrase("ph")),
            desiredSealEntries(current, SealMaterial(passwordPkB64 = "pw", phrasePkB64 = null)),
        )
        // Saving a phrase adds it.
        assertEquals(
            listOf(password("pw"), phrase("mine")),
            desiredSealEntries(view(password("pw")), SealMaterial(passwordPkB64 = "pw", phrasePkB64 = "mine")),
        )
    }

    @Test
    fun `passkey seals follow the account's passkeys - kept, replaced, dropped, bounded`() {
        val current = view(password("pw"), passkey("aa"), passkey("bb"))
        // The list could not be read: every announced seal is kept.
        assertEquals(
            listOf(password("pw"), passkey("aa"), passkey("bb")),
            desiredSealEntries(current, SealMaterial("pw", null)),
        )
        // A passkey that was deleted no longer keeps the chats.
        assertEquals(
            listOf(password("pw"), passkey("bb")),
            desiredSealEntries(current, SealMaterial("pw", null, livePasskeyTagsHex = setOf("bb"))),
        )
        // A new seal of the same tag replaces the announced one.
        assertEquals(
            listOf(password("pw"), passkey("aa", "new"), passkey("bb")),
            desiredSealEntries(current, SealMaterial("pw", null, newPasskeys = listOf(passkey("aa", "new")))),
        )
        // A passkey just added is kept even when the list read does not name
        // it yet.
        assertEquals(
            listOf(password("pw"), passkey("cc")),
            desiredSealEntries(
                current,
                SealMaterial("pw", null, newPasskeys = listOf(passkey("cc")), livePasskeyTagsHex = emptySet()),
            ),
        )
        // At most eight, the new ones first.
        val many = view(password("pw"), *(1..8).map { passkey("%02d".format(it)) }.toTypedArray())
        val bounded = desiredSealEntries(many, SealMaterial("pw", null, newPasskeys = listOf(passkey("zz"))))!!
        assertEquals(SEAL_MAX_PASSKEYS, bounded.count { it.kind == SealKindName.PASSKEY })
        assertTrue(passkey("zz") in bounded)
        assertFalse(passkey("08") in bounded)
    }

    @Test
    fun `only a password typed in this run puts its seal forward`() {
        val current = view(password("new-pw"), issuedAtMs = 5_000)
        // A password sign-in, sign-up or change in this run: its seal is the
        // current password's, announced or not.
        assertEquals("mine", passwordSealToAssert(null, "mine", fromThisRun = true))
        assertEquals("mine", passwordSealToAssert(current, "mine", fromThisRun = true))
        // The same key as announced: nothing to decide.
        assertEquals("new-pw", passwordSealToAssert(current, "new-pw", fromThisRun = false))
        // A cached export key from an earlier run never puts a seal forward,
        // not even into an account that has no announcement yet: the
        // password may have been changed elsewhere since (the web rule).
        assertNull(passwordSealToAssert(current, "mine", fromThisRun = false))
        assertNull(passwordSealToAssert(null, "mine", fromThisRun = false))
        assertNull(passwordSealToAssert(current, null, fromThisRun = true))
    }

    @Test
    fun `a landing with only a cached export key and no announcement publishes nothing`() = runTest {
        val directory = FakeDirectory(bundle(current = null))
        val outcome = announceSeals(account, SealMaterial("cached", null, passwordFromThisRun = false), FakeCore(), directory, 0, 50)
        assertEquals(AnnounceOutcome.NoPasswordSeal, outcome)
        assertTrue(directory.uploads.isEmpty())
    }

    @Test
    fun `the announcement is due when there is none for this generation or its entries differ`() {
        val desired = listOf(password("pw"), phrase("ph"))
        assertTrue(announcementDue(null, desired, keyGeneration = 1))
        assertTrue(announcementDue(view(password("pw")), desired, keyGeneration = 1))
        assertTrue(announcementDue(view(password("pw"), phrase("ph"), generation = 1), desired, keyGeneration = 2))
        // The order of the entries is not a difference.
        assertFalse(announcementDue(view(phrase("ph"), password("pw")), desired, keyGeneration = 1))
    }

    @Test
    fun `a number is above every one this device knows of`() {
        assertEquals(1L, nextSealSeq())
        assertEquals(1L, nextSealSeq(null, null))
        assertEquals(8L, nextSealSeq(3L, 7L, null, 5L))
    }

    // ---- will this reset bring things back? --------------------------------

    @Test
    fun `a reset is sealed by the first seal key the account announced`() {
        val current = view(password("pw"), passkey("aa"), phrase("ph"), seq = 9)
        val sealed = predictSeal(current, listOf(SealCandidate(password("pw")), SealCandidate(phrase("ph"))))
        assertEquals(SealPrediction.Sealed(password("pw"), announcementSeq = 9), sealed)
        assertEquals("restore_identity.reset.sealed", sealPredictionKey(sealed))
        // The password was changed by a mailed code since: the phrase seals.
        assertEquals(
            SealPrediction.Sealed(phrase("ph"), 9),
            predictSeal(current, listOf(SealCandidate(password("other")), SealCandidate(phrase("ph")))),
        )
    }

    @Test
    fun `a reset nothing announced signs is unsealed, and says when a passkey could seal it`() {
        val current = view(password("pw"), passkey("aa"))
        val unsealed = predictSeal(current, listOf(SealCandidate(password("other"))))
        assertEquals(SealPrediction.Unsealed(passkeyCouldSeal = true), unsealed)
        assertEquals("restore_identity.reset.unsealed", sealPredictionKey(unsealed))
        // The passkey that keeps the chats WAS the one used and its secret
        // did not match: offering it again would go nowhere.
        assertEquals(
            SealPrediction.Unsealed(passkeyCouldSeal = false),
            predictSeal(current, listOf(SealCandidate(password("other")), SealCandidate(passkey("aa", "wrong")))),
        )
        // No passkey keeps the chats.
        assertEquals(
            SealPrediction.Unsealed(passkeyCouldSeal = false),
            predictSeal(view(password("pw")), listOf(SealCandidate(password("other")))),
        )
        // The account's protection could not be read: never a promise.
        assertEquals(SealPrediction.Unsealed(false), predictSeal(null, listOf(SealCandidate(password("pw")))))
    }

    @Test
    fun `the kept-by answer reads the announcement`() {
        assertEquals(ChatsKeptBy.PasswordOnly, view(password("pw")).keptBy)
        assertEquals(ChatsKeptBy.PasskeySeal, view(password("pw"), passkey("aa")).keptBy)
        assertEquals(ChatsKeptBy.Phrase, view(password("pw"), passkey("aa"), phrase("ph")).keptBy)
    }

    // ---- publishing --------------------------------------------------------

    /** A core that "signs" by writing the view down as text. */
    private class FakeCore : SealCore {
        val signed = ArrayList<SealAnnouncementView>()
        override fun entryOf(secret: SealSecret): SealEntryView = when (secret) {
            is SealSecret.Password -> SealEntryView(SealKindName.PASSWORD, SEAL_TAG_NONE_HEX, "pw:" + secret.exportKey[0])
            is SealSecret.Passkey -> SealEntryView(SealKindName.PASSKEY, passkeyTagHex(secret.credentialId), "pk:" + secret.prfOutput[0])
            is SealSecret.Phrase -> SealEntryView(SealKindName.PHRASE, SEAL_TAG_NONE_HEX, "ph:" + secret.phrase)
        }
        override fun passkeyTagHex(credentialId: ByteArray): String = "tag" + credentialId[0]
        override fun open(announcement: ByteArray, accountKey: ByteArray): SealAnnouncementView? {
            val text = announcement.toString(Charsets.UTF_8)
            if (!text.startsWith("key" + accountKey[0] + "|")) return null
            return decode(text.substringAfter("|"))
        }
        override fun decodeUnverified(announcement: ByteArray): SealAnnouncementView? =
            decode(announcement.toString(Charsets.UTF_8).substringAfter("|"))
        override fun signAnnouncement(
            accountSeed: ByteArray,
            accountId: ByteArray,
            keyGeneration: Long,
            seq: Long,
            issuedAtMs: Long,
            entries: List<SealEntryView>,
        ): ByteArray {
            val view = SealAnnouncementView(keyGeneration, seq, entries, issuedAtMs)
            signed += view
            return encode(accountSeed[0], view)
        }
        override fun signResetSeal(
            secret: SealSecret,
            accountId: ByteArray,
            announcementSeq: Long,
            fromGeneration: Long,
            newSignaturePubkey: ByteArray,
        ): ByteArray = "seal:$announcementSeq:$fromGeneration".toByteArray()

        companion object {
            private val json = kotlinx.serialization.json.Json
            fun encode(key: Byte, view: SealAnnouncementView): ByteArray =
                ("key$key|" + json.encodeToString(SealAnnouncementView.serializer(), view)).toByteArray()
            fun decode(text: String): SealAnnouncementView? =
                runCatching { json.decodeFromString(SealAnnouncementView.serializer(), text) }.getOrNull()
        }
    }

    private class FakeDirectory(var own: OwnSealBundle?) : SealDirectoryPorts {
        val uploads = ArrayList<ByteArray>()
        var staleOnce = false
        var staleAlways = false
        var fetches = 0
        override suspend fun fetchOwn(): OwnSealBundle? {
            fetches += 1
            return own
        }
        override suspend fun upload(announcement: ByteArray) {
            if (staleAlways) throw SealAnnouncementStale()
            if (staleOnce) {
                staleOnce = false
                throw SealAnnouncementStale()
            }
            uploads += announcement
        }
    }

    private val key = ByteArray(32) { 7 }
    private val account = SealAccount(accountId = ByteArray(16) { 1 }, accountKey = key, accountSeed = ByteArray(64) { 7 }, keyGeneration = 2)

    private fun bundle(current: SealAnnouncementView? = null, previous: SealAnnouncementView? = null, generation: Long = 2) =
        OwnSealBundle(
            signaturePubkey = key,
            keyGeneration = generation,
            resetSeals = current?.let { FakeCore.encode(7, it) },
            previousResetSeals = previous?.let { FakeCore.encode(9, it) },
        )

    @Test
    fun `the first announcement of a generation is published above the previous generation's number`() = runTest {
        val core = FakeCore()
        // After a reset: no announcement of generation 2 yet; the directory
        // still serves generation 1's, signed by the key the device no
        // longer holds. Its number is read without its signature.
        val directory = FakeDirectory(bundle(previous = view(password("old"), generation = 1, seq = 6)))
        val outcome = announceSeals(account, SealMaterial("pw", null, passwordFromThisRun = true), core, directory, seqFloor = 2, nowMs = 50)
        assertEquals(
            AnnounceOutcome.Published(SealAnnouncementView(2, 7, listOf(password("pw")), 50)),
            outcome,
        )
        assertEquals(1, directory.uploads.size)
    }

    @Test
    fun `an announcement that already says it is not sent again`() = runTest {
        val current = view(password("pw"), phrase("ph"), generation = 2, seq = 4)
        val directory = FakeDirectory(bundle(current = current))
        val outcome = announceSeals(account, SealMaterial("pw", "ph"), FakeCore(), directory, 0, 50)
        assertEquals(AnnounceOutcome.Current(current), outcome)
        assertTrue(directory.uploads.isEmpty())
    }

    @Test
    fun `saving a phrase publishes the announcement again with the phrase in it`() = runTest {
        val current = view(password("pw"), generation = 2, seq = 4)
        val directory = FakeDirectory(bundle(current = current))
        val outcome = announceSeals(account, SealMaterial("pw", "ph"), FakeCore(), directory, seqFloor = 4, nowMs = 50)
        val published = (outcome as AnnounceOutcome.Published).view
        assertEquals(5L, published.seq)
        assertEquals(ChatsKeptBy.Phrase, published.keptBy)
    }

    @Test
    fun `a device that never held the password and finds none announced waits`() = runTest {
        val directory = FakeDirectory(bundle())
        assertEquals(
            AnnounceOutcome.NoPasswordSeal,
            announceSeals(account, SealMaterial(null, "ph"), FakeCore(), directory, 0, 50),
        )
        assertTrue(directory.uploads.isEmpty())
    }

    @Test
    fun `nothing is announced over a row that is not this device's key or generation`() = runTest {
        val core = FakeCore()
        // No row yet: the landing rule and the publish come first.
        assertEquals(AnnounceOutcome.NotThisKey, announceSeals(account, SealMaterial("pw", null), core, FakeDirectory(null), 0, 1))
        // A reset or rotation this device has not caught up with.
        val otherKey = FakeDirectory(OwnSealBundle(ByteArray(32) { 8 }, 2, null, null))
        assertEquals(AnnounceOutcome.NotThisKey, announceSeals(account, SealMaterial("pw", null), core, otherKey, 0, 1))
        val otherGeneration = FakeDirectory(bundle(generation = 3))
        assertEquals(AnnounceOutcome.NotThisKey, announceSeals(account, SealMaterial("pw", null), core, otherGeneration, 0, 1))
        assertTrue(core.signed.isEmpty())
    }

    @Test
    fun `a stale number is answered by reading again once, above the refused one`() = runTest {
        val core = FakeCore()
        val directory = FakeDirectory(bundle()).apply { staleOnce = true }
        val outcome = announceSeals(account, SealMaterial("pw", null, passwordFromThisRun = true), core, directory, seqFloor = 0, nowMs = 9)
        assertEquals(2, directory.fetches)
        assertEquals(listOf(1L, 2L), core.signed.map { it.seq })
        assertEquals(2L, (outcome as AnnounceOutcome.Published).view.seq)
    }

    @Test
    fun `a directory that keeps refusing is a failure, not a loop`() = runTest {
        val directory = FakeDirectory(bundle()).apply { staleAlways = true }
        try {
            announceSeals(account, SealMaterial("pw", null, passwordFromThisRun = true), FakeCore(), directory, 0, 9)
            fail("two refusals must throw")
        } catch (_: SealAnnouncementStale) {
            assertEquals(2, directory.fetches)
        }
    }

    @Test
    fun `an announcement of another generation under this key is not this generation's`() = runTest {
        // The row still carries generation 1's announcement as current.
        val directory = FakeDirectory(bundle(current = view(password("pw"), generation = 1, seq = 4)))
        val outcome = announceSeals(account, SealMaterial("pw", null, passwordFromThisRun = true), FakeCore(), directory, seqFloor = 4, nowMs = 9)
        assertEquals(5L, (outcome as AnnounceOutcome.Published).view.seq)
        assertEquals(2L, outcome.view.keyGeneration)
    }

    // ---- what the device remembers -----------------------------------------

    @Test
    fun `the store keeps the last announcement and never lowers the number floor`() {
        val storage = MapStorage()
        val store = SealStateStore(storage)
        assertNull(store.view("a"))
        assertEquals(0L, store.seqFloor("a"))
        val kept = view(password("pw"), phrase("ph"), seq = 5)
        store.keep("a", kept)
        assertEquals(kept, SealStateStore(storage).view("a"))
        assertEquals(5L, store.seqFloor("a"))
        store.keep("a", kept.copy(seq = 3))
        assertEquals("a lower number never lowers the floor", 5L, store.seqFloor("a"))
        store.raiseFloor("a", 9)
        store.raiseFloor("a", 2)
        assertEquals(9L, store.seqFloor("a"))
        // After a reset the old announcement is not this account's any more;
        // the floor stays so a number is never reused.
        store.dropView("a")
        assertNull(store.view("a"))
        assertEquals(9L, store.seqFloor("a"))
        store.clearAccount("a")
        assertEquals(0L, store.seqFloor("a"))
        assertTrue(storage.rows.isEmpty())
    }

    @Test
    fun `a signed-out account's store takes no write`() {
        val storage = MapStorage()
        val store = SealStateStore(storage)
        AccountStoreGate.seal("a")
        store.keep("a", view(password("pw")))
        store.raiseFloor("a", 4)
        assertTrue(storage.rows.isEmpty())
    }

    @Test
    fun `a seal secret is wiped and never printed by the entry it gives`() {
        val export = ByteArray(64) { 5 }
        val secret = SealSecret.Password(export)
        val entry = FakeCore().entryOf(secret)
        secret.wipe()
        assertTrue(export.all { it == 0.toByte() })
        assertEquals("pw:5", entry.sealPkB64)
    }
}
