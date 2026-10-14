// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.coroutines.launch
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

        override fun get(key: String): String? = rows[key]

        override fun set(key: String, value: String) {
            rows[key] = value
        }

        override fun remove(key: String) {
            rows.remove(key)
        }
    }

    private object ThrowingStorage : MarkerStorage {
        override fun get(key: String): String? = throw IllegalStateException("blocked")

        override fun set(key: String, value: String): Unit = throw IllegalStateException("blocked")

        override fun remove(key: String): Unit = throw IllegalStateException("blocked")
    }

    private val t0 = 1_760_000_000_000L

    // ---- the marker --------------------------------------------------------

    @Test
    fun `an account nobody marked is not a guest`() {
        assertNull(guestAccountState(MemoryStorage(), "acct"))
        assertNull(guestAccountState(null, "acct"))
    }

    @Test
    fun `a marked account is a guest with no username yet`() {
        val storage = MemoryStorage()
        markGuestAccount(storage, "acct", t0)
        assertEquals(GuestAccountState(markedAtMs = t0, claimedUsername = null), guestAccountState(storage, "acct"))
        // The stored form, which the wipe test and the other clients read as
        // the same thing.
        assertEquals("guest@$t0", storage.rows[guestMarkerKey("acct")])
    }

    @Test
    fun `the marker is kept per account`() {
        val storage = MemoryStorage()
        markGuestAccount(storage, "one", t0)
        assertNull(guestAccountState(storage, "two"))
    }

    @Test
    fun `the username a half-finished Create account took is remembered`() {
        // A username cannot be changed, so the second attempt must go on with it.
        val storage = MemoryStorage()
        markGuestAccount(storage, "acct", t0)
        noteGuestUsernameClaimed(storage, "acct", "mira")
        // The age is kept: taking a name does not buy the account another day.
        assertEquals(GuestAccountState(markedAtMs = t0, claimedUsername = "mira"), guestAccountState(storage, "acct"))
        assertEquals("named@$t0:mira", storage.rows[guestMarkerKey("acct")])
    }

    @Test
    fun `noting a name does not turn an ordinary account into a guest`() {
        val storage = MemoryStorage()
        noteGuestUsernameClaimed(storage, "acct", "mira")
        assertNull(guestAccountState(storage, "acct"))
        assertTrue(storage.rows.isEmpty())
    }

    @Test
    fun `a cleared account stops being a guest and leaves no row`() {
        val storage = MemoryStorage()
        markGuestAccount(storage, "acct", t0)
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
        val refused = listOf(
            "1", "guest", "guest@", "guest@soon", "guest@-5", "guest@+5", "guest@12345678901234567",
            "named@12", "named@x:mira", "named:mira",
        )
        for (value in refused) {
            storage.set(guestMarkerKey("acct"), value)
            assertNull(value, guestAccountState(storage, "acct"))
        }
    }

    @Test
    fun `the marker never throws when storage refuses`() {
        markGuestAccount(ThrowingStorage, "acct", t0)
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
        val guest = GuestSession(accountIdB64 = "acct", markedAtMs = t0, claimedUsername = null)
        assertEquals(GuestAccountState(markedAtMs = t0, claimedUsername = null), guest.account)
        val session = AuthState.Session(SessionAccount(accountIdB64 = "acct", username = ""))
        assertSame(guest, guestSessionOf(session, guest))
        assertNull(guestSessionOf(session, null))
        // A value left from another account.
        assertNull(guestSessionOf(AuthState.Session(SessionAccount("other", "bob")), guest))
        // The instant before the session lands, and the one after it ended.
        assertNull(guestSessionOf(AuthState.Loading, guest))
        assertNull(guestSessionOf(AuthState.Anon(), guest))
    }

    // ---- a guest account lives less than a day on a device -----------------

    private val young = GuestAccountState(markedAtMs = t0, claimedUsername = null)
    private val named = GuestAccountState(markedAtMs = t0, claimedUsername = "mira")
    private val later = t0 + GUEST_LIFETIME_MS

    @Test
    fun `a guest is not expired while younger than the lifetime`() {
        assertFalse(guestExpired(young, t0))
        assertFalse(guestExpired(young, t0 + GUEST_LIFETIME_MS - 1))
    }

    @Test
    fun `a guest is expired from the lifetime on`() {
        assertTrue(guestExpired(young, t0 + GUEST_LIFETIME_MS))
        assertTrue(guestExpired(young, t0 + 10 * GUEST_LIFETIME_MS))
    }

    @Test
    fun `the device stops before the servers' own day, never after it`() {
        // The servers delete an unkept account 24 hours after it was made. The
        // device must stop offering Create account before that.
        assertTrue(GUEST_LIFETIME_MS < 24L * 60 * 60 * 1000)
        assertEquals(23L * 60 * 60 * 1000, GUEST_LIFETIME_MS)
    }

    @Test
    fun `a clock set back is not read as an old account`() {
        assertFalse(guestExpired(young, t0 - 5 * GUEST_LIFETIME_MS))
    }

    @Test
    fun `an ordinary account is left to the ordinary restore`() {
        assertEquals(GuestRestoreAction.NONE, guestRestoreAction(null, restorable = true, nowMs = later))
        assertEquals(GuestRestoreAction.NONE, guestRestoreAction(null, restorable = false, nowMs = later))
    }

    @Test
    fun `a young guest is restored`() {
        assertEquals(GuestRestoreAction.NONE, guestRestoreAction(young, restorable = true, nowMs = t0 + 1))
    }

    @Test
    fun `a guest past its lifetime is deleted`() {
        assertEquals(GuestRestoreAction.DISCARD, guestRestoreAction(young, restorable = true, nowMs = later))
    }

    @Test
    fun `a guest whose session cannot be restored is wiped, whatever its age`() {
        // No password exists that anyone could type: a signed-out guest
        // account is unreachable, and its phrase must not stay on the device.
        assertEquals(GuestRestoreAction.WIPE, guestRestoreAction(young, restorable = false, nowMs = t0 + 1))
        assertEquals(GuestRestoreAction.WIPE, guestRestoreAction(young, restorable = false, nowMs = later))
    }

    @Test
    fun `the auth service decides for an unnamed guest`() {
        assertEquals(
            listOf(
                GuestDeleteStep(GuestDeleteService.AUTH, required = true),
                GuestDeleteStep(GuestDeleteService.DIRECTORY, required = false),
            ),
            guestDeletePlan(young),
        )
    }

    @Test
    fun `a taken username is freed first, and that is required`() {
        // The directory never sweeps an entry that has a username: skipping
        // this delete would leave the name taken for good.
        assertEquals(
            listOf(
                // The half-claimed login handle goes first: the auth service
                // then deletes the account on its session alone.
                GuestDeleteStep(GuestDeleteService.LOGIN_HANDLE, required = true),
                GuestDeleteStep(GuestDeleteService.DIRECTORY, required = true),
                GuestDeleteStep(GuestDeleteService.AUTH, required = true),
            ),
            guestDeletePlan(named),
        )
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
        var expired = false

        override suspend fun expireIfDue(): Boolean {
            calls += "expire-check"
            return expired
        }

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

        override suspend fun landAsAccount(phrase: String) {
            calls += "land-as-account"
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
                "expire-check",
                "read-phrase",
                "claim-username:Mira",
                "note-username:mira",
                "set-password:mira:hunter22:abandon ability able",
                "land-as-account",
                "clear-guest",
                "delete-guest-phrase",
            ),
            world.calls,
        )
        assertFalse(world.guest)
        assertFalse(world.phraseRow)
        // The phrase the account keeps (and is never shown here) is the guest's own.
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
        assertEquals(listOf("expire-check", "read-phrase"), world.calls)
        assertTrue(world.guest)
    }

    @Test
    fun `an account past its lifetime is not kept and nothing else is started`() = runTest {
        val world = ClaimWorld()
        world.expired = true
        try {
            keepGuestAccount(world, "mira", "hunter22".toByteArray())
            fail("an expired guest account is deleted, not kept")
        } catch (expected: GuestAccountExpired) {
            // The screen that asked is going away; it says nothing.
        }
        // Not even the phrase is read.
        assertEquals(listOf("expire-check"), world.calls)
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
        assertEquals(listOf("expire-check", "read-phrase", "claim-username:mira"), world.calls)
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
                "expire-check",
                "read-phrase",
                "set-password:mira:hunter22:abandon ability able",
                "land-as-account",
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
        assertEquals(listOf("expire-check", "read-phrase"), world.calls)
    }

    // ---- deleting the account ----------------------------------------------

    /** The servers and the device, as far as removing a guest account goes. */
    private class DiscardWorld : GuestDiscardPorts, GuestRestorePorts {
        val calls = mutableListOf<String>()
        val logged = mutableListOf<String>()
        var failAuth: Exception? = null
        var failDirectory: Exception? = null
        var hangAuth = false

        override suspend fun releaseLoginHandle() {
            calls += "handle"
        }

        override suspend fun deleteAtAuth() {
            calls += "auth"
            if (hangAuth) kotlinx.coroutines.awaitCancellation()
            failAuth?.let { throw it }
        }

        override suspend fun deleteAtDirectory() {
            calls += "directory"
            failDirectory?.let { throw it }
        }

        override fun forgetDevice() {
            calls += "forget-device"
        }

        override fun wipeDevice() {
            calls += "wipe-device"
        }

        override fun landSignedOut() {
            calls += "land-signed-out"
        }

        override fun log(message: String, cause: Throwable?) {
            logged += message
        }
    }

    private fun offline() = AuthNetworkError("/account", java.io.IOException("offline"))

    @Test
    fun `No, thanks for an unnamed guest goes auth, directory, then this device`() = runTest {
        val world = DiscardWorld()
        deleteGuestAccount(world, young)
        assertEquals(listOf("auth", "directory", "forget-device"), world.calls)
        assertTrue(world.logged.isEmpty())
    }

    @Test
    fun `a failed delete at auth throws and leaves everything in place`() = runTest {
        val world = DiscardWorld()
        world.failAuth = offline()
        try {
            deleteGuestAccount(world, young)
            fail("the auth failure propagates")
        } catch (expected: AuthNetworkError) {
            // The person can try again; nothing else was touched.
        }
        assertEquals(listOf("auth"), world.calls)
    }

    @Test
    fun `a failed directory delete of an unnamed guest is logged and the device is still wiped`() = runTest {
        val world = DiscardWorld()
        world.failDirectory = IllegalStateException("directory down")
        deleteGuestAccount(world, young)
        assertEquals(listOf("auth", "directory", "forget-device"), world.calls)
        assertEquals(1, world.logged.size)
    }

    @Test
    fun `No, thanks with a claimed name frees the name at the directory first`() = runTest {
        val world = DiscardWorld()
        deleteGuestAccount(world, named)
        assertEquals(listOf("handle", "directory", "auth", "forget-device"), world.calls)
    }

    @Test
    fun `with a claimed name a failed directory delete stops everything and wipes nothing`() = runTest {
        val world = DiscardWorld()
        world.failDirectory = IllegalStateException("directory down")
        try {
            deleteGuestAccount(world, named)
            fail("the name must be freed before the account goes")
        } catch (expected: IllegalStateException) {
            assertEquals("directory down", expected.message)
        }
        // The auth service was not asked and nothing local was removed: the
        // account still exists, so the retry can free the name.
        assertEquals(listOf("handle", "directory"), world.calls)
    }

    @Test
    fun `with a claimed name a failed auth delete also leaves the device alone`() = runTest {
        val world = DiscardWorld()
        world.failAuth = offline()
        try {
            deleteGuestAccount(world, named)
            fail("both deletes are required for a named guest")
        } catch (expected: AuthNetworkError) {
            // Both deletes are idempotent, so the retry completes.
        }
        assertEquals(listOf("handle", "directory", "auth"), world.calls)
    }

    // ---- an expired guest (not strict) --------------------------------------

    @Test
    fun `a guest still young enough is not expired and nothing is touched`() = runTest {
        val world = DiscardWorld()
        assertFalse(expireGuestWhenDue(world, young, nowMs = later - 1))
        assertFalse(expireGuestWhenDue(world, null, nowMs = later))
        assertTrue(world.calls.isEmpty())
    }

    @Test
    fun `an expired guest is deleted on the servers and wiped`() = runTest {
        val world = DiscardWorld()
        assertTrue(expireGuestWhenDue(world, young, nowMs = later))
        assertEquals(listOf("auth", "directory", "forget-device"), world.calls)
    }

    @Test
    fun `expiry swallows every server failure and still wipes`() = runTest {
        for (guest in listOf(young, named)) {
            val world = DiscardWorld()
            world.failAuth = offline()
            world.failDirectory = IllegalStateException("directory down")
            assertTrue(expireGuestWhenDue(world, guest, nowMs = later))
            // Every step was tried, in the plan's order (a named guest's
            // login handle is released first), and the wipe still ran.
            assertEquals(guestDeletePlan(guest).size + 1, world.calls.size)
            assertEquals("forget-device", world.calls.last())
            assertEquals(2, world.logged.size)
        }
    }

    @Test
    fun `expiry does not wait on a server that never answers`() = runTest {
        val world = DiscardWorld()
        world.hangAuth = true
        assertTrue(expireGuestWhenDue(world, young, nowMs = later))
        assertEquals(listOf("auth", "directory", "forget-device"), world.calls)
        assertEquals(1, world.logged.size)
        assertEquals(GUEST_BEST_EFFORT_DELETE_TIMEOUT_MS, testScheduler.currentTime)
    }

    @Test
    fun `No, thanks waits for the server however long it takes`() = runTest {
        // Strict: the person pressed a button and gets a real answer; the
        // bound above is only for deletes nobody asked for.
        val world = DiscardWorld()
        world.hangAuth = true
        val job = launch { deleteGuestAccount(world, young) }
        testScheduler.advanceTimeBy(10 * GUEST_BEST_EFFORT_DELETE_TIMEOUT_MS)
        assertTrue(job.isActive)
        assertEquals(listOf("auth"), world.calls)
        job.cancel()
    }

    // ---- at launch ---------------------------------------------------------

    @Test
    fun `a launch with no guest leaves the ordinary restore alone`() = runTest {
        val world = DiscardWorld()
        assertFalse(settleGuestAtRestore(world, guest = null, restorable = false, nowMs = later))
        assertFalse(settleGuestAtRestore(world, guest = null, restorable = true, nowMs = later))
        assertFalse(settleGuestAtRestore(world, guest = young, restorable = true, nowMs = t0 + 1))
        assertTrue(world.calls.isEmpty())
    }

    @Test
    fun `a guest whose session cannot be restored is wiped and lands signed out`() = runTest {
        // The session token expired, or a store is unreadable: there is no
        // token to delete with, and a guest is never left as a cached,
        // signed-out account.
        val world = DiscardWorld()
        assertTrue(settleGuestAtRestore(world, guest = young, restorable = false, nowMs = t0 + 1))
        assertEquals(listOf("wipe-device", "land-signed-out"), world.calls)
    }

    @Test
    fun `an expired guest is deleted on the servers, then wiped, at launch`() = runTest {
        val world = DiscardWorld()
        assertTrue(settleGuestAtRestore(world, guest = young, restorable = true, nowMs = later))
        assertEquals(listOf("auth", "directory", "wipe-device", "land-signed-out"), world.calls)
    }

    @Test
    fun `an expired guest with a claimed name frees the name first at launch`() = runTest {
        val world = DiscardWorld()
        assertTrue(settleGuestAtRestore(world, guest = named, restorable = true, nowMs = later))
        assertEquals(listOf("handle", "directory", "auth", "wipe-device", "land-signed-out"), world.calls)
    }

    @Test
    fun `a launch still wipes an expired guest when the servers cannot be reached`() = runTest {
        val world = DiscardWorld()
        world.failAuth = offline()
        world.failDirectory = IllegalStateException("directory down")
        assertTrue(settleGuestAtRestore(world, guest = young, restorable = true, nowMs = later))
        assertEquals(listOf("auth", "directory", "wipe-device", "land-signed-out"), world.calls)
        assertEquals(2, world.logged.size)
    }
}
