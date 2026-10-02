// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * A guest account (`docs/ux/call-links.md` section 9): the marker, the phrase
 * key, what a guest is shown, and the three orchestrations over fakes. The
 * marker and screen cases are the list of
 * `apps/web/src/auth/guest-account.test.ts`.
 */
class GuestAccountTest {

    private class MemoryStorage : MarkerStorage {
        val rows = LinkedHashMap<String, String>()

        override fun getItem(key: String): String? = rows[key]

        override fun setItem(key: String, value: String) {
            rows[key] = value
        }

        override fun removeItem(key: String) {
            rows.remove(key)
        }
    }

    private object ThrowingStorage : MarkerStorage {
        override fun getItem(key: String): String? = throw IllegalStateException("blocked")

        override fun setItem(key: String, value: String): Unit = throw IllegalStateException("blocked")

        override fun removeItem(key: String): Unit = throw IllegalStateException("blocked")
    }

    // ---- the marker --------------------------------------------------------

    @Test
    fun `an account nobody marked is not a guest`() {
        assertNull(guestAccountState(MemoryStorage(), "acct"))
        assertNull(guestAccountState(null, "acct"))
    }

    @Test
    fun `a marked account is a guest with no username yet`() {
        val storage = MemoryStorage()
        markGuestAccount(storage, "acct")
        assertEquals(GuestAccountState(claimedUsername = null), guestAccountState(storage, "acct"))
    }

    @Test
    fun `the marker is kept per account`() {
        val storage = MemoryStorage()
        markGuestAccount(storage, "one")
        assertNull(guestAccountState(storage, "two"))
    }

    @Test
    fun `the username a half-finished Create account took is remembered`() {
        // A username cannot be changed, so the second attempt must go on with it.
        val storage = MemoryStorage()
        markGuestAccount(storage, "acct")
        noteGuestUsernameClaimed(storage, "acct", "mira")
        assertEquals(GuestAccountState(claimedUsername = "mira"), guestAccountState(storage, "acct"))
    }

    @Test
    fun `a cleared account stops being a guest and leaves no row`() {
        val storage = MemoryStorage()
        markGuestAccount(storage, "acct")
        noteGuestUsernameClaimed(storage, "acct", "mira")
        clearGuestAccount(storage, "acct")
        assertNull(guestAccountState(storage, "acct"))
        assertTrue(storage.rows.isEmpty())
    }

    @Test
    fun `a value this build did not write is not a guest`() {
        // The offer includes deleting the account: never on the strength of
        // unknown bytes.
        val storage = MemoryStorage()
        storage.setItem(guestMarkerKey("acct"), "1")
        assertNull(guestAccountState(storage, "acct"))
    }

    @Test
    fun `the marker never throws when storage refuses`() {
        markGuestAccount(ThrowingStorage, "acct")
        noteGuestUsernameClaimed(ThrowingStorage, "acct", "mira")
        clearGuestAccount(ThrowingStorage, "acct")
        assertNull(guestAccountState(ThrowingStorage, "acct"))
    }

    // ---- the phrase key ----------------------------------------------------

    @Test
    fun `the guest phrase key is never the account's own key in the phrase store`() {
        // The automatic backup reads the phrase by the account id. A guest's
        // phrase under that key would upload a backup for an account the
        // person has not decided to keep.
        assertNotEquals("acct", guestPhraseKey("acct"))
        assertNotEquals(guestPhraseKey("one"), guestPhraseKey("two"))
    }

    // ---- what a guest is shown ---------------------------------------------

    @Test
    fun `an ordinary account is left alone`() {
        assertEquals(GuestScreen.NONE, guestScreen(guest = false, callInProgress = false))
        assertEquals(GuestScreen.NONE, guestScreen(guest = false, callInProgress = true))
    }

    @Test
    fun `a guest is sent back to the call while one runs`() {
        // Never the offer here: one of its two buttons deletes the account.
        assertEquals(GuestScreen.IN_CALL, guestScreen(guest = true, callInProgress = true))
    }

    @Test
    fun `a guest is asked the question once there is no call`() {
        assertEquals(GuestScreen.OFFER, guestScreen(guest = true, callInProgress = false))
    }

    @Test
    fun `a guest never gets the app, and a pending phrase comes first`() {
        assertEquals(SessionSurface.APP, sessionSurface(signupPhrasePending = false, guest = false))
        assertEquals(SessionSurface.GUEST, sessionSurface(signupPhrasePending = false, guest = true))
        assertEquals(SessionSurface.SAVE_SIGNUP_PHRASE, sessionSurface(signupPhrasePending = true, guest = false))
        // The guest who has just chosen to keep the account.
        assertEquals(SessionSurface.SAVE_SIGNUP_PHRASE, sessionSurface(signupPhrasePending = true, guest = true))
    }

    @Test
    fun `the guest flag counts only for the session's own account`() {
        val guest = GuestSession(accountIdB64 = "acct", claimedUsername = null)
        val session = AuthState.Session(SessionAccount(accountIdB64 = "acct", username = ""))
        assertSame(guest, guestSessionOf(session, guest))
        assertNull(guestSessionOf(session, null))
        // A value left from another account.
        assertNull(guestSessionOf(AuthState.Session(SessionAccount("other", "bob")), guest))
        // The instant before the session lands, and the one after it ended.
        assertNull(guestSessionOf(AuthState.Loading, guest))
        assertNull(guestSessionOf(AuthState.Anon(), guest))
    }

    // ---- what became of a guest --------------------------------------------

    private val session = AuthState.Session(SessionAccount(accountIdB64 = "acct", username = "mira"))

    private fun observe(
        wasGuest: Boolean,
        guest: Boolean,
        state: AuthState,
        phrase: Boolean = false,
        call: Boolean = false,
    ) = observeGuestOutcome(wasGuest, guest, state, signupPhrasePending = phrase, callInProgress = call)

    @Test
    fun `a session that was never a guest reports nothing`() {
        assertEquals(GuestWatch(false, GuestOutcome.NONE), observe(wasGuest = false, guest = false, state = session))
        assertEquals(GuestWatch(false, GuestOutcome.NONE), observe(wasGuest = false, guest = false, state = AuthState.Anon()))
    }

    @Test
    fun `a guest session is only remembered`() {
        assertEquals(GuestWatch(true, GuestOutcome.NONE), observe(wasGuest = false, guest = true, state = session))
        assertEquals(GuestWatch(true, GuestOutcome.NONE), observe(wasGuest = true, guest = true, state = session, call = true))
    }

    @Test
    fun `signed out after being a guest means the account was deleted`() {
        assertEquals(GuestWatch(false, GuestOutcome.DELETED), observe(wasGuest = true, guest = false, state = AuthState.Anon()))
    }

    @Test
    fun `a kept account is reported only once its phrase is acknowledged and no call runs`() {
        // The phrase screen is still up: not yet.
        assertEquals(GuestWatch(true, GuestOutcome.NONE), observe(wasGuest = true, guest = false, state = session, phrase = true))
        // A call still runs: not yet.
        assertEquals(GuestWatch(true, GuestOutcome.NONE), observe(wasGuest = true, guest = false, state = session, call = true))
        assertEquals(GuestWatch(false, GuestOutcome.KEPT), observe(wasGuest = true, guest = false, state = session))
    }

    // ---- landing -----------------------------------------------------------

    @Test
    fun `a guest signup keeps the phrase, marks the guest and lands the session, in that order`() {
        val calls = mutableListOf<String>()
        landGuestSignup(
            object : GuestLandingPorts {
                override fun storeGuestPhrase() {
                    calls += "store-phrase"
                }

                override fun markGuest() {
                    calls += "mark-guest"
                }

                override fun landSession() {
                    calls += "land-session"
                }
            },
        )
        // Nothing shows a phrase and nothing registers for push: the three
        // steps are the whole tail.
        assertEquals(listOf("store-phrase", "mark-guest", "land-session"), calls)
    }

    @Test
    fun `a guest signup whose phrase cannot be kept fails before anything is marked or landed`() {
        val calls = mutableListOf<String>()
        try {
            landGuestSignup(
                object : GuestLandingPorts {
                    override fun storeGuestPhrase(): Unit = throw IllegalStateException("keystore")

                    override fun markGuest() {
                        calls += "mark-guest"
                    }

                    override fun landSession() {
                        calls += "land-session"
                    }
                },
            )
            fail("an account whose phrase is neither shown nor kept must not land")
        } catch (expected: IllegalStateException) {
            assertEquals("keystore", expected.message)
        }
        assertTrue(calls.isEmpty())
    }

    // ---- keeping the account -----------------------------------------------

    /** The device and the servers, as far as keeping a guest account goes. */
    private class ClaimWorld(
        var phrase: String? = "abandon ability able",
        var claimed: String? = null,
    ) : GuestClaimPorts {
        val calls = mutableListOf<String>()
        var guest = true
        var phraseRow = true
        var owed: String? = null
        var failClaim: Throwable? = null
        var failPassword: Throwable? = null

        override suspend fun readGuestPhrase(): String? {
            calls += "read-phrase"
            return phrase
        }

        override fun claimedUsername(): String? = claimed

        override suspend fun claimUsername(username: String): String {
            calls += "claim-username:$username"
            failClaim?.let { throw it }
            return username.lowercase()
        }

        override fun noteUsernameClaimed(username: String) {
            calls += "note-username:$username"
            claimed = username
        }

        override suspend fun setPassword(phrase: String, password: ByteArray, username: String) {
            calls += "set-password:$username:${String(password)}:$phrase"
            failPassword?.let { throw it }
        }

        override fun owePhrase(phrase: String) {
            calls += "owe-phrase"
            owed = phrase
        }

        override fun clearGuest() {
            calls += "clear-guest"
            guest = false
        }

        override fun deleteGuestPhrase() {
            calls += "delete-guest-phrase"
            phraseRow = false
        }
    }

    @Test
    fun `keeping a guest account runs phrase, username, password, then the guest ends`() = runTest {
        val world = ClaimWorld()
        keepGuestAccount(world, "  Mira ", "hunter22".toByteArray())
        assertEquals(
            listOf(
                "read-phrase",
                "claim-username:Mira",
                "note-username:mira",
                "set-password:mira:hunter22:abandon ability able",
                "owe-phrase",
                "clear-guest",
                "delete-guest-phrase",
            ),
            world.calls,
        )
        assertFalse(world.guest)
        assertFalse(world.phraseRow)
        // The phrase the person is shown is the guest's own.
        assertEquals("abandon ability able", world.owed)
    }

    @Test
    fun `an account whose phrase is gone cannot be kept and nothing is started`() = runTest {
        val world = ClaimWorld(phrase = null)
        try {
            keepGuestAccount(world, "mira", "hunter22".toByteArray())
            fail("no phrase, no account")
        } catch (expected: GuestPhraseUnavailable) {
            // The typed error the form answers with `No, thanks` alone.
        }
        assertEquals(listOf("read-phrase"), world.calls)
        assertTrue(world.guest)
    }

    @Test
    fun `a refused username leaves the account a guest with no name taken`() = runTest {
        val world = ClaimWorld()
        val taken = AuthClientArgError("taken")
        world.failClaim = taken
        try {
            keepGuestAccount(world, "mira", "hunter22".toByteArray())
            fail("the claim's refusal propagates")
        } catch (expected: AuthClientArgError) {
            assertSame(taken, expected)
        }
        assertEquals(listOf("read-phrase", "claim-username:mira"), world.calls)
        assertNull(world.claimed)
        assertTrue(world.guest)
        assertTrue(world.phraseRow)
    }

    @Test
    fun `the guest marker and the phrase row stay until the password is set`() = runTest {
        val world = ClaimWorld()
        world.failPassword = AuthNetworkError("/password/reset/start", java.io.IOException("offline"))
        try {
            keepGuestAccount(world, "mira", "hunter22".toByteArray())
            fail("the password step's failure propagates")
        } catch (expected: AuthNetworkError) {
            // Still a guest, the phrase still on the device, nothing owed.
        }
        assertTrue(world.guest)
        assertTrue(world.phraseRow)
        assertNull(world.owed)
        assertFalse("clear-guest" in world.calls)
        // The username was taken and recorded before the failure.
        assertEquals("mira", world.claimed)
    }

    @Test
    fun `a retry goes on with the username already taken and never claims a second one`() = runTest {
        val world = ClaimWorld()
        world.failPassword = AuthNetworkError("/password/reset/start", java.io.IOException("offline"))
        try {
            keepGuestAccount(world, "mira", "hunter22".toByteArray())
        } catch (_: AuthNetworkError) {
            // First attempt: the name is taken, the password is not set.
        }
        world.failPassword = null
        world.calls.clear()
        // Whatever the form sends the second time, the taken name is used.
        keepGuestAccount(world, "someone-else", "hunter22".toByteArray())
        assertEquals(
            listOf(
                "read-phrase",
                "set-password:mira:hunter22:abandon ability able",
                "owe-phrase",
                "clear-guest",
                "delete-guest-phrase",
            ),
            world.calls,
        )
    }

    @Test
    fun `a blank username is refused before anything reaches a server`() = runTest {
        val world = ClaimWorld()
        try {
            keepGuestAccount(world, "   ", "hunter22".toByteArray())
            fail("a blank username is a caller error")
        } catch (expected: AuthClientArgError) {
            // Nothing was claimed.
        }
        assertEquals(listOf("read-phrase"), world.calls)
    }

    // ---- deleting the account ----------------------------------------------

    private class DiscardWorld : GuestDiscardPorts {
        val calls = mutableListOf<String>()
        val logged = mutableListOf<String>()
        var failAuth: Throwable? = null
        var failDirectory: Exception? = null

        override suspend fun deleteAtAuth() {
            calls += "auth"
            failAuth?.let { throw it }
        }

        override suspend fun deleteAtDirectory() {
            calls += "directory"
            failDirectory?.let { throw it }
        }

        override fun forgetDevice() {
            calls += "forget-device"
        }

        override fun log(message: String, cause: Throwable) {
            logged += message
        }
    }

    @Test
    fun `deleting a guest account goes auth, directory, then this device`() = runTest {
        val world = DiscardWorld()
        deleteGuestAccount(world)
        assertEquals(listOf("auth", "directory", "forget-device"), world.calls)
        assertTrue(world.logged.isEmpty())
    }

    @Test
    fun `a failed delete at auth throws and leaves everything in place`() = runTest {
        val world = DiscardWorld()
        world.failAuth = AuthNetworkError("/password/reset/start", java.io.IOException("offline"))
        try {
            deleteGuestAccount(world)
            fail("the auth failure propagates")
        } catch (expected: AuthNetworkError) {
            // The person can try again; nothing else was touched.
        }
        assertEquals(listOf("auth"), world.calls)
    }

    @Test
    fun `a failed directory delete is logged and the device is still wiped`() = runTest {
        val world = DiscardWorld()
        world.failDirectory = IllegalStateException("directory down")
        deleteGuestAccount(world)
        assertEquals(listOf("auth", "directory", "forget-device"), world.calls)
        assertEquals(1, world.logged.size)
    }
}
