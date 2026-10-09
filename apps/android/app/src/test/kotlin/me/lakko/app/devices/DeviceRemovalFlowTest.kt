// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import kotlinx.coroutines.test.runTest
import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.AuthNetworkError
import me.lakko.app.auth.MarkerStorage
import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.l10n.UiText
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0066 section 2.3 — removing another device as one operation: the
 * order of the steps, the owed record and its retries, what each refusal
 * means, and what the sheet shows. Fake ports; no server, no native core.
 */
class DeviceRemovalFlowTest {

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

    private class FakePorts : DeviceRemovalPorts {
        val calls = mutableListOf<String>()
        var grant: String? = "grant"
        var signed = 0
        val statementsSent = mutableListOf<ByteArray>()
        var revoke: () -> Unit = {}
        var delete: () -> Unit = {}
        var group: (ByteArray?) -> GroupRemoval = { GroupRemoval.Removed }
        var chats: () -> Boolean = { true }
        var attestationSeen: ByteArray? = null

        override fun signStatement(deviceId: ByteArray): ByteArray {
            signed += 1
            return ByteArray(138) { signed.toByte() }
        }

        override fun freshGrant(): String? = grant

        override suspend fun revoke(deviceId: ByteArray, statement: ByteArray, freshGrant: String) {
            calls += "revoke"
            statementsSent += statement
            revoke()
        }

        override suspend fun deleteDirectoryRow(deviceId: ByteArray, statement: ByteArray) {
            calls += "delete"
            statementsSent += statement
            delete()
        }

        override suspend fun removeFromDeviceGroup(deviceId: ByteArray, attestation: ByteArray?): GroupRemoval {
            calls += "group"
            attestationSeen = attestation
            return group(attestation)
        }

        override suspend fun retireFromChats(deviceId: ByteArray): Boolean {
            calls += "chats"
            return chats()
        }
    }

    private val account = "acct"
    private val device = ByteArray(32) { (it + 5).toByte() }
    private val deviceB64 = base64UrlEncodeNoPad(device)
    private var now = 1_000_000L

    private fun fixture(ports: FakePorts = FakePorts()): Triple<DeviceRemovalRunner, DeviceRemovalOwedStore, FakePorts> {
        val store = DeviceRemovalOwedStore(MapStorage())
        return Triple(DeviceRemovalRunner(account, store, ports, clock = { now }), store, ports)
    }

    private fun offline() = AuthNetworkError("/account/devices/revoke", java.io.IOException("offline"))

    // ---- the order ----------------------------------------------------------

    @Test
    fun `the steps run auth first, then the directory, then the device list, then the chats`() {
        val fresh = DeviceRemovalOwed(deviceB64, 0L)
        assertEquals(RemovalStep.Revoke, nextRemovalStep(fresh))
        assertEquals(RemovalStep.DirectoryDelete, nextRemovalStep(fresh.with(revoked = true)))
        assertEquals(
            RemovalStep.DeviceGroupRemove,
            nextRemovalStep(fresh.with(revoked = true, directoryDeleted = true)),
        )
        assertEquals(
            RemovalStep.RetireChats,
            nextRemovalStep(fresh.with(revoked = true, directoryDeleted = true, groupRemoved = true)),
        )
        assertEquals(
            RemovalStep.Done,
            nextRemovalStep(fresh.with(revoked = true, directoryDeleted = true, groupRemoved = true, chatsRetired = true)),
        )
        // Nothing after the revoke is reachable without it.
        assertEquals(
            RemovalStep.Revoke,
            nextRemovalStep(fresh.with(directoryDeleted = true, groupRemoved = true, chatsRetired = true)),
        )
    }

    @Test
    fun `a removal that meets no failure runs all four requests in order and leaves no record`() = runTest {
        val (runner, store, ports) = fixture()
        assertSame(RemovalResult.Done, runner.remove(device, attestation = null))
        assertEquals(listOf("revoke", "delete", "group", "chats"), ports.calls)
        assertTrue(store.load(account).isEmpty())
        assertTrue(runner.pendingDeviceIds().isEmpty())
    }

    @Test
    fun `the revoke and the directory delete carry ONE signed statement`() = runTest {
        val (runner, _, ports) = fixture()
        runner.remove(device, attestation = null)
        assertEquals(1, ports.signed)
        assertEquals(2, ports.statementsSent.size)
        assertTrue(ports.statementsSent[0].contentEquals(ports.statementsSent[1]))
    }

    @Test
    fun `a statement older than its reuse window is signed again, never stored`() = runTest {
        val ports = FakePorts()
        ports.revoke = { now += REMOVAL_STATEMENT_REUSE_MS + 1 }
        val (runner, store, _) = fixture(ports)
        runner.remove(device, attestation = null)
        assertEquals(2, ports.signed)
        assertFalse(ports.statementsSent[0].contentEquals(ports.statementsSent[1]))
        // The record never holds a statement.
        assertTrue(store.load(account).isEmpty())
    }

    // ---- the revoke: if it fails, nothing else happened ----------------------

    @Test
    fun `a refused sign-out stops the removal and leaves nothing behind`() = runTest {
        val refusals = mapOf<Throwable, Class<out RevokeFailure>>(
            AuthHttpError(403, "/account/devices/revoke", "", code = "fresh-auth-required") to
                RevokeFailure.ProofRequired::class.java,
            AuthHttpError(403, "/account/devices/revoke", "", code = "account-key-required") to
                RevokeFailure.AccountKeyRequired::class.java,
            AuthHttpError(429, "/account/devices/revoke", "", code = "too-many-attempts", retryAfterSecs = 30) to
                RevokeFailure.RateLimited::class.java,
            AuthHttpError(400, "/account/devices/revoke", "") to RevokeFailure.Refused::class.java,
            AuthHttpError(500, "/account/devices/revoke", "") to RevokeFailure.Refused::class.java,
        )
        for ((err, expected) in refusals) {
            val ports = FakePorts()
            ports.revoke = { throw err }
            val (runner, store, _) = fixture(ports)
            val result = runner.remove(device, attestation = null) as RemovalResult.NotRevoked
            assertEquals(err.toString(), expected, result.failure.javaClass)
            assertEquals(listOf("revoke"), ports.calls)
            assertTrue(store.load(account).isEmpty())
        }
    }

    @Test
    fun `the rate limit carries the server's wait`() {
        val failure = revokeFailureOf(
            AuthHttpError(429, "/account/devices/revoke", "", code = "too-many-attempts", retryAfterSecs = 30),
        )
        assertEquals(30L, (failure as RevokeFailure.RateLimited).retryAfterSecs)
    }

    @Test
    fun `a sign-out with no answer keeps the record, shows no pending row, and is settled by the next run`() = runTest {
        val ports = FakePorts()
        ports.revoke = { throw offline() }
        val (runner, store, _) = fixture(ports)
        val result = runner.remove(device, attestation = null) as RemovalResult.NotRevoked
        assertTrue(result.failure is RevokeFailure.Unconfirmed)
        // The request may have landed: something must be left to retry it.
        assertEquals(1, store.load(account).size)
        assertFalse(store.load(account).single().revoked)
        // Not "Removing...": the row keeps its Remove button.
        assertTrue(runner.pendingDeviceIds().isEmpty())
        // Nothing past the revoke was attempted.
        assertEquals(listOf("revoke"), ports.calls)

        // Online again, with a proof: the same button finishes it.
        ports.revoke = {}
        assertSame(RemovalResult.Done, runner.remove(device, attestation = null))
        assertTrue(store.load(account).isEmpty())
    }

    @Test
    fun `without a live proof nothing is sent and no record is left`() = runTest {
        val ports = FakePorts().apply { grant = null }
        val (runner, store, _) = fixture(ports)
        val result = runner.remove(device, attestation = null) as RemovalResult.NotRevoked
        assertSame(RevokeFailure.ProofRequired, result.failure)
        assertTrue(ports.calls.isEmpty())
        assertTrue(store.load(account).isEmpty())
    }

    @Test
    fun `a record whose revoke was never confirmed survives a run without a proof`() = runTest {
        val ports = FakePorts()
        ports.revoke = { throw offline() }
        val (runner, store, _) = fixture(ports)
        runner.remove(device, attestation = null)
        ports.calls.clear()

        // At a landing there is no proof: nothing is sent, the record stays.
        ports.grant = null
        val resumed = runner.resume()
        assertEquals(RemovalStep.Revoke, (resumed[deviceB64] as RemovalResult.Pending).step)
        assertTrue(ports.calls.isEmpty())
        assertEquals(1, store.load(account).size)

        // The sheet without a proof: the block appears, the record stays.
        val again = runner.remove(device, attestation = null) as RemovalResult.NotRevoked
        assertSame(RevokeFailure.ProofRequired, again.failure)
        assertEquals(1, store.load(account).size)
    }

    // ---- after the revoke: every step is owed until it answers ---------------

    @Test
    fun `a device signed out at auth is never left in the groups without a retry`() = runTest {
        val ports = FakePorts()
        ports.delete = { throw java.io.IOException("offline") }
        val (runner, store, _) = fixture(ports)

        val first = runner.remove(device, attestation = null) as RemovalResult.Pending
        assertEquals(RemovalStep.DirectoryDelete, first.step)
        assertEquals(listOf("revoke", "delete"), ports.calls)
        val owed = store.load(account).single()
        assertTrue(owed.revoked)
        assertFalse(owed.directoryDeleted)
        // The row reads "Removing..." with no control.
        assertEquals(setOf(deviceB64), runner.pendingDeviceIds())

        // The next landing: the revoke is NOT sent again, the rest finishes.
        ports.calls.clear()
        ports.delete = {}
        ports.grant = null
        assertSame(RemovalResult.Done, runner.resume()[deviceB64])
        assertEquals(listOf("delete", "group", "chats"), ports.calls)
        assertTrue(store.load(account).isEmpty())
    }

    @Test
    fun `each later step resumes where it stopped`() = runTest {
        val ports = FakePorts()
        ports.group = { throw IllegalStateException("ds unreachable") }
        val (runner, store, _) = fixture(ports)
        assertEquals(
            RemovalStep.DeviceGroupRemove,
            (runner.remove(device, attestation = null) as RemovalResult.Pending).step,
        )
        assertTrue(store.load(account).single().directoryDeleted)

        ports.calls.clear()
        ports.group = { GroupRemoval.Removed }
        ports.chats = { false }
        assertEquals(RemovalStep.RetireChats, (runner.resume()[deviceB64] as RemovalResult.Pending).step)
        assertEquals(listOf("group", "chats"), ports.calls)
        assertTrue(store.load(account).single().groupRemoved)

        // The chats are retried until every one has answered.
        ports.calls.clear()
        assertEquals(RemovalStep.RetireChats, (runner.resume()[deviceB64] as RemovalResult.Pending).step)
        ports.chats = { true }
        assertSame(RemovalResult.Done, runner.resume()[deviceB64])
        assertEquals(listOf("chats", "chats"), ports.calls)
        assertTrue(store.load(account).isEmpty())
    }

    @Test
    fun `a device already out of the device list counts as removed there`() = runTest {
        val ports = FakePorts()
        ports.group = { GroupRemoval.NotInRoster }
        val (runner, _, _) = fixture(ports)
        assertSame(RemovalResult.Done, runner.remove(device, attestation = null))
        assertEquals(listOf("revoke", "delete", "group", "chats"), ports.calls)
    }

    @Test
    fun `a chat step that throws is owed, not fatal`() = runTest {
        val ports = FakePorts()
        ports.chats = { throw IllegalStateException("chat not bootstrapped") }
        val (runner, store, _) = fixture(ports)
        assertEquals(RemovalStep.RetireChats, (runner.remove(device, attestation = null) as RemovalResult.Pending).step)
        assertEquals(1, store.load(account).size)
    }

    // ---- the ADR-0015 attestation -------------------------------------------

    @Test
    fun `the attestation rides with the device-list commit while it is fresh`() = runTest {
        val (runner, _, ports) = fixture()
        val attestation = ByteArray(121) { 4 }
        runner.remove(device, attestation)
        assertTrue(ports.attestationSeen!!.contentEquals(attestation))
    }

    @Test
    fun `a device list that needs a passkey gesture waits for one instead of forking`() = runTest {
        val ports = FakePorts()
        ports.group = { att -> if (att == null) GroupRemoval.NeedsAttestation else GroupRemoval.Removed }
        val (runner, store, _) = fixture(ports)
        val first = runner.remove(device, attestation = null) as RemovalResult.Pending
        assertEquals(RemovalStep.DeviceGroupRemove, first.step)
        assertFalse(store.load(account).single().groupRemoved)

        // A later run started by the person supplies the gesture's attestation.
        assertSame(RemovalResult.Done, runner.remove(device, ByteArray(121) { 4 }))
        assertEquals(1, ports.calls.count { it == "revoke" })
    }

    @Test
    fun `a removal that needs the passkey again shows Finish removing, and the gesture finishes it`() = runTest {
        val ports = FakePorts()
        ports.group = { att -> if (att == null) GroupRemoval.NeedsAttestation else GroupRemoval.Removed }
        val (runner, store, _) = fixture(ports)
        // No passkey gesture was made (a relaunch): the step waits.
        runner.remove(device, attestation = null)
        val owed = store.load(account).single()
        assertTrue(owed.revoked)
        assertTrue(owed.needsAttestation)
        assertTrue(removalShowsPending(owed))
        assertTrue(removalWaitsOnPerson(owed, now))
        assertEquals(setOf(deviceB64), runner.pendingDeviceIds())
        assertEquals(setOf(deviceB64), runner.waitingOnPersonDeviceIds())

        // The landing's retry cannot finish it either, and never drops the mark.
        ports.grant = null
        assertEquals(RemovalStep.DeviceGroupRemove, (runner.resume()[deviceB64] as RemovalResult.Pending).step)
        assertEquals(setOf(deviceB64), runner.waitingOnPersonDeviceIds())

        // `Finish removing`: the gesture's attestation, no proof, no second
        // sign-out at auth, and nothing is left owed.
        assertSame(RemovalResult.Done, runner.remove(device, ByteArray(121) { 4 }))
        assertEquals(1, ports.calls.count { it == "revoke" })
        assertTrue(store.load(account).isEmpty())
        assertTrue(runner.waitingOnPersonDeviceIds().isEmpty())
    }

    @Test
    fun `an attestation that aged out before the device list took it puts the button back`() = runTest {
        val ports = FakePorts()
        var reachable = false
        ports.group = { att ->
            if (!reachable) throw java.io.IOException("offline")
            if (att == null) GroupRemoval.NeedsAttestation else GroupRemoval.Removed
        }
        val (runner, store, _) = fixture(ports)
        // The gesture was made, but the device list could not be reached.
        runner.remove(device, ByteArray(121) { 4 })
        assertFalse(removalWaitsOnPerson(store.load(account).single(), now))
        assertTrue(runner.waitingOnPersonDeviceIds().isEmpty())

        // Back online after the attestation's window: the step says it needs
        // one, and from then on the row carries the button.
        now += REMOVAL_ATTESTATION_USABLE_MS + 1
        reachable = true
        runner.resume()
        assertEquals(setOf(deviceB64), runner.waitingOnPersonDeviceIds())
    }

    @Test
    fun `only a signed-out device still in the device list ever waits on the person`() {
        val base = DeviceRemovalOwed(deviceB64, 0L, needsAttestation = true)
        // Not signed out at auth yet: the row keeps Remove, which is the retry.
        assertFalse(removalWaitsOnPerson(base, 10L))
        assertTrue(removalWaitsOnPerson(base.with(revoked = true), 10L))
        // Out of the device list already: the chats finish by themselves.
        assertFalse(removalWaitsOnPerson(base.with(revoked = true, groupRemoved = true), 10L))
        // A usable attestation is held: the next run uses it.
        val held = base.with(
            revoked = true,
            attestationB64Url = base64UrlEncodeNoPad(ByteArray(121) { 4 }),
            attestationAtMs = 5L,
        )
        assertFalse(removalWaitsOnPerson(held, 10L))
        // A step that never asked for one (offline, the directory): no button.
        assertFalse(removalWaitsOnPerson(DeviceRemovalOwed(deviceB64, 0L, revoked = true), 10L))
        // A row written before the mark existed reads as not waiting.
        val old = DeviceRemovalOwedStore(MapStorage().apply {
            set("device-removal-v1:acct", """[{"deviceIdB64Url":"$deviceB64","startedAtMs":1,"revoked":true}]""")
        })
        assertFalse(old.load("acct").single().needsAttestation)
    }

    @Test
    fun `a stored attestation is not used after its window`() {
        val owed = DeviceRemovalOwed(
            deviceB64,
            startedAtMs = 0L,
            attestationB64Url = base64UrlEncodeNoPad(ByteArray(121) { 4 }),
            attestationAtMs = 1_000L,
        )
        assertTrue(usableAttestation(owed, 1_000L + REMOVAL_ATTESTATION_USABLE_MS)!!.isNotEmpty())
        assertNull(usableAttestation(owed, 1_001L + REMOVAL_ATTESTATION_USABLE_MS))
        // A clock set back never extends it; a missing or broken row is none.
        assertNull(usableAttestation(owed, 999L))
        assertNull(usableAttestation(DeviceRemovalOwed(deviceB64, 0L), 1_000L))
        assertNull(usableAttestation(owed.with(attestationB64Url = "***"), 1_000L))
    }

    // ---- the gate -----------------------------------------------------------

    @Test
    fun `no removal runs while a key reset's publish is owed, and none without the account keys`() {
        assertEquals(RemovalGate.Allowed, removalGate(holdsAccountKeys = true, keyResetPublishOwed = false))
        assertEquals(RemovalGate.KeyResetPublishOwed, removalGate(holdsAccountKeys = true, keyResetPublishOwed = true))
        assertEquals(RemovalGate.NoAccountKeys, removalGate(holdsAccountKeys = false, keyResetPublishOwed = false))
        assertEquals(RemovalGate.NoAccountKeys, removalGate(holdsAccountKeys = false, keyResetPublishOwed = true))
        assertNull(removalGateLine(RemovalGate.Allowed))
        // The existing "publish owed" copy.
        assertEquals(UiText("recovery_phrase.fresh.publish_owed"), removalGateLine(RemovalGate.KeyResetPublishOwed))
    }

    // ---- the store ----------------------------------------------------------

    @Test
    fun `the store keeps one record per device and account`() {
        val store = DeviceRemovalOwedStore(MapStorage())
        store.save("A", DeviceRemovalOwed("d1", 1L))
        store.save("A", DeviceRemovalOwed("d2", 2L, revoked = true))
        store.save("B", DeviceRemovalOwed("d1", 3L))
        store.save("A", DeviceRemovalOwed("d1", 1L, revoked = true, directoryDeleted = true))
        assertEquals(2, store.load("A").size)
        assertTrue(store.find("A", "d1")!!.directoryDeleted)
        assertFalse(store.find("B", "d1")!!.revoked)
        store.clear("A", "d1")
        assertNull(store.find("A", "d1"))
        assertEquals(listOf("d2"), store.load("A").map { it.deviceIdB64Url })
        store.clearAccount("A")
        assertTrue(store.load("A").isEmpty())
        assertEquals(1, store.load("B").size)
    }

    @Test
    fun `a malformed row reads as nothing owed`() {
        val storage = MapStorage()
        val store = DeviceRemovalOwedStore(storage)
        store.save("A", DeviceRemovalOwed("d1", 1L))
        storage.map.keys.toList().forEach { storage.map[it] = "{not json" }
        assertTrue(store.load("A").isEmpty())
    }

    @Test
    fun `a record with an unreadable device id is dropped at the next landing`() = runTest {
        val (runner, store, ports) = fixture()
        store.save(account, DeviceRemovalOwed("***", 1L, revoked = true))
        assertTrue(runner.resume().isEmpty())
        assertTrue(store.load(account).isEmpty())
        assertTrue(ports.calls.isEmpty())
    }

    // ---- what the sheet shows -----------------------------------------------

    @Test
    fun `the sheet closes when the device is signed out, whether or not the rest has finished`() {
        assertSame(RemovalSheetOutcome.Close, removalSheetOutcome(RemovalResult.Done))
        assertSame(RemovalSheetOutcome.Close, removalSheetOutcome(RemovalResult.Pending(RemovalStep.RetireChats)))
    }

    @Test
    fun `a proof that ran out shows the block again, with no error`() {
        assertSame(
            RemovalSheetOutcome.ShowProofBlock,
            removalSheetOutcome(RemovalResult.NotRevoked(RevokeFailure.ProofRequired)),
        )
    }

    @Test
    fun `every other failure of the sign-out keeps the sheet open with a line that is in the catalog`() {
        val failures = listOf(
            RevokeFailure.AccountKeyRequired,
            RevokeFailure.RateLimited(30),
            RevokeFailure.Unconfirmed(offline()),
            RevokeFailure.Refused(IllegalStateException("x")),
        )
        val catalog = listOf("src/main/assets/l10n/source.json", "app/src/main/assets/l10n/source.json")
            .map { java.io.File(it) }.first { it.exists() }.readText()
        for (failure in failures) {
            val line = removalSheetOutcome(RemovalResult.NotRevoked(failure)) as RemovalSheetOutcome.Line
            assertTrue("missing catalog key ${line.text.key}", catalog.contains("\"${line.text.key}\": {"))
        }
        assertEquals(
            "common.error.too_many_attempts",
            (removalSheetOutcome(RemovalResult.NotRevoked(RevokeFailure.RateLimited(30))) as RemovalSheetOutcome.Line).text.key,
        )
    }

    @Test
    fun `every key the removal surfaces draw is in the catalog today`() {
        val catalog = listOf("src/main/assets/l10n/source.json", "app/src/main/assets/l10n/source.json")
            .map { java.io.File(it) }.first { it.exists() }.readText()
        val keys = listOf(
            RemovalCopy.SHEET_TITLE, RemovalCopy.SHEET_BODY, RemovalCopy.CONFIRM, RemovalCopy.WORKING,
            RemovalCopy.ROW_PENDING, RemovalCopy.ROW_FINISH, RemovalCopy.DONE,
            RemovalCopy.ERROR_NOT_REVOKED, RemovalCopy.ERROR_TRY_LATER,
            RemovalCopy.BLOCKED_PUBLISH_OWED, RemovalCopy.RATE_LIMITED, RemovalCopy.REMOVED_TITLE,
            RemovalCopy.REMOVED_BODY_REMOVING, RemovalCopy.REMOVED_BODY_DONE, RemovalCopy.REMOVED_PROGRESS,
            RemovalCopy.REMOVED_SIGN_IN,
        )
        for (key in keys) assertTrue("missing catalog key $key", catalog.contains("\"$key\": {"))
    }

    @Test
    fun `the removal surfaces use the keys of UX 18_6, not stand-ins`() {
        assertEquals("settings.devices.remove.confirm", RemovalCopy.CONFIRM)
        assertEquals("settings.devices.remove.working", RemovalCopy.WORKING)
        assertEquals("devices.remove.pending", RemovalCopy.ROW_PENDING)
        assertEquals("devices.remove.finish", RemovalCopy.ROW_FINISH)
        assertEquals("devices.remove.done", RemovalCopy.DONE)
        assertEquals("devices.remove.error.not_revoked", RemovalCopy.ERROR_NOT_REVOKED)
        assertEquals("devices.remove.error.try_later", RemovalCopy.ERROR_TRY_LATER)
        assertEquals("devices.removed.body_removing", RemovalCopy.REMOVED_BODY_REMOVING)
        assertEquals("devices.removed.body_done", RemovalCopy.REMOVED_BODY_DONE)
    }
}
