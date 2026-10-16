// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import java.util.Base64
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.profile.InMemoryProfilePersistence
import me.lakko.app.profile.PeerProfile
import me.lakko.app.profile.ProfileDirectory
import me.lakko.app.profile.UsernameResolution
import me.lakko.app.profile.UsernameVerification
import me.lakko.app.profile.resolveName
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The name on a remote call tile — `docs/ux/calls.md`, "Every tile carries the
 * person's name" — for both call kinds, and the two link-call mechanics that
 * keep a tile from staying unnamed or being named for somebody else.
 *
 * Who a peer is comes from its verified announcement (ADR-0078 section 2),
 * given here as `peerIdentified`, so no UniFFI `.so` is needed.
 */
class CallTileNameTest {
    init {
        // ADR-0061: the words under test are the English catalog's.
        EnglishForTests.install()
    }

    private val key: ByteArray = ByteArray(32) { 0xa1.toByte() }
    private val keyB64: String = Base64.getEncoder().encodeToString(key)

    private val otherKey: ByteArray = ByteArray(32) { 0xb2.toByte() }
    private val otherKeyB64: String = Base64.getEncoder().encodeToString(otherKey)
    private val ownerAccount: ByteArray = ByteArray(16) { 0x0a }

    private fun directory(): ProfileDirectory = ProfileDirectory(
        persistence = InMemoryProfilePersistence(),
        scope = CoroutineScope(Dispatchers.Unconfined),
        isNewer = { incoming, stored -> incoming > stored },
        passesRule = { it.matches(Regex("[a-z0-9_]{3,32}")) },
    )

    /** The production wiring of `callPeerNames`, over [directory]. */
    private fun resolver(directory: ProfileDirectory): CallPeerNames = CallPeerNames(
        personName = { keyB64 -> personNameOrNull(directory.resolve(keyB64)) },
        storedUsername = { keyB64 -> directory.claimedUsernameOf(keyB64) },
    )

    // ---- the rule --------------------------------------------------------

    @Test
    fun `the one resolver's name comes first`() {
        assertEquals(
            "Anna (work)",
            callTileName(resolved = "Anna (work)", announcedName = "Anna", claimedUsername = "anna"),
        )
    }

    @Test
    fun `the name announced in the call comes before the claimed username`() {
        assertEquals(
            "Guest",
            callTileName(resolved = null, announcedName = "Guest", claimedUsername = "guest_1"),
        )
    }

    @Test
    fun `a person with no profile name is named by the username they claim, without the at sign`() {
        assertEquals(
            "valentin",
            callTileName(resolved = null, announcedName = null, claimedUsername = "valentin"),
        )
    }

    @Test
    fun `a tile is unnamed only when nothing has arrived`() {
        assertNull(callTileName(resolved = null, announcedName = null, claimedUsername = null))
        assertNull(callTileName(resolved = "  ", announcedName = "", claimedUsername = " "))
    }

    // ---- what the one resolver contributes -------------------------------

    @Test
    fun `a verified username is shown with the at sign`() {
        val resolved = resolveName(
            localRename = null,
            profile = PeerProfile(null, "valentin", 1uL),
            verificationOf = { UsernameVerification.VERIFIED },
            signatureKey = key,
        )
        assertEquals("@valentin", personNameOrNull(resolved))
    }

    @Test
    fun `a username that is not verified is left to the plain-text rule`() {
        for (verification in listOf(null, UsernameVerification.FAILED)) {
            val resolved = resolveName(
                localRename = null,
                profile = PeerProfile(null, "valentin", 1uL),
                verificationOf = { verification },
                signatureKey = key,
            )
            assertNull(personNameOrNull(resolved))
        }
    }

    @Test
    fun `the fingerprint and the placeholder word are never returned`() {
        val resolved = resolveName(
            localRename = null,
            profile = null,
            verificationOf = { null },
            signatureKey = key,
        )
        assertEquals("a1a1 a1a1", resolved.fingerprint)
        assertNull(personNameOrNull(resolved))
    }

    @Test
    fun `the viewer's rename comes before the person's own name`() {
        val resolved = resolveName(
            localRename = "Mum",
            profile = PeerProfile("Elena", null, 1uL),
            verificationOf = { null },
            signatureKey = key,
        )
        assertEquals("Mum", personNameOrNull(resolved))
    }

    @Test
    fun `a stored profile names its person by the plain username until it verifies`() {
        val directory = directory()
        assertNull(callNameOfPerson(directory, keyB64))
        directory.onProfileReceived(keyB64, PeerProfile(null, "alice", 1uL))
        // No verification has run: the claim reads as a bare word.
        assertEquals("alice", callNameOfPerson(directory, keyB64))
        directory.attachUsernameResolver { UsernameResolution.Found(keyB64) }
        assertEquals("@alice", callNameOfPerson(directory, keyB64))
    }

    @Test
    fun `a claim that failed verification never gains the at sign`() {
        val directory = directory()
        directory.onProfileReceived(keyB64, PeerProfile(null, "pavel", 1uL))
        directory.attachUsernameResolver { UsernameResolution.Found("SOMEONE_ELSE") }
        assertEquals("pavel", callNameOfPerson(directory, keyB64))
    }

    // ---- a group call ----------------------------------------------------

    @Test
    fun `a peer nobody can name has an empty name, with no number and no id`() {
        val names = resolver(directory())
        assertEquals("", names.name("p1"))
        assertEquals("", names.name("p2"))
        // Asked again (the welcome snapshot, then peer-joined): still nothing.
        assertEquals("", names.name("p1"))
    }

    @Test
    fun `a peer is named only once its announcement verifies`() {
        val directory = directory()
        directory.onProfileReceived(keyB64, PeerProfile("Anna", null, 1uL))
        val names = resolver(directory)
        assertEquals("", names.name("p1"))
        assertTrue(names.peerIdentified("p1", keyB64, "aa"))
        assertEquals("Anna", names.name("p1"))
        assertFalse("a re-announcement changes nothing", names.peerIdentified("p1", keyB64, "aa"))
    }

    @Test
    fun `a group member with a stored username and no display name is named by it`() {
        val directory = directory()
        val names = resolver(directory)
        directory.onProfileReceived(keyB64, PeerProfile(null, "alice", 1uL))
        names.peerIdentified("p1", keyB64, "aa")
        assertEquals("alice", names.name("p1"))
        directory.attachUsernameResolver { UsernameResolution.Found(keyB64) }
        assertEquals("@alice", names.name("p1"))
    }

    // ---- a link call -----------------------------------------------------

    @Test
    fun `a sealed profile without a display name names the tile at once and by the verified username after`() {
        val directory = directory()
        val names = resolver(directory)
        val ui = CallUiState(CoroutineScope(Dispatchers.Unconfined))
        ui.setParticipants(listOf(remote("peer-p1", names.name("p1"))))
        assertEquals(listOf(""), ui.participants.value.map { it.displayName })

        names.peerIdentified("p1", keyB64, "aa")
        names.memberAnnounced(peerId = "p1", senderLeafKeyHex = "aa", displayName = null, username = "alice")
        directory.onProfileReceived(keyB64, PeerProfile(null, "alice", 1uL))
        names.reapply(ui) { "peer-$it" }
        assertEquals(listOf("alice"), ui.participants.value.map { it.displayName })

        // The store verifies the claim; the host re-names on its change flow.
        directory.attachUsernameResolver { UsernameResolution.Found(keyB64) }
        names.reapply(ui) { "peer-$it" }
        assertEquals(listOf("@alice"), ui.participants.value.map { it.displayName })
    }

    @Test
    fun `the viewer's own name for a contact applies to an identified member`() {
        val directory = directory()
        val names = resolver(directory)
        names.name("p1")
        directory.setRenames(mapOf(keyB64 to "Mum"))
        names.peerIdentified("p1", keyB64, "aa")
        names.memberAnnounced("p1", "aa", "Elena", null)
        assertEquals("Mum", names.name("p1"))
    }

    @Test
    fun `a profile relayed by another peer names nobody`() {
        val names = resolver(directory())
        names.name("p1")
        names.peerIdentified("p1", keyB64, "aa")
        // A copy of someone else's profile (sealed by leaf bb), relayed by p1.
        names.memberAnnounced("p1", "bb", "Boris", "boris")
        assertEquals("", names.name("p1"))
        // Nor does a profile from a peer that has not announced itself.
        names.name("p2")
        names.memberAnnounced("p2", "cc", "Carol", null)
        assertEquals("", names.name("p2"))
    }

    @Test
    fun `a peer that left and came back under the same id starts unnamed`() {
        val names = resolver(directory())
        names.name("p1")
        names.peerIdentified("p1", otherKeyB64, "bb")
        names.memberAnnounced("p1", "bb", "Anna", null)
        assertEquals("Anna", names.name("p1"))
        names.forget("p1")
        assertEquals("", names.name("p1"))
    }

    @Test
    fun `a later announcement without a name clears the earlier one`() {
        val names = resolver(directory())
        names.name("p1")
        names.peerIdentified("p1", otherKeyB64, "bb")
        names.memberAnnounced("p1", "bb", "Anna", "anna")
        names.memberAnnounced("p1", "bb", "  ", "anna")
        assertEquals("anna", names.name("p1"))
    }

    // ---- whose profile claim is kept -------------------------------------

    @Test
    fun `only the owner's claim of the owner's account is kept`() {
        val cert = ByteArray(153) { 0x5c }
        val claim = uniffi.lakko_ffi_uniffi.FfiProfile(
            displayName = "Anna", username = "anna", updatedAtMs = 5uL,
            accountId = ownerAccount, usernameCertificate = cert, grants = emptyList(),
        )
        val kept = boundProfileClaim(claim, fromOwner = true, ownerAccountId = ownerAccount)
        assertTrue(kept.accountId!!.contentEquals(ownerAccount))
        assertTrue(kept.usernameCertificate!!.contentEquals(cert))
        for (stripped in listOf(
            boundProfileClaim(claim, fromOwner = false, ownerAccountId = ownerAccount),
            boundProfileClaim(claim, fromOwner = true, ownerAccountId = ByteArray(16) { 0x0b }),
        )) {
            assertNull(stripped.accountId)
            assertNull(stripped.usernameCertificate)
            assertEquals("Anna", stripped.displayName)
            assertEquals("anna", stripped.username)
        }
    }

    // ---- early relays ----------------------------------------------------

    @Test
    fun `early relays are handed back oldest first, once`() {
        val relays = EarlyRelays<Int>()
        relays.keep(1)
        relays.keep(2)
        assertEquals(listOf(1, 2), relays.drain())
        assertEquals(emptyList<Int>(), relays.drain())
    }

    @Test
    fun `early relays keep the newest when more arrive than they hold`() {
        assertEquals(64, EARLY_RELAY_LIMIT)
        val relays = EarlyRelays<Int>()
        for (i in 0 until EARLY_RELAY_LIMIT + 5) relays.keep(i)
        val kept = relays.drain()
        assertEquals(EARLY_RELAY_LIMIT, kept.size)
        assertEquals(5, kept.first())
        assertEquals(EARLY_RELAY_LIMIT + 4, kept.last())
    }

    private fun remote(id: String, name: String) = CallUiState.Participant(
        id = id,
        displayName = name,
        isLocal = false,
        micOn = true,
        camOn = false,
        quality = CallUiState.QualityTier.Good,
        speaking = false,
    )
}
