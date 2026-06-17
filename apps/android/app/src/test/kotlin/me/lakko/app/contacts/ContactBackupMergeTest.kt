// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.BackupContactRecord

/**
 * Host-side unit tests for the contact-in-backup restore merge
 * ([reconstructRestoredContacts], ADR-0021). Runs as
 * `:app:testDebugUnitTest` — the helper + the `BackupContactRecord` /
 * `AcceptedContact` / `IncomingContactRequest` data classes are plain Kotlin,
 * so no emulator + no UniFFI native-library load is needed.
 */
class ContactBackupMergeTest {
    private fun record(
        edByte: Byte,
        name: String,
        accountId: ByteArray?,
    ): BackupContactRecord =
        BackupContactRecord(
            ed25519Pubkey = ByteArray(32) { edByte },
            x25519Pubkey = ByteArray(32),
            displayName = name,
            accountId = accountId,
        )

    @Test
    fun `reconstructs a contact with account_id and display name`() {
        val accountId = ByteArray(16) { 0xAB.toByte() }
        val out =
            reconstructRestoredContacts(
                existing = emptyList(),
                restored = listOf(record(0x01, "Alice", accountId)),
                createdAtMs = 1_700_000_000_000uL,
            )
        assertEquals(1, out.size)
        val c = out[0]
        assertEquals("Alice", c.displayName)
        assertTrue(c.request.fromAccountId!!.contentEquals(accountId))
        assertTrue(c.request.fromPubkey.contentEquals(ByteArray(32) { 0x01 }))
        // The reconstructed request carries the snapshot timestamp.
        assertEquals(1_700_000_000_000uL, c.request.validAfterMs)
    }

    @Test
    fun `an empty display name round-trips to null`() {
        val out =
            reconstructRestoredContacts(
                existing = emptyList(),
                restored = listOf(record(0x02, "", null)),
                createdAtMs = 1uL,
            )
        assertEquals(1, out.size)
        assertNull("an empty backup name must not become a chosen name", out[0].displayName)
    }

    @Test
    fun `a null account_id stays null`() {
        val out =
            reconstructRestoredContacts(
                existing = emptyList(),
                restored = listOf(record(0x03, "Bob", null)),
                createdAtMs = 1uL,
            )
        assertNull(out[0].request.fromAccountId)
    }

    @Test
    fun `a pubkey already present is skipped (no clobber)`() {
        val existingPubkey = ByteArray(32) { 0x04 }
        val existing =
            listOf(
                AcceptedContact(
                    request =
                        IncomingContactRequest(
                            messageId = 42L,
                            fromPubkey = existingPubkey,
                            fromAccountId = null,
                            fromPseudonym = ByteArray(32),
                            validAfterMs = 0uL,
                            intro = null,
                            createdAt = "",
                        ),
                    groupIdHex = "abcd",
                    displayName = "Carol (live)",
                ),
            )
        val out =
            reconstructRestoredContacts(
                existing = existing,
                restored =
                    listOf(
                        record(0x04, "Carol (stale backup)", null), // same pubkey -> skip
                        record(0x05, "Dave", null), // new -> keep
                    ),
                createdAtMs = 1uL,
            )
        assertEquals("only the new pubkey is reconstructed", 1, out.size)
        assertEquals("Dave", out[0].displayName)
    }

    @Test
    fun `sentinel messageIds are negative and distinct`() {
        val out =
            reconstructRestoredContacts(
                existing = emptyList(),
                restored =
                    listOf(
                        record(0x06, "A", null),
                        record(0x07, "B", null),
                        record(0x08, "C", null),
                    ),
                createdAtMs = 1uL,
            )
        val ids = out.map { it.request.messageId }
        assertEquals(3, ids.size)
        assertTrue("all sentinel ids are negative", ids.all { it < 0 })
        assertEquals("sentinel ids are distinct", ids.size, ids.toSet().size)
    }
}
