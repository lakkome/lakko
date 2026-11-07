// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.LinkRecord
import uniffi.lakko_ffi_uniffi.LinkRecordKind

/** UX `contact-invite-link.md` section 9: the edit form's prefill and what a
 *  row's synced record allows. */
class InviteLinkEditTest {

    private val minute = 60_000L
    private val hour = 60 * minute
    private val day = 24 * hour
    private val minted = 1_800_000_000_000L

    private fun form(expiryMs: Long, maxUses: Long = 0L, mintedAt: Long? = minted, label: String = "") =
        inviteFormFromTerms(label = label, expiryMs = expiryMs, mintedAtMs = mintedAt, maxUses = maxUses, nowMs = minted)

    @Test
    fun `Create link mints 7 days, one person, no label`() {
        assertEquals(InviteCreateParams(label = "", expiryMillisFromNow = 7 * day, maxUses = 0u), INVITE_DEFAULT_PARAMS)
    }

    @Test
    fun `an expiry within a minute of a preset is that preset`() {
        assertEquals(InviteExpiryChoice.OneDay, form(minted + day).expiry)
        assertEquals(InviteExpiryChoice.SevenDays, form(minted + 7 * day - 40_000).expiry)
        assertEquals(InviteExpiryChoice.SevenDays, form(minted + 7 * day + minute).expiry)
        assertEquals(InviteExpiryChoice.ThirtyDays, form(minted + 30 * day).expiry)
    }

    @Test
    fun `never expiring is Never, never a preset`() {
        val f = form(0L)
        assertEquals(InviteExpiryChoice.Never, f.expiry)
        assertEquals(null, f.toParams()!!.expiryMillisFromNow)
    }

    @Test
    fun `other periods are custom days when whole days, else hours`() {
        val days = form(minted + 3 * day + 20_000)
        assertEquals(InviteExpiryChoice.Custom, days.expiry)
        assertEquals(InviteExpiryUnit.Days, days.customExpiryUnit)
        assertEquals("3", days.customExpiryAmount)

        val hours = form(minted + 5 * hour)
        assertEquals(InviteExpiryChoice.Custom, hours.expiry)
        assertEquals(InviteExpiryUnit.Hours, hours.customExpiryUnit)
        assertEquals("5", hours.customExpiryAmount)

        // Off a preset by more than a minute: not that preset.
        val near = form(minted + 7 * day + 2 * minute)
        assertEquals(InviteExpiryChoice.Custom, near.expiry)
        assertEquals(InviteExpiryUnit.Hours, near.customExpiryUnit)
        assertEquals("168", near.customExpiryAmount)

        // At least one hour, so the form stays valid.
        assertEquals("1", form(minted + 10 * minute).customExpiryAmount)
        assertTrue(form(minted + 10 * minute).toParams() != null)
    }

    @Test
    fun `uses map back to their choices`() {
        assertEquals(InviteUsesChoice.One, form(0L, maxUses = 0L).uses)
        assertEquals(InviteUsesChoice.One, form(0L, maxUses = 1L).uses)
        assertEquals(InviteUsesChoice.UpToTen, form(0L, maxUses = 10L).uses)
        assertEquals(InviteUsesChoice.Unlimited, form(0L, maxUses = INVITE_UNLIMITED_USES).uses)
        val custom = form(0L, maxUses = 25L)
        assertEquals(InviteUsesChoice.Custom, custom.uses)
        assertEquals("25", custom.customUses)
        assertEquals(25u, custom.toParams()!!.maxUses)
    }

    @Test
    fun `the prefilled form saves the same terms`() {
        val f = form(minted + 30 * day, maxUses = 10L, label = "Mum")
        val p = f.toParams()!!
        assertEquals("Mum", p.label)
        assertEquals(30 * day, p.expiryMillisFromNow)
        assertEquals(10u, p.maxUses)
    }

    private fun record(
        ledger: Byte,
        editable: Boolean,
        shared: Boolean = false,
        uses: UInt = 0u,
        kind: LinkRecordKind = LinkRecordKind.CONTACT,
    ) = LinkRecord(
        hlc = 1uL,
        linkId = ByteArray(16) { ledger },
        linkSecret = ByteArray(32),
        kind = kind,
        ledgerId = ByteArray(16) { ledger },
        revocationSeed = ByteArray(32),
        expiryMs = 0uL,
        maxUses = 0u,
        uses = uses,
        revoked = false,
        mintedAtMs = 1uL,
        label = "",
        answered = emptyList(),
        shared = shared,
        editable = editable,
        targetId = ByteArray(0),
        inviteSeed = ByteArray(0),
        capability = ByteArray(0),
    )

    @Test
    fun `the pencil follows the record, and no record has none`() {
        assertTrue(inviteRowEditable(inviteLinkSync(record(1, editable = true))))
        assertFalse(inviteRowEditable(inviteLinkSync(record(1, editable = false, shared = true))))
        assertFalse(inviteRowEditable(null))
    }

    @Test
    fun `delete asks first unless never copied and never used`() {
        assertFalse(inviteDeleteNeedsConfirmation(inviteLinkSync(record(1, editable = true))))
        assertTrue(inviteDeleteNeedsConfirmation(inviteLinkSync(record(1, editable = false, shared = true))))
        assertTrue(inviteDeleteNeedsConfirmation(inviteLinkSync(record(1, editable = false, uses = 1u))))
        assertTrue(inviteDeleteNeedsConfirmation(null))
    }

    @Test
    fun `records are joined to rows by the slug of their ledger id`() {
        val bySlug = inviteLinkSyncBySlug(
            listOf(record(1, editable = true), record(2, editable = false, kind = LinkRecordKind.CALL)),
        )
        val slug = me.lakko.app.auth.base64UrlEncodeNoPad(ByteArray(16) { 1 })
        assertEquals(setOf(slug), bySlug.keys)
        assertTrue(bySlug.getValue(slug).editable)
    }
}
