// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import me.lakko.app.auth.AuthClientArgError
import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.AuthNetworkError
import me.lakko.app.auth.FreshAuthOtherAccount
import me.lakko.app.auth.FreshAuthRequired
import me.lakko.app.auth.FreshAuthWrongPassword
import me.lakko.app.auth.PasskeyCancelled
import me.lakko.app.webauthn.WebauthnHttpError
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException

/** `docs/ux/identity-recovery.md` section 17.1: the proof block's shared
 *  states, and each surface's "what is shown". */
class ProofBlockRulesTest {

    private fun view(
        proofLive: Boolean = false,
        hasUsablePasskey: Boolean = false,
        usernameKnown: Boolean = true,
        state: ProofBlockState = ProofBlockState(),
        nowMs: Long = 1_000L,
    ) = proofBlockView(proofLive, hasUsablePasskey, usernameKnown, state, nowMs)

    // --------------------------------------------------------- shared states

    @Test
    fun `idle with a live proof draws no block and does not hold the main button`() {
        val v = view(proofLive = true, hasUsablePasskey = true, state = ProofBlockState(password = "x"))
        assertFalse(v.visible)
        assertFalse(v.showPasswordField)
        assertFalse(v.showPasskeyButton)
        assertFalse(v.showPasskeyNotice)
        assertTrue(v.mainEnabled)
    }

    @Test
    fun `idle with no proof draws the field and holds the main button while it is empty`() {
        val empty = view()
        assertTrue(empty.visible)
        assertTrue(empty.showPasswordField)
        assertTrue(empty.fieldsEnabled)
        assertFalse(empty.mainEnabled)
        assertTrue(view(state = ProofBlockState(password = "hunter2")).mainEnabled)
    }

    @Test
    fun `the passkey button is absent without a usable passkey, never disabled for ever`() {
        assertFalse(view(hasUsablePasskey = false).showPasskeyButton)
        val with = view(hasUsablePasskey = true)
        assertTrue(with.showPasskeyButton)
        assertTrue(with.passkeyButtonEnabled)
    }

    @Test
    fun `the username field shows only with the password field and only when the name is unknown`() {
        assertFalse(view(usernameKnown = true).showUsernameField)
        val unknown = view(usernameKnown = false, state = ProofBlockState(password = "hunter2"))
        assertTrue(unknown.showUsernameField)
        assertFalse(unknown.mainEnabled)
        assertFalse(view(usernameKnown = false, state = ProofBlockState(password = "hunter2", username = "  ")).mainEnabled)
        assertTrue(view(usernameKnown = false, state = ProofBlockState(password = "hunter2", username = "alice")).mainEnabled)
        // The passkey notice asks for no name.
        assertFalse(
            view(
                hasUsablePasskey = true,
                usernameKnown = false,
                state = ProofBlockState(mode = ProofMode.PasskeyNotice),
            ).showUsernameField,
        )
    }

    @Test
    fun `checking disables the field and both buttons and shows the working word`() {
        val v = view(
            hasUsablePasskey = true,
            state = ProofBlockState(phase = ProofPhase.Checking, password = "hunter2"),
        )
        assertTrue(v.working)
        assertFalse(v.fieldsEnabled)
        assertFalse(v.passkeyButtonEnabled)
        assertFalse(v.mainEnabled)
    }

    @Test
    fun `the passkey sheet up disables everything on the block`() {
        val v = view(hasUsablePasskey = true, state = ProofBlockState(phase = ProofPhase.PasskeySheet))
        assertTrue(v.working)
        assertFalse(v.fieldsEnabled)
        assertFalse(v.passkeyButtonEnabled)
        assertFalse(v.mainEnabled)
    }

    @Test
    fun `rate limited holds the field and the main button until zero and keeps the passkey route`() {
        val limited = ProofBlockState(password = "hunter2", waitUntilMs = 61_000L)
        val during = view(hasUsablePasskey = true, state = limited, nowMs = 1_000L)
        assertEquals(60_000L, during.waitLeftMs)
        assertFalse(during.fieldsEnabled)
        assertFalse(during.mainEnabled)
        assertTrue(during.passkeyButtonEnabled)
        val after = view(hasUsablePasskey = true, state = limited, nowMs = 61_000L)
        assertEquals(0L, after.waitLeftMs)
        assertTrue(after.fieldsEnabled)
        assertTrue(after.mainEnabled)
    }

    @Test
    fun `the passkey notice mode shows no field and keeps the main button usable under a limit`() {
        val state = ProofBlockState(mode = ProofMode.PasskeyNotice, waitUntilMs = 61_000L)
        val v = view(hasUsablePasskey = true, state = state)
        assertFalse(v.showPasswordField)
        assertFalse(v.showPasskeyButton)
        assertTrue(v.showPasskeyNotice)
        assertTrue(v.showUsePasswordInstead)
        // The limit is on the password budget; the gesture is not held by it.
        assertTrue(v.mainEnabled)
    }

    @Test
    fun `a passkey notice without a usable passkey falls back to the field`() {
        val v = view(hasUsablePasskey = false, state = ProofBlockState(mode = ProofMode.PasskeyNotice))
        assertTrue(v.showPasswordField)
        assertFalse(v.showPasskeyNotice)
        assertFalse(v.showUsePasswordInstead)
        assertFalse(v.mainEnabled)
    }

    @Test
    fun `the wait is clamped at zero`() {
        assertEquals(0L, waitLeftMs(null, 5L))
        assertEquals(0L, waitLeftMs(1_000L, 2_000L))
        assertEquals(500L, waitLeftMs(1_500L, 1_000L))
    }

    @Test
    fun `the block state never prints the typed password`() {
        assertFalse(ProofBlockState(password = "hunter2").toString().contains("hunter2"))
    }

    // -------------------------------------------------------------- failures

    @Test
    fun `failures are classified by what happened, not by where`() {
        assertEquals(ProofFailure.WrongPassword, proofFailureOf(FreshAuthWrongPassword(), viaPasskey = false))
        assertEquals(ProofFailure.Cancelled, proofFailureOf(PasskeyCancelled(), viaPasskey = true))
        assertEquals(ProofFailure.OtherAccount, proofFailureOf(FreshAuthOtherAccount(), viaPasskey = true))
        assertEquals(ProofFailure.Offline, proofFailureOf(AuthNetworkError("/login/start", IOException("x")), false))
        assertEquals(ProofFailure.Offline, proofFailureOf(IOException("x"), viaPasskey = true))
        assertEquals(
            ProofFailure.RateLimited(900L),
            proofFailureOf(AuthHttpError(429, "/login/start", "{}", "too-many-attempts", 900L), false),
        )
        assertEquals(
            ProofFailure.RateLimited(null),
            proofFailureOf(AuthHttpError(429, "/login/start", "{}", "too-many-attempts"), false),
        )
        assertEquals(ProofFailure.PasskeyFailed, proofFailureOf(AuthClientArgError("ceremony failed"), viaPasskey = true))
        assertEquals(ProofFailure.PasskeyFailed, proofFailureOf(RuntimeException("x"), viaPasskey = true))
        assertEquals(ProofFailure.Generic, proofFailureOf(AuthHttpError(503, "/login/start", ""), viaPasskey = false))
    }

    @Test
    fun `a wrong password says what did not happen, per surface, and keeps the field's contents`() {
        val expected = mapOf(
            ProofSurface.Reset to "restore_identity.reset.error.wrong_password",
            ProofSurface.DeleteAccount to "proof.delete.wrong_password",
            ProofSurface.AddDeviceRequest to "proof.add_device.request.wrong_password",
            ProofSurface.AddDeviceScan to "proof.add_device.scan.wrong_password",
            ProofSurface.AddPasskey to "restore_identity.reset.error.wrong_password",
            ProofSurface.ChangePassword to "restore_identity.reset.error.wrong_password",
        )
        for ((surface, key) in expected) {
            val before = ProofBlockState(phase = ProofPhase.Checking, password = "typo")
            val after = afterProofFailure(before, ProofFailure.WrongPassword, surface, nowMs = 0L)
            assertEquals(ProofPhase.Idle, after.phase)
            assertEquals(key, (after.line as ProofLine.Error).text.key)
            assertEquals("typo", after.password)
        }
    }

    @Test
    fun `a cancelled gesture is one plain line, with no error styling`() {
        val expected = mapOf(
            ProofSurface.Reset to "proof.reset.cancelled",
            ProofSurface.DeleteAccount to "settings.account.delete.error.cancelled",
            ProofSurface.AddDeviceRequest to "proof.add_device.request.cancelled",
            ProofSurface.AddDeviceScan to "proof.add_device.scan.cancelled",
            ProofSurface.AddPasskey to "proof.add_passkey.cancelled",
            ProofSurface.ChangePassword to "proof.change_password.cancelled",
        )
        assertEquals(ProofSurface.entries.toSet(), expected.keys)
        for ((surface, key) in expected) {
            val after = afterProofFailure(
                ProofBlockState(phase = ProofPhase.PasskeySheet),
                ProofFailure.Cancelled,
                surface,
                nowMs = 0L,
            )
            assertEquals(ProofPhase.Idle, after.phase)
            assertEquals(key, (after.line as ProofLine.Plain).text.key)
        }
    }

    @Test
    fun `another account's passkey, a failed check and offline each have their line`() {
        fun key(f: ProofFailure) =
            (afterProofFailure(ProofBlockState(password = "kept"), f, ProofSurface.Reset, 0L).line as ProofLine.Error).text.key
        assertEquals("proof.passkey.other_account", key(ProofFailure.OtherAccount))
        assertEquals("proof.passkey.failed", key(ProofFailure.PasskeyFailed))
        assertEquals("common.error.offline", key(ProofFailure.Offline))
        assertEquals("restore_identity.reset.error.generic", key(ProofFailure.Generic))
        // Offline keeps the field's contents.
        assertEquals(
            "kept",
            afterProofFailure(ProofBlockState(password = "kept"), ProofFailure.Offline, ProofSurface.Reset, 0L).password,
        )
    }

    @Test
    fun `a limit with a retry time starts the countdown and one without says too many attempts`() {
        val timed = afterProofFailure(ProofBlockState(), ProofFailure.RateLimited(900L), ProofSurface.DeleteAccount, nowMs = 1_000L)
        assertEquals(901_000L, timed.waitUntilMs)
        assertNull(timed.line)
        val untimed = afterProofFailure(ProofBlockState(), ProofFailure.RateLimited(null), ProofSurface.DeleteAccount, nowMs = 1_000L)
        assertNull(untimed.waitUntilMs)
        assertEquals("common.error.too_many_attempts", (untimed.line as ProofLine.Error).text.key)
    }

    @Test
    fun `a proof that ran out shows the block idle and empty, with no line`() {
        val after = afterProofRequired(
            ProofBlockState(phase = ProofPhase.Checking, password = "old", line = ProofLine.Error(me.lakko.app.l10n.UiText("x"))),
        )
        assertEquals(ProofPhase.Idle, after.phase)
        assertEquals("", after.password)
        assertNull(after.line)
    }

    @Test
    fun `the helper names the action on every surface`() {
        assertEquals("restore_identity.reset.password_help", proofCopy(ProofSurface.Reset).helper)
        assertEquals("proof.delete.help", proofCopy(ProofSurface.DeleteAccount).helper)
        assertEquals("proof.add_device.help", proofCopy(ProofSurface.AddDeviceRequest).helper)
        assertEquals("proof.add_device.help", proofCopy(ProofSurface.AddDeviceScan).helper)
        assertEquals("proof.add_passkey.help", proofCopy(ProofSurface.AddPasskey).helper)
        assertEquals("proof.change_password.help", proofCopy(ProofSurface.ChangePassword).helper)
    }

    @Test
    fun `every key of the copy table and the surface labels is in the catalog`() {
        val keys = ProofSurface.entries.flatMap {
            val copy = proofCopy(it)
            listOf(copy.helper, copy.wrongPassword, copy.cancelled, copy.generic)
        } + PASSWORD_CHANGE_USE_WORDS +
            deleteButtonLabel(deleting = true, checking = false) +
            deleteButtonLabel(deleting = false, checking = true) +
            deleteButtonLabel(deleting = false, checking = false)
        val catalog = listOf("src/main/assets/l10n/source.json", "app/src/main/assets/l10n/source.json")
            .map { java.io.File(it) }.first { it.exists() }.readText()
        for (key in keys) assertTrue("missing catalog key $key", catalog.contains("\"$key\": {"))
    }

    @Test
    fun `the password-change gate links to the three words with its own line`() {
        assertEquals("proof.change_password.use_words", PASSWORD_CHANGE_USE_WORDS)
    }

    // ---------------------------------------------------------- the surfaces

    @Test
    fun `only add a device opens on the passkey notice`() {
        assertTrue(proofOpensOnPasskey(ProofSurface.AddDeviceRequest))
        assertTrue(proofOpensOnPasskey(ProofSurface.AddDeviceScan))
        for (s in listOf(ProofSurface.Reset, ProofSurface.DeleteAccount, ProofSurface.AddPasskey, ProofSurface.ChangePassword)) {
            assertFalse(proofOpensOnPasskey(s))
        }
    }

    @Test
    fun `delete - a passkey account always shows the passkey notice, even with a live proof`() {
        assertEquals(DeletePanel.PasskeyNotice, deletePanel(hasPasskey = true, proofLive = true, saidNoPassword = false))
        assertEquals(DeletePanel.PasskeyNotice, deletePanel(hasPasskey = true, proofLive = false, saidNoPassword = true))
    }

    @Test
    fun `delete - no passkey - a live proof shows no block, none shows it, and no password shows cannot`() {
        assertEquals(DeletePanel.NoBlock, deletePanel(hasPasskey = false, proofLive = true, saidNoPassword = true))
        assertEquals(DeletePanel.Block, deletePanel(hasPasskey = false, proofLive = false, saidNoPassword = false))
        assertEquals(DeletePanel.Cannot, deletePanel(hasPasskey = false, proofLive = false, saidNoPassword = true))
    }

    @Test
    fun `delete - the button waits for the password, and does not exist on the cannot panel`() {
        assertFalse(deleteButtonEnabled(DeletePanel.Block, view(), busy = false))
        assertTrue(deleteButtonEnabled(DeletePanel.Block, view(state = ProofBlockState(password = "p")), busy = false))
        assertFalse(deleteButtonEnabled(DeletePanel.Block, view(state = ProofBlockState(password = "p")), busy = true))
        assertTrue(deleteButtonEnabled(DeletePanel.NoBlock, view(proofLive = true), busy = false))
        assertTrue(deleteButtonEnabled(DeletePanel.PasskeyNotice, view(), busy = false))
        assertFalse(deleteButtonEnabled(DeletePanel.PasskeyNotice, view(), busy = true))
        assertFalse(deleteButtonEnabled(DeletePanel.Cannot, view(proofLive = true), busy = false))
    }

    @Test
    fun `delete - a passkey account never stops for a proof - its one gesture is the proof`() {
        assertFalse(deleteNeedsProofFirst(hasPasskey = true, proofLive = false))
        assertFalse(deleteNeedsProofFirst(hasPasskey = true, proofLive = true))
    }

    @Test
    fun `delete - without a passkey nothing is sent until a proof is live`() {
        assertTrue(deleteNeedsProofFirst(hasPasskey = false, proofLive = false))
        assertFalse(deleteNeedsProofFirst(hasPasskey = false, proofLive = true))
    }

    @Test
    fun `delete - the button says deleting while deleting and checking while the password is checked`() {
        assertEquals("settings.account.delete.confirm", deleteButtonLabel(deleting = false, checking = false))
        assertEquals("common.checking", deleteButtonLabel(deleting = false, checking = true))
        assertEquals("settings.account.delete.working", deleteButtonLabel(deleting = true, checking = false))
        assertEquals("settings.account.delete.working", deleteButtonLabel(deleting = true, checking = true))
    }

    // ------------------------------------------- kept across a rotation only

    private fun slots(cleared: MutableList<String>) = ProofSlots<StringBuilder> { cleared += it.toString() }

    @Test
    fun `a rotation keeps the controller and what was typed in it`() {
        val cleared = mutableListOf<String>()
        val slots = slots(cleared)
        val first = slots.attach("AddPasskey|") { StringBuilder("typed") }
        assertTrue(slots.detach("AddPasskey|", changingConfigurations = true))
        val second = slots.attach("AddPasskey|") { StringBuilder("new") }
        assertTrue(first === second)
        // The surface came back: the wait ending changes nothing.
        slots.expire("AddPasskey|")
        assertTrue(slots.holds("AddPasskey|"))
        assertTrue(cleared.isEmpty())
    }

    @Test
    fun `back, cancel and a closed dialog clear it at once`() {
        val cleared = mutableListOf<String>()
        val slots = slots(cleared)
        slots.attach("DeleteAccount|") { StringBuilder("typed") }
        assertFalse(slots.detach("DeleteAccount|", changingConfigurations = false))
        assertEquals(listOf("typed"), cleared)
        assertFalse(slots.holds("DeleteAccount|"))
        assertEquals("fresh", slots.attach("DeleteAccount|") { StringBuilder("fresh") }.toString())
    }

    @Test
    fun `a surface that does not come back after a rotation keeps nothing`() {
        val cleared = mutableListOf<String>()
        val slots = slots(cleared)
        slots.attach("DeleteAccount|") { StringBuilder("typed") }
        slots.detach("DeleteAccount|", changingConfigurations = true)
        assertTrue(cleared.isEmpty())
        slots.expire("DeleteAccount|")
        assertEquals(listOf("typed"), cleared)
        assertFalse(slots.holds("DeleteAccount|"))
    }

    @Test
    fun `leaving the screen for good clears every controller and slots do not share one`() {
        val cleared = mutableListOf<String>()
        val slots = slots(cleared)
        val a = slots.attach("AddPasskey|") { StringBuilder("a") }
        val b = slots.attach("DeleteAccount|") { StringBuilder("b") }
        assertFalse(a === b)
        slots.detach("AddPasskey|", changingConfigurations = true)
        slots.clearAll()
        assertEquals(setOf("a", "b"), cleared.toSet())
        // A late wait finds nothing to clear twice.
        slots.expire("AddPasskey|")
        assertEquals(2, cleared.size)
        assertFalse(slots.detach("AddPasskey|", changingConfigurations = false))
    }

    @Test
    fun `the 403 mapping - fresh-auth-required re-opens the block, anything else is the surface's own`() {
        assertEquals(GatedFailure.ProofRequired, gatedFailureOf(FreshAuthRequired()))
        assertEquals(
            GatedFailure.ProofRequired,
            gatedFailureOf(AuthHttpError(403, "/device/authorize/start", "{}", code = "fresh-auth-required")),
        )
        assertEquals(
            GatedFailure.ProofRequired,
            gatedFailureOf(WebauthnHttpError(403, "/webauthn/register/start", """{"error":"fresh-auth-required"}""")),
        )
        assertEquals(GatedFailure.Other, gatedFailureOf(AuthHttpError(403, "/x", "{}", code = "password-required")))
        assertEquals(GatedFailure.Other, gatedFailureOf(WebauthnHttpError(403, "/account", "")))
        assertEquals(GatedFailure.Other, gatedFailureOf(AuthHttpError(401, "/x", "")))
        assertEquals(GatedFailure.Other, gatedFailureOf(IOException("down")))
    }

    @Test
    fun `change password - a bare session sees the proof first, never the form`() {
        assertEquals(PasswordChangePanel.Proof, passwordChangePanel(unlocked = false, choseWords = false, holdsPhrase = true))
        assertEquals(PasswordChangePanel.Proof, passwordChangePanel(unlocked = false, choseWords = false, holdsPhrase = false))
    }

    @Test
    fun `change password - three words are offered only on a device that holds the phrase`() {
        assertEquals(PasswordChangePanel.Words, passwordChangePanel(unlocked = false, choseWords = true, holdsPhrase = true))
        assertEquals(PasswordChangePanel.Proof, passwordChangePanel(unlocked = false, choseWords = true, holdsPhrase = false))
    }

    @Test
    fun `change password - once unlocked the form shows, whichever way it was unlocked`() {
        assertEquals(PasswordChangePanel.Form, passwordChangePanel(unlocked = true, choseWords = false, holdsPhrase = false))
        assertEquals(PasswordChangePanel.Form, passwordChangePanel(unlocked = true, choseWords = true, holdsPhrase = true))
    }
}
