// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import java.util.Base64
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0098 — the receive rule for an account-deleted statement, its wire
 * peek, and what a deleted one-to-one shows. The verifier is the core's
 * `verifyAccountDeletedEnvelope`, faked here because the host suite does not
 * load the native library; the rule under test is what the client hands it
 * and what it does with the answer.
 */
class AccountDeletedTest {

    private val senderKey = ByteArray(32) { 7 }
    private val senderB64 = Base64.getEncoder().encodeToString(senderKey)
    private val groupId = ByteArray(32) { 0x33 }
    private val envelope = ACCOUNT_DELETED_ENVELOPE_DOMAIN + byteArrayOf(1) + ByteArray(64)

    /** Verifies only the statement of [senderKey] over [groupId]. */
    private val verify: (ByteArray, ByteArray, ByteArray) -> Boolean = { env, group, key ->
        env.contentEquals(envelope) && group.contentEquals(groupId) && key.contentEquals(senderKey)
    }

    @Test
    fun `a statement verified for its sender and this group marks the sender`() {
        assertEquals(senderB64, acceptAccountDeletedStatement(envelope, groupId, senderB64, verify))
    }

    @Test
    fun `a wrong sender or a bad signature changes nothing`() {
        val other = Base64.getEncoder().encodeToString(ByteArray(32) { 8 })
        assertNull(acceptAccountDeletedStatement(envelope, groupId, other, verify))
        val tampered = envelope.copyOf().also { it[it.size - 1] = 1 }
        assertNull(acceptAccountDeletedStatement(tampered, groupId, senderB64, verify))
        // Moved into another conversation.
        assertNull(acceptAccountDeletedStatement(envelope, ByteArray(32) { 0x34 }, senderB64, verify))
    }

    @Test
    fun `an unresolved or malformed sender is dropped without asking the core`() {
        var asked = 0
        val counting: (ByteArray, ByteArray, ByteArray) -> Boolean = { _, _, _ -> asked++; true }
        assertNull(acceptAccountDeletedStatement(envelope, groupId, null, counting))
        assertNull(acceptAccountDeletedStatement(envelope, groupId, "", counting))
        assertNull(acceptAccountDeletedStatement(envelope, groupId, "not base64!", counting))
        assertNull(acceptAccountDeletedStatement(envelope, groupId, Base64.getEncoder().encodeToString(ByteArray(16)), counting))
        assertNull(acceptAccountDeletedStatement(envelope, groupId, OWN_REACTION_SENTINEL, counting))
        assertEquals(0, asked)
    }

    @Test
    fun `a verifier that throws is a refusal`() {
        assertNull(acceptAccountDeletedStatement(envelope, groupId, senderB64) { _, _, _ -> error("boom") })
    }

    @Test
    fun `the peek matches the frozen domain and nothing a chat envelope starts with`() {
        assertEquals(25, ACCOUNT_DELETED_ENVELOPE_DOMAIN.size)
        assertTrue(looksLikeAccountDeletedEnvelope(envelope))
        assertFalse(looksLikeAccountDeletedEnvelope("{\"v\":1}".toByteArray()))
        assertFalse(looksLikeAccountDeletedEnvelope("lakko/account-deleted/v1".toByteArray()))
        assertFalse(looksLikeAccountDeletedEnvelope(GROUP_CLOSED_ENVELOPE_DOMAIN + ByteArray(10)))
    }

    @Test
    fun `a deleted one-to-one has no composer, only the one line, and no draft block`() {
        val slot = composerState(MembershipState.Active, draft = "half a sentence", peerDeleted = true)
        assertTrue(slot is ComposerSlot.Withdrawn)
        slot as ComposerSlot.Withdrawn
        assertEquals("person.deleted.body", slot.notice.key)
        assertNull(slot.draft)
        assertNull(slot.draftLabel)
        assertEquals(ComposerSlot.Composer, composerState(MembershipState.Active, draft = "", peerDeleted = false))
    }

    @Test
    fun `a deleted one-to-one has no call buttons`() {
        assertTrue(callButtonsHidden(callOccupied = false, membershipEnded = false, callControls = true, peerDeleted = true))
        assertFalse(callButtonsHidden(callOccupied = false, membershipEnded = false, callControls = true, peerDeleted = false))
    }
}
