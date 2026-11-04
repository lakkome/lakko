// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import java.io.IOException
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0074 section 3.3 - a kept device: the pending envelope store, the
 * verdicts (pending / adopt / drop / superseded without a pending envelope),
 * the upgrade's answers, and this account's own predecessors.
 */
class KeyRotationKeptTest {

    private class MapBlobs : SealedBlobStorage {
        val map = mutableMapOf<String, ByteArray>()
        var failWrites = false
        override fun read(name: String): ByteArray? = map[name]?.copyOf()
        override fun write(name: String, plaintext: ByteArray) {
            if (failWrites) throw IOException("disk")
            map[name] = plaintext.copyOf()
        }
        override fun delete(name: String) {
            map.remove(name)
        }
    }

    private val account = base64Encode(ByteArray(16) { 7 })

    private fun pending(tag: Int, preparedAtMs: Long = tag * 1_000L) = PendingRotation(
        newSignaturePubkey = ByteArray(32) { tag.toByte() },
        newEntropy = ByteArray(16) { (tag + 1).toByte() },
        supersessionProof = ByteArray(145) { (tag + 2).toByte() },
        preparedAtMs = preparedAtMs,
        proofExpiresAtUnixSecs = 9_000L + tag,
    )

    private fun pair(generation: Long) = SessionPair(
        accessToken = "a$generation",
        accessExpiresAtMs = 1,
        refreshToken = "r$generation",
        refreshExpiresAtMs = 2,
        keyGeneration = generation,
    )

    private fun http(code: String?, status: Int = 401, body: String = "{}") =
        AuthHttpError(status, "/auth/session/refresh", body, code = code)

    // --- ADR-0074 section 3.7: the conversation link -------------------------

    private fun predecessor(tag: Int) =
        OwnPredecessor(ByteArray(32) { tag.toByte() }, ByteArray(64) { tag.toByte() }, tag * 1_000L)

    /** Stand-in for `Identity.fromSeed(seed).ed25519Seed()`. */
    private val edOf: (ByteArray) -> ByteArray = { seed -> seed.copyOf(32) }

    @Test
    fun `a rotation's link is signed by the oldest predecessor and the current key`() {
        val current = ByteArray(64) { 9 }
        val seen = mutableListOf<Triple<ByteArray, ByteArray, Long>>()
        val link = conversationLinkOf(
            rows = listOf(predecessor(1), predecessor(2)),
            currentSeed = current,
            nowSecs = 77L,
            ed25519Of = edOf,
            sign = { conversation, now, at ->
                seen += Triple(conversation.copyOf(), now.copyOf(), at)
                ByteArray(201) { 3 }
            },
        )
        assertEquals(201, link?.size)
        val (conversation, now, at) = seen.single()
        assertEquals("not the key just left", 1.toByte(), conversation[0])
        assertEquals(9.toByte(), now[0])
        assertEquals(77L, at)
    }

    @Test
    fun `a first rotation links the key just left and no predecessor links nothing`() {
        var conversation: Byte? = null
        val link = conversationLinkOf(listOf(predecessor(4)), ByteArray(64) { 9 }, 1L, edOf) { c, _, _ ->
            conversation = c[0]
            ByteArray(201)
        }
        assertEquals(201, link?.size)
        assertEquals(4.toByte(), conversation)
        assertNull(conversationLinkOf(emptyList(), ByteArray(64), 1L, edOf) { _, _, _ -> ByteArray(201) })
    }

    @Test
    fun `the seeds a link was signed with are zeroed`() {
        val handed = mutableListOf<ByteArray>()
        conversationLinkOf(listOf(predecessor(1)), ByteArray(64) { 9 }, 1L, edOf) { c, n, _ ->
            handed += c
            handed += n
            ByteArray(201)
        }
        assertEquals(2, handed.size)
        assertTrue(handed.all { seed -> seed.all { it == 0.toByte() } })
    }

    // --- the store ----------------------------------------------------------

    @Test
    fun `a pending envelope survives the store byte for byte`() {
        val store = PendingRotationStore(MapBlobs())
        assertTrue(store.load(account).isEmpty())
        assertTrue(store.save(account, listOf(pending(1), pending(2))))
        val back = store.load(account)
        assertEquals(2, back.size)
        assertArrayEquals(pending(2).newEntropy, back[1].newEntropy)
        assertArrayEquals(pending(2).supersessionProof, back[1].supersessionProof)
        assertEquals(2_000L, back[1].preparedAtMs)
        assertEquals(9_002L, back[1].proofExpiresAtUnixSecs)
        store.clear(account)
        assertTrue(store.load(account).isEmpty())
    }

    @Test
    fun `a damaged or foreign blob is no pending envelope`() {
        assertNull(PendingRotationCodec.decode(ByteArray(0)))
        assertNull(PendingRotationCodec.decode(byteArrayOf(2, 0)))
        assertNull(PendingRotationCodec.decode(byteArrayOf(1, 1, 0, 0)))
        // A count above the cap is refused before anything is read.
        assertNull(PendingRotationCodec.decode(byteArrayOf(1, 99.toByte())))
        val good = PendingRotationCodec.encode(listOf(pending(3)))
        assertNull(PendingRotationCodec.decode(good + byteArrayOf(0)))
    }

    @Test
    fun `a store that cannot write says so, and the redacted value prints no key`() {
        val blobs = MapBlobs().apply { failWrites = true }
        assertFalse(PendingRotationStore(blobs).save(account, listOf(pending(1))))
        assertEquals("PendingRotation(<redacted>)", pending(1).toString())
        assertEquals("OwnPredecessor(<redacted>)", OwnPredecessor(ByteArray(32), ByteArray(64), 0).toString())
    }

    @Test
    fun `an envelope is keyed on the new key and the oldest goes past the cap`() {
        val again = pending(1, preparedAtMs = 5_000L)
        val rows = withPendingRotation(listOf(pending(1), pending(2)), again)
        assertEquals(2, rows.size)
        assertEquals(5_000L, rows.single { it.newSignaturePubkey[0] == 1.toByte() }.preparedAtMs)
        var many = emptyList<PendingRotation>()
        for (i in 1..(MAX_PENDING_ROTATIONS + 3)) many = withPendingRotation(many, pending(i))
        assertEquals(MAX_PENDING_ROTATIONS, many.size)
        assertEquals(4.toByte(), many.first().newSignaturePubkey[0])
    }

    @Test
    fun `an envelope whose key never became the account's is dropped when it expires`() {
        val fourteenDays = 14L * 24 * 60 * 60 * 1000
        val expired: (Long, Long) -> Boolean = { preparedAt, now -> now - preparedAt >= fourteenDays }
        val rows = listOf(pending(1, preparedAtMs = 0), pending(2, preparedAtMs = fourteenDays))
        assertEquals(2, withoutExpiredRotations(rows, fourteenDays - 1, expired).size)
        val kept = withoutExpiredRotations(rows, fourteenDays, expired)
        assertEquals(1, kept.size)
        assertEquals(2.toByte(), kept.single().newSignaturePubkey[0])
    }

    // --- the verdicts -------------------------------------------------------

    @Test
    fun `a pending envelope alone changes nothing`() {
        // Stored, and nothing says the session is over.
        assertEquals(
            KeptDeviceVerdict.Nothing,
            keptDeviceVerdict(listOf(pending(1)), publishedKey = null, superseded = false, journalRead = false),
        )
        // The directory still serves another key (the old one).
        assertEquals(
            KeptDeviceVerdict.Nothing,
            keptDeviceVerdict(listOf(pending(1)), publishedKey = ByteArray(32) { 50 }, superseded = false, journalRead = true),
        )
    }

    @Test
    fun `the device adopts when the published bundle carries the envelope's key`() {
        val held = listOf(pending(1), pending(2))
        val verdict = keptDeviceVerdict(held, publishedKey = ByteArray(32) { 2 }, superseded = false, journalRead = true)
        assertTrue(verdict is KeptDeviceVerdict.Adopt)
        assertSame(held[1], (verdict as KeptDeviceVerdict.Adopt).pending)
    }

    @Test
    fun `a superseded device with pending envelopes asks the server, newest first`() {
        val verdict = keptDeviceVerdict(
            listOf(pending(1), pending(3), pending(2)),
            publishedKey = null,
            superseded = true,
            journalRead = false,
        )
        val candidates = (verdict as KeptDeviceVerdict.TryUpgrade).candidates
        assertEquals(listOf<Byte>(3, 2, 1), candidates.map { it.newSignaturePubkey[0] })
    }

    @Test
    fun `a superseded device without a pending envelope reads its journal once before it calls it a reset`() {
        assertEquals(
            KeptDeviceVerdict.ReadJournalOnce,
            keptDeviceVerdict(emptyList(), publishedKey = null, superseded = true, journalRead = false),
        )
        // The journal held none: a reset (the staged wipe, "Was it you?").
        assertEquals(
            KeptDeviceVerdict.TreatAsReset,
            keptDeviceVerdict(emptyList(), publishedKey = null, superseded = true, journalRead = true),
        )
        // The journal held one: it is tried.
        assertTrue(
            keptDeviceVerdict(listOf(pending(1)), publishedKey = null, superseded = true, journalRead = true)
                is KeptDeviceVerdict.TryUpgrade,
        )
    }

    @Test
    fun `a published key that matches no envelope is not yet a reset`() {
        // The directory upload trails the commit: the server's answer to the
        // upgrade decides, not the directory.
        val verdict = keptDeviceVerdict(
            listOf(pending(1)),
            publishedKey = ByteArray(32) { 77 },
            superseded = true,
            journalRead = true,
        )
        assertTrue(verdict is KeptDeviceVerdict.TryUpgrade)
    }

    // --- the upgrade's answers ---------------------------------------------

    @Test
    fun `each refresh refusal means one thing`() {
        assertEquals(UpgradeOutcome.NotAccepted, upgradeOutcomeOf(http("key-generation-superseded")))
        assertEquals(UpgradeOutcome.DeviceRevoked, upgradeOutcomeOf(http("device-revoked")))
        assertEquals(UpgradeOutcome.RefreshInvalid, upgradeOutcomeOf(http("refresh-invalid")))
        // Nothing else decides anything: no wipe on a failed request.
        assertTrue(upgradeOutcomeOf(http(null)) is UpgradeOutcome.Unanswered)
        assertTrue(upgradeOutcomeOf(http("too-many-attempts", status = 429)) is UpgradeOutcome.Unanswered)
        assertTrue(upgradeOutcomeOf(http(null, status = 503)) is UpgradeOutcome.Unanswered)
        assertTrue(upgradeOutcomeOf(AuthNetworkError("/x", IOException("down"))) is UpgradeOutcome.Unanswered)
        assertTrue(
            upgradeOutcomeOf(http("statement-stale", body = """{"error":"statement-stale","server_time_ms":5}"""))
                is UpgradeOutcome.Unanswered,
        )
    }

    @Test
    fun `an upgrade proves the key only when the generation moved`() {
        assertTrue(upgradeProvesKey(previousGeneration = 3, upgraded = pair(4)))
        // A credential that was not of a lower generation is refreshed
        // without the statement being read.
        assertFalse(upgradeProvesKey(previousGeneration = 3, upgraded = pair(3)))
    }

    @Test
    fun `the first accepted key is adopted`() {
        val a = pending(1)
        val b = pending(2)
        val resolution = keptResolutionOf(
            listOf(a to UpgradeOutcome.NotAccepted, b to UpgradeOutcome.Upgraded(pair(5))),
        )
        assertSame(b, (resolution as KeptResolution.Adopted).pending)
        assertEquals(5L, resolution.pair.keyGeneration)
    }

    @Test
    fun `a device that was the one removed is wiped in full, never told it was kept`() {
        assertEquals(
            KeptResolution.Removed,
            keptResolutionOf(listOf(pending(1) to UpgradeOutcome.DeviceRevoked)),
        )
        assertEquals(
            KeptResolution.SignInAgain,
            keptResolutionOf(listOf(pending(1) to UpgradeOutcome.RefreshInvalid)),
        )
    }

    @Test
    fun `no accepted key is a reset only when every trial was answered`() {
        assertEquals(
            KeptResolution.Reset,
            keptResolutionOf(listOf(pending(1) to UpgradeOutcome.NotAccepted, pending(2) to UpgradeOutcome.NotAccepted)),
        )
        // One trial had no answer: the device must not conclude "reset".
        assertEquals(
            KeptResolution.Later,
            keptResolutionOf(
                listOf(
                    pending(1) to UpgradeOutcome.NotAccepted,
                    pending(2) to UpgradeOutcome.Unanswered(IOException("down")),
                ),
            ),
        )
        assertEquals(KeptResolution.Reset, keptResolutionOf(emptyList()))
    }

    // --- this account's own predecessors -----------------------------------

    @Test
    fun `the old identity is kept as the predecessor, earliest first`() {
        val blobs = MapBlobs()
        val store = OwnPredecessorStore(blobs)
        assertNull(store.conversationSeed(account))
        val first = OwnPredecessor(ByteArray(32) { 1 }, ByteArray(64) { 11 }, supersededAtMs = 100)
        val second = OwnPredecessor(ByteArray(32) { 2 }, ByteArray(64) { 22 }, supersededAtMs = 200)
        assertTrue(store.add(account, first))
        assertTrue(store.add(account, second))
        // Adding the same key again changes nothing.
        assertTrue(store.add(account, OwnPredecessor(ByteArray(32) { 1 }, ByteArray(64) { 99 }, 300)))
        val rows = store.load(account)
        assertEquals(2, rows.size)
        // The conversation leaves that exist are bound to the EARLIEST one.
        assertArrayEquals(ByteArray(64) { 11 }, store.conversationSeed(account))
        assertArrayEquals(ByteArray(64) { 22 }, rows.last().phraseSeed)
    }

    @Test
    fun `a predecessor that cannot be stored is reported, so the seed is not overwritten`() {
        val blobs = MapBlobs().apply { failWrites = true }
        val store = OwnPredecessorStore(blobs)
        assertFalse(store.add(account, OwnPredecessor(ByteArray(32) { 1 }, ByteArray(64) { 11 }, 1)))
        assertNull(store.conversationSeed(account))
    }

    @Test
    fun `the old mailbox's cursor only moves forward`() {
        val store = OwnPredecessorStore(MapBlobs())
        val key = ByteArray(32) { 1 }
        store.add(account, OwnPredecessor(key, ByteArray(64) { 11 }, 1, mailboxCursor = 10))
        store.advanceMailboxCursor(account, key, 7)
        assertEquals(10L, store.load(account).single().mailboxCursor)
        store.advanceMailboxCursor(account, key, 42)
        assertEquals(42L, store.load(account).single().mailboxCursor)
        // Another key's cursor is not this one's.
        store.advanceMailboxCursor(account, ByteArray(32) { 9 }, 99)
        assertEquals(42L, store.load(account).single().mailboxCursor)
    }

    @Test
    fun `past the cap the earliest predecessor is the one that stays`() {
        var rows = emptyList<OwnPredecessor>()
        for (i in 1..(MAX_OWN_PREDECESSORS + 2)) {
            rows = withOwnPredecessor(rows, OwnPredecessor(ByteArray(32) { i.toByte() }, ByteArray(64), i.toLong()))
        }
        assertEquals(MAX_OWN_PREDECESSORS, rows.size)
        assertEquals(1.toByte(), rows.first().signaturePubkey[0])
        assertEquals((MAX_OWN_PREDECESSORS + 2).toByte(), rows.last().signaturePubkey[0])
    }

    @Test
    fun `the conversation seed is the predecessor's after a rotation and the current one before`() {
        val blobs = MapBlobs()
        val store = OwnPredecessorStore(blobs)
        val current = ByteArray(64) { 5 }
        assertNull(store.conversationSeed(account))
        store.add(account, OwnPredecessor(ByteArray(32) { 1 }, ByteArray(64) { 11 }, 1))
        assertArrayEquals(ByteArray(64) { 11 }, store.conversationSeed(account))
        // The caller gets a copy: zeroing it does not touch the store.
        store.conversationSeed(account)!!.fill(0)
        assertArrayEquals(ByteArray(64) { 11 }, store.conversationSeed(account))
        assertArrayEquals(ByteArray(64) { 5 }, current)
    }

    // --- the rotating device's prepared material ----------------------------

    @Test
    fun `the prepared material survives the store and is read back before it is trusted`() {
        val blobs = MapBlobs()
        val store = PreparedRotationStore(blobs)
        assertNull(store.load(account))
        val prepared = PreparedRotation(ByteArray(16) { 3 }, ByteArray(145) { 4 }, preparedAtMs = 77)
        assertTrue(store.save(account, prepared))
        val back = store.load(account)!!
        assertArrayEquals(ByteArray(16) { 3 }, back.newEntropy)
        assertArrayEquals(ByteArray(145) { 4 }, back.supersessionProof)
        assertEquals(77L, back.preparedAtMs)
        assertEquals("PreparedRotation(<redacted>)", back.toString())
        blobs.failWrites = true
        assertFalse(store.save(account, prepared))
        store.clear(account)
        assertNull(store.load(account))
        assertNull(PreparedRotationCodec.decode(ByteArray(3)))
    }

    @Test
    fun `a prepared proof is replaced before it gets close to its fourteen days`() {
        val day = 24L * 60 * 60 * 1000
        assertTrue(preparedRotationUsable(preparedAtMs = 0, nowMs = 0))
        assertTrue(preparedRotationUsable(0, 13 * day))
        assertFalse(preparedRotationUsable(0, 13 * day + 1_000))
        assertFalse(preparedRotationUsable(0, 14 * day))
        // A clock that went backwards.
        assertFalse(preparedRotationUsable(10 * day, 9 * day))
        assertEquals(14L * 24 * 60 * 60, ROTATION_PROOF_VALIDITY_SECS)
    }

    @Test
    fun `the three rotation files of an account are named by its id in hex`() {
        val hex = KeyRotationFiles.hexOf(account)
        assertEquals("07".repeat(16), hex)
        assertNull(KeyRotationFiles.hexOf("not base64 !!"))
        val blobs = MapBlobs()
        PendingRotationStore(blobs).save(account, listOf(pending(1)))
        OwnPredecessorStore(blobs).add(account, OwnPredecessor(ByteArray(32), ByteArray(64), 1))
        PreparedRotationStore(blobs).save(account, PreparedRotation(ByteArray(16), ByteArray(145), 1))
        assertEquals(
            setOf("$hex.pending", "$hex.pred", "$hex.prepared"),
            blobs.map.keys,
        )
    }

    // --- the landing check on a device that adopted -------------------------

    @Test
    fun `the directory still serving this account's previous key is not a mismatch`() {
        val own = ByteArray(32) { 2 }
        val previous = ByteArray(32) { 1 }
        val published = PublishedIdentityKeys(previous, ByteArray(32), keyGeneration = 4)
        // This device adopted (generation 5); the rotating device's upload
        // has not landed yet. Nothing is uploaded, nothing is reported.
        assertEquals(
            LandingVerdict.OwnPredecessorPublished,
            landingVerdict(published, own, ownKeyGeneration = 5, ownPredecessors = listOf(previous)),
        )
        // Without the predecessor it is the anomaly it always was.
        assertEquals(LandingVerdict.KeyMismatch, landingVerdict(published, own, 5))
        // A previous key at the SAME generation is still an anomaly.
        assertEquals(
            LandingVerdict.KeyMismatch,
            landingVerdict(published, own, ownKeyGeneration = 4, ownPredecessors = listOf(previous)),
        )
        // And a stranger's key is never excused by a predecessor.
        val stranger = PublishedIdentityKeys(ByteArray(32) { 9 }, ByteArray(32), keyGeneration = 4)
        assertEquals(
            LandingVerdict.KeyMismatch,
            landingVerdict(stranger, own, ownKeyGeneration = 5, ownPredecessors = listOf(previous)),
        )
        // A higher generation is a reset elsewhere, predecessor or not.
        val later = PublishedIdentityKeys(previous, ByteArray(32), keyGeneration = 6)
        assertEquals(
            LandingVerdict.StaleKeys,
            landingVerdict(later, own, ownKeyGeneration = 5, ownPredecessors = listOf(previous)),
        )
    }
}
