// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import android.content.Context
import android.util.Log
import me.lakko.app.auth.ThreeWordsBudgetStore
import uniffi.lakko_ffi_uniffi.RecoveryMnemonic

/**
 * The recovery phrase on a device that holds the account's keys.
 *
 * Mirror of `apps/web/src/backup/device-phrase.ts`. A device that holds the
 * keys also holds the 12-word phrase, in [PhraseStore] (Keystore-backed
 * `EncryptedSharedPreferences`). Three things rest on that:
 *
 *   - the local three-word check on the recovery screen: the device compares
 *     the typed words with the phrase it holds, no server takes part;
 *   - handing the keys to a device added through "another device" (the
 *     recovery payload carries the phrase entropy);
 *   - "Show recovery phrase" in Settings and the encrypted backup.
 *
 * So every path that lands a device with keys writes the phrase store.
 * [LandingPath] names them; `AuthRepository` calls [store] once per path, and
 * `DevicePhraseTest` reads the source and fails when a named path has no call.
 *
 * There is no path that lands keys without the phrase: keys only ever arrive
 * as the phrase (typed, minted, opened from a passkey) or as its entropy
 * (another device).
 */
public object DevicePhrase {

    /** Every way a device comes to hold the account's keys. */
    public enum class LandingPath(public val tag: String) {
        /** `signup`: the phrase was just generated. */
        Signup("signup"),

        /** `signupGuest`: generated for a username-less account. */
        SignupGuest("signup-guest"),

        /** `recover`: the 12 words plus a new password. */
        Recover("recover"),

        /** `provideRecoveryPhrase`: typed in the restore prompt, or opened
         *  from a passkey that carries the keys (that route ends here too). */
        ProvidePhrase("provide-phrase"),

        /** `completeFromPairing`: another device, on a signed-in device. */
        DevicePairing("device-pairing"),

        /** `completeFromDeviceRestore`: another device, no password. */
        DeviceRestore("device-restore"),

        /** `mintFreshIdentity`: a new identity and a new phrase. */
        FreshIdentity("fresh-identity"),

        /** ADR-0066 part 3: the account keys were changed and the devices
         *  kept (the rotating device, and a kept device adopting). */
        KeyRotation("key-rotation"),
    }

    /**
     * Store [phrase] for [accountIdB64]. Best-effort like the other caches:
     * a failed write is logged (with the path, never the phrase) and the
     * landing continues; the device then simply does not offer what needs
     * the phrase. Returns whether the write happened. A write that happened
     * also lifts a suspended three-word route for the account.
     */
    public fun store(
        context: Context,
        accountIdB64: String,
        phrase: String,
        path: LandingPath,
    ): Boolean = storeWith(
        phrase = phrase,
        path = path,
        write = { bytes -> PhraseStore.putPhrase(context, accountIdB64, bytes) },
        // The phrase was just written by a landing path, so it is the
        // account's: a suspended three-word route is offered again.
        onStored = { ThreeWordsBudgetStore.onDevice(context).clearSuspended(accountIdB64) },
    )

    /** [store] over an injected writer (host tests). The UTF-8 copy handed
     *  to [write] is zeroed afterwards. */
    internal fun storeWith(
        phrase: String,
        path: LandingPath,
        write: (ByteArray) -> Unit,
        onStored: () -> Unit = {},
        log: (String, Throwable) -> Unit = { message, cause -> Log.w("lakko-auth", message, cause) },
    ): Boolean {
        val bytes = phrase.toByteArray(Charsets.UTF_8)
        return try {
            write(bytes)
            // Only after a write that happened; a failure here is logged
            // like a failed write (the marker then outlives this landing).
            onStored()
            true
        } catch (cause: Throwable) {
            log("recovery phrase store (${path.tag}) failed", cause)
            false
        } finally {
            bytes.fill(0)
        }
    }

    /** The phrase this device holds for [accountIdB64], or `null`: none
     *  stored, or storage that cannot be read. Never throws. */
    public fun read(context: Context, accountIdB64: String): String? =
        try {
            val bytes = PhraseStore.getPhrase(context, accountIdB64)
            if (bytes == null) {
                null
            } else {
                val phrase = String(bytes, Charsets.UTF_8)
                bytes.fill(0)
                phrase.takeIf { it.isNotBlank() }
            }
        } catch (cause: Throwable) {
            Log.w("lakko-auth", "reading the recovery phrase stored on this device failed", cause)
            null
        }

    /** Whether a phrase is stored for [accountIdB64]. */
    public fun isStored(context: Context, accountIdB64: String): Boolean =
        read(context, accountIdB64) != null

    /**
     * The 16-byte entropy of the phrase this device holds (what the
     * another-device transfer carries), or `null` when there is no stored
     * phrase or it does not parse. The caller owns zeroing the result.
     */
    public fun entropy(context: Context, accountIdB64: String): ByteArray? =
        entropyOfStoredPhrase(read(context, accountIdB64)) { phrase ->
            RecoveryMnemonic.fromPhrase(phrase).use { it.toEntropy() }
        }
}

/** Entropy length of the 12-word recovery phrase (BIP39, 128 bits). */
public const val PHRASE_ENTROPY_LENGTH: Int = 16

/**
 * The entropy of [storedPhrase] through [toEntropy] (the core's
 * `RecoveryMnemonic.fromPhrase(..).toEntropy()`), or `null` when there is no
 * phrase, it does not parse, or the core answers with another length.
 */
internal fun entropyOfStoredPhrase(
    storedPhrase: String?,
    toEntropy: (String) -> ByteArray,
): ByteArray? {
    if (storedPhrase == null) return null
    val entropy = try {
        toEntropy(storedPhrase)
    } catch (_: Throwable) {
        return null
    }
    if (entropy.size != PHRASE_ENTROPY_LENGTH) {
        entropy.fill(0)
        return null
    }
    return entropy
}
