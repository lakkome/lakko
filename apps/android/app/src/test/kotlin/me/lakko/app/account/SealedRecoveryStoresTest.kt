// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.account

import me.lakko.app.auth.AccountNoticeKind
import me.lakko.app.auth.AccountNoticeStore
import me.lakko.app.auth.AccountProtectionStore
import me.lakko.app.auth.KeyResetOwed
import me.lakko.app.auth.KeyResetOwedStore
import me.lakko.app.auth.LogoutOwed
import me.lakko.app.auth.LogoutOwedStore
import me.lakko.app.auth.MarkerStorage
import me.lakko.app.auth.OwedPhraseOrigin
import me.lakko.app.auth.RecallBlobStorage
import me.lakko.app.auth.RecallRecord
import me.lakko.app.auth.RecallRecordStore
import me.lakko.app.auth.RecoveryOwedStore
import me.lakko.app.auth.ThreeWordsBudgetStore
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/**
 * The account-recovery stores behind [AccountStoreGate], the way
 * `SealedAccountStoresTest` pins the chat and push stores: a flow that
 * outlives a sign-out or a wipe gets to its write late, and that write must
 * not put a row back.
 *
 * Two rules. The stores of a live session (what is owed, how the account is
 * protected, its notices, an owed key reset) refuse a write for a sealed
 * account. The three rows that are written for a signed-OUT account by
 * design (the three-word guess budget, the logout the server was not told
 * about, the recall record) take a write while the account is only sealed
 * and refuse it once the account's data was wiped.
 */
class SealedRecoveryStoresTest {

    private class Memory : MarkerStorage {
        val rows = HashMap<String, String>()

        override fun get(key: String): String? = rows[key]

        override fun set(key: String, value: String) {
            rows[key] = value
        }

        override fun remove(key: String) {
            rows.remove(key)
        }
    }

    private class Blobs : RecallBlobStorage {
        val rows = HashMap<String, ByteArray>()

        override fun list(): List<String> = rows.keys.toList()

        override fun read(accountIdHex: String): ByteArray? = rows[accountIdHex]?.copyOf()

        override fun write(accountIdHex: String, plaintext: ByteArray) {
            rows[accountIdHex] = plaintext.copyOf()
        }

        override fun delete(accountIdHex: String) {
            rows.remove(accountIdHex)
        }
    }

    private val accountId = ByteArray(16) { 0x0B }
    private val account = me.lakko.app.auth.base64Encode(accountId)
    private val owedReset = KeyResetOwed(
        attestationB64 = "AA==",
        keyGeneration = 2L,
        signedAtMs = 1L,
        newSignaturePubkeyB64 = "AA==",
    )

    private fun record() = RecallRecord(
        accountId = accountId.copyOf(),
        oldAccountSeed = ByteArray(32) { 7 },
        username = "alice",
        learnedAtMs = 1L,
    )

    @Before
    fun setUp() = AccountStoreGate.resetForTest()

    @After
    fun tearDown() = AccountStoreGate.resetForTest()

    @Test
    fun `nothing of a session's recovery state is written back after the wipe`() {
        val owed = RecoveryOwedStore(Memory())
        val protection = AccountProtectionStore(Memory())
        val notices = AccountNoticeStore(Memory())
        val resets = KeyResetOwedStore(Memory())
        owed.markPhraseOwed(account, OwedPhraseOrigin.FreshIdentity)
        protection.setProtectStepOwed(account, true)
        notices.setCursor(account, 5L)
        resets.save(account, owedReset)

        // The wipe, as `AuthRepository.forgetDevice` runs it.
        AccountStoreGate.seal(account) {
            AccountStoreGate.markWiped(account)
            owed.clearAccount(account)
            protection.clearAccount(account)
            notices.clearAccount(account)
            resets.clear(account)
        }
        // Writers of the forgotten session that get to their write only now.
        owed.markPhraseOwed(account, OwedPhraseOrigin.FreshIdentity)
        owed.markPublishOwed(account)
        protection.setProtectStepOwed(account, true)
        protection.setPhraseSaved(account, true)
        notices.setCursor(account, 9L)
        notices.markOwn(account, AccountNoticeKind.EmailAdded, 1L)
        resets.save(account, owedReset)

        assertFalse(owed.isPhraseOwed(account))
        assertFalse(owed.isPublishOwed(account))
        assertFalse(protection.protectStepOwed(account))
        assertFalse(protection.phraseSaved(account))
        assertNull(notices.cursor(account))
        assertTrue(notices.ownActions(account).isEmpty())
        assertNull(resets.load(account))
    }

    @Test
    fun `a sign-out deletes nothing and stops later writes of the session's stores`() {
        val owed = RecoveryOwedStore(Memory())
        val notices = AccountNoticeStore(Memory())
        owed.markPhraseOwed(account, OwedPhraseOrigin.FreshIdentity)
        notices.setCursor(account, 5L)

        AccountStoreGate.seal(account)
        owed.markPublishOwed(account)
        notices.setCursor(account, 9L)

        assertTrue(owed.isPhraseOwed(account))
        assertFalse(owed.isPublishOwed(account))
        assertEquals(5L, notices.cursor(account))
    }

    @Test
    fun `the account's next landing writes its recovery stores again`() {
        val protection = AccountProtectionStore(Memory())
        AccountStoreGate.seal(account) { AccountStoreGate.markWiped(account) }
        protection.setProtectStepOwed(account, true)
        assertFalse(protection.protectStepOwed(account))

        AccountStoreGate.open(account)
        protection.setProtectStepOwed(account, true)

        assertTrue(protection.protectStepOwed(account))
        assertFalse(AccountStoreGate.isWiped(account))
    }

    @Test
    fun `the rows of a signed-out account are still written, and refused once it was wiped`() {
        val budgets = ThreeWordsBudgetStore(Memory())
        val logouts = LogoutOwedStore(Memory())
        val owedLogout = LogoutOwed("access", 1L, "refresh", 2L)

        // Signed out: the guess budget of the three-word route counts, and
        // the logout the server was not told about is kept for the retry.
        AccountStoreGate.seal(account)
        budgets.suspend(account)
        logouts.save(account, owedLogout)
        assertTrue(budgets.isSuspended(account))
        assertNotNull(logouts.load(account))

        // Wiped: a late write puts neither back.
        AccountStoreGate.seal(account) {
            AccountStoreGate.markWiped(account)
            budgets.clearSuspended(account)
            logouts.clear(account)
        }
        budgets.suspend(account)
        logouts.save(account, owedLogout)

        assertFalse(budgets.isSuspended(account))
        assertNull(logouts.load(account))
        assertTrue(logouts.accounts().isEmpty())
    }

    @Test
    fun `the recall record is written for a sealed account and refused after the wipe`() {
        // The staged wipe: the record is written BEFORE the wipe starts and
        // the wipe keeps it (ADR-0074 section 1.1).
        val kept = Blobs()
        val store = RecallRecordStore(kept)
        AccountStoreGate.seal(account)
        assertTrue(store.save(record()))
        assertNotNull(store.load(account))

        // Once the account's data was wiped, a late save puts no seed back:
        // the caller is told the record was not stored.
        AccountStoreGate.seal(account) {
            AccountStoreGate.markWiped(account)
            store.delete(account)
        }
        assertFalse(store.save(record()))
        assertTrue(kept.rows.isEmpty())
    }
}
