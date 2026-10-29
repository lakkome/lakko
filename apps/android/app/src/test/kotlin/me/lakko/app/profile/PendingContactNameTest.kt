// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import java.util.Base64
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import me.lakko.app.contacts.PendingContact
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0079 — a pending contact (an inviter added through their link, no
 * one-to-one yet) is named in every roster by what the link states, below a
 * local rename and a received profile, the same way the contact list names
 * it. Seen live 2026-10-29: such a person read "Member" in a group roster
 * while the contact list showed their name.
 */
class PendingContactNameTest {

    private val keyBytes = ByteArray(32) { 0x42 }
    private val keyB64 = Base64.getEncoder().encodeToString(keyBytes)

    private fun pending(name: String = "VG2", username: String = "vg2", key: String = keyB64) = PendingContact(
        accountIdHex = "0a".repeat(16),
        signaturePubkeyB64 = key,
        x25519PubkeyB64 = Base64.getEncoder().encodeToString(ByteArray(32) { 1 }),
        displayName = name,
        username = username,
        addedAtMs = 1L,
        linkIdHex = "0b".repeat(16),
    )

    private fun directory(): ProfileDirectory = ProfileDirectory(
        persistence = InMemoryProfilePersistence(),
        scope = CoroutineScope(Dispatchers.Unconfined),
        isNewer = { a, b -> a > b },
        passesRule = { true },
    )

    @Test
    fun `a key known only as a pending contact resolves to the link's name`() {
        val directory = directory()
        assertTrue(directory.resolve(keyB64).isPlaceholder)

        directory.setPendingContacts(listOf(pending()))

        val name = directory.resolve(keyB64)
        assertEquals("VG2", name.primary)
        assertFalse(name.isPlaceholder)
        // The stated username is a claim: never shown as verified without a check.
        assertNull(name.verifiedUsername)
        // The link statement is a profile input, not a rename.
        assertNull(name.theirName)
    }

    @Test
    fun `a received profile wins over the link statement`() {
        val directory = directory()
        directory.setPendingContacts(listOf(pending()))
        directory.onProfileReceived(keyB64, PeerProfile("Valentin", null, 5uL))
        assertEquals("Valentin", directory.resolve(keyB64).primary)
    }

    @Test
    fun `a local rename wins over the link statement`() {
        val directory = directory()
        directory.setPendingContacts(listOf(pending()))
        directory.setRenames(mapOf(keyB64 to "My friend"))
        val name = directory.resolve(keyB64)
        assertEquals("My friend", name.primary)
        // The link statement is not the person's own profile name.
        assertEquals("VG2", name.theirName)
    }

    @Test
    fun `removing the pending contact goes back to the placeholder and re-resolves`() {
        val directory = directory()
        val v0 = directory.version.value
        directory.setPendingContacts(listOf(pending()))
        val v1 = directory.version.value
        assertTrue(v1 > v0)
        // The same list again changes nothing.
        directory.setPendingContacts(listOf(pending()))
        assertEquals(v1, directory.version.value)

        directory.setPendingContacts(emptyList())
        assertTrue(directory.version.value > v1)
        val name = directory.resolve(keyB64)
        assertTrue(name.isPlaceholder)
        assertEquals("Member", name.primary)
    }

    @Test
    fun `no check is started for a link-stated username`() {
        val directory = directory()
        val calls = java.util.concurrent.atomic.AtomicInteger(0)
        directory.attachUsernameResolver {
            calls.incrementAndGet()
            UsernameResolution.NotRegistered
        }
        directory.setPendingContacts(listOf(pending(name = "")))
        // A link carries no certificate: nothing is looked up, nothing is
        // marked "not verified", and a username-only link stays the placeholder.
        assertEquals(0, calls.get())
        val name = directory.resolve(keyB64)
        assertTrue(name.isPlaceholder)
        assertFalse(name.usernameNotVerified)
        assertNull(directory.claimedUsernameOf(keyB64))
        directory.ensureVerified(keyB64)
        assertEquals(0, calls.get())
    }

    @Test
    fun `a link-stated username shows when the pair is already verified`() {
        val directory = directory()
        directory.verifications.put(keyB64, "vg2", UsernameVerification.VERIFIED)
        directory.setPendingContacts(listOf(pending(name = "")))
        val name = directory.resolve(keyB64)
        assertEquals("@vg2", name.primary)
        assertEquals("vg2", name.verifiedUsername)
        directory.setPendingContacts(listOf(pending()))
        assertEquals("VG2 · @vg2", directory.resolve(keyB64).listLabel)
    }

    @Test
    fun `a pending contact whose link states nothing is not a name`() {
        assertEquals(emptyMap<String, PeerProfile>(), pendingStatementsOf(listOf(pending(name = "  ", username = ""))))
        assertEquals(emptyMap<String, PeerProfile>(), pendingStatementsOf(listOf(pending(key = ""))))
        val statement = pendingStatementsOf(listOf(pending(name = " VG2 ")))[keyB64]
        assertEquals("VG2", statement?.displayName)
        assertEquals("vg2", statement?.username)
    }

    @Test
    fun `the background snapshot knows a pending contact and still prefers a profile`() {
        val other = Base64.getEncoder().encodeToString(ByteArray(32) { 3 })
        val snapshot = ProfileNameSnapshot(
            profiles = mapOf(other to PeerProfile("Bob", null, 1uL)),
            verifications = emptyMap(),
            renames = emptyMap(),
            pendingStatements = pendingStatementsOf(listOf(pending(), pending(name = "Stale", key = other))),
        )
        assertTrue(snapshot.isKnown(keyB64))
        assertEquals("VG2", snapshot.resolve(keyB64).primary)
        assertEquals("Bob", snapshot.resolve(other).primary)
        val empty = ProfileNameSnapshot(emptyMap(), emptyMap(), emptyMap())
        assertFalse(empty.isKnown(keyB64))
    }
}
