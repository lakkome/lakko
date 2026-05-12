// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import uniffi.lakko_ffi_uniffi.Identity

/**
 * Single-activity Compose entry point for the 1.15c
 * "Hello core" smoke build.
 *
 * Mirror of `apps/web`'s 1.16a `DevProbe.tsx`:
 *
 *   1. Construct `Identity("lakko-android-skeleton")` via
 *      the UniFFI-generated Kotlin bindings.
 *   2. Read `signaturePublicKey()` — 32-byte Ed25519
 *      verifying key.
 *   3. Render it as a hex string. If we see hex, the JNA
 *      cdylib bridge works on this device and every later
 *      phase of 1.15 (OPAQUE auth, contacts, chat) sits on a
 *      proven foundation.
 *
 * The Identity instance is intentionally **leaked** — for a
 * smoke screen on a single frame the auto-cleaner that
 * `lakko_ffi_uniffi.kt` registers on construction takes care
 * of it eventually. Production paths will hold the Identity
 * inside a session manager (1.15d) and explicitly `.destroy()`
 * on logout.
 */
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            MaterialTheme {
                Surface(
                    modifier = Modifier.fillMaxSize(),
                    color = MaterialTheme.colorScheme.background,
                ) {
                    HelloCoreScreen()
                }
            }
        }
    }
}

@Composable
private fun HelloCoreScreen() {
    // `state` carries one of:
    //   * "Loading core…"  before the FFI call lands
    //   * "<hex>"            on success
    //   * "Error: <msg>"    if the Rust core throws (shouldn't
    //                       on a fresh Identity construction,
    //                       but surface anyway — we'd rather
    //                       see the error than show a blank
    //                       screen).
    var state by remember { mutableStateOf<String>("Loading core…") }

    LaunchedEffect(Unit) {
        state =
            try {
                // UniFFI's generated constructor invokes
                // `Identity::new("lakko-android-skeleton")` on
                // the Rust side, returning a Pointer-backed
                // wrapper.
                val identity = Identity("lakko-android-skeleton")
                val pubkey = identity.signaturePublicKey()
                // Render hex — short on screen because a 64-
                // char string wraps awkwardly. Full key in
                // 1.15d's auth surface when there's a proper
                // identity-detail view.
                pubkey.joinToString(separator = "") { "%02x".format(it) }
            } catch (e: Throwable) {
                "Error: ${e.message ?: e::class.simpleName}"
            }
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(24.dp),
        verticalArrangement = Arrangement.Center,
    ) {
        Text(
            text = "Lakko",
            style = MaterialTheme.typography.displaySmall,
        )
        Text(
            text = "Identity public key",
            style = MaterialTheme.typography.titleMedium,
            modifier = Modifier.padding(top = 16.dp),
        )
        Text(
            text = state,
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.padding(top = 8.dp),
        )
        Text(
            text = "Phase 1.15c smoke — JNA + UniFFI bridge",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(top = 24.dp),
        )
    }
}
