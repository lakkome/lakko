// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host-side unit tests for the PURE half of [PushRoutingSecretStore] —
 * the row-value codec that carries the ADR-0042 `push_routing_secret`
 * alongside its durable `distributed` flag.
 *
 * The `EncryptedSharedPreferences` half needs the Android framework (and
 * `android.util.Base64`), so it stays out of the host JVM; the codec is
 * deliberately framework-free so the one rule that governs an UPGRADE —
 * a legacy row lacking the flag reads as already distributed — is pinned
 * here rather than only in an emulator run.
 */
class PushRoutingSecretStoreTest {

    /** Stands in for a base64 secret body; the codec never decodes it. */
    private val body = "QUJDREVGRw=="

    @Test
    fun `a LEGACY value with no flag suffix reads as DISTRIBUTED`() {
        // Rows written before the flag existed are the whole reason this
        // rule exists: those groups are provisioned and working, so
        // reading them as pending would re-broadcast every group on the
        // first launch after an upgrade, for no benefit.
        val parsed = PushRoutingSecretStore.parseValue(body)
        assertEquals(body, parsed?.encodedSecret)
        assertTrue("no suffix == legacy == already distributed", parsed!!.distributed)
    }

    @Test
    fun `a flagged value round-trips through format and parse`() {
        for (distributed in listOf(true, false)) {
            val raw = PushRoutingSecretStore.formatValue(body, distributed)
            val parsed = PushRoutingSecretStore.parseValue(raw)
            assertEquals("secret body survives", body, parsed?.encodedSecret)
            assertEquals("flag survives", distributed, parsed?.distributed)
        }
    }

    @Test
    fun `the two flag renderings are distinct and unambiguous`() {
        val pending = PushRoutingSecretStore.formatValue(body, distributed = false)
        val done = PushRoutingSecretStore.formatValue(body, distributed = true)
        assertFalse("pending and distributed must not collide", pending == done)
        assertFalse("pending parses as pending", PushRoutingSecretStore.parseValue(pending)!!.distributed)
        assertTrue("distributed parses as distributed", PushRoutingSecretStore.parseValue(done)!!.distributed)
        // The separator is absent from the base64 alphabet, so the split
        // can never bite into the secret body.
        assertFalse("base64 body carries no separator", body.contains(':'))
    }

    @Test
    fun `an absent row parses to null`() {
        assertNull(PushRoutingSecretStore.parseValue(null))
    }

    @Test
    fun `an unrecognised flag suffix is MALFORMED, not silently pending`() {
        // This client is the only writer, so anything else is corruption.
        // Fail closed to `null` (the same posture as an undecodable base64
        // body) rather than inventing a flag value.
        assertNull(PushRoutingSecretStore.parseValue("$body:"))
        assertNull(PushRoutingSecretStore.parseValue("$body:2"))
        assertNull(PushRoutingSecretStore.parseValue("$body:11"))
        assertNull(PushRoutingSecretStore.parseValue("$body:true"))
        assertNull(PushRoutingSecretStore.parseValue("$body:1:0"))
    }

    @Test
    fun `an empty legacy value still parses (the base64 decode is what rejects it)`() {
        // The codec's job is only the split; length / base64 validity is
        // enforced downstream, so an empty body must not crash the parse.
        val parsed = PushRoutingSecretStore.parseValue("")
        assertEquals("", parsed?.encodedSecret)
        assertTrue(parsed!!.distributed)
    }
}
