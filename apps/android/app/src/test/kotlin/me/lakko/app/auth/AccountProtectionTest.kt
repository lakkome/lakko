// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0067 / UX 19.1 - the account's mode and what the screens draw from it. */
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

    private fun status(available: Boolean = true, enabled: Boolean, vault: Boolean, phraseSaved: Boolean) =
        RecoveryEmailStatus(available = available, enabled = enabled, vault = vault, phraseSaved = phraseSaved)

    @Test
    fun `the mode is phrase, email or none`() {
        assertEquals(AccountProtection.None, accountProtection(enabled = false, vault = false, phraseSaved = false))
        assertEquals(AccountProtection.Email, accountProtection(enabled = true, vault = true, phraseSaved = false))
        // A saved phrase wins whatever else is true.
        assertEquals(AccountProtection.Phrase, accountProtection(enabled = false, vault = false, phraseSaved = true))
        assertEquals(AccountProtection.Phrase, accountProtection(enabled = true, vault = false, phraseSaved = true))
        assertEquals(AccountProtection.Phrase, accountProtection(enabled = true, vault = true, phraseSaved = true))
    }

    @Test
    fun `an email without a vault and without a phrase protects nothing`() {
        // After a key reset or a recall the server dropped the vault.
        val dropped = status(enabled = true, vault = false, phraseSaved = false)
        assertEquals(AccountProtection.None, accountProtection(dropped))
        assertEquals(RecoveryEmailRow.AddAgain, recoveryEmailRow(dropped))
        assertTrue(protectBannerVisible(accountProtection(dropped), dismissedAtMs = null, nowMs = 0L))
    }

    @Test
    fun `new words are shown only to a phrase-protected account`() {
        // After a rotation, a key reset or a recall.
        assertTrue(showsNewWords(phraseSaved = true))
        assertFalse(showsNewWords(phraseSaved = false))
    }

    @Test
    fun `after the keys changed the banner and the row ask for the address again`() {
        val dropped = status(enabled = true, vault = false, phraseSaved = false)
        assertEquals(
            ProtectPrompt("recovery_email.add_again.title", "recovery_email.add_again.body"),
            protectPrompt(dropped),
        )
        // Never protected: the ordinary banner.
        assertEquals(
            ProtectPrompt("protect.banner.none", null),
            protectPrompt(status(enabled = false, vault = false, phraseSaved = false)),
        )
        // A phrase-protected account with an email and no vault is not "add again".
        assertEquals(RecoveryEmailRow.On, recoveryEmailRow(status(enabled = true, vault = false, phraseSaved = true)))
    }

    @Test
    fun `restore your keys hides the phrase without a saved one and offers the email code with a vault`() {
        // Mail-protected: no phrase row, the email code row.
        assertEquals(
            RestoreKeysRows(phrase = false, emailCode = true),
            restoreKeysRows(status(enabled = true, vault = true, phraseSaved = false)),
        )
        // Phrase-protected, with or without an email: the phrase row only.
        assertEquals(
            RestoreKeysRows(phrase = true, emailCode = false),
            restoreKeysRows(status(enabled = true, vault = false, phraseSaved = true)),
        )
        // Not protected, or the vault dropped: neither.
        assertEquals(
            RestoreKeysRows(phrase = false, emailCode = false),
            restoreKeysRows(status(enabled = true, vault = false, phraseSaved = false)),
        )
        assertEquals(
            RestoreKeysRows(phrase = false, emailCode = false),
            restoreKeysRows(status(enabled = false, vault = false, phraseSaved = false)),
        )
        // Mail not set up: no email row even with a vault.
        assertFalse(restoreKeysRows(status(available = false, enabled = true, vault = true, phraseSaved = false)).emailCode)
        // Not read: the phrase row stays, no email row.
        assertEquals(RestoreKeysRows(phrase = true, emailCode = false), restoreKeysRows(null))
    }

    @Test
    fun `the status line has one key per mode`() {
        assertEquals("protect.status.email", protectionStatusKey(AccountProtection.Email))
        assertEquals("protect.status.phrase", protectionStatusKey(AccountProtection.Phrase))
        assertEquals("protect.status.none", protectionStatusKey(AccountProtection.None))
    }

    @Test
    fun `the recovery email row is absent when the server cannot send mail`() {
        assertEquals(
            RecoveryEmailRow.Absent,
            recoveryEmailRow(status(available = false, enabled = true, vault = true, phraseSaved = false)),
        )
        assertEquals(RecoveryEmailRow.Off, recoveryEmailRow(status(enabled = false, vault = false, phraseSaved = false)))
        assertEquals(RecoveryEmailRow.On, recoveryEmailRow(status(enabled = true, vault = true, phraseSaved = false)))
        // Phrase-protected with an email: on, and it never has a vault.
        assertEquals(RecoveryEmailRow.On, recoveryEmailRow(status(enabled = true, vault = false, phraseSaved = true)))
    }

    @Test
    fun `show phrase exists only for a phrase-protected account`() {
        assertEquals(RecoveryPhraseRow.Save, recoveryPhraseRow(phraseSaved = false, phraseOnDevice = true))
        assertEquals(RecoveryPhraseRow.Save, recoveryPhraseRow(phraseSaved = false, phraseOnDevice = false))
        assertEquals(RecoveryPhraseRow.Show, recoveryPhraseRow(phraseSaved = true, phraseOnDevice = true))
        assertEquals(RecoveryPhraseRow.SavedElsewhere, recoveryPhraseRow(phraseSaved = true, phraseOnDevice = false))
    }

    @Test
    fun `three words are offered only to a phrase-protected account`() {
        assertFalse(threeWordsSurfaceOffered(phraseSaved = false))
        assertTrue(threeWordsSurfaceOffered(phraseSaved = true))
        // The Get back in row: every other condition met, no saved phrase.
        assertFalse(
            offersThreeWords("alice", phraseOnDevice = true, keysOnDevice = true, suspended = false, phraseSaved = false),
        )
        assertTrue(
            offersThreeWords("alice", phraseOnDevice = true, keysOnDevice = true, suspended = false, phraseSaved = true),
        )
    }

    @Test
    fun `the vault is sealed only while no phrase is saved`() {
        assertTrue(enrolSealsVault(phraseSaved = false))
        assertFalse(enrolSealsVault(phraseSaved = true))
    }

    @Test
    fun `the protect step drops the email row without mail or without a username`() {
        assertEquals(
            ProtectStepRows(email = true, passkey = true, phrase = true),
            protectStepRows(mailAvailable = true, hasUsername = true, passkeysSupported = true),
        )
        assertEquals(
            ProtectStepRows(email = false, passkey = true, phrase = true),
            protectStepRows(mailAvailable = false, hasUsername = true, passkeysSupported = true),
        )
        // A guest: neither the email nor the passkey row.
        assertEquals(
            ProtectStepRows(email = false, passkey = false, phrase = true),
            protectStepRows(mailAvailable = true, hasUsername = false, passkeysSupported = true),
        )
    }

    @Test
    fun `the passkey row is shown only where this device can use passkeys`() {
        assertTrue(protectStepRows(mailAvailable = true, hasUsername = true, passkeysSupported = true).passkey)
        assertFalse(protectStepRows(mailAvailable = true, hasUsername = true, passkeysSupported = false).passkey)
    }

    @Test
    fun `a finished row is marked done, here or already true of the account`() {
        assertEquals(ProtectStepDone(), protectStepDone(ProtectStepDone(), null))
        assertEquals(
            ProtectStepDone(passkey = true),
            protectStepDone(ProtectStepDone(passkey = true), status(enabled = false, vault = false, phraseSaved = false)),
        )
        // The email is done when `enabled && (vault || phraseSaved)`.
        assertTrue(protectStepDone(ProtectStepDone(), status(enabled = true, vault = true, phraseSaved = false)).email)
        assertTrue(protectStepDone(ProtectStepDone(), status(enabled = true, vault = false, phraseSaved = true)).email)
        assertFalse(protectStepDone(ProtectStepDone(), status(enabled = true, vault = false, phraseSaved = false)).email)
        assertFalse(protectStepDone(ProtectStepDone(), status(enabled = false, vault = false, phraseSaved = true)).email)
        assertFalse(protectStepDone(ProtectStepDone(), null).email)
        // The passkey: created here, or the account already has one.
        assertTrue(protectStepDone(ProtectStepDone(), null, hasPasskey = true).passkey)
        assertFalse(protectStepDone(ProtectStepDone(), null, hasPasskey = false).passkey)
        assertTrue(protectStepDone(ProtectStepDone(), status(enabled = false, vault = false, phraseSaved = true)).phrase)
    }

    @Test
    fun `once a row is done Not now becomes Continue`() {
        assertEquals(ProtectStepExit.NotNow, protectStepExit(ProtectStepDone()))
        assertEquals(ProtectStepExit.Continue, protectStepExit(ProtectStepDone(email = true)))
        assertEquals(ProtectStepExit.Continue, protectStepExit(ProtectStepDone(passkey = true)))
        assertEquals(ProtectStepExit.Continue, protectStepExit(ProtectStepDone(phrase = true)))
    }

    @Test
    fun `an account with a passkey gets no banner, now or after 30 days`() {
        val day = 24L * 60L * 60L * 1000L
        val now = 1_000L * day
        assertTrue(protectBannerVisible(AccountProtection.None, null, now, hasPasskey = false))
        assertFalse(protectBannerVisible(AccountProtection.None, null, now, hasPasskey = true))
        assertFalse(protectBannerVisible(AccountProtection.None, now - 31 * day, now, hasPasskey = true))
        // Without a passkey the 30-day return still holds.
        assertTrue(protectBannerVisible(AccountProtection.None, now - 31 * day, now, hasPasskey = false))
    }

    @Test
    fun `the banner shows for an unprotected account and returns after 30 days`() {
        val day = 24L * 60L * 60L * 1000L
        val now = 1_000L * day
        assertTrue(protectBannerVisible(AccountProtection.None, dismissedAtMs = null, nowMs = now))
        assertFalse(protectBannerVisible(AccountProtection.None, dismissedAtMs = now - day, nowMs = now))
        assertFalse(protectBannerVisible(AccountProtection.None, dismissedAtMs = now - 29 * day, nowMs = now))
        assertTrue(protectBannerVisible(AccountProtection.None, dismissedAtMs = now - 30 * day, nowMs = now))
        // A clock set back never hides it longer.
        assertTrue(protectBannerVisible(AccountProtection.None, dismissedAtMs = now + day, nowMs = now))
        // Never for a protected account.
        assertFalse(protectBannerVisible(AccountProtection.Email, dismissedAtMs = null, nowMs = now))
        assertFalse(protectBannerVisible(AccountProtection.Phrase, dismissedAtMs = null, nowMs = now))
    }

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

    @Test
    fun `the store keeps flags per account and never an address`() {
        val storage = MapStorage()
        val store = AccountProtectionStore(storage)
        assertFalse(store.phraseSaved("a"))
        assertNull(store.bannerDismissedAtMs("a"))
        assertFalse(store.protectStepOwed("a"))
        store.setPhraseSaved("a", true)
        store.dismissBanner("a", 42L)
        store.setProtectStepOwed("a", true)
        store.setPhraseSaved("b", true)
        assertTrue(AccountProtectionStore(storage).phraseSaved("a"))
        assertEquals(42L, store.bannerDismissedAtMs("a"))
        assertTrue(store.protectStepOwed("a"))
        store.setProtectStepOwed("a", false)
        assertFalse(store.protectStepOwed("a"))
        store.clearAccount("a")
        assertFalse(store.phraseSaved("a"))
        assertNull(store.bannerDismissedAtMs("a"))
        assertTrue("another account's rows stay", store.phraseSaved("b"))
        // Only flags and times were ever written.
        assertTrue(storage.rows.values.all { it == "1" || it.toLongOrNull() != null })
    }
}
