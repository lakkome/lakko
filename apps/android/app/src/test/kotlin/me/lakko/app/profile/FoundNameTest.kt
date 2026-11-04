// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import java.util.Base64
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import me.lakko.app.contacts.FoundName
import me.lakko.app.contacts.IncomingContactAck
import me.lakko.app.contacts.SentContactRequest
import me.lakko.app.contacts.foundNamesOfAnswers
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/who-can-find-me.md` section 6.1 — a person who answered a request is
 * a contact before their profile arrives, and the row is not a stand-in
 * meanwhile: the name they were found under is filed as a profile at stamp 0,
 * which any real profile replaces. Seen live 2026-10-31: the row read `Member`
 * under `#` for about 4 s, and for good when the Welcome failed.
 */
class FoundNameTest {

    private val keyBytes = ByteArray(32) { 0x42 }
    private val keyB64 = Base64.getEncoder().encodeToString(keyBytes)
    private val otherKey = ByteArray(32) { 0x43 }
    private val accountB64 = Base64.getEncoder().encodeToString(ByteArray(16) { 5 })

    private fun record(name: String? = "Elin Holm", username: String = "elin", key: String? = keyB64) =
        SentContactRequest(
            recipientUsername = username,
            recipientAccountIdB64 = accountB64,
            messageId = 7L,
            createdAt = "2026-10-31T00:00:00Z",
            recipientKeyB64 = key,
            recipientName = name,
        )

    private fun ack(key: ByteArray = keyBytes) = IncomingContactAck(
        messageId = 40L,
        accepterPubkey = key,
        accepterAccountId = ByteArray(16) { 5 },
        accepterX25519Pubkey = ByteArray(32),
        ackToPseudonym = ByteArray(32),
        acceptedAtMs = 1uL,
        createdAt = "",
    )

    private fun directory(persistence: ProfilePersistence = InMemoryProfilePersistence()) = ProfileDirectory(
        persistence = persistence,
        scope = CoroutineScope(Dispatchers.Unconfined),
        isNewer = { a, b -> a > b },
        passesRule = { true },
    )

    // ---- what a Sent record gives ----

    @Test
    fun `an ack gives the name and the username the row showed`() {
        assertEquals(
            listOf(FoundName(keyB64, "Elin Holm", "elin")),
            foundNamesOfAnswers(listOf(record()), listOf(ack()), emptySet()),
        )
    }

    @Test
    fun `a Welcome from the person asked gives it too`() {
        assertEquals(
            listOf(FoundName(keyB64, "Elin Holm", "elin")),
            foundNamesOfAnswers(listOf(record()), emptyList(), setOf(keyB64)),
        )
    }

    @Test
    fun `a person found by name alone has no username`() {
        assertEquals(
            listOf(FoundName(keyB64, "Elin Holm", null)),
            foundNamesOfAnswers(listOf(record(username = "")), listOf(ack()), emptySet()),
        )
    }

    @Test
    fun `an empty name on the record gives nothing`() {
        assertTrue(foundNamesOfAnswers(listOf(record(name = null)), listOf(ack()), emptySet()).isEmpty())
        assertTrue(foundNamesOfAnswers(listOf(record(name = "  ")), listOf(ack()), emptySet()).isEmpty())
    }

    @Test
    fun `a row that showed the username alone gives the username and no name`() {
        assertEquals(
            listOf(FoundName(keyB64, null, "elin")),
            foundNamesOfAnswers(listOf(record(name = "@elin")), listOf(ack()), emptySet()),
        )
    }

    @Test
    fun `a name that is only the key's fingerprint gives nothing`() {
        val fingerprint = fingerprintLabel(keyBytes)
        assertTrue(foundNamesOfAnswers(listOf(record(name = fingerprint)), listOf(ack()), emptySet()).isEmpty())
    }

    @Test
    fun `an ack from another key names no one, whatever account it claims`() {
        // The ack names the record's account but is signed by another key.
        assertTrue(foundNamesOfAnswers(listOf(record()), listOf(ack(otherKey)), emptySet()).isEmpty())
        // A record that never learned the key cannot be tied to one.
        assertTrue(foundNamesOfAnswers(listOf(record(key = null)), listOf(ack()), emptySet()).isEmpty())
    }

    // ---- what the directory does with it ----

    @Test
    fun `the new contact reads the found name, not the stand-in`() {
        val directory = directory()
        assertTrue(directory.resolve(keyB64).isPlaceholder)

        assertEquals(ProfileOffer.STORED, directory.onFoundPersonAnswered(keyB64, "Elin Holm", "elin"))

        val name = directory.resolve(keyB64)
        assertEquals("Elin Holm", name.primary)
        assertFalse(name.isPlaceholder)
        assertEquals("elin", name.verifiedUsername)
    }

    @Test
    fun `a found username never reads as not verified`() {
        val directory = directory()
        directory.onFoundPersonAnswered(keyB64, "Elin Holm", "elin")
        assertFalse(directory.resolve(keyB64).usernameNotVerified)
        assertEquals(UsernameVerification.VERIFIED, directory.verifications.get(keyB64, "elin"))
    }

    @Test
    fun `a found name is still counted as unnamed for the profile request`() {
        val directory = directory()
        directory.onFoundPersonAnswered(keyB64, "Elin Holm", "elin")
        assertEquals(1, countUnnamedMembers(listOf(keyBytes), null, directory::awaitsProfile))
    }

    @Test
    fun `a real profile, or the viewer's own name for them, ends the asking`() {
        val received = directory()
        received.onFoundPersonAnswered(keyB64, "Elin Holm", null)
        received.onProfileReceived(keyB64, PeerProfile("Elin H.", null, 1uL))
        assertFalse(received.awaitsProfile(keyBytes))

        val renamed = directory()
        renamed.onFoundPersonAnswered(keyB64, "Elin Holm", null)
        renamed.setRenames(mapOf(keyB64 to "Sis"))
        assertFalse(renamed.awaitsProfile(keyBytes))
    }

    @Test
    fun `nobody named at all is still awaited`() {
        assertTrue(directory().awaitsProfile(keyBytes))
    }

    @Test
    fun `a name found without a username shows no username`() {
        val directory = directory()
        directory.onFoundPersonAnswered(keyB64, "Elin Holm", null)
        assertEquals("Elin Holm", directory.resolve(keyB64).primary)
        assertNull(directory.resolve(keyB64).verifiedUsername)
    }

    @Test
    fun `a person found by username alone reads as the username`() {
        val directory = directory()
        directory.onFoundPersonAnswered(keyB64, null, "elin")
        assertEquals("@elin", directory.resolve(keyB64).primary)
        assertFalse(directory.resolve(keyB64).isPlaceholder)
    }

    @Test
    fun `any real profile replaces it`() {
        val directory = directory()
        directory.onFoundPersonAnswered(keyB64, "Elin Holm", "elin")

        val offer = directory.onProfileReceived(keyB64, PeerProfile("Elin H.", null, 1uL))

        assertEquals(ProfileOffer.STORED, offer)
        assertEquals("Elin H.", directory.resolve(keyB64).primary)
        assertNull(directory.resolve(keyB64).verifiedUsername)
    }

    @Test
    fun `an account that never set a name replaces a found name at stamp 0`() {
        val directory = directory()
        directory.onFoundPersonAnswered(keyB64, "Elin Holm", null)

        // What a client with no name of its own sends: stamp 0, its username
        // and account id.
        val own = PeerProfile(null, "elin", 0uL, accountIdHex = "0a".repeat(16))
        assertEquals(ProfileOffer.STORED, directory.onProfileReceived(keyB64, own))

        assertEquals(own, directory.profiles.get(keyB64))
        assertFalse(directory.profiles.get(keyB64)!!.isFoundName)
    }

    @Test
    fun `a profile saying nothing leaves a found name standing`() {
        val directory = directory()
        directory.onFoundPersonAnswered(keyB64, "Elin Holm", null)

        directory.onProfileReceived(keyB64, PeerProfile(null, null, 0uL, accountIdHex = "0a".repeat(16)))

        assertEquals("Elin Holm", directory.resolve(keyB64).primary)
    }

    @Test
    fun `a second found name does not replace the first`() {
        val directory = directory()
        directory.onFoundPersonAnswered(keyB64, "Elin Holm", null)
        assertEquals(ProfileOffer.NOT_NEWER, directory.onFoundPersonAnswered(keyB64, "E", null))
        assertEquals("Elin Holm", directory.resolve(keyB64).primary)
    }

    @Test
    fun `a found name does not replace a real profile at stamp 0`() {
        val directory = directory()
        val own = PeerProfile(null, "elin", 0uL, accountIdHex = "0a".repeat(16))
        directory.onProfileReceived(keyB64, own)
        assertEquals(ProfileOffer.NOT_NEWER, directory.onFoundPersonAnswered(keyB64, "Elin Holm", null))
        assertEquals(own, directory.profiles.get(keyB64))
    }

    @Test
    fun `a pairing bundle carries a found name without its username`() {
        val found = PeerProfile("Elin Holm", "elin", 0uL)
        assertEquals(PeerProfile("Elin Holm", null, 0uL), found.forStateBundle)
        // The username was all there was: nothing to carry.
        assertNull(PeerProfile(null, "elin", 0uL).forStateBundle)
        // A real profile goes as it is, at stamp 0 too.
        val real = PeerProfile(null, "elin", 0uL, accountIdHex = "0a".repeat(16), usernameCertificateHex = "ab")
        assertEquals(real, real.forStateBundle)
        val named = PeerProfile("Elin", "elin", 9uL)
        assertEquals(named, named.forStateBundle)
    }

    @Test
    fun `a found name carried to a new device is still replaced by a real profile`() {
        val directory = directory()
        val carried = PeerProfile("Elin Holm", "elin", 0uL).forStateBundle!!
        directory.onProfileReceived(keyB64, carried)
        assertTrue(directory.awaitsProfile(keyBytes))
        val own = PeerProfile(null, "elin", 0uL, accountIdHex = "0a".repeat(16))
        assertEquals(ProfileOffer.STORED, directory.onProfileReceived(keyB64, own))
    }

    @Test
    fun `a profile held already stays`() {
        val directory = directory()
        directory.onProfileReceived(keyB64, PeerProfile("Elin H.", null, 5uL))

        assertEquals(ProfileOffer.NOT_NEWER, directory.onFoundPersonAnswered(keyB64, "Elin Holm", "elin"))

        assertEquals("Elin H.", directory.resolve(keyB64).primary)
        // Nothing was verified for a claim that was not filed.
        assertNull(directory.verifications.get(keyB64, "elin"))
    }

    @Test
    fun `it survives a relaunch and a real profile still replaces it then`() {
        val persistence = InMemoryProfilePersistence()
        directory(persistence).onFoundPersonAnswered(keyB64, "Elin Holm", "elin")

        val relaunched = directory(persistence)
        assertEquals("Elin Holm", relaunched.resolve(keyB64).primary)
        assertEquals("elin", relaunched.resolve(keyB64).verifiedUsername)

        relaunched.onProfileReceived(keyB64, PeerProfile("Elin H.", null, 1uL))
        assertEquals("Elin H.", relaunched.resolve(keyB64).primary)
    }

    @Test
    fun `no account signed in files nothing`() {
        val directory = directory(InMemoryProfilePersistence(account = null))
        assertEquals(ProfileOffer.NO_ACCOUNT, directory.onFoundPersonAnswered(keyB64, "Elin Holm", "elin"))
        assertTrue(directory.resolve(keyB64).isPlaceholder)
    }
}
