// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Replay guard (2026-11-14) — a device's KeyPackage is last-resort, so the
 * Delivery Service can replay an OLD owner-signed Welcome for a channel's
 * group; it must never replace a newer held group. The replace runs against
 * a fake provider here, in the order [CoPublisher.joinReplacingHeld] runs it
 * (the host JVM loads no native core).
 */
class WelcomeReplayGuardTest {

    /** The guarded replace as `joinReplacingHeld` composes it; `null` when
     *  the Welcome was ignored or not joined. */
    private fun replace(
        provider: MutableMap<String, ULong>,
        heldActive: Boolean,
        suspected: Boolean,
        welcomeEpoch: ULong,
    ): ULong? {
        if (!heldGroupReplaceable(heldActive, suspected)) return null
        val heldEpoch = provider.getValue("group")
        return runCatching {
            replaceHeldThenJoin(
                snapshot = { provider.toMap() },
                delete = { provider.remove("group") },
                join = {
                    provider["group"] = welcomeEpoch
                    if (!joinedGroupReplacesHeld(welcomeEpoch, heldEpoch, heldActive)) throw StaleWelcomeRefused()
                    welcomeEpoch
                },
                restore = { before -> provider.clear(); provider.putAll(before) },
            )
        }.getOrNull()
    }

    @Test
    fun `an active unsuspected held group is not replaced`() {
        assertFalse(heldGroupReplaceable(heldActive = true, resetSuspected = false))
        val provider = mutableMapOf("group" to 9uL)
        assertEquals(null, replace(provider, heldActive = true, suspected = false, welcomeEpoch = 12uL))
        assertEquals(9uL, provider["group"])
    }

    @Test
    fun `a Welcome joining at an epoch not above the held one restores the old group`() {
        for (old in listOf(4uL, 9uL)) {
            val provider = mutableMapOf("group" to 9uL)
            assertEquals(null, replace(provider, heldActive = false, suspected = false, welcomeEpoch = old))
            assertEquals("the held group came back", 9uL, provider["group"])
        }
    }

    @Test
    fun `an inactive held group is replaced by a newer Welcome`() {
        assertTrue(heldGroupReplaceable(heldActive = false, resetSuspected = false))
        val provider = mutableMapOf("group" to 9uL)
        assertEquals(10uL, replace(provider, heldActive = false, suspected = false, welcomeEpoch = 10uL))
        assertEquals(10uL, provider["group"])
        // The owner device waiting for a Welcome is both inactive and suspended.
        val waiting = mutableMapOf("group" to 9uL)
        assertEquals(11uL, replace(waiting, heldActive = false, suspected = true, welcomeEpoch = 11uL))
    }

    @Test
    fun `under reset suspicion an active held group is replaced by a new chain at a lower epoch`() {
        // Decision A founded the channel again elsewhere: a new chain starts
        // low, so its epochs are not compared with the held group's.
        assertTrue(heldGroupReplaceable(heldActive = true, resetSuspected = true))
        val provider = mutableMapOf("group" to 9uL)
        assertEquals(2uL, replace(provider, heldActive = true, suspected = true, welcomeEpoch = 2uL))
        assertEquals(2uL, provider["group"])
    }
}
