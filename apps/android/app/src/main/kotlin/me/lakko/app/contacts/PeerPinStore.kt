// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import android.content.Context
import android.util.Base64
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import me.lakko.app.transparency.LogVerifyStatus

/**
 * G-4 (ADR-0016) — persistent per-account AccountIdentity pubkey
 * PIN store.
 *
 * Mirror of:
 *
 *   * `apps/web/src/contacts/local-store.ts` (the `pins` map +
 *     `readPin` / `setPin` / `removePin`)
 *   * `apps/ios/Lakko/Contacts/PeerPinStore.swift`
 *
 * A peer's discovery identity bundle carries the peer's
 * AccountIdentity Ed25519 `signature_pubkey`; that key **rotates**
 * on recover-from-words. ADR-0016 correctness rule #3 requires the
 * pin be keyed on the stable **account_id**, not on the (mutable)
 * pubkey — keying on the pubkey forks the contact silently when the
 * key changes, defeating the supersession-proof bridge.
 *
 * Each pin records:
 *   * [Pin.pinnedSignaturePubkeyB64] — the currently-trusted
 *     AccountIdentity pubkey for this account (mutable; advanced
 *     only via a verified supersession proof on the fetch path).
 *   * [Pin.rotationMarker] — a **monotonic** marker (ADR-0016 rule
 *     #4): the `expires_at` unix-seconds of the last-accepted
 *     proof, default `0` for a TOFU-pin. A re-pin that would move
 *     the marker backwards is refused, blunting within-window
 *     rollback.
 *
 * ## Why a dedicated persistent store is genuinely new on Android
 *
 * Android re-polls the stranger inbox from cursor `0` on each
 * launch, and the contacts list is in-memory only — there was **no**
 * persistent contact-pubkey pin before this. The pin therefore MUST
 * survive a process restart and MUST NOT be re-derived from a fresh
 * poll, or every relaunch would silently re-TOFU a changed key,
 * defeating the gate.
 *
 * ## Threat model
 *
 * The pin bytes are NOT secret (a pubkey is public), but the
 * *trust* decision they encode is security-critical — see the
 * fetch-path verify in [ContactsClient]. We still store them in
 * [EncryptedSharedPreferences] for the same posture as
 * [me.lakko.app.auth.ExportKeyStore] (at-rest hardware-backed
 * encryption, never leaves the Keystore sandbox) so a device-image
 * attacker cannot silently rewrite a pin to mount a downgrade.
 *
 * ## Storage layout
 *
 * `EncryptedSharedPreferences` named `"lakko-peer-pins"`. Keys:
 * `"pin-v1:<accountIdB64>"`. Value: `"<pubkeyB64>|<marker>"` — the
 * pubkey base64 and the decimal `rotationMarker`, separated by a
 * single `|` (the pubkey base64 never contains `|`).
 */

/** A pinned AccountIdentity pubkey for one peer (ADR-0016). */
public data class Pin(
    /** The currently-trusted AccountIdentity Ed25519 pubkey for
     *  this account, base64-encoded (standard alphabet, padded).
     *  Advanced only via a verified supersession proof. */
    val pinnedSignaturePubkeyB64: String,
    /** Monotonic rotation marker (ADR-0016 rule #4): the
     *  `expires_at` unix-seconds of the last-accepted proof, or
     *  `0` for a plain TOFU-pin. A re-pin that would lower this is
     *  refused. */
    val rotationMarker: Long,
    /** KT-log S3e: the last transparency-log inclusion verdict observed
     *  for this account on the identity-fetch path. The log check is the
     *  equivocation-detection layer ON TOP of the supersession/pin gate
     *  (ADR-0016); this records the latest best-effort outcome so the UI
     *  can surface it. `null` on a pin written before S3e (or by a path
     *  that did not run the check). */
    val logStatus: LogVerifyStatus? = null,
)

/** Outcome of [PinStore.setPin]. [Set] covers both a first-time
 *  TOFU-pin and a monotonic forward re-pin; [RejectedBackward]
 *  signals the marker would have moved backwards (ADR-0016 rule
 *  #4) so nothing was written. */
public enum class SetPinResult {
    Set,
    RejectedBackward,
}

/**
 * The abstract pin-store surface the fetch-path gate
 * ([ContactsClient.verifyBundleAccountKey]) drives. Abstracted so
 * the verify *decision* logic is unit-testable with an in-memory
 * implementation that needs no Android `Context` / native lib.
 */
public interface PinStore {
    /**
     * Read the pinned AccountIdentity pubkey for a peer, keyed on
     * the peer's stable `account_id` (base64, standard alphabet).
     *
     * Returns `null` when no pin exists yet — the caller treats
     * that as the TOFU (trust-on-first-use) case and pins the
     * freshly-observed key.
     */
    public fun readPin(accountIdB64: String): Pin?

    /**
     * Set or update the account-pubkey pin for `accountIdB64`,
     * enforcing the monotonic-marker invariant (ADR-0016 rule #4).
     *
     * Semantics:
     *   * No existing pin → store `{ pubkey, marker }` (TOFU or
     *     first verified pin). Returns [SetPinResult.Set].
     *   * Existing pin and `marker >= existing.rotationMarker` →
     *     overwrite the pubkey + advance the marker. Returns
     *     [SetPinResult.Set]. (A `==` marker permits re-pinning the
     *     *same* successor — an idempotent re-observation — without
     *     forking.)
     *   * Existing pin and `marker < existing.rotationMarker` →
     *     **refuse**, leave the pin untouched, return
     *     [SetPinResult.RejectedBackward]. Blunts a within-window
     *     rollback to an older successor.
     *
     * The pubkey is intentionally **mutable** for a given
     * `account_id`: a changed key is a *detected mutation*, not a
     * new contact (ADR-0016 rule #3). The trust gate that
     * authorises the mutation lives on the fetch path, not here.
     */
    public fun setPin(
        accountIdB64: String,
        pinnedSignaturePubkeyB64: String,
        rotationMarker: Long,
    ): SetPinResult

    /** Remove the pin for `accountIdB64`. No-op when absent.
     *  Exposed for the "forget contact" / re-verify-from-scratch
     *  flows. */
    public fun removePin(accountIdB64: String)

    /**
     * KT-log S3e — record the latest transparency-log inclusion verdict
     * for an account, keyed on the same stable `account_id` as the pin.
     *
     * Writes onto the existing pin record so the verdict travels with the
     * pinned key. **No-op when no pin exists** — the log check is always
     * run right after the supersession gate (which TOFU-pins on first
     * sight), so by the time a verdict is recorded a pin is present; if
     * for any reason it is not, we do not fabricate a pin entry with no
     * pubkey. Returns whether a write happened (a present pin).
     */
    public fun recordLogStatus(accountIdB64: String, logStatus: LogVerifyStatus): Boolean
}

/**
 * Production [PinStore] backed by [EncryptedSharedPreferences].
 *
 * Construct with an Android [Context]; the prefs are opened against
 * `context.applicationContext` so the store outlives any Activity.
 */
public class PeerPinStore(context: Context) : PinStore {

    private val appContext: Context = context.applicationContext

    override fun readPin(accountIdB64: String): Pin? {
        val raw = openPrefs(appContext)
            .getString(KEY_PREFIX + accountIdB64, null)
            ?: return null
        // Stored as "<pubkeyB64>|<marker>" or, since KT-log S3e,
        // "<pubkeyB64>|<marker>|<logStatus>". Base64 standard alphabet
        // never emits `|`, so the fields split cleanly. We parse from the
        // FRONT: the first `|` ends the pubkey; the remainder is
        // "<marker>" or "<marker>|<logStatus>".
        val firstSep = raw.indexOf('|')
        if (firstSep <= 0 || firstSep == raw.length - 1) return null
        val pubkeyB64 = raw.substring(0, firstSep)
        val rest = raw.substring(firstSep + 1)
        val secondSep = rest.indexOf('|')
        val markerStr: String
        val logStatusStr: String?
        if (secondSep < 0) {
            markerStr = rest
            logStatusStr = null
        } else {
            markerStr = rest.substring(0, secondSep)
            logStatusStr = rest.substring(secondSep + 1).ifEmpty { null }
        }
        val marker = markerStr.toLongOrNull() ?: return null
        // Defensive: the pubkey segment must be valid base64.
        try {
            Base64.decode(pubkeyB64, Base64.NO_WRAP)
        } catch (_: IllegalArgumentException) {
            return null
        }
        // A present-but-unrecognised logStatus is dropped (forward-compat
        // / tamper guard); the structural fields are still valid so the
        // pin itself is kept.
        val logStatus = logStatusStr?.let { s ->
            try {
                LogVerifyStatus.valueOf(s)
            } catch (_: IllegalArgumentException) {
                null
            }
        }
        return Pin(
            pinnedSignaturePubkeyB64 = pubkeyB64,
            rotationMarker = marker,
            logStatus = logStatus,
        )
    }

    override fun setPin(
        accountIdB64: String,
        pinnedSignaturePubkeyB64: String,
        rotationMarker: Long,
    ): SetPinResult {
        val existing = readPin(accountIdB64)
        if (existing != null && rotationMarker < existing.rotationMarker) {
            return SetPinResult.RejectedBackward
        }
        // Preserve any recorded logStatus (KT-log S3e) across a re-pin —
        // it is an orthogonal, separately-written field, and a key
        // rotation shouldn't silently drop the last log verdict.
        write(accountIdB64, pinnedSignaturePubkeyB64, rotationMarker, existing?.logStatus)
        return SetPinResult.Set
    }

    override fun removePin(accountIdB64: String) {
        openPrefs(appContext).edit().remove(KEY_PREFIX + accountIdB64).apply()
    }

    override fun recordLogStatus(accountIdB64: String, logStatus: LogVerifyStatus): Boolean {
        val existing = readPin(accountIdB64) ?: return false
        if (existing.logStatus == logStatus) return true // idempotent, no write
        write(
            accountIdB64,
            existing.pinnedSignaturePubkeyB64,
            existing.rotationMarker,
            logStatus,
        )
        return true
    }

    /** Serialize + persist one pin record. The logStatus segment is
     *  appended only when present, so a pin with no verdict stays in the
     *  legacy two-field shape. */
    private fun write(
        accountIdB64: String,
        pubkeyB64: String,
        marker: Long,
        logStatus: LogVerifyStatus?,
    ) {
        val value = if (logStatus == null) {
            "$pubkeyB64|$marker"
        } else {
            "$pubkeyB64|$marker|${logStatus.name}"
        }
        openPrefs(appContext).edit().putString(KEY_PREFIX + accountIdB64, value).apply()
    }

    public companion object {
        private const val PREFS_NAME = "lakko-peer-pins"
        private const val KEY_PREFIX = "pin-v1:"

        private fun openPrefs(
            context: Context,
        ): android.content.SharedPreferences {
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
}

/**
 * In-memory [PinStore] used by unit tests for the verify-decision
 * logic (no Android `Context`, no Keystore). Same monotonic-marker
 * semantics as [PeerPinStore]; not used in production.
 */
public class InMemoryPinStore : PinStore {
    private val pins = HashMap<String, Pin>()

    override fun readPin(accountIdB64: String): Pin? = pins[accountIdB64]

    override fun setPin(
        accountIdB64: String,
        pinnedSignaturePubkeyB64: String,
        rotationMarker: Long,
    ): SetPinResult {
        val existing = pins[accountIdB64]
        if (existing != null && rotationMarker < existing.rotationMarker) {
            return SetPinResult.RejectedBackward
        }
        // Preserve any recorded logStatus (KT-log S3e) across a re-pin.
        pins[accountIdB64] = Pin(
            pinnedSignaturePubkeyB64 = pinnedSignaturePubkeyB64,
            rotationMarker = rotationMarker,
            logStatus = existing?.logStatus,
        )
        return SetPinResult.Set
    }

    override fun removePin(accountIdB64: String) {
        pins.remove(accountIdB64)
    }

    override fun recordLogStatus(accountIdB64: String, logStatus: LogVerifyStatus): Boolean {
        val existing = pins[accountIdB64] ?: return false
        if (existing.logStatus == logStatus) return true
        pins[accountIdB64] = existing.copy(logStatus = logStatus)
        return true
    }
}
