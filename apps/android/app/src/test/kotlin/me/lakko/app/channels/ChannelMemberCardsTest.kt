// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import kotlinx.coroutines.test.runTest
import me.lakko.app.reach.reachHex
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiProfile
import uniffi.lakko_ffi_uniffi.FfiReachClass
import uniffi.lakko_ffi_uniffi.FfiReachGrant

/**
 * ADR-0060 section 3 — channel member cards: the pure rules (web
 * `member-card-rules.ts`) and the reader's open / refuse paths over fake
 * ports (web `member-cards.ts` `loadChannelMembers`).
 */
class ChannelMemberCardsTest {

    // ---------------------------------------------------------------------
    // The rules
    // ---------------------------------------------------------------------

    @Test
    fun `the own card carries the username and the grant only where the setting allows`() {
        val hidden = cardContent(" Anna ", "anna", channelAllowed = false, reachAvailable = true)
        assertEquals(CardContent("Anna", null, false), hidden)
        val shown = cardContent("Anna", "anna", channelAllowed = true, reachAvailable = true)
        assertEquals(CardContent("Anna", "anna", true), shown)
        val noSession = cardContent("Anna", "anna", channelAllowed = true, reachAvailable = false)
        assertFalse(noSession.includeGrant)
        assertNull(noSession.username)
    }

    @Test
    fun `a card is uploaded again for a newer epoch or new content, never an older epoch`() {
        val content = CardContent("Anna", null, false)
        val fp = cardFingerprint(content, 10, null)
        assertTrue(cardUploadDue(null, 3u, fp))
        val last = UploadedCard(3u, fp)
        assertFalse(cardUploadDue(last, 3u, fp))
        assertTrue(cardUploadDue(last, 4u, fp))
        assertFalse(cardUploadDue(last, 2u, "other"))
        assertTrue(cardUploadDue(last, 3u, cardFingerprint(content, 11, null)))
    }

    @Test
    fun `a grant is filed only after the reach check accepted it`() {
        fun decide(carries: Boolean, verified: Boolean?) = cardGrantDecision(
            isOwnCard = false,
            senderAccountKeyHex = "aa",
            ownAccountKeyHex = "bb",
            carriesChannelGrant = carries,
            grantVerified = verified,
        )
        assertEquals(CardGrantDecision.FILE, decide(true, true))
        assertEquals(CardGrantDecision.SKIP, decide(true, false))
        assertEquals(CardGrantDecision.SKIP, decide(true, null))
        assertEquals(CardGrantDecision.WITHDRAW, decide(false, null))
        assertEquals(
            CardGrantDecision.SKIP,
            cardGrantDecision(true, "aa", "bb", carriesChannelGrant = true, grantVerified = true),
        )
        assertEquals(
            CardGrantDecision.SKIP,
            cardGrantDecision(false, "bb", "bb", carriesChannelGrant = true, grantVerified = true),
        )
    }

    // ---------------------------------------------------------------------
    // The reader
    // ---------------------------------------------------------------------

    private val channelHex = "11".repeat(16)
    private val ownKey = ByteArray(32) { 0x0B }
    private val bobKey = ByteArray(32) { 0x0C }
    private val bobAccount = ByteArray(16) { 0x0D }
    private val channelGrant = FfiReachGrant(FfiReachClass.CHANNEL, 4u, ByteArray(32) { 5 }, ByteArray(97) { 6 })

    /** card name -> (identity key, profile), or a card that fails to open. */
    private inner class Fake(
        val cards: List<Pair<String, ByteArray>>,
        val opens: Map<String, Pair<ByteArray, FfiProfile>?>,
        val epochs: Map<String, UInt> = emptyMap(),
        val published: Map<String, String?> = emptyMap(),
        val reach: Boolean? = true,
        held: Set<String> = emptySet(),
        /** ADR-0101 section 8 — the card's profile is not signed by the
         *  account its binding names. */
        val refuseProfile: Boolean = false,
    ) : MemberCardPorts {
        val heldSet = held.toMutableSet()
        val filed = mutableListOf<String>()
        val withdrawn = mutableListOf<String>()
        val checks = mutableListOf<String>()

        override suspend fun listPage(channelIdHex: String, after: String?) = MemberCardPage(
            members = cards.map { (name, pk) -> MemberCardEntry(pk, name.toByteArray()) },
            nextAfter = null,
        )

        override fun cardEpoch(card: ByteArray): UInt = epochs[String(card)] ?: 1u

        override fun openCard(seed: ByteArray, card: ByteArray, channelId: ByteArray, subscriberPk: ByteArray): OpenedCard {
            val o = opens[String(card)] ?: throw IllegalStateException("binding")
            return OpenedCard(o.first, String(card).toByteArray())
        }

        override fun decodeProfile(bytes: ByteArray): FfiProfile = opens.getValue(String(bytes))!!.second

        override fun verifyProfile(bytes: ByteArray, identityPk: ByteArray): FfiProfile? =
            if (refuseProfile) null else decodeProfile(bytes)

        override suspend fun resolveAccountKeyHex(accountId: ByteArray): String? = published[reachHex(accountId)]

        override suspend fun reachCheck(accountKey: ByteArray, grant: FfiReachGrant): Boolean? {
            checks += reachHex(accountKey)
            return reach
        }

        override suspend fun fileChannelGrant(accountKey: ByteArray, grant: FfiReachGrant, channelIdHex: String) {
            filed += reachHex(accountKey)
            heldSet += reachHex(accountKey)
        }

        override suspend fun withdrawChannelGrant(accountKey: ByteArray, channelIdHex: String) {
            withdrawn += reachHex(accountKey)
            heldSet -= reachHex(accountKey)
        }

        override fun holdsChannelGrant(accountKey: ByteArray, channelIdHex: String): Boolean =
            reachHex(accountKey) in heldSet

        override val ownAccountKeyHex: String? = reachHex(ownKey)
        override val reachAvailable: Boolean = true
    }

    private fun profile(name: String?, accountId: ByteArray? = bobAccount, grant: Boolean = true) = FfiProfile(
        displayName = name,
        username = "bob",
        updatedAtMs = 1uL,
        accountId = accountId,
        usernameCertificate = null,
        grants = if (grant) listOf(channelGrant) else emptyList(),
    )

    private suspend fun read(fake: Fake, seeds: Set<UInt> = setOf(1u), ownPk: ByteArray? = null) =
        loadChannelMembers(
            channelIdHex = channelHex,
            seedForEpoch = { if (it in seeds) ByteArray(32) else null },
            ownSubscriberPkHex = ownPk?.let(::reachHex),
            ports = fake,
        )

    @Test
    fun `a verified grant is filed under the binding key and the username shows`() = runTest {
        val fake = Fake(
            cards = listOf("bob" to ByteArray(32) { 1 }),
            opens = mapOf("bob" to (bobKey to profile("Bob"))),
            published = mapOf(reachHex(bobAccount) to reachHex(bobKey)),
        )
        val row = read(fake).rows.single()
        assertEquals("Bob", row.name)
        assertEquals("bob", row.username)
        assertEquals(listOf(reachHex(bobKey)), fake.filed)
    }

    @Test
    fun `ADR-0101 - a card whose profile its account did not sign is an unnamed member, nothing filed`() = runTest {
        val fake = Fake(
            cards = listOf("bob" to ByteArray(32) { 1 }),
            opens = mapOf("bob" to (bobKey to profile("Bob"))),
            published = mapOf(reachHex(bobAccount) to reachHex(bobKey)),
            refuseProfile = true,
        )
        val result = read(fake)
        val row = result.rows.single()
        assertEquals(reachHex(bobKey), row.key)
        assertNull(row.username)
        assertNull(row.avatar)
        assertEquals(false, row.named)
        assertEquals(0, result.unreadable)
        assertTrue(fake.filed.isEmpty())
        assertTrue(fake.checks.isEmpty())
        // The viewer's own key reads as the self row.
        assertEquals("self", unnamedCardRow(reachHex(ownKey), isOwnCard = false, ownAccountKeyHex = reachHex(ownKey)).key)
    }

    @Test
    fun `a refused grant is not filed and the username stays hidden`() = runTest {
        val fake = Fake(
            cards = listOf("bob" to ByteArray(32) { 1 }),
            opens = mapOf("bob" to (bobKey to profile("Bob"))),
            reach = false,
        )
        val row = read(fake).rows.single()
        assertNull(row.username)
        assertTrue(fake.filed.isEmpty())
    }

    @Test
    fun `a card without the grant withdraws the one held`() = runTest {
        val fake = Fake(
            cards = listOf("bob" to ByteArray(32) { 1 }),
            opens = mapOf("bob" to (bobKey to profile("Bob", grant = false))),
            held = setOf(reachHex(bobKey)),
        )
        val row = read(fake).rows.single()
        assertNull(row.username)
        assertEquals(listOf(reachHex(bobKey)), fake.withdrawn)
        assertTrue(fake.checks.isEmpty())
    }

    @Test
    fun `a card naming an account the directory publishes another key for is dropped`() = runTest {
        val fake = Fake(
            cards = listOf("bob" to ByteArray(32) { 1 }),
            opens = mapOf("bob" to (bobKey to profile("Bob"))),
            published = mapOf(reachHex(bobAccount) to "ff".repeat(32)),
        )
        val result = read(fake)
        assertTrue(result.rows.isEmpty())
        assertEquals(1, result.unreadable)
        assertTrue(fake.filed.isEmpty())
    }

    @Test
    fun `an account id the directory cannot resolve does not drop a verified card`() = runTest {
        val fake = Fake(
            cards = listOf("bob" to ByteArray(32) { 1 }),
            opens = mapOf("bob" to (bobKey to profile("Bob"))),
            published = emptyMap(),
        )
        assertEquals("Bob", read(fake).rows.single().name)
    }

    @Test
    fun `cards that do not open, or have no seed here, are counted, not shown`() = runTest {
        val fake = Fake(
            cards = listOf("broken" to ByteArray(32) { 1 }, "old" to ByteArray(32) { 2 }),
            opens = mapOf("broken" to null, "old" to (bobKey to profile("Bob"))),
            epochs = mapOf("old" to 7u),
        )
        val result = read(fake)
        assertTrue(result.rows.isEmpty())
        assertEquals(2, result.unreadable)
    }

    @Test
    fun `the own card is You and files nothing`() = runTest {
        me.lakko.app.l10n.EnglishForTests.install()
        val ownPk = ByteArray(32) { 3 }
        val fake = Fake(
            cards = listOf("me" to ownPk),
            opens = mapOf("me" to (ownKey to profile("Me"))),
        )
        val row = read(fake, ownPk = ownPk).rows.single()
        assertEquals("You", row.name)
        assertTrue(row.isSelf)
        assertTrue(fake.filed.isEmpty())
        assertTrue(fake.checks.isEmpty())
    }
}
