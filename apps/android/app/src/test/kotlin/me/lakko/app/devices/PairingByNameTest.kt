// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import kotlinx.coroutines.test.runTest
import me.lakko.app.auth.AuthClientArgError
import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.DeviceAuthorizeRedeemResult
import me.lakko.app.auth.LoginHandleSource
import me.lakko.app.auth.NamedHandle
import me.lakko.app.auth.base64Encode
import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.ui.auth.DeviceRestoreTarget
import me.lakko.app.ui.auth.deviceRestoreCompletion
import me.lakko.app.ui.auth.deviceRestoreTargetFor
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.DecodedPairingQr
import uniffi.lakko_ffi_uniffi.FfiException

/**
 * ADR-0056 — a fresh device names its account by the login handle of the
 * username the person types; nothing before a session carries the account id.
 * Host JVM: the native core is replaced by fakes.
 */
class PairingByNameTest {

    /** Fake core: the handle is the name's bytes, padded to 32; a name with
     *  a space fails the username rule; every call is recorded. */
    private class FakeSource : LoginHandleSource {
        val asked = mutableListOf<String>()
        override fun handleFor(username: String): NamedHandle {
            asked += username
            if (username.contains(' ')) throw FfiException.UsernameInvalidCharacters("bad")
            val canonical = username.trim().lowercase()
            return NamedHandle(canonical, handleOf(canonical))
        }
    }

    private fun decoded() = DecodedPairingQr(
        keyPackage = ByteArray(8) { 7 },
        rendezvousPseudonym = ByteArray(32) { 9 },
    )

    // ---------------------------------------------------------------
    // Fresh device: the payload carries the handle of the typed name
    // ---------------------------------------------------------------

    @Test
    fun theRestoreTargetCarriesTheHandleOfTheTypedNameAndNoAccountId() {
        val source = FakeSource()
        val target = deviceRestoreTargetFor("Alice", cachedUsername = "bob", source = source)
        assertEquals("alice", target.username)
        assertArrayEquals(handleOf("alice"), target.loginHandle)
        // The handle source is the only thing consulted: no lookup.
        assertEquals(listOf("Alice"), source.asked)
    }

    @Test
    fun anEmptyFieldFallsBackToTheCachedAccountsName() {
        val target = deviceRestoreTargetFor("  ", cachedUsername = "bob", source = FakeSource())
        assertArrayEquals(handleOf("bob"), target.loginHandle)
    }

    @Test
    fun noNameAtAllIsRefusedWithTheFormCopy() {
        val err = assertThrows(AuthClientArgError::class.java) {
            deviceRestoreTargetFor("", cachedUsername = null, source = FakeSource())
        }
        assertEquals(PAIRING_NO_NAME_COPY, err.message)
    }

    @Test
    fun aNameTheRuleRefusesShowsTheRulesCopy() {
        val err = assertThrows(AuthClientArgError::class.java) {
            deviceRestoreTargetFor("al ice", cachedUsername = null, source = FakeSource())
        }
        assertTrue(err.message.orEmpty().isNotEmpty())
        assertTrue(err.message != PAIRING_NO_NAME_COPY)
    }

    // ---------------------------------------------------------------
    // Confirming device: passes its own handle
    // ---------------------------------------------------------------

    @Test
    fun theConfirmingSideDecodesAgainstItsOwnHandle() {
        var passed: ByteArray? = null
        val expected = decoded()
        val decision = decodeForOwnAccount(
            scanned = byteArrayOf(1, 2, 3),
            ownUsername = "alice",
            source = FakeSource(),
            decode = { wire, handle ->
                assertArrayEquals(byteArrayOf(1, 2, 3), wire)
                passed = handle.copyOf()
                expected
            },
        )
        assertArrayEquals(handleOf("alice"), passed)
        assertTrue(decision is PairingScanDecision.Accepted)
        assertTrue((decision as PairingScanDecision.Accepted).decoded === expected)
    }

    @Test
    fun aHandleMismatchShowsTheWrongAccountCopy() {
        val decision = decodeForOwnAccount(
            scanned = ByteArray(4),
            ownUsername = "alice",
            source = FakeSource(),
            decode = { _, _ -> throw FfiException.Group("pairing: login_handle mismatch") },
        )
        assertEquals(PairingScanDecision.Refused(PAIRING_WRONG_ACCOUNT_COPY), decision)
    }

    @Test
    fun anUnsupportedVersionShowsTheUpdateCopy() {
        val decision = decodeForOwnAccount(
            scanned = ByteArray(4),
            ownUsername = "alice",
            source = FakeSource(),
            decode = { _, _ -> throw FfiException.Unsupported("pairing version 1") },
        )
        assertEquals(PairingScanDecision.Refused(PAIRING_UNSUPPORTED_COPY), decision)
    }

    @Test
    fun aMalformedPayloadShowsTheGenericFailure() {
        val decision = decodeForOwnAccount(
            scanned = ByteArray(4),
            ownUsername = "alice",
            source = FakeSource(),
            decode = { _, _ -> throw FfiException.Codec("bad protobuf") },
        )
        assertEquals(PairingScanDecision.Refused(PAIRING_FAILED_COPY), decision)
    }

    @Test
    fun aGuestCannotConfirmAndNothingIsDecoded() {
        var decodes = 0
        for (name in listOf("", "   ", null)) {
            val decision = decodeForOwnAccount(
                scanned = ByteArray(4),
                ownUsername = name,
                source = FakeSource(),
                decode = { _, _ ->
                    decodes += 1
                    decoded()
                },
            )
            assertEquals(PairingScanDecision.Refused(PAIRING_NO_USERNAME_COPY), decision)
        }
        assertEquals(0, decodes)
        assertNull(ownPairingHandle("", FakeSource()))
    }

    // ---------------------------------------------------------------
    // Redeem: the grant only; the account id comes from the response
    // ---------------------------------------------------------------

    @Test
    fun theRedeemSendsTheGrantOnlyAndTheAccountComesFromTheResponse() = runTest {
        val grant = ByteArray(32) { 5 }
        val accountId = ByteArray(16) { 3 }
        val sent = mutableListOf<String>()
        val result = completeJoinByName(
            joinedGroupId = groupIdOf(accountId),
            grantToken = grant,
            redeem = { grantB64 ->
                sent += grantB64
                DeviceAuthorizeRedeemResult(accountId, "tok", 99L)
            },
            groupIdFor = ::groupIdOf,
        )
        assertEquals(listOf(base64UrlEncodeNoPad(grant)), sent)
        assertTrue(result is JoinByNameResult.Joined)
        result as JoinByNameResult.Joined
        assertArrayEquals(accountId, result.accountId)
        assertEquals("tok", result.sessionToken)
        assertEquals(99L, result.sessionExpiresAtMs)
    }

    @Test
    fun aRedeemNamingAnotherAccountsGroupIsRefused() = runTest {
        val result = completeJoinByName(
            joinedGroupId = groupIdOf(ByteArray(16) { 1 }),
            grantToken = ByteArray(32),
            redeem = { DeviceAuthorizeRedeemResult(ByteArray(16) { 2 }, "tok", 1L) },
            groupIdFor = ::groupIdOf,
        )
        assertEquals(JoinByNameResult.WrongGroup, result)
    }

    @Test
    fun aFailedRedeemLeavesTheAccountUnknown() = runTest {
        val result = completeJoinByName(
            joinedGroupId = ByteArray(32),
            grantToken = ByteArray(32),
            redeem = { throw AuthHttpError(status = 401, endpoint = "/device/authorize/redeem", body = "") },
            groupIdFor = ::groupIdOf,
        )
        assertEquals(JoinByNameResult.RedeemFailed, result)
    }

    // ---------------------------------------------------------------
    // Completion: the session installs under the redeemed account id
    // ---------------------------------------------------------------

    @Test
    fun theRestoreCompletesUnderTheAccountTheRedeemNamed() {
        val accountId = ByteArray(16) { 4 }
        val target = DeviceRestoreTarget(username = "alice", loginHandle = handleOf("alice"))
        val done = deviceRestoreCompletion(
            target,
            PairingOrchestrator.JoinRecoveryOutcome(
                phraseEntropy = ByteArray(16) { 1 },
                sessionToken = "tok",
                sessionExpiresAtMs = 7L,
                accountId = accountId,
            ),
        )
        assertArrayEquals(accountId, done.accountIdBytes)
        assertEquals(base64Encode(accountId), done.accountIdB64)
        assertEquals("alice", done.username)
        assertEquals("tok", done.sessionToken)
    }

    @Test
    fun aKeysOnlyOutcomeIsRefusedAndTheEntropyZeroized() {
        val seed = ByteArray(16) { 1 }
        val target = DeviceRestoreTarget(username = "alice", loginHandle = handleOf("alice"))
        assertThrows(AuthClientArgError::class.java) {
            deviceRestoreCompletion(
                target,
                PairingOrchestrator.JoinRecoveryOutcome(seed, null, null, null),
            )
        }
        assertTrue(seed.all { it == 0.toByte() })
    }

    // ---------------------------------------------------------------
    // Ordering: nothing is persisted before the redeem names the account
    // and its DeviceGroup is the joined group
    // ---------------------------------------------------------------

    private class FakeSteps(
        private val joinedAccount: ByteArray,
        private val redeemedAccount: ByteArray?,
        private val row: Boolean = true,
    ) : JoinByNameSteps {
        val calls = mutableListOf<String>()
        val grant = ByteArray(RecoveryPayload.RECOVERY_GRANT_TOKEN_LENGTH) { 6 }
        val seed = ByteArray(RecoveryPayload.RECOVERY_PHRASE_ENTROPY_LENGTH) { 8 }
        var persistedAs: ByteArray? = null

        override suspend fun pollRecoveryRow(): ByteArray? {
            calls += "poll"
            return if (row) ByteArray(4) else null
        }

        override fun openRecoveryRow(sealed: ByteArray): RecoveryPayload.ParsedRecoveryPayload {
            calls += "open"
            return RecoveryPayload.ParsedRecoveryPayload(grant, seed)
        }

        override fun joinedGroupId(): ByteArray = groupIdOf(joinedAccount)

        override suspend fun redeem(grantTokenB64: String): DeviceAuthorizeRedeemResult {
            calls += "redeem:$grantTokenB64"
            val account = redeemedAccount
                ?: throw AuthHttpError(status = 401, endpoint = "/device/authorize/redeem", body = "")
            return DeviceAuthorizeRedeemResult(account, "tok", 5L)
        }

        override fun groupIdFor(accountId: ByteArray): ByteArray = groupIdOf(accountId)

        override suspend fun bindAs(accountId: ByteArray): PendingJoin {
            calls += "bind"
            return object : PendingJoin {
                override suspend fun persist() {
                    calls += "persist"
                    persistedAs = accountId
                }
            }
        }
    }

    @Test
    fun aJoinByNameWritesNothingItselfAndPersistsUnderTheRedeemedAccountOnlyWhenTold() = runTest {
        val account = ByteArray(16) { 3 }
        val steps = FakeSteps(joinedAccount = account, redeemedAccount = account)
        val grantB64 = base64UrlEncodeNoPad(steps.grant)
        val outcome = runJoinByName(steps)!!
        // ADR-0056 security pass: the join binds in memory; the restore
        // decides when (and whether) anything is written.
        assertEquals(listOf("poll", "open", "redeem:$grantB64", "bind"), steps.calls)
        assertNull(steps.persistedAs)
        outcome.persistJoin!!.invoke()
        assertEquals(listOf("poll", "open", "redeem:$grantB64", "bind", "persist"), steps.calls)
        assertArrayEquals(account, steps.persistedAs)
        assertArrayEquals(account, outcome.accountId)
        assertEquals("tok", outcome.sessionToken)
        assertTrue(outcome.phraseEntropy.any { it != 0.toByte() })
    }

    @Test
    fun aWelcomeForAnotherAccountIsRefusedAfterTheRedeemWithNothingPersisted() = runTest {
        val steps = FakeSteps(
            joinedAccount = ByteArray(16) { 1 },
            redeemedAccount = ByteArray(16) { 2 },
        )
        val err = assertThrows(JoinWrongAccount::class.java) {
            kotlinx.coroutines.runBlocking { runJoinByName(steps) }
        }
        assertEquals(JOIN_WRONG_ACCOUNT_COPY, err.message)
        assertTrue(steps.calls.none { it == "persist" || it == "bind" })
        assertTrue(steps.calls.any { it.startsWith("redeem:") })
        assertTrue(steps.seed.all { it == 0.toByte() })
        assertTrue(steps.grant.all { it == 0.toByte() })
    }

    @Test
    fun aFailedRedeemPersistsNothingAndCarriesNoAccount() = runTest {
        val steps = FakeSteps(joinedAccount = ByteArray(16) { 1 }, redeemedAccount = null)
        val outcome = runJoinByName(steps)!!
        assertTrue(steps.calls.none { it == "persist" || it == "bind" })
        assertNull(outcome.persistJoin)
        assertNull(outcome.accountId)
        assertNull(outcome.sessionToken)
    }

    @Test
    fun noRecoveryRowMeansNoRedeemAndNothingPersisted() = runTest {
        val steps = FakeSteps(joinedAccount = ByteArray(16), redeemedAccount = ByteArray(16), row = false)
        assertNull(runJoinByName(steps))
        assertEquals(listOf("poll"), steps.calls)
    }

    private companion object {
        fun handleOf(name: String): ByteArray = name.toByteArray().copyOf(32)

        fun groupIdOf(accountId: ByteArray): ByteArray = ByteArray(32) { accountId[it % 16] }
    }
}
