// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host-side unit tests for the per-channel publish counter in
 * [ChannelsUiState] — the AEAD nonce-reuse defence.
 *
 * Run as `:app:testDebugUnitTest` — no emulator, no UniFFI native
 * library load, no `org.json`. Constructed with `publisherPrefs = null`,
 * so the persistence paths (`loadPublisherCounters` / `persistPublisher-
 * Counters`) early-return before touching `EncryptedSharedPreferences`
 * or `org.json`; only the in-memory counter LOGIC is exercised here.
 *
 * The at-rest round-trip (does a restarted process actually read the
 * persisted floor?) is NOT unit-testable in the plain JVM harness —
 * it needs a real Keystore-backed store, exactly like the publisher
 * signing-seed store. That path is covered by construction (the
 * production host passes `ChannelPublisherKeyStore.encryptedPrefs`).
 *
 * The regression these tests pin: [ChannelsUiState.setKeys] used to
 * reset the counter to 0 unconditionally. On a relaunch the counter is
 * restored from the store, then `setKeys` runs on the publisher-open
 * path — a reset there would restart at 0 within the SAME epoch + the
 * SAME (stable, MLS-re-derived) content seed, reusing a `(key, nonce)`
 * pair on a new plaintext (catastrophic ChaCha20 keystream reuse).
 * `setKeys` is now load-else-0.
 */
class ChannelPublishCounterTest {

    private fun keys(): ChannelPublisherKeys =
        ChannelPublisherKeys(
            seed = ByteArray(32) { it.toByte() },
            signingKeySeed = ByteArray(32) { (it + 1).toByte() },
        )

    @Test
    fun `nextCounter is monotone from zero with no repeats`() {
        val state = ChannelsUiState()
        val chan = "aa".repeat(16)
        val seen = mutableListOf<ULong>()
        repeat(1_000) { seen.add(state.nextCounter(chan)) }
        // First value is 0, each strictly one greater than the last,
        // and no value is ever handed out twice.
        assertEquals(0uL, seen.first())
        assertEquals(999uL, seen.last())
        for (i in 1 until seen.size) {
            assertEquals(seen[i - 1] + 1u, seen[i])
        }
        assertEquals(seen.size, seen.toSet().size)
    }

    @Test
    fun `setKeys does NOT reset an existing counter`() {
        val state = ChannelsUiState()
        val chan = "bb".repeat(16)
        // Advance the counter to simulate a run that has already published.
        assertEquals(0uL, state.nextCounter(chan))
        assertEquals(1uL, state.nextCounter(chan))
        assertEquals(2uL, state.nextCounter(chan))
        // Re-installing keys (the publisher-open path, incl. after a
        // relaunch that restored the counter) must PRESERVE the floor.
        state.setKeys(chan, keys())
        // The next post still carries 3, not 0 — no nonce reuse.
        assertEquals(3uL, state.nextCounter(chan))
    }

    @Test
    fun `setKeys seeds zero for a channel with no counter yet`() {
        val state = ChannelsUiState()
        val chan = "cc".repeat(16)
        state.setKeys(chan, keys())
        assertEquals(0uL, state.counterByChannel.value[chan])
        assertEquals(0uL, state.nextCounter(chan))
    }

    @Test
    fun `counters are independent per channel`() {
        val state = ChannelsUiState()
        val a = "11".repeat(16)
        val b = "22".repeat(16)
        assertEquals(0uL, state.nextCounter(a))
        assertEquals(1uL, state.nextCounter(a))
        // b starts fresh, untouched by a's advances.
        assertEquals(0uL, state.nextCounter(b))
        assertEquals(2uL, state.nextCounter(a))
        assertEquals(1uL, state.nextCounter(b))
    }

    @Test
    fun `removeChannel keeps the counter so a re-add cannot rewind the AEAD nonce`() {
        val state = ChannelsUiState()
        val chan = "dd".repeat(16)
        assertEquals(0uL, state.nextCounter(chan))
        assertEquals(1uL, state.nextCounter(chan))
        state.removeChannel(chan)
        // The counter is a forever-monotone floor: un-publishing does NOT
        // destroy the MLS PublisherGroup content seed, so a re-add of the same
        // channel_id re-derives the same content seed + epoch. The counter must
        // resume, never rewind to 0 (AEAD nonce-reuse defence).
        assertTrue(state.counterByChannel.value.containsKey(chan))
        assertEquals(2uL, state.nextCounter(chan))
    }

    @Test
    fun `reset clears all counters`() {
        val state = ChannelsUiState()
        val chan = "ee".repeat(16)
        state.nextCounter(chan)
        state.nextCounter(chan)
        state.reset()
        assertTrue(state.counterByChannel.value.isEmpty())
        assertNull(state.counterByChannel.value[chan])
    }
}
