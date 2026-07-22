// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import android.content.SharedPreferences
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

/**
 * Hardware-backed storage for the channel **publisher signing seeds** (G-29
 * hardening, invariant #7) — the Android sibling of the iOS
 * `ChannelPublisherKeyStore`, and the publisher-side twin of [ChannelKeyStore].
 *
 * A channel publisher signs every `ChannelPost` with a per-channel Ed25519
 * key. Its 32-byte seed used to live in memory only, so an app relaunch minted
 * a *fresh* signing key and the publisher could no longer continue its channel
 * under the same key (subscribers verify against the key earlier posts were
 * signed with). [ChannelsUiState] now persists the seed here — an
 * [EncryptedSharedPreferences] backed by an AES-256-GCM [MasterKey] in the
 * Android Keystore, never plaintext `SharedPreferences` — so the publisher
 * survives a relaunch and keeps signing under the same key. It matches the
 * device-identity stores' (`PeerPinStore` / `AccountIdentitySeedStore`)
 * at-rest posture.
 *
 * Distinct from the channel **content-key** seed, which is re-derived from the
 * persisted MLS PublisherGroup exporter on each publisher open (not stored
 * here).
 */
public object ChannelPublisherKeyStore {

    /** Encrypted prefs file name (distinct from the subscriber-key store). */
    private const val PREFS_NAME: String = "lakko.channels.publisher-keys.enc"

    /**
     * Build the [EncryptedSharedPreferences] [ChannelsUiState] persists the
     * publisher signing seeds into. The value/key are AES-encrypted at rest
     * under the Keystore master key; the `SharedPreferences` interface is
     * unchanged, so the caller's `getString` / `putString` paths work verbatim.
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
