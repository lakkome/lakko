// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.lakko_ffi_uniffi.CallKey
import uniffi.lakko_ffi_uniffi.unwrapSframeFrame

/**
 * G-26 cross-stack SFrame interop — the Android leg.
 *
 * Pins the SAME golden vector the Rust core
 * (`crates/lakko-calls/src/sframe.rs::golden_vector_*`), the wasm/web
 * (`apps/web/src/calls/sframe-golden.test.ts`), and the iOS FFI
 * (`apps/ios/LakkoTests/SFrameGoldenTests.swift`) pin: the frame
 * `(seed=[0x42;32], leaf=3, stream_index=0 => KID=0x0300,
 * plaintext="lakko sframe golden vector v1")` sealed at counter 1. The UniFFI
 * `openFramed` MUST recover the plaintext from these EXACT bytes. Since
 * ADR-0069 a `ParticipantKey` starts at a random counter below 2^62 and
 * nothing on the FFI sets a counter, so `sealFramed` no longer reproduces the
 * golden bytes; the seal is proved by opening what it sealed and by reading
 * the header (the KID, and the 64-bit counter). Because all four stacks open
 * the identical wire, a frame sealed on any one opens on any other (the
 * cross-runtime half of G-26), no live SFU needed. Any drift in the Android
 * SFrame stack fails here.
 *
 * Instrumented (runs on a device/emulator) because the UniFFI core is backed by
 * the native `liblakko_ffi_uniffi` library.
 */
@RunWith(AndroidJUnit4::class)
class SframeGoldenTest {

    private val goldenSeed = ByteArray(32) { 0x42 }
    private val goldenLeaf = 3u
    private val goldenStreamIndex: UByte = 0u
    private val goldenPlaintext = "lakko sframe golden vector v1"
    private val goldenWireHex =
        "910300fe2c2af65799594a4c693ff4c9e0d49b0565756ddd7299b0945fc8adb1c9c12fced57c6937f372e6b94bdae40a"

    /**
     * A fresh key seals a frame that carries the composite KID and a 64-bit
     * counter one above the key's random start, and the frame opens under an
     * independently derived key (ADR-0069). The name is kept from when a fresh
     * key sealed the golden bytes themselves.
     */
    @Test
    fun sealsToTheCanonicalCrossStackWire() {
        val callKey = CallKey.fromSeed(goldenSeed)
        val sender = callKey.participantKey(goldenLeaf, goldenStreamIndex)
        val start: ULong = sender.counter()
        assertTrue(start < (1uL shl 62))

        val wire = sender.sealFramed(goldenPlaintext.toByteArray(Charsets.UTF_8))
        val header = unwrapSframeFrame(wire)
        // The whole composite KID: (leaf 3 << 8) | stream 0.
        assertEquals(0x0300u, header.senderLeaf)
        assertEquals(start + 1uL, header.counter)
        assertEquals(start + 1uL, sender.counter())

        // An independently derived key (its own random counter) opens it.
        val receiver = callKey.participantKey(goldenLeaf, goldenStreamIndex)
        assertNotEquals(start, receiver.counter())
        assertEquals(goldenPlaintext, String(receiver.openFramed(wire), Charsets.UTF_8))
    }

    @Test
    fun opensTheCanonicalWireToThePlaintext() {
        val pk = CallKey.fromSeed(goldenSeed).participantKey(goldenLeaf, goldenStreamIndex)
        val wire = goldenWireHex.hexToBytes()
        val opened = pk.openFramed(wire)
        assertEquals(goldenPlaintext, String(opened, Charsets.UTF_8))

        val header = unwrapSframeFrame(wire)
        assertEquals(0x0300u, header.senderLeaf)
        assertEquals(1uL, header.counter)
    }

    private fun String.hexToBytes(): ByteArray =
        ByteArray(length / 2) { substring(it * 2, it * 2 + 2).toInt(16).toByte() }
}
