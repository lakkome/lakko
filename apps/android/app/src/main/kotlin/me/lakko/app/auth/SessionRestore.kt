// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

/**
 * What a cold start found on disk, and where it lands the user.
 *
 * ## The defect this replaces
 *
 * `AuthRepository.rehydrate` used to restore only the `(accountId, username)`
 * marker plus a device-key existence probe, and returned `Session` with NO
 * identity secrets. Every consumer keys off the secrets, not the state — the
 * chat bootstrap never ran and the Contacts panel told the user to log out and
 * back in — so a process restart locked the user out of their own session
 * while the router claimed they were signed in. That "Session without
 * secrets" combination is the half state this decision exists to make
 * unreachable: the only two outcomes are a fully-populated session and a
 * signed-out screen.
 *
 * ## The rule
 *
 * Restore silently ONLY when every one of these is present and readable:
 * the marker, the device key, a well-formed account id, a session pair whose
 * REFRESH credential is unexpired (ADR-0065 section 3.3: the restore refreshes
 * the access token instead of reusing it, so the access token's own expiry
 * does not matter here), and the 64-byte AccountIdentity (phrase) seed.
 * Anything missing, expired, malformed or undecryptable lands signed out —
 * never a guess, never a partial session (fail closed). The OPAQUE export key
 * is optional: no consumer outside `AuthRepository` reads
 * `IdentitySecrets.exportKey`, and the no-password landing paths already
 * install it empty.
 *
 * ## Trade-off, stated plainly
 *
 * A relaunch now re-opens the full session without the password. That widens
 * nothing an attacker could not already reach: the AccountIdentity seed is
 * read WITHOUT a password on login, access recovery, passkey sign-in and by
 * the background push decrypt (ADR-0017 / ADR-0041 sibling on Android), and
 * the token sits under the same Keystore-backed posture. What it does change
 * is that "the app was killed" no longer acts as a soft sign-out — an explicit
 * logout is now the only thing that ends a session on this device, which is
 * why logout deletes the persisted token synchronously.
 */
internal sealed interface SessionRestoreDecision {
    /** No account marker on this device at all. */
    data object NoAccount : SessionRestoreDecision

    /** An account is known but the session cannot be restored silently. */
    data class SignedOut(
        val cachedUsername: String,
        val reason: SignedOutReason,
    ) : SessionRestoreDecision

    /** Everything needed is present: land a full session with these secrets. */
    class Restored(
        val account: SessionAccount,
        val secrets: IdentitySecrets,
    ) : SessionRestoreDecision
}

/** Why a known account was not silently restored. Diagnostic only. */
internal enum class SignedOutReason {
    NO_DEVICE_KEY,
    MALFORMED_ACCOUNT_ID,
    NO_SESSION_TOKEN,

    /** The refresh credential is past its expiry: nothing can renew the
     *  session without a sign-in. */
    SESSION_TOKEN_EXPIRED,
    NO_IDENTITY_SEED,
}

/**
 * Reads the stores the decision needs. Every read may throw (Keystore revoked,
 * ciphertext corrupted, prefs unreadable); [decideSessionRestore] treats a
 * throw exactly like an absent value.
 */
internal interface SessionRestoreSource {
    fun loadMarker(): PersistedSession?

    /** `true` when a device root key is present for [accountIdB64]. */
    fun hasDeviceKey(accountIdB64: String): Boolean

    fun loadSessionPair(accountIdB64: String): SessionPair?

    fun loadAccountIdentitySeed(accountIdB64: String): ByteArray?

    fun loadExportKey(accountIdB64: String): ByteArray?

    /** Drop a persisted token that will not be used (expired / orphaned). */
    fun discardSessionToken(accountIdB64: String)
}

/** Raw 16-byte UUIDv7 account id length. */
private const val RESTORE_ACCOUNT_ID_LEN: Int = 16

/**
 * Decide where a cold start lands. Pure over [source] + [nowMs]; see
 * [SessionRestoreDecision] for the rule and the trade-off.
 */
internal fun decideSessionRestore(
    source: SessionRestoreSource,
    nowMs: Long,
    log: (String, Throwable?) -> Unit,
): SessionRestoreDecision {
    val marker = readOrNull(log, "session marker") { source.loadMarker() }
        ?: return SessionRestoreDecision.NoAccount
    val acct = marker.accountIdB64

    fun signedOut(reason: SignedOutReason): SessionRestoreDecision.SignedOut {
        log("session restore refused: $reason", null)
        return SessionRestoreDecision.SignedOut(cachedUsername = marker.username, reason = reason)
    }

    val hasDeviceKey = readOrNull(log, "device key") { source.hasDeviceKey(acct) } ?: false
    if (!hasDeviceKey) return signedOut(SignedOutReason.NO_DEVICE_KEY)

    val accountIdBytes = try {
        base64Decode(acct)
    } catch (_: IllegalArgumentException) {
        null
    }
    if (accountIdBytes == null || accountIdBytes.size != RESTORE_ACCOUNT_ID_LEN) {
        return signedOut(SignedOutReason.MALFORMED_ACCOUNT_ID)
    }

    val pair = readOrNull(log, "session pair") { source.loadSessionPair(acct) }
    if (pair == null || pair.accessToken.isBlank() || pair.refreshToken.isBlank()) {
        return signedOut(SignedOutReason.NO_SESSION_TOKEN)
    }
    // The access token may well be expired after a night away; the refresh
    // credential is what the restore needs (the provider renews the access
    // token before the first request).
    if (pair.refreshExpiresAtMs <= nowMs) {
        discardQuietly(source, acct, log)
        return signedOut(SignedOutReason.SESSION_TOKEN_EXPIRED)
    }

    val seed = readOrNull(log, "account identity seed") { source.loadAccountIdentitySeed(acct) }
    if (seed == null || seed.size != AccountIdentitySeedStore.SEED_LENGTH) {
        seed?.fill(0)
        // A token with no identity to go with it would only ever restore this
        // same refusal, so drop it: the disk state now matches "signed out".
        discardQuietly(source, acct, log)
        return signedOut(SignedOutReason.NO_IDENTITY_SEED)
    }

    val exportKey = readOrNull(log, "export key") { source.loadExportKey(acct) }
        ?.takeIf { it.size == ExportKeyStore.EXPORT_KEY_LENGTH }
        ?: ByteArray(0)

    return SessionRestoreDecision.Restored(
        account = SessionAccount(accountIdB64 = acct, username = marker.username),
        secrets = IdentitySecrets(
            accountId = accountIdBytes,
            exportKey = exportKey,
            accountIdentitySeed = seed,
            session = pair,
        ),
    )
}

private inline fun <T> readOrNull(
    log: (String, Throwable?) -> Unit,
    what: String,
    read: () -> T?,
): T? =
    try {
        read()
    } catch (cause: Exception) {
        log("session restore: $what read failed; treating as absent", cause)
        null
    }

private fun discardQuietly(
    source: SessionRestoreSource,
    accountIdB64: String,
    log: (String, Throwable?) -> Unit,
) {
    try {
        source.discardSessionToken(accountIdB64)
    } catch (cause: Exception) {
        log("session restore: discarding the unusable session token failed", cause)
    }
}
