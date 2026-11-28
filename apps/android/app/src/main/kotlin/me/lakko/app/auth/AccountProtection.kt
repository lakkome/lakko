// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context
import kotlinx.coroutines.flow.first

/**
 * ADR-0075 (amended 2026-11-26, twice) / `docs/ux/identity-recovery.md`
 * sections 19.1 to 19.1d - how an account is protected, and everything the
 * screens decide from it. Pure: the server's answers and the account's own
 * seal announcement go in, what a screen draws comes out, so a host JVM test
 * runs every rule.
 *
 * One kind of account since the email vault was removed: a mailed code gives
 * a new password and confirms a key reset, never the keys. Every account
 * with a username has at least one second way in (a recovery email, an
 * authenticator app, a passkey); the recovery phrase is an optional extra.
 * What brings the chats back after a reset is what the account published in
 * its seal announcement ([ChatsKeptBy], `auth/ResetSeals.kt`).
 */

/** `GET /recovery-email` for the session's account. Never the address. */
public data class RecoveryEmailStatus(
    /** The server can send a recovery code. */
    val available: Boolean,
    /** The account has a recovery email. */
    val enabled: Boolean,
    /** The account has a confirmed authenticator. */
    val totp: Boolean = false,
)

/**
 * Whether NEW words are shown after the account keys changed (a rotation, a
 * recall): only to an account whose owner saved the phrase. After a key
 * reset the new phrase is not saved (ADR-0101 section 10), so no words are
 * shown; the person saves a phrase from Settings when they want one.
 */
public fun showsNewWords(phraseSaved: Boolean): Boolean = phraseSaved

/** The rows of "Restore your keys" (section 8) that depend on the account. */
public data class RestoreKeysRows(val phrase: Boolean)

/**
 * Restore your keys on a signed-in device without keys: the 12-word phrase
 * row only for an account whose owner saved the phrase ([phraseSaved], read
 * from the account's published protection). Not known keeps the row (the
 * words may exist). No mailed code brings keys, so there is no email row.
 */
public fun restoreKeysRows(phraseSaved: Boolean?): RestoreKeysRows = RestoreKeysRows(phrase = phraseSaved ?: true)

// ---------------------------------------------------------------------------
// The three second ways in (19.1a, 19.1c)
// ---------------------------------------------------------------------------

/** One of the three. The phrase is not one of them. */
public enum class FactorKind { Email, Totp, Passkey }

/** Which of the three the account has. */
public data class AccountFactors(val email: Boolean, val totp: Boolean, val passkey: Boolean) {
    val any: Boolean get() = email || totp || passkey

    /** The authenticator is the account's only second way in. */
    val onlyTotp: Boolean get() = totp && !email && !passkey

    /** The factors in the fixed order of the reset panel (15.1b). */
    val inOrder: List<FactorKind>
        get() = buildList {
            if (totp) add(FactorKind.Totp)
            if (email) add(FactorKind.Email)
            if (passkey) add(FactorKind.Passkey)
        }
}

/**
 * The account's factors from `GET /recovery-email` and the passkey list.
 * `null` while either could not be read: nothing is decided from a guess
 * (the landing rule shows no step, a removal is left to the server).
 */
public fun accountFactors(status: RecoveryEmailStatus?, hasPasskey: Boolean?): AccountFactors? {
    if (status == null || hasPasskey == null) return null
    return AccountFactors(email = status.enabled, totp = status.totp, passkey = hasPasskey)
}

/**
 * The landing rule (19.1c): an account with a username and none of the
 * three is shown the protect step before the app, with no way past it. A
 * guest (no username) never is; factors that could not be read decide
 * nothing.
 */
public fun factorStepRequired(hasUsername: Boolean, factors: AccountFactors?): Boolean =
    hasUsername && factors != null && !factors.any

/**
 * Whether the landing's step stays up (19.1c): once shown it is latched until
 * its `Continue`, so adding the first way in marks that row Done instead of
 * closing the step, and the person may add another before going on.
 */
public fun factorStepLatched(open: Boolean, hasUsername: Boolean, factors: AccountFactors?): Boolean =
    open || factorStepRequired(hasUsername, factors)

/** The landing rule without a flash (alignment 2026-11-27): how long the app
 *  waits for the factor read before it shows the app anyway. Chosen, not
 *  measured. */
public const val FACTOR_READ_WAIT_MS: Long = 3_000L

/**
 * After a sign-in or at launch the app content is not shown before the
 * factor read answers: wait for the first non-null [factors], at most
 * [timeoutMs]. The answer, or `null` when it did not come in time (the app
 * is then shown, and no step: only an answer with no factor shows it).
 */
public suspend fun awaitFactorRead(
    factors: kotlinx.coroutines.flow.Flow<AccountFactors?>,
    timeoutMs: Long = FACTOR_READ_WAIT_MS,
): AccountFactors? = kotlinx.coroutines.withTimeoutOrNull(timeoutMs) {
    factors.first { it != null }
}

/** Whether the landing cover hides the app: the factor read has not
 *  answered yet for this landing ([waited] is `false`), and no other step
 *  (the protect step after sign-up) is drawn instead. */
public fun landingCoverShown(waited: Boolean, protectStepShown: Boolean): Boolean = !waited && !protectStepShown

/** The step's heading: sign-up, or an account found without a factor. */
public fun protectStepTitleKey(afterSignup: Boolean): String =
    if (afterSignup) "protect.title" else "protect.title_required"

/** The server's refusal of removing the last of the three. */
public const val LAST_FACTOR: String = "last-factor"

private const val HTTP_CONFLICT_STATUS: Int = 409

/** `409 last-factor` from `DELETE /recovery-email`, `DELETE
 *  /webauthn/credentials/{id}` or a handle change that would drop the
 *  recovery email. */
public fun isLastFactor(err: Throwable): Boolean = when (err) {
    is AuthHttpError -> err.status == HTTP_CONFLICT_STATUS && err.code == LAST_FACTOR
    is me.lakko.app.webauthn.WebauthnHttpError ->
        err.status == HTTP_CONFLICT_STATUS && me.lakko.app.anon.errorCodeOf(err.body) == LAST_FACTOR
    else -> false
}

/**
 * Whether removing one [removing] factor would leave the account with none
 * of the three ([passkeyCount] passkeys now): the app's own dialog refuses
 * it before anything is sent (`protect.last_factor`). The server refuses it
 * too.
 */
public fun removalLeavesNoFactor(factors: AccountFactors, removing: FactorKind, passkeyCount: Int): Boolean =
    !afterRemoval(factors, removing, passkeyCount).any

/**
 * The line a removal's confirmation adds when the authenticator would be
 * the only second way in afterwards (19.1c), or `null`.
 */
public fun removalTotpOnlyNoticeKey(
    factors: AccountFactors,
    removing: FactorKind,
    passkeyCount: Int,
    phraseSaved: Boolean,
): String? {
    if (factors.onlyTotp || !afterRemoval(factors, removing, passkeyCount).onlyTotp) return null
    return if (phraseSaved) "protect.totp_only.remove_notice_phrase" else "protect.totp_only.remove_notice"
}

private fun afterRemoval(factors: AccountFactors, removing: FactorKind, passkeyCount: Int): AccountFactors =
    when (removing) {
        FactorKind.Email -> factors.copy(email = false)
        FactorKind.Totp -> factors.copy(totp = false)
        FactorKind.Passkey -> factors.copy(passkey = factors.passkey && passkeyCount > 1)
    }

/**
 * The authenticator-only warning (19.1a, 19.1c), in the warning colour:
 * shown while the authenticator is the only one of the three. `null`
 * otherwise.
 */
public fun totpOnlyWarningKey(factors: AccountFactors, phraseSaved: Boolean): String? = when {
    !factors.onlyTotp -> null
    phraseSaved -> "protect.totp_only.warning_phrase"
    else -> "protect.totp_only.warning"
}

// ---------------------------------------------------------------------------
// What keeps the chats (19.1c status line, 19.1d)
// ---------------------------------------------------------------------------

/** What would bring the chats back after a key reset, strongest first. */
public enum class ChatsKeptBy {
    /** A saved recovery phrase. */
    Phrase,

    /** A passkey that keeps the chats. */
    PasskeySeal,

    /** The password as it is now, and nothing else. */
    PasswordOnly,
}

/** From the account's published protection (its seal announcement). */
public fun chatsKeptBy(phraseSeal: Boolean, passkeySeal: Boolean): ChatsKeptBy = when {
    phraseSeal -> ChatsKeptBy.Phrase
    passkeySeal -> ChatsKeptBy.PasskeySeal
    else -> ChatsKeptBy.PasswordOnly
}

/** The catalog key of the Settings > Account status line. */
public fun protectionStatusKey(kept: ChatsKeptBy): String = when (kept) {
    ChatsKeptBy.Phrase -> "protect.status.phrase"
    ChatsKeptBy.PasskeySeal -> "protect.status.passkey_seal"
    ChatsKeptBy.PasswordOnly -> "protect.status.password_only"
}

/**
 * The honest notice "Your chats depend on your password" (19.1d): the
 * account has a second way in and is kept by the password only. `null`
 * [kept] (the account's protection could not be read) shows nothing.
 */
public fun passwordOnlyNoticeShown(hasFactor: Boolean, kept: ChatsKeptBy?): Boolean =
    hasFactor && kept == ChatsKeptBy.PasswordOnly

/**
 * The notice's third place: the app's own dialog, once after a sign-in on a
 * device, the first time the app opens there. Never after a key reset (the
 * reset's screens said what was kept) and never again once closed.
 */
public fun passwordOnlyDialogDue(
    noticeShown: Boolean,
    shownBefore: Boolean,
    afterKeyReset: Boolean,
): Boolean = noticeShown && !shownBefore && !afterKeyReset

/** The `Recovery email` row of Settings > Account. */
public enum class RecoveryEmailRow {
    /** Mail is not set up on this server: the row is absent. */
    Absent,

    /** `Off`, opening the add form. */
    Off,

    /** `On`, with `Replace` and `Remove`. */
    On,
}

public fun recoveryEmailRow(status: RecoveryEmailStatus): RecoveryEmailRow = when {
    !status.available -> RecoveryEmailRow.Absent
    !status.enabled -> RecoveryEmailRow.Off
    else -> RecoveryEmailRow.On
}

/**
 * What the add form starts with: the held address when the account has no
 * recovery email now (after a recall the server dropped the stored value and
 * a letter must confirm the address again), nothing when an address is being
 * replaced.
 */
public fun enrolPrefill(enabled: Boolean, heldAddress: String?): String =
    if (!enabled) heldAddress.orEmpty() else ""

/** The `Recovery phrase` row of Settings > Account. */
public enum class RecoveryPhraseRow {
    /** No phrase saved: `Save a recovery phrase` (section 19.5). */
    Save,

    /** A phrase is saved and this device holds it: `Show phrase`. */
    Show,

    /** A phrase is saved; this device does not hold the words. */
    SavedElsewhere,
}

public fun recoveryPhraseRow(phraseSaved: Boolean, phraseOnDevice: Boolean): RecoveryPhraseRow = when {
    !phraseSaved -> RecoveryPhraseRow.Save
    phraseOnDevice -> RecoveryPhraseRow.Show
    else -> RecoveryPhraseRow.SavedElsewhere
}

/**
 * Whether a "three words" surface exists on this device (section 19.1): only
 * for an account whose owner saved the phrase. Otherwise the words were
 * never shown to the person, so asking for three of them is a dead end.
 */
public fun threeWordsSurfaceOffered(phraseSaved: Boolean): Boolean = phraseSaved

/** How "Save a recovery phrase" ended (19.5, amended 2026-11-26: no key
 *  changes; the account's protection is published again with the phrase). */
public sealed interface PhraseSavedOutcome {
    /** The phrase is in the account's published protection. */
    public data object Saved : PhraseSavedOutcome

    /** No live proof: the block appears, nothing counted. */
    public data object ProofRequired : PhraseSavedOutcome

    /** Offline, or anything else: "We could not finish." with `Try again`. */
    public data object Failed : PhraseSavedOutcome
}

/** What [savePhraseKeepingKeys] needs from the device and the directory. */
public interface PhraseKeptPorts {
    /** A fresh-auth proof is live (17.1). */
    public fun proofLive(): Boolean

    /** Publish the account's seal announcement with the phrase seal in it;
     *  throws when it did not land. */
    public suspend fun publishWithPhrase()

    /** The account's phrase is saved: keep the flag and tell the screens. */
    public fun storeSaved()
}

/**
 * The last step of "Save a recovery phrase", after the three words passed.
 * Nothing counts as saved before the account's protection was published with
 * the phrase in it: a person who leaves before then has saved nothing.
 */
public suspend fun savePhraseKeepingKeys(ports: PhraseKeptPorts): PhraseSavedOutcome {
    if (!ports.proofLive()) return PhraseSavedOutcome.ProofRequired
    try {
        ports.publishWithPhrase()
    } catch (cancel: kotlinx.coroutines.CancellationException) {
        throw cancel
    } catch (_: Throwable) {
        return PhraseSavedOutcome.Failed
    }
    ports.storeSaved()
    return PhraseSavedOutcome.Saved
}

/** The "Protect your account" step (section 19.1a), in the order drawn:
 *  email, authenticator, passkey, then the optional phrase. */
public data class ProtectStepRows(
    val email: Boolean,
    val totp: Boolean,
    val passkey: Boolean,
    val phrase: Boolean,
)

/**
 * The rows of the protect step: the email row is absent when the server
 * cannot send mail or the account has no username (a guest); the
 * authenticator row is ALWAYS there (the server refuses to start without
 * it); the passkey row only where this device can use passkeys (and the
 * account has a username: a passkey is registered under it); the phrase row
 * is always there, under `Optional`.
 */
public fun protectStepRows(
    mailAvailable: Boolean,
    hasUsername: Boolean,
    passkeysSupported: Boolean = false,
): ProtectStepRows = ProtectStepRows(
    email = mailAvailable && hasUsername,
    totp = true,
    passkey = passkeysSupported && hasUsername,
    phrase = true,
)

/** Which rows of the step are done (each is marked `protect.done`). */
public data class ProtectStepDone(
    val email: Boolean = false,
    val totp: Boolean = false,
    val passkey: Boolean = false,
    val phrase: Boolean = false,
) {
    /** One of the first three is done: the only thing `Continue` asks. */
    val factor: Boolean get() = email || totp || passkey

    val factors: AccountFactors get() = AccountFactors(email = email, totp = totp, passkey = passkey)
}

/**
 * The done marks: a row is done when its flow finished on the step ([here])
 * or the account already has it ([status], [hasPasskey], [phraseSaved]).
 */
public fun protectStepDone(
    here: ProtectStepDone,
    status: RecoveryEmailStatus?,
    hasPasskey: Boolean = false,
    phraseSaved: Boolean = false,
): ProtectStepDone = ProtectStepDone(
    email = here.email || status?.enabled == true,
    totp = here.totp || status?.totp == true,
    passkey = here.passkey || hasPasskey,
    phrase = here.phrase || phraseSaved,
)

/**
 * `Continue` is the only way off the step and is enabled only when one of
 * the first three rows is done; a saved phrase alone does not enable it.
 * Disabled is drawn with `protect.continue_needs` under it, never hidden.
 */
public fun protectContinueEnabled(done: ProtectStepDone): Boolean = done.factor

/**
 * The honest notice on the step (19.1a, 19.1d): once a second way in is done
 * and while neither a phrase nor a passkey that keeps the chats is.
 * [passkeyKeepsChats] is what the passkey created on the step turned out to
 * be. It never blocks `Continue`.
 */
public fun protectStepNoticeShown(done: ProtectStepDone, passkeyKeepsChats: Boolean): Boolean =
    done.factor && !done.phrase && !passkeyKeepsChats

/** Why a typed address cannot be sent, by the core's rule
 *  (`normalize_recovery_email`): trimmed, one `@`, both sides non-empty, 3
 *  to 254 bytes, no whitespace or control character. */
public fun recoveryEmailPlausible(typed: String): Boolean {
    val address = typed.trim().lowercase()
    if (address.any { it.isISOControl() || it.isWhitespace() }) return false
    val bytes = address.toByteArray(Charsets.UTF_8).size
    if (bytes < RECOVERY_EMAIL_MIN_BYTES || bytes > RECOVERY_EMAIL_MAX_BYTES) return false
    val parts = address.split('@')
    return parts.size == 2 && parts[0].isNotEmpty() && parts[1].isNotEmpty()
}

public const val RECOVERY_EMAIL_MIN_BYTES: Int = 3
public const val RECOVERY_EMAIL_MAX_BYTES: Int = 254

/** The length of a mailed code and of an authenticator code. */
public const val EMAIL_CODE_DIGITS: Int = 6

/** The six digits of a mailed code as typed (spaces and dashes dropped), or
 *  `null` while it is not six digits. */
public fun normalizeEmailCode(typed: String): String? {
    val digits = typed.filter { !it.isWhitespace() && it != '-' }
    return digits.takeIf { it.length == EMAIL_CODE_DIGITS && it.all { c -> c in '0'..'9' } }
}

/**
 * What this device remembers about the account's protection: plain flags
 * and times, no secret, never the address.
 *
 *   - **phrase saved**: the account's published protection lists a phrase
 *     seal, as last read from the account's own seal announcement or set
 *     when the phrase was saved on or brought to this device. Gates every
 *     three-word surface and `Show phrase`.
 *   - **password-only dialog shown**: the once-per-device dialog of 19.1d
 *     was closed here.
 *   - **protect step owed**: sign-up finished and the "Protect your account"
 *     step was not answered yet (a restart shows it again).
 */
public class AccountProtectionStore(private val storage: MarkerStorage) {

    public fun phraseSaved(accountIdB64: String): Boolean = read(PHRASE_PREFIX + accountIdB64) != null

    public fun setPhraseSaved(accountIdB64: String, saved: Boolean) {
        if (saved) write(accountIdB64, PHRASE_PREFIX + accountIdB64, "1") else drop(PHRASE_PREFIX + accountIdB64)
    }

    /** The once-per-device dialog of 19.1d was closed on this device. */
    public fun passwordOnlyDialogShown(accountIdB64: String): Boolean = read(DIALOG_PREFIX + accountIdB64) != null

    public fun markPasswordOnlyDialogShown(accountIdB64: String): Unit =
        write(accountIdB64, DIALOG_PREFIX + accountIdB64, "1")

    /** The guest chose a username and its first password is not set yet. */
    public fun firstPasswordOwed(accountIdB64: String): Boolean = read(FIRST_PASSWORD_PREFIX + accountIdB64) != null

    public fun setFirstPasswordOwed(accountIdB64: String, owed: Boolean) {
        if (owed) write(accountIdB64, FIRST_PASSWORD_PREFIX + accountIdB64, "1") else drop(FIRST_PASSWORD_PREFIX + accountIdB64)
    }

    public fun protectStepOwed(accountIdB64: String): Boolean = read(STEP_PREFIX + accountIdB64) != null

    public fun setProtectStepOwed(accountIdB64: String, owed: Boolean) {
        if (owed) write(accountIdB64, STEP_PREFIX + accountIdB64, "1") else drop(STEP_PREFIX + accountIdB64)
    }

    /** Everything kept for [accountIdB64] (Forget device, a wipe). */
    public fun clearAccount(accountIdB64: String) {
        drop(PHRASE_PREFIX + accountIdB64)
        drop(DIALOG_PREFIX + accountIdB64)
        drop(STEP_PREFIX + accountIdB64)
        drop(FIRST_PASSWORD_PREFIX + accountIdB64)
    }

    private fun read(key: String): String? = try {
        storage.get(key)
    } catch (_: Throwable) {
        null
    }

    private fun write(accountIdB64: String, key: String, value: String) {
        try {
            // Refused for a signed-out or wiped account
            // ([me.lakko.app.account.AccountStoreGate]).
            me.lakko.app.account.AccountStoreGate.write(accountIdB64) { storage.set(key, value) }
        } catch (_: Throwable) {
            // The flag then lives for this process only.
        }
    }

    private fun drop(key: String) {
        try {
            storage.remove(key)
        } catch (_: Throwable) {
            // Nothing to drop.
        }
    }

    public companion object {
        public const val PREFS_NAME: String = "lakko-account-protection"
        private const val PHRASE_PREFIX: String = "phrase-saved:"
        private const val DIALOG_PREFIX: String = "password-only-dialog:"
        private const val STEP_PREFIX: String = "protect-step-owed:"
        private const val FIRST_PASSWORD_PREFIX: String = "first-password-owed:"

        public fun onDevice(context: Context): AccountProtectionStore =
            AccountProtectionStore(SharedPrefsMarkerStorage(context, PREFS_NAME))
    }
}

/**
 * The recovery email address as this device's synced state holds it.
 * [ready] is `false` until the state is open (nothing can be read or
 * written before that). Never logged.
 */
public data class RecoveryAddressState(val ready: Boolean, val address: String?) {
    override fun toString(): String = "RecoveryAddressState(ready=$ready, held=${address != null})"

    public companion object {
        public val NOT_READY: RecoveryAddressState = RecoveryAddressState(ready = false, address = null)
    }
}

/** What the auth side needs of the synced state's recovery-email register. */
public interface RecoveryAddressPort {
    /** The register as held now. */
    public fun state(): RecoveryAddressState

    /** Write the register (`null` = removed); `false` when the state is not
     *  open yet. */
    public suspend fun write(address: String?): Boolean
}
