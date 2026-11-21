// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0060 — the pure half of the reach rules: which keys the owner's mailbox
 * holds, what a registration sends, whose grants are kept (C-68), and which
 * held grant signs a write. Host JVM only; no native library.
 */
class ReachRulesTest {

    private val now = 1_000_000L

    private fun inputs(
        versions: Map<ReachSlot, Int> = emptyMap(),
        joined: Set<String> = emptySet(),
        visible: Set<String> = emptySet(),
        channels: Set<String> = emptySet(),
        fof: Boolean = false,
        links: Map<String, Long?> = emptyMap(),
        replies: Map<String, Long> = emptyMap(),
    ) = ReachInputs(versions, joined, visible, channels, fof, links, replies)

    // -----------------------------------------------------------------
    // The desired key set
    // -----------------------------------------------------------------

    @Test
    fun `a new account holds exactly its contact key`() {
        val keys = desiredReachKeys(inputs(), now)
        assertEquals(listOf(DesiredReachKey(ReachSlot(ReachKind.CONTACT, ""), 0, null)), keys)
    }

    /** ADR-0078 section 9 — one `member` and one `group` key for the
     *  account, never one per group, and no group id names a key. */
    @Test
    fun `the groups carry one member key and one group key for the account`() {
        val keys = desiredReachKeys(inputs(joined = setOf("aa", "bb"), visible = setOf("bb")), now)
        assertEquals(
            listOf(
                ReachSlot(ReachKind.CONTACT, ""),
                ReachSlot(ReachKind.MEMBER, ""),
                ReachSlot(ReachKind.GROUP, ""),
            ),
            keys.map { it.slot },
        )
        val none = desiredReachKeys(inputs(joined = setOf("aa"), visible = emptySet()), now)
        assertFalse("no visible group, no group key", none.any { it.slot.kind == ReachKind.GROUP })
        assertTrue(none.any { it.slot == ReachSlot(ReachKind.MEMBER, "") })
    }

    @Test
    fun `a visible group this account left carries no group key`() {
        val keys = desiredReachKeys(inputs(joined = setOf("aa"), visible = setOf("gone")), now)
        assertFalse(keys.any { it.slot.kind == ReachKind.GROUP })
    }

    @Test
    fun `the channels carry one channel key while any is visible`() {
        val keys = desiredReachKeys(inputs(channels = setOf("c1", "c2")), now)
        assertEquals(listOf(ReachSlot(ReachKind.CHANNEL, "")), keys.filter { it.slot.kind == ReachKind.CHANNEL }.map { it.slot })
    }

    @Test
    fun `every class but link and reply is account-wide`() {
        assertEquals(
            setOf(ReachKind.CONTACT, ReachKind.GROUP, ReachKind.MEMBER, ReachKind.CHANNEL, ReachKind.FOF),
            ReachKind.entries.filter { it.scopeless }.toSet(),
        )
    }

    /** ADR-0078 section 9 — leaving a group (or a channel) rotates the class;
     *  the first list, a join and an empty (not yet loaded) list do not. */
    @Test
    fun `a departure is a set difference over the membership`() {
        assertTrue(reachDeparted(listOf("aa", "bb"), listOf("aa")))
        assertTrue(reachDeparted(listOf("AA"), listOf("cc")))
        assertFalse(reachDeparted(listOf("aa"), listOf("aa", "bb")))
        assertFalse(reachDeparted(emptyList(), listOf("aa")))
        assertFalse(reachDeparted(listOf("aa"), emptyList()))
        assertFalse(reachDeparted(listOf("Aa"), listOf("aA")))
    }

    @Test
    fun `contacts of contacts adds the fof key`() {
        assertTrue(desiredReachKeys(inputs(fof = true), now).any { it.slot.kind == ReachKind.FOF })
        assertFalse(desiredReachKeys(inputs(fof = false), now).any { it.slot.kind == ReachKind.FOF })
    }

    @Test
    fun `an expired link or reply contributes no key and a live one keeps its expiry`() {
        val keys = desiredReachKeys(
            inputs(
                links = mapOf("11" to now + 5, "22" to now - 1, "33" to null),
                replies = mapOf("r1" to now + 10, "r2" to now),
            ),
            now,
        )
        val link11 = keys.single { it.slot == ReachSlot(ReachKind.LINK, "11") }
        assertEquals(now + 5, link11.expiresAtMs)
        assertNull(keys.single { it.slot == ReachSlot(ReachKind.LINK, "33") }.expiresAtMs)
        assertFalse(keys.any { it.slot == ReachSlot(ReachKind.LINK, "22") })
        assertTrue(keys.any { it.slot == ReachSlot(ReachKind.REPLY, "r1") })
        assertFalse(keys.any { it.slot == ReachSlot(ReachKind.REPLY, "r2") })
    }

    @Test
    fun `a rotated slot is desired at its new version`() {
        val keys = desiredReachKeys(
            inputs(versions = mapOf(ReachSlot(ReachKind.CONTACT, "") to 3)),
            now,
        )
        // ADR-0060 section 1 (2026-11-06): nothing registered before, so the
        // new version alone (a retired version is never registered again).
        assertEquals(listOf(3), keys.filter { it.slot.kind == ReachKind.CONTACT }.map { it.version })
    }

    // -----------------------------------------------------------------
    // The registration diff
    // -----------------------------------------------------------------

    @Test
    fun `a key already registered with the same expiry is not re-sent`() {
        val key = DesiredReachKey(ReachSlot(ReachKind.CONTACT, ""), 0, null)
        val diff = diffReachKeys(
            desired = mapOf("pk1" to key),
            registered = mapOf("pk1" to RegisteredReachKey(key.slot, 0, null)),
        )
        assertTrue(diff.isEmpty)
    }

    @Test
    fun `a rotation adds the new key and removes the old one in one update`() {
        val slot = ReachSlot(ReachKind.CONTACT, "")
        val diff = diffReachKeys(
            desired = mapOf("pkNew" to DesiredReachKey(slot, 1, null)),
            registered = mapOf("pkOld" to RegisteredReachKey(slot, 0, null)),
        )
        assertEquals(setOf("pkNew"), diff.add.keys)
        assertEquals(setOf("pkOld"), diff.remove)
    }

    @Test
    fun `a changed expiry re-adds the key`() {
        val slot = ReachSlot(ReachKind.LINK, "11")
        val diff = diffReachKeys(
            desired = mapOf("pk" to DesiredReachKey(slot, 0, 20)),
            registered = mapOf("pk" to RegisteredReachKey(slot, 0, 10)),
        )
        assertEquals(setOf("pk"), diff.add.keys)
        assertTrue(diff.remove.isEmpty())
    }

    @Test
    fun `a revoked link's key comes off the mailbox`() {
        val diff = diffReachKeys(
            desired = emptyMap(),
            registered = mapOf("pkLink" to RegisteredReachKey(ReachSlot(ReachKind.LINK, "11"), 0, null)),
        )
        assertEquals(setOf("pkLink"), diff.remove)
    }

    // -----------------------------------------------------------------
    // Which received grants are kept (C-68)
    // -----------------------------------------------------------------

    private val self = "5e1f"
    private val peer = "beef"
    private val third = "cafe"

    @Test
    fun `a contact grant is kept only from the other participant of a one-to-one`() {
        assertTrue(acceptReceivedGrant(ReachKind.CONTACT, GrantVenue.ONE_TO_ONE, peer, self, peer, true))
        assertFalse(acceptReceivedGrant(ReachKind.CONTACT, GrantVenue.ONE_TO_ONE, third, self, peer, true))
        assertFalse(acceptReceivedGrant(ReachKind.CONTACT, GrantVenue.GROUP, peer, self, null, true))
        assertFalse(acceptReceivedGrant(ReachKind.CONTACT, GrantVenue.CHANNEL_CARD, peer, self, null, true))
    }

    @Test
    fun `group and member grants are kept only from a member`() {
        assertTrue(acceptReceivedGrant(ReachKind.GROUP, GrantVenue.GROUP, peer, self, null, true))
        assertFalse(acceptReceivedGrant(ReachKind.GROUP, GrantVenue.GROUP, peer, self, null, false))
        assertTrue(acceptReceivedGrant(ReachKind.MEMBER, GrantVenue.GROUP, peer, self, null, true))
        assertTrue(acceptReceivedGrant(ReachKind.MEMBER, GrantVenue.ONE_TO_ONE, peer, self, peer, true))
        assertFalse(acceptReceivedGrant(ReachKind.MEMBER, GrantVenue.GROUP, peer, self, null, false))
        assertFalse(acceptReceivedGrant(ReachKind.GROUP, GrantVenue.ONE_TO_ONE, peer, self, peer, true))
    }

    @Test
    fun `a channel grant is kept only from a member card`() {
        assertTrue(acceptReceivedGrant(ReachKind.CHANNEL, GrantVenue.CHANNEL_CARD, peer, self, null, false))
        assertFalse(acceptReceivedGrant(ReachKind.CHANNEL, GrantVenue.GROUP, peer, self, null, true))
    }

    @Test
    fun `link reply and fof grants never ride a profile`() {
        for (kind in listOf(ReachKind.LINK, ReachKind.REPLY, ReachKind.FOF)) {
            for (venue in GrantVenue.entries) {
                assertFalse(acceptReceivedGrant(kind, venue, peer, self, peer, true))
            }
        }
    }

    @Test
    fun `an unresolved sender or this account itself is refused`() {
        assertFalse(acceptReceivedGrant(ReachKind.MEMBER, GrantVenue.GROUP, null, self, null, true))
        assertFalse(acceptReceivedGrant(ReachKind.MEMBER, GrantVenue.GROUP, "", self, null, true))
        assertFalse(acceptReceivedGrant(ReachKind.CONTACT, GrantVenue.ONE_TO_ONE, self, self, self, true))
    }

    // -----------------------------------------------------------------
    // Which held grant signs a write
    // -----------------------------------------------------------------

    @Test
    fun `a contact request never uses a member key`() {
        val held = listOf(HeldGrantRef(ReachKind.MEMBER, "aa"))
        assertNull(pickWriteGrant(held, WritePurpose.CONTACT_REQUEST))
    }

    @Test
    fun `a contact request uses a group grant, then channel, then fof`() {
        val held = listOf(
            HeldGrantRef(ReachKind.FOF, ""),
            HeldGrantRef(ReachKind.CHANNEL, "cc"),
            HeldGrantRef(ReachKind.GROUP, "aa"),
        )
        assertEquals(HeldGrantRef(ReachKind.GROUP, "aa"), pickWriteGrant(held, WritePurpose.CONTACT_REQUEST))
        assertEquals(
            HeldGrantRef(ReachKind.FOF, ""),
            pickWriteGrant(listOf(HeldGrantRef(ReachKind.FOF, "")), WritePurpose.CONTACT_REQUEST),
        )
    }

    @Test
    fun `a reply prefers the requester's reply key, then their contact key`() {
        val held = listOf(HeldGrantRef(ReachKind.CONTACT, ""), HeldGrantRef(ReachKind.REPLY, ""))
        assertEquals(HeldGrantRef(ReachKind.REPLY, ""), pickWriteGrant(held, WritePurpose.REPLY))
        assertEquals(
            HeldGrantRef(ReachKind.CONTACT, ""),
            pickWriteGrant(listOf(HeldGrantRef(ReachKind.CONTACT, "")), WritePurpose.REPLY),
        )
        assertNull(pickWriteGrant(listOf(HeldGrantRef(ReachKind.GROUP, "aa")), WritePurpose.REPLY))
    }

    @Test
    fun `a Welcome is not ordered by the client`() {
        // G-53 — the core's plan orders a Welcome (planHeldWrites).
        val held = listOf(HeldGrantRef(ReachKind.CONTACT, ""), HeldGrantRef(ReachKind.MEMBER, "aa"))
        assertTrue(orderedWriteGrants(held, WritePurpose.WELCOME).isEmpty())
        assertNull(pickWriteGrant(held, WritePurpose.WELCOME))
    }

    @Test
    fun `nothing held means no grant`() {
        for (purpose in WritePurpose.entries) assertNull(pickWriteGrant(emptyList(), purpose))
    }

    // -----------------------------------------------------------------
    // Reply lifecycle and hex
    // -----------------------------------------------------------------

    @Test
    fun `a reply key lives seven days and is expired exactly at its expiry`() {
        assertEquals(now + 7L * 24 * 60 * 60 * 1000, replyExpiry(now))
        val replies = mapOf("a" to now, "b" to now + 1)
        assertEquals(setOf("a"), expiredReplies(replies, now))
    }

    @Test
    fun `hex round-trips and refuses malformed input`() {
        val bytes = byteArrayOf(0, 1, 0x7f, -1, -128)
        assertEquals("00017fff80", reachHex(bytes))
        assertArrayEquals(bytes, reachUnhex("00017FFF80"))
        assertNull(reachUnhex("abc"))
        assertNull(reachUnhex("zz"))
    }
}

class ReachShrinkRotationTest {
    @Test
    fun `someone leaving rotates, growth and a torn read do not`() {
        assertTrue(reachRotationOnShrink(listOf("a", "b"), listOf("a"), selfEvicted = false))
        assertFalse(reachRotationOnShrink(listOf("a"), listOf("a", "b"), selfEvicted = false))
        assertFalse(reachRotationOnShrink(listOf("a", "b"), listOf("a", "b"), selfEvicted = false))
        assertFalse(reachRotationOnShrink(null, listOf("a"), selfEvicted = false))
        assertFalse(reachRotationOnShrink(listOf("a", "b"), emptyList(), selfEvicted = false))
    }

    @Test
    fun `a remove and an add in one batch still counts as a shrink`() {
        assertTrue(reachRotationOnShrink(listOf("a", "b"), listOf("a", "c"), selfEvicted = false))
    }

    @Test
    fun `an evicted device does not rotate`() {
        assertFalse(reachRotationOnShrink(listOf("a", "b"), listOf("a"), selfEvicted = true))
    }
}

/**
 * ADR-0060 §1 ("Versions after a restore", amended 2026-10-01): a device with
 * no version state resumes each slot at the highest version whose key the
 * mailbox still holds, and registers nothing versioned before it knows.
 */
class ReachVersionRecoveryTest {

    /** A stand-in for the core derivation: a distinct "public key" per slot
     *  and version. */
    private fun pk(slot: ReachSlot, version: Int): String = "${slot.kind.name}/${slot.scopeHex}/$version"

    private val contact = ReachSlot(ReachKind.CONTACT, "")

    @Test
    fun `a restored device with v1 registered resumes at v1 and never re-registers v0`() {
        val registered = setOf(pk(contact, 1))
        val recovered = recoverVersions(listOf(contact), registered, ::pk)
        assertEquals(mapOf(contact to 1), recovered)

        // The key set built from the recovered version holds v1, never v0.
        val inputs = ReachInputs(
            versions = recovered,
            joinedGroups = emptySet(),
            visibleGroups = emptySet(),
            visibleChannels = emptySet(),
            contactsOfContacts = false,
            links = emptyMap(),
            replies = emptyMap(),
        )
        val keys = desiredReachKeys(inputs, nowMs = 0L).map { pk(it.slot, it.version) }
        // ADR-0060 section 1 (2026-11-06): a key rotated away is never
        // registered again, even inside the retired window.
        assertEquals(listOf(pk(contact, 1)), keys)
        assertFalse(pk(contact, 0) in keys)
    }

    @Test
    fun `the highest registered version wins and a slot with none starts at zero`() {
        val group = ReachSlot(ReachKind.MEMBER, "")
        val fof = ReachSlot(ReachKind.FOF, "")
        val registered = setOf(pk(group, 2), pk(group, 5), "a link key nobody can name")
        val recovered = recoverVersions(listOf(contact, fof, group), registered, ::pk)
        assertEquals(mapOf(group to 5), recovered)
        assertNull(recoverSlotVersion(contact, registered, ::pk))
    }

    @Test
    fun `a version past the bound is not found`() {
        val registered = setOf(pk(contact, MAX_RECOVERED_REACH_VERSION + 1))
        assertNull(recoverSlotVersion(contact, registered, ::pk))
        assertEquals(
            MAX_RECOVERED_REACH_VERSION,
            recoverSlotVersion(contact, setOf(pk(contact, MAX_RECOVERED_REACH_VERSION)), ::pk),
        )
    }

    @Test
    fun `the nameable slots are the account-wide classes`() {
        val slots = recoverableSlots(joinedGroups = listOf("BB", "aa"), channels = listOf("cc"))
        assertEquals(
            listOf(
                ReachSlot(ReachKind.CONTACT, ""),
                ReachSlot(ReachKind.FOF, ""),
                ReachSlot(ReachKind.MEMBER, ""),
                ReachSlot(ReachKind.GROUP, ""),
                ReachSlot(ReachKind.CHANNEL, ""),
            ),
            slots,
        )
        assertEquals(
            listOf(ReachSlot(ReachKind.CONTACT, ""), ReachSlot(ReachKind.FOF, "")),
            recoverableSlots(joinedGroups = emptyList(), channels = emptyList()),
        )
        assertFalse(slots.any { it.kind == ReachKind.LINK || it.kind == ReachKind.REPLY })
    }

    @Test
    fun `only a device with no version state recovers`() {
        assertTrue(holdsNoVersionState(versionsEmpty = true, registeredEmpty = true, reachStateKnown = false))
        assertFalse(holdsNoVersionState(versionsEmpty = false, registeredEmpty = true, reachStateKnown = false))
        assertFalse(holdsNoVersionState(versionsEmpty = true, registeredEmpty = false, reachStateKnown = false))
        assertFalse(holdsNoVersionState(versionsEmpty = true, registeredEmpty = true, reachStateKnown = true))
    }

    /** The read failed: nothing versioned goes out (fail closed); link and
     *  reply keys, which carry no history, may. */
    @Test
    fun `while recovery is pending nothing versioned is registered`() {
        val inputs = ReachInputs(
            versions = emptyMap(),
            joinedGroups = setOf("aa"),
            visibleGroups = setOf("aa"),
            visibleChannels = setOf("cc"),
            contactsOfContacts = true,
            links = mapOf("11" to null),
            replies = mapOf("22" to 10_000L),
        )
        val allowed = registrableBeforeRecovery(desiredReachKeys(inputs, nowMs = 0L))
        assertEquals(setOf(ReachKind.LINK, ReachKind.REPLY), allowed.map { it.slot.kind }.toSet())
        assertEquals(2, allowed.size)
    }
}

/** ADR-0050 section 2.5a item 7 — the grants a Welcome writer walks. */
class ReachWelcomeGrantOrderTest {
    private val everyKind = listOf(
        HeldGrantRef(ReachKind.GROUP, "g1"),
        HeldGrantRef(ReachKind.MEMBER, "g1"),
        HeldGrantRef(ReachKind.CHANNEL, "c1"),
        HeldGrantRef(ReachKind.FOF, ""),
        HeldGrantRef(ReachKind.CONTACT, ""),
        HeldGrantRef(ReachKind.REPLY, ""),
    )

    @Test
    fun `the answer to a request is the reply grant, then a contact grant, and nothing else`() {
        assertEquals(
            listOf(HeldGrantRef(ReachKind.REPLY, ""), HeldGrantRef(ReachKind.CONTACT, "")),
            orderedWriteGrants(everyKind, WritePurpose.REPLY),
        )
    }

    @Test
    fun `a person with only a group or member grant gets no answer written`() {
        val onlyGroup = listOf(HeldGrantRef(ReachKind.GROUP, "g1"), HeldGrantRef(ReachKind.MEMBER, "g1"))
        assertTrue(orderedWriteGrants(onlyGroup, WritePurpose.REPLY).isEmpty())
    }

    @Test
    fun `a stale key moves on to the next one, and a group key is never reached`() = runTest {
        val tried = mutableListOf<String>()
        val candidates = orderedWriteGrants(everyKind, WritePurpose.REPLY).map { ref ->
            { ref.kind.name }
        }
        val outcome = firstAcceptedWrite(
            candidates,
            { auth: String ->
                tried += auth
                if (auth == "REPLY") throw IllegalStateException("401") else auth
            },
            { true },
        )
        assertEquals("CONTACT", (outcome as SignedWriteOutcome.Written).result)
        assertEquals(listOf("REPLY", "CONTACT"), tried)
    }

    @Test
    fun `every candidate refused is the last refusal, and nothing else was tried`() = runTest {
        val tried = mutableListOf<String>()
        val refusal = IllegalStateException("401")
        val candidates = orderedWriteGrants(everyKind, WritePurpose.REPLY).map { ref ->
            { ref.kind.name }
        }
        val outcome = firstAcceptedWrite<String, String>(
            candidates,
            { auth ->
                tried += auth
                throw refusal
            },
            { true },
        )
        assertEquals(refusal, (outcome as SignedWriteOutcome.Exhausted).lastRefusal)
        assertEquals(listOf("REPLY", "CONTACT"), tried)
        // G-53 — what the open-mailbox rule is told: two tried, two refused.
        assertEquals(2, outcome.tried)
        assertEquals(2, outcome.refused)
    }

    @Test
    fun `a check moves past a refused key, and says no when all are refused`() = runTest {
        val asked = mutableListOf<String>()
        val candidates: List<() -> String?> = listOf({ "stale" }, { null }, { "live" }, { "never" })
        assertTrue(
            anyCandidateOpens(candidates) { auth ->
                asked += auth
                auth == "live"
            },
        )
        assertEquals(listOf("stale", "live"), asked)
        assertFalse(anyCandidateOpens(candidates) { false })
        assertFalse(anyCandidateOpens(emptyList<() -> String?>()) { true })
    }

    @Test
    fun `a check that cannot be made is not a no`() = runTest {
        val thrown = runCatching {
            anyCandidateOpens(listOf<() -> String?>({ "key" })) { throw IllegalStateException("offline") }
        }.exceptionOrNull()
        assertTrue(thrown is IllegalStateException)
    }

    @Test
    fun `a proofless write is offered only when no grant yielded one`() = runTest {
        val none: List<() -> String?> = listOf({ null })
        assertEquals(
            "open",
            (firstAcceptedWrite(withProoflessLast(none) { "open" }, { auth: String -> auth }, { false })
                as SignedWriteOutcome.Written).result,
        )
        // A refused grant never falls back to a write the DS would drop.
        val tried = mutableListOf<String>()
        val outcome = firstAcceptedWrite<String, String>(
            withProoflessLast(listOf<() -> String?>({ "stale" })) { "open" },
            { auth ->
                tried += auth
                throw IllegalStateException("401")
            },
            { true },
        )
        assertTrue(outcome is SignedWriteOutcome.Exhausted)
        assertEquals(listOf("stale"), tried)
    }

    @Test
    fun `a contact request tries every held grant in order and never a member key`() {
        val held = listOf(
            HeldGrantRef(ReachKind.FOF, "") to "fof",
            HeldGrantRef(ReachKind.CHANNEL, "c1") to "channel",
            HeldGrantRef(ReachKind.GROUP, "g2") to "group2",
            HeldGrantRef(ReachKind.MEMBER, "aa") to "member",
            HeldGrantRef(ReachKind.REPLY, "r") to "reply",
            HeldGrantRef(ReachKind.CONTACT, "") to "contact",
            HeldGrantRef(ReachKind.GROUP, "aa") to "group1",
            // The same class and scope filed under a predecessor key.
            HeldGrantRef(ReachKind.GROUP, "aa") to "group1-older",
        )
        assertEquals(
            listOf("contact", "group1", "group1-older", "group2", "channel", "fof"),
            contactRequestGrantsInOrder(held),
        )
    }

    // --- only the contacts I choose ------------------------------------------

    private val all = ContactsReach(onlyChosen = false, chosenHex = emptySet())
    private fun chosen(vararg ids: String) = ContactsReach(onlyChosen = true, chosenHex = ids.toSet())

    @Test
    fun `moving from all to chosen narrows`() {
        assertTrue(contactsNarrowed(all, chosen("a", "b")))
        assertTrue(contactsNarrowed(all.copy(chosenHex = setOf("a")), chosen("a")))
    }

    @Test
    fun `switching a contact off narrows and switching one on does not`() {
        assertTrue(contactsNarrowed(chosen("a", "b"), chosen("a")))
        assertFalse(contactsNarrowed(chosen("a"), chosen("a", "b")))
        assertFalse(contactsNarrowed(chosen("a"), chosen("a")))
    }

    @Test
    fun `going back to all is widening`() {
        assertFalse(contactsNarrowed(chosen("a"), all))
        assertFalse(contactsNarrowed(all, all))
        assertFalse("no setting before: nothing narrowed", contactsNarrowed(null, chosen("a")))
    }

    @Test
    fun `a change of the setting rotates fof only when it narrowed`() {
        assertEquals(listOf(ReachKind.GROUP, ReachKind.CHANNEL), settingChangeRotation(chosen("a"), chosen("a", "b")))
        assertEquals(
            listOf(ReachKind.GROUP, ReachKind.CHANNEL, ReachKind.FOF),
            settingChangeRotation(chosen("a", "b"), chosen("b")),
        )
    }
}
