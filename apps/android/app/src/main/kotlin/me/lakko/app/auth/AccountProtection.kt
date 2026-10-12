// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context

/**
 * ADR-0067 / `docs/ux/identity-recovery.md` section 19 - how an account is
 * protected, and everything the screens decide from it. Pure: the answer of
 * `GET /recovery-email` goes in, what a screen draws comes out, so a host
 * JVM test runs every rule.
 */

/** `GET /recovery-email` for the session's account. Never the address. */
public data class RecoveryEmailStatus(
    /** The server can send a recovery code. */
    val available: Boolean,
    /** The account has a recovery email. */
    val enabled: Boolean,
    /** The account has an email vault: a mailed code brings its keys back. */
    val vault: Boolean,
    /** The owner saved a recovery phrase: a mailed code changes the password
     *  only. */
    val phraseSaved: Boolean,
    /** The account has an authenticator: its code is asked for as well. */
    val totp: Boolean = false,
)

/**
 * Whether NEW words are shown after the account keys changed (a rotation, a
 * key reset, a recall): only to a phrase-protected account. An account
 * without a saved phrase is never shown words anywhere; its change completes
 * silently as far as the phrase goes.
 */
public fun showsNewWords(phraseSaved: Boolean): Boolean = phraseSaved

/** The no-protection banner's line and the email row's title: after the
 *  keys changed the server keeps the address value without a vault, and the
 *  person is asked to type the address again. */
public data class ProtectPrompt(val title: String, val body: String?)

public fun protectPrompt(status: RecoveryEmailStatus): ProtectPrompt =
    if (recoveryEmailRow(status) == RecoveryEmailRow.AddAgain) {
        ProtectPrompt(title = "recovery_email.add_again.title", body = "recovery_email.add_again.body")
    } else {
        ProtectPrompt(title = "protect.banner.none", body = null)
    }

/** The rows of "Restore your keys" (section 8) that depend on the account. */
public data class RestoreKeysRows(val phrase: Boolean, val emailCode: Boolean)

/**
 * Restore your keys on a signed-in device without keys: the 12-word phrase
 * row only for an account whose owner saved the phrase, and the `Email code`
 * row only while a mailed code brings the keys (an email AND its vault). A
 * status that could not be read keeps the phrase row (the words may exist)
 * and offers no email row.
 */
public fun restoreKeysRows(status: RecoveryEmailStatus?): RestoreKeysRows = when {
    status == null -> RestoreKeysRows(phrase = true, emailCode = false)
    else -> RestoreKeysRows(
        phrase = status.phraseSaved,
        emailCode = status.available && status.enabled && status.vault && !status.phraseSaved,
    )
}

/** The three states of section 19.1. */
public enum class AccountProtection {
    /** No phrase saved; a mailed code brings everything back. */
    Email,

    /** A phrase is saved. */
    Phrase,

    /** Neither: the password and the devices that hold it. */
    None,
}

/**
 * The account's mode. A saved phrase wins. Without one the account is
 * mail-protected only while the server holds BOTH the address value and the
 * vault: after a key reset or a recall the vault is gone (it sealed the old
 * phrase) and the account is unprotected until the email is added again.
 */
public fun accountProtection(enabled: Boolean, vault: Boolean, phraseSaved: Boolean): AccountProtection = when {
    phraseSaved -> AccountProtection.Phrase
    enabled && vault -> AccountProtection.Email
    else -> AccountProtection.None
}

public fun accountProtection(status: RecoveryEmailStatus): AccountProtection =
    accountProtection(status.enabled, status.vault, status.phraseSaved)

/** The catalog key of the Settings > Account status line. */
public fun protectionStatusKey(protection: AccountProtection): String = when (protection) {
    AccountProtection.Email -> "protect.status.email"
    AccountProtection.Phrase -> "protect.status.phrase"
    AccountProtection.None -> "protect.status.none"
}

/** The `Recovery email` row of Settings > Account. */
public enum class RecoveryEmailRow {
    /** Mail is not set up on this server: the row is absent. */
    Absent,

    /** `Off`, opening the add form. */
    Off,

    /** The server keeps the address value but no vault and no phrase is
     *  saved (a key reset or a recall dropped the vault): shown `Off`, and
     *  adding the address again seals the new phrase. */
    AddAgain,

    /** `On`, with `Replace` and `Remove`. */
    On,
}

public fun recoveryEmailRow(status: RecoveryEmailStatus): RecoveryEmailRow = when {
    !status.available -> RecoveryEmailRow.Absent
    !status.enabled -> RecoveryEmailRow.Off
    !status.vault && !status.phraseSaved -> RecoveryEmailRow.AddAgain
    else -> RecoveryEmailRow.On
}

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
 * for a phrase-protected account. In a mail-protected account the words were
 * never shown to the person, so asking for three of them is a dead end.
 */
public fun threeWordsSurfaceOffered(phraseSaved: Boolean): Boolean = phraseSaved

/**
 * The enrol sealing rule (ADR-0067 section 6): the vault is sealed and sent
 * with `enrol/finish` only while no phrase is saved. A phrase-protected
 * account never gets a vault (the server answers `409 phrase-saved`).
 */
public fun enrolSealsVault(phraseSaved: Boolean): Boolean = !phraseSaved

/** The "Protect your account" step of sign-up (section 19.1a), in the
 *  order drawn: email, passkey, phrase. */
public data class ProtectStepRows(val email: Boolean, val passkey: Boolean, val phrase: Boolean)

/**
 * The rows of the protect step: the email row is absent when the server
 * cannot send mail or the account has no username (a guest); the passkey row
 * only where this device can use passkeys (and the account has a username:
 * a passkey is registered under it); the phrase row is always there.
 */
public fun protectStepRows(
    mailAvailable: Boolean,
    hasUsername: Boolean,
    passkeysSupported: Boolean = false,
): ProtectStepRows = ProtectStepRows(
    email = mailAvailable && hasUsername,
    passkey = passkeysSupported && hasUsername,
    phrase = true,
)

/** Which rows of the step are done (each is marked `protect.done`). */
public data class ProtectStepDone(
    val email: Boolean = false,
    val passkey: Boolean = false,
    val phrase: Boolean = false,
) {
    val any: Boolean get() = email || passkey || phrase
}

/**
 * The done marks. The email row is done when the account has a recovery
 * email that works (`enabled && (vault || phraseSaved)`, read from
 * [status]); the passkey row when one was created on the step ([here]) or
 * the account already has one ([hasPasskey]); the phrase row when a phrase
 * is saved.
 */
public fun protectStepDone(
    here: ProtectStepDone,
    status: RecoveryEmailStatus?,
    hasPasskey: Boolean = false,
): ProtectStepDone = ProtectStepDone(
    email = status != null && status.enabled && (status.vault || status.phraseSaved),
    passkey = here.passkey || hasPasskey,
    phrase = here.phrase || status?.phraseSaved == true,
)

/** How the step is left. */
public enum class ProtectStepExit {
    /** Nothing is done: the plain `Not now`, behind the confirmation. */
    NotNow,

    /** A row is done: the primary `Continue`, no confirmation. */
    Continue,
}

public fun protectStepExit(done: ProtectStepDone): ProtectStepExit =
    if (done.any) ProtectStepExit.Continue else ProtectStepExit.NotNow

/** How long a dismissed no-protection banner stays away. */
public const val PROTECT_BANNER_RETURN_MS: Long = 30L * 24L * 60L * 60L * 1000L

/**
 * The one banner of an unprotected account (Settings > Account, section
 * 19.1a): shown while the account has no way back in, unless it was
 * dismissed less than 30 days ago. A clock set back never hides it longer.
 */
public fun protectBannerVisible(
    protection: AccountProtection,
    dismissedAtMs: Long?,
    nowMs: Long,
    /** The account has at least one passkey: it has a way back in, so the
     *  banner (and its 30-day return) is not shown. */
    hasPasskey: Boolean = false,
): Boolean {
    if (protection != AccountProtection.None || hasPasskey) return false
    if (dismissedAtMs == null) return true
    val since = nowMs - dismissedAtMs
    return since < 0L || since >= PROTECT_BANNER_RETURN_MS
}

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
 *   - **phrase saved**: the account is phrase-protected, as last learned at
 *     a landing (`GET /recovery-email`) or set when the phrase was saved on
 *     or brought to this device. Gates every three-word surface and `Show
 *     phrase`.
 *   - **banner dismissed at**: when the no-protection banner was closed.
 *   - **protect step owed**: sign-up finished and the "Protect your account"
 *     step was not answered yet (a restart shows it again).
 */
public class AccountProtectionStore(private val storage: MarkerStorage) {

    public fun phraseSaved(accountIdB64: String): Boolean = read(PHRASE_PREFIX + accountIdB64) != null

    public fun setPhraseSaved(accountIdB64: String, saved: Boolean) {
        if (saved) write(PHRASE_PREFIX + accountIdB64, "1") else drop(PHRASE_PREFIX + accountIdB64)
    }

    public fun bannerDismissedAtMs(accountIdB64: String): Long? =
        read(BANNER_PREFIX + accountIdB64)?.toLongOrNull()

    public fun dismissBanner(accountIdB64: String, nowMs: Long): Unit =
        write(BANNER_PREFIX + accountIdB64, nowMs.toString())

    public fun protectStepOwed(accountIdB64: String): Boolean = read(STEP_PREFIX + accountIdB64) != null

    public fun setProtectStepOwed(accountIdB64: String, owed: Boolean) {
        if (owed) write(STEP_PREFIX + accountIdB64, "1") else drop(STEP_PREFIX + accountIdB64)
    }

    /** Everything kept for [accountIdB64] (Forget device, a wipe). */
    public fun clearAccount(accountIdB64: String) {
        drop(PHRASE_PREFIX + accountIdB64)
        drop(BANNER_PREFIX + accountIdB64)
        drop(STEP_PREFIX + accountIdB64)
    }

    private fun read(key: String): String? = try {
        storage.get(key)
    } catch (_: Throwable) {
        null
    }

    private fun write(key: String, value: String) {
        try {
            storage.set(key, value)
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
        private const val BANNER_PREFIX: String = "banner-dismissed:"
        private const val STEP_PREFIX: String = "protect-step-owed:"

        public fun onDevice(context: Context): AccountProtectionStore =
            AccountProtectionStore(SharedPrefsMarkerStorage(context, PREFS_NAME))
    }
}
