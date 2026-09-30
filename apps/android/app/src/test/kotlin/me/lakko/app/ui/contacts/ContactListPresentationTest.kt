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
}

/** The invite-links sheet rules. */
class InviteLinkRowsTest {

    private fun row(slug: String, createdAtMs: Long? = null, expiryMs: Long = 1_000L, usesLeft: Long? = null, fromSession: Boolean = createdAtMs != null) =
        InviteLinkRow(
            slug = slug, url = "https://lakko.me/i/$slug", label = "", createdAtMs = createdAtMs,
            expiryMs = expiryMs, usesLeft = usesLeft, fromSession = fromSession,
        )

    @Test
    fun `uses left follows the wire sentinels and never goes negative`() {
        assertEquals(null, invitesUsesLeft(INVITE_UNLIMITED_USES, 5))
        assertEquals(1L, invitesUsesLeft(0L, 0))
        assertEquals(0L, invitesUsesLeft(0L, 1))
        assertEquals(7L, invitesUsesLeft(10L, 3))
        assertEquals(0L, invitesUsesLeft(2L, 9))
    }

    @Test
    fun `merge keeps the session row, takes the server count, and orders`() {
        val session = listOf(row("a", createdAtMs = 10L), row("b", createdAtMs = 20L))
        val server = listOf(
            row("a", expiryMs = 500L, usesLeft = 3L),
            row("z", expiryMs = 0L),
            row("c", expiryMs = 900L),
            row("d", expiryMs = 100L),
        )
        val merged = mergeInviteLinkRows(session, server)
        assertEquals(listOf("b", "a", "d", "c", "z"), merged.map { it.slug })
        val a = merged.first { it.slug == "a" }
        assertEquals(10L, a.createdAtMs)
        assertEquals(3L, a.usesLeft)
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
        assertEquals(null, describeUsesLeft(null)?.en)
        assertEquals("1 use left", describeUsesLeft(1L)?.en)
        assertEquals("0 uses left", describeUsesLeft(0L)?.en)
        assertEquals("4 uses left", describeUsesLeft(4L)?.en)
    }
}
