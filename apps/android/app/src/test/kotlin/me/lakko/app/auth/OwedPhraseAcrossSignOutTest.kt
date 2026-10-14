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

    private lateinit var context: Context
    private val account = "AAAAAAAAAAAAAAAAAAAAAA=="

    @Before
    fun setUp() {
        context = RuntimeEnvironment.getApplication()
        AcceptedContactsStore.unbind()
    }

    @After
    fun tearDown() {
        AcceptedContactsStore.unbind()
        me.lakko.app.account.AccountStoreGate.resetForTest()
    }

    @Test
    fun `a sign-out keeps the phrase owed`() {
        // The phrase a device still owes its person is `RecoveryOwedStore`
        // (the fresh identity's phrase; sign-up shows none, ADR-0075).
        val owed = RecoveryOwedStore.onDevice(context)
        val sessions = SessionStore(context)
        sessions.save(PersistedSession(accountIdB64 = account, username = "alice"))
        val auth = AuthRepository(context, sessionStore = sessions)
        owed.markPhraseOwed(account, OwedPhraseOrigin.FreshIdentity)

        auth.logout()

        assertTrue(auth.state.value is AuthState.Anon)
        assertTrue(owed.isPhraseOwed(account))
    }

    @Test
    fun `the source of the sign-out no longer clears the owed marker`() {
        // Pinned in the source as well: the marker has exactly two ways out,
        // the confirmation and the forget-device wipe.
        val source = java.io.File("src/main/kotlin/me/lakko/app/auth/AuthRepository.kt").readText()
        val logout = source.substringAfter("    public fun logout() {").substringBefore("\n    }\n")
        assertFalse("clearPhraseOwed" in logout)
        val confirm = source.substringAfter("    public fun confirmFreshPhraseSaved() {").substringBefore("\n    }\n")
        assertTrue(
            confirm.indexOf("owedStore.clearPhraseOwed(pending.accountIdB64)") in
                0 until confirm.indexOf("landedState = AuthState.Session("),
        )
    }

    @Test
    fun `the phrase of new keys is stored before the server step and owed after it`() {
        val source = java.io.File("src/main/kotlin/me/lakko/app/auth/AuthRepository.kt").readText()
        val mint = source.substringAfter("    public suspend fun mintFreshIdentity(): String {")
            .substringBefore("\n    }\n")
        val stage = mint.indexOf("staged.stage()")
        val reset = mint.indexOf("it.resetAccountKeys(")
        val decide = mint.indexOf("freshIdentityAfterPublish(")
        assertTrue(stage in 0 until reset)
        assertTrue(mint.indexOf("staged.rollback()") > reset)
        assertTrue(decide > reset)
        // Owed in every case: marked before the branch on where the device is.
        val land = source.substringAfter("    private suspend fun landKeyReset(").substringBefore("\n    }\n")
        val owed = land.indexOf("owedStore.markPhraseOwed(accountIdB64, OwedPhraseOrigin.FreshIdentity)")
        assertTrue(owed in 0 until land.indexOf("if (!landHere) {"))
    }

    @Test
    fun `a typed phrase is no longer owed and one the person has not seen still is`() {
        val source = java.io.File("src/main/kotlin/me/lakko/app/auth/AuthRepository.kt").readText()
        // The restore prompt: typed by the person.
        assertTrue(
            "restoreFromPhrase(mnemonicPhrase, phraseHeldByPerson = true)" in
                source.substringAfter("    public suspend fun provideRecoveryPhrase(").substringBefore("\n    /**"),
        )
        // The keys a passkey carried: opened from its vault, not typed.
        val passkey = source.substringAfter("    private suspend fun keysFromPasskeyAssertion(")
            .substringBefore("\n    }\n")
        assertTrue("restoreFromPhrase(phrase, phraseHeldByPerson = false)" in passkey)
        // The email vault: opened by the device, never shown.
        val email = source.substringAfter("    public suspend fun emailRecoverFinish(").substringBefore("\n    }\n")
        assertTrue("recover(phrase, newPassword, named.username, phraseHeldByPerson = false)" in email)
        // Cleared before the landing shows what is owed, on both typed routes.
        for (route in listOf("    private suspend fun restoreFromPhrase(", "    public suspend fun recover(")) {
            val body = source.substringAfter(route).substringBefore("\n    }\n")
            val cleared = body.indexOf("if (phraseHeldByPerson) owedStore.clearPhraseOwed(")
            val landed = body.indexOf("installIdentitySecrets(")
            assertTrue(route, cleared in 0 until landed)
        }
    }

    @Test
    fun `a refusal with its own sentence shows it and any other failure shows the generic one`() {
        val named = AuthClientArgError(
            "stored nothing",
            me.lakko.app.l10n.UiText("restore_identity.lost.error.phrase_not_stored"),
        )
        assertTrue(
            me.lakko.app.ui.auth.resetFailureText(named) ==
                me.lakko.app.l10n.UiText("restore_identity.lost.error.phrase_not_stored"),
        )
        assertTrue(
            me.lakko.app.ui.auth.resetFailureText(IllegalStateException("network")) ==
                me.lakko.app.l10n.UiText("restore_identity.reset.error.generic"),
        )
        // The key is in the catalog the app embeds.
        val catalog = java.io.File("src/main/assets/l10n/source.json").readText()
        assertTrue("restore_identity.lost.error.phrase_not_stored" in catalog)
    }

    @Test
    fun `a sign-out seals the account's stores and its next landing opens them`() {
        val sessions = SessionStore(context)
        sessions.save(PersistedSession(accountIdB64 = account, username = "alice"))
        val auth = AuthRepository(context, sessionStore = sessions)

        auth.logout()

        assertTrue(me.lakko.app.account.AccountStoreGate.isSealed(account))
        // Nothing durable was deleted: the session record is still there.
        assertTrue(sessions.load()?.accountIdB64 == account)
    }
}
