// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.webauthn

/**
 * A blob is stored for the passkey but it did not open under this ceremony's
 * PRF output (a tampered blob, or an output that is not this credential's).
 * Nothing was changed.
 */
public class PasskeyVaultUnusable(cause: Throwable? = null) :
    Exception("the keys stored for this passkey did not open", cause)

/** The published identity does not match the phrase a passkey gave. */
public class PasskeyPhraseNotThisAccount : Exception("the keys stored for this passkey are not this account's")

/**
 * Ask a passkey for the account's keys, on a device that is signed in and
 * holds none (web: `AuthContext.keysFromPasskey`).
 *
 *   - no PRF output from the ceremony: the passkey signs in only;
 *   - [fetchBlob] answers `null`: nothing is stored for it, same outcome;
 *   - [open] throws [PasskeyVaultUnusable]: a blob exists and does not open;
 *   - else the phrase goes to [providePhrase], the ordinary phrase path
 *     whose check against the published identity is fail-closed; it throws
 *     [PasskeyPhraseNotThisAccount] on a mismatch.
 *
 * [PasskeyKeysOutcome.Restored] means [providePhrase] landed the session.
 * Anything else these ports throw (the vault or the directory could not be
 * reached) propagates: it is neither "no keys" nor "unusable".
 */
public suspend fun keysFromPasskey(
    prfOutput: ByteArray?,
    fetchBlob: suspend () -> ByteArray?,
    open: (prfOutput: ByteArray, sealed: ByteArray) -> String,
    providePhrase: suspend (phrase: String) -> Unit,
): PasskeyKeysOutcome {
    var blobStored = false
    var phrase: String? = null
    if (prfOutput != null) {
        val sealed = fetchBlob()
        if (sealed != null) {
            blobStored = true
            phrase = try {
                open(prfOutput, sealed)
            } catch (_: PasskeyVaultUnusable) {
                null
            }
        }
    }
    var matched = false
    if (phrase != null) {
        try {
            providePhrase(phrase)
            matched = true
        } catch (_: PasskeyPhraseNotThisAccount) {
            matched = false
        }
    }
    return passkeyKeysOutcome(
        prfOutput = prfOutput != null,
        blobStored = blobStored,
        opened = phrase != null,
        matched = matched,
    )
}

/** How storing the keys for one passkey ended, or what it still needs
 *  ([turnOnPasskeyKeys]). */
public enum class PasskeyKeysChoiceResult {
    /** The keys are stored for the passkey. */
    Stored,

    /** The passkey gave no secret to protect keys with. */
    NoPrf,

    /** The server does not store keys for passkeys. */
    Unavailable,

    /** This device holds no recovery phrase: the person types it. */
    PhraseNeeded,

    /** The typed phrase is not a valid recovery phrase. */
    PhraseInvalid,

    /** The typed phrase is not this account's. Nothing was changed. */
    PhraseMismatch,

    /** A different passkey answered the prompt. */
    OtherPasskey,
}

/**
 * Store the account's keys for one passkey (UX 10.2): the phrase (stored on
 * this device, else typed and required to give this device's identity
 * seed), the passkey's secret (from the registration, else one more prompt
 * limited to that credential), then seal and store. Every refusal leaves the
 * passkey "Logs in only". Used right after a passkey is created and by a
 * row's "Restore keys with this passkey".
 *
 * The ports keep the rule testable without the core or a server:
 * [seedOf] is `RecoveryMnemonic.fromPhrase(p).toSeed()` (throws on a phrase
 * that does not parse), [evaluatePrf] is the limited ceremony (the id of
 * the passkey that answered and its PRF output), [sealAndStore] is
 * `passkeyVaultSeal` + `PUT /vault/passkey` (`false` on `503`).
 */
public suspend fun turnOnPasskeyKeys(
    credentialIdB64: String,
    storedPhrase: String?,
    typedPhrase: String?,
    identitySeed: ByteArray,
    registrationPrfOutput: ByteArray?,
    seedOf: (phrase: String) -> ByteArray,
    evaluatePrf: suspend () -> PasskeyCeremonyExtras,
    sealAndStore: suspend (phrase: String, prfOutput: ByteArray) -> Boolean,
): PasskeyKeysChoiceResult {
    val phrase: String = storedPhrase ?: run {
        val typed = typedPhrase?.trim().orEmpty()
        if (typed.isEmpty()) return PasskeyKeysChoiceResult.PhraseNeeded
        val seed = try {
            seedOf(typed)
        } catch (_: Throwable) {
            return PasskeyKeysChoiceResult.PhraseInvalid
        }
        val same = constantTimeEquals(seed, identitySeed)
        seed.fill(0)
        if (!same) return PasskeyKeysChoiceResult.PhraseMismatch
        typed
    }
    var prfOutput = registrationPrfOutput
    if (prfOutput == null) {
        val extras = evaluatePrf()
        if (extras.credentialIdB64 != credentialIdB64) return PasskeyKeysChoiceResult.OtherPasskey
        prfOutput = extras.prfOutput
    }
    if (prfOutput == null) return PasskeyKeysChoiceResult.NoPrf
    return if (sealAndStore(phrase, prfOutput)) {
        PasskeyKeysChoiceResult.Stored
    } else {
        PasskeyKeysChoiceResult.Unavailable
    }
}

private fun constantTimeEquals(a: ByteArray, b: ByteArray): Boolean {
    if (a.size != b.size) return false
    var diff = 0
    for (i in a.indices) diff = diff or (a[i].toInt() xor b[i].toInt())
    return diff == 0
}

// ---------------------------------------------------------------------------
// Every passkey restores keys (UX 10.2, amended 2026-11-11): no choice
// ---------------------------------------------------------------------------

/** How storing the keys for a passkey ended, once the ports' errors are
 *  read: the add flow and the row's repair both end in one of these. */
public enum class PasskeyKeysStoreEnd {
    /** The keys are stored: the passkey logs in and restores keys. */
    Stored,

    /** The passkey gave no secret to protect keys with, on this device and
     *  passkey provider: it logs in only. */
    NoPrf,

    /** This device holds no recovery phrase: the person types it. */
    PhraseNeeded,

    /** The typed phrase is not a valid recovery phrase. */
    PhraseInvalid,

    /** The typed phrase is not this account's. Nothing was changed. */
    PhraseMismatch,

    /** A different passkey answered the second prompt. */
    OtherPasskey,

    /** The second prompt was dismissed. */
    Cancelled,

    /** The server does not store keys for passkeys (`503`). */
    Unavailable,

    /** The vault or the ceremony could not be reached. */
    Network,

    /** Anything else. */
    Failed,
}

/** [result] as a [PasskeyKeysStoreEnd]. */
public fun passkeyKeysStoreEnd(result: PasskeyKeysChoiceResult): PasskeyKeysStoreEnd = when (result) {
    PasskeyKeysChoiceResult.Stored -> PasskeyKeysStoreEnd.Stored
    PasskeyKeysChoiceResult.NoPrf -> PasskeyKeysStoreEnd.NoPrf
    PasskeyKeysChoiceResult.Unavailable -> PasskeyKeysStoreEnd.Unavailable
    PasskeyKeysChoiceResult.PhraseNeeded -> PasskeyKeysStoreEnd.PhraseNeeded
    PasskeyKeysChoiceResult.PhraseInvalid -> PasskeyKeysStoreEnd.PhraseInvalid
    PasskeyKeysChoiceResult.PhraseMismatch -> PasskeyKeysStoreEnd.PhraseMismatch
    PasskeyKeysChoiceResult.OtherPasskey -> PasskeyKeysStoreEnd.OtherPasskey
}

/**
 * Run [store] ([turnOnPasskeyKeys] with its ports bound) and read what it
 * threw: a dismissed sheet, a `503`, a network failure, anything else.
 * Cancellation of the calling coroutine propagates. [onFailure] is told
 * what ended in [PasskeyKeysStoreEnd.Failed], for the log.
 */
public suspend fun runPasskeyKeysStore(
    onFailure: (Throwable) -> Unit = {},
    store: suspend () -> PasskeyKeysChoiceResult,
): PasskeyKeysStoreEnd = try {
    passkeyKeysStoreEnd(store())
} catch (cause: kotlinx.coroutines.CancellationException) {
    throw cause
} catch (_: WebauthnUserCancelledError) {
    PasskeyKeysStoreEnd.Cancelled
} catch (err: me.lakko.app.auth.AuthHttpError) {
    if (err.status == HTTP_UNAVAILABLE) {
        PasskeyKeysStoreEnd.Unavailable
    } else {
        onFailure(err)
        PasskeyKeysStoreEnd.Failed
    }
} catch (_: me.lakko.app.auth.AuthNetworkError) {
    PasskeyKeysStoreEnd.Network
} catch (_: java.io.IOException) {
    PasskeyKeysStoreEnd.Network
} catch (cause: Throwable) {
    onFailure(cause)
    PasskeyKeysStoreEnd.Failed
}

private const val HTTP_UNAVAILABLE = 503

/**
 * Store the keys for a passkey just created, with nothing to choose (UX
 * 10.2 step 2): from the creation's PRF output when it gave one, else with
 * one more prompt for that passkey. Only a provider that said the new
 * credential cannot evaluate PRF ([prfEnabled] `false`, no output) skips the
 * second prompt: it is then [PasskeyKeysStoreEnd.NoPrf] at once. A provider
 * that said nothing gets the second prompt, since some return the output
 * only when a passkey is used. [store] is handed the creation's output.
 */
public suspend fun storeKeysForAddedPasskey(
    prfOutput: ByteArray?,
    prfEnabled: Boolean?,
    onFailure: (Throwable) -> Unit = {},
    store: suspend (registrationPrfOutput: ByteArray?) -> PasskeyKeysChoiceResult,
): PasskeyKeysStoreEnd {
    if (prfOutput == null && prfEnabled == false) return PasskeyKeysStoreEnd.NoPrf
    return runPasskeyKeysStore(onFailure) { store(prfOutput) }
}

/** Whether a passkey row offers "Restore keys with this passkey": it holds
 *  no keys and this device does not know it cannot carry them. */
public fun passkeyOffersRestoreKeys(
    credentialIdB64: String,
    withKeys: List<String>?,
    knownNoPrf: Set<String>,
): Boolean = passkeyKeysLabel(credentialIdB64, withKeys) == PasskeyKeysLabel.SignInOnly &&
    credentialIdB64 !in knownNoPrf

/** The line after a passkey was added and its keys were (or were not)
 *  stored (UX 10.2). [name] is the passkey's name. */
public fun addedPasskeyText(end: PasskeyKeysStoreEnd, name: String): me.lakko.app.l10n.UiText = when (end) {
    PasskeyKeysStoreEnd.Stored -> me.lakko.app.l10n.UiText("settings.passkeys.added_restores")
    PasskeyKeysStoreEnd.NoPrf -> me.lakko.app.l10n.UiText("settings.passkeys.added_no_keys")
    // Added as "Logs in only"; its row asks for the phrase (step 4).
    PasskeyKeysStoreEnd.PhraseNeeded,
    PasskeyKeysStoreEnd.PhraseInvalid,
    PasskeyKeysStoreEnd.PhraseMismatch,
    -> me.lakko.app.l10n.UiText("settings.passkeys.added", texts = mapOf("name" to name))
    PasskeyKeysStoreEnd.Cancelled -> me.lakko.app.l10n.UiText("settings.passkeys.keys_choice.cancelled")
    PasskeyKeysStoreEnd.OtherPasskey -> me.lakko.app.l10n.UiText("settings.passkeys.keys_choice.error.other_passkey")
    PasskeyKeysStoreEnd.Unavailable -> me.lakko.app.l10n.UiText("settings.passkeys.keys_choice.unavailable")
    // The passkey exists; only its keys are owed (repair from its row).
    PasskeyKeysStoreEnd.Network,
    PasskeyKeysStoreEnd.Failed,
    -> me.lakko.app.l10n.UiText("settings.passkeys.added_keys_failed")
}

/** Whether [addedPasskeyText] is an error line: the passkey exists in every
 *  case, so the lines that say it logs in only for now (cancelled, offline,
 *  another failure) are not errors; a different passkey answering and a
 *  server that keeps no keys for passkeys are. */
public fun addedPasskeyIsError(end: PasskeyKeysStoreEnd): Boolean = when (end) {
    PasskeyKeysStoreEnd.Stored,
    PasskeyKeysStoreEnd.NoPrf,
    PasskeyKeysStoreEnd.PhraseNeeded,
    PasskeyKeysStoreEnd.PhraseInvalid,
    PasskeyKeysStoreEnd.PhraseMismatch,
    PasskeyKeysStoreEnd.Cancelled,
    -> false
    PasskeyKeysStoreEnd.OtherPasskey,
    PasskeyKeysStoreEnd.Unavailable,
    PasskeyKeysStoreEnd.Network,
    PasskeyKeysStoreEnd.Failed,
    -> true
}

/** The line after a row's "Restore keys with this passkey", or `null` when
 *  nothing is said: the phrase input opens instead, or the sheet was
 *  dismissed (nothing changed). */
public fun restoreKeysText(end: PasskeyKeysStoreEnd): me.lakko.app.l10n.UiText? = when (end) {
    PasskeyKeysStoreEnd.Stored -> me.lakko.app.l10n.UiText("settings.passkeys.keys_choice.done")
    PasskeyKeysStoreEnd.NoPrf -> me.lakko.app.l10n.UiText("settings.passkeys.keys_choice.no_prf")
    PasskeyKeysStoreEnd.PhraseNeeded, PasskeyKeysStoreEnd.Cancelled -> null
    PasskeyKeysStoreEnd.PhraseInvalid -> me.lakko.app.l10n.UiText("restore_identity.error.invalid_phrase")
    PasskeyKeysStoreEnd.PhraseMismatch ->
        me.lakko.app.l10n.UiText("settings.passkeys.keys_choice.error.phrase_mismatch")
    PasskeyKeysStoreEnd.OtherPasskey -> me.lakko.app.l10n.UiText("settings.passkeys.keys.error.other_passkey")
    PasskeyKeysStoreEnd.Unavailable -> me.lakko.app.l10n.UiText("settings.passkeys.keys_choice.unavailable")
    PasskeyKeysStoreEnd.Network -> me.lakko.app.l10n.UiText("settings.passkeys.error.network")
    PasskeyKeysStoreEnd.Failed -> me.lakko.app.l10n.UiText("settings.passkeys.error.generic")
}
