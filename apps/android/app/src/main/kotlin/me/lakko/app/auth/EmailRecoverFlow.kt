// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import me.lakko.app.l10n.UiText

/**
 * ADR-0075 section 7 / `docs/ux/identity-recovery.md` section 19.3 - "Get
 * back in" > Email code, as a pure step machine. The view model runs the
 * requests and feeds the answers in; every decision about what the person
 * sees next is here, so a host JVM test runs the whole flow.
 *
 * The steps, in order: [EmailRecoverStep.Address] (username and address),
 * [EmailRecoverStep.Code] (always, whatever was typed), the authenticator
 * code only when the account has one, the new password, then the restore.
 * Nothing before the new password tells which kind of account this is: the
 * server is asked for the vault only after the person chose the password.
 */
public sealed interface EmailRecoverStep {
    /** Step 1: username and address. */
    public data object Address : EmailRecoverStep

    /** Step 2: "Check your email". Identical for a matching pair, a wrong
     *  pair, an unknown username and an account with no email. */
    public data class Code(
        val challengeId: String,
        /** When `Send again` becomes available (unix ms). */
        val resendAtMs: Long,
        /** When the code stops working (unix ms). */
        val expiresAtMs: Long,
        /** Wrong tries on this code so far. The server allows
         *  [EMAIL_CODE_MAX_TRIES] and then answers every further try like a
         *  wrong code, so the count is kept here to say "send a new code". */
        val wrongTries: Int = 0,
    ) : EmailRecoverStep

    /** Step 3: the authenticator's code. Only after the mailed code was
     *  accepted, and there is no way past it. */
    public data class Totp(val ticket: String) : EmailRecoverStep

    /** Step 4: "Choose a new password". */
    public data class NewPassword(val ticket: String) : EmailRecoverStep

    /** "Bringing back your account": the vault is being opened and the
     *  phrase restore runs, or the new password is being set. */
    public data object Restoring : EmailRecoverStep

    /** The flow ended without a session; the person is back on the list of
     *  methods with [line] shown there. Nothing changed on the server. */
    public data class Closed(val line: UiText) : EmailRecoverStep
}

/** Everything the email form renders. The address and the codes live here
 *  in memory only; nothing of this is ever persisted or logged. */
public data class EmailRecoverState(
    val step: EmailRecoverStep = EmailRecoverStep.Address,
    val error: UiText? = null,
    /** A server wait (too many letters / attempts): when it ends, unix ms. */
    val waitUntilMs: Long? = null,
    val busy: Boolean = false,
) {
    override fun toString(): String = "EmailRecoverState(step=${step::class.simpleName}, busy=$busy)"
}

/** `recover/start` answered. The same answer for a match and a miss, so the
 *  same step: this function takes no fact about the account. */
public fun afterEmailRecoverStart(
    challengeId: String,
    resendAfterSecs: Long,
    expiresInSecs: Long,
    nowMs: Long,
): EmailRecoverState = EmailRecoverState(
    step = EmailRecoverStep.Code(
        challengeId = challengeId,
        resendAtMs = nowMs + resendAfterSecs.coerceAtLeast(0L) * MILLIS,
        expiresAtMs = nowMs + expiresInSecs.coerceAtLeast(0L) * MILLIS,
    ),
)

/** `recover/verify` accepted the mailed code. */
public fun afterEmailRecoverVerify(ticket: String, totpRequired: Boolean): EmailRecoverState =
    EmailRecoverState(
        step = if (totpRequired) EmailRecoverStep.Totp(ticket) else EmailRecoverStep.NewPassword(ticket),
    )

/** `recover/totp` accepted the authenticator code. */
public fun afterEmailRecoverTotp(ticket: String): EmailRecoverState =
    EmailRecoverState(step = EmailRecoverStep.NewPassword(ticket))

/** What `recover/open` released. */
public sealed interface EmailRecoverOpen {
    /** No phrase saved: the sealed keys. The ticket is spent. */
    public class Vault(public val vaultSalt: ByteArray, public val sealedEntropy: ByteArray) : EmailRecoverOpen

    /** A phrase is saved: the new password only. */
    public data object Password : EmailRecoverOpen
}

/** What the device does with the answer of `recover/open`. */
public enum class EmailRecoverAction {
    /** Open the vault with the typed username and address, rebuild the
     *  phrase, and run the existing phrase restore with the new password. */
    RestoreFromVault,

    /** Set the new password under the ticket and land the session. */
    SetPassword,
}

public fun emailRecoverAction(open: EmailRecoverOpen): EmailRecoverAction = when (open) {
    is EmailRecoverOpen.Vault -> EmailRecoverAction.RestoreFromVault
    EmailRecoverOpen.Password -> EmailRecoverAction.SetPassword
}

/**
 * The vault did not open under the typed username and address (the ticket is
 * already spent). Nothing was changed on the server: the person is told the
 * account could not be brought back and is back on "Get back in".
 */
public fun afterEmailVaultNotOpened(): EmailRecoverState =
    EmailRecoverState(step = EmailRecoverStep.Closed(UiText("recovery.email.failed")))

/** Whether the code step's code has run out at [nowMs]. */
public fun emailCodeExpired(step: EmailRecoverStep.Code, nowMs: Long): Boolean = nowMs >= step.expiresAtMs

/** What a failed request of the flow means. */
public sealed interface EmailRecoverFailure {
    /** One line under the fields; the step stays. */
    public data class Line(val text: UiText) : EmailRecoverFailure

    /** The line, and the code fields are cleared (a wrong code). */
    public data class WrongCode(val text: UiText) : EmailRecoverFailure

    /** Back to the address step with the line (the code is used up, or the
     *  ticket ran out: a new code is needed). */
    public data class Restart(val text: UiText) : EmailRecoverFailure

    /** A wait with a countdown (`recovery.error.rate_limited`). */
    public data class Wait(val retryAfterSecs: Long?) : EmailRecoverFailure

    /** Mail is not set up on this server. */
    public data object Unavailable : EmailRecoverFailure
}

/** Which request failed. */
public enum class EmailRecoverCall { Start, Verify, Totp, Open, Password }

/**
 * Classify a failure of [call]. A wrong code and a code that was never sent
 * read the same ("That code is not right."); a limit is never shown as a
 * wrong code; a transport failure is the offline line.
 */
@Suppress("ReturnCount")
public fun emailRecoverFailureOf(call: EmailRecoverCall, err: Throwable): EmailRecoverFailure {
    if (err is AuthNetworkError) return EmailRecoverFailure.Line(UiText("common.error.offline"))
    if (err is AuthClientArgError) {
        return EmailRecoverFailure.Line(err.text ?: UiText("recovery.error.unavailable"))
    }
    val http = err as? AuthHttpError ?: return EmailRecoverFailure.Line(UiText("recovery.error.unavailable"))
    if (http.code == RECOVERY_EMAIL_DISABLED || http.code == "login-by-name-disabled") {
        return EmailRecoverFailure.Unavailable
    }
    // A limit is a wait with its countdown, never a wrong code.
    if (http.status == HTTP_TOO_MANY) return EmailRecoverFailure.Wait(http.retryAfterSecs)
    return when (http.code) {
        "address-invalid" -> EmailRecoverFailure.Line(UiText("recovery_email.invalid"))
        "code-refused" -> EmailRecoverFailure.WrongCode(
            UiText(if (call == EmailRecoverCall.Totp) "recovery.totp.error.wrong" else "recovery_email.code.wrong"),
        )
        "code-expired", "ticket-refused" -> EmailRecoverFailure.Restart(UiText("recovery_email.code.expired"))
        // The authenticator step was skipped: cannot happen through this
        // machine; treated as a dead ticket.
        "totp-required", "wrong-mode" -> EmailRecoverFailure.Restart(UiText("recovery_email.code.expired"))
        else -> EmailRecoverFailure.Line(UiText("recovery.error.unavailable"))
    }
}

/** Apply [failure] to [state] at [nowMs]. */
public fun afterEmailRecoverFailure(
    state: EmailRecoverState,
    failure: EmailRecoverFailure,
    nowMs: Long,
): EmailRecoverState = when (failure) {
    is EmailRecoverFailure.Line -> state.copy(busy = false, error = failure.text)
    is EmailRecoverFailure.WrongCode -> {
        val code = state.step as? EmailRecoverStep.Code
        val tries = (code?.wrongTries ?: 0) + 1
        when {
            code == null -> state.copy(busy = false, error = failure.text)
            // The fifth wrong try used the code up: a new one is needed.
            emailCodeUsedUp(tries) ->
                EmailRecoverState(step = EmailRecoverStep.Address, error = UiText("recovery_email.code.used_up"))
            else -> state.copy(busy = false, error = failure.text, step = code.copy(wrongTries = tries))
        }
    }
    is EmailRecoverFailure.Restart ->
        EmailRecoverState(step = EmailRecoverStep.Address, error = failure.text)
    is EmailRecoverFailure.Wait -> state.copy(
        busy = false,
        error = if (failure.retryAfterSecs == null) UiText("common.error.too_many_attempts") else null,
        waitUntilMs = failure.retryAfterSecs?.let { nowMs + it * MILLIS },
    )
    EmailRecoverFailure.Unavailable -> state.copy(busy = false, error = UiText("recovery_email.unavailable"))
}

/** Whether [failure] empties the code fields. */
public fun emailFailureClearsCode(failure: EmailRecoverFailure): Boolean =
    failure is EmailRecoverFailure.WrongCode || failure is EmailRecoverFailure.Restart

/** How many tries one mailed code allows (the server's `MAX_CODE_ATTEMPTS`). */
public const val EMAIL_CODE_MAX_TRIES: Int = 5

/** Whether [wrongTries] wrong tries have used a code up: "Too many attempts.
 *  Send a new code." and back to the address step. */
public fun emailCodeUsedUp(wrongTries: Int): Boolean = wrongTries >= EMAIL_CODE_MAX_TRIES

/** `503` body code when the server has no mail pepper. */
public const val RECOVERY_EMAIL_DISABLED: String = "recovery-email-disabled"

private const val HTTP_TOO_MANY: Int = 429
private const val MILLIS: Long = 1_000L

/** `enrol/start` answered: the code is on its way and the vault salt is known. */
public class RecoveryEmailEnrolStart(
    public val challengeId: String,
    /** The 16-byte salt the email vault is sealed with. */
    public val vaultSalt: ByteArray,
    public val resendAfterSecs: Long,
    public val expiresInSecs: Long,
)

/** The email vault's salt (ADR-0075 section 2). */
public const val EMAIL_VAULT_SALT_LEN: Int = 16

/** The sealed phrase entropy: the passkey vault's 44-byte blob shape. */
public const val EMAIL_VAULT_SEALED_LEN: Int = 44

/**
 * Classify a failure of the enrol routes (UX 19.2's table). [finishing] is
 * the code step. The same shapes as the recover flow, so one form code
 * applies them ([afterEmailRecoverFailure] is not used here: the enrol form
 * keeps the address step's fields).
 */
@Suppress("CyclomaticComplexMethod", "ReturnCount")
public fun recoveryEmailEnrolFailureOf(finishing: Boolean, err: Throwable): EmailRecoverFailure {
    if (err is AuthNetworkError) return EmailRecoverFailure.Line(UiText("common.error.offline"))
    if (err is AuthClientArgError) {
        return EmailRecoverFailure.Line(err.text ?: UiText("common.error.unavailable"))
    }
    val http = err as? AuthHttpError ?: return EmailRecoverFailure.Line(UiText("common.error.unavailable"))
    if (http.code == RECOVERY_EMAIL_DISABLED) return EmailRecoverFailure.Unavailable
    if (http.status == HTTP_TOO_MANY) return EmailRecoverFailure.Wait(http.retryAfterSecs)
    return when (http.code) {
        "address-invalid" -> EmailRecoverFailure.Line(UiText("recovery_email.invalid"))
        "username-required" -> EmailRecoverFailure.Line(UiText("recovery_email.needs_username"))
        "mail-send-failed" -> EmailRecoverFailure.Line(UiText("recovery_email.send_failed"))
        "code-refused" -> EmailRecoverFailure.WrongCode(UiText("recovery_email.code.wrong"))
        // Expired: the code step stays, with `Send again`.
        "code-expired" -> EmailRecoverFailure.WrongCode(UiText("recovery_email.code.expired"))
        else -> EmailRecoverFailure.Line(UiText("common.error.unavailable"))
    }
}
