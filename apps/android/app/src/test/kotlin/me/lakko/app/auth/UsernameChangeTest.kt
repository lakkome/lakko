// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import java.io.IOException
import kotlinx.coroutines.test.runTest
import me.lakko.app.discovery.DiscoveryHttpError
import me.lakko.app.discovery.DiscoveryNetworkError
import me.lakko.app.l10n.UiText
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0076 - the handle sync, the own-username register and the rename. */
class UsernameChangeTest {

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

    // ---- the session's handle sync ----------------------------------------

    @Test
    fun `the session binds only when the account holds no handle`() {
        assertEquals(HandleSyncAction.Nothing, handleSyncAction(LoginHandleState.Same))
        assertEquals(HandleSyncAction.Bind, handleSyncAction(LoginHandleState.None))
        // Renamed elsewhere: a bind here would revert it.
        assertEquals(HandleSyncAction.WaitForRegister, handleSyncAction(LoginHandleState.Other))
        assertEquals(LoginHandleState.Other, LoginHandleState.ofWire("other"))
        assertNull(LoginHandleState.ofWire("surprise"))
    }

    private class Binder(private val state: LoginHandleState) : LoginHandleBinder {
        var binds = 0
        var checks = 0
        override suspend fun bind(sessionToken: String, handle: ByteArray): LoginHandleBinding {
            binds += 1
            return LoginHandleBinding.Bound
        }
        override suspend fun unbind(sessionToken: String): Boolean = error("never unbinds")
        override suspend fun check(sessionToken: String, handle: ByteArray): LoginHandleState {
            checks += 1
            return state
        }
    }

    private suspend fun syncWith(state: LoginHandleState): Binder {
        val binder = Binder(state)
        val launched = mutableListOf<suspend () -> Unit>()
        LoginHandleSessionRebinder(
            source = { name -> NamedHandle(username = name, handle = ByteArray(32) { 3 }) },
            binder = binder,
            launch = { launched += it },
            log = {},
        ).onSessionAvailable(LoginHandleBindTarget(sessionToken = "tok", username = "alice"))
        launched.forEach { it() }
        return binder
    }

    @Test
    fun `a stale device does not revert a rename`() = runTest {
        val renamedElsewhere = syncWith(LoginHandleState.Other)
        assertEquals(1, renamedElsewhere.checks)
        assertEquals(0, renamedElsewhere.binds)
        assertEquals(0, syncWith(LoginHandleState.Same).binds)
        assertEquals(1, syncWith(LoginHandleState.None).binds)
    }

    // ---- the own-username register ------------------------------------------

    private val closed = OwnUsernameState.NOT_READY
    private fun register(name: String?) = OwnUsernameState(ready = true, username = name)

    private fun sync(
        register: OwnUsernameState,
        local: String,
        parked: String? = null,
        renaming: Boolean = false,
        confirmed: Boolean = true,
    ) = ownUsernameSync(register, local, parked, renaming, handleConfirmed = confirmed)

    @Test
    fun `a newer name from the register is adopted by the local marker`() {
        assertEquals(OwnUsernameSync.Adopt("alicia"), sync(register("alicia"), "alice"))
        // Adopting never waits for the handle check: the register is the word.
        assertEquals(OwnUsernameSync.Adopt("alicia"), sync(register("alicia"), "alice", confirmed = false))
        // The same name: nothing.
        assertEquals(OwnUsernameSync.Nothing, sync(register("alice"), "alice"))
        // A guest device learns the name another device claimed.
        assertEquals(OwnUsernameSync.Adopt("alice"), sync(register("alice"), ""))
    }

    @Test
    fun `an account from before the register seeds it once from its marker`() {
        assertEquals(OwnUsernameSync.Seed("alice"), sync(register(null), "alice", confirmed = true))
        // Seeded: nothing more.
        assertEquals(OwnUsernameSync.Nothing, sync(register("alice"), "alice"))
        // A guest has nothing to seed.
        assertEquals(OwnUsernameSync.Nothing, sync(register(null), ""))
    }

    @Test
    fun `a stale device does not seed`() {
        // The account was renamed elsewhere and this device has not received
        // the register yet: auth answered `other` (or has not answered), so
        // the local name is NOT confirmed. Seeding it would stamp the old
        // name newer than the rename and undo it on every device.
        assertEquals(OwnUsernameSync.Nothing, sync(register(null), "alice", confirmed = false))
        // When the register arrives, the device adopts the new name instead.
        assertEquals(OwnUsernameSync.Adopt("alicia"), sync(register("alicia"), "alice", confirmed = false))
    }

    @Test
    fun `only a same answer or this device's own bind confirms a handle`() = runTest {
        suspend fun confirmedAfter(state: LoginHandleState): Boolean {
            val launched = mutableListOf<suspend () -> Unit>()
            val rebinder = LoginHandleSessionRebinder(
                source = { name -> NamedHandle(username = name, handle = ByteArray(32) { 3 }) },
                binder = Binder(state),
                launch = { launched += it },
                log = {},
            )
            rebinder.onSessionAvailable(LoginHandleBindTarget(sessionToken = "tok", username = "alice"))
            // The check has not answered yet: not confirmed.
            assertFalse(rebinder.isConfirmed("alice"))
            launched.forEach { it() }
            return rebinder.isConfirmed("alice")
        }
        assertTrue(confirmedAfter(LoginHandleState.Same))
        assertTrue("bound here on none", confirmedAfter(LoginHandleState.None))
        assertFalse("renamed elsewhere", confirmedAfter(LoginHandleState.Other))
    }

    @Test
    fun `a failed check or a taken bind confirms nothing, and a session's end forgets`() = runTest {
        val launched = mutableListOf<suspend () -> Unit>()
        val failing = object : LoginHandleBinder {
            override suspend fun bind(sessionToken: String, handle: ByteArray) = LoginHandleBinding.Taken
            override suspend fun unbind(sessionToken: String): Boolean = error("never unbinds")
            override suspend fun check(sessionToken: String, handle: ByteArray): LoginHandleState = LoginHandleState.None
        }
        var told = 0
        val rebinder = LoginHandleSessionRebinder(
            source = { name -> NamedHandle(username = name, handle = name.toByteArray().copyOf(32)) },
            binder = failing,
            launch = { launched += it },
            log = {},
            onConfirmed = { told += 1 },
        )
        rebinder.onSessionAvailable(LoginHandleBindTarget(sessionToken = "tok", username = "alice"))
        launched.forEach { it() }
        assertFalse(rebinder.isConfirmed("alice"))
        assertEquals(0, told)
        // A bind this device made itself (sign-up, a claim, a rename).
        rebinder.markBound("tok", "bob")
        assertTrue(rebinder.isConfirmed("bob"))
        assertEquals(1, told)
        rebinder.forgetConfirmed()
        assertFalse(rebinder.isConfirmed("bob"))
    }

    @Test
    fun `nothing happens before the state is open or during this device's own rename`() {
        assertEquals(OwnUsernameSync.Nothing, sync(closed, "alice", parked = "alice"))
        assertEquals(OwnUsernameSync.Nothing, sync(register("alice"), "alicia", renaming = true))
    }

    @Test
    fun `a name written while the state was closed goes first`() {
        assertEquals(OwnUsernameSync.Flush("alice"), sync(register(null), "alice", parked = "alice"))
        // Once the register holds it, the parked write is spent.
        assertEquals(OwnUsernameSync.Nothing, sync(register("alice"), "alice", parked = "alice"))
    }

    // ---- too soon, and what the screen shows --------------------------------

    @Test
    fun `one rename per 30 days by the device's own stamp`() {
        val day = 24L * 60L * 60L * 1000L
        val changed = 100L * day
        assertNull(renameAllowedAtMs(lastChangedAtMs = null, nowMs = changed))
        assertEquals(changed + 30 * day, renameAllowedAtMs(changed, changed + day))
        assertEquals(changed + 30 * day, renameAllowedAtMs(changed, changed + 30 * day - 1))
        assertNull(renameAllowedAtMs(changed, changed + 30 * day))
        // A clock set back never keeps the form closed.
        assertNull(renameAllowedAtMs(changed, changed - day))
        // The server's answer names the date.
        assertEquals(1_000L + 60_000L, renameAllowedAfterSecs(retryAfterSecs = 60L, nowMs = 1_000L))
    }

    @Test
    fun `the recovery email warning shows only without a held address`() {
        assertEquals(RenameWarnings(standard = true, recoveryEmailTurnsOff = false), renameWarnings(true, true))
        assertEquals(RenameWarnings(standard = true, recoveryEmailTurnsOff = true), renameWarnings(true, false))
        assertEquals(RenameWarnings(standard = true, recoveryEmailTurnsOff = false), renameWarnings(false, false))
        assertEquals(RenameWarnings(standard = true, recoveryEmailTurnsOff = false), renameWarnings(false, true))
    }

    @Test
    fun `the bind carries the address, and a vault only while no phrase is saved`() {
        assertEquals(RenameRecoveryEmail.AddressAndVault, renameRecoveryEmail(true, addressHeld = true, phraseSaved = false))
        assertEquals(RenameRecoveryEmail.AddressOnly, renameRecoveryEmail(true, addressHeld = true, phraseSaved = true))
        assertEquals(RenameRecoveryEmail.None, renameRecoveryEmail(true, addressHeld = false, phraseSaved = false))
        assertEquals(RenameRecoveryEmail.None, renameRecoveryEmail(false, addressHeld = true, phraseSaved = false))
    }

    // ---- the rename machine -------------------------------------------------

    private class Ports : UsernameRenamePorts {
        val calls = mutableListOf<String>()
        var bind: (from: String, to: String) -> Unit = { _, _ -> }
        var replace: (String) -> ByteArray? = { ByteArray(153) { 1 } }
        var adopt: (String, ByteArray?) -> Unit = { _, _ -> }
        var adopted: Pair<String, ByteArray?>? = null

        override suspend fun bind(from: String, to: String) {
            calls += "bind:$from>$to"
            bind.invoke(from, to)
        }

        override suspend fun replace(newUsername: String): ByteArray? {
            calls += "replace:$newUsername"
            return replace.invoke(newUsername)
        }

        override suspend fun adopt(newUsername: String, certificate: ByteArray?) {
            calls += "adopt:$newUsername"
            adopt.invoke(newUsername, certificate)
            adopted = newUsername to certificate
        }
    }

    private fun runner(ports: Ports, storage: MapStorage = MapStorage()) =
        UsernameRenameRunner("acct", UsernameRenameStore(storage), ports, nowMs = { 5_000L })

    private fun discovery(status: Int, body: String) = DiscoveryHttpError(status, "/discovery/username/replace", body)

    @Test
    fun `a rename binds the new handle, replaces the name, then adopts it, and leaves no record`() = runTest {
        val ports = Ports()
        val storage = MapStorage()
        val result = runner(ports, storage).start("alice", "alicia")
        assertEquals(RenameResult.Done("alicia"), result)
        assertEquals(listOf("bind:alice>alicia", "replace:alicia", "adopt:alicia"), ports.calls)
        assertEquals(153, ports.adopted!!.second!!.size)
        assertTrue(storage.rows.isEmpty())
    }

    @Test
    fun `a taken name binds the old handle back and shows the taken line`() = runTest {
        val ports = Ports().apply { replace = { throw discovery(409, """{"error":"username-taken"}""") } }
        val storage = MapStorage()
        val result = runner(ports, storage).start("alice", "bob")
        assertEquals(RenameResult.Refused(UiText("auth.signup.error.username_taken")), result)
        assertEquals(listOf("bind:alice>bob", "replace:bob", "bind:bob>alice"), ports.calls)
        assertNull(ports.adopted)
        assertTrue("rolled back: nothing is owed", storage.rows.isEmpty())
    }

    @Test
    fun `too soon rolls back and names the date`() = runTest {
        val ports = Ports().apply {
            replace = { throw discovery(429, """{"error":"rename-too-soon","retry_after_secs":120}""") }
        }
        val result = runner(ports).start("alice", "alicia") as RenameResult.Refused
        assertEquals(UiText("username.change.too_soon"), result.line)
        assertEquals(5_000L + 120_000L, result.allowedAtMs)
        assertEquals("bind:alicia>alice", ports.calls.last())
    }

    @Test
    fun `a refused bind changes nothing and asks for the proof when that is why`() = runTest {
        val taken = Ports().apply {
            bind = { _, _ -> throw AuthClientArgError("taken", UiText("auth.signup.error.username_taken")) }
        }
        val storage = MapStorage()
        assertEquals(
            RenameResult.Refused(UiText("auth.signup.error.username_taken")),
            runner(taken, storage).start("alice", "bob"),
        )
        assertEquals(listOf("bind:alice>bob"), taken.calls)
        assertTrue(storage.rows.isEmpty())

        val noProof = Ports().apply { bind = { _, _ -> throw FreshAuthRequired() } }
        assertTrue((runner(noProof).start("alice", "bob") as RenameResult.Refused).needsProof)
    }

    @Test
    fun `a replace with no answer keeps the record and the next landing finishes it`() = runTest {
        val storage = MapStorage()
        val ports = Ports().apply { replace = { throw DiscoveryNetworkError("/discovery/username/replace", IOException("down")) } }
        val pending = runner(ports, storage).start("alice", "alicia")
        assertEquals(RenameResult.Pending(UiText("common.error.offline")), pending)
        val owed = UsernameRenameStore(storage).load("acct")!!
        assertTrue(owed.bound)
        assertFalse(owed.replaced)
        assertEquals(RenameStep.Replace, nextRenameStep(owed))
        // The landing: the bind is not repeated.
        val later = Ports()
        assertEquals(RenameResult.Done("alicia"), runner(later, storage).resume())
        assertEquals(listOf("replace:alicia", "adopt:alicia"), later.calls)
        assertTrue(storage.rows.isEmpty())
    }

    @Test
    fun `a crash after both servers moved is finished by adopting the name`() = runTest {
        val storage = MapStorage()
        UsernameRenameStore(storage).save(
            "acct",
            UsernameRenameOwed("alice", "alicia", bindSent = true, bound = true, replaced = true),
        )
        val ports = Ports()
        assertEquals(RenameResult.Done("alicia"), runner(ports, storage).resume())
        assertEquals(listOf("adopt:alicia"), ports.calls)
        // No certificate is in hand: the adopt is told so (it fetches it).
        assertNull(ports.adopted!!.second)
    }

    @Test
    fun `a landing that finds the name taken rolls back, and keeps the record when it cannot`() = runTest {
        val storage = MapStorage()
        UsernameRenameStore(storage).save("acct", UsernameRenameOwed("alice", "bob", bindSent = true, bound = true))
        // The roll-back needs a proof the landing does not hold.
        val stuck = Ports().apply {
            replace = { throw discovery(409, """{"error":"username-taken"}""") }
            bind = { _, _ -> throw FreshAuthRequired() }
        }
        assertTrue(runner(stuck, storage).resume() is RenameResult.Pending)
        assertTrue(UsernameRenameStore(storage).load("acct") != null)
        // A later run that holds one rolls back.
        val able = Ports().apply { replace = { throw discovery(409, """{"error":"username-taken"}""") } }
        assertTrue(runner(able, storage).resume() is RenameResult.Refused)
        assertEquals("bind:bob>alice", able.calls.last())
        assertTrue(storage.rows.isEmpty())
    }

    @Test
    fun `a bind that may have landed is not forgotten when the proof ran out`() = runTest {
        val storage = MapStorage()
        val lost = Ports().apply { bind = { _, _ -> throw AuthNetworkError("/auth/login-handle", IOException("down")) } }
        assertTrue(runner(lost, storage).start("alice", "alicia") is RenameResult.Pending)
        assertTrue(UsernameRenameStore(storage).load("acct")!!.bindSent)
        val noProof = Ports().apply { bind = { _, _ -> throw FreshAuthRequired() } }
        assertTrue(runner(noProof, storage).resume() is RenameResult.Pending)
        assertTrue("still owed", UsernameRenameStore(storage).load("acct") != null)
        // With a proof the same bind is asked again and the rename finishes.
        assertEquals(RenameResult.Done("alicia"), runner(Ports(), storage).resume())
    }

    @Test
    fun `nothing owed, nothing resumed, and null when there is no record`() = runTest {
        assertNull(runner(Ports()).resume())
    }

    @Test
    fun `the 409 bodies are read as JSON codes`() {
        assertTrue(isUsernameTaken(DiscoveryHttpError(409, "/discovery/register", """{"error":"username-taken"}""")))
        assertFalse(isUsernameTaken(DiscoveryHttpError(409, "/discovery/register", """{"error":"username-already-set"}""")))
        assertFalse(isUsernameTaken(DiscoveryHttpError(409, "/discovery/username/replace", """{"error":"no-username"}""")))
        // The older bodiless answer still reads as taken.
        assertTrue(isUsernameTaken(DiscoveryHttpError(409, "/discovery/register", "username already registered")))
        assertTrue(isUsernameTaken(AuthHttpError(409, "/auth/login-handle", "", code = "handle-taken")))
        assertFalse(isUsernameTaken(AuthHttpError(409, "/auth/login-handle", "", code = "handle-mismatch")))
        assertEquals(
            120L,
            renameTooSoonSecs(DiscoveryHttpError(429, "/x", """{"error":"rename-too-soon","retry_after_secs":120}""")),
        )
        assertNull(renameTooSoonSecs(DiscoveryHttpError(429, "/x", """{"error":"rate-limited"}""")))
    }

    @Test
    fun `the store keeps the record and the stamp per account`() {
        val storage = MapStorage()
        val store = UsernameRenameStore(storage)
        assertNull(store.load("a"))
        assertNull(store.lastChangedAtMs("a"))
        store.save("a", UsernameRenameOwed("alice", "alicia", bindSent = true))
        store.markChanged("a", 77L)
        assertEquals(UsernameRenameOwed("alice", "alicia", bindSent = true), UsernameRenameStore(storage).load("a"))
        assertEquals(77L, store.lastChangedAtMs("a"))
        assertNull(store.load("b"))
        store.clearAccount("a")
        assertNull(store.load("a"))
        assertNull(store.lastChangedAtMs("a"))
    }
}
