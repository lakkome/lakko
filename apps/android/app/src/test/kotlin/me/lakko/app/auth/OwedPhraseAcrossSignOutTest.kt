// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context
import me.lakko.app.contacts.AcceptedContactsStore
import org.junit.After
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * A recovery phrase the person has not confirmed stays owed: a sign-out keeps
 * the marker (and the cached phrase), so the account's next sign-in on this
 * device shows it again. Forget device is what removes it.
 */
@RunWith(RobolectricTestRunner::class)
class OwedPhraseAcrossSignOutTest {

    private class MemoryStorage : MarkerStorage {
        val rows = HashMap<String, String>()

        override fun getItem(key: String): String? = rows[key]

        override fun setItem(key: String, value: String) {
            rows[key] = value
        }

        override fun removeItem(key: String) {
            rows.remove(key)
        }
    }

    private lateinit var context: Context
    private val account = "AAAAAAAAAAAAAAAAAAAAAA=="

    @Before
    fun setUp() {
        context = RuntimeEnvironment.getApplication()
        AcceptedContactsStore.unbind()
    }

    @After
    fun tearDown() = AcceptedContactsStore.unbind()

    @Test
    fun `a sign-out keeps the phrase owed`() {
        val markers = MemoryStorage()
        val sessions = SessionStore(context)
        sessions.save(PersistedSession(accountIdB64 = account, username = "alice"))
        val auth = AuthRepository(context, sessionStore = sessions, phraseOwedMarkers = markers)
        markSignupPhraseOwed(markers, account)

        auth.logout()

        assertTrue(auth.state.value is AuthState.Anon)
        assertTrue(isSignupPhraseOwed(markers, account))
    }

    @Test
    fun `the source of the sign-out no longer clears the owed marker`() {
        // Pinned in the source as well: the marker has exactly two ways out,
        // the confirmation and the forget-device wipe.
        val source = java.io.File("src/main/kotlin/me/lakko/app/auth/AuthRepository.kt").readText()
        val logout = source.substringAfter("    public fun logout() {").substringBefore("\n    }\n")
        assertFalse("clearSignupPhraseOwed" in logout)
        val confirm = source.substringAfter("    public fun confirmFreshPhraseSaved() {").substringBefore("\n    }\n")
        assertTrue(
            confirm.indexOf("clearSignupPhraseOwed(phraseOwedMarkers, pending.accountIdB64)") in
                0 until confirm.indexOf("landedState = AuthState.Session("),
        )
    }

    @Test
    fun `the phrase of a fresh identity is stored before its publish and owed after it`() {
        val source = java.io.File("src/main/kotlin/me/lakko/app/auth/AuthRepository.kt").readText()
        val mint = source.substringAfter("    public suspend fun mintFreshIdentity(): String {")
            .substringBefore("\n    }\n")
        val stage = mint.indexOf("staged.stage()")
        val publish = mint.indexOf("publishIdentityFor(")
        val owed = mint.indexOf("markSignupPhraseOwed(phraseOwedMarkers, accountIdB64)")
        val decide = mint.indexOf("freshIdentityAfterPublish(")
        assertTrue(stage in 0 until publish)
        assertTrue(mint.indexOf("staged.rollback()") > publish)
        // Owed in every case: marked before the branch on where the device is.
        assertTrue(owed in (publish + 1) until decide)
    }
}
