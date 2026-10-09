// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import java.io.IOException
import kotlinx.coroutines.test.runTest
import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.AuthNetworkError
import me.lakko.app.auth.MarkerStorage
import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.discovery.DiscoveryHttpError
import me.lakko.app.l10n.UiText
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * ADR-0066 part 3, Stage A - "remove and change keys" as one owed
 * operation: the order of the steps, the record and its resumes, what each
 * refusal means ("removed but not rotated" included), the one seam that
 * seals the new seed AFTER the removal, and what the sheet shows. Fake
 * ports; no server, no native core.
 */
class KeyRotationFlowTest {

    private class MapStorage : MarkerStorage {
        val map = mutableMapOf<String, String>()
        override fun get(key: String): String? = map[key]
        override fun set(key: String, value: String) {
            map[key] = value
        }
        override fun remove(key: String) {
            map.remove(key)
        }
    }

    /** A model of the account's DeviceGroup: an epoch and a roster. */
    private class FakeDeviceGroup {
        var epoch: ULong = 7uL
        var stolenInRoster = true

        /** Where a message sealed now would be readable by the stolen device. */
        val published = mutableListOf<Pair<String, ULong>>()
        val readableByStolenDevice = mutableListOf<Boolean>()

        fun removeStolenDevice() {
            stolenInRoster = false
            epoch += 1uL
        }
    }

    private class FakePorts(val group: FakeDeviceGroup = FakeDeviceGroup()) : KeyRotationPorts {
        val calls = mutableListOf<String>()
        var grant: String? = "grant"
        var removal: () -> RemovalResult = { RemovalResult.Done }
        var removalChangesGroup = true
        var journalEpoch: () -> ULong? = { group.epoch }
        var commit: () -> CommitAnswer.Committed = { CommitAnswer.Committed("ATTEST", 5) }
        var lost: () -> LostCommit = { LostCommit.Unknown }
        var adopt: () -> Unit = {}
        var identity: (String?, Boolean) -> IdentityPublish = { _, _ -> IdentityPublish.Landed }
        var keyPackage: () -> Boolean = { true }
        var deviceGroupRoot: () -> Boolean = { true }
        var journals: () -> Boolean = { true }
        var channels: () -> Boolean = { true }
        var reach: () -> Boolean = { true }
        var backup: () -> Boolean = { true }
        var preparedCount = 0
        var discarded = 0
        var phraseReleased = 0
        var revokedWithCommit: ByteArray? = null
        val identityCalls = mutableListOf<Pair<String?, Boolean>>()
        val rootCalls = mutableListOf<Pair<String?, Long?>>()
        var paused = 0L

        override fun freshGrant(): String? = grant

        override suspend fun epochIfInRoster(): ULong? = if (group.stolenInRoster) group.epoch else null

        override suspend fun removeDevice(): RemovalResult {
            calls += "remove"
            val result = removal()
            if (removalChangesGroup && result !is RemovalResult.NotRevoked && group.stolenInRoster) {
                group.removeStolenDevice()
            }
            return result
        }

        override suspend fun standing(): RemovalStanding =
            RemovalStanding(inRoster = group.stolenInRoster, localEpoch = group.epoch, journalEpoch = journalEpoch())

        override suspend fun seal(envelope: ByteArray): Pair<ByteArray, ULong> {
            calls += "seal"
            return envelope to group.epoch
        }

        override suspend fun publish(wire: ByteArray) {
            calls += "announce"
            group.published += "key-rotation" to group.epoch
            group.readableByStolenDevice += group.stolenInRoster
        }

        override fun prepare(nowMs: Long) {
            calls += "prepare"
            preparedCount += 1
        }

        override fun envelope(): ByteArray = ByteArray(192) { 1 }

        override suspend fun commit(freshGrant: String, revokeDeviceId: ByteArray): CommitAnswer.Committed {
            calls += "commit"
            revokedWithCommit = revokeDeviceId
            return commit()
        }

        override suspend fun resolveLostCommit(): LostCommit {
            calls += "resolve-lost"
            return lost()
        }

        override suspend fun adoptLocally(attestationB64: String?, keyGeneration: Long?) {
            calls += "adopt"
            adopt()
        }

        override suspend fun publishIdentity(attestationB64: String?, withProof: Boolean): IdentityPublish {
            calls += "identity"
            identityCalls += attestationB64 to withProof
            return identity(attestationB64, withProof)
        }

        override suspend fun publishKeyPackage(): Boolean {
            calls += "keypackage"
            return keyPackage()
        }

        override suspend fun moveDeviceGroupRoot(attestationB64: String?, keyGeneration: Long?): Boolean {
            calls += "dg-root"
            rootCalls += attestationB64 to keyGeneration
            return deviceGroupRoot()
        }

        override suspend fun moveOwnedJournals(): Boolean {
            calls += "journals"
            return journals()
        }

        override suspend fun moveOwnedChannels(): Boolean {
            calls += "channels"
            return channels()
        }

        override suspend fun rekeyReach(): Boolean {
            calls += "reach"
            return reach()
        }

        override suspend fun sealBackup(): Boolean {
            calls += "backup"
            return backup()
        }

        override fun onPhraseReleased() {
            phraseReleased += 1
        }

        override fun discardPrepared() {
            discarded += 1
        }

        override suspend fun pause(ms: Long) {
            paused += ms
        }
    }

    private val account = "acct"
    private val device = ByteArray(32) { 9 }
    private val deviceId = base64UrlEncodeNoPad(device)

    private fun runner(ports: FakePorts, storage: MapStorage, steps: MutableList<RotationUiStep>? = null) =
        KeyRotationRunner(
            accountIdB64 = account,
            store = KeyRotationOwedStore(storage),
            ports = ports,
            clock = { 1_000L },
            onStep = { steps?.add(it) },
        )

    private fun offline() = AuthNetworkError("/account/keys/rotate/finish", IOException("down"))

    private fun http(status: Int, code: String?, retryAfter: Long? = null) =
        AuthHttpError(status, "/account/keys/rotate/finish", "{}", code = code, retryAfterSecs = retryAfter)

    // --- the order ----------------------------------------------------------

    @Test
    fun `the steps follow the order of the ADR`() {
        var owed = KeyRotationOwed(deviceId, 0)
        val seen = mutableListOf<RotationStep>()
        fun step(next: KeyRotationOwed) {
            seen += nextRotationStep(owed)
            owed = next
        }
        step(owed.with(removalWon = true))
        step(owed.with(prepared = true))
        step(owed.with(announced = true))
        step(owed.with(committed = true))
        step(owed.with(adopted = true))
        step(owed.with(identityPublished = true))
        step(owed.with(keyPackagePublished = true))
        step(owed.with(deviceGroupRootMoved = true))
        step(owed.with(journalsMoved = true))
        step(owed.with(channelsMoved = true))
        step(owed.with(reachRekeyed = true))
        step(owed.with(backupSealed = true))
        seen += nextRotationStep(owed)
        assertEquals(RotationStep.entries.toList(), seen)
    }

    @Test
    fun `a rotation that meets no failure runs every step once, in order, and leaves no record`() = runTest {
        val ports = FakePorts()
        val storage = MapStorage()
        val steps = mutableListOf<RotationUiStep>()
        val result = runner(ports, storage, steps).rotate(device)
        assertEquals(RotationResult.Done, result)
        assertEquals(
            listOf(
                "remove", "prepare", "seal", "announce", "commit", "adopt", "identity",
                "keypackage", "dg-root", "journals", "channels", "reach", "backup",
            ),
            ports.calls,
        )
        assertTrue(storage.map.isEmpty())
        assertEquals(1, ports.discarded)
        assertTrue(ports.phraseReleased >= 1)
        // The removed device is also given a floor with the commit.
        assertTrue(device.contentEquals(ports.revokedWithCommit))
        // The directory gets the proof AND the attestation; the root too.
        assertEquals(listOf<Pair<String?, Boolean>>("ATTEST" to true), ports.identityCalls)
        assertEquals(listOf<Pair<String?, Long?>>("ATTEST" to 5L), ports.rootCalls)
        // The step list moves through its three lines in order.
        assertEquals(
            listOf(RotationUiStep.RemovingDevice, RotationUiStep.ChangingKeys, RotationUiStep.UpdatingDevices),
            steps.distinct(),
        )
    }

    @Test
    fun `the record is written before anything is sent`() = runTest {
        val ports = FakePorts()
        val storage = MapStorage()
        var recordAtRemoval: KeyRotationOwed? = null
        ports.removal = {
            recordAtRemoval = KeyRotationOwedStore(storage).load(account)
            RemovalResult.Done
        }
        runner(ports, storage).rotate(device)
        assertEquals(deviceId, recordAtRemoval!!.deviceIdB64Url)
        assertEquals("7", recordAtRemoval!!.epochBeforeRemoval)
        assertFalse(recordAtRemoval!!.removalWon)
    }

    // --- the seam: the envelope after the removal ---------------------------

    @Test
    fun `the envelope is sealed at the post-removal epoch, which the removed device never held`() = runTest {
        val ports = FakePorts()
        val before = ports.group.epoch
        runner(ports, MapStorage()).rotate(device)
        val (_, sealedAt) = ports.group.published.single()
        // Asserted on the EMITTED message, not on call order.
        assertEquals(before + 1uL, sealedAt)
        assertTrue(sealedAt > before)
        assertEquals(listOf(false), ports.group.readableByStolenDevice)
    }

    @Test
    fun `inverted companion - announcing before the removal seals at an epoch the stolen device holds`() = runTest {
        // The swap, run for real against the same model: this is what the
        // assertion above discriminates against.
        val ports = FakePorts()
        val before = ports.group.epoch
        val (wire, sealedAt) = ports.seal(ports.envelope())
        ports.publish(wire)
        ports.removeDevice()
        assertEquals(before, sealedAt)
        assertEquals(listOf(true), ports.group.readableByStolenDevice)
        assertEquals(before, ports.group.published.single().second)
        assertFalse(ports.group.published.single().second > before)
    }

    @Test
    fun `the seam refuses to seal while the removed device is still in the device list`() = runTest {
        val ports = FakePorts()
        try {
            removeThenAnnounce(ports, epochBeforeRemoval = ports.group.epoch, envelope = { ports.envelope() })
            fail("the envelope must not be sealed before the removal")
        } catch (_: RotationOrderViolation) {
            // Nothing was sealed and nothing left the device.
        }
        assertTrue(ports.group.published.isEmpty())
        assertFalse("seal" in ports.calls)
    }

    @Test
    fun `the seam refuses an envelope whose own epoch is not past the removal's`() = runTest {
        // The roster reads as "out", but the handle that seals is still at
        // the pre-removal epoch (a stale handle).
        val ports = FakePorts()
        val before = ports.group.epoch
        ports.group.stolenInRoster = false
        try {
            removeThenAnnounce(ports, epochBeforeRemoval = before, envelope = { ports.envelope() })
            fail("an envelope at the pre-removal epoch must not be published")
        } catch (_: RotationOrderViolation) {
            // expected
        }
        assertTrue(ports.group.published.isEmpty())
    }

    @Test
    fun `the removal has won only when the device is out, the epoch moved and the journal agrees`() {
        fun standing(inRoster: Boolean?, local: ULong?, journal: ULong?) = RemovalStanding(inRoster, local, journal)
        assertTrue(removalHasWon(standing(false, 8uL, 8uL), 7uL))
        // Still in the roster, or the roster could not be read.
        assertFalse(removalHasWon(standing(true, 8uL, 8uL), 7uL))
        assertFalse(removalHasWon(standing(null, 8uL, 8uL), 7uL))
        // The epoch did not move past the one the removal started from.
        assertFalse(removalHasWon(standing(false, 7uL, 7uL), 7uL))
        assertFalse(removalHasWon(standing(false, null, 8uL), 7uL))
        // The journal's head is another epoch: the commit may have lost.
        assertFalse(removalHasWon(standing(false, 8uL, 9uL), 7uL))
        // The journal's head could not be read: the roster and epoch decide.
        assertTrue(removalHasWon(standing(false, 8uL, null), 7uL))
        // The removal happened before this rotation started.
        assertTrue(removalHasWon(standing(false, 3uL, 3uL), null))
    }

    // --- nothing removed -----------------------------------------------------

    @Test
    fun `a refused sign-out removes nothing, changes nothing and leaves no record`() = runTest {
        val ports = FakePorts()
        val storage = MapStorage()
        ports.removal = { RemovalResult.NotRevoked(RevokeFailure.AccountKeyRequired) }
        val result = runner(ports, storage).rotate(device)
        assertTrue(result is RotationResult.NotRemoved)
        assertEquals(listOf("remove"), ports.calls)
        assertTrue(storage.map.isEmpty())
        assertTrue(ports.group.published.isEmpty())
    }

    @Test
    fun `without a proof nothing is started`() = runTest {
        val ports = FakePorts().apply { grant = null }
        val storage = MapStorage()
        val result = runner(ports, storage).rotate(device)
        assertTrue((result as RotationResult.NotStarted).reason == NotStartedReason.ProofRequired)
        assertTrue(ports.calls.isEmpty())
        assertTrue(storage.map.isEmpty())
    }

    // --- removed but not rotated ---------------------------------------------

    @Test
    fun `a removal that has not left the device list yet stops before any key is made`() = runTest {
        val ports = FakePorts()
        val storage = MapStorage()
        ports.removal = { RemovalResult.Pending(RemovalStep.DeviceGroupRemove) }
        ports.removalChangesGroup = false
        val result = runner(ports, storage).rotate(device)
        assertEquals(NotStartedReason.RemovalUnfinished, (result as RotationResult.NotStarted).reason)
        assertEquals(listOf("remove"), ports.calls)
        // The record stays (Try again continues it) and may be dropped.
        val owed = KeyRotationOwedStore(storage).load(account)!!
        assertFalse(owed.removalWon)
        assertTrue(rotationCanBeAbandoned(owed))
        assertFalse(rotationWithholdsPhrase(owed))
    }

    @Test
    fun `chats still being updated do not hold the rotation back`() = runTest {
        val ports = FakePorts()
        ports.removal = { RemovalResult.Pending(RemovalStep.RetireChats) }
        assertEquals(RotationResult.Done, runner(ports, MapStorage()).rotate(device))
    }

    @Test
    fun `a removal whose commit is not seen as won waits, then reports the keys unchanged`() = runTest {
        val ports = FakePorts()
        // The journal's head stays ahead of this device: not won.
        ports.journalEpoch = { ports.group.epoch + 1uL }
        val result = runner(ports, MapStorage()).rotate(device)
        assertEquals(NotStartedReason.RemovalUnfinished, (result as RotationResult.NotStarted).reason)
        assertFalse("prepare" in ports.calls)
        assertEquals(
            (KeyRotationRunner.REMOVAL_WAIT_TRIES - 1) * KeyRotationRunner.REMOVAL_WAIT_STEP_MS,
            ports.paused,
        )
    }

    @Test
    fun `a commit refused for want of a proof leaves the device removed and the keys unchanged`() = runTest {
        val ports = FakePorts()
        val storage = MapStorage()
        ports.commit = { throw http(403, "fresh-auth-required") }
        val result = runner(ports, storage).rotate(device)
        assertEquals(NotStartedReason.ProofRequired, (result as RotationResult.NotStarted).reason)
        val owed = KeyRotationOwedStore(storage).load(account)!!
        assertTrue(owed.removalWon)
        assertTrue(owed.announced)
        assertFalse(owed.commitSent)
        assertFalse(owed.committed)
        assertFalse("adopt" in ports.calls)
        // "Try again" continues at the commit: no second removal, no second
        // envelope.
        ports.calls.clear()
        ports.commit = { CommitAnswer.Committed("ATTEST", 5) }
        assertEquals(RotationResult.Done, runner(ports, storage).rotate(device))
        assertEquals("commit", ports.calls.first())
        assertFalse("remove" in ports.calls)
        assertFalse("announce" in ports.calls)
    }

    @Test
    fun `each definite refusal of the commit has its own reason and changes nothing`() {
        assertEquals(
            NotStartedReason.ProofRequired,
            (commitFailureOf(http(403, "fresh-auth-required")) as CommitFailure.Refused).reason,
        )
        assertEquals(
            NotStartedReason.AccountKeyRequired,
            (commitFailureOf(http(403, "account-key-required")) as CommitFailure.Refused).reason,
        )
        val limited = (commitFailureOf(http(429, "too-many-attempts", retryAfter = 60)) as CommitFailure.Refused).reason
        assertEquals(60L, (limited as NotStartedReason.RateLimited).retryAfterSecs)
        assertTrue((commitFailureOf(http(400, null)) as CommitFailure.Refused).reason is NotStartedReason.Failed)
        // No answer: the commit MAY have landed.
        assertTrue(commitFailureOf(offline()) is CommitFailure.AnswerLost)
        assertTrue(commitFailureOf(http(503, null)) is CommitFailure.AnswerLost)
    }

    @Test
    fun `Not now drops a rotation that did not start and the prepared keys with it`() = runTest {
        val ports = FakePorts()
        val storage = MapStorage()
        ports.commit = { throw http(403, "account-key-required") }
        val r = runner(ports, storage)
        r.rotate(device)
        assertTrue(r.abandon())
        assertTrue(storage.map.isEmpty())
        assertEquals(1, ports.discarded)
        assertNull(r.resume())
    }

    @Test
    fun `a rotation that has not committed never resumes by itself`() = runTest {
        val ports = FakePorts()
        val storage = MapStorage()
        ports.commit = { throw http(429, "too-many-attempts") }
        val r = runner(ports, storage)
        r.rotate(device)
        ports.calls.clear()
        // A landing must not change the account's keys without the person.
        assertNull(r.resume())
        assertTrue(ports.calls.isEmpty())
    }

    // --- the commit's answer was lost ----------------------------------------

    @Test
    fun `a commit with no answer is settled before anything else, and cannot be dropped`() = runTest {
        val ports = FakePorts()
        val storage = MapStorage()
        ports.commit = { throw offline() }
        ports.lost = { LostCommit.Unknown }
        val r = runner(ports, storage)
        val result = r.rotate(device)
        assertEquals(RotationStep.Commit, (result as RotationResult.Pending).step)
        val owed = KeyRotationOwedStore(storage).load(account)!!
        assertTrue(owed.commitSent)
        assertFalse(owed.committed)
        assertFalse(rotationCanBeAbandoned(owed))
        assertFalse(r.abandon())
        assertEquals(0, ports.discarded)
        assertFalse("adopt" in ports.calls)
    }

    @Test
    fun `a lost commit that landed is finished from the proof alone`() = runTest {
        val ports = FakePorts()
        val storage = MapStorage()
        ports.commit = { throw offline() }
        ports.lost = { LostCommit.Unknown }
        val r = runner(ports, storage)
        r.rotate(device)
        ports.calls.clear()
        // The next landing: the session upgrade by the new key is accepted.
        ports.lost = { LostCommit.Committed(keyGeneration = 6) }
        assertEquals(RotationResult.Done, r.resume())
        assertEquals("resolve-lost", ports.calls.first())
        assertFalse("commit" in ports.calls)
        assertTrue("adopt" in ports.calls)
        // No attestation in hand: the directory gets the proof alone and the
        // device list's root a plain transfer.
        assertEquals(listOf<Pair<String?, Boolean>>(null to true), ports.identityCalls)
        assertEquals(listOf<Pair<String?, Long?>>(null to 6L), ports.rootCalls)
    }

    @Test
    fun `a lost commit that did not land is the device removed and the keys unchanged`() = runTest {
        val ports = FakePorts()
        val storage = MapStorage()
        ports.lost = { LostCommit.NotCommitted }
        val r = runner(ports, storage)
        // The same run settles it and tries the commit once more.
        var commits = 0
        ports.commit = {
            commits += 1
            if (commits == 1) throw offline() else throw http(429, "too-many-attempts")
        }
        val result = r.rotate(device)
        assertTrue((result as RotationResult.NotStarted).reason is NotStartedReason.RateLimited)
        val owed = KeyRotationOwedStore(storage).load(account)!!
        assertFalse(owed.commitSent)
        assertTrue(rotationCanBeAbandoned(owed))
    }

    // --- after the commit ----------------------------------------------------

    @Test
    fun `the words are withheld until the directory and the device list carry the new key`() {
        val owed = KeyRotationOwed(deviceId, 0, removalWon = true, prepared = true, announced = true)
        assertFalse(rotationWithholdsPhrase(owed))
        val committed = owed.with(commitSent = true, committed = true)
        assertTrue(rotationWithholdsPhrase(committed))
        assertTrue(rotationWithholdsPhrase(committed.with(adopted = true)))
        assertTrue(rotationWithholdsPhrase(committed.with(adopted = true, identityPublished = true)))
        assertFalse(
            rotationWithholdsPhrase(committed.with(adopted = true, identityPublished = true, deviceGroupRootMoved = true)),
        )
        assertTrue(rotationResumesByItself(committed))
        assertFalse(rotationResumesByItself(owed))
    }

    @Test
    fun `a directory upload that did not land keeps the words back and is retried by itself`() = runTest {
        val ports = FakePorts()
        val storage = MapStorage()
        ports.identity = { _, _ -> IdentityPublish.Retry(IOException("down")) }
        val r = runner(ports, storage)
        val result = r.rotate(device)
        assertEquals(RotationStep.PublishIdentity, (result as RotationResult.Pending).step)
        assertEquals(0, ports.phraseReleased)
        assertTrue(rotationWithholdsPhrase(r.owed()!!))
        assertFalse("keypackage" in ports.calls)
        ports.calls.clear()
        ports.identity = { _, _ -> IdentityPublish.Landed }
        assertEquals(RotationResult.Done, r.resume())
        // Nothing before the commit is repeated: no second rotation.
        assertEquals(listOf("identity", "keypackage", "dg-root", "journals", "channels", "reach", "backup"), ports.calls)
        assertTrue(ports.phraseReleased >= 1)
    }

    @Test
    fun `a proof the directory no longer accepts falls back to the attestation alone`() = runTest {
        // The removed device published a key change of its own first.
        val ports = FakePorts()
        ports.identity = { _, withProof -> if (withProof) IdentityPublish.ProofRefused else IdentityPublish.Landed }
        assertEquals(RotationResult.Done, runner(ports, MapStorage()).rotate(device))
        assertEquals(listOf<Pair<String?, Boolean>>("ATTEST" to true, "ATTEST" to false), ports.identityCalls)
    }

    @Test
    fun `the fallback is not taken without an attestation`() = runTest {
        val ports = FakePorts()
        val storage = MapStorage()
        ports.commit = { throw offline() }
        ports.lost = { LostCommit.Committed(6) }
        ports.identity = { _, _ -> IdentityPublish.ProofRefused }
        val result = runner(ports, storage).rotate(device)
        assertEquals(RotationStep.PublishIdentity, (result as RotationResult.Pending).step)
        // Never an upload with neither the proof nor the attestation.
        assertEquals(listOf<Pair<String?, Boolean>>(null to true), ports.identityCalls)
    }

    @Test
    fun `an already landed upload is treated as landed`() {
        assertEquals(IdentityPublish.Landed, rotationIdentityPublishOf(null))
        assertEquals(
            IdentityPublish.AlreadyLanded,
            rotationIdentityPublishOf(
                DiscoveryHttpError(409, "/discovery/identity", """{"error":"key-generation-not-newer"}"""),
            ),
        )
        assertEquals(
            IdentityPublish.ProofRefused,
            rotationIdentityPublishOf(
                DiscoveryHttpError(400, "/discovery/identity", """{"error":"supersession-proof-invalid"}"""),
            ),
        )
        assertTrue(
            rotationIdentityPublishOf(
                DiscoveryHttpError(400, "/discovery/identity", """{"error":"key-reset-attestation-invalid"}"""),
            ) is IdentityPublish.Retry,
        )
        assertTrue(rotationIdentityPublishOf(IOException("down")) is IdentityPublish.Retry)
    }

    @Test
    fun `a publishing step that fails is retried alone and does not hold the others back`() = runTest {
        val ports = FakePorts()
        val storage = MapStorage()
        ports.channels = { false }
        ports.backup = { throw IOException("down") }
        val r = runner(ports, storage)
        val result = r.rotate(device)
        // The first step still owed is reported; the rest ran.
        assertEquals(RotationStep.MoveChannels, (result as RotationResult.Pending).step)
        assertTrue("reach" in ports.calls)
        val owed = r.owed()!!
        assertTrue(owed.journalsMoved)
        assertFalse(owed.channelsMoved)
        assertTrue(owed.reachRekeyed)
        assertFalse(owed.backupSealed)
        // The words are not held back by a channel or the backup.
        assertFalse(rotationWithholdsPhrase(owed))
        assertTrue(ports.phraseReleased >= 1)
        ports.calls.clear()
        ports.channels = { true }
        ports.backup = { true }
        assertEquals(RotationResult.Done, r.resume())
        assertEquals(listOf("channels", "backup"), ports.calls)
        assertTrue(storage.map.isEmpty())
    }

    @Test
    fun `keys that could not be stored locally are retried without a second commit`() = runTest {
        val ports = FakePorts()
        val storage = MapStorage()
        ports.adopt = { throw IllegalStateException("keystore") }
        val r = runner(ports, storage)
        assertEquals(RotationStep.Adopt, (r.rotate(device) as RotationResult.Pending).step)
        ports.calls.clear()
        ports.adopt = {}
        assertEquals(RotationResult.Done, r.resume())
        assertEquals("adopt", ports.calls.first())
        assertFalse("commit" in ports.calls)
    }

    @Test
    fun `a rotation past its commit for another device finishes before a new one starts`() = runTest {
        val ports = FakePorts()
        val storage = MapStorage()
        ports.identity = { _, _ -> IdentityPublish.Retry(IOException("down")) }
        val r = runner(ports, storage)
        r.rotate(device)
        ports.calls.clear()
        ports.identity = { _, _ -> IdentityPublish.Landed }
        // The sheet is opened for another device: the owed one is finished.
        val other = ByteArray(32) { 4 }
        assertEquals(RotationResult.Done, r.rotate(other))
        assertFalse("remove" in ports.calls)
        assertFalse("commit" in ports.calls)
    }

    // --- the sheet -----------------------------------------------------------

    @Test
    fun `the sheet shows the phrase, the proof block, or the device removed and keys unchanged`() {
        assertEquals(
            RotationSheetOutcome.ShowPhrase(withheld = false),
            rotationSheetOutcome(RotationResult.Done, phraseWithheld = false),
        )
        assertEquals(
            RotationSheetOutcome.ShowPhrase(withheld = true),
            rotationSheetOutcome(RotationResult.Pending(RotationStep.PublishIdentity), phraseWithheld = true),
        )
        assertEquals(
            RotationSheetOutcome.ShowPhrase(withheld = false),
            rotationSheetOutcome(RotationResult.Pending(RotationStep.SealBackup), phraseWithheld = false),
        )
        assertEquals(
            RotationSheetOutcome.ShowProofBlock,
            rotationSheetOutcome(RotationResult.NotStarted(NotStartedReason.ProofRequired), false),
        )
        assertEquals(
            RotationSheetOutcome.ShowProofBlock,
            rotationSheetOutcome(RotationResult.NotRemoved(RevokeFailure.ProofRequired), false),
        )
        // Nothing was removed: the removal's own line.
        assertEquals(
            RotationSheetOutcome.NothingRemoved(UiText(RemovalCopy.ERROR_NOT_REVOKED)),
            rotationSheetOutcome(RotationResult.NotRemoved(RevokeFailure.Refused(IOException())), false),
        )
        // Removed, keys unchanged: Try again / Not now.
        assertEquals(
            RotationSheetOutcome.NotStarted(null),
            rotationSheetOutcome(RotationResult.NotStarted(NotStartedReason.RemovalUnfinished), false),
        )
        assertEquals(
            RotationSheetOutcome.NotStarted(null),
            rotationSheetOutcome(RotationResult.NotStarted(NotStartedReason.Failed(IOException())), false),
        )
        assertEquals(
            RotationSheetOutcome.NotStarted(UiText(RemovalCopy.RATE_LIMITED)),
            rotationSheetOutcome(RotationResult.NotStarted(NotStartedReason.RateLimited(30)), false),
        )
        assertEquals(
            RotationSheetOutcome.NotStarted(UiText(RemovalCopy.ERROR_TRY_LATER)),
            rotationSheetOutcome(RotationResult.NotStarted(NotStartedReason.AccountKeyRequired), false),
        )
    }

    @Test
    fun `the step list has three lines in the order they happen`() {
        assertEquals(
            listOf("devices.rotate.step_remove", "devices.rotate.step_keys", "devices.rotate.step_devices"),
            ROTATION_STEP_LINES.map { it.second },
        )
        assertEquals(RotationUiStep.RemovingDevice, rotationUiStep(RotationStep.RemoveDevice))
        assertEquals(RotationUiStep.ChangingKeys, rotationUiStep(RotationStep.Commit))
        assertEquals(RotationUiStep.UpdatingDevices, rotationUiStep(RotationStep.PublishIdentity))
        assertEquals(RotationUiStep.UpdatingDevices, rotationUiStep(RotationStep.SealBackup))
    }

    // --- the envelope is never rendered --------------------------------------

    @Test
    fun `a key-rotation envelope is recognised by its domain and nothing else is`() {
        val domain = "lakko/key-rotation/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0)
        assertEquals(22, domain.size)
        assertTrue(looksLikeKeyRotationEnvelope(domain + ByteArray(170)))
        assertTrue(looksLikeKeyRotationEnvelope(domain))
        assertFalse(looksLikeKeyRotationEnvelope(domain.copyOf(21)))
        // A sibling domain and ordinary text are not it.
        assertFalse(looksLikeKeyRotationEnvelope("lakko/key-rotation/v2".toByteArray() + byteArrayOf(0)))
        assertFalse(looksLikeKeyRotationEnvelope("lakko/key-rotation/v1 hello".toByteArray()))
        assertFalse(looksLikeKeyRotationEnvelope("hello".toByteArray()))
        assertFalse(looksLikeKeyRotationEnvelope(ByteArray(0)))
    }

    @Test
    fun `the background push filter suppresses a key-rotation envelope`() {
        // 192 bytes: the domain, the version, the stamp, the entropy, the proof.
        val envelope = KEY_ROTATION_ENVELOPE_DOMAIN + ByteArray(192 - KEY_ROTATION_ENVELOPE_DOMAIN.size) { 1 }
        assertEquals(192, envelope.size)
        assertTrue(me.lakko.app.push.isServiceEnvelope(envelope, { false }, { false }, 27))
        assertFalse(
            me.lakko.app.push.isServiceEnvelope("""{"v":1,"text":"hi"}""".toByteArray(), { false }, { false }, 27),
        )
    }
}
