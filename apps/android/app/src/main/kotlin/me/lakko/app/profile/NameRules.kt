// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import me.lakko.app.l10n.UiText
import uniffi.lakko_ffi_uniffi.FfiException

/**
 * ADR-0048 §4 / UX `docs/ux/profile-names.md` §4 — the two input rules the UI
 * shows as the user types: the username rule (signup and every search field)
 * and the profile-name rule (signup and Settings -> Profile).
 *
 * ## The rule lives in the core, never here
 *
 * Both checks call the shared Rust core (`validateUsername` /
 * `normalizeDisplayName`) and only MAP its typed refusal to copy. This file
 * does not restate the character table: a client that re-implemented the
 * control / format-character list would drift from the envelope codec, and a
 * name the field accepted would then be one the receiver refuses.
 *
 * The core functions are injected with their production binding as the
 * default, so the mapping runs in a plain JVM unit test without the `.so`
 * (the `FfiException` subclasses are plain Kotlin classes; only the FFI calls
 * need native code).
 *
 * ## Fail closed
 *
 * An error the core did not name as a username / display-name refusal is not
 * guessed at: it reads as [UsernameProblem.UNUSABLE] /
 * [DisplayNameProblem.UNUSABLE], which blocks the lookup or the save exactly
 * like a named refusal does.
 */

/**
 * Why a username fails the ADR-0048 §4 rule. [text] is what a screen shows
 * (ADR-0058: the catalog key, rendered in the current language); [copy] is
 * the English sentence for logs and exception messages only, never a screen.
 */
public enum class UsernameProblem(public val copy: String, public val text: UiText) {
    EMPTY("Enter a username", UiText("username.error.empty")),
    TOO_SHORT("At least 3 characters", UiText("username.error.too_short", counts = mapOf("count" to 3uL))),
    TOO_LONG("At most 32 characters", UiText("username.error.too_long", counts = mapOf("count" to 32uL))),
    INVALID_CHARACTERS("Only Latin letters, digits and _", UiText("username.error.invalid_characters")),

    /** The core refused for a reason it did not classify as a username rule. */
    UNUSABLE("This username can't be used", UiText("username.error.unknown")),
}

/** Outcome of checking a typed username. */
public sealed interface UsernameCheck {
    /** Passes the rule; [normalized] is the canonical form (`@Alice` -> `alice`). */
    public data class Valid(val normalized: String) : UsernameCheck

    /** Fails the rule for [problem]. */
    public data class Invalid(val problem: UsernameProblem) : UsernameCheck
}

/** The username refusal [error] carries, or `null` when it is not one. */
public fun usernameProblemOf(error: Throwable): UsernameProblem? = when (error) {
    is FfiException.UsernameEmpty -> UsernameProblem.EMPTY
    is FfiException.UsernameTooShort -> UsernameProblem.TOO_SHORT
    is FfiException.UsernameTooLong -> UsernameProblem.TOO_LONG
    is FfiException.UsernameInvalidCharacters -> UsernameProblem.INVALID_CHARACTERS
    else -> null
}

/**
 * Check [input] against the username rule.
 *
 * @param validate the core's `validateUsername` (returns the normalised form or
 *   throws the typed refusal). Injected for host tests.
 */
public fun checkUsername(
    input: String,
    validate: (String) -> String = { uniffi.lakko_ffi_uniffi.validateUsername(it) },
): UsernameCheck =
    try {
        UsernameCheck.Valid(validate(input))
    } catch (e: Throwable) {
        UsernameCheck.Invalid(usernameProblemOf(e) ?: UsernameProblem.UNUSABLE)
    }

/**
 * The as-you-type hint shown when the name passes but is not typed in its
 * canonical form (uppercase is accepted and shown lowercased): `@alice` for
 * `Alice`. `null` otherwise.
 */
public fun usernameCanonicalHintText(check: UsernameCheck, input: String): UiText? {
    val valid = check as? UsernameCheck.Valid ?: return null
    val typed = input.trim().removePrefix("@")
    return if (typed == valid.normalized) {
        null
    } else {
        UiText("username.hint.shown_as", texts = mapOf("username" to "@${valid.normalized}"))
    }
}

/**
 * The as-you-type line under a username field: `null` while the field is
 * blank (nothing typed is not an error to shout about) or the name passes;
 * otherwise the reason.
 */
public fun usernameFieldErrorText(check: UsernameCheck, input: String): UiText? {
    if (input.isBlank()) return null
    return (check as? UsernameCheck.Invalid)?.problem?.text
}

/**
 * What a search field does with [input]: a lookup for a name that passes, or
 * nothing and the reason for one that fails. A search field NEVER sends a
 * lookup for a failing name (UX §4) — the blinder would refuse it anyway, but
 * the round-trip (and its lookup token) is not spent on it.
 */
public sealed interface SearchDecision {
    /** Send the lookup for [normalized]. */
    public data class Lookup(val normalized: String) : SearchDecision

    /** Do not send anything; show [text]. */
    public data class Refuse(val text: UiText) : SearchDecision
}

/** Decide a search field's action for [input]. */
public fun searchDecision(
    input: String,
    validate: (String) -> String = { uniffi.lakko_ffi_uniffi.validateUsername(it) },
): SearchDecision = when (val check = checkUsername(input, validate)) {
    is UsernameCheck.Valid -> SearchDecision.Lookup(check.normalized)
    is UsernameCheck.Invalid -> SearchDecision.Refuse(check.problem.text)
}

/** Why a profile name fails the ADR-0048 §1 rule; [text] and [copy] as for
 *  [UsernameProblem] (only [text] reaches a screen). */
public enum class DisplayNameProblem(public val copy: String, public val text: UiText) {
    LINE_BREAK("One line only", UiText("display_name.error.line_break")),
    CONTROL_OR_FORMAT("Remove invisible or text-direction characters", UiText("display_name.error.control")),
    TOO_LONG("At most 64 characters", UiText("display_name.error.too_long", counts = mapOf("count" to 64uL))),
    TOO_MANY_BYTES("This name is too long", UiText("display_name.error.too_many_bytes")),

    /** The core refused for a reason it did not classify as a name rule. */
    UNUSABLE("This name can't be used", UiText("display_name.error.unknown")),
}

/** Outcome of checking a typed profile name. */
public sealed interface DisplayNameCheck {
    /** Passes; [normalized] is the trimmed name, or `null` for "no name". */
    public data class Valid(val normalized: String?) : DisplayNameCheck

    /** Fails for [problem]. */
    public data class Invalid(val problem: DisplayNameProblem) : DisplayNameCheck
}

/** The display-name refusal [error] carries, or `null` when it is not one. */
public fun displayNameProblemOf(error: Throwable): DisplayNameProblem? = when (error) {
    is FfiException.DisplayNameLineBreak -> DisplayNameProblem.LINE_BREAK
    is FfiException.DisplayNameControlOrFormatCharacter -> DisplayNameProblem.CONTROL_OR_FORMAT
    is FfiException.DisplayNameTooLong -> DisplayNameProblem.TOO_LONG
    is FfiException.DisplayNameTooManyBytes -> DisplayNameProblem.TOO_MANY_BYTES
    else -> null
}

/**
 * Check [input] against the profile-name rule.
 *
 * @param normalize the core's `normalizeDisplayName` (trimmed name, `null` for
 *   blank, or the typed refusal). Injected for host tests.
 */
public fun checkDisplayName(
    input: String,
    normalize: (String) -> String? = { uniffi.lakko_ffi_uniffi.normalizeDisplayName(it) },
): DisplayNameCheck =
    try {
        DisplayNameCheck.Valid(normalize(input))
    } catch (e: Throwable) {
        DisplayNameCheck.Invalid(displayNameProblemOf(e) ?: DisplayNameProblem.UNUSABLE)
    }
