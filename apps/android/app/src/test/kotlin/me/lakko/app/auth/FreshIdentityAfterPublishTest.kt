// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * ADR-0017 lost-everything fallback: what `mintFreshIdentity` does once its
 * publish, which cannot be taken back, has landed.
 */
class FreshIdentityAfterPublishTest {

    private val a = SessionAccount(accountIdB64 = "account-a", username = "a")
    private val b = SessionAccount(accountIdB64 = "account-b", username = "b")

    private fun after(stillBound: Boolean, now: AuthState) =
        freshIdentityAfterPublish(stillBound = stillBound, now = now, accountIdB64 = a.accountIdB64)

    @Test
    fun `not overtaken - the seed is cached and the phrase shown`() {
        assertEquals(
            FreshIdentityAfterPublish.SHOW_AND_KEEP,
            after(stillBound = true, now = AuthState.NeedsRecoveryPhrase(a)),
        )
    }

    @Test
    fun `signed out during the publish - the phrase is still shown and nothing is written to the device`() {
        // Nobody else can ever show this phrase; a forget-device looks the
        // same from here, so nothing of the account is written back.
        assertEquals(FreshIdentityAfterPublish.SHOW_ONLY, after(stillBound = false, now = AuthState.Anon()))
        assertEquals(
            FreshIdentityAfterPublish.SHOW_ONLY,
            after(stillBound = false, now = AuthState.Anon(cachedUsername = "a")),
        )
    }

    @Test
    fun `signed out and parked on the same account again - the phrase is shown`() {
        assertEquals(
            FreshIdentityAfterPublish.SHOW_AND_KEEP,
            after(stillBound = false, now = AuthState.NeedsRecoveryPhrase(a)),
        )
    }

    @Test
    fun `another account holds the device - the phrase is kept owed and its screen is never put over that session`() {
        for (now in listOf(
            AuthState.Session(b),
            AuthState.NeedsRecoveryPhrase(b),
            AuthState.SaveFreshPhrase(b),
        )) {
            assertEquals(FreshIdentityAfterPublish.KEEP_OWED, after(stillBound = false, now = now))
        }
    }

    @Test
    fun `a session or a waiting phrase screen keeps its screen even for the same account - the phrase is kept owed`() {
        assertEquals(FreshIdentityAfterPublish.KEEP_OWED, after(stillBound = false, now = AuthState.Session(a)))
        assertEquals(FreshIdentityAfterPublish.KEEP_OWED, after(stillBound = false, now = AuthState.SaveFreshPhrase(a)))
        assertEquals(FreshIdentityAfterPublish.KEEP_OWED, after(stillBound = false, now = AuthState.Loading))
    }
}
