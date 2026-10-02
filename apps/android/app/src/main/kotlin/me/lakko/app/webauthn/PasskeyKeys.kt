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

/** How "Let this passkey restore your keys?" ended, or what it still needs. */
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
 * "Turn on" for one passkey: the phrase (stored on this device, else typed
 * and required to give this device's identity seed), the passkey's secret
 * (from the registration, else one more prompt limited to that credential),
 * then seal and store. Every refusal leaves the passkey "Signs in only".
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
