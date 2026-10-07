// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.AuthNetworkError
import me.lakko.app.auth.FreshAuthRequired
import me.lakko.app.l10n.UiText
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException

/** UX 15.1 + 17.2: the reset warning's state decisions. */
class ResetWarningRulesTest {

    private fun view(
        proofLive: Boolean,
        password: String = "",
        usernameKnown: Boolean = true,
        username: String = "",
        hasUsablePasskey: Boolean = false,
    ) = proofBlockView(
        proofLive = proofLive,
        hasUsablePasskey = hasUsablePasskey,
        usernameKnown = usernameKnown,
        state = ProofBlockState(password = password, username = username),
        nowMs = 0L,
    )

    @Test
    fun `with a live proof the word alone enables the button`() {
        assertTrue(resetButtonEnabled("RESET", view(proofLive = true)))
        assertTrue(resetButtonEnabled(" RESET ", view(proofLive = true)))
        assertFalse(resetButtonEnabled("reset", view(proofLive = true)))
        assertFalse(resetButtonEnabled("", view(proofLive = true)))
    }

    @Test
    fun `without a live proof the button waits for both the password and the word`() {
        assertFalse(resetButtonEnabled("RESET", view(proofLive = false, password = "")))
        assertFalse(resetButtonEnabled("", view(proofLive = false, password = "hunter2")))
        assertTrue(resetButtonEnabled("RESET", view(proofLive = false, password = "hunter2")))
    }

    @Test
    fun `with an unknown username the button waits for the name too`() {
        assertFalse(resetButtonEnabled("RESET", view(false, "hunter2", usernameKnown = false, username = "")))
        assertFalse(resetButtonEnabled("RESET", view(false, "hunter2", usernameKnown = false, username = "  ")))
        assertFalse(resetButtonEnabled("RESET", view(false, "", usernameKnown = false, username = "alice")))
        assertTrue(resetButtonEnabled("RESET", view(false, "hunter2", usernameKnown = false, username = "alice")))
        // A live proof never waits for a typed name.
        assertTrue(resetButtonEnabled("RESET", view(true, usernameKnown = false)))
    }

    @Test
    fun `a missing or refused proof is the proof-required failure`() {
        assertSame(ResetFailure.ProofRequired, resetFailureOf(FreshAuthRequired()))
        assertSame(
            ResetFailure.ProofRequired,
            resetFailureOf(AuthHttpError(403, "/account/keys/reset/finish", "{}", code = "fresh-auth-required")),
        )
    }

    @Test
    fun `a limit keeps the sign-in screen's line and offline keeps the reset's`() {
        val limit = resetFailureOf(
            AuthHttpError(429, "/account/keys/reset/start", "{}", code = "too-many-attempts", retryAfterSecs = 300),
        ) as ResetFailure.Other
        assertEquals("common.error.too_many_attempts_minutes", limit.text.key)
        val offline = resetFailureOf(AuthNetworkError("/account/keys/reset/start", IOException("down"))) as ResetFailure.Other
        assertEquals("restore_identity.reset.error.offline", offline.text.key)
        val other = resetFailureOf(AuthHttpError(403, "/account/keys/reset/start", "{}", code = "nope")) as ResetFailure.Other
        assertEquals("restore_identity.reset.error.generic", other.text.key)
    }

    @Test
    fun `proof required leaves the screen idle with no error styling`() {
        val before = RestoreIdentityUiState(busy = true, error = UiText("stale"))
        val after = afterResetFailure(before, ResetFailure.ProofRequired)
        assertFalse(after.busy)
        assertNull(after.error)
    }

    @Test
    fun `any other failure puts its sentence on the form`() {
        val before = RestoreIdentityUiState(busy = true)
        val after = afterResetFailure(before, ResetFailure.Other(UiText("restore_identity.reset.error.offline")))
        assertFalse(after.busy)
        assertEquals("restore_identity.reset.error.offline", after.error?.key)
    }

    @Test
    fun `the screen state never prints the typed phrase`() {
        assertFalse(RestoreIdentityUiState(phrase = "abandon ability").toString().contains("abandon"))
    }

    // ------------------------------------------------------------ the panel

    @Test
    fun `a proof in the last ten minutes shows 15-1 with no block`() {
        for (passkey in listOf(true, false)) {
            for (totp in listOf(true, false)) {
                assertEquals(
                    ResetPanel.NoBlock,
                    resetPanel(
                        proofLive = true,
                        hasUsablePasskey = passkey,
                        authenticatorLanding = totp,
                        knowsPassword = false,
                        saidNoPassword = true,
                    ),
                )
            }
        }
    }

    @Test
    fun `no proof with a usable passkey shows the block and never the cannot panel`() {
        assertEquals(ResetPanel.Block, resetPanel(false, true, authenticatorLanding = true, knowsPassword = false, saidNoPassword = false))
        assertEquals(ResetPanel.Block, resetPanel(false, true, authenticatorLanding = false, knowsPassword = false, saidNoPassword = true))
    }

    @Test
    fun `no proof and no passkey shows the block with the no-password link`() {
        val panel = resetPanel(false, false, authenticatorLanding = false, knowsPassword = false, saidNoPassword = false)
        assertEquals(ResetPanel.Block, panel)
        assertTrue(resetShowsNoPasswordLink(panel, hasUsablePasskey = false))
        assertFalse(resetShowsNoPasswordLink(ResetPanel.Block, hasUsablePasskey = true))
        assertFalse(resetShowsNoPasswordLink(ResetPanel.NoBlock, hasUsablePasskey = false))
        assertFalse(resetShowsNoPasswordLink(ResetPanel.Cannot, hasUsablePasskey = false))
    }

    @Test
    fun `an authenticator sign-in with no passkey opens on the cannot panel first`() {
        assertEquals(
            ResetPanel.Cannot,
            resetPanel(false, false, authenticatorLanding = true, knowsPassword = false, saidNoPassword = false),
        )
        // `I know my password` switches to the block.
        assertEquals(
            ResetPanel.Block,
            resetPanel(false, false, authenticatorLanding = true, knowsPassword = true, saidNoPassword = false),
        )
    }

    @Test
    fun `I do not know my password shows the cannot panel whatever the landing`() {
        assertEquals(
            ResetPanel.Cannot,
            resetPanel(false, false, authenticatorLanding = false, knowsPassword = false, saidNoPassword = true),
        )
    }

    @Test
    fun `the cannot panel lists its three ways out in order`() {
        assertEquals(
            listOf(
                "proof.reset.cannot.option_device",
                "proof.reset.cannot.option_phrase",
                "proof.reset.cannot.option_passkey",
            ),
            RESET_CANNOT_OPTION_KEYS,
        )
    }
}
