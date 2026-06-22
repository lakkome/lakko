// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import android.content.SharedPreferences
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

/**
 * Hardware-backed storage for the channel **subscriber keypairs** (G-29
 * hardening, invariant #7) — the Android parallel of the iOS
 * `ChannelSubscriberKeyStore`.
 *
 * The subscriber-side X25519 **secret** half unwraps the per-subscriber HPKE
 * seed envelopes a channel publisher fans out on every rotation. It is
 * persisted by [ChannelsUiState] via an [EncryptedSharedPreferences] backed by
 * an AES-256-GCM [MasterKey] in the Android Keystore — never plaintext
 * `SharedPreferences` — so it matches the device-identity stores'
 * (`PeerPinStore` / `AccountIdentitySeedStore`) at-rest posture.
 */
public object ChannelKeyStore {

    /** Encrypted prefs file name (distinct from the device-identity stores). */
    private const val PREFS_NAME: String = "lakko.channels.subscriber-keys.enc"

    /**
     * Build the [EncryptedSharedPreferences] [ChannelsUiState] persists the
     * subscriber keypairs into. The value/key are AES-encrypted at rest under
     * the Keystore master key; the `SharedPreferences` interface is unchanged,
     * so the caller's `getString` / `putString` paths work verbatim.
     */
    public fun encryptedPrefs(context: Context): SharedPreferences {
        val master = MasterKey.Builder(context.applicationContext)
            .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
            .build()
        return EncryptedSharedPreferences.create(
            context.applicationContext,
            PREFS_NAME,
            master,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    }
}
