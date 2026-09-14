// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0048 §5 / UX §3 — the one resolver's precedence table:
 * local rename -> profile name -> VERIFIED @username -> fingerprint without `@`.
 */
class DisplayNameResolverTest {

    private val key = byteArrayOf(0xa1.toByte(), 0xb2.toByte(), 0xc3.toByte(), 0xd4.toByte(), 0x55, 0x66)

    private fun profile(name: String?, username: String?) =
        PeerProfile(displayName = name, username = username, updatedAtMs = 1uL)

    private fun resolve(
        rename: String? = null,
        profile: PeerProfile? = null,
        verification: UsernameVerification? = null,
    ): ResolvedName = resolveName(rename, profile, { verification }, key)

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
        // 4. fingerprint, without `@`.
        val bare = resolve()
        assertEquals("a1b2 c3d4", bare.primary)
        assertTrue(bare.isFingerprint)
        assertFalse(bare.primary.contains("@"))
    }

    @Test
    fun `an unverified or failed username is never shown`() {
        for (v in listOf(null, UsernameVerification.FAILED)) {
            val r = resolve(profile = profile(null, "pavel"), verification = v)
            assertEquals("a1b2 c3d4", r.primary)
            assertNull(r.verifiedUsername)
            assertEquals("a1b2 c3d4", r.listLabel)
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
        assertEquals("a1b2 c3d4 wants to connect", requestCardTitle(resolve()))
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
