// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import me.lakko.app.l10n.UiText

/*
 * ADR-0073 (amended 2026-11-26) / UX `identity-recovery.md` 15.1b and 15.1c -
 * a key reset needs the password AND one second factor the account has: a
 * code from its authenticator app, a code mailed to its recovery email, or
 * one of its passkeys. The recovery phrase is not one of them (whoever has
 * it restores the keys and does not reset).
 *
 * Everything the reset panel decides is here, pure, so the host suite runs
 * it: which panel is drawn, in which order a confirmation goes (the password
 * first, then the factor), what each refusal reads as, when the button is
 * enabled, and which line says whether the chats will come back.
 */

/** What the reset panel draws under the warning. */
public enum class ResetPanel {
    /** The factor section: the password block (when no password proof is
     *  live) and the rows of the factors the account has. */
    Factors,

    /** `I cannot use any of these`: the "cannot reset" panel with `Back`. */
    CannotUse,

    /** The account has none of the three: the same panel, the other body,
     *  no `Back`. No control to add a factor: adding one needs only the
     *  password, so a person who knows nothing else could add and reset. */
    NoFactor,

    /** The account's factors are being read (or could not be): nothing is
     *  offered yet. */
    Loading,
}

/**
 * The panel for [factors] (`null` while not read). [saidCannot] is the
 * person's `I cannot use any of these`.
 */
public fun resetPanel(factors: AccountFactors?, saidCannot: Boolean): ResetPanel = when {
    factors == null -> ResetPanel.Loading
    !factors.any -> ResetPanel.NoFactor
    saidCannot -> ResetPanel.CannotUse
    else -> ResetPanel.Factors
}

/**
 * The rows of the factor section, in the fixed order: authenticator, email,
 * passkey. A row the account does not have is absent, never disabled; the
 * passkey row is also absent where this device cannot use passkeys.
 */
public fun resetFactorRows(factors: AccountFactors, passkeysUsableHere: Boolean): List<FactorKind> =
    factors.inOrder.filter { it != FactorKind.Passkey || passkeysUsableHere }

/**
 * Whether the password side lets a factor be confirmed now: a password
 * proof is live, or the password is typed (and the username too, where this
 * device does not know it). While it is `false` the rows can be opened but
 * their action (the code field, `Send code`, `Use passkey`) waits: the
 * password is checked first, and there is none to check.
 */
public fun resetPasswordReady(
    passwordProofLive: Boolean,
    password: String,
    usernameKnown: Boolean,
    username: String,
): Boolean = passwordProofLive || (password.isNotEmpty() && (usernameKnown || username.isNotBlank()))

/** The first line of the factor section: with the password block above it,
 *  or without (a password proof is live). */
public fun resetFactorTitleKey(passwordProofLive: Boolean): String =
    if (passwordProofLive) "proof.reset.factor.title_no_password" else "proof.reset.factor.title"

/** The one line the section collapses to once a factor is confirmed. */
public fun resetFactorDoneKey(factor: FactorKind): String = when (factor) {
    FactorKind.Totp -> "proof.reset.factor.done.totp"
    FactorKind.Email -> "proof.reset.factor.done.email"
    FactorKind.Passkey -> "proof.reset.factor.done.passkey"
}

/** The "cannot reset" panel's body. */
public fun resetCannotBodyKey(panel: ResetPanel): String =
    if (panel == ResetPanel.NoFactor) "proof.reset.no_factor.body_none" else "proof.reset.no_factor.body"

/**
 * The "keeps working" bullet of the warning (UX 15.1, amended 2026-11-28):
 * the password always, then the authenticator app and the recovery email
 * only when the account has them. The passkeys are never named: the reset
 * deletes them.
 */
public fun resetKeepsKey(factors: AccountFactors): String = when {
    factors.totp && factors.email -> "restore_identity.reset.keeps.password_totp_email"
    factors.totp -> "restore_identity.reset.keeps.password_totp"
    factors.email -> "restore_identity.reset.keeps.password_email"
    else -> "restore_identity.reset.keeps.password"
}

/**
 * The bullets of the warning above the reset panel, in order; `null` when
 * it is not drawn. It describes a reset this account can do, so it is drawn
 * only above the factor section: never above a "cannot reset" panel (no
 * factor, or `I cannot use any of these`), and not while the factors are
 * still being read (the reset may turn out impossible). Whether the chats
 * come back is not a bullet: the line of 15.1c says it.
 */
public fun resetWarningKeys(panel: ResetPanel, factors: AccountFactors?): List<String>? {
    if (panel != ResetPanel.Factors || factors == null) return null
    return listOf(
        "restore_identity.reset.effect_devices",
        "restore_identity.reset.effect_passkeys",
        "restore_identity.reset.effect_safety_number",
        resetKeepsKey(factors),
    )
}

/** A confirmed factor: what the panel shows and the reset sends. */
public class ResetFactorConfirmed(
    public val factor: FactorKind,
    /** Unix milliseconds; the confirmation is unusable at or after this. */
    public val expiresAtMs: Long,
    /** Whether the reset would be sealed (UX 15.1c), decided on this device
     *  from the account's published protection. */
    public val prediction: SealPrediction,
)

/** Whether [confirmed] still stands at [nowMs]. A confirmation that ran out
 *  opens the section again as it was, with no error styling. */
public fun resetFactorLive(confirmed: ResetFactorConfirmed?, nowMs: Long): Boolean =
    confirmed != null && nowMs < confirmed.expiresAtMs

/** `Reset account keys` is enabled only when a factor is confirmed and the
 *  word is typed. */
public fun resetButtonEnabled(wordMatches: Boolean, confirmed: ResetFactorConfirmed?, nowMs: Long, busy: Boolean): Boolean =
    wordMatches && !busy && resetFactorLive(confirmed, nowMs)

/**
 * The second line of an unsealed prediction: "Use a passkey that keeps your
 * chats to bring them back." with `Use passkey`, only when the account has
 * such a passkey, it was not the factor used, and this device can use
 * passkeys.
 */
public fun unsealedOffersPasskey(prediction: SealPrediction, passkeysUsableHere: Boolean): Boolean =
    prediction is SealPrediction.Unsealed && prediction.passkeyCouldSeal && passkeysUsableHere

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/** The server's refusal of a reset without a usable factor grant. */
public const val RESET_FACTOR_REQUIRED: String = "reset-factor-required"

/** The one refusal of every factor route: a wrong code, a decoy or spent
 *  challenge, no such factor on the account. */
public const val FACTOR_REFUSED: String = "factor-refused"

private const val HTTP_UNAUTHORIZED: Int = 401
private const val HTTP_FORBIDDEN: Int = 403
private const val HTTP_GONE_STATUS: Int = 410
private const val HTTP_TOO_MANY_STATUS: Int = 429

/** No live factor confirmation, or the server refused it (`403
 *  reset-factor-required`): the section opens again; nothing was reset. */
public class ResetFactorRequired : RuntimeException("resetting the account keys needs a confirmed second factor")

public fun isResetFactorRequired(err: Throwable): Boolean = when (err) {
    is ResetFactorRequired -> true
    is AuthHttpError -> err.status == HTTP_FORBIDDEN && err.code == RESET_FACTOR_REQUIRED
    else -> false
}

/** Which request of the factor section failed. */
public enum class ResetFactorCall { Password, Totp, EmailStart, EmailFinish, Passkey }

/** What a failed confirmation shows. */
public sealed interface ResetFactorFailure {
    /** Under the password field; no factor was tried, no code used up. */
    public data object WrongPassword : ResetFactorFailure

    /** The password proof ran out or was never made: the block appears. */
    public data object PasswordNeeded : ResetFactorFailure

    /** One line under the expanded row; the digits clear and keep focus. */
    public data class WrongCode(val line: UiText) : ResetFactorFailure

    /** The email row returns to its `Send code` state with the line. */
    public data class EmailRestart(val line: UiText) : ResetFactorFailure

    /** `recovery.error.rate_limited` with the countdown, on that row only:
     *  the other rows stay usable. */
    public data class Wait(val retryAfterSecs: Long?) : ResetFactorFailure

    /** One line under the row. */
    public data class Line(val line: UiText) : ResetFactorFailure
}

/**
 * Classify a failure of [call] (UX 15.1b's state table). The password is
 * checked FIRST: a wrong password stops there and the factor is not tried.
 * A limit is a wait with its countdown, never a wrong code.
 */
@Suppress("CyclomaticComplexMethod", "ReturnCount")
public fun resetFactorFailureOf(call: ResetFactorCall, err: Throwable): ResetFactorFailure {
    if (err is FreshAuthWrongPassword) return ResetFactorFailure.WrongPassword
    if (isFreshAuthRequired(err)) return ResetFactorFailure.PasswordNeeded
    if (err is AuthNetworkError) return ResetFactorFailure.Line(UiText("common.error.offline"))
    if (err is me.lakko.app.webauthn.WebauthnUserCancelledError || err is PasskeyCancelled) {
        return ResetFactorFailure.Line(UiText("proof.reset.cancelled"))
    }
    if (err is FreshAuthOtherAccount) return ResetFactorFailure.Line(UiText("proof.passkey.other_account"))
    val http = err as? AuthHttpError
    if (http == null) {
        return ResetFactorFailure.Line(
            UiText(if (call == ResetFactorCall.Passkey) "proof.passkey.failed" else "restore_identity.reset.error.generic"),
        )
    }
    if (http.status == HTTP_TOO_MANY_STATUS) return ResetFactorFailure.Wait(http.retryAfterSecs)
    return when {
        call == ResetFactorCall.Password && http.status == HTTP_UNAUTHORIZED -> ResetFactorFailure.WrongPassword
        http.code == FACTOR_REFUSED && call == ResetFactorCall.Totp ->
            ResetFactorFailure.WrongCode(UiText("settings.recovery.totp.error.wrong"))
        http.code == FACTOR_REFUSED && call == ResetFactorCall.EmailFinish ->
            ResetFactorFailure.WrongCode(UiText("recovery_email.code.wrong"))
        http.code == FACTOR_REFUSED && call == ResetFactorCall.Passkey ->
            ResetFactorFailure.Line(UiText("proof.passkey.failed"))
        http.code == "code-expired" || http.status == HTTP_GONE_STATUS ->
            ResetFactorFailure.EmailRestart(UiText("recovery_email.code.expired"))
        http.code == "mail-send-failed" -> ResetFactorFailure.Line(UiText("recovery_email.send_failed"))
        http.code == "address-invalid" -> ResetFactorFailure.Line(UiText("recovery_email.invalid"))
        http.code == RECOVERY_EMAIL_DISABLED -> ResetFactorFailure.Line(UiText("recovery_email.unavailable"))
        else -> ResetFactorFailure.Line(UiText("restore_identity.reset.error.generic"))
    }
}

/**
 * A wrong mailed code: five tries per code, then "Too many attempts. Send a
 * new code." and the row returns to `Send code`. [wrongTries] counts the
 * tries BEFORE this one.
 */
public fun afterWrongResetEmailCode(wrongTries: Int): ResetFactorFailure =
    if (emailCodeUsedUp(wrongTries + 1)) {
        ResetFactorFailure.EmailRestart(UiText("recovery_email.code.used_up"))
    } else {
        ResetFactorFailure.WrongCode(UiText("recovery_email.code.wrong"))
    }

// ---------------------------------------------------------------------------
// The seal of the reset (ADR-0101 section 10)
// ---------------------------------------------------------------------------

/** One seal key the resetting device can sign with, with its secret. */
public class ResetSealOption(public val entry: SealEntryView, public val secret: SealSecret)

/**
 * The seal keys the resetting device holds, in the order they are tried:
 * the password just proven, the passkey just used (when its authenticator
 * gave a seal secret), the phrase (when this device holds the words). A
 * secret the core refuses is left out.
 */
public fun resetSealOptions(
    core: SealCore,
    exportKey: ByteArray?,
    passkey: SealSecret.Passkey?,
    phrase: String?,
): List<ResetSealOption> = buildList {
    fun offer(secret: SealSecret?) {
        if (secret == null) return
        val entry = try {
            core.entryOf(secret)
        } catch (_: Throwable) {
            return
        }
        add(ResetSealOption(entry, secret))
    }
    offer(exportKey?.takeIf { it.size == ExportKeyStore.EXPORT_KEY_LENGTH }?.let { SealSecret.Password(it) })
    offer(passkey)
    offer(phrase?.let { SealSecret.Phrase(it) })
}

/** The option [prediction] chose, or `null` for an unsealed reset. */
public fun chosenSealOption(prediction: SealPrediction, options: List<ResetSealOption>): ResetSealOption? =
    (prediction as? SealPrediction.Sealed)?.let { sealed -> options.firstOrNull { it.entry == sealed.entry } }
