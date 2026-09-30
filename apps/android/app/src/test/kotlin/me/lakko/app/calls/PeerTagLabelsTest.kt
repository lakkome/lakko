// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.l10n.EnglishForTests
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0021 / ADR-0048 v2 — the group-call `peer_tag -> label` map over both
 * sources (accepted contacts and received profiles) and the live resolver
 * around it. The tag derivation is injected (a stand-in for `sfuPeerTag`), so
 * no UniFFI `.so` is needed.
 */
class PeerTagLabelsTest {
    init {
        // ADR-0058: the words under test are the English catalog's.
        EnglishForTests.install()
    }


    /** A deterministic stand-in for `sfuPeerTag(callId, accountIdHex)`. */
    private val tagOf: (String) -> String = { accountIdHex -> "tag:$accountIdHex" }

    private val names = mapOf(
        "KEY_ALICE" to "Alice",
        "KEY_BOB" to "Bob",
        "KEY_MALLORY" to "Mallory",
    )
    private val labelOf: (String) -> String = { key -> names[key] ?: "Member" }

    private fun contact(accountIdHex: String, key: String) = PeerTagSource(accountIdHex, key)
    private fun profile(accountIdHex: String, key: String) = PeerTagSource(accountIdHex, key)

    @Test
    fun `contacts and profiles both label a tag`() {
        val labels = buildPeerTagLabels(
            sources = listOf(contact("aa", "KEY_ALICE"), profile("bb", "KEY_BOB")),
            tagOf = tagOf,
            labelOf = labelOf,
        )
        assertEquals("Alice", labels.labelFor("tag:aa"))
        assertEquals("Bob", labels.labelFor("tag:bb"))
        assertNull(labels.labelFor("tag:cc"))
        assertNull(labels.labelFor(""))
    }

    @Test
    fun `the same person from both sources is one label, not a collision`() {
        val labels = buildPeerTagLabels(
            sources = listOf(contact("aa", "KEY_ALICE"), profile("aa", "KEY_ALICE")),
            tagOf = tagOf,
            labelOf = labelOf,
        )
        assertEquals("Alice", labels.labelFor("tag:aa"))
        assertTrue(labels.collided.isEmpty())
    }

    @Test
    fun `two different keys claiming one account id hide the tag for both`() {
        // Mallory's profile claims Alice's account id; Alice's own contact row
        // claims it too. Neither name may land on Alice's tile.
        val labels = buildPeerTagLabels(
            sources = listOf(contact("aa", "KEY_ALICE"), profile("aa", "KEY_MALLORY")),
            tagOf = tagOf,
            labelOf = labelOf,
        )
        assertNull(labels.labelFor("tag:aa"))
        assertEquals(setOf("tag:aa"), labels.collided)
    }

    @Test
    fun `a profile arriving mid-call names a tile that was Participant N`() {
        var version = 0L
        var sources = listOf(contact("aa", "KEY_ALICE"))
        val resolver = GroupCallNameResolver(
            sourcesVersion = { version },
            sources = { sources },
            tagOf = tagOf,
            labelOf = labelOf,
        )
        assertEquals("Alice", resolver.resolve("p1", "pseudo-1", "tag:aa"))
        assertEquals("Participant 1", resolver.resolve("p2", "pseudo-2", "tag:bb"))
        // Asked again for the same peer (welcome, then peer-joined): same number.
        assertEquals("Participant 1", resolver.resolve("p2", "pseudo-2", "tag:bb"))

        // Bob's profile lands; the directory's version moves.
        sources = sources + profile("bb", "KEY_BOB")
        version += 1
        assertEquals("Bob", resolver.resolve("p2", "pseudo-2", "tag:bb"))
    }

    @Test
    fun `the map is not rebuilt while the version stands still`() {
        var builds = 0
        val resolver = GroupCallNameResolver(
            sourcesVersion = { 7L },
            sources = { builds += 1; listOf(contact("aa", "KEY_ALICE")) },
            tagOf = tagOf,
            labelOf = labelOf,
        )
        resolver.resolve("p1", "", "tag:aa")
        resolver.resolve("p2", "", "tag:zz")
        resolver.resolve("p1", "", "tag:aa")
        assertEquals(1, builds)
    }

    @Test
    fun `a later resolution never renumbers the other unresolved peers`() {
        var version = 0L
        var sources = emptyList<PeerTagSource>()
        val resolver = GroupCallNameResolver(
            sourcesVersion = { version },
            sources = { sources },
            tagOf = tagOf,
            labelOf = labelOf,
        )
        assertEquals("Participant 1", resolver.resolve("p1", "", "tag:aa"))
        assertEquals("Participant 2", resolver.resolve("p2", "", "tag:bb"))
        sources = listOf(profile("aa", "KEY_ALICE"))
        version += 1
        assertEquals("Alice", resolver.resolve("p1", "", "tag:aa"))
        // p2 keeps its number; nobody moves down into p1's slot.
        assertEquals("Participant 2", resolver.resolve("p2", "", "tag:bb"))
        assertEquals("Participant 3", resolver.resolve("p3", "", "tag:cc"))
    }

    @Test
    fun `reapply renames the tiles already on screen`() {
        var version = 0L
        var sources = emptyList<PeerTagSource>()
        val resolver = GroupCallNameResolver(
            sourcesVersion = { version },
            sources = { sources },
            tagOf = tagOf,
            labelOf = labelOf,
        )
        val ui = CallUiState(CoroutineScope(Dispatchers.Unconfined))
        ui.setParticipants(
            listOf(
                remote("peer-p1", resolver.resolve("p1", "", "tag:aa")),
                remote("peer-p2", resolver.resolve("p2", "", "tag:bb")),
            ),
        )
        assertEquals(listOf("Participant 1", "Participant 2"), ui.participants.value.map { it.displayName })

        sources = listOf(profile("bb", "KEY_BOB"))
        version += 1
        resolver.reapply(ui) { peerId -> "peer-$peerId" }
        assertEquals(listOf("Participant 1", "Bob"), ui.participants.value.map { it.displayName })
    }

    @Test
    fun `a source that fails to load keeps the previous map`() {
        var version = 0L
        var fail = false
        val warnings = mutableListOf<String>()
        val resolver = GroupCallNameResolver(
            sourcesVersion = { version },
            sources = { if (fail) error("store unreadable") else listOf(contact("aa", "KEY_ALICE")) },
            tagOf = tagOf,
            labelOf = labelOf,
            warn = { message, _ -> warnings.add(message) },
        )
        assertEquals("Alice", resolver.resolve("p1", "", "tag:aa"))
        fail = true
        version += 1
        assertEquals("Alice", resolver.resolve("p1", "", "tag:aa"))
        assertEquals(1, warnings.size)
    }

    private fun remote(id: String, name: String) = CallUiState.Participant(
        id = id,
        displayName = name,
        isLocal = false,
        micOn = true,
        camOn = false,
        handRaised = false,
        quality = CallUiState.QualityTier.Good,
        speaking = false,
        screenSharing = false,
    )
}
