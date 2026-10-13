// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Unit tests for [resolveTapGroupHex] — the ADR-0042 notification-TAP
 * reverse-map that turns the wake's opaque `push_gid` back into the real
 * DS `group_id` before it goes into the click `Intent` extras.
 *
 * The load-bearing property is the FALLBACK: every miss shape (unknown
 * route, blank stored value, a throwing store) must return the raw wake
 * hex unchanged so a legacy / unprovisioned wake behaves byte-for-byte
 * as it did before ADR-0042 — and must never throw, since this runs on
 * the FCM callback thread on the way to posting a notification.
 */
class TapDeepLinkRoutingTest {

    private companion object {
        /** The opaque `push_gid` a post-ADR-0042 wake carries as `g`. */
        val PUSH_GID_HEX = "cc".repeat(32)

        /** The real DS `group_id` the route index maps it back to. */
        val GROUP_HEX = "aa".repeat(32)
    }

    @Test
    fun `a provisioned push_gid resolves to the real group_id`() {
        val resolved = resolveTapGroupHex(PUSH_GID_HEX) { hex ->
            if (hex == PUSH_GID_HEX) GROUP_HEX else null
        }
        assertEquals(GROUP_HEX, resolved)
    }

    @Test
    fun `the lookup is keyed on the wake hex verbatim`() {
        var seen: String? = null
        resolveTapGroupHex(PUSH_GID_HEX) { hex ->
            seen = hex
            GROUP_HEX
        }
        assertEquals(PUSH_GID_HEX, seen)
    }

    @Test
    fun `a route miss keeps the raw wake hex`() {
        val resolved = resolveTapGroupHex(PUSH_GID_HEX) { null }
        assertEquals(PUSH_GID_HEX, resolved)
    }

    @Test
    fun `a legacy raw-group_id wake passes through unchanged`() {
        // Pre-ADR-0042 wakes carry the group_id itself; no route row
        // exists for it, so the miss path must hand it straight through.
        val resolved = resolveTapGroupHex(GROUP_HEX) { null }
        assertEquals(GROUP_HEX, resolved)
    }

    @Test
    fun `a blank stored group_id is treated as a miss`() {
        assertEquals(PUSH_GID_HEX, resolveTapGroupHex(PUSH_GID_HEX) { "" })
        assertEquals(PUSH_GID_HEX, resolveTapGroupHex(PUSH_GID_HEX) { "   " })
    }

    @Test
    fun `a resolved group_id is trimmed`() {
        val resolved = resolveTapGroupHex(PUSH_GID_HEX) { "  $GROUP_HEX  " }
        assertEquals(GROUP_HEX, resolved)
    }

    @Test
    fun `a throwing store degrades to the raw wake hex, never propagates`() {
        val resolved = resolveTapGroupHex(PUSH_GID_HEX) {
            throw IllegalStateException("shared prefs unavailable")
        }
        assertEquals(PUSH_GID_HEX, resolved)
    }

    @Test
    fun `an Error from the store is also contained`() {
        // The catch is on Throwable on purpose — a notification handler
        // must never let anything escape on the way to `notify`.
        val resolved = resolveTapGroupHex(PUSH_GID_HEX) { throw StackOverflowError() }
        assertEquals(PUSH_GID_HEX, resolved)
    }

    @Test
    fun `the result is never blank for a non-blank wake hex`() {
        for (lookup in listOf<(String) -> String?>({ null }, { "" }, { GROUP_HEX })) {
            assertTrue(resolveTapGroupHex(PUSH_GID_HEX, lookup).isNotEmpty())
        }
    }

    // ---- ADR-0073 phase 2 / UX 16.4: a closed one-to-one's wake ------------

    @Test
    fun `a wake for a closed one-to-one is recognised through the route index`() {
        val asked = mutableListOf<String>()
        val closed = wakeNamesClosedOneToOne(
            wakeGroupHex = PUSH_GID_HEX,
            lookupGroupIdHex = { hex -> if (hex == PUSH_GID_HEX) GROUP_HEX else null },
            isClosedOneToOne = { hex ->
                asked += hex
                hex == GROUP_HEX
            },
        )
        assertTrue(closed)
        assertEquals(listOf(GROUP_HEX), asked)
    }

    @Test
    fun `a legacy wake carrying the group id itself is recognised too`() {
        assertTrue(wakeNamesClosedOneToOne(GROUP_HEX, { null }, { it == GROUP_HEX }))
    }

    @Test
    fun `a wake for an open conversation, or with no group, is not suppressed`() {
        assertEquals(false, wakeNamesClosedOneToOne(PUSH_GID_HEX, { GROUP_HEX }, { false }))
        assertEquals(false, wakeNamesClosedOneToOne(null, { GROUP_HEX }, { true }))
        assertEquals(false, wakeNamesClosedOneToOne("  ", { GROUP_HEX }, { true }))
    }

    @Test
    fun `a store that will not open shows the banner rather than swallowing the wake`() {
        assertEquals(
            false,
            wakeNamesClosedOneToOne(PUSH_GID_HEX, { GROUP_HEX }, { throw IllegalStateException("store") }),
        )
        // A throwing route index falls back to the raw id, which is not closed.
        assertEquals(
            false,
            wakeNamesClosedOneToOne(PUSH_GID_HEX, { throw IllegalStateException("store") }, { it == GROUP_HEX }),
        )
    }
}
