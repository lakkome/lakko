// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.lakko_ffi_uniffi.CallKey

/**
 * G-26 cross-stack SFrame interop — the Android leg.
 *
 * Pins the SAME golden vector the Rust core
 * (`crates/lakko-calls/src/sframe.rs::golden_vector_*`), the wasm/web
 * (`apps/web/src/calls/sframe-golden.test.ts`), and the iOS FFI
 * (`apps/ios/LakkoTests/SFrameGoldenTests.swift`) pin: sealing
 * `(seed=[0x42;32], leaf=3, plaintext="lakko sframe golden vector v1")` on a
 * fresh `ParticipantKey` (counter 0) MUST produce these EXACT bytes via the
 * UniFFI `sealFramed`, and `openFramed` MUST recover the plaintext. Because all
 * four stacks pin the identical wire, a frame sealed on any one opens on any
 * other (the cross-runtime half of G-26), no live SFU needed. Any drift in the
 * Android SFrame stack fails here.
 *
 * Instrumented (runs on a device/emulator) because the UniFFI core is backed by
 * the native `liblakko_ffi_uniffi` library.
 */
@RunWith(AndroidJUnit4::class)
class SframeGoldenTest {

    private val goldenSeed = ByteArray(32) { 0x42 }
    private val goldenLeaf = 3u
    private val goldenPlaintext = "lakko sframe golden vector v1"
    private val goldenWireHex =
        "31a6d1fee0dd6f091a0071e56d66a889f734a5b2fa6ef8f079d2e6b3e9b256e41ff55ca0102c28b8b697c526be59"

    @Test
    fun sealsToTheCanonicalCrossStackWire() {
        val pk = CallKey.fromSeed(goldenSeed).participantKey(goldenLeaf)
        val wire = pk.sealFramed(goldenPlaintext.toByteArray(Charsets.UTF_8))
        assertEquals(goldenWireHex, wire.toHex())
    }

    @Test
    fun opensTheCanonicalWireToThePlaintext() {
        val pk = CallKey.fromSeed(goldenSeed).participantKey(goldenLeaf)
        val opened = pk.openFramed(goldenWireHex.hexToBytes())
        assertEquals(goldenPlaintext, String(opened, Charsets.UTF_8))
    }

    private fun ByteArray.toHex(): String =
        joinToString("") { "%02x".format(it) }

    private fun String.hexToBytes(): ByteArray =
        ByteArray(length / 2) { substring(it * 2, it * 2 + 2).toInt(16).toByte() }
}
