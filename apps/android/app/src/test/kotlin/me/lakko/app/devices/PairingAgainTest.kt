// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import java.io.File
import kotlinx.coroutines.test.runTest
import me.lakko.app.auth.KeyRotationFiles
import me.lakko.app.auth.SealedBlobStorage
import me.lakko.app.auth.base64Encode
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.AddMember
import uniffi.lakko_ffi_uniffi.DecodedPairingQr
import uniffi.lakko_ffi_uniffi.DeviceLeaf

/**
 * G-56 — a device parked on the earlier-keys screen after it already joined
 * the account's DeviceGroup by pairing pairs again with a self-removal proof,
 * and the confirming device replaces its leaf in one commit. Host JVM: the
 * native core is replaced by fakes.
 */
class PairingAgainTest {

    private val groupId = ByteArray(32) { 0x11 }
    private val ownKey = ByteArray(32) { 0x22 }
    private val otherKey = ByteArray(32) { 0x33 }
    private val keyPackage = ByteArray(40) { 0x44 }
    private val proof = ByteArray(137) { 0x55 }
    private val rendezvous = ByteArray(32) { 0x66 }
    private val account = base64Encode(ByteArray(16) { 7 })

    // ---- the joining side ------------------------------------------------

    private class JoiningPorts(
        var recorded: ByteArray?,
        var heldEpoch: ULong?,
        var probe: suspend () -> ULong?,
    ) : RejoinPairingPorts {
        val probedIds = mutableListOf<ByteArray>()
        override fun joinedGroupId(): ByteArray? = recorded
        override fun heldEpochIfMember(groupId: ByteArray): ULong? = heldEpoch
        override suspend fun latestEpoch(groupId: ByteArray): ULong? {
            probedIds += groupId
            return probe()
        }
    }

    @Test
    fun aParkedMemberPairsAgainAtTheEpochTheDsNames() = runTest {
        val ports = JoiningPorts(groupId, heldEpoch = 4uL, probe = { 6uL })
        val plan = planRejoinPairing(ports)
        assertTrue(plan is RejoinPairingPlan.Rejoin)
        plan as RejoinPairingPlan.Rejoin
        assertArrayEquals(groupId, plan.groupId)
        assertEquals(6uL, plan.epoch)
        assertArrayEquals(groupId, ports.probedIds.single())
    }

    @Test
    fun aFailedOrEmptyProbeFallsBackToTheHeldEpoch() = runTest {
        val failed = planRejoinPairing(JoiningPorts(groupId, 4uL, probe = { throw IllegalStateException("down") }))
        assertEquals(4uL, (failed as RejoinPairingPlan.Rejoin).epoch)
        val empty = planRejoinPairing(JoiningPorts(groupId, 4uL, probe = { null }))
        assertEquals(4uL, (empty as RejoinPairingPlan.Rejoin).epoch)
    }

    @Test
    fun withoutARecordedIdOrAHeldLeafTheJoinIsTheOrdinaryOne() = runTest {
        val noId = JoiningPorts(null, 4uL, probe = { 6uL })
        assertSame(RejoinPairingPlan.Plain, planRejoinPairing(noId))
        assertTrue(noId.probedIds.isEmpty())
        // No DeviceGroup state that opens, or its leaf is gone.
        val notMember = JoiningPorts(groupId, null, probe = { 6uL })
        assertSame(RejoinPairingPlan.Plain, planRejoinPairing(notMember))
        assertTrue(notMember.probedIds.isEmpty())
    }

    // ---- the confirming side ---------------------------------------------

    private inner class GroupFake(
        val roster: List<DeviceLeaf>,
        val epoch: ULong,
        val coversAt: ULong?,
    ) : PairingCommitPorts {
        val replaced = mutableListOf<UInt>()
        var added = 0
        val coversAsked = mutableListOf<ULong>()
        override fun keyPackageKey(keyPackage: ByteArray): ByteArray = ownKey
        override fun devices(): List<DeviceLeaf> = roster
        override fun groupId(): ByteArray = this@PairingAgainTest.groupId
        override fun epoch(): ULong = epoch
        override fun covers(
            proof: ByteArray,
            groupId: ByteArray,
            epoch: ULong,
            removedKey: ByteArray,
            keyPackage: ByteArray,
        ): Boolean {
            coversAsked += epoch
            return coversAt == epoch &&
                proof.contentEquals(this@PairingAgainTest.proof) &&
                removedKey.contentEquals(ownKey) &&
                groupId.contentEquals(this@PairingAgainTest.groupId)
        }
        override fun replaceDeviceAt(leafIndex: UInt, keyPackage: ByteArray): AddMember {
            replaced += leafIndex
            return AddMember(commit = byteArrayOf(1, 2, 3), welcome = byteArrayOf(9))
        }
        override fun addDeviceFromWire(keyPackage: ByteArray): AddMember {
            added += 1
            return AddMember(commit = byteArrayOf(4, 5), welcome = byteArrayOf(8))
        }
    }

    private fun roster() = listOf(
        DeviceLeaf(leafIndex = 0u, signaturePublicKey = otherKey),
        DeviceLeaf(leafIndex = 3u, signaturePublicKey = ownKey),
    )

    private fun payload(withProof: Boolean) = DecodedPairingQr(
        keyPackage = keyPackage,
        rendezvousPseudonym = rendezvous,
        selfRemovalProof = if (withProof) proof else null,
    )

    @Test
    fun aParkedMembersLeafIsReplacedInOneCommitCarryingTheProof() {
        val group = GroupFake(roster(), epoch = 6uL, coversAt = 6uL)
        val built = buildPairingCommit(payload(withProof = true), group)
        assertEquals(DeviceGroupCommitEntry.Kind.REJOIN_REPLACE, built.kind)
        assertEquals(listOf(3u), group.replaced)
        assertEquals(0, group.added)
        assertArrayEquals(DeviceGroupJournalEnvelope.encode(byteArrayOf(1, 2, 3), proof), built.row)
        assertArrayEquals(byteArrayOf(9), built.welcome)
        assertArrayEquals(rendezvous, built.welcomeRendezvous)
        assertEquals(listOf(0u, 3u), built.priorLeaves)
        assertEquals(listOf(6uL), group.coversAsked)
    }

    @Test
    fun aProofSignedAtAStaleEpochIsRefusedAndNothingIsBuilt() {
        val group = GroupFake(roster(), epoch = 7uL, coversAt = 6uL)
        assertThrows(SelfRemovalProofNotCovering::class.java) {
            buildPairingCommit(payload(withProof = true), group)
        }
        assertTrue(group.replaced.isEmpty())
        assertEquals(0, group.added)
    }

    @Test
    fun aPayloadWithoutAProofIsTheOrdinaryAdd() {
        val group = GroupFake(listOf(DeviceLeaf(0u, otherKey)), epoch = 6uL, coversAt = 6uL)
        val built = buildPairingCommit(payload(withProof = false), group)
        assertEquals(DeviceGroupCommitEntry.Kind.ADD, built.kind)
        assertEquals(1, group.added)
        assertTrue(group.replaced.isEmpty())
        assertTrue(group.coversAsked.isEmpty())
        assertArrayEquals(DeviceGroupJournalEnvelope.encode(byteArrayOf(4, 5), null), built.row)
        assertEquals(listOf(0u), built.priorLeaves)
    }

    @Test
    fun aProofWhoseKeyHasNoLeafIsTheOrdinaryAdd() {
        val group = GroupFake(listOf(DeviceLeaf(0u, otherKey)), epoch = 6uL, coversAt = null)
        val built = buildPairingCommit(payload(withProof = true), group)
        assertEquals(DeviceGroupCommitEntry.Kind.ADD, built.kind)
        assertEquals(1, group.added)
    }

    // ---- the joined DeviceGroup id ---------------------------------------

    private class MapBlobs : SealedBlobStorage {
        val map = mutableMapOf<String, ByteArray>()
        override fun read(name: String): ByteArray? = map[name]?.copyOf()
        override fun write(name: String, plaintext: ByteArray) {
            map[name] = plaintext.copyOf()
        }
        override fun delete(name: String) {
            map.remove(name)
        }
    }

    @Test
    fun theJoinedIdIsRecordedReadBackAndCleared() {
        val blobs = MapBlobs()
        val store = JoinedDeviceGroupIdStore(blobs)
        assertNull(store.load(account))
        assertTrue(recordJoinedDeviceGroup(store, account) { groupId })
        assertArrayEquals(groupId, store.load(account))
        assertTrue(blobs.map.keys.single().endsWith(KeyRotationFiles.JOINED_GROUP_SUFFIX))
        store.clear(account)
        assertNull(store.load(account))
        // A wrong-length id is not recorded; a failing read records nothing.
        assertFalse(store.save(account, ByteArray(16)))
        assertFalse(recordJoinedDeviceGroup(store, account) { throw IllegalStateException("closed") })
        assertNull(store.load(account))
    }

    @Test
    fun forgetDeviceDeletesTheJoinedIdFile() {
        // KeyRotationFiles.deleteAll is the Forget-device step; its suffix
        // list must name the joined-group file.
        val source = source("auth/KeyRotationKept.kt")
        val deleteAll = source.substringAfter("fun deleteAll(").substringBefore("\n    }\n")
        assertTrue(deleteAll.contains("JOINED_GROUP_SUFFIX"))
    }

    @Test
    fun bothJoinsRecordTheIdAndAResetClearsIt() {
        val orchestrator = source("devices/PairingOrchestrator.kt")
        // The pairing join for a known account and the join by name.
        assertEquals(2, Regex("recordJoinedDeviceGroup\\(").findAll(orchestrator).count())
        val accept = orchestrator.substringAfter("public suspend fun accept(").substringBefore("private suspend fun acceptByName(")
        assertTrue(accept.contains("recordJoinedDeviceGroup("))
        val byName = orchestrator.substringAfter("override suspend fun persist()").substringBefore("applyFetchedBundles(joined")
        assertTrue(byName.contains("recordJoinedDeviceGroup("))
        // A key reset or recall on this device (landKeyReset).
        val auth = source("auth/AuthRepository.kt")
        val reset = auth.substringAfter("private suspend fun landKeyReset(").substringBefore("markSignedInNow(accountIdB64)")
        assertTrue(reset.contains("JoinedDeviceGroupIdStore.onDevice(applicationContext).clear(accountIdB64)"))
    }

    private fun source(relative: String): String {
        val path = "src/main/kotlin/me/lakko/app/$relative"
        val file = sequenceOf(File(path), File("app/$path"), File("apps/android/app/$path"))
            .firstOrNull { it.isFile }
            ?: error("$relative not found from ${File(".").absolutePath}")
        return file.readText()
    }
}
