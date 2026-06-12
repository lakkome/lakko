// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.transparency

import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test
import org.junit.runner.RunWith

/**
 * KT-log S3e — instrumented coverage of the native-lib-backed
 * [NativeAccountKeyDeriver] (the real `transparencyAccountKey` FFI call).
 *
 * `transparencyAccountKey` derives
 * `BLAKE3("lakko/transparency/account/v1" || account_id)`, which needs
 * `liblakko_ffi_uniffi.so` — so it runs inside the emulator / device
 * process, not the host JVM. The host suite [TransparencyVerifyTest]
 * stubs this seam (alongside the inclusion gate) and covers the full
 * verdict state machine; this proves the production seam wraps the real
 * derivation correctly.
 *
 * The OTHER native dependency, `verifyLogInclusion`, is exercised inside
 * Rust (`lakko-transparency`'s own tests) — building a VALID Merkle proof
 * needs the Rust log builder, which is not exposed to the FFI, so a
 * client-only test cannot construct a `CONFIRMED` proof. The host suite
 * therefore covers the proof-fail `MISMATCH` and `CONFIRMED` branches via
 * the [InclusionGate] seam (throw / return = the native contract).
 */
@RunWith(AndroidJUnit4::class)
class TransparencyAccountKeyInstrumentedTest {

    private val accountId = ByteArray(16) { it.toByte() }

    @Test
    fun nativeDeriverReturnsThirtyTwoBytes() {
        val key = NativeAccountKeyDeriver.derive(accountId)
        assertEquals(32, key.size)
    }

    @Test
    fun nativeDeriverIsDeterministic() {
        val a = NativeAccountKeyDeriver.derive(accountId)
        val b = NativeAccountKeyDeriver.derive(accountId)
        // Same account_id → same account-key (so the client query +
        // verify can't drift from the server's append-side derivation).
        assertEquals(a.toList(), b.toList())
    }

    @Test
    fun differentAccountIdsDeriveDifferentKeys() {
        val a = NativeAccountKeyDeriver.derive(accountId)
        val other = ByteArray(16) { (it + 1).toByte() }
        val b = NativeAccountKeyDeriver.derive(other)
        assertFalse(a.contentEquals(b))
    }
}
