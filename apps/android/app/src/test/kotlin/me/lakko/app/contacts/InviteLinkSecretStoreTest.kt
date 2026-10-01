// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.discovery.InviteLinkLengths
import me.lakko.app.discovery.heldInviteLinkUrl
import me.lakko.app.l10n.en
import me.lakko.app.ui.chat.InMemoryPreferences
import me.lakko.app.ui.contacts.INVITE_CREATED_ELSEWHERE_COPY
import me.lakko.app.ui.contacts.InviteLinkRow
import me.lakko.app.ui.contacts.InviteRowShare
import me.lakko.app.ui.contacts.inviteRowShare
import me.lakko.app.ui.contacts.mergeInviteLinkRows
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * docs/ux/client-redesign.md section 5.3 — the device that created an `/i/`
 * link keeps its secret per account across restarts, a revoke and a
 * forget-device delete it, and a listed row shows Copy / Share only when the
 * secret is held here.
 */
class InviteLinkSecretStoreTest {

    private val lengths = InviteLinkLengths(blobIdLen = 6, blobIdUrlLen = 8, linkSecretLen = 12, linkSecretUrlLen = 16)
    private val secretText = base64UrlEncodeNoPad(ByteArray(12) { (it + 1).toByte() })

    @Test
    fun `a stored secret survives a restart over the same file`() {
        val file = InMemoryPreferences()
        SharedPrefsInviteLinkSecretStore { file }.put("acct", "AbCdEfGh", secretText)
        // A second instance over the same file is what a restart looks like.
        val restarted = SharedPrefsInviteLinkSecretStore { file }
        assertEquals(secretText, restarted.get("acct", "AbCdEfGh"))
        assertEquals(mapOf("AbCdEfGh" to secretText), restarted.all("acct"))
    }

    @Test
    fun `revoke deletes one secret and forget-device every secret of the account`() {
        val file = InMemoryPreferences()
        val store = SharedPrefsInviteLinkSecretStore { file }
        store.put("acct", "one11111", secretText)
        store.put("acct", "two22222", secretText)
        store.put("acctB", "three333", secretText)
        store.remove("acct", "one11111")
        assertNull(store.get("acct", "one11111"))
        assertEquals(setOf("two22222"), store.all("acct").keys)
        store.clearForAccount("acct")
        assertTrue(store.all("acct").isEmpty())
        // Another account's secrets, including one whose id starts with this
        // one's, are untouched.
        assertEquals(setOf("three333"), store.all("acctB").keys)
    }

    @Test
    fun `the in-memory store follows the same rules`() {
        val store = InMemoryInviteLinkSecretStore()
        store.put("acct", "one11111", secretText)
        store.put("acctB", "two22222", secretText)
        assertEquals(secretText, store.get("acct", "one11111"))
        store.clearForAccount("acct")
        assertNull(store.get("acct", "one11111"))
        assertEquals(secretText, store.get("acctB", "two22222"))
    }

    @Test
    fun `a held secret rebuilds the full link, anything else offers none`() {
        assertEquals(
            "https://lakko.me/i/AbCdEfGh#$secretText",
            heldInviteLinkUrl("https://lakko.me", "/i/", "AbCdEfGh", secretText, lengths),
        )
        assertNull(heldInviteLinkUrl("https://lakko.me", "/i/", "AbCdEfGh", null, lengths))
        assertNull(heldInviteLinkUrl("https://lakko.me", "/i/", "AbCdEfGh", "short", lengths))
        assertNull(heldInviteLinkUrl("https://lakko.me", "/i/", "AbCdEfGh", "!".repeat(16), lengths))
    }

    private fun serverRow(slug: String, url: String?) = InviteLinkRow(
        slug = slug,
        url = url,
        label = "",
        createdAtMs = null,
        expiryMs = 0L,
        usesLeft = null,
        fromSession = false,
    )

    @Test
    fun `a listed row whose secret is held shows Copy and Share with the full URL`() {
        val url = "https://lakko.me/i/AbCdEfGh#$secretText"
        assertEquals(InviteRowShare.Shareable(url), inviteRowShare(serverRow("AbCdEfGh", url)))
    }

    @Test
    fun `a listed row without a held secret shows the created-elsewhere line`() {
        assertEquals(InviteRowShare.CreatedElsewhere, inviteRowShare(serverRow("AbCdEfGh", null)))
        assertEquals(
            "Created on another device. Copy it there, or revoke it here.",
            INVITE_CREATED_ELSEWHERE_COPY.en,
        )
    }

    @Test
    fun `merging keeps a server row's held URL`() {
        val url = "https://lakko.me/i/AbCdEfGh#$secretText"
        val merged = mergeInviteLinkRows(session = emptyList(), server = listOf(serverRow("AbCdEfGh", url), serverRow("zzzzzzzz", null)))
        assertEquals(url, merged.first { it.slug == "AbCdEfGh" }.url)
        assertNull(merged.first { it.slug == "zzzzzzzz" }.url)
    }
}
