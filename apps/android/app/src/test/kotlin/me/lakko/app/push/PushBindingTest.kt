// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import kotlinx.coroutines.test.runTest
import me.lakko.app.ds.PushBindingOutcome
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0057 obligation 8, as amended by the second security pass (finding 5) —
 * the owner (or an admin) binds the group's journal to its push routing id
 * BEFORE broadcasting a minted or rotated secret; a 409 re-mints once; a 503
 * or a failure broadcasts anyway and binds again on the next pass; a member
 * never binds, and a failed bind never fails the mint or the rotation.
 */
class PushBindingTest {

    private val account = "acct=="
    private val group = "bb".repeat(32)

    private class Ports : PushRoutingPorts {
        val secrets = mutableMapOf<String, Pair<ByteArray, ULong>>()
        val bound = mutableListOf<ByteArray>()
        val events = mutableListOf<String>()
        var bindFails = false
        val bindAnswers = ArrayDeque<PushBindingOutcome>()
        private var minted = 0

        override fun mintSecret(): ByteArray {
            minted += 1
            return ByteArray(32) { (minted * 16 + it).toByte() }
        }

        override fun derivePushGid(secret: ByteArray): ByteArray = ByteArray(32) { (secret[31 - it].toInt() xor 0x5A).toByte() }

        override val isNewer: GroupPushRouting.PushRoutingSecretIsNewer =
            GroupPushRouting.PushRoutingSecretIsNewer { a, _, b, _ -> a > b }

        override fun composeEnvelope(stamp: GroupPushRouting.MintStamp, secret: ByteArray): ByteArray =
            byteArrayOf(0x6c) + secret

        override suspend fun freshMintStamp(): GroupPushRouting.MintStamp.Fresh = GroupPushRouting.MintStamp.Fresh(3uL)

        override fun hasSecret(groupIdHex: String): Boolean = secrets.containsKey(groupIdHex)

        override fun getRecord(groupIdHex: String): GroupPushRouting.StoredSecret? =
            secrets[groupIdHex]?.let { GroupPushRouting.StoredSecret(it.first, it.second) }

        override fun isDistributed(groupIdHex: String): Boolean = true

        override fun persist(groupIdHex: String, secret: ByteArray, mintEpoch: ULong, pushGid: ByteArray, distributed: Boolean) {
            secrets[groupIdHex] = secret to mintEpoch
        }

        override fun markDistributed(groupIdHex: String) {}

        override fun markPending(groupIdHex: String) {}

        override suspend fun subscribeUnderPushGid(pushGid: ByteArray) {}

        override suspend fun unsubscribeFromPushGid(pushGid: ByteArray) {}

        override fun deleteRoute(pushGid: ByteArray) {}

        override suspend fun broadcast(envelope: ByteArray) {
            events += "broadcast"
        }

        override suspend fun bindPushGid(pushGid: ByteArray): PushBindingOutcome {
            events += "bind"
            if (bindFails) throw IllegalStateException("offline")
            val answer = bindAnswers.removeFirstOrNull() ?: PushBindingOutcome.BOUND
            if (answer == PushBindingOutcome.BOUND) bound += pushGid
            return answer
        }
    }

    @Test
    fun `the owner binds the freshly minted id before broadcasting it`() = runTest {
        val ports = Ports()
        GroupPushRouting.ensure(account, group, isOwner = true, ports = ports)
        val secret = ports.secrets.getValue(group).first
        assertEquals(listOf("bind", "broadcast"), ports.events)
        assertArrayEquals(ports.derivePushGid(secret), ports.bound.single())
    }

    @Test
    fun `a member never binds`() = runTest {
        val ports = Ports()
        ports.secrets[group] = ByteArray(32) { 1 } to 2uL
        GroupPushRouting.ensure(account, group, isOwner = false, ports = ports)
        assertTrue(ports.bound.isEmpty())
    }

    @Test
    fun `an owner holding a secret from before re-binds it`() = runTest {
        val ports = Ports()
        val held = ByteArray(32) { 9 }
        ports.secrets[group] = held to 1uL
        GroupPushRouting.ensure(account, group, isOwner = true, ports = ports)
        assertArrayEquals(ports.derivePushGid(held), ports.bound.single())
    }

    @Test
    fun `a rotation binds the new id before redistributing`() = runTest {
        val ports = Ports()
        ports.secrets[group] = ByteArray(32) { 1 } to 1uL
        val outcome = GroupPushRouting.rotate(account, group, GroupPushRouting.RotationRole.ADMIN, ports)
        assertEquals(GroupPushRouting.RotationOutcome.ROTATED, outcome)
        val next = ports.secrets.getValue(group).first
        assertEquals(listOf("bind", "broadcast"), ports.events)
        assertArrayEquals(ports.derivePushGid(next), ports.bound.single())
    }

    @Test
    fun `a failed bind never fails the mint`() = runTest {
        val ports = Ports().apply { bindFails = true }
        GroupPushRouting.ensure(account, "cc".repeat(32), isOwner = true, ports = ports)
        assertTrue(ports.secrets.containsKey("cc".repeat(32)))
        assertEquals(listOf("bind", "broadcast"), ports.events)
    }

    @Test
    fun `a 409 mints a fresh secret once and binds that one`() = runTest {
        val ports = Ports()
        ports.bindAnswers += PushBindingOutcome.CONFLICT
        GroupPushRouting.ensure(account, "dd".repeat(32), isOwner = true, ports = ports)
        val secret = ports.secrets.getValue("dd".repeat(32)).first
        assertEquals(listOf("bind", "bind", "broadcast"), ports.events)
        // The broadcast secret is the SECOND mint, whose id is the bound one.
        assertEquals((2 * 16).toByte(), secret[0])
        assertArrayEquals(ports.derivePushGid(secret), ports.bound.single())
    }

    @Test
    fun `a second 409 broadcasts unbound and never mints a third time`() = runTest {
        val ports = Ports()
        ports.bindAnswers += PushBindingOutcome.CONFLICT
        ports.bindAnswers += PushBindingOutcome.CONFLICT
        GroupPushRouting.ensure(account, "ee".repeat(32), isOwner = true, ports = ports)
        assertEquals(listOf("bind", "bind", "broadcast"), ports.events)
        assertTrue(ports.bound.isEmpty())
        assertTrue(ports.secrets.containsKey("ee".repeat(32)))
    }

    @Test
    fun `a disabled binding broadcasts anyway and an admin binds again on the next pass`() = runTest {
        val ports = Ports()
        val g = "ff".repeat(32)
        ports.secrets[g] = ByteArray(32) { 1 } to 1uL
        ports.bindAnswers += PushBindingOutcome.DISABLED
        val outcome = GroupPushRouting.rotate(account, g, GroupPushRouting.RotationRole.ADMIN, ports)
        assertEquals(GroupPushRouting.RotationOutcome.ROTATED, outcome)
        assertEquals(listOf("bind", "broadcast"), ports.events)
        assertTrue(ports.bound.isEmpty())
        // The next provisioning pass (an admin is not the owner) binds again.
        GroupPushRouting.ensure(account, g, isOwner = false, ports = ports)
        val next = ports.secrets.getValue(g).first
        assertArrayEquals(ports.derivePushGid(next), ports.bound.single())
    }
}
