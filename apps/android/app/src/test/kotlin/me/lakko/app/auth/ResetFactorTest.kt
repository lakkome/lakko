// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import java.io.IOException
import me.lakko.app.l10n.UiText
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0073 (amended 2026-11-26) / UX 15.1b and 15.1c - the reset warning's
 * "Confirm it is you" section: the password, then one second factor the
 * account has.
 */
class ResetFactorTest {

    private fun factors(email: Boolean = false, totp: Boolean = false, passkey: Boolean = false) =
        AccountFactors(email = email, totp = totp, passkey = passkey)

    private fun confirmed(
        factor: FactorKind = FactorKind.Totp,
        expiresAtMs: Long = 1_000,
        prediction: SealPrediction = SealPrediction.Unsealed(false),
    ) = ResetFactorConfirmed(factor, expiresAtMs, prediction)

    private fun http(status: Int, code: String?, retry: Long? = null) =
        AuthHttpError(status, "/account/keys/reset/factor/x", "{}", code = code, retryAfterSecs = retry)

    // ---- the panel ---------------------------------------------------------

    @Test
    fun `the panel is the factors, the cannot panel, or nothing until the account's factors are read`() {
        assertEquals(ResetPanel.Loading, resetPanel(null, saidCannot = false))
        assertEquals(ResetPanel.Loading, resetPanel(null, saidCannot = true))
        assertEquals(ResetPanel.Factors, resetPanel(factors(totp = true), saidCannot = false))
        assertEquals(ResetPanel.CannotUse, resetPanel(factors(totp = true), saidCannot = true))
        // An account with none of the three: the panel, with no way back.
        assertEquals(ResetPanel.NoFactor, resetPanel(factors(), saidCannot = false))
        assertEquals(ResetPanel.NoFactor, resetPanel(factors(), saidCannot = true))
    }

    @Test
    fun `the cannot panel has a body per reason`() {
        assertEquals("proof.reset.no_factor.body", resetCannotBodyKey(ResetPanel.CannotUse))
        assertEquals("proof.reset.no_factor.body_none", resetCannotBodyKey(ResetPanel.NoFactor))
    }

    // ---- the warning above the panel (UX 15.1, amended 2026-11-28) ----------

    @Test
    fun `the warning is not drawn above a cannot-reset panel nor while the factors load`() {
        assertNull(resetWarningKeys(resetPanel(factors(), saidCannot = false), factors()))
        val some = factors(totp = true)
        assertNull(resetWarningKeys(resetPanel(some, saidCannot = true), some))
        assertNull(resetWarningKeys(resetPanel(null, saidCannot = false), null))
    }

    @Test
    fun `the warning above the section names only what the account keeps`() {
        val f = factors(totp = true, passkey = true)
        assertEquals(
            listOf(
                "restore_identity.reset.effect_devices",
                "restore_identity.reset.effect_passkeys",
                "restore_identity.reset.effect_safety_number",
                "restore_identity.reset.keeps.password_totp",
            ),
            resetWarningKeys(resetPanel(f, saidCannot = false), f),
        )
        assertEquals("restore_identity.reset.keeps.password", resetKeepsKey(factors(passkey = true)))
        assertEquals("restore_identity.reset.keeps.password_email", resetKeepsKey(factors(email = true)))
        assertEquals(
            "restore_identity.reset.keeps.password_totp_email",
            resetKeepsKey(factors(email = true, totp = true, passkey = true)),
        )
    }

    @Test
    fun `the rows are the factors the account has, in order, the passkey only where usable`() {
        val all = factors(email = true, totp = true, passkey = true)
        assertEquals(
            listOf(FactorKind.Totp, FactorKind.Email, FactorKind.Passkey),
            resetFactorRows(all, passkeysUsableHere = true),
        )
        // Absent, never disabled.
        assertEquals(listOf(FactorKind.Totp, FactorKind.Email), resetFactorRows(all, passkeysUsableHere = false))
        assertEquals(listOf(FactorKind.Email), resetFactorRows(factors(email = true), passkeysUsableHere = true))
    }

    @Test
    fun `the first line follows whether the password block is drawn`() {
        assertEquals("proof.reset.factor.title", resetFactorTitleKey(passwordProofLive = false))
        assertEquals("proof.reset.factor.title_no_password", resetFactorTitleKey(passwordProofLive = true))
    }

    @Test
    fun `a factor's action waits for the password, typed or proven`() {
        assertTrue(resetPasswordReady(passwordProofLive = true, password = "", usernameKnown = true, username = ""))
        assertTrue(resetPasswordReady(false, "hunter2", usernameKnown = true, username = ""))
        assertFalse(resetPasswordReady(false, "", usernameKnown = true, username = ""))
        // A device that does not know the username needs the name too.
        assertFalse(resetPasswordReady(false, "hunter2", usernameKnown = false, username = " "))
        assertTrue(resetPasswordReady(false, "hunter2", usernameKnown = false, username = "alice"))
        // A live password proof never waits for a typed name.
        assertTrue(resetPasswordReady(true, "", usernameKnown = false, username = ""))
    }

    @Test
    fun `the collapsed line names the factor that confirmed`() {
        assertEquals("proof.reset.factor.done.totp", resetFactorDoneKey(FactorKind.Totp))
        assertEquals("proof.reset.factor.done.email", resetFactorDoneKey(FactorKind.Email))
        assertEquals("proof.reset.factor.done.passkey", resetFactorDoneKey(FactorKind.Passkey))
    }

    @Test
    fun `a confirmation stands for its lifetime and then the section opens again`() {
        assertTrue(resetFactorLive(confirmed(expiresAtMs = 1_000), nowMs = 999))
        assertFalse(resetFactorLive(confirmed(expiresAtMs = 1_000), nowMs = 1_000))
        assertFalse(resetFactorLive(null, nowMs = 0))
    }

    @Test
    fun `the reset button needs a confirmed factor and the word`() {
        val live = confirmed(expiresAtMs = 1_000)
        assertTrue(resetButtonEnabled(wordMatches = true, confirmed = live, nowMs = 10, busy = false))
        assertFalse(resetButtonEnabled(wordMatches = false, confirmed = live, nowMs = 10, busy = false))
        assertFalse(resetButtonEnabled(wordMatches = true, confirmed = null, nowMs = 10, busy = false))
        assertFalse("a confirmation that ran out", resetButtonEnabled(true, live, nowMs = 1_000, busy = false))
        assertFalse(resetButtonEnabled(true, live, nowMs = 10, busy = true))
    }

    @Test
    fun `use passkey is offered under an unsealed line only when a passkey could seal and can be used here`() {
        assertTrue(unsealedOffersPasskey(SealPrediction.Unsealed(passkeyCouldSeal = true), passkeysUsableHere = true))
        assertFalse(unsealedOffersPasskey(SealPrediction.Unsealed(passkeyCouldSeal = true), passkeysUsableHere = false))
        assertFalse(unsealedOffersPasskey(SealPrediction.Unsealed(passkeyCouldSeal = false), passkeysUsableHere = true))
        val sealed = SealPrediction.Sealed(SealEntryView(SealKindName.PASSWORD, SEAL_TAG_NONE_HEX, "pw"), 1)
        assertFalse(unsealedOffersPasskey(sealed, passkeysUsableHere = true))
    }

    // ---- refusals ----------------------------------------------------------

    @Test
    fun `the server's two refusals of the reset itself are recognised`() {
        assertTrue(isResetFactorRequired(ResetFactorRequired()))
        assertTrue(isResetFactorRequired(http(403, RESET_FACTOR_REQUIRED)))
        assertFalse(isResetFactorRequired(http(403, FRESH_AUTH_REQUIRED)))
        assertFalse(isResetFactorRequired(http(401, RESET_FACTOR_REQUIRED)))
    }

    @Test
    fun `the password is judged first - a wrong one stops there and no code is used up`() {
        for (call in ResetFactorCall.entries) {
            assertEquals(ResetFactorFailure.WrongPassword, resetFactorFailureOf(call, FreshAuthWrongPassword()))
            // The proof ran out or was never made: the block appears.
            assertEquals(ResetFactorFailure.PasswordNeeded, resetFactorFailureOf(call, FreshAuthRequired()))
            assertEquals(ResetFactorFailure.PasswordNeeded, resetFactorFailureOf(call, http(403, FRESH_AUTH_REQUIRED)))
        }
        assertEquals(ResetFactorFailure.WrongPassword, resetFactorFailureOf(ResetFactorCall.Password, http(401, null)))
    }

    @Test
    fun `a wrong code reads per factor, and a limit is a wait, never a wrong code`() {
        assertEquals(
            ResetFactorFailure.WrongCode(UiText("settings.recovery.totp.error.wrong")),
            resetFactorFailureOf(ResetFactorCall.Totp, http(401, FACTOR_REFUSED)),
        )
        assertEquals(
            ResetFactorFailure.WrongCode(UiText("recovery_email.code.wrong")),
            resetFactorFailureOf(ResetFactorCall.EmailFinish, http(401, FACTOR_REFUSED)),
        )
        assertEquals(
            ResetFactorFailure.Line(UiText("proof.passkey.failed")),
            resetFactorFailureOf(ResetFactorCall.Passkey, http(401, FACTOR_REFUSED)),
        )
        for (call in ResetFactorCall.entries) {
            assertEquals(
                ResetFactorFailure.Wait(900),
                resetFactorFailureOf(call, http(429, "too-many-attempts", retry = 900)),
            )
        }
    }

    @Test
    fun `the email row's failures follow the letter's table`() {
        assertEquals(
            ResetFactorFailure.EmailRestart(UiText("recovery_email.code.expired")),
            resetFactorFailureOf(ResetFactorCall.EmailFinish, http(401, "code-expired")),
        )
        assertEquals(
            ResetFactorFailure.Line(UiText("recovery_email.send_failed")),
            resetFactorFailureOf(ResetFactorCall.EmailStart, http(502, "mail-send-failed")),
        )
        assertEquals(
            ResetFactorFailure.Line(UiText("recovery_email.invalid")),
            resetFactorFailureOf(ResetFactorCall.EmailStart, http(400, "address-invalid")),
        )
        assertEquals(
            ResetFactorFailure.Line(UiText("recovery_email.unavailable")),
            resetFactorFailureOf(ResetFactorCall.EmailStart, http(503, RECOVERY_EMAIL_DISABLED)),
        )
    }

    @Test
    fun `five wrong mailed codes send the row back to Send code`() {
        for (before in 0..3) {
            assertEquals(
                ResetFactorFailure.WrongCode(UiText("recovery_email.code.wrong")),
                afterWrongResetEmailCode(wrongTries = before),
            )
        }
        assertEquals(
            ResetFactorFailure.EmailRestart(UiText("recovery_email.code.used_up")),
            afterWrongResetEmailCode(wrongTries = 4),
        )
    }

    @Test
    fun `a cancelled sheet, another account's passkey, offline and the rest each have their line`() {
        assertEquals(
            ResetFactorFailure.Line(UiText("proof.reset.cancelled")),
            resetFactorFailureOf(ResetFactorCall.Passkey, PasskeyCancelled()),
        )
        assertEquals(
            ResetFactorFailure.Line(UiText("proof.passkey.other_account")),
            resetFactorFailureOf(ResetFactorCall.Passkey, FreshAuthOtherAccount()),
        )
        assertEquals(
            ResetFactorFailure.Line(UiText("common.error.offline")),
            resetFactorFailureOf(ResetFactorCall.Totp, AuthNetworkError("/x", IOException("down"))),
        )
        assertEquals(
            ResetFactorFailure.Line(UiText("proof.passkey.failed")),
            resetFactorFailureOf(ResetFactorCall.Passkey, IllegalStateException("x")),
        )
        assertEquals(
            ResetFactorFailure.Line(UiText("restore_identity.reset.error.generic")),
            resetFactorFailureOf(ResetFactorCall.Totp, IllegalStateException("x")),
        )
        assertEquals(
            ResetFactorFailure.Line(UiText("restore_identity.reset.error.generic")),
            resetFactorFailureOf(ResetFactorCall.Totp, http(500, null)),
        )
    }

    // ---- the seal of the reset ---------------------------------------------

    private object Core : SealCore {
        override fun entryOf(secret: SealSecret): SealEntryView = when (secret) {
            is SealSecret.Password -> SealEntryView(SealKindName.PASSWORD, SEAL_TAG_NONE_HEX, "pw" + secret.exportKey[0])
            is SealSecret.Passkey -> {
                require(secret.prfOutput.isNotEmpty()) { "refused" }
                SealEntryView(SealKindName.PASSKEY, "tag", "pk")
            }
            is SealSecret.Phrase -> SealEntryView(SealKindName.PHRASE, SEAL_TAG_NONE_HEX, "ph")
        }
        override fun passkeyTagHex(credentialId: ByteArray): String = "tag"
        override fun open(announcement: ByteArray, accountKey: ByteArray): SealAnnouncementView? = null
        override fun decodeUnverified(announcement: ByteArray): SealAnnouncementView? = null
        override fun signAnnouncement(
            accountSeed: ByteArray,
            accountId: ByteArray,
            keyGeneration: Long,
            seq: Long,
            issuedAtMs: Long,
            entries: List<SealEntryView>,
        ): ByteArray = ByteArray(0)
        override fun signResetSeal(
            secret: SealSecret,
            accountId: ByteArray,
            announcementSeq: Long,
            fromGeneration: Long,
            newSignaturePubkey: ByteArray,
        ): ByteArray = ByteArray(0)
    }

    @Test
    fun `the device's seal keys are tried in order - password, passkey, phrase`() {
        val export = ByteArray(ExportKeyStore.EXPORT_KEY_LENGTH) { 3 }
        val options = resetSealOptions(
            Core,
            exportKey = export,
            passkey = SealSecret.Passkey(ByteArray(32) { 1 }, ByteArray(16)),
            phrase = "twelve words",
        )
        assertEquals(
            listOf(SealKindName.PASSWORD, SealKindName.PASSKEY, SealKindName.PHRASE),
            options.map { it.entry.kind },
        )
        // What the device does not hold, an export key of the wrong length
        // and a secret the core refuses are left out.
        assertTrue(resetSealOptions(Core, null, null, null).isEmpty())
        assertTrue(resetSealOptions(Core, ByteArray(5), null, null).isEmpty())
        assertTrue(resetSealOptions(Core, null, SealSecret.Passkey(ByteArray(0), ByteArray(16)), null).isEmpty())
    }

    @Test
    fun `the reset signs with the key the prediction chose, or with none`() {
        val options = resetSealOptions(Core, ByteArray(ExportKeyStore.EXPORT_KEY_LENGTH) { 3 }, null, "words")
        val phrase = options.first { it.entry.kind == SealKindName.PHRASE }
        assertEquals(phrase, chosenSealOption(SealPrediction.Sealed(phrase.entry, 4), options))
        assertNull(chosenSealOption(SealPrediction.Unsealed(passkeyCouldSeal = true), options))
        // A prediction for a key this device does not hold signs nothing.
        val other = SealEntryView(SealKindName.PASSKEY, "zz", "pk")
        assertNull(chosenSealOption(SealPrediction.Sealed(other, 4), options))
    }
}
