// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0074 section 0.4 — what a landing sends as its device, and what is
 * done about the device a landed session names. Pure rules; no server and
 * no native core.
 */
class DeviceBindingTest {

    private val own = ByteArray(32) { (it + 1).toByte() }
    private val other = ByteArray(32) { (it + 101).toByte() }

    private fun token(deviceSegment: String) = "v3.AAAAAAAAAAAAAAAAAAAAAA.1.$deviceSegment.10.20.tag"

    // ---- what a landing sends ----------------------------------------------

    @Test
    fun `a landing whose account is known beforehand names this device`() {
        val asked = mutableListOf<String>()
        val landing = landingDeviceFor("acct-A") {
            asked += it
            own
        }
        landing as LandingDevice.Named
        assertEquals("acct-A", landing.accountIdB64)
        assertTrue(landing.deviceId.contentEquals(own))
        assertEquals(listOf("acct-A"), asked)
    }

    @Test
    fun `a landing that cannot know its account sends nothing and binds afterwards`() {
        var asked = 0
        val reader: (String) -> ByteArray? = {
            asked += 1
            own
        }
        assertSame(LandingDevice.BindAfter, landingDeviceFor(null, reader))
        assertSame(LandingDevice.BindAfter, landingDeviceFor("  ", reader))
        // No device seed is minted for an account nobody named.
        assertEquals(0, asked)
    }

    @Test
    fun `an unreadable, short or all-zero device id is never sent`() {
        assertSame(LandingDevice.BindAfter, landingDeviceFor("acct-A") { null })
        assertSame(LandingDevice.BindAfter, landingDeviceFor("acct-A") { ByteArray(31) { 1 } })
        // All zero means "no device" to the server: it must not be asserted.
        assertSame(LandingDevice.BindAfter, landingDeviceFor("acct-A") { ByteArray(32) })
    }

    @Test
    fun `a sign-in by name expects the account this device holds a record for under that name`() {
        val record = PersistedSession(accountIdB64 = "acct-A", username = "alice")
        assertEquals("acct-A", candidateAccountForName("alice", record))
        assertEquals("acct-A", candidateAccountForName("  @Alice ", record))
        // Another name, no record, an empty name: nothing is guessed.
        assertNull(candidateAccountForName("bob", record))
        assertNull(candidateAccountForName("alice", null))
        assertNull(candidateAccountForName("", PersistedSession("acct-G", "")))
    }

    // ---- what is done about the device a session names ----------------------

    @Test
    fun `a session that names this device needs nothing`() {
        assertEquals(DeviceBindingAction.Bound, deviceBindingActionFor(token(base64UrlEncodeNoPad(own)), own))
    }

    @Test
    fun `a session that names no device is bound`() {
        assertEquals(DeviceBindingAction.Bind, deviceBindingActionFor(token(UNBOUND_DEVICE_SEGMENT), own))
        assertEquals(DeviceBindingAction.Bind, deviceBindingActionFor(token(base64UrlEncodeNoPad(ByteArray(32))), own))
    }

    @Test
    fun `a session that names another device cannot be used by this one`() {
        assertEquals(DeviceBindingAction.Foreign, deviceBindingActionFor(token(base64UrlEncodeNoPad(other)), own))
    }

    @Test
    fun `nothing is decided on a string that is not a v3 access token`() {
        for (junk in listOf("", "t", "v2.A.1.2.tag", "r2.A.1.${UNBOUND_DEVICE_SEGMENT}.1.2.tag", "k1.A.1.2.tag")) {
            assertEquals(junk, DeviceBindingAction.Unknown, deviceBindingActionFor(junk, own))
        }
    }

    // ---- what bind-device answered ------------------------------------------

    @Test
    fun `the bind's answers are told apart`() {
        val pair = SessionPair("a", 1L, "r", 2L, 1L)
        assertSame(pair, (bindDeviceOutcome(pair, null) as BindDeviceOutcome.Bound).pair)
        assertEquals(
            BindDeviceOutcome.AlreadyBoundElsewhere,
            bindDeviceOutcome(null, AuthHttpError(409, "/auth/session/bind-device", "", code = "device-already-bound")),
        )
        assertEquals(
            BindDeviceOutcome.Revoked,
            bindDeviceOutcome(null, AuthHttpError(401, "/auth/session/bind-device", "", code = "device-revoked")),
        )
        assertEquals(
            BindDeviceOutcome.Superseded,
            bindDeviceOutcome(null, AuthHttpError(401, "/auth/session/bind-device", "", code = "key-generation-superseded")),
        )
    }

    @Test
    fun `everything else leaves the session unbound for the next landing to retry`() {
        val retried = listOf(
            AuthNetworkError("/auth/session/bind-device", java.io.IOException("offline")),
            AuthHttpError(401, "/auth/session/bind-device", "", code = "refresh-invalid"),
            AuthHttpError(400, "/auth/session/bind-device", ""),
            AuthHttpError(429, "/auth/session/bind-device", ""),
            AuthHttpError(503, "/auth/session/bind-device", ""),
            // A 409 with another code is not "bound elsewhere".
            AuthHttpError(409, "/auth/session/bind-device", "", code = "something-else"),
        )
        for (err in retried) assertTrue(err.toString(), bindDeviceOutcome(null, err) is BindDeviceOutcome.Retry)
        assertTrue(bindDeviceOutcome(null, null) is BindDeviceOutcome.Retry)
    }
}
