// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import me.lakko.app.l10n.en
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** Host-side tests for the pure Contacts presentation rules (UX 5.3). */
class ContactListPresentationTest {

    private fun row(key: String, name: String, username: String? = null, placeholder: Boolean = false) =
        ContactRowModel(key = key, name = name, username = username, placeholder = placeholder)

    @Test
    fun `sections are alphabetical with the other bucket last`() {
        val rows = listOf(
            row("k1", "zoe"),
            row("k2", "Anna"),
            row("k3", "123 shop"),
            row("k4", "anton"),
            row("k5", "Member", username = null, placeholder = true),
            row("k6", "@boris"),
        )
        val sections = contactSections(rows)
        assertEquals(listOf("A", "B", "Z", OTHER_SECTION), sections.map { it.letter })
        assertEquals(listOf("Anna", "anton"), sections[0].rows.map { it.name })
        assertEquals(listOf("@boris"), sections[1].rows.map { it.name })
        // The digit-led name and the placeholder both land under `#`; a
        // placeholder with no username has an empty sort key and comes first.
        assertEquals(listOf("k5", "k3"), sections[3].rows.map { it.key })
    }

    @Test
    fun `a placeholder row sorts by its username when it has one`() {
        val rows = listOf(
            row("k1", "Member", username = "pavel", placeholder = true),
            row("k2", "Olga"),
        )
        val sections = contactSections(rows)
        assertEquals(listOf("O", "P"), sections.map { it.letter })
        assertEquals("k1", sections[1].rows.single().key)
    }

    @Test
    fun `identical names keep a stable order by key`() {
        val rows = listOf(row("b", "Sam"), row("a", "Sam"), row("c", "sam"))
        val section = contactSections(rows).single()
        assertEquals(listOf("a", "b", "c"), section.rows.map { it.key })
    }

    @Test
    fun `empty input gives no sections`() {
        assertTrue(contactSections(emptyList()).isEmpty())
    }

    @Test
    fun `search matches name or username, ignoring case and a leading at`() {
        val rows = listOf(
            row("k1", "Anna Petrova", username = "anna"),
            row("k2", "Boris", username = "bob"),
            row("k3", "Member", username = "carol", placeholder = true),
            row("k4", "Member", username = null, placeholder = true),
        )
        assertEquals(listOf("k1"), filterContacts(rows, "petr").map { it.key })
        assertEquals(listOf("k2"), filterContacts(rows, "@BOB").map { it.key })
        assertEquals(listOf("k3"), filterContacts(rows, "car").map { it.key })
        // The placeholder word itself is not searchable.
        assertTrue(filterContacts(rows, "member").isEmpty())
        assertEquals(4, filterContacts(rows, "   ").size)
    }

    @Test
    fun `requests sort newest first by message id`() {
        val ids = listOf(3L, 10L, 1L)
        assertEquals(listOf(10L, 3L, 1L), sortRequestsNewestFirst(ids) { it })
    }

    @Test
    fun `safety number groups are five digits regardless of the separator`() {
        val raw = "12345 67890 11111 22222 33333 44444 55555 66666 77777 88888 99999 00000"
        val groups = safetyNumberGroups(raw)
        assertEquals(12, groups.size)
        assertTrue(groups.all { it.length == 5 })
        assertEquals("12345", groups.first())
        assertEquals("00000", groups.last())
        // A different separator or none at all gives the same groups.
        assertEquals(groups, safetyNumberGroups(raw.replace(" ", "")))
        assertEquals(groups, safetyNumberGroups(raw.replace(" ", "-")))
    }

    @Test
    fun `a short tail keeps its digits and an empty input gives nothing`() {
        assertEquals(listOf("12345", "67"), safetyNumberGroups("1234567"))
        assertTrue(safetyNumberGroups("").isEmpty())
        assertTrue(safetyNumberGroups("abc").isEmpty())
    }

    @Test
    fun `safety number rows hold three groups each`() {
        val groups = (1..12).map { "%05d".format(it) }
        val rows = safetyNumberRows(groups)
        assertEquals(4, rows.size)
        assertTrue(rows.all { it.size == 3 })
        assertEquals(listOf(listOf("00001", "00002"), ), safetyNumberRows(groups.take(2)))
        assertTrue(safetyNumberRows(emptyList()).isEmpty())
    }

    @Test
    fun `section letter of a username-only name skips the at sign`() {
        assertEquals("K", sectionLetterOf(row("k", "@kira")))
        assertEquals(OTHER_SECTION, sectionLetterOf(row("k", "  ")))
        assertEquals(OTHER_SECTION, sectionLetterOf(row("k", "_under")))
    }

    /** docs/ux/conversation-info.md section 2.1, amended 2026-10-24. */
    @Test
    fun `a removed person's card is built from the one-to-one they still have`() {
        val key = ByteArray(32) { 0x21 }
        val route = contactRouteKey(key)
        val card = removedPeerContact(route) { k -> if (k.contentEquals(key)) "beef" else null }
        assertTrue(card != null && card.request.fromPubkey.contentEquals(key))
        assertEquals("beef", card?.groupIdHex)
        // No one-to-one, or a malformed key: no card.
        assertEquals(null, removedPeerContact(route) { null })
        assertEquals(null, removedPeerContact("not base64url!") { "beef" })
    }
}

/** The invite-links sheet rules. */
class InviteLinkRowsTest {

    private fun row(slug: String, createdAtMs: Long? = null, expiryMs: Long = 1_000L, maxUses: Long = 0L, fromSession: Boolean = createdAtMs != null) =
        InviteLinkRow(
            slug = slug, url = "https://lakko.me/i/$slug", label = "", createdAtMs = createdAtMs,
            expiryMs = expiryMs, maxUses = maxUses, fromSession = fromSession,
        )

    @Test
    fun `a link without a label has no title line`() {
        assertEquals(null, inviteRowTitle(row("a")))
        assertEquals(null, inviteRowTitle(row("a").copy(label = "   ")))
        assertEquals("Work", inviteRowTitle(row("a").copy(label = " Work ")))
    }

    @Test
    fun `the limit follows the wire sentinels`() {
        assertEquals("unlimited", describeInviteLimit(INVITE_UNLIMITED_USES).en)
        assertEquals("single-use", describeInviteLimit(0L).en)
        assertEquals("single-use", describeInviteLimit(1L).en)
        assertEquals("up to 10 uses", describeInviteLimit(10L).en)
    }

    @Test
    fun `merge keeps the session row and orders`() {
        val session = listOf(row("a", createdAtMs = 10L), row("b", createdAtMs = 20L))
        val server = listOf(
            row("a", expiryMs = 500L, maxUses = 3L),
            row("z", expiryMs = 0L),
            row("c", expiryMs = 900L),
            row("d", expiryMs = 100L),
        )
        val merged = mergeInviteLinkRows(session, server)
        assertEquals(listOf("b", "a", "d", "c", "z"), merged.map { it.slug })
        val a = merged.first { it.slug == "a" }
        assertEquals(10L, a.createdAtMs)
        assertTrue(a.fromSession)
        assertTrue(merged.none { it.slug == "a" && it.createdAtMs == null })
    }

    @Test
    fun `expiry and uses copy`() {
        val now = 1_000_000L
        assertEquals("Never expires", describeInviteExpiry(0L, now).en)
        assertEquals("Expired", describeInviteExpiry(now, now).en)
        assertEquals("Expires in 2d", describeInviteExpiry(now + 2 * 24 * 3_600_000L + 5, now).en)
        assertEquals("Expires in 3h", describeInviteExpiry(now + 3 * 3_600_000L + 5, now).en)
        assertEquals("Expires in 1m", describeInviteExpiry(now + 10, now).en)
    }

    @Test
    fun `expiry rounds to the nearest unit, minutes up`() {
        val now = 1_000_000L
        val min = 60_000L
        val hour = 60 * min
        val day = 24 * hour
        // A 7-day link made an hour ago still reads 7d (was a floored 6d).
        assertEquals("Expires in 7d", describeInviteExpiry(now + 7 * day - hour, now).en)
        assertEquals("Expires in 7d", describeInviteExpiry(now + (6.9 * day).toLong(), now).en)
        assertEquals("Expires in 1d", describeInviteExpiry(now + (23.6 * hour).toLong(), now).en)
        assertEquals("Expires in 23h", describeInviteExpiry(now + (23.4 * hour).toLong(), now).en)
        assertEquals("Expires in 2d", describeInviteExpiry(now + 36 * hour, now).en)
        assertEquals("Expires in 1h", describeInviteExpiry(now + (59.5 * min).toLong(), now).en)
        assertEquals("Expires in 59m", describeInviteExpiry(now + 59 * min, now).en)
        assertEquals("Expires in 1m", describeInviteExpiry(now + 30_000L, now).en)
    }

    @Test
    fun `a sent row shows the name it was found under, then the username`() {
        assertEquals(SentRowText("Kaj Ulm", "@kaj_u"), sentRowText("Kaj Ulm", "kaj_u"))
        assertEquals("found by name alone", SentRowText("Kaj Ulm", null), sentRowText("Kaj Ulm", ""))
    }

    @Test
    fun `a sent row says the username once and stands without a name`() {
        assertEquals(SentRowText("@kaj_u", null), sentRowText("@kaj_u", "kaj_u"))
        assertEquals(SentRowText("@kaj_u", null), sentRowText(null, "kaj_u"))
        assertEquals(SentRowText("@kaj_u", null), sentRowText("  ", "kaj_u"))
    }

    @Test
    fun `the people the list shows count contacts and pending contacts alike`() {
        val contact = ContactRowModel(key = "route-key", name = "Anna", username = null)
        val invited = ContactRowModel(key = "invited:0a", name = "Ines", username = "ines", personKey = "ines-key")
        val keys = listedPersonKeys(contactSections(listOf(contact, invited))) { rowKey ->
            "anna-key".takeIf { rowKey == "route-key" }
        }
        assertEquals(setOf("anna-key", "ines-key"), keys)
    }

    @Test
    fun `a pending contact the text does not match is not listed`() {
        val invited = ContactRowModel(key = "invited:0a", name = "Ines", username = "ines", personKey = "ines-key")
        val shown = contactSections(filterContacts(listOf(invited, ContactRowModel(key = "k", name = "Anna", username = null)), "anna"))
        assertEquals(emptySet<String>(), listedPersonKeys(shown) { null })
    }

    @Test
    fun `the card of a started-over key never offers Add to contacts`() {
        val sections = listOf(
            uniffi.lakko_ffi_uniffi.InfoSection.MESSAGE,
            uniffi.lakko_ffi_uniffi.InfoSection.ADD_CONTACT,
        )
        org.junit.Assert.assertEquals(
            listOf(uniffi.lakko_ffi_uniffi.InfoSection.MESSAGE),
            withoutAddForStartedOver(sections, keyStartedOver = true),
        )
        // Any other key keeps what the shared rule gave.
        org.junit.Assert.assertEquals(sections, withoutAddForStartedOver(sections, keyStartedOver = false))
    }
}
