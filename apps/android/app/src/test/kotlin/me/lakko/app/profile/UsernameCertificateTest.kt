// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import java.util.Base64
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0060 section 5 — a claimed username is shown only once its certificate
 * verifies for the OPRF of the username and the claimed account, AND that
 * account's published key is the sender's (a copied certificate fails).
 */
class UsernameCertificateTest {

    private val sender = ByteArray(32) { 7 }
    private val senderB64 = Base64.getEncoder().encodeToString(sender)
    private val account = ByteArray(16) { 3 }
    private val accountHex = account.joinToString("") { "%02x".format(it) }
    private val certHex = "ab".repeat(153)

    private suspend fun verify(
        binds: Boolean = true,
        published: ByteArray? = sender,
        haveKey: Boolean = true,
        certificateHex: String = certHex,
        accountIdHex: String = accountHex,
        oprfFails: Boolean = false,
    ): UsernameResolution = verifyUsernameByCertificate(
        username = "alice",
        certificateHex = certificateHex,
        accountIdHex = accountIdHex,
        senderKeyB64 = senderB64,
        oprf = { if (oprfFails) throw IllegalStateException("offline") else ByteArray(64) },
        verifyBinding = if (haveKey) {
            { _, _, _ -> binds }
        } else {
            null
        },
        fetchSignatureKey = { published },
    )

    @Test
    fun `a certificate that binds and the sender's own account verify`() = runTest {
        assertEquals(UsernameResolution.Found(senderB64), verify())
    }

    @Test
    fun `a certificate that does not bind is refused`() = runTest {
        assertEquals(UsernameResolution.NotRegistered, verify(binds = false))
    }

    @Test
    fun `a certificate copied from another account is refused`() = runTest {
        assertEquals(UsernameResolution.NotRegistered, verify(published = ByteArray(32) { 9 }))
        assertEquals(UsernameResolution.NotRegistered, verify(published = null))
    }

    @Test
    fun `malformed inputs are refused, not retried`() = runTest {
        assertEquals(UsernameResolution.NotRegistered, verify(certificateHex = "zz"))
        assertEquals(UsernameResolution.NotRegistered, verify(accountIdHex = "0102"))
    }

    @Test
    fun `no verifying key or no network is no conclusion`() = runTest {
        assertTrue(verify(haveKey = false) is UsernameResolution.Unavailable)
        assertTrue(verify(oprfFails = true) is UsernameResolution.Unavailable)
    }
}
