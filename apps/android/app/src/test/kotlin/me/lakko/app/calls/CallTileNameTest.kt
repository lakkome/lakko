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
 * The tag derivation is injected (a stand-in for `sfuPeerTag`), so no UniFFI
 * `.so` is needed.
 */
class CallTileNameTest {
    init {
        // ADR-0061: the words under test are the English catalog's.
        EnglishForTests.install()
    }

    private val key: ByteArray = ByteArray(32) { 0xa1.toByte() }
    private val keyB64: String = Base64.getEncoder().encodeToString(key)

    /** The start of an account id, as the SFU reports it. Never a name. */
    private val pseudonym = "01a0a045"

    private val tagOf: (String) -> String = { accountIdHex -> "tag:$accountIdHex" }

    private fun directory(): ProfileDirectory = ProfileDirectory(
        persistence = InMemoryProfilePersistence(),
        scope = CoroutineScope(Dispatchers.Unconfined),
        isNewer = { incoming, stored -> incoming > stored },
        passesRule = { it.matches(Regex("[a-z0-9_]{3,32}")) },
    )

    /** The production wiring of `callPeerNameResolver`, over [directory]. */
    private fun resolver(directory: ProfileDirectory): GroupCallNameResolver = GroupCallNameResolver(
        sourcesVersion = { directory.version.value },
        sources = {
            directory.accountIdsByKey().map { (keyB64, accountIdHex) -> PeerTagSource(accountIdHex, keyB64) }
        },
        tagOf = tagOf,
        labelOf = { keyB64 -> callNameOfPerson(directory, keyB64) },
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
    fun `a peer nobody can name has an empty name, with no number and no pseudonym`() {
        val names = resolver(directory())
        assertEquals("", names.resolve("p1", pseudonym, "tag:aa"))
        assertEquals("", names.resolve("p2", "0fe1d2c3", ""))
        // Asked again (the welcome snapshot, then peer-joined): still nothing.
        assertEquals("", names.resolve("p1", pseudonym, "tag:aa"))
    }

    @Test
    fun `a group member with a stored username and no display name is named by it`() {
        val directory = directory()
        val names = resolver(directory)
        directory.onProfileReceived(keyB64, PeerProfile(null, "alice", 1uL, accountIdHex = "aa"))
        assertEquals("alice", names.resolve("p1", pseudonym, "tag:aa"))
        directory.attachUsernameResolver { UsernameResolution.Found(keyB64) }
        assertEquals("@alice", names.resolve("p1", pseudonym, "tag:aa"))
    }

    // ---- a link call -----------------------------------------------------

    @Test
    fun `a sealed profile without a display name names the tile at once and by the verified username after`() {
        val directory = directory()
        val names = resolver(directory)
        val ui = CallUiState(CoroutineScope(Dispatchers.Unconfined))
        ui.setParticipants(listOf(remote("peer-p1", names.resolve("p1", pseudonym, "tag:aa"))))
        assertEquals(listOf(""), ui.participants.value.map { it.displayName })

        val profile = PeerProfile(null, "alice", 1uL, accountIdHex = "aa")
        val file = names.memberAnnounced(
            peerId = "p1",
            senderKeyB64 = keyB64,
            displayName = profile.displayName,
            username = profile.username,
            claimedAccountIdHex = profile.accountIdHex,
        )
        assertTrue(file)
        directory.onProfileReceived(keyB64, profile)
        names.reapply(ui) { "peer-$it" }
        assertEquals(listOf("alice"), ui.participants.value.map { it.displayName })

        // The store verifies the claim; the host re-names on its change flow.
        directory.attachUsernameResolver { UsernameResolution.Found(keyB64) }
        names.reapply(ui) { "peer-$it" }
        assertEquals(listOf("@alice"), ui.participants.value.map { it.displayName })
    }

    @Test
    fun `the viewer's own name for a contact applies to a filed member`() {
        val directory = directory()
        val names = resolver(directory)
        names.resolve("p1", pseudonym, "tag:aa")
        directory.setRenames(mapOf(keyB64 to "Mum"))
        assertTrue(names.memberAnnounced("p1", keyB64, "Elena", null, "aa"))
        directory.onProfileReceived(keyB64, PeerProfile("Elena", null, 1uL, accountIdHex = "aa"))
        assertEquals("Mum", names.resolve("p1", pseudonym, "tag:aa"))
    }

    @Test
    fun `a profile claiming another account is not filed and binds nobody`() {
        val directory = directory()
        val names = resolver(directory)
        // The SFU admitted p1 as account bb; its profile claims account aa.
        names.resolve("p1", pseudonym, "tag:bb")
        val file = names.memberAnnounced(
            peerId = "p1",
            senderKeyB64 = keyB64,
            displayName = "Anna",
            username = "anna",
            claimedAccountIdHex = "aa",
        )
        assertFalse(file)
        // The tile still shows what was announced, as plain text.
        assertEquals("Anna", names.resolve("p1", pseudonym, "tag:bb"))

        // Even were that key known to the store under a rename, the peer is
        // not bound to it: the rename does not reach the tile.
        directory.setRenames(mapOf(keyB64 to "Mum"))
        assertEquals("Anna", names.resolve("p1", pseudonym, "tag:bb"))
    }

    @Test
    fun `a profile with no claim, no tag or no resolved sender is not filed`() {
        val names = resolver(directory())
        names.resolve("p1", pseudonym, "tag:aa")
        names.resolve("p2", pseudonym, "")
        assertFalse(names.memberAnnounced("p1", keyB64, null, "anna", null))
        assertFalse(names.memberAnnounced("p2", keyB64, null, "anna", "aa"))
        assertFalse(names.memberAnnounced("p1", null, null, "anna", "aa"))
        // A peer the roster never reported has no tag to match.
        assertFalse(names.memberAnnounced("p9", keyB64, null, "anna", "aa"))
        // Rule 3 still names the tile, without the at sign.
        assertEquals("anna", names.resolve("p1", pseudonym, "tag:aa"))
    }

    @Test
    fun `a person this device already knows is named before they announce`() {
        val directory = directory()
        directory.onProfileReceived(keyB64, PeerProfile("Anna", null, 1uL, accountIdHex = "aa"))
        val names = resolver(directory)
        assertEquals("Anna", names.resolve("p1", pseudonym, "tag:aa"))
    }

    @Test
    fun `a peer that left and came back under the same id starts unnamed`() {
        val names = resolver(directory())
        names.resolve("p1", pseudonym, "tag:bb")
        names.memberAnnounced("p1", keyB64, "Anna", null, null)
        assertEquals("Anna", names.resolve("p1", pseudonym, "tag:bb"))
        names.forget("p1")
        assertEquals("", names.resolve("p1", pseudonym, "tag:bb"))
    }

    @Test
    fun `a later announcement without a name clears the earlier one`() {
        val names = resolver(directory())
        names.resolve("p1", pseudonym, "tag:bb")
        names.memberAnnounced("p1", keyB64, "Anna", "anna", null)
        names.memberAnnounced("p1", keyB64, "  ", "anna", null)
        assertEquals("anna", names.resolve("p1", pseudonym, "tag:bb"))
    }

    // ---- who may be filed ------------------------------------------------

    @Test
    fun `the claimed account must be the one the SFU admitted the peer as`() {
        assertTrue(profileClaimMatchesPeer("aa", "tag:aa", tagOf))
        assertFalse(profileClaimMatchesPeer("bb", "tag:aa", tagOf))
        assertFalse(profileClaimMatchesPeer(null, "tag:aa", tagOf))
        assertFalse(profileClaimMatchesPeer("aa", null, tagOf))
        assertFalse(profileClaimMatchesPeer("aa", "", tagOf))
    }

    @Test
    fun `a tag that cannot be derived refuses the claim`() {
        assertFalse(profileClaimMatchesPeer("zz", "tag:zz") { error("not an account id") })
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
