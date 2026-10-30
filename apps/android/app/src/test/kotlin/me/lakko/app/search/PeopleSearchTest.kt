// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.search

import java.util.Base64
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.OprfResult

/**
 * ADR-0060 section 5 / UX 6.1 — the network half of people search, over fake
 * lookups. An OPRF round's output and lookup key are distinct, recognisable
 * bytes, so the tests can tell which one reached discovery.
 */
class PeopleSearchTest {

    private fun output(tag: String) = "out:$tag".toByteArray()
    private fun lookup(tag: String) = "key:$tag".toByteArray()

    private val aliceAccount = ByteArray(16) { 1 }
    private val aliceKey = ByteArray(32) { 0xA1.toByte() }
    private val malloryKey = ByteArray(32) { 0x66 }

    private class Recorded {
        val usernameLookups = mutableListOf<ByteArray>()
        val nameLookups = mutableListOf<ByteArray>()
        val reachChecks = mutableListOf<String>()
        val verifiedWith = mutableListOf<ByteArray>()
        val counts = mutableListOf<SearchCounts>()
    }

    /** A configurable fake. Everything the test does not set answers
     *  "nothing". */
    private inner class Fake(
        val rec: Recorded = Recorded(),
        var usernameHit: LookupHit? = null,
        var nameHits: List<LookupHit> = emptyList(),
        var bundleKeys: Map<String, ByteArray?> = emptyMap(),
        var bundleThrows: Boolean = false,
        var known: Map<String, KnownPerson> = emptyMap(),
        var opened: Map<String, OpenedPublicRecord> = emptyMap(),
        var introductions: List<OpenedIntroduction> = emptyList(),
        var reachAnswer: Boolean = true,
        var reachThrows: Boolean = false,
        var certificateValid: Boolean = true,
        var nameLookupThrows: Throwable? = null,
    ) : PeopleSearchDeps {
        override fun canonicalUsername(typed: String): String? =
            typed.trim().trimStart('@').lowercase().takeIf { it.matches(Regex("[a-z0-9_]{3,32}")) }

        override fun queryTerm(query: String): String? =
            query.trim().lowercase().split(" ").maxByOrNull { it.length }?.takeIf { it.length >= 2 }

        override fun nameMatches(displayName: String, query: String): Boolean {
            val words = displayName.lowercase().split(" ")
            return query.lowercase().trim().split(" ").all { q -> words.any { it.startsWith(q) } }
        }

        override suspend fun lookupChannel(handle: String): String? = null

        override suspend fun usernameOprf(username: String) = OprfResult(output("u:$username"), lookup("u:$username"))

        override suspend fun accountOfUsernameLookupKey(lookupKey: ByteArray): LookupHit? {
            rec.usernameLookups += lookupKey
            return usernameHit
        }

        override suspend fun nameTermOprf(term: String) = OprfResult(output("n:$term"), lookup("n:$term"))

        override suspend fun accountsOfNameTermLookupKey(lookupKey: ByteArray): List<LookupHit> {
            rec.nameLookups += lookupKey
            nameLookupThrows?.let { throw it }
            return nameHits
        }

        override fun openPublicRecord(record: ByteArray, oprfOutput: ByteArray): OpenedPublicRecord? =
            opened[String(record) + "|" + String(oprfOutput)]

        override suspend fun identityKeyOf(accountId: ByteArray): ByteArray? {
            if (bundleThrows) throw IllegalStateException("offline")
            return bundleKeys[searchHex(accountId)]
        }

        override fun known(keyB64: String): KnownPerson = known[keyB64] ?: KnownPerson(null, null, false)

        override suspend fun openIntroductions(probes: List<IntroductionProbe>): List<OpenedIntroduction> =
            introductions.filter { i -> probes.any { it.via == i.via } }

        override suspend fun verifyUsername(certificate: ByteArray, usernameLookupKey: ByteArray, accountId: ByteArray): Boolean {
            rec.verifiedWith += usernameLookupKey
            return certificateValid
        }

        override suspend fun reachCheck(identityPk: ByteArray, grantWireB64: String): Boolean {
            rec.reachChecks += grantWireB64
            if (reachThrows) throw IllegalStateException("offline")
            return reachAnswer
        }

        override fun legOutcomeOf(error: Throwable): LegOutcome = LegOutcome.FAILED

        override val ownKeyHex: String? = null

        override fun noteSearch(counts: SearchCounts) {
            rec.counts += counts
        }
    }

    @Test
    fun `discovery is asked with the lookup key, never the OPRF output`() = runTest {
        val fake = Fake()
        searchPeople("anna", fake)
        assertEquals(1, fake.rec.usernameLookups.size)
        assertArrayEquals(lookup("u:anna"), fake.rec.usernameLookups.single())
        assertArrayEquals(lookup("n:anna"), fake.rec.nameLookups.single())
        for (sent in fake.rec.usernameLookups + fake.rec.nameLookups) {
            assertFalse(String(sent).startsWith("out:"))
        }
    }

    @Test
    fun `a name hit without an identity key is dropped`() = runTest {
        val fake = Fake(
            nameHits = listOf(LookupHit(aliceAccount, "rec".toByteArray())),
            opened = mapOf("rec|out:n:anna" to OpenedPublicRecord(aliceKey, aliceAccount, "Anna Weber")),
        )
        val outcome = searchPeople("anna", fake)
        assertTrue(outcome.people.isEmpty())
        assertEquals(PeopleSearchState.NOT_FOUND, outcome.state)
    }

    @Test
    fun `a name hit named by a matching public record is kept, opened with the output`() = runTest {
        val fake = Fake(
            nameHits = listOf(LookupHit(aliceAccount, "rec".toByteArray())),
            bundleKeys = mapOf(searchHex(aliceAccount) to aliceKey),
            opened = mapOf("rec|out:n:anna" to OpenedPublicRecord(aliceKey, aliceAccount, "Anna Weber")),
        )
        val p = searchPeople("anna", fake).people.single()
        assertEquals("Anna Weber", p.name)
        assertEquals(PersonReach.OPEN, p.reach)
        assertEquals(searchHex(aliceKey), p.keyHex)
    }

    @Test
    fun `a public record naming another identity key names nobody`() = runTest {
        val fake = Fake(
            nameHits = listOf(LookupHit(aliceAccount, "rec".toByteArray())),
            bundleKeys = mapOf(searchHex(aliceAccount) to aliceKey),
            opened = mapOf("rec|out:n:anna" to OpenedPublicRecord(malloryKey, aliceAccount, "Anna Weber")),
        )
        assertTrue(searchPeople("anna", fake).people.isEmpty())
    }

    @Test
    fun `a public record naming another account names nobody`() = runTest {
        val fake = Fake(
            nameHits = listOf(LookupHit(aliceAccount, "rec".toByteArray())),
            bundleKeys = mapOf(searchHex(aliceAccount) to aliceKey),
            opened = mapOf("rec|out:n:anna" to OpenedPublicRecord(aliceKey, ByteArray(16) { 9 }, "Anna Weber")),
        )
        assertTrue(searchPeople("anna", fake).people.isEmpty())
    }

    @Test
    fun `a profile held for the bundle key names a hit without a record`() = runTest {
        val fake = Fake(
            nameHits = listOf(LookupHit(aliceAccount, null)),
            bundleKeys = mapOf(searchHex(aliceAccount) to aliceKey),
            known = mapOf(searchKeyB64(aliceKey) to KnownPerson("Anna Weber", null, false)),
        )
        assertEquals("Anna Weber", searchPeople("anna", fake).people.single().name)
    }

    @Test
    fun `a contact held only by fingerprint is found and shown under the name they published`() = runTest {
        val fake = Fake(
            nameHits = listOf(LookupHit(aliceAccount, "rec".toByteArray())),
            bundleKeys = mapOf(searchHex(aliceAccount) to aliceKey),
            opened = mapOf("rec|out:n:anna" to OpenedPublicRecord(aliceKey, aliceAccount, "Anna Weber")),
            known = mapOf(
                searchKeyB64(aliceKey) to KnownPerson(
                    name = null,
                    verifiedUsername = null,
                    isContact = true,
                    fallbackName = "a1a1 a1a1",
                ),
            ),
        )
        val p = searchPeople("anna", fake).people.single()
        assertEquals("Anna Weber", p.name)
        assertEquals(PersonReach.CONTACT, p.reach)
    }

    @Test
    fun `a name the device holds for a contact still beats the published one`() = runTest {
        val fake = Fake(
            nameHits = listOf(LookupHit(aliceAccount, "rec".toByteArray())),
            bundleKeys = mapOf(searchHex(aliceAccount) to aliceKey),
            opened = mapOf("rec|out:n:anna" to OpenedPublicRecord(aliceKey, aliceAccount, "Anna Weber")),
            known = mapOf(
                searchKeyB64(aliceKey) to KnownPerson("Anna from work", null, true, fallbackName = null),
            ),
        )
        assertEquals("Anna from work", searchPeople("anna", fake).people.single().name)
    }

    @Test
    fun `the label rule - held name, then the found name, then a contact's fingerprint`() {
        val contactByFingerprint = KnownPerson(null, null, true, fallbackName = "a1a1 a1a1")
        assertEquals("Anna Weber", resultName(contactByFingerprint, "Anna Weber"))
        assertEquals("a1a1 a1a1", resultName(contactByFingerprint, null))
        assertEquals("Anna", resultName(KnownPerson("Anna", null, true, "a1a1 a1a1"), "Anna Weber"))
        assertNull(resultName(KnownPerson(null, null, false), null))
    }

    @Test
    fun `a network failure is never not found`() = runTest {
        val fake = Fake(nameLookupThrows = IllegalStateException("offline"))
        assertEquals(PeopleSearchState.FAILED, searchPeople("an", fake).state)
    }

    private fun intro(
        via: ProbeVia,
        key: ByteArray = aliceKey,
        name: String? = "Anna Weber",
        username: String? = null,
    ) = OpenedIntroduction(
        identityPk = key,
        accountId = aliceAccount,
        displayName = name,
        username = username,
        usernameCertificate = username?.let { "cert".toByteArray() },
        fofWire = "fof".toByteArray(),
        via = via,
    )

    @Test
    fun `an introduction whose account the bundle serves another key for is dropped`() = runTest {
        val fake = Fake(
            bundleKeys = mapOf(searchHex(aliceAccount) to aliceKey),
            introductions = listOf(intro(ProbeVia.TERM, key = malloryKey)),
        )
        val outcome = searchPeople("anna", fake)
        assertTrue(outcome.people.isEmpty())
        assertTrue("no reach check for an unbound record", fake.rec.reachChecks.isEmpty())
    }

    @Test
    fun `a bound introduction is reach-checked with its grant and shown`() = runTest {
        val fake = Fake(
            bundleKeys = mapOf(searchHex(aliceAccount) to aliceKey),
            introductions = listOf(intro(ProbeVia.TERM)),
        )
        val p = searchPeople("anna", fake).people.single()
        assertEquals(PersonReach.GRANT, p.reach)
        assertEquals(Base64.getEncoder().encodeToString("fof".toByteArray()), p.grantWireB64)
        assertEquals(listOf(p.grantWireB64), fake.rec.reachChecks)
        assertTrue("the grant passed a check", p.grantChecked)
    }

    @Test
    fun `a search reports counts of what it did`() = runTest {
        val fake = Fake(
            bundleKeys = mapOf(searchHex(aliceAccount) to aliceKey),
            introductions = listOf(intro(ProbeVia.TERM), intro(ProbeVia.TERM, name = "Bob Other")),
            reachAnswer = false,
        )
        searchPeople("anna", fake)
        assertEquals(
            listOf(SearchCounts(recordsOpened = 2, keptAfterMatch = 1, refusedByReachCheck = 1, people = 0)),
            fake.rec.counts,
        )
    }

    @Test
    fun `a refused reach check hides the person silently`() = runTest {
        val fake = Fake(
            bundleKeys = mapOf(searchHex(aliceAccount) to aliceKey),
            introductions = listOf(intro(ProbeVia.TERM)),
            reachAnswer = false,
        )
        val outcome = searchPeople("anna", fake)
        assertTrue(outcome.people.isEmpty())
        assertEquals(PeopleSearchState.NOT_FOUND, outcome.state)
    }

    @Test
    fun `an unanswered reach check is a failed search, not not found`() = runTest {
        val fake = Fake(
            bundleKeys = mapOf(searchHex(aliceAccount) to aliceKey),
            introductions = listOf(intro(ProbeVia.TERM)),
            reachThrows = true,
        )
        assertEquals(PeopleSearchState.FAILED, searchPeople("anna", fake).state)
    }

    @Test
    fun `an introduction's username is verified against the lookup key`() = runTest {
        val fake = Fake(
            bundleKeys = mapOf(searchHex(aliceAccount) to aliceKey),
            introductions = listOf(intro(ProbeVia.USERNAME, username = "anna")),
        )
        val p = searchPeople("anna", fake).people.single()
        assertEquals("anna", p.username)
        assertArrayEquals(lookup("u:anna"), fake.rec.verifiedWith.single())
    }

    @Test
    fun `an unverified username is not shown`() = runTest {
        val fake = Fake(
            bundleKeys = mapOf(searchHex(aliceAccount) to aliceKey),
            introductions = listOf(intro(ProbeVia.USERNAME, username = "anna")),
            certificateValid = false,
        )
        assertNull(searchPeople("anna", fake).people.single().username)
    }

    @Test
    fun `a username found by discovery shows under the username when no record names it`() = runTest {
        val fake = Fake(
            usernameHit = LookupHit(aliceAccount, null),
            bundleKeys = mapOf(searchHex(aliceAccount) to aliceKey),
        )
        val p = searchPeople("anna", fake).people.single()
        assertEquals("@anna", p.name)
        assertEquals("anna", p.username)
        assertEquals(PersonReach.OPEN, p.reach)
    }
}
