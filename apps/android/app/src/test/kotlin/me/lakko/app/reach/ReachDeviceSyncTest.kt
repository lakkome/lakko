// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiIncomingEnvelope
import uniffi.lakko_ffi_uniffi.FfiIncomingEnvelopeKind
import uniffi.lakko_ffi_uniffi.FfiReachClass
import uniffi.lakko_ffi_uniffi.FfiReachState
import uniffi.lakko_ffi_uniffi.FfiReachStateEntry
import uniffi.lakko_ffi_uniffi.FfiReachStateMerge
import uniffi.lakko_ffi_uniffi.FfiVisibilityMode
import uniffi.lakko_ffi_uniffi.FfiVisibilitySetting

/**
 * ADR-0060 section 7 — the setting and the reach-key counters between the
 * account's devices. The host JVM has no native library, so the dispatcher
 * and the ordering are passed in: the ordering is the core's contract
 * (`visibilityIsNewer` / `reachStateIsNewer`: strictly greater stamp), written
 * here as that.
 */
class ReachDeviceSyncTest {

    /** The core's rule: strictly greater is newer. */
    private val strictlyNewer: (ULong, ULong) -> Boolean = { incoming, stored -> incoming > stored }

    private fun setting(stamp: ULong) = FfiVisibilitySetting(
        mode = FfiVisibilityMode.EVERYONE,
        contactsOfContacts = false,
        groups = null,
        chosen = emptyList(),
        lastContactsOfContacts = false,
        lastGroups = null,
        updatedAtMs = stamp,
    )

    private fun envelope(
        kind: FfiIncomingEnvelopeKind,
        visibility: FfiVisibilitySetting? = null,
        reachState: FfiReachState? = null,
    ) = FfiIncomingEnvelope(
        kind = kind,
        payload = ByteArray(0),
        cameraOn = null,
        mintEpoch = null,
        profile = null,
        groupName = null,
        pin = null,
        introductionRecord = null,
        introductions = null,
        visibility = visibility,
        reachState = reachState,
        committerDelegation = null,
    )

    @Test
    fun `a newer stamp is kept, an older or equal one refused`() {
        assertTrue(keepsIncoming(11uL, 10uL, strictlyNewer))
        assertFalse(keepsIncoming(10uL, 10uL, strictlyNewer))
        assertFalse(keepsIncoming(9uL, 10uL, strictlyNewer))
        // Nothing held yet compares as the default's stamp, 0.
        assertTrue(keepsIncoming(1uL, null, strictlyNewer))
        assertFalse(keepsIncoming(0uL, null, strictlyNewer))
    }

    @Test
    fun `a Visibility envelope is kept with the bytes it arrived as`() {
        val wire = byteArrayOf(1, 2, 3)
        val got = classifyDeviceSync(wire) { envelope(FfiIncomingEnvelopeKind.VISIBILITY, visibility = setting(42uL)) }
        got as ReceivedDeviceSync.Visibility
        assertEquals(42uL, got.setting.updatedAtMs)
        assertArrayEquals(wire, got.wire)
    }

    @Test
    fun `a ReachState envelope is kept with the bytes it arrived as`() {
        val wire = byteArrayOf(9, 8)
        val state = FfiReachState(entries = emptyList(), updatedAtMs = 7uL)
        val got = classifyDeviceSync(wire) { envelope(FfiIncomingEnvelopeKind.REACH_STATE, reachState = state) }
        got as ReceivedDeviceSync.ReachState
        assertEquals(7uL, got.state.updatedAtMs)
        assertArrayEquals(wire, got.wire)
    }

    @Test
    fun `any other envelope, or one that does not decode, is dropped`() {
        for (kind in FfiIncomingEnvelopeKind.entries) {
            if (kind == FfiIncomingEnvelopeKind.VISIBILITY || kind == FfiIncomingEnvelopeKind.REACH_STATE) continue
            assertSame(ReceivedDeviceSync.Foreign, classifyDeviceSync(byteArrayOf(1)) { envelope(kind) })
        }
        // The right kind without its decoded body.
        assertSame(
            ReceivedDeviceSync.Foreign,
            classifyDeviceSync(byteArrayOf(1)) { envelope(FfiIncomingEnvelopeKind.VISIBILITY) },
        )
        // A dispatcher that throws (not a lakko-contacts envelope at all).
        assertSame(
            ReceivedDeviceSync.Foreign,
            classifyDeviceSync(byteArrayOf(1)) { throw IllegalArgumentException("not an envelope") },
        )
    }

    @Test
    fun `a re-send carries the original stamp, verbatim`() {
        val visibility = byteArrayOf(0x6c, 1, 2, 3)
        val reachState = byteArrayOf(0x6c, 4, 5)
        val sent = deviceSyncEnvelopesToSend(reachHex(visibility), reachHex(reachState))
        assertEquals(2, sent.size)
        assertArrayEquals(visibility, sent[0])
        assertArrayEquals(reachState, sent[1])
        // Only what is held goes out.
        assertEquals(1, deviceSyncEnvelopesToSend(reachHex(visibility), null).size)
        assertTrue(deviceSyncEnvelopesToSend(null, null).isEmpty())
    }

    @Test
    fun `a local change stamps strictly after the one it replaces`() {
        assertEquals(1_000uL, nextSyncStamp(1_000, null))
        assertEquals(1_000uL, nextSyncStamp(1_000, 999uL))
        // The wall clock went backwards: still strictly after.
        assertEquals(1_001uL, nextSyncStamp(500, 1_000uL))
        assertEquals(1_001uL, nextSyncStamp(1_000, 1_000uL))
    }

    private val scopeA = ByteArray(16) { 0x22 }

    private fun state(stamp: ULong, contact: UInt, group: UInt? = null) = FfiReachState(
        entries = listOfNotNull(
            FfiReachStateEntry(FfiReachClass.CONTACT, ByteArray(0), contact),
            group?.let { FfiReachStateEntry(FfiReachClass.GROUP, scopeA, it) },
        ),
        updatedAtMs = stamp,
    )

    /** A stand-in for `composeReachStateEnvelope` whose bytes name the stamp,
     *  so a test can see which state a wire carries. */
    private val compose: (FfiReachState) -> ByteArray = { "wire:${it.updatedAtMs}".toByteArray() }

    @Test
    fun `the core's merge decides, and the answer is stored and sent as returned`() {
        val stored = state(10uL, contact = 1u)
        val incoming = state(20uL, contact = 1u, group = 2u)
        var askedWith: Triple<FfiReachState, FfiReachState, ULong>? = null
        val merged = state(21uL, contact = 1u, group = 2u)
        val adoption = adoptReachStateMerge(
            stored = stored,
            incoming = incoming,
            nowMs = 5,
            merge = { s, i, now -> askedWith = Triple(s, i, now); FfiReachStateMerge(merged, true, true) },
            compose = compose,
        )
        assertSame(stored, askedWith!!.first)
        assertSame(incoming, askedWith!!.second)
        assertEquals(5uL, askedWith!!.third)
        // Stored with the core's stamp (21), sent as the very same bytes: never
        // re-stamped by the client.
        assertEquals("wire:21", String(adoption.storeWire!!))
        assertArrayEquals(adoption.storeWire, adoption.sendWire)
        assertEquals(2, adoption.versions!![ReachSlot(ReachKind.GROUP, reachHex(scopeA))])
    }

    @Test
    fun `store without send persists and sends nothing`() {
        val incoming = state(20uL, contact = 3u)
        val adoption = adoptReachStateMerge(
            stored = state(10uL, contact = 1u),
            incoming = incoming,
            nowMs = 0,
            merge = { _, i, _ -> FfiReachStateMerge(i, true, false) },
            compose = compose,
        )
        // The incoming state adopted verbatim: its own stamp is kept.
        assertEquals("wire:20", String(adoption.storeWire!!))
        assertNull(adoption.sendWire)
        assertEquals(3, adoption.versions!![ReachSlot(ReachKind.CONTACT, "")])
    }

    @Test
    fun `send without store sends the state as returned and persists nothing`() {
        val adoption = adoptReachStateMerge(
            stored = state(30uL, contact = 4u),
            incoming = state(20uL, contact = 1u),
            nowMs = 0,
            merge = { s, _, _ -> FfiReachStateMerge(s, false, true) },
            compose = compose,
        )
        assertNull(adoption.storeWire)
        assertNull(adoption.versions)
        assertEquals("wire:30", String(adoption.sendWire!!))
    }

    @Test
    fun `nothing to do, or a merge the core refuses, changes nothing`() {
        val quiet = adoptReachStateMerge(
            stored = state(30uL, contact = 4u),
            incoming = state(20uL, contact = 1u),
            nowMs = 0,
            merge = { s, _, _ -> FfiReachStateMerge(s, false, false) },
            compose = compose,
        )
        assertNull(quiet.storeWire)
        assertNull(quiet.sendWire)
        assertNull(quiet.versions)
        val refused = adoptReachStateMerge(
            stored = state(30uL, contact = 4u),
            incoming = state(20uL, contact = 1u),
            nowMs = 0,
            merge = { _, _, _ -> throw IllegalStateException("too many entries") },
            compose = compose,
        )
        assertNull(refused.storeWire)
        assertNull(refused.sendWire)
    }

    @Test
    fun `counters convert to and from ReachState entries`() {
        val scope = ByteArray(16) { 0x11 }
        val state = FfiReachState(
            entries = listOf(
                FfiReachStateEntry(FfiReachClass.CONTACT, ByteArray(0), 2u),
                FfiReachStateEntry(FfiReachClass.GROUP, scope, 5u),
            ),
            updatedAtMs = 1uL,
        )
        val versions = reachStateVersions(state)
        assertEquals(2, versions[ReachSlot(ReachKind.CONTACT, "")])
        assertEquals(5, versions[ReachSlot(ReachKind.GROUP, reachHex(scope))])
        val entries = reachStateEntries(versions)
        assertEquals(2, entries.size)
        assertEquals(FfiReachClass.CONTACT, entries[0].`class`)
        assertEquals(0, entries[0].scopeId.size)
        assertEquals(FfiReachClass.GROUP, entries[1].`class`)
        assertArrayEquals(scope, entries[1].scopeId)
        assertEquals(5u, entries[1].version)
    }
}
