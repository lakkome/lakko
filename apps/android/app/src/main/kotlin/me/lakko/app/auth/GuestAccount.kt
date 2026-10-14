// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.coroutines.CancellationException

/*
 * A guest account: the account made for someone who joined a call from a
 * link without having one (`docs/ux/call-links.md` section 9, ADR-0032).
 * Android twin of `apps/web/src/auth/guest-account.ts`.
 *
 * ## What changed (2026-10-02)
 *
 * The first version showed the 24-word recovery phrase before the call. A
 * person who was sent a link wants the call; whether they want an account is
 * a question for afterwards. So now:
 *
 * - the account is made silently and the person goes straight into the call;
 * - while it is a guest account no backup is uploaded and no push
 *   registration is made for it;
 * - after the call the person is asked once: `Create account` or
 *   `No, thanks`;
 * - `No, thanks` deletes the account at once, here and on the servers;
 * - an account that was never given a username is deleted by the servers a
 *   day after it was made, and the device deletes its own guest an hour
 *   before that ([GUEST_LIFETIME_MS]), so closing the app leaves nothing
 *   behind either;
 * - a guest account is never left as a cached, signed-out account: nobody
 *   knows its password, so one whose session cannot be restored is wiped at
 *   launch ([guestRestoreAction]).
 *
 * ## What this file is
 *
 * The two facts a device keeps about a guest account, the decisions the
 * screens and the launch read, and the orchestrations (land, keep, delete,
 * expire, settle at launch) over ports, so the JVM tests run all of it
 * without the Android Keystore or a network.
 *
 * 1. The marker: this account is a guest account, when it was made, and
 *    whether a username was already taken for it. Plain `SharedPreferences`;
 *    it holds no secret.
 * 2. The phrase row: the recovery phrase, in the Keystore-backed
 *    [me.lakko.app.backup.PhraseStore], under a key of its own
 *    ([guestPhraseKey]) and not the account's. That choice is what keeps the
 *    automatic backup from running for a guest without it knowing what a
 *    guest is: it reads the phrase by the account's key and finds nothing.
 */

/** The preferences file of the guest marker. */
public const val GUEST_ACCOUNT_PREFS: String = "lakko-guest-account"

/**
 * How long a guest account lives on a device before the device deletes it
 * unasked.
 *
 * The servers delete an unkept account a day after it was made. The device
 * stops an hour earlier, so `Create account` is never offered for an account
 * the servers are about to remove under it. Chosen, not measured.
 */
public const val GUEST_LIFETIME_MS: Long = 23L * 60L * 60L * 1000L

/** What the device knows about a guest account. */
public data class GuestAccountState(
    /** When this device marked the account as a guest (its creation, to the
     *  second or so), unix milliseconds. */
    val markedAtMs: Long,
    /**
     * The username already taken for this account by an earlier
     * `Create account` that did not finish, or `null`.
     *
     * A username cannot be changed and an account holds one, so a second
     * attempt must go on with this one instead of asking again.
     */
    val claimedUsername: String?,
)

/** The guest account of the live session, as the screens observe it. */
public data class GuestSession(
    val accountIdB64: String,
    /** See [GuestAccountState.markedAtMs]. */
    val markedAtMs: Long,
    val claimedUsername: String?,
) {
    /** The marker's view of this session. */
    val account: GuestAccountState
        get() = GuestAccountState(markedAtMs = markedAtMs, claimedUsername = claimedUsername)
}

/** The marker's key for [accountIdB64] in [GUEST_ACCOUNT_PREFS]. */
public fun guestMarkerKey(accountIdB64: String): String = "account:$accountIdB64"

// `guest@<ms>` or `named@<ms>:<username>`. A username is `[a-z0-9_]`, so the
// first `:` after the stamp ends it.
private const val MARK_GUEST_PREFIX = "guest@"
private const val MARK_NAMED_PREFIX = "named@"
private const val MAX_STAMP_DIGITS = 16

private fun parseStamp(text: String): Long? {
    if (text.isEmpty() || text.length > MAX_STAMP_DIGITS) return null
    if (!text.all { it in '0'..'9' }) return null
    return text.toLongOrNull()
}

/**
 * The [me.lakko.app.backup.PhraseStore] key a guest's recovery phrase is kept
 * under. Not the account id: see the file comment.
 */
public fun guestPhraseKey(accountIdB64: String): String = "guest:$accountIdB64"

/** Record that [accountIdB64] is a guest account, made at [nowMs]. Never
 *  throws. */
public fun markGuestAccount(storage: MarkerStorage?, accountIdB64: String, nowMs: Long) {
    try {
        storage?.set(guestMarkerKey(accountIdB64), MARK_GUEST_PREFIX + nowMs)
    } catch (_: Exception) {
        // Storage refused: the account then reads as an ordinary one with no
        // username, which is what it is. It is still deleted after a day.
    }
}

/**
 * Record the username taken for a guest account that is being kept. The
 * account keeps the age it had: taking a name does not buy it another day.
 * No-op for an account that is not marked as a guest. Never throws.
 */
public fun noteGuestUsernameClaimed(storage: MarkerStorage?, accountIdB64: String, username: String) {
    val current = guestAccountState(storage, accountIdB64) ?: return
    try {
        storage?.set(
            guestMarkerKey(accountIdB64),
            "$MARK_NAMED_PREFIX${current.markedAtMs}:$username",
        )
    } catch (_: Exception) {
        // Storage refused: a retry asks for the name again and the server
        // answers that the account already has one.
    }
}

/** What the device knows about [accountIdB64] as a guest, or `null` when it
 *  is not a guest account. Never throws. */
public fun guestAccountState(storage: MarkerStorage?, accountIdB64: String): GuestAccountState? {
    val raw = try {
        storage?.get(guestMarkerKey(accountIdB64))
    } catch (_: Exception) {
        null
    } ?: return null
    if (raw.startsWith(MARK_GUEST_PREFIX)) {
        val markedAtMs = parseStamp(raw.removePrefix(MARK_GUEST_PREFIX)) ?: return null
        return GuestAccountState(markedAtMs = markedAtMs, claimedUsername = null)
    }
    if (raw.startsWith(MARK_NAMED_PREFIX)) {
        val rest = raw.removePrefix(MARK_NAMED_PREFIX)
        val colon = rest.indexOf(':')
        if (colon < 0) return null
        val markedAtMs = parseStamp(rest.substring(0, colon)) ?: return null
        val name = rest.substring(colon + 1)
        return GuestAccountState(markedAtMs = markedAtMs, claimedUsername = name.ifEmpty { null })
    }
    // An unknown value is not a guest: never offer to delete an account on the
    // strength of bytes this build did not write.
    return null
}

/** Forget that [accountIdB64] is a guest: it became an account, or it is
 *  being removed from this device. Never throws. */
public fun clearGuestAccount(storage: MarkerStorage?, accountIdB64: String) {
    try {
        storage?.remove(guestMarkerKey(accountIdB64))
    } catch (_: Exception) {
        // Storage refused: nothing was recorded either.
    }
}

/**
 * The guest account of the session [state] names, or `null`.
 *
 * [guest] is published separately from the auth state, so it is matched to
 * the session's account here: a value left from another account, or one
 * observed in the instant between the two writes, is never a guest.
 */
public fun guestSessionOf(state: AuthState, guest: GuestSession?): GuestSession? {
    if (state !is AuthState.Session || guest == null) return null
    return guest.takeIf { it.accountIdB64 == state.account.accountIdB64 }
}

/** What a screen shows a guest. */
public enum class GuestScreen {
    /** Not a guest, or there is nothing to say: the screen's own content. */
    NONE,

    /** A call is running. The guest is in it; the only thing to offer is the
     *  way back to it. Deleting the account under a live call is never one
     *  press away. */
    IN_CALL,

    /** No call is running: `Create account` or `No, thanks`. */
    OFFER,
}

/**
 * What a signed-in screen shows in place of the app.
 *
 * A guest account has no conversations and no contacts, so the app behind it
 * is an empty shell; a guest never lands in it. While a call runs the person
 * is sent back to the call, and once there is none they are asked the one
 * question.
 */
public fun guestScreen(guest: Boolean, callInProgress: Boolean): GuestScreen = when {
    !guest -> GuestScreen.NONE
    callInProgress -> GuestScreen.IN_CALL
    else -> GuestScreen.OFFER
}

/** What the app root draws for a live session. */
public enum class SessionSurface {
    /** A signup's recovery phrase, not yet acknowledged. */
    SAVE_SIGNUP_PHRASE,

    /** A guest account: the way back to its call, or the question whether to
     *  keep the account. Never the app itself. */
    GUEST,

    /** The app. */
    APP,
}

/**
 * The surface of a live session (the session half of the web
 * `sessionGuardView`). A pending signup phrase comes first: that is the guest
 * who has just chosen to keep the account, and whose marker is already gone.
 */
public fun sessionSurface(signupPhrasePending: Boolean, guest: Boolean): SessionSurface = when {
    signupPhrasePending -> SessionSurface.SAVE_SIGNUP_PHRASE
    guest -> SessionSurface.GUEST
    else -> SessionSurface.APP
}

/**
 * Whether a guest account has outlived [GUEST_LIFETIME_MS] on this device.
 *
 * A clock that reads earlier than the mark (it was set back) says nothing
 * about the account's age, so it is not expired on that evidence.
 */
public fun guestExpired(guest: GuestAccountState, nowMs: Long): Boolean =
    nowMs - guest.markedAtMs >= GUEST_LIFETIME_MS

/** What a launch does about a guest account this device holds. */
public enum class GuestRestoreAction {
    /** Not a guest, or a guest young enough to go on: the ordinary restore. */
    NONE,

    /** A guest past its lifetime whose session can still be restored: delete
     *  it on the servers (best effort), then wipe the device. */
    DISCARD,

    /** A guest whose session cannot be restored (the token expired or is
     *  missing, a store is unreadable): nothing can sign in to it from here
     *  and nothing can show its phrase, so every trace of it leaves the
     *  device. The servers remove their side on their own clock. */
    WIPE,
}

/**
 * Decide it. [restorable] is the ordinary restore's own verdict: every piece
 * a silent restore needs is present.
 *
 * A guest account never ends up as a cached, signed-out account: there is no
 * password anyone could type to get back into it.
 */
public fun guestRestoreAction(guest: GuestAccountState?, restorable: Boolean, nowMs: Long): GuestRestoreAction =
    when {
        guest == null -> GuestRestoreAction.NONE
        !restorable -> GuestRestoreAction.WIPE
        guestExpired(guest, nowMs) -> GuestRestoreAction.DISCARD
        else -> GuestRestoreAction.NONE
    }

/** A service a guest account is deleted on. */
public enum class GuestDeleteService { LOGIN_HANDLE, AUTH, DIRECTORY }

/** One step of [guestDeletePlan]. */
public data class GuestDeleteStep(
    val service: GuestDeleteService,
    /** A failure of this step stops a strict delete. */
    val required: Boolean,
)

/**
 * The order `No, thanks` deletes in, and which steps may fail quietly.
 *
 * An unnamed guest: the auth service decides (once the account is gone there,
 * nothing can sign in to it), and the directory is best effort, since its own
 * sweep removes an entry with no username.
 *
 * A guest for whom a username was already taken: the login handle bound for
 * that name is released FIRST. The auth service deletes an account on its
 * session alone only while it has no login handle and no passkey (a guest),
 * and releasing the handle of an account younger than a day needs nothing
 * more than the session either, so `No, thanks` never depends on a
 * fresh-auth proof the guest has no password to mint. Then the directory,
 * which must succeed: its sweep never removes an entry that has a username,
 * so a directory delete skipped here would leave that name taken for good.
 * Every step is idempotent, so a retry after a half-finished attempt
 * completes.
 */
public fun guestDeletePlan(guest: GuestAccountState): List<GuestDeleteStep> =
    if (guest.claimedUsername == null) {
        listOf(
            GuestDeleteStep(GuestDeleteService.AUTH, required = true),
            GuestDeleteStep(GuestDeleteService.DIRECTORY, required = false),
        )
    } else {
        listOf(
            GuestDeleteStep(GuestDeleteService.LOGIN_HANDLE, required = true),
            GuestDeleteStep(GuestDeleteService.DIRECTORY, required = true),
            GuestDeleteStep(GuestDeleteService.AUTH, required = true),
        )
    }

/** What became of a guest account while a screen was watching it. */
public enum class GuestOutcome {
    /** Nothing to report. */
    NONE,

    /** `No, thanks`: the account is gone and the device is signed out. */
    DELETED,

    /** `Create account`, the phrase acknowledged, no call running: the person
     *  goes into the app, where an account belongs. */
    KEPT,
}

/** [GuestOutcome] plus the watcher's memory for the next observation. */
public data class GuestWatch(
    /** A guest session was seen and its end has not been reported yet. */
    val wasGuest: Boolean,
    val outcome: GuestOutcome,
)

/**
 * One observation of the auth state by a screen that may have shown a guest
 * (the call-link landing, the app root). Mirror of the effect in the web
 * `JoinCall.tsx`.
 *
 * While the session is a guest's the watcher only remembers it. Once it is
 * not: signed out means the account was deleted; a session with no phrase
 * screen pending and no call running means it was kept and acknowledged. A
 * kept account whose phrase is still on screen, or whose call still runs,
 * stays remembered until both are over.
 */
public fun observeGuestOutcome(
    wasGuest: Boolean,
    guest: Boolean,
    state: AuthState,
    signupPhrasePending: Boolean,
    callInProgress: Boolean,
): GuestWatch = when {
    guest -> GuestWatch(wasGuest = true, outcome = GuestOutcome.NONE)
    !wasGuest -> GuestWatch(wasGuest = false, outcome = GuestOutcome.NONE)
    state is AuthState.Anon -> GuestWatch(wasGuest = false, outcome = GuestOutcome.DELETED)
    state is AuthState.Session && !signupPhrasePending && !callInProgress ->
        GuestWatch(wasGuest = false, outcome = GuestOutcome.KEPT)
    else -> GuestWatch(wasGuest = true, outcome = GuestOutcome.NONE)
}

// =============================================================================
// Landing
// =============================================================================

/** What [landGuestSignup] does with the account a guest signup just made. */
internal interface GuestLandingPorts {
    /** Keep the recovery phrase on this device under [guestPhraseKey].
     *  Throws when it cannot be stored. */
    fun storeGuestPhrase()

    /** Record the guest marker and publish it to the screens. */
    fun markGuest()

    /** Land the session, with the real export key. */
    fun landSession()
}

/**
 * The tail of a guest signup, after the identity bundle was published.
 *
 * The phrase first, fail loud: an account whose phrase is neither shown nor
 * kept could never be kept afterwards, so a guest signup that cannot store it
 * fails before anything marks or lands a session. The marker is published
 * before the session, so no screen observes the session without it. Nothing
 * here shows a phrase and nothing here registers for push.
 */
internal fun landGuestSignup(ports: GuestLandingPorts) {
    ports.storeGuestPhrase()
    ports.markGuest()
    ports.landSession()
}

// =============================================================================
// Keep
// =============================================================================

/**
 * A guest account cannot be kept because this device no longer holds its
 * recovery phrase. The screen then offers only `No, thanks`.
 */
public class GuestPhraseUnavailable :
    IllegalStateException("the guest account's recovery phrase is not on this device")

/**
 * `Create account` was pressed for a guest account past its lifetime. The
 * account has been deleted and the session is over by the time this is
 * thrown: the screen that asked is going away, and says nothing.
 */
public class GuestAccountExpired : IllegalStateException("the guest account has expired")

/** What [keepGuestAccount] drives. */
internal interface GuestClaimPorts {
    /** Delete the account if it is past its lifetime ([expireGuestWhenDue]);
     *  `true` when it was. */
    suspend fun expireIfDue(): Boolean

    /** The guest's recovery phrase, or `null` when it cannot be read: the
     *  guest's own row, else the account's row (an earlier attempt whose
     *  password reset went through cached it there before it was
     *  interrupted). */
    suspend fun readGuestPhrase(): String?

    /** The username an earlier attempt already took, or `null`. */
    fun claimedUsername(): String?

    /** Bind the login handle at auth, then register the name in the
     *  directory, releasing the handle if the directory refuses.
     *  @return the normalised username. */
    suspend fun claimUsername(username: String): String

    /** Record the taken username in the marker and the persisted account. */
    fun noteUsernameClaimed(username: String)

    /** Set the account's first password (the device's seed-based password
     *  change, ADR-0076). */
    suspend fun setPassword(phrase: String, password: ByteArray, username: String)

    /** Make it an ordinary account: the phrase under the account's own row,
     *  the "Protect your account" step owed (ADR-0075), the first backup,
     *  the push registration. No phrase is shown. */
    suspend fun landAsAccount(phrase: String)

    /** Clear the guest marker. */
    fun clearGuest()

    /** Delete the guest phrase row. A failure is the port's to log. */
    fun deleteGuestPhrase()
}

/**
 * Keep a guest account: give it [username] and [password].
 *
 * Order (the web `claimGuestAccount`):
 *
 *   0. an account past its lifetime is not kept: it is deleted and
 *      [GuestAccountExpired] is thrown;
 *   1. the phrase, or [GuestPhraseUnavailable]: an account whose phrase this
 *      device does not hold cannot be backed up or protected later, so
 *      nothing starts without it;
 *   2. the username, once: an account holds one and it cannot be changed, so
 *      a retry after a failed password step goes on with the name already
 *      taken and never asks the directory again;
 *   3. the first password, by the device's seed-based password change;
 *   4. only then the account stops being a guest: it lands as an ordinary
 *      account on the "Protect your account" step (ADR-0075; no phrase is
 *      shown and no words are checked), the marker goes, the guest phrase
 *      row goes.
 *
 * The device's published KeyPackage is never deleted on this path: the
 * password change keeps the device identity, and a deleted directory row is
 * tombstoned for 30 days (ADR-0074), which would keep the kept account's own
 * device out of every chat. Push registration is
 * the last step of `recover` itself here, so it also happens only once the
 * password is set.
 */
internal suspend fun keepGuestAccount(ports: GuestClaimPorts, username: String, password: ByteArray) {
    if (ports.expireIfDue()) throw GuestAccountExpired()
    val phrase = ports.readGuestPhrase() ?: throw GuestPhraseUnavailable()
    var name = ports.claimedUsername()
    if (name == null) {
        val trimmed = username.trim()
        if (trimmed.isEmpty()) throw AuthClientArgError("username must be non-empty")
        name = ports.claimUsername(trimmed)
        ports.noteUsernameClaimed(name)
    }
    ports.setPassword(phrase, password, name)
    // The protect step is armed before the marker goes, so no screen sees an
    // account that is neither a guest nor owed that step.
    ports.landAsAccount(phrase)
    ports.clearGuest()
    ports.deleteGuestPhrase()
}

// =============================================================================
// Delete
// =============================================================================

/**
 * The bound on one step of a best-effort (non-strict) delete. An expired
 * guest is deleted at launch or when the offer appears, where nobody pressed
 * anything and nothing should wait on a network that does not answer.
 * Chosen, not measured.
 */
public const val GUEST_BEST_EFFORT_DELETE_TIMEOUT_MS: Long = 15_000L

/** The server-side deletes of a guest account. */
internal interface GuestServerPorts {
    /** `DELETE /auth/login-handle`: release the handle a half-finished
     *  `Create account` bound. Idempotent. Throws on any failure. */
    suspend fun releaseLoginHandle()

    /** `DELETE /account` on the auth service. Throws on any failure. */
    suspend fun deleteAtAuth()

    /** The directory's delete-account. Throws on any failure. */
    suspend fun deleteAtDirectory()

    fun log(message: String, cause: Throwable?)
}

/**
 * Delete a guest account on the servers, in the order and with the
 * strictness [guestDeletePlan] gives.
 *
 * Strict (`No, thanks`): a required step that fails throws, and the caller
 * removes nothing local. Not strict (an expired guest): every failure is
 * logged and swallowed, and no step waits longer than
 * [GUEST_BEST_EFFORT_DELETE_TIMEOUT_MS]; both deletes are idempotent and the
 * servers sweep an unkept account themselves.
 */
internal suspend fun deleteGuestOnServers(ports: GuestServerPorts, guest: GuestAccountState, strict: Boolean) {
    for (step in guestDeletePlan(guest)) {
        val run: suspend () -> Unit = when (step.service) {
            GuestDeleteService.LOGIN_HANDLE -> { -> ports.releaseLoginHandle() }
            GuestDeleteService.AUTH -> { -> ports.deleteAtAuth() }
            GuestDeleteService.DIRECTORY -> { -> ports.deleteAtDirectory() }
        }
        val name = step.service.name.lowercase()
        try {
            if (strict) {
                run()
            } else if (kotlinx.coroutines.withTimeoutOrNull(GUEST_BEST_EFFORT_DELETE_TIMEOUT_MS) { run() } == null) {
                ports.log("guest account: $name delete did not answer in time", null)
            }
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (cause: Exception) {
            if (strict && step.required) throw cause
            ports.log("guest account: $name rows not deleted", cause)
        }
    }
}

/** What [deleteGuestAccount] and [expireGuestWhenDue] drive. */
internal interface GuestDiscardPorts : GuestServerPorts {
    /** Everything on this device (the guest marker and the phrase row are in
     *  the wipe list); lands signed out. */
    fun forgetDevice()
}

/**
 * `No, thanks`: delete a guest account at once.
 *
 * The servers first, strictly ([deleteGuestOnServers]): a required delete
 * that fails throws and leaves everything in place so the person can try
 * again; the order frees a username an unfinished `Create account` took.
 * Then everything on this device.
 */
internal suspend fun deleteGuestAccount(ports: GuestDiscardPorts, guest: GuestAccountState) {
    deleteGuestOnServers(ports, guest, strict = true)
    ports.forgetDevice()
}

/**
 * Delete the live session's guest account if it is past its lifetime:
 * best effort on the servers, then everything on this device. `true` when it
 * was deleted (the session is over). `false`, and nothing done, for a session
 * that is not a guest's ([guest] `null`) or a guest still young enough.
 */
internal suspend fun expireGuestWhenDue(ports: GuestDiscardPorts, guest: GuestAccountState?, nowMs: Long): Boolean {
    if (guest == null || !guestExpired(guest, nowMs)) return false
    deleteGuestOnServers(ports, guest, strict = false)
    ports.forgetDevice()
    return true
}

// =============================================================================
// Launch
// =============================================================================

/** What [settleGuestAtRestore] drives. */
internal interface GuestRestorePorts : GuestServerPorts {
    /** Remove every trace of the account from this device: the wipe Forget
     *  device uses, the cached-account record included. */
    fun wipeDevice()

    /** Land signed out, with no cached username. */
    fun landSignedOut()
}

/**
 * The guest half of a launch, run before any session state lands
 * ([guestRestoreAction]).
 *
 * `false`: nothing was done, the ordinary restore goes on. `true`: the guest
 * account is gone from this device and the launch has landed signed out; the
 * ordinary restore must not run. An expired guest whose session is still
 * usable is deleted on the servers first, best effort; one whose session
 * cannot be restored has no token to delete with and is only wiped.
 */
internal suspend fun settleGuestAtRestore(
    ports: GuestRestorePorts,
    guest: GuestAccountState?,
    restorable: Boolean,
    nowMs: Long,
): Boolean {
    when (guestRestoreAction(guest, restorable, nowMs)) {
        GuestRestoreAction.NONE -> return false
        GuestRestoreAction.DISCARD -> if (guest != null) deleteGuestOnServers(ports, guest, strict = false)
        GuestRestoreAction.WIPE -> Unit
    }
    ports.wipeDevice()
    ports.landSignedOut()
    return true
}
