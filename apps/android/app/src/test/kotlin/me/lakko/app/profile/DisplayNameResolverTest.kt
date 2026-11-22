// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import me.lakko.app.l10n.EnglishForTests
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0048 §5 / UX §3 — the one resolver's precedence table:
 * local rename -> profile name -> VERIFIED @username -> a neutral placeholder.
 * The fingerprint is never a name; it stays on a separate accessor.
 */
class DisplayNameResolverTest {
    init {
        // ADR-0061: the words under test are the English catalog's.
        EnglishForTests.install()
    }


    private val key = byteArrayOf(0xa1.toByte(), 0xb2.toByte(), 0xc3.toByte(), 0xd4.toByte(), 0x55, 0x66)

    private fun profile(name: String?, username: String?) =
        PeerProfile(displayName = name, username = username, updatedAtMs = 1uL)

    private fun resolve(
        rename: String? = null,
        profile: PeerProfile? = null,
        verification: UsernameVerification? = null,
    ): ResolvedName = resolveName(rename, profile, { verification }, key)

    /** Seen live 2026-11-22: a member row read `G53Person · @w6g53x2201` on
     *  one line. A row is the name, and the verified username once under it. */
    @Test
    fun `a member row is the name with the verified username once under it`() {
        assertEquals(
            PersonRowText("G53Person", "@w6g53x2201"),
            personRowText(resolve(profile = profile("G53Person", "w6g53x2201"), verification = UsernameVerification.VERIFIED)),
        )
        assertEquals(
            PersonRowText("@ann", null),
            personRowText(resolve(profile = profile(null, "ann"), verification = UsernameVerification.VERIFIED)),
        )
        assertEquals(
            PersonRowText("Bob", null),
            personRowText(resolve(profile = profile("Bob", "bob"), verification = null)),
        )
    }

    @Test
    fun `the precedence table`() {
        // 1. rename beats everything.
        assertEquals(
            "Mum",
            resolve("Mum", profile("Alice Petrova", "alice"), UsernameVerification.VERIFIED).primary,
        )
        // 2. profile name beats the verified username.
        assertEquals(
            "Alice Petrova",
            resolve(profile = profile("Alice Petrova", "alice"), verification = UsernameVerification.VERIFIED)
                .primary,
        )
        // 3. verified username when there is no name.
        assertEquals(
            "@alice",
            resolve(profile = profile(null, "alice"), verification = UsernameVerification.VERIFIED).primary,
        )
        // 4. the neutral placeholder; the fingerprint only on its own accessor.
        val bare = resolve()
        assertEquals("Member", bare.primary)
        assertTrue(bare.isPlaceholder)
        assertFalse(bare.primary.contains("@"))
        assertEquals("a1b2 c3d4", bare.fingerprint)
        assertFalse(bare.listLabel.contains("a1b2"))
    }

    @Test
    fun `the fingerprint accessor is stable and never carries an at sign`() {
        val named = resolve(profile = profile("Alice Petrova", "alice"), verification = UsernameVerification.VERIFIED)
        assertEquals("a1b2 c3d4", named.fingerprint)
        assertFalse(named.isPlaceholder)
        assertEquals("a1b2 c3d4", fingerprintLabel(key))
        assertEquals("", fingerprintLabel(ByteArray(0)))
    }

    @Test
    fun `an unverified or failed username is never shown`() {
        for (v in listOf(null, UsernameVerification.FAILED)) {
            val r = resolve(profile = profile(null, "pavel"), verification = v)
            assertEquals("Member", r.primary)
            assertTrue(r.isPlaceholder)
            assertNull(r.verifiedUsername)
            assertEquals("Member", r.listLabel)
            assertFalse(r.listLabel.contains("pavel"))
        }
    }

    @Test
    fun `a failed claim is flagged for the contact detail, a pending one is not`() {
        assertTrue(resolve(profile = profile("P", "pavel"), verification = UsernameVerification.FAILED).usernameNotVerified)
        assertFalse(resolve(profile = profile("P", "pavel"), verification = null).usernameNotVerified)
        assertFalse(resolve(profile = profile("P", null)).usernameNotVerified)
    }

    @Test
    fun `the list label adds the verified username under a name`() {
        val r = resolve(profile = profile("Alice Petrova", "alice"), verification = UsernameVerification.VERIFIED)
        assertEquals("Alice Petrova · @alice", r.listLabel)
        val onlyUser = resolve(profile = profile(null, "alice"), verification = UsernameVerification.VERIFIED)
        assertEquals("@alice", onlyUser.listLabel)
        assertEquals("Alice Petrova · @alice wants to connect", requestCardTitle(r))
        assertEquals("@alice wants to connect", requestCardTitle(onlyUser))
        // A request from someone nothing is known about says so, never a key.
        assertEquals("Someone wants to connect", requestCardTitle(resolve()))
        assertEquals("Someone", requestSenderLabel(resolve()))
        assertEquals("Alice Petrova · @alice", requestSenderLabel(r))
    }

    @Test
    fun `a rename that overrides a profile name exposes their name`() {
        assertEquals("Alice Petrova", resolve("Mum", profile("Alice Petrova", null)).theirName)
        assertNull(resolve("Alice Petrova", profile("Alice Petrova", null)).theirName)
        assertNull(resolve(profile = profile("Alice Petrova", null)).theirName)
    }

    @Test
    fun `blank rename and blank profile name fall through`() {
        assertEquals(
            "@alice",
            resolve("  ", profile("  ", "alice"), UsernameVerification.VERIFIED).primary,
        )
    }
}
