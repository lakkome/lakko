// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.search

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiIntroductionPayload
import uniffi.lakko_ffi_uniffi.FfiIntroductionsBundle
import uniffi.lakko_ffi_uniffi.FfiReachClass
import uniffi.lakko_ffi_uniffi.FfiReachGrant

/** ADR-0060 section 4 — opening kept introduction records, over a fake core
 *  whose tags are `"tag:" + output` and whose records open under one output. */
class IntroductionsTest {

    private val fof = FfiReachGrant(FfiReachClass.FOF, 1u, ByteArray(32) { 7 }, ByteArray(97) { 8 })

    private fun payload(withGrant: Boolean = true) = FfiIntroductionPayload(
        identityPk = ByteArray(32) { 1 },
        accountId = ByteArray(16) { 2 },
        displayName = "Anna Weber",
        username = "anna",
        usernameCertificate = null,
        fof = if (withGrant) fof else null,
        mailboxBinding = ByteArray(97) { 8 },
    )

    /** record name -> (the one output it opens under, what it opens to, or a
     *  forgery that fails to authenticate). */
    private class Rec(val output: String, val payload: FfiIntroductionPayload?, val forged: Boolean = false)

    private fun core(records: Map<String, Rec>, bundles: Map<String, List<String>> = emptyMap()) =
        object : IntroductionCore {
            override fun introductionTags(record: ByteArray): List<ByteArray> {
                val r = records[String(record)] ?: throw IllegalArgumentException("malformed")
                return listOf("tag:other".toByteArray(), "tag:${r.output}".toByteArray())
            }

            override fun fofTag(oprfOutput: ByteArray): ByteArray = "tag:${String(oprfOutput)}".toByteArray()

            override fun openIntroduction(record: ByteArray, oprfOutput: ByteArray): FfiIntroductionPayload? {
                val r = records.getValue(String(record))
                if (r.forged) throw IllegalStateException("aead")
                return if (String(oprfOutput) == r.output) r.payload else null
            }

            override fun decodeIntroductionsBundle(plaintext: ByteArray): FfiIntroductionsBundle {
                val names = bundles[String(plaintext)] ?: throw IllegalArgumentException("malformed")
                return FfiIntroductionsBundle(5uL, names.map { it.toByteArray() })
            }

            override fun encodeReachGrant(grant: FfiReachGrant): ByteArray = "grant:${grant.version}".toByteArray()
        }

    @Test
    fun `a record opens only under a probe whose tag it carries`() {
        val c = core(mapOf("r1" to Rec("anna", payload()), "r2" to Rec("bob", payload())))
        val opened = openMatchingIntroductions(
            listOf("r1".toByteArray(), "r2".toByteArray()),
            listOf(IntroductionProbe("anna".toByteArray(), ProbeVia.TERM)),
            c,
        )
        val one = opened.single()
        assertEquals(ProbeVia.TERM, one.via)
        assertEquals("Anna Weber", one.displayName)
        assertArrayEquals("grant:1".toByteArray(), one.fofWire)
    }

    @Test
    fun `a forgery, a record without a grant and a malformed record open nothing`() {
        val c = core(
            mapOf(
                "forged" to Rec("anna", payload(), forged = true),
                "public" to Rec("anna", payload(withGrant = false)),
            ),
        )
        val opened = openMatchingIntroductions(
            listOf("forged".toByteArray(), "public".toByteArray(), "garbage".toByteArray()),
            listOf(IntroductionProbe("anna".toByteArray(), ProbeVia.USERNAME)),
            c,
        )
        assertTrue(opened.isEmpty())
    }

    @Test
    fun `records are every bundle's records plus the offers, a broken bundle adding none`() {
        val c = core(emptyMap(), bundles = mapOf("b1" to listOf("x", "y")))
        val records = recordsOf(
            bundleWires = listOf("b1".toByteArray(), "broken".toByteArray()),
            offeredRecords = listOf("o".toByteArray()),
            core = c,
        )
        assertEquals(listOf("x", "y", "o"), records.map { String(it) })
    }
}
