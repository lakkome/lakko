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
 * alongside its durable `distributed` flag and its ADR-0044 mint epoch.
 *
 * The `EncryptedSharedPreferences` half needs the Android framework (and
 * `android.util.Base64`), so it stays out of the host JVM; the codec is
 * deliberately framework-free so the two rules that govern an UPGRADE are
 * pinned here rather than only in an emulator run:
 *
 *   - a row lacking the flag reads as already **distributed** (else every
 *     working group re-broadcasts on the first launch after an upgrade);
 *   - a row lacking the epoch reads as **0**, the LOWEST position in the
 *     ADR-0044 `(mintEpoch, secretBytes)` order, so any authorised
 *     stamped mint supersedes a legacy secret. That is the direction that
 *     converges — guessing "current" would let a legacy secret out-rank a
 *     live rotation, which is the F-A shape itself.
 */
class PushRoutingSecretStoreTest {

    /** Stands in for a base64 secret body; the codec never decodes it. */
    private val body = "QUJDREVGRw=="

    @Test
    fun `a LEGACY value with no suffix reads as DISTRIBUTED at epoch 0`() {
        // Rows written before the flag existed are the whole reason this
        // rule exists: those groups are provisioned and working, so
        // reading them as pending would re-broadcast every group on the
        // first launch after an upgrade, for no benefit.
        val parsed = PushRoutingSecretStore.parseValue(body)
        assertEquals(body, parsed?.encodedSecret)
        assertTrue("no suffix == legacy == already distributed", parsed!!.distributed)
        assertEquals(
            "no epoch field == the lowest position in the order",
            PushRoutingSecretStore.LEGACY_MINT_EPOCH,
            parsed.mintEpoch,
        )
    }

    @Test
    fun `an ADR-0042 row (flag, no epoch) keeps its flag and reads epoch 0`() {
        // The intermediate generation: the ADR-0042 / ADR-0043 flag landed,
        // the ADR-0044 stamp had not. Both flag values must survive, and
        // both must read as the epoch floor so a stamped mint supersedes
        // them.
        for (distributed in listOf(true, false)) {
            val raw = body + ":" + (if (distributed) "1" else "0")
            val parsed = PushRoutingSecretStore.parseValue(raw)
            assertEquals("secret body survives", body, parsed?.encodedSecret)
            assertEquals("flag survives", distributed, parsed?.distributed)
            assertEquals(
                "a row with no stamp is at the floor",
                PushRoutingSecretStore.LEGACY_MINT_EPOCH,
                parsed?.mintEpoch,
            )
        }
    }

    @Test
    fun `a full value round-trips through format and parse`() {
        for (distributed in listOf(true, false)) {
            for (epoch in listOf(0uL, 1uL, 7uL, ULong.MAX_VALUE)) {
                val raw = PushRoutingSecretStore.formatValue(body, distributed, epoch)
                val parsed = PushRoutingSecretStore.parseValue(raw)
                assertEquals("secret body survives", body, parsed?.encodedSecret)
                assertEquals("flag survives", distributed, parsed?.distributed)
                assertEquals("epoch survives", epoch, parsed?.mintEpoch)
            }
        }
    }

    @Test
    fun `the epoch survives the FULL u64 range (no Long truncation)`() {
        // Kotlin has no unsigned primitive on the JVM, so an epoch past
        // Long.MAX_VALUE is the case a `toLong()` shortcut silently
        // corrupts — and it is exactly the value an attacker would stamp
        // to attempt the permanent freeze the clamp exists to refuse.
        val past = ULong.MAX_VALUE
        val raw = PushRoutingSecretStore.formatValue(body, distributed = true, mintEpoch = past)
        assertEquals("18446744073709551615", raw.substringAfterLast(':'))
        assertEquals(past, PushRoutingSecretStore.parseValue(raw)?.mintEpoch)
    }

    @Test
    fun `the two flag renderings are distinct and unambiguous`() {
        val pending = PushRoutingSecretStore.formatValue(body, distributed = false, mintEpoch = 3uL)
        val done = PushRoutingSecretStore.formatValue(body, distributed = true, mintEpoch = 3uL)
        assertFalse("pending and distributed must not collide", pending == done)
        assertFalse(
            "pending parses as pending",
            PushRoutingSecretStore.parseValue(pending)!!.distributed,
        )
        assertTrue(
            "distributed parses as distributed",
            PushRoutingSecretStore.parseValue(done)!!.distributed,
        )
        // The separator is absent from the base64 alphabet AND from a
        // decimal integer, so the split can never bite into either field.
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
        assertNull(PushRoutingSecretStore.parseValue("$body:2:5"))
    }

    @Test
    fun `a MALFORMED epoch field is refused, never defaulted`() {
        // Defaulting a corrupt stamp to 0 would silently demote a live
        // secret to the bottom of the order, letting anything supersede
        // it; defaulting to "current" would do the inverse. Neither is a
        // guess worth making, so the whole row is refused — the same
        // fail-closed posture as an undecodable base64 body.
        assertNull("not a number", PushRoutingSecretStore.parseValue("$body:1:x"))
        assertNull("empty", PushRoutingSecretStore.parseValue("$body:1:"))
        assertNull("signed", PushRoutingSecretStore.parseValue("$body:1:-1"))
        assertNull("hex", PushRoutingSecretStore.parseValue("$body:1:0x10"))
        assertNull(
            "past u64::MAX",
            PushRoutingSecretStore.parseValue("$body:1:18446744073709551616"),
        )
        assertNull("a fourth field", PushRoutingSecretStore.parseValue("$body:1:0:2"))
    }

    @Test
    fun `an empty legacy value still parses (the base64 decode is what rejects it)`() {
        // The codec's job is only the split; length / base64 validity is
        // enforced downstream, so an empty body must not crash the parse.
        val parsed = PushRoutingSecretStore.parseValue("")
        assertEquals("", parsed?.encodedSecret)
        assertTrue(parsed!!.distributed)
        assertEquals(PushRoutingSecretStore.LEGACY_MINT_EPOCH, parsed.mintEpoch)
    }

    @Test
    fun `Entry equality is by CONTENT and includes the stamp`() {
        // The stamp is what the ADR-0044 order is decided on, so two rows
        // holding the same secret at different epochs are NOT the same row.
        val secret = ByteArray(32) { 0x7f }
        assertEquals(
            PushRoutingSecretStore.Entry(secret.copyOf(), distributed = true, mintEpoch = 4uL),
            PushRoutingSecretStore.Entry(secret, distributed = true, mintEpoch = 4uL),
        )
        assertFalse(
            "a different stamp is a different row",
            PushRoutingSecretStore.Entry(secret, distributed = true, mintEpoch = 4uL) ==
                PushRoutingSecretStore.Entry(secret, distributed = true, mintEpoch = 5uL),
        )
    }
}
