// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.transport

import android.content.Context
import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.lakko_ffi_uniffi.fetchBootstrapConfig

/** ADR-0026 Phase 4 — the arming lifecycle surfaced to the settings UI. */
public sealed interface AntiCensorshipState {
    /** The user preference is off — direct path, as today. */
    public data object Disabled : AntiCensorshipState

    /** The preference is on but this build pinned no bootstrap anchor. */
    public data object Unconfigured : AntiCensorshipState

    /** Fetching + verifying the signed config. */
    public data object Arming : AntiCensorshipState

    /** The tunnel is armed; clients route through it. */
    public data object Armed : AntiCensorshipState

    /** The preference is on and a pin is present, but fetch / verify failed. */
    public data class Unavailable(val reason: String) : AntiCensorshipState
}

/**
 * Whether the person has turned anti-censorship on, read without a manager
 * (the background work has none). The same preference [AntiCensorshipManager]
 * owns.
 */
public fun antiCensorshipEnabled(context: Context): Boolean =
    context.applicationContext.getSharedPreferences("lakko.anticensorship", Context.MODE_PRIVATE)
        .getBoolean("enabled", false)

/**
 * ADR-0026 Phase 4 — the arming controller (the Kotlin twin of the iOS
 * `AntiCensorshipManager`).
 *
 * Owns the user "Anti-censorship" preference (persisted in `SharedPreferences`)
 * and, when on, fetches + verifies the signed bootstrap config against the
 * build-time-pinned trust anchor and publishes the armed
 * [AntiCensorshipConfig] so every `DiscoveryClient` (and later every other
 * client) routes through the obfuscated tunnel.
 *
 * Fail-closed: a fetch / verify failure leaves the ambient config **disarmed**
 * and surfaces a loud [AntiCensorshipState.Unavailable] — never a silent armed
 * state with an unverified config, and the failure is signalled (not silent).
 */
public class AntiCensorshipManager(
    appContext: Context,
    private val scope: CoroutineScope,
) {
    private val prefs =
        appContext.getSharedPreferences("lakko.anticensorship", Context.MODE_PRIVATE)

    private val _state = MutableStateFlow<AntiCensorshipState>(
        if (prefs.getBoolean(KEY_ENABLED, false)) {
            AntiCensorshipState.Arming
        } else {
            AntiCensorshipState.Disabled
        },
    )

    /** The current arming state, for the settings UI. */
    public val state: StateFlow<AntiCensorshipState> = _state.asStateFlow()

    /** Whether the user has turned anti-censorship on. */
    public val isEnabled: Boolean
        get() = prefs.getBoolean(KEY_ENABLED, false)

    /**
     * Apply the persisted preference: arm (fetch + verify + publish) when on,
     * otherwise reset the ambient config to disarmed. Safe to call repeatedly;
     * `MainActivity` calls it once at boot.
     */
    public fun armFromSettings() {
        if (!isEnabled) {
            AntiCensorshipConfig.current = AntiCensorshipConfig.DISARMED
            _state.value = AntiCensorshipState.Disabled
            return
        }
        val anchor = BootstrapPin.trustAnchor
        val domain = BootstrapPin.dohDomain
        val path = BootstrapPin.configPath
        if (anchor == null || domain == null || path == null) {
            // Preference on, but no anchor was pinned into this build — cannot
            // arm. Stay disarmed (loud), never an insecure default.
            AntiCensorshipConfig.current = AntiCensorshipConfig.DISARMED
            _state.value = AntiCensorshipState.Unconfigured
            Log.w(TAG, "enabled but no bootstrap pin in this build - staying disarmed")
            return
        }

        _state.value = AntiCensorshipState.Arming
        val nowSecs = (System.currentTimeMillis() / 1000L).toULong()
        scope.launch {
            try {
                // Synchronous FFI (drives a private tokio runtime); run off the
                // main dispatcher.
                val configJson = withContext(Dispatchers.IO) {
                    fetchBootstrapConfig(domain, path, anchor, nowSecs)
                }
                AntiCensorshipConfig.current = AntiCensorshipConfig(
                    armed = true,
                    bootstrapConfigJson = configJson,
                    trustAnchor = anchor,
                )
                _state.value = AntiCensorshipState.Armed
                Log.i(TAG, "armed - clients route through the obfuscated tunnel")
            } catch (e: Exception) {
                // Fail-closed: disarm + surface the failure loudly.
                AntiCensorshipConfig.current = AntiCensorshipConfig.DISARMED
                _state.value = AntiCensorshipState.Unavailable(e.message ?: e.toString())
                Log.w(TAG, "config fetch/verify failed - staying disarmed", e)
            }
        }
    }

    /** Persist the preference and (re-)apply it. */
    public fun setEnabled(on: Boolean) {
        prefs.edit().putBoolean(KEY_ENABLED, on).apply()
        armFromSettings()
    }

    private companion object {
        private const val TAG = "AntiCensorship"
        private const val KEY_ENABLED = "enabled"
    }
}
