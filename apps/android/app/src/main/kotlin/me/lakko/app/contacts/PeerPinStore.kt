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
 * `"pin-v2:<ownerAccountIdB64>:<peerAccountIdB64>"` — one namespace per
 * signed-in account (see [PeerPinStore]). Value is a `|`-separated record (the pubkey
 * base64 never contains `|`), parsed front-to-back so older shapes stay
 * readable (back-compat):
 *   * `"<pubkeyB64>|<marker>"` — the original two-field shape.
 *   * `"<pubkeyB64>|<marker>|<logStatus>"` — KT-log S3e added the verdict.
 *   * `"<pubkeyB64>|<marker>|<logStatus>|<isReset>"` — KT-log S4 added the
 *     `/latest` `is_reset` marker. The `<logStatus>` segment may be empty
 *     (an `is_reset` recorded with no verdict yet), and `<isReset>` is
 *     `"true"` / `"false"` (absent = the marker is unknown / null).
 *   * `"<pubkeyB64>|<marker>|<logStatus>|<isReset>|<pinnedLogVersion>"` —
 *     G-30 #2 (ADR-0019) added the chain-confirmed transparency-log version.
 *     Earlier segments may be empty to keep the positional layout (a version
 *     recorded with no verdict / reset writes empty slots at indices 2 and 3);
 *     `<pinnedLogVersion>` is a base-10 non-negative `Long` (absent = no
 *     chain-confirmed version floor, treated as `0`).
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
    /** KT-log S4: the `/latest` entry's `is_reset` marker captured on the
     *  identity-fetch path, alongside [logStatus]. `true` = the latest
     *  logged entry was an owner-initiated reset (ADR-0017 fresh-start);
     *  `false` = it was not (an unexplained change); `null` = there was no
     *  latest entry to read it from (off / unreachable / absent), or the
     *  pin predates S4 / the check did not run. Lets the UI distinguish a
     *  reset from a possible compromise on a changed key. */
    val isReset: Boolean? = null,
    /** G-30 #2 (ADR-0019): the transparency-log VERSION at which the
     *  currently-pinned key was chain-confirmed (the `/latest` entry's
     *  per-account-key `version`). Written ONLY on a `CONFIRMED`
     *  rotation-chain verdict ([PinStore.recordLogVersion]). The next
     *  rotation's chain proof must reference a STRICTLY-NEWER logged version,
     *  so persisting it gives the chain verifier its downgrade-protection
     *  floor. `null` on a pin that has never been chain-confirmed (a TOFU pin,
     *  or one written before G-30 #2); a missing floor degrades safely to
     *  `0`. */
    val pinnedLogVersion: Long? = null,
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

    /**
     * KT-log S4 — record the `/latest` entry's `is_reset` marker for an
     * account, keyed on the same stable `account_id` as the pin and written
     * next to [recordLogStatus].
     *
     * Writes onto the existing pin record so the marker travels with the
     * pinned key. A `null` clears any stale marker — no latest entry was
     * read this round. **No-op when no pin exists** (same rationale as
     * [recordLogStatus] — the supersession gate TOFU-pins first). Returns
     * whether a write happened (a present pin).
     */
    public fun recordLogReset(accountIdB64: String, isReset: Boolean?): Boolean

    /**
     * G-30 #2 (ADR-0019) — record the chain-confirmed transparency-log
     * VERSION for an account, keyed on the same stable `account_id` as the
     * pin and written next to [recordLogReset].
     *
     * Writes onto the existing pin record so the version travels with the
     * pinned key. A `null` clears any stored version. The caller persists it
     * ONLY on a `CONFIRMED` rotation-chain verdict — a transient
     * `UNREACHABLE` / `MISMATCH` / `ABSENT` outcome must NOT wipe a
     * previously-confirmed version, so the caller does not pass `null` on
     * those (see [ContactsClient.checkTransparencyLog]). **No-op when no pin
     * exists** (same rationale as [recordLogReset] — the supersession gate
     * TOFU-pins first). Idempotent on an unchanged value (no write). Returns
     * whether a write happened (a present pin).
     */
    public fun recordLogVersion(accountIdB64: String, version: Long?): Boolean

    /**
     * G-30 #2 (ADR-0019) — read the chain-confirmed log version for an
     * account, or `null` when no pin / no recorded version. The next
     * rotation's chain proof is checked against this as the downgrade floor
     * (default `0` when absent).
     */
    public fun readLogVersion(accountIdB64: String): Long?
}

/**
 * Production [PinStore] backed by [EncryptedSharedPreferences].
 *
 * Construct with an Android [Context]; the prefs are opened against
 * `context.applicationContext` so the store outlives any Activity.
 *
 * **Scoped to the OWNING account** ([ownerAccountIdB64], the signed-in
 * account whose trust decisions these are), like the accepted list in
 * [AcceptedContactsStore]: a second account signed in on the same phone
 * neither sees nor inherits the first account's pins. The key is
 * `pin-v2:<ownerAccountIdB64>:<peerAccountIdB64>`; the retired device-wide
 * `pin-v1:` keys are never read. [deleteForOwner] drops one owner's pins
 * (forget device).
 */
public class PeerPinStore(
    context: Context,
    /** The signed-in account these pins belong to, standard base64. */
    private val ownerAccountIdB64: String,
) : PinStore {

    private val appContext: Context = context.applicationContext

    private fun keyOf(accountIdB64: String): String = pinKey(ownerAccountIdB64, accountIdB64)

    override fun readPin(accountIdB64: String): Pin? {
        val raw = openPrefs(appContext)
            .getString(keyOf(accountIdB64), null)
            ?: return null
        // Stored as one of (KT-log back-compat, see the class doc):
        //   "<pubkeyB64>|<marker>"                                   (original)
        //   "<pubkeyB64>|<marker>|<logStatus>"                       (S3e)
        //   "<pubkeyB64>|<marker>|<logStatus>|<isReset>"             (S4)
        //   "<pubkeyB64>|<marker>|<logStatus>|<isReset>|<version>"   (G-30 #2)
        // Base64 standard alphabet never emits `|`, so the fields split
        // cleanly. We split into AT MOST 5 segments from the FRONT so the
        // pubkey is segment 0 regardless of how many later fields exist.
        val parts = raw.split('|', limit = 5)
        if (parts.size < 2) return null
        val pubkeyB64 = parts[0]
        if (pubkeyB64.isEmpty()) return null
        val markerStr = parts[1]
        val logStatusStr: String? = parts.getOrNull(2)?.ifEmpty { null }
        val isResetStr: String? = parts.getOrNull(3)?.ifEmpty { null }
        val versionStr: String? = parts.getOrNull(4)?.ifEmpty { null }
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
        // KT-log S4: only the exact tokens "true" / "false" are honoured;
        // anything else (including an absent segment) means "unknown".
        val isReset: Boolean? = when (isResetStr) {
            "true" -> true
            "false" -> false
            else -> null
        }
        // G-30 #2: a present-but-junk (non-numeric or negative) version is
        // dropped (forward-compat / tamper guard) while the pin itself stays
        // valid — a missing chain-version floor degrades safely to 0.
        val pinnedLogVersion: Long? = versionStr?.toLongOrNull()?.takeIf { it >= 0L }
        return Pin(
            pinnedSignaturePubkeyB64 = pubkeyB64,
            rotationMarker = marker,
            logStatus = logStatus,
            isReset = isReset,
            pinnedLogVersion = pinnedLogVersion,
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
        // Preserve any recorded logStatus (KT-log S3e) + isReset (KT-log
        // S4) + pinnedLogVersion (G-30 #2) across a re-pin — they are
        // orthogonal, separately-written fields, and a key rotation shouldn't
        // silently drop the last log verdict / reset marker / downgrade
        // floor. The fetch path re-records each right after a re-pin, so a
        // stale carried value is replaced immediately.
        write(
            accountIdB64,
            pinnedSignaturePubkeyB64,
            rotationMarker,
            existing?.logStatus,
            existing?.isReset,
            existing?.pinnedLogVersion,
        )
        return SetPinResult.Set
    }

    override fun removePin(accountIdB64: String) {
        openPrefs(appContext).edit().remove(keyOf(accountIdB64)).apply()
    }

    override fun recordLogStatus(accountIdB64: String, logStatus: LogVerifyStatus): Boolean {
        val existing = readPin(accountIdB64) ?: return false
        if (existing.logStatus == logStatus) return true // idempotent, no write
        write(
            accountIdB64,
            existing.pinnedSignaturePubkeyB64,
            existing.rotationMarker,
            logStatus,
            existing.isReset,
            existing.pinnedLogVersion,
        )
        return true
    }

    override fun recordLogReset(accountIdB64: String, isReset: Boolean?): Boolean {
        val existing = readPin(accountIdB64) ?: return false
        if (existing.isReset == isReset) return true // idempotent, no write
        write(
            accountIdB64,
            existing.pinnedSignaturePubkeyB64,
            existing.rotationMarker,
            existing.logStatus,
            isReset,
            existing.pinnedLogVersion,
        )
        return true
    }

    override fun recordLogVersion(accountIdB64: String, version: Long?): Boolean {
        val existing = readPin(accountIdB64) ?: return false
        if (existing.pinnedLogVersion == version) return true // idempotent, no write
        write(
            accountIdB64,
            existing.pinnedSignaturePubkeyB64,
            existing.rotationMarker,
            existing.logStatus,
            existing.isReset,
            version,
        )
        return true
    }

    override fun readLogVersion(accountIdB64: String): Long? =
        readPin(accountIdB64)?.pinnedLogVersion

    /** Serialize + persist one pin record. Trailing fields are emitted
     *  only as far as the last present value, so a pin with no verdict and
     *  no marker stays in the legacy two-field shape. When a later field is
     *  set but an earlier one is null, the earlier slot is written empty so
     *  the positional layout is preserved (e.g. an `isReset` with no
     *  `logStatus`, or a `pinnedLogVersion` with neither). */
    private fun write(
        accountIdB64: String,
        pubkeyB64: String,
        marker: Long,
        logStatus: LogVerifyStatus?,
        isReset: Boolean?,
        pinnedLogVersion: Long?,
    ) {
        val value = serializePin(pubkeyB64, marker, logStatus, isReset, pinnedLogVersion)
        openPrefs(appContext).edit().putString(keyOf(accountIdB64), value).apply()
    }

    public companion object {
        private const val PREFS_NAME = "lakko-peer-pins"
        private const val KEY_PREFIX = "pin-v2:"

        /** The retired device-wide prefix (one namespace for every account).
         *  Never read; [deleteForOwner] removes any such rows. */
        private const val LEGACY_KEY_PREFIX = "pin-v1:"

        /** The storage key of [peerAccountIdB64]'s pin as held by
         *  [ownerAccountIdB64]. Visible for the unit test. */
        internal fun pinKey(ownerAccountIdB64: String, peerAccountIdB64: String): String =
            "$KEY_PREFIX$ownerAccountIdB64:$peerAccountIdB64"

        /** The keys among [keys] that belong to [ownerAccountIdB64], plus any
         *  retired device-wide `pin-v1:` rows. Visible for the unit test. */
        internal fun keysOwnedBy(keys: Collection<String>, ownerAccountIdB64: String): List<String> {
            val prefix = "$KEY_PREFIX$ownerAccountIdB64:"
            return keys.filter { it.startsWith(prefix) || it.startsWith(LEGACY_KEY_PREFIX) }
        }

        /** The pin store of the account bound in [AcceptedContactsStore]
         *  (the signed-in one), or `null` when no session is live. */
        public fun forBoundAccount(context: Context): PeerPinStore? =
            AcceptedContactsStore.boundAccountB64?.let { PeerPinStore(context, it) }

        /** Forget device: drop every pin [ownerAccountIdB64] holds. Other
         *  accounts' pins stay. Best-effort. */
        public fun deleteForOwner(context: Context, ownerAccountIdB64: String) {
            val prefs = openPrefs(context)
            val doomed = keysOwnedBy(prefs.all.keys, ownerAccountIdB64)
            if (doomed.isEmpty()) return
            val edit = prefs.edit()
            for (key in doomed) edit.remove(key)
            edit.apply()
        }

        /**
         * Serialize a pin record to the `|`-separated on-disk form. Visible
         * for the parser unit test so the round-trip is exercised without an
         * Android `Context`. Trailing fields collapse: a pin with neither a
         * verdict, reset marker, nor chain version stays in the legacy
         * two-field shape; a later-set field writes empty slots for any
         * earlier null ones to keep the positional layout (e.g. an isReset
         * with no verdict writes an empty verdict slot; a pinnedLogVersion
         * with neither writes empty verdict + reset slots).
         */
        internal fun serializePin(
            pubkeyB64: String,
            marker: Long,
            logStatus: LogVerifyStatus?,
            isReset: Boolean?,
            pinnedLogVersion: Long?,
        ): String {
            val versionToken = pinnedLogVersion?.toString()
            val resetToken = isReset?.toString()
            return when {
                versionToken != null ->
                    "$pubkeyB64|$marker|${logStatus?.name ?: ""}|${resetToken ?: ""}|$versionToken"
                resetToken != null ->
                    "$pubkeyB64|$marker|${logStatus?.name ?: ""}|$resetToken"
                logStatus != null -> "$pubkeyB64|$marker|${logStatus.name}"
                else -> "$pubkeyB64|$marker"
            }
        }

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
        // Preserve any recorded logStatus (KT-log S3e) + isReset (S4) +
        // pinnedLogVersion (G-30 #2) across a re-pin.
        pins[accountIdB64] = Pin(
            pinnedSignaturePubkeyB64 = pinnedSignaturePubkeyB64,
            rotationMarker = rotationMarker,
            logStatus = existing?.logStatus,
            isReset = existing?.isReset,
            pinnedLogVersion = existing?.pinnedLogVersion,
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

    override fun recordLogReset(accountIdB64: String, isReset: Boolean?): Boolean {
        val existing = pins[accountIdB64] ?: return false
        if (existing.isReset == isReset) return true
        pins[accountIdB64] = existing.copy(isReset = isReset)
        return true
    }

    override fun recordLogVersion(accountIdB64: String, version: Long?): Boolean {
        val existing = pins[accountIdB64] ?: return false
        if (existing.pinnedLogVersion == version) return true
        pins[accountIdB64] = existing.copy(pinnedLogVersion = version)
        return true
    }

    override fun readLogVersion(accountIdB64: String): Long? =
        pins[accountIdB64]?.pinnedLogVersion
}
