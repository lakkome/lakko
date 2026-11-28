// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import kotlinx.coroutines.async
import kotlinx.coroutines.test.advanceTimeBy
import org.junit.Test

/**
 * ADR-0075 (amended 2026-11-26) / UX 19.1 to 19.1d - one kind of account,
 * its three second ways in, and what the screens draw from them.
 */
class AccountProtectionTest {

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

    private fun factors(email: Boolean = false, totp: Boolean = false, passkey: Boolean = false) =
        AccountFactors(email = email, totp = totp, passkey = passkey)

    @Test
    fun `new words are shown only to an account whose phrase is saved`() {
        // After a rotation or a recall. After a key reset the new phrase is
        // not saved, so no words are shown.
        assertTrue(showsNewWords(phraseSaved = true))
        assertFalse(showsNewWords(phraseSaved = false))
    }

    @Test
    fun `restore your keys lists the phrase only when one is saved, and keeps it while not known`() {
        assertTrue(restoreKeysRows(phraseSaved = true).phrase)
        assertFalse(restoreKeysRows(phraseSaved = false).phrase)
        // The account's published protection could not be read: the words
        // may exist, so the row stays.
        assertTrue(restoreKeysRows(phraseSaved = null).phrase)
    }

    // ---- the three second ways in ------------------------------------------

    @Test
    fun `the factors come from the server's status and the passkey list, or are not known`() {
        val status = RecoveryEmailStatus(available = true, enabled = true, totp = false)
        assertEquals(factors(email = true, passkey = true), accountFactors(status, hasPasskey = true))
        assertEquals(factors(email = true), accountFactors(status, hasPasskey = false))
        assertNull("nothing is decided from a guess", accountFactors(null, hasPasskey = true))
        assertNull(accountFactors(status, hasPasskey = null))
    }

    @Test
    fun `the factors are listed in the reset panel's fixed order`() {
        assertEquals(
            listOf(FactorKind.Totp, FactorKind.Email, FactorKind.Passkey),
            factors(email = true, totp = true, passkey = true).inOrder,
        )
        assertEquals(listOf(FactorKind.Email), factors(email = true).inOrder)
        assertTrue(factors().inOrder.isEmpty())
    }

    @Test
    fun `an account with a username and none of the three is shown the step before the app`() {
        assertTrue(factorStepRequired(hasUsername = true, factors = factors()))
        for (one in listOf(factors(email = true), factors(totp = true), factors(passkey = true))) {
            assertFalse(factorStepRequired(hasUsername = true, factors = one))
        }
        // A guest has no username and sees none of this.
        assertFalse(factorStepRequired(hasUsername = false, factors = factors()))
        // Factors that could not be read decide nothing.
        assertFalse(factorStepRequired(hasUsername = true, factors = null))
    }

    @Test
    fun `the step's heading says sign-up or a second way to get back in`() {
        assertEquals("protect.title", protectStepTitleKey(afterSignup = true))
        assertEquals("protect.title_required", protectStepTitleKey(afterSignup = false))
    }

    @Test
    fun `the server's last-factor refusal is recognised from auth and from the passkey routes`() {
        assertTrue(isLastFactor(AuthHttpError(409, "/recovery-email", "{}", code = "last-factor")))
        assertFalse(isLastFactor(AuthHttpError(409, "/recovery-email", "{}", code = "handle-taken")))
        assertFalse(isLastFactor(AuthHttpError(403, "/recovery-email", "{}", code = "last-factor")))
        assertFalse(isLastFactor(IllegalStateException("x")))
    }

    @Test
    fun `the last of the three cannot be removed`() {
        assertTrue(removalLeavesNoFactor(factors(email = true), FactorKind.Email, passkeyCount = 0))
        assertFalse(removalLeavesNoFactor(factors(email = true, totp = true), FactorKind.Email, passkeyCount = 0))
        // The last passkey of an account with nothing else.
        assertTrue(removalLeavesNoFactor(factors(passkey = true), FactorKind.Passkey, passkeyCount = 1))
        // One of two passkeys: a passkey stays.
        assertFalse(removalLeavesNoFactor(factors(passkey = true), FactorKind.Passkey, passkeyCount = 2))
        assertFalse(removalLeavesNoFactor(factors(passkey = true, email = true), FactorKind.Passkey, passkeyCount = 1))
    }

    @Test
    fun `a removal that leaves only the authenticator says so, with or without a phrase`() {
        val both = factors(email = true, totp = true)
        assertEquals(
            "protect.totp_only.remove_notice",
            removalTotpOnlyNoticeKey(both, FactorKind.Email, passkeyCount = 0, phraseSaved = false),
        )
        assertEquals(
            "protect.totp_only.remove_notice_phrase",
            removalTotpOnlyNoticeKey(both, FactorKind.Email, passkeyCount = 0, phraseSaved = true),
        )
        // The last passkey beside an authenticator.
        assertEquals(
            "protect.totp_only.remove_notice",
            removalTotpOnlyNoticeKey(factors(totp = true, passkey = true), FactorKind.Passkey, 1, phraseSaved = false),
        )
        // One of two passkeys: a passkey stays, nothing is said.
        assertNull(removalTotpOnlyNoticeKey(factors(totp = true, passkey = true), FactorKind.Passkey, 2, false))
        // An email stays beside the authenticator.
        assertNull(
            removalTotpOnlyNoticeKey(factors(email = true, totp = true, passkey = true), FactorKind.Passkey, 1, false),
        )
        // No authenticator: the refusal of the last factor applies instead.
        assertNull(removalTotpOnlyNoticeKey(factors(email = true), FactorKind.Email, 0, false))
    }

    @Test
    fun `the authenticator-only warning stands while it is the only one of the three`() {
        assertEquals("protect.totp_only.warning", totpOnlyWarningKey(factors(totp = true), phraseSaved = false))
        assertEquals("protect.totp_only.warning_phrase", totpOnlyWarningKey(factors(totp = true), phraseSaved = true))
        // It goes as soon as an email or a passkey is added.
        assertNull(totpOnlyWarningKey(factors(totp = true, email = true), phraseSaved = false))
        assertNull(totpOnlyWarningKey(factors(totp = true, passkey = true), phraseSaved = false))
        assertNull(totpOnlyWarningKey(factors(email = true), phraseSaved = false))
        assertNull(totpOnlyWarningKey(factors(), phraseSaved = false))
    }

    // ---- what keeps the chats ----------------------------------------------

    @Test
    fun `the chats are kept by the phrase, else a passkey, else the password only`() {
        assertEquals(ChatsKeptBy.Phrase, chatsKeptBy(phraseSeal = true, passkeySeal = false))
        assertEquals(ChatsKeptBy.Phrase, chatsKeptBy(phraseSeal = true, passkeySeal = true))
        assertEquals(ChatsKeptBy.PasskeySeal, chatsKeptBy(phraseSeal = false, passkeySeal = true))
        assertEquals(ChatsKeptBy.PasswordOnly, chatsKeptBy(phraseSeal = false, passkeySeal = false))
    }

    @Test
    fun `the status line has one key per answer`() {
        assertEquals("protect.status.phrase", protectionStatusKey(ChatsKeptBy.Phrase))
        assertEquals("protect.status.passkey_seal", protectionStatusKey(ChatsKeptBy.PasskeySeal))
        assertEquals("protect.status.password_only", protectionStatusKey(ChatsKeptBy.PasswordOnly))
    }

    @Test
    fun `the honest notice is for an account with a second way in kept by the password only`() {
        assertTrue(passwordOnlyNoticeShown(hasFactor = true, kept = ChatsKeptBy.PasswordOnly))
        assertFalse(passwordOnlyNoticeShown(hasFactor = true, kept = ChatsKeptBy.Phrase))
        assertFalse(passwordOnlyNoticeShown(hasFactor = true, kept = ChatsKeptBy.PasskeySeal))
        // No factor yet: the step of 19.1a is what is shown.
        assertFalse(passwordOnlyNoticeShown(hasFactor = false, kept = ChatsKeptBy.PasswordOnly))
        // The account's protection could not be read: nothing is shown.
        assertFalse(passwordOnlyNoticeShown(hasFactor = true, kept = null))
    }

    @Test
    fun `the dialog is shown once per device after a sign-in and never after a key reset`() {
        assertTrue(passwordOnlyDialogDue(noticeShown = true, shownBefore = false, afterKeyReset = false))
        assertFalse(passwordOnlyDialogDue(noticeShown = true, shownBefore = true, afterKeyReset = false))
        assertFalse(passwordOnlyDialogDue(noticeShown = true, shownBefore = false, afterKeyReset = true))
        assertFalse(passwordOnlyDialogDue(noticeShown = false, shownBefore = false, afterKeyReset = false))
    }

    // ---- Settings rows -----------------------------------------------------

    @Test
    fun `the recovery email row is absent without mail, else On or Off`() {
        assertEquals(RecoveryEmailRow.Absent, recoveryEmailRow(RecoveryEmailStatus(available = false, enabled = false)))
        assertEquals(RecoveryEmailRow.Off, recoveryEmailRow(RecoveryEmailStatus(available = true, enabled = false)))
        assertEquals(RecoveryEmailRow.On, recoveryEmailRow(RecoveryEmailStatus(available = true, enabled = true)))
    }

    @Test
    fun `show phrase exists only for a saved phrase this device holds`() {
        assertEquals(RecoveryPhraseRow.Save, recoveryPhraseRow(phraseSaved = false, phraseOnDevice = true))
        assertEquals(RecoveryPhraseRow.Show, recoveryPhraseRow(phraseSaved = true, phraseOnDevice = true))
        assertEquals(RecoveryPhraseRow.SavedElsewhere, recoveryPhraseRow(phraseSaved = true, phraseOnDevice = false))
    }

    @Test
    fun `three words are offered only to an account whose phrase is saved`() {
        assertTrue(threeWordsSurfaceOffered(phraseSaved = true))
        assertFalse(threeWordsSurfaceOffered(phraseSaved = false))
    }

    @Test
    fun `the add form is pre-filled after a recall and empty for a replacement`() {
        // After a recall the server dropped the stored value (`enabled` false).
        assertEquals("name@example.com", enrolPrefill(enabled = false, heldAddress = "name@example.com"))
        assertEquals("", enrolPrefill(enabled = false, heldAddress = null))
        // Replace: a new address is being typed.
        assertEquals("", enrolPrefill(enabled = true, heldAddress = "name@example.com"))
    }

    // ---- the required step -------------------------------------------------

    @Test
    fun `the step always has the authenticator and the phrase rows`() {
        val bare = protectStepRows(mailAvailable = false, hasUsername = false, passkeysSupported = false)
        assertEquals(ProtectStepRows(email = false, totp = true, passkey = false, phrase = true), bare)
        // The email row needs mail on the server and a username.
        assertFalse(protectStepRows(mailAvailable = true, hasUsername = false).email)
        assertFalse(protectStepRows(mailAvailable = false, hasUsername = true).email)
        assertTrue(protectStepRows(mailAvailable = true, hasUsername = true).email)
        // The passkey row only where this device can use passkeys, for an
        // account with a username.
        assertTrue(protectStepRows(true, hasUsername = true, passkeysSupported = true).passkey)
        assertFalse(protectStepRows(true, hasUsername = true, passkeysSupported = false).passkey)
        assertFalse(protectStepRows(true, hasUsername = false, passkeysSupported = true).passkey)
    }

    @Test
    fun `a row is done when finished here or already true of the account`() {
        val status = RecoveryEmailStatus(available = true, enabled = true, totp = true)
        val fromAccount = protectStepDone(ProtectStepDone(), status, hasPasskey = true, phraseSaved = true)
        assertEquals(ProtectStepDone(email = true, totp = true, passkey = true, phrase = true), fromAccount)
        val here = protectStepDone(ProtectStepDone(totp = true), status = null)
        assertEquals(ProtectStepDone(totp = true), here)
        assertEquals(AccountFactors(email = false, totp = true, passkey = false), here.factors)
    }

    @Test
    fun `Continue needs one of the first three, and a saved phrase alone does not enable it`() {
        assertFalse(protectContinueEnabled(ProtectStepDone()))
        assertFalse(protectContinueEnabled(ProtectStepDone(phrase = true)))
        assertTrue(protectContinueEnabled(ProtectStepDone(email = true)))
        assertTrue(protectContinueEnabled(ProtectStepDone(totp = true)))
        assertTrue(protectContinueEnabled(ProtectStepDone(passkey = true)))
    }

    @Test
    fun `the notice on the step shows once a factor is done and nothing keeps the chats but the password`() {
        assertFalse(protectStepNoticeShown(ProtectStepDone(), passkeyKeepsChats = false))
        assertTrue(protectStepNoticeShown(ProtectStepDone(email = true), passkeyKeepsChats = false))
        // A passkey that does not keep the chats does not end it.
        assertTrue(protectStepNoticeShown(ProtectStepDone(passkey = true), passkeyKeepsChats = false))
        assertFalse(protectStepNoticeShown(ProtectStepDone(passkey = true), passkeyKeepsChats = true))
        assertFalse(protectStepNoticeShown(ProtectStepDone(email = true, phrase = true), passkeyKeepsChats = false))
        // A phrase alone: `Continue` is still disabled and nothing is said.
        assertFalse(protectStepNoticeShown(ProtectStepDone(phrase = true), passkeyKeepsChats = false))
    }

    // ---- typed values ------------------------------------------------------

    @Test
    fun `an address is plausible by the core's rule`() {
        assertTrue(recoveryEmailPlausible("name@example.com"))
        assertTrue(recoveryEmailPlausible("  Name@Example.COM \n"))
        assertTrue(recoveryEmailPlausible("a@b"))
        assertFalse(recoveryEmailPlausible(""))
        assertFalse(recoveryEmailPlausible("name"))
        assertFalse(recoveryEmailPlausible("@example.com"))
        assertFalse(recoveryEmailPlausible("name@"))
        assertFalse(recoveryEmailPlausible("a@b@c"))
        assertFalse(recoveryEmailPlausible("na me@example.com"))
        assertFalse(recoveryEmailPlausible("a@" + "b".repeat(254)))
    }

    @Test
    fun `a code is six digits`() {
        assertEquals("123456", normalizeEmailCode("123456"))
        assertEquals("123456", normalizeEmailCode(" 123 456 "))
        assertEquals("123456", normalizeEmailCode("123-456"))
        assertNull(normalizeEmailCode("12345"))
        assertNull(normalizeEmailCode("1234567"))
        assertNull(normalizeEmailCode("12345a"))
    }

    // ---- what the device remembers -----------------------------------------

    @Test
    fun `the store keeps flags per account and never an address`() {
        val storage = MapStorage()
        val store = AccountProtectionStore(storage)
        assertFalse(store.phraseSaved("a"))
        assertFalse(store.passwordOnlyDialogShown("a"))
        assertFalse(store.protectStepOwed("a"))
        store.setPhraseSaved("a", true)
        store.markPasswordOnlyDialogShown("a")
        store.setProtectStepOwed("a", true)
        store.setPhraseSaved("b", true)
        assertTrue(AccountProtectionStore(storage).phraseSaved("a"))
        assertTrue("the dialog is not shown again on this device", store.passwordOnlyDialogShown("a"))
        assertFalse(store.passwordOnlyDialogShown("b"))
        assertTrue(store.protectStepOwed("a"))
        store.setProtectStepOwed("a", false)
        assertFalse(store.protectStepOwed("a"))
        store.clearAccount("a")
        assertFalse(store.phraseSaved("a"))
        assertFalse(store.passwordOnlyDialogShown("a"))
        assertTrue("another account's rows stay", store.phraseSaved("b"))
        // Only flags were ever written.
        assertTrue(storage.rows.values.all { it == "1" })
    }

    @Test
    fun `the held address never prints`() {
        val text = RecoveryAddressState(ready = true, address = "name@example.com").toString()
        assertFalse(text.contains("name@example.com"))
    }

    @Test
    fun `the first-password debt survives a restart and goes with the account`() {
        val storage = MapStorage()
        AccountProtectionStore(storage).setFirstPasswordOwed("a", true)
        val store = AccountProtectionStore(storage)
        assertTrue(store.firstPasswordOwed("a"))
        assertFalse(store.firstPasswordOwed("b"))
        store.setFirstPasswordOwed("a", false)
        assertFalse(store.firstPasswordOwed("a"))
        store.setFirstPasswordOwed("a", true)
        store.clearAccount("a")
        assertFalse(store.firstPasswordOwed("a"))
    }

    // ---- the landing rule without a flash (alignment 2026-11-27) -------------

    @Test
    fun `the landing waits for the factor read and takes its answer`() = kotlinx.coroutines.test.runTest {
        val flow = kotlinx.coroutines.flow.MutableStateFlow<AccountFactors?>(null)
        val waiting = async { awaitFactorRead(flow) }
        advanceTimeBy(1_000)
        assertFalse(waiting.isCompleted)
        flow.value = AccountFactors(email = false, totp = false, passkey = false)
        assertEquals(AccountFactors(email = false, totp = false, passkey = false), waiting.await())
        // Answered with no factor: the step is shown.
        assertTrue(factorStepRequired(hasUsername = true, factors = waiting.await()))
    }

    @Test
    fun `the landing shows the app after three seconds without an answer, and no step`() = kotlinx.coroutines.test.runTest {
        val flow = kotlinx.coroutines.flow.MutableStateFlow<AccountFactors?>(null)
        val waiting = async { awaitFactorRead(flow) }
        advanceTimeBy(FACTOR_READ_WAIT_MS + 1)
        assertNull(waiting.await())
        assertFalse(factorStepRequired(hasUsername = true, factors = null))
        assertEquals(3_000L, FACTOR_READ_WAIT_MS)
    }

    @Test
    fun `the cover hides the app only while the read is awaited and no step is drawn`() {
        assertTrue(landingCoverShown(waited = false, protectStepShown = false))
        assertFalse(landingCoverShown(waited = true, protectStepShown = false))
        assertFalse(landingCoverShown(waited = false, protectStepShown = true))
    }
}
