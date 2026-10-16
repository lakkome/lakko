// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ds

import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0057 — the client half of journal ownership: who signs a registration,
 * who is evicted and revoked, which registrars the owner appoints and how a
 * seed reaches them, the receive gate, claim vs reset, and the push binding.
 */
class JournalOwnershipTest {

    private val journal = ByteArray(32) { 3 }
    private fun key(b: Int) = ByteArray(32) { b.toByte() }
    private fun hex(b: ByteArray) = hexOf(b)

    // ------------------------------------------------------------ who signs

    @Test
    fun `the owner registers with its root and never reads a registrar`() {
        val root = RecordingStatementSigner("root")
        val chosen = commitStatementSigner(isOwner = true, root = { root }, registrar = { error("owner reads no registrar") })
        assertSame(root, chosen)
    }

    @Test
    fun `an appointed member registers with its registrar and never derives the root`() {
        val registrar = RecordingStatementSigner("registrar")
        val chosen = commitStatementSigner(isOwner = false, root = { error("a member derives no root") }, registrar = { registrar })
        assertSame(registrar, chosen)
    }

    @Test
    fun `a device with neither key cannot commit`() {
        assertNull(commitStatementSigner(isOwner = false, root = { null }, registrar = { null }))
    }

    @Test
    fun `register-before-publish signs N+1 with the chosen signer and carries the revoke list`() = runTest {
        val registrar = RecordingStatementSigner("registrar")
        val revoke = listOf(key(0x44))
        var putPath: String? = null
        val current = registerEpochBeforePublish(
            journalId = journal,
            before = FakeJournalSigner(4uL),
            after = FakeJournalSigner(5uL),
            statements = registrar,
            revoke = revoke,
        ) { make -> putPath = make().path }
        assertEquals(4uL, current.epoch)
        assertEquals(listOf("register@5"), registrar.calls)
        assertArrayEquals(revoke.single(), registrar.lastRevoke.single())
        assertTrue(putPath!!.endsWith("/access"))
    }

    @Test
    fun `a device with no statement signer never registers or publishes`() {
        var puts = 0
        assertThrows(DsRegistrationUnavailable::class.java) {
            runBlocking {
                registerEpochBeforePublish(journal, FakeJournalSigner(1uL), FakeJournalSigner(2uL), null, emptyList()) {
                    puts += 1
                }
            }
        }
        assertEquals(0, puts)
    }

    // ------------------------------------------------------------ evictees

    @Test
    fun `removing one device of an account that keeps another evicts nobody`() {
        val roster = listOf(RosterLeaf(0u, key(1)), RosterLeaf(1u, key(2)), RosterLeaf(2u, key(2)))
        assertTrue(evictedAccounts(roster, listOf(1u)).isEmpty())
    }

    @Test
    fun `removing every leaf of an account evicts it once`() {
        val roster = listOf(RosterLeaf(0u, key(1)), RosterLeaf(1u, key(2)), RosterLeaf(2u, key(2)))
        val evicted = evictedAccounts(roster, listOf(1u, 2u))
        assertEquals(1, evicted.size)
        assertArrayEquals(key(2), evicted.single())
    }

    @Test
    fun `the accounts gone between two rosters are the evictees of a merged commit`() {
        val gone = evictedBetween(before = listOf(key(1), key(2), key(2), key(3)), after = listOf(key(1), key(3)))
        assertEquals(listOf(hex(key(2))), gone.map(::hex))
    }

    @Test
    fun `only the owner revokes, never itself, and a failed derivation is skipped`() {
        val own = key(1)
        val evictees = listOf(key(1), key(2), key(3))
        val keys = revokeListFor(evictees, own) { account ->
            if (account.contentEquals(key(3))) error("no seed")
            ByteArray(32) { (account[0] + 100).toByte() }
        }
        assertEquals(1, keys.size)
        assertEquals(102.toByte(), keys.single()[0])
        // An admin committer cannot derive a registrar key: it revokes nothing.
        assertTrue(revokeListFor(evictees, own, deriveRegistrarKey = null).isEmpty())
    }

    // ------------------------------------------------------------ appointments

    @Test
    fun `the owner wants every other member, admins ranked admin`() {
        val owner = key(1)
        val desired = desiredRegistrars(
            memberAccounts = listOf(key(1), key(2), key(3), key(3)),
            ownerAccountKey = owner,
            admins = listOf(key(2)),
        )
        assertEquals(mapOf(hex(key(2)) to JournalRank.ADMIN, hex(key(3)) to JournalRank.MEMBER), desired)
    }

    private class Sent(val account: ByteArray, val x25519: ByteArray, val rank: JournalRank, val seed: ByteArray)

    private suspend fun reconcile(
        delivered: Map<String, JournalRank>,
        desired: Map<String, JournalRank>,
        root: RecordingStatementSigner,
        sent: MutableList<Sent>,
        lastSentSet: String? = null,
        x25519: (ByteArray) -> ByteArray? = { ByteArray(32) { 7 } },
        putFails: Boolean = false,
        sendFails: Boolean = false,
        devicesOf: ((String) -> Collection<String>)? = null,
    ): AppointmentResult = reconcileAppointments(
        journalId = journal,
        desired = desired,
        delivered = delivered,
        lastSentSet = lastSentSet,
        root = root,
        registrarSeedFor = { account -> ByteArray(32) { (account[0] + 1).toByte() } },
        registrarPublicKeyOf = { seed -> ByteArray(32) { (seed[0] + 1).toByte() } },
        memberX25519 = { x25519(it) },
        put = { make ->
            make()
            if (putFails) throw DsHttpError(403, "/registrars-set", "")
        },
        sendEnvelope = { account, x, rank, seed ->
            if (sendFails) error("offline")
            sent += Sent(account.copyOf(), x, rank, seed.copyOf())
        },
        devicesOf = devicesOf,
    )

    @Test
    fun `the owner sends the whole desired set, then each member its seed sealed to its key`() = runTest {
        val root = RecordingStatementSigner("root")
        val sent = mutableListOf<Sent>()
        val member = key(5)
        val admin = key(4)
        val desired = mapOf(hex(member) to JournalRank.MEMBER, hex(admin) to JournalRank.ADMIN)
        val result = reconcile(emptyMap(), desired, root, sent)
        assertEquals(listOf("registrars-set"), root.calls)
        // Sorted by account key: 04.. before 05..; the entry key is the
        // REGISTRAR public key (pk(seed(a)) = a + 2), never the account key.
        val entries = root.lastRegistrarSet
        assertEquals(listOf(JournalRank.ADMIN, JournalRank.MEMBER), entries.map { it.rank })
        assertEquals(listOf(6.toByte(), 7.toByte()), entries.map { it.publicKey[0] })
        assertEquals(2, sent.size)
        assertEquals(desired, result.delivered)
        assertEquals(registrarSetFingerprint(desired), result.sentSet)
        assertTrue(result.complete)
    }

    @Test
    fun `the set comes from the roster alone, whatever the local record says`() = runTest {
        // A record that missed an eviction (another owner device, a lost
        // write) still names the evictee; the statement does not.
        val root = RecordingStatementSigner("root")
        val sent = mutableListOf<Sent>()
        val stays = key(5)
        val evicted = key(8)
        val result = reconcile(
            delivered = mapOf(hex(stays) to JournalRank.MEMBER, hex(evicted) to JournalRank.ADMIN),
            desired = mapOf(hex(stays) to JournalRank.MEMBER),
            root = root,
            sent = sent,
        )
        assertEquals(listOf("registrars-set"), root.calls)
        assertEquals(1, root.lastRegistrarSet.size)
        assertEquals(7.toByte(), root.lastRegistrarSet.single().publicKey[0])
        assertTrue(sent.isEmpty())
        assertEquals(mapOf(hex(stays) to JournalRank.MEMBER), result.delivered)
    }

    @Test
    fun `an empty roster removes every registrar`() = runTest {
        val root = RecordingStatementSigner("root")
        val result = reconcile(mapOf(hex(key(8)) to JournalRank.ADMIN), emptyMap(), root, mutableListOf())
        assertEquals(listOf("registrars-set"), root.calls)
        assertTrue(root.lastRegistrarSet.isEmpty())
        assertTrue(result.delivered.isEmpty())
    }

    @Test
    fun `a promotion re-sends the set and a fresh envelope at the new rank`() = runTest {
        val root = RecordingStatementSigner("root")
        val sent = mutableListOf<Sent>()
        val admin = key(5)
        val before = mapOf(hex(admin) to JournalRank.MEMBER)
        val result = reconcile(
            delivered = before,
            desired = mapOf(hex(admin) to JournalRank.ADMIN),
            root = root,
            sent = sent,
            lastSentSet = registrarSetFingerprint(before),
        )
        assertEquals(JournalRank.ADMIN, root.lastRegistrarSet.single().rank)
        assertEquals(JournalRank.ADMIN, sent.single().rank)
        assertEquals(JournalRank.ADMIN, result.delivered[hex(admin)])
    }

    @Test
    fun `a member whose bundle does not resolve is registered but its envelope stays owed`() = runTest {
        val root = RecordingStatementSigner("root")
        val sent = mutableListOf<Sent>()
        val result = reconcile(emptyMap(), mapOf(hex(key(5)) to JournalRank.MEMBER), root, sent, x25519 = { null })
        assertEquals(listOf("registrars-set"), root.calls)
        assertTrue(sent.isEmpty())
        assertTrue(result.delivered.isEmpty())
        assertFalse(result.complete)
    }

    @Test
    fun `a refused set sends no seed and records nothing`() = runTest {
        val root = RecordingStatementSigner("root")
        val sent = mutableListOf<Sent>()
        val before = mapOf("ee" to JournalRank.MEMBER)
        val result = reconcile(before, mapOf(hex(key(5)) to JournalRank.MEMBER), root, sent, putFails = true)
        assertTrue(sent.isEmpty())
        assertEquals(before, result.delivered)
        assertNull(result.sentSet)
        assertFalse(result.complete)
    }

    @Test
    fun `a seed that did not go out is not recorded as delivered`() = runTest {
        val root = RecordingStatementSigner("root")
        val sent = mutableListOf<Sent>()
        val result = reconcile(emptyMap(), mapOf(hex(key(5)) to JournalRank.MEMBER), root, sent, sendFails = true)
        assertTrue(result.delivered.isEmpty())
        assertFalse(result.complete)
    }

    @Test
    fun `the set already sent this session with every seed delivered sends nothing`() = runTest {
        val root = RecordingStatementSigner("root")
        val sent = mutableListOf<Sent>()
        val same = mapOf(hex(key(5)) to JournalRank.MEMBER)
        val result = reconcile(same, same, root, sent, lastSentSet = registrarSetFingerprint(same))
        assertTrue(root.calls.isEmpty())
        assertTrue(sent.isEmpty())
        assertTrue(result.complete)
    }

    @Test
    fun `a new session sends the set once even with every seed delivered`() = runTest {
        val root = RecordingStatementSigner("root")
        val sent = mutableListOf<Sent>()
        val same = mapOf(hex(key(5)) to JournalRank.MEMBER)
        reconcile(same, same, root, sent, lastSentSet = null)
        assertEquals(listOf("registrars-set"), root.calls)
        assertTrue(sent.isEmpty())
    }

    // ------------------- "Who revokes" item 2: delivery per device (2026-10-01)

    @Test
    fun `the owner re-sends the seed when a new device of the account appears`() = runTest {
        val root = RecordingStatementSigner("root")
        val member = key(5)
        val desired = mapOf(hex(member) to JournalRank.MEMBER)
        var devices = listOf("d1")
        val first = mutableListOf<Sent>()
        val delivered = reconcile(emptyMap(), desired, root, first, devicesOf = { devices }).delivered
        assertEquals(1, first.size)
        assertEquals(mapOf(deliveryKey(hex(member), "d1") to JournalRank.MEMBER), delivered)

        // Nothing new: nothing is sent.
        val again = mutableListOf<Sent>()
        val unchanged = reconcile(
            delivered,
            desired,
            root,
            again,
            lastSentSet = registrarSetFingerprint(desired),
            devicesOf = { devices },
        )
        assertTrue(again.isEmpty())
        assertEquals(delivered, unchanged.delivered)

        // A second device of the same account joined after the envelope was
        // posted: the account is owed one again, sealed to its account key.
        devices = listOf("d1", "d2")
        val second = mutableListOf<Sent>()
        val result = reconcile(
            delivered,
            desired,
            root,
            second,
            lastSentSet = registrarSetFingerprint(desired),
            devicesOf = { devices },
        )
        assertEquals(1, second.size)
        assertArrayEquals(member, second.single().account)
        assertEquals(
            mapOf(
                deliveryKey(hex(member), "d1") to JournalRank.MEMBER,
                deliveryKey(hex(member), "d2") to JournalRank.MEMBER,
            ),
            result.delivered,
        )
        assertEquals(expectedDeliveries(desired) { devices }, result.delivered)
    }

    @Test
    fun `a departed device drops out of the delivery record`() = runTest {
        val root = RecordingStatementSigner("root")
        val member = key(5)
        val desired = mapOf(hex(member) to JournalRank.MEMBER)
        val delivered = mapOf(
            deliveryKey(hex(member), "d1") to JournalRank.MEMBER,
            deliveryKey(hex(member), "gone") to JournalRank.MEMBER,
        )
        val sent = mutableListOf<Sent>()
        val result = reconcile(
            delivered,
            desired,
            root,
            sent,
            lastSentSet = registrarSetFingerprint(desired),
            devicesOf = { listOf("d1") },
        )
        assertTrue(sent.isEmpty())
        assertEquals(mapOf(deliveryKey(hex(member), "d1") to JournalRank.MEMBER), result.delivered)
        assertEquals(hex(member), accountHexOfDeliveryKey(deliveryKey(hex(member), "d1")))
    }

    /** "Who revokes" item 2, receive half: every device of the addressee
     *  account opens and stores the seed from the same envelope, whichever of
     *  them processed it first (each keeps its own store). */
    @Test
    fun `a second device of the account stores the seed another device already processed`() {
        val owner = key(1)
        val account = key(2)
        val envelopeHeader = SeedEnvelopeHeader(journal, account, SeedEnvelopeRank.MEMBER, registrarPublicKey = key(0x70))
        val firstDevice = InMemoryRegistrarSeedStore()
        val secondDevice = InMemoryRegistrarSeedStore()
        for (store in listOf(firstDevice, secondDevice)) {
            val outcome = receiveSeedEnvelope(
                header = envelopeHeader,
                groupId = journal,
                ownAccountKey = account,
                senderAccountKey = owner,
                ownerAccountKey = owner,
                open = { key(0x33) },
                storeRegistrarSeed = { seed -> store.put("acct", "aa", seed) },
                takeOver = { error("not a handover") },
            )
            assertEquals(SeedEnvelopeOutcome.REGISTRAR_STORED, outcome)
        }
        assertArrayEquals(key(0x33), firstDevice.get("acct", "aa"))
        assertArrayEquals(key(0x33), secondDevice.get("acct", "aa"))
    }

    @Test
    fun `per-device delivery records survive their encoding`() {
        val appointed = mapOf(deliveryKey("aa01", "ff") to JournalRank.ADMIN, deliveryKey("bb02", "ee") to JournalRank.MEMBER)
        assertEquals(appointed, decodeAppointments(encodeAppointments(appointed)))
    }

    // ------------------- "Who revokes" item 1: the registrar list (2026-10-01)

    private fun header(addressee: ByteArray, rank: SeedEnvelopeRank, pk: ByteArray = key(0x70), journalId: ByteArray = journal) =
        SeedEnvelopeHeader(journalId = journalId, addresseeAccountKey = addressee, rank = rank, registrarPublicKey = pk)

    @Test
    fun `every member lists an owner-sent appointment whoever it is addressed to`() {
        val owner = key(1)
        // Addressed to someone else entirely: still listed.
        assertTrue(registrarListEntryAccepted(header(key(9), SeedEnvelopeRank.MEMBER), journal, owner, owner))
        assertTrue(registrarListEntryAccepted(header(key(9), SeedEnvelopeRank.ADMIN), journal, owner, owner))
        // Not from the owner, unresolved, another journal, or a root handover.
        assertFalse(registrarListEntryAccepted(header(key(9), SeedEnvelopeRank.MEMBER), journal, key(4), owner))
        assertFalse(registrarListEntryAccepted(header(key(9), SeedEnvelopeRank.MEMBER), journal, null, owner))
        assertFalse(registrarListEntryAccepted(header(key(9), SeedEnvelopeRank.MEMBER), journal, owner, null))
        assertFalse(
            registrarListEntryAccepted(header(key(9), SeedEnvelopeRank.MEMBER, journalId = key(8)), journal, owner, owner),
        )
        assertFalse(registrarListEntryAccepted(header(key(9), SeedEnvelopeRank.ROOT), journal, owner, owner))
    }

    @Test
    fun `an admin removal revokes the removed member's listed key and skips an equal rank`() {
        val store = InMemoryRegistrarListStore()
        val memberGone = key(5)
        val adminGone = key(6)
        assertTrue(recordListedRegistrar(store, "acct", "aa", memberGone, key(0x55), JournalRank.MEMBER))
        assertTrue(recordListedRegistrar(store, "acct", "aa", adminGone, key(0x66), JournalRank.ADMIN))
        // Re-recording the same entry changes nothing.
        assertFalse(recordListedRegistrar(store, "acct", "aa", memberGone, key(0x55), JournalRank.MEMBER))
        val listed = store.get("acct", "aa")

        val asAdmin = revokeListFromRegistrarList(listOf(memberGone, adminGone, key(7)), key(2), JournalRank.ADMIN, listed)
        assertEquals("the member's key only: an admin cannot revoke an admin", listOf(hex(key(0x55))), asAdmin.map(::hex))

        // A member-rank registrar (a device-admission sweep) revokes nothing,
        // and a device with no registrar key at all neither.
        assertTrue(revokeListFromRegistrarList(listOf(memberGone), key(2), JournalRank.MEMBER, listed).isEmpty())
        assertTrue(revokeListFromRegistrarList(listOf(memberGone), key(2), null, listed).isEmpty())
        // Never this account's own key.
        assertTrue(revokeListFromRegistrarList(listOf(memberGone), memberGone, JournalRank.ADMIN, listed).isEmpty())
    }

    @Test
    fun `the registrar list survives its encoding and forget-device`() {
        val listed = mapOf(
            hex(key(5)) to ListedRegistrar(key(0x55), JournalRank.MEMBER),
            hex(key(6)) to ListedRegistrar(key(0x66), JournalRank.ADMIN),
        )
        val decoded = decodeRegistrarList(encodeRegistrarList(listed))
        assertEquals(listed.keys, decoded.keys)
        assertArrayEquals(key(0x55), decoded.getValue(hex(key(5))).publicKey)
        assertEquals(JournalRank.ADMIN, decoded.getValue(hex(key(6))).rank)
        assertTrue(decodeRegistrarList("junk,=,aa=zz:A").isEmpty())

        val store = InMemoryRegistrarListStore()
        store.put("acct", "aa", listed)
        store.clearForAccount("acct")
        assertTrue(store.get("acct", "aa").isEmpty())
    }

    // ------------------------------------------------------------ receive gate

    @Test
    fun `a seed addressed to this account and sent by the owner is accepted`() {
        val own = key(2)
        assertEquals(
            RegistrarSeedVerdict.ACCEPT,
            registrarSeedVerdict(own, addresseeAccountKey = key(2), senderAccountKey = key(1), ownerAccountKey = key(1)),
        )
    }

    @Test
    fun `a seed addressed to another account is not ours`() {
        assertEquals(
            RegistrarSeedVerdict.NOT_ADDRESSED_TO_US,
            registrarSeedVerdict(key(2), addresseeAccountKey = key(3), senderAccountKey = key(1), ownerAccountKey = key(1)),
        )
    }

    @Test
    fun `a seed from anyone but the owner is refused`() {
        assertEquals(
            RegistrarSeedVerdict.SENDER_NOT_OWNER,
            registrarSeedVerdict(key(2), addresseeAccountKey = key(2), senderAccountKey = key(4), ownerAccountKey = key(1)),
        )
    }

    @Test
    fun `an unresolved sender, owner or own key fails closed`() {
        assertEquals(RegistrarSeedVerdict.UNRESOLVED, registrarSeedVerdict(key(2), key(2), null, key(1)))
        assertEquals(RegistrarSeedVerdict.UNRESOLVED, registrarSeedVerdict(key(2), key(2), key(1), null))
        assertEquals(RegistrarSeedVerdict.UNRESOLVED, registrarSeedVerdict(null, key(2), key(1), key(1)))
    }

    @Test
    fun `a newer seed replaces the stored one and forget-device clears it`() {
        val store = InMemoryRegistrarSeedStore()
        store.put("acct", "AA", key(1))
        store.put("acct", "aa", key(2))
        assertArrayEquals(key(2), store.get("acct", "aa"))
        store.put("other", "aa", key(3))
        store.clearForAccount("acct")
        assertNull(store.get("acct", "aa"))
        assertArrayEquals(key(3), store.get("other", "aa"))
    }

    @Test
    fun `appointments survive their encoding`() {
        val appointed = mapOf("aa01" to JournalRank.ADMIN, "bb02" to JournalRank.MEMBER)
        assertEquals(appointed, decodeAppointments(encodeAppointments(appointed)))
        assertTrue(decodeAppointments(null).isEmpty())
        assertEquals(mapOf("cc" to JournalRank.MEMBER), decodeAppointments("cc=M,bad,dd=Z"))
    }

    // ------------------------------------------------------------ claims

    @Test
    fun `the owner's claim lands`() = runTest {
        val root = RecordingStatementSigner("root")
        val outcome = claimOrReset(journal, 3uL, key(9), root, refounded = false) { make -> make() }
        assertEquals(JournalClaimOutcome.CLAIMED, outcome)
        assertEquals(listOf("claim@3"), root.calls)
    }

    @Test
    fun `a 409 on a live chain is left alone`() = runTest {
        val root = RecordingStatementSigner("root")
        val outcome = claimOrReset(journal, 3uL, key(9), root, refounded = false) { make ->
            if (make().path.endsWith("/claim")) throw DsHttpError(409, "/claim", "")
        }
        assertEquals(JournalClaimOutcome.ALREADY_CLAIMED, outcome)
        assertEquals(listOf("claim@3"), root.calls)
    }

    @Test
    fun `a re-founded group resets the chain with its root`() = runTest {
        val root = RecordingStatementSigner("root")
        val paths = mutableListOf<String>()
        val outcome = claimOrReset(journal, 0uL, key(9), root, refounded = true) { make ->
            val statement = make()
            paths += statement.path
            if (statement.path.endsWith("/claim")) throw DsHttpError(409, "/claim", "")
        }
        assertEquals(JournalClaimOutcome.RESET, outcome)
        assertEquals(listOf("claim@0", "reset@0"), root.calls)
        assertTrue(paths.last().endsWith("/reset"))
    }

    @Test
    fun `any other claim refusal propagates`() {
        assertThrows(DsHttpError::class.java) {
            runBlocking {
                claimOrReset(journal, 1uL, key(9), RecordingStatementSigner("root"), refounded = true) {
                    throw DsHttpError(401, "/claim", "proof required")
                }
            }
        }
    }

    // ------------------------------------------------------------ second security pass

    @Test
    fun `a new owner keeps signing with its registrar seed until the root is held`() {
        val root = RecordingStatementSigner("root")
        val registrar = RecordingStatementSigner("registrar")
        assertSame(registrar, commitStatementSigner(isOwner = true, root = { root }, registrar = { registrar }, rootHeld = false))
        assertSame(root, commitStatementSigner(isOwner = true, root = { root }, registrar = { registrar }, rootHeld = true))
    }

    @Test
    fun `an owner with no registrar seed signs with its root before the marker`() {
        // A group this account founded: the previous owner appointed nobody.
        val root = RecordingStatementSigner("root")
        assertSame(root, commitStatementSigner(isOwner = true, root = { root }, registrar = { null }, rootHeld = false))
        assertEquals(JournalAuthorityKind.ROOT, journalAuthorityKind(isOwner = true, rootHeld = false) { null })
        assertEquals(JournalAuthorityKind.REGISTRAR, journalAuthorityKind(isOwner = true, rootHeld = false) { Unit })
        assertEquals(JournalAuthorityKind.NONE, journalAuthorityKind(isOwner = false, rootHeld = true) { null })
    }

    @Test
    fun `a commit registration is an append`() = runTest {
        val root = RecordingStatementSigner("root")
        registerEpochBeforePublish(journal, FakeJournalSigner(4uL), FakeJournalSigner(5uL), root, emptyList()) { make -> make() }
        assertEquals(listOf("register@5"), root.calls)
    }

    @Test
    fun `an append 409 aborts the commit and never sends a replace`() {
        val root = RecordingStatementSigner("root")
        assertThrows(DsHttpError::class.java) {
            runBlocking {
                registerEpochBeforePublish(journal, FakeJournalSigner(4uL), FakeJournalSigner(5uL), root, emptyList()) { make ->
                    make()
                    throw DsHttpError(409, "/access", "")
                }
            }
        }
        assertEquals(listOf("register@5"), root.calls)
    }

    @Test
    fun `a claim at creation that meets another chain fails closed and nothing is published`() {
        val root = RecordingStatementSigner("root")
        var welcomes = 0
        assertThrows(JournalClaimRefused::class.java) {
            runBlocking {
                requireClaim {
                    claimOrReset(journal, 1uL, key(9), root, refounded = false) { make ->
                        make()
                        throw DsHttpError(409, "/claim", "")
                    }
                }
                welcomes += 1
            }
        }
        assertEquals(0, welcomes)
    }

    @Test
    fun `a claim at creation that fails on the network sends no Welcome`() {
        var welcomes = 0
        assertThrows(IllegalStateException::class.java) {
            runBlocking {
                requireClaim { throw IllegalStateException("offline") }
                welcomes += 1
            }
        }
        assertEquals(0, welcomes)
    }

    @Test
    fun `a claim that lands lets the creation publish`() = runTest {
        val root = RecordingStatementSigner("root")
        val outcome = requireClaim { claimOrReset(journal, 1uL, key(9), root, refounded = false) { make -> make() } }
        assertEquals(JournalClaimOutcome.CLAIMED, outcome)
    }

    @Test
    fun `an unconfirmed founding is claimed before anything else and the marker is cleared only on success`() = runTest {
        var cleared = 0
        claimFoundingIfUnconfirmed(unconfirmed = true, claim = { JournalClaimOutcome.RESET }, clear = { cleared += 1 })
        assertEquals(1, cleared)
        claimFoundingIfUnconfirmed(unconfirmed = false, claim = { error("a confirmed founding claims nothing") }, clear = { cleared += 1 })
        assertEquals(1, cleared)
        val failed = runCatching {
            claimFoundingIfUnconfirmed(unconfirmed = true, claim = { throw DsHttpError(503, "/claim", "") }, clear = { cleared += 1 })
        }
        assertTrue(failed.isFailure)
        assertEquals(1, cleared)
    }

    @Test
    fun `the root probe names its own root and reads 204 as held, 401 as not`() = runTest {
        val root = RecordingStatementSigner("root")
        assertTrue(probeRoot(journal, root) { make -> make() })
        assertArrayEquals(root.publicKey, root.lastNewRoot)
        assertFalse(probeRoot(journal, root) { make -> make(); throw DsHttpError(401, "/root", "") })
        assertThrows(DsHttpError::class.java) {
            runBlocking { probeRoot(journal, root) { make -> make(); throw DsHttpError(503, "/root", "") } }
        }
    }

    @Test
    fun `the registrar set fingerprint ignores insertion order`() {
        val a = linkedMapOf("bb" to JournalRank.MEMBER, "aa" to JournalRank.ADMIN)
        val b = linkedMapOf("aa" to JournalRank.ADMIN, "bb" to JournalRank.MEMBER)
        assertEquals(registrarSetFingerprint(a), registrarSetFingerprint(b))
        assertFalse(registrarSetFingerprint(a) == registrarSetFingerprint(mapOf("aa" to JournalRank.MEMBER, "bb" to JournalRank.MEMBER)))
    }
}
