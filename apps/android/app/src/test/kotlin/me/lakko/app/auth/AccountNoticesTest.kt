// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0067 section 9 / UX 19.6 - which notices a landing shows. */
class AccountNoticesTest {

    private class MapStorage : MarkerStorage {
        val rows = HashMap<String, String>()
        override fun get(key: String): String? = rows[key]
        override fun set(key: String, value: String) {
            rows[key] = value
        }
        override fun remove(key: String) {
            rows.remove(key)
        }
    }

    private fun row(id: Long, kind: AccountNoticeKind, atMs: Long = 1_000_000L) =
        WireAccountNotice(id = id, kind = kind.wire, atMs = atMs)

    @Test
    fun `every wire kind is known and round-trips`() {
        assertEquals(
            setOf(
                "password-changed-by-email", "account-opened-by-email", "recovery-email-added",
                "recovery-email-replaced", "recovery-email-removed", "recovery-phrase-saved",
            ),
            AccountNoticeKind.entries.map { it.wire }.toSet(),
        )
        for (kind in AccountNoticeKind.entries) assertEquals(kind, AccountNoticeKind.ofWire(kind.wire))
        assertNull(AccountNoticeKind.ofWire("something-new"))
    }

    @Test
    fun `each notice past the cursor is shown once, oldest first`() {
        val fetched = listOf(
            row(7, AccountNoticeKind.EmailRemoved),
            row(5, AccountNoticeKind.PasswordChangedByEmail),
            row(6, AccountNoticeKind.EmailAdded),
        )
        val first = selectAccountNotices(fetched, nextAfter = 7, cursor = 5, ownActions = emptyMap())
        assertEquals(listOf(6L, 7L), first.show.map { it.id })
        assertEquals(7L, first.cursor)
        // The same page again, with the stored cursor: nothing.
        val again = selectAccountNotices(fetched, nextAfter = 7, cursor = first.cursor, ownActions = emptyMap())
        assertTrue(again.show.isEmpty())
        assertEquals(7L, again.cursor)
    }

    @Test
    fun `a device with no cursor asks for the last 30 days, shows them, then keeps a cursor`() {
        val now = 100L * 24L * 60L * 60L * 1000L
        // The first request names a time, never an id.
        assertEquals(NoticeQuery(after = null, sinceMs = now - NOTICE_FIRST_LOOKBACK_MS), noticeQuery(null, now))
        assertEquals(30L * 24L * 60L * 60L * 1000L, NOTICE_FIRST_LOOKBACK_MS)
        val fetched = listOf(row(3, AccountNoticeKind.EmailAdded), row(9, AccountNoticeKind.PhraseSaved))
        val selection = selectAccountNotices(fetched, nextAfter = 9, cursor = null, ownActions = emptyMap())
        assertEquals(listOf(3L, 9L), selection.show.map { it.id })
        assertEquals(9L, selection.cursor)
        // From then on the device pages by its cursor and sees nothing twice.
        assertEquals(NoticeQuery(after = 9L, sinceMs = null), noticeQuery(selection.cursor, now))
        assertTrue(selectAccountNotices(fetched, 9, selection.cursor, emptyMap()).show.isEmpty())
    }

    @Test
    fun `a device with no cursor is still not shown its own action`() {
        val at = 5_000_000L
        val selection = selectAccountNotices(
            listOf(row(1, AccountNoticeKind.EmailAdded, atMs = at + 1_000L), row(2, AccountNoticeKind.EmailRemoved, atMs = at)),
            nextAfter = 2,
            cursor = null,
            ownActions = mapOf(AccountNoticeKind.EmailAdded to at),
        )
        assertEquals(listOf(2L), selection.show.map { it.id })
        assertEquals(setOf(AccountNoticeKind.EmailAdded), selection.spentOwn)
    }

    @Test
    fun `the own-action window is ten minutes`() {
        assertEquals(10L * 60L * 1000L, OWN_ACTION_WINDOW_MS)
        val at = 50_000_000L
        fun shown(deltaMs: Long) = selectAccountNotices(
            listOf(row(2, AccountNoticeKind.EmailAdded, atMs = at + deltaMs)),
            nextAfter = 2,
            cursor = 0,
            ownActions = mapOf(AccountNoticeKind.EmailAdded to at),
        ).show.size
        assertEquals(0, shown(OWN_ACTION_WINDOW_MS))
        assertEquals(0, shown(-OWN_ACTION_WINDOW_MS))
        assertEquals(1, shown(OWN_ACTION_WINDOW_MS + 1))
    }

    @Test
    fun `an unknown kind is skipped and the cursor still passes it`() {
        val selection = selectAccountNotices(
            listOf(WireAccountNotice(4, "something-new", 0L), row(5, AccountNoticeKind.EmailAdded)),
            nextAfter = 5,
            cursor = 0,
            ownActions = emptyMap(),
        )
        assertEquals(listOf(5L), selection.show.map { it.id })
        assertEquals(5L, selection.cursor)
    }

    @Test
    fun `a device is not shown its own action`() {
        val at = 5_000_000L
        val selection = selectAccountNotices(
            listOf(row(2, AccountNoticeKind.EmailAdded, atMs = at + 2_000L)),
            nextAfter = 2,
            cursor = 0,
            ownActions = mapOf(AccountNoticeKind.EmailAdded to at),
        )
        assertTrue(selection.show.isEmpty())
        assertEquals(setOf(AccountNoticeKind.EmailAdded), selection.spentOwn)
        assertEquals(2L, selection.cursor)
    }

    @Test
    fun `the own-action marker covers one notice, of its own kind, near its own time`() {
        val at = 5_000_000L
        // Another kind is shown.
        val otherKind = selectAccountNotices(
            listOf(row(2, AccountNoticeKind.EmailRemoved, atMs = at)),
            nextAfter = 2,
            cursor = 0,
            ownActions = mapOf(AccountNoticeKind.EmailAdded to at),
        )
        assertEquals(listOf(2L), otherKind.show.map { it.id })
        assertTrue(otherKind.spentOwn.isEmpty())
        // The same kind long after is someone else's.
        val later = selectAccountNotices(
            listOf(row(2, AccountNoticeKind.EmailAdded, atMs = at + OWN_ACTION_WINDOW_MS + 1)),
            nextAfter = 2,
            cursor = 0,
            ownActions = mapOf(AccountNoticeKind.EmailAdded to at),
        )
        assertEquals(listOf(2L), later.show.map { it.id })
        // Two of the kind in the window: the first is this device's, the
        // second is shown.
        val two = selectAccountNotices(
            listOf(
                row(2, AccountNoticeKind.PasswordChangedByEmail, atMs = at),
                row(3, AccountNoticeKind.PasswordChangedByEmail, atMs = at + 1_000L),
            ),
            nextAfter = 3,
            cursor = 0,
            ownActions = mapOf(AccountNoticeKind.PasswordChangedByEmail to at),
        )
        assertEquals(listOf(3L), two.show.map { it.id })
    }

    @Test
    fun `two kinds have a title and a dated body, the rest one line`() {
        assertEquals(AccountNoticeShape.Alarm, accountNoticeShape(AccountNoticeKind.PasswordChangedByEmail))
        assertEquals(AccountNoticeShape.Alarm, accountNoticeShape(AccountNoticeKind.AccountOpenedByEmail))
        for (kind in listOf(
            AccountNoticeKind.EmailAdded, AccountNoticeKind.EmailReplaced,
            AccountNoticeKind.EmailRemoved, AccountNoticeKind.PhraseSaved,
        )) {
            assertEquals(AccountNoticeShape.Line, accountNoticeShape(kind))
            assertNull(accountNoticeBodyKey(kind))
        }
        assertEquals("notice.password_by_email.title", accountNoticeTitleKey(AccountNoticeKind.PasswordChangedByEmail))
        assertEquals("notice.password_by_email.body", accountNoticeBodyKey(AccountNoticeKind.PasswordChangedByEmail))
        assertEquals("notice.opened_by_email.body", accountNoticeBodyKey(AccountNoticeKind.AccountOpenedByEmail))
        assertEquals("notice.phrase_saved", accountNoticeTitleKey(AccountNoticeKind.PhraseSaved))
    }

    @Test
    fun `it was not me opens the one secure screen for every kind`() {
        for (kind in AccountNoticeKind.entries) {
            assertEquals(NotMeDestination.SecureAccount, notMeDestination(kind))
        }
    }

    @Test
    fun `the secure screen offers save a phrase only while none is saved`() {
        val mail = RecoveryEmailStatus(available = true, enabled = true, vault = true, phraseSaved = false)
        assertEquals(SecureAccountRows(newPassword = true, removeEmail = true, savePhrase = true), secureAccountRows(mail))
        assertEquals(
            SecureAccountRows(newPassword = true, removeEmail = true, savePhrase = false),
            secureAccountRows(mail.copy(vault = false, phraseSaved = true)),
        )
        assertEquals(
            SecureAccountRows(newPassword = true, removeEmail = false, savePhrase = true),
            secureAccountRows(mail.copy(enabled = false, vault = false)),
        )
        // Not read yet: the password row alone is certain.
        assertEquals(SecureAccountRows(newPassword = true, removeEmail = false, savePhrase = true), secureAccountRows(null))
    }

    @Test
    fun `the store keeps the cursor and the own markers per account`() {
        val store = AccountNoticeStore(MapStorage())
        assertNull(store.cursor("a"))
        store.setCursor("a", 12L)
        store.markOwn("a", AccountNoticeKind.EmailAdded, 100L)
        store.markOwn("b", AccountNoticeKind.EmailAdded, 200L)
        assertEquals(12L, store.cursor("a"))
        assertEquals(mapOf(AccountNoticeKind.EmailAdded to 100L), store.ownActions("a"))
        store.clearOwn("a", setOf(AccountNoticeKind.EmailAdded))
        assertTrue(store.ownActions("a").isEmpty())
        // An unmatched marker is dropped after a day.
        store.markOwn("a", AccountNoticeKind.PhraseSaved, 0L)
        store.pruneOwn("a", AccountNoticeStore.OWN_ACTION_KEEP_MS)
        assertFalse(store.ownActions("a").isEmpty())
        store.pruneOwn("a", AccountNoticeStore.OWN_ACTION_KEEP_MS + 1)
        assertTrue(store.ownActions("a").isEmpty())
        store.clearAccount("a")
        assertNull(store.cursor("a"))
        assertEquals(mapOf(AccountNoticeKind.EmailAdded to 200L), store.ownActions("b"))
    }
}
