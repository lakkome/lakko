// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context
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
 *   day after it was made, so closing the app leaves nothing behind either.
 *
 * ## What this file is
 *
 * The two facts a device keeps about a guest account, the one decision the
 * screens read, and the three orchestrations (land, keep, delete) over ports,
 * so the JVM tests run all of it without the Android Keystore or a network.
 *
 * 1. The marker: this account is a guest account, and whether a username was
 *    already taken for it. Plain `SharedPreferences`; it holds no secret.
 * 2. The phrase row: the recovery phrase, in the Keystore-backed
 *    [me.lakko.app.backup.PhraseStore], under a key of its own
 *    ([guestPhraseKey]) and not the account's. That choice is what keeps the
 *    automatic backup from running for a guest without it knowing what a
 *    guest is: it reads the phrase by the account's key and finds nothing.
 */

/** The subset of a key-value store a plain per-account marker needs. */
public interface MarkerStorage {
    public fun getItem(key: String): String?

    public fun setItem(key: String, value: String)

    public fun removeItem(key: String)
}

/** [MarkerStorage] over one plain `SharedPreferences` file. Writes commit
 *  synchronously: a marker must be on disk before the step after it runs. */
public class SharedPreferencesMarkerStorage(
    context: Context,
    prefsName: String,
) : MarkerStorage {
    private val prefs = context.applicationContext.getSharedPreferences(prefsName, Context.MODE_PRIVATE)

    override fun getItem(key: String): String? = prefs.getString(key, null)

    override fun setItem(key: String, value: String) {
        prefs.edit().putString(key, value).commit()
    }

    override fun removeItem(key: String) {
        prefs.edit().remove(key).commit()
    }
}

/** The preferences file of the guest marker. */
public const val GUEST_ACCOUNT_PREFS: String = "lakko-guest-account"

/** What the device knows about a guest account. */
public data class GuestAccountState(
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
    val claimedUsername: String?,
)

/** The marker's key for [accountIdB64] in [GUEST_ACCOUNT_PREFS]. */
public fun guestMarkerKey(accountIdB64: String): String = "account:$accountIdB64"

private const val MARK_GUEST = "guest"
private const val MARK_NAMED_PREFIX = "named:"

/**
 * The [me.lakko.app.backup.PhraseStore] key a guest's recovery phrase is kept
 * under. Not the account id: see the file comment.
 */
public fun guestPhraseKey(accountIdB64: String): String = "guest:$accountIdB64"

/** Record that [accountIdB64] is a guest account. Never throws. */
public fun markGuestAccount(storage: MarkerStorage?, accountIdB64: String) {
    try {
        storage?.setItem(guestMarkerKey(accountIdB64), MARK_GUEST)
    } catch (_: Exception) {
        // Storage refused: the account then reads as an ordinary one with no
        // username, which is what it is. It is still deleted after a day.
    }
}

/** Record the username taken for a guest account that is being kept. Never
 *  throws. */
public fun noteGuestUsernameClaimed(storage: MarkerStorage?, accountIdB64: String, username: String) {
    try {
        storage?.setItem(guestMarkerKey(accountIdB64), MARK_NAMED_PREFIX + username)
    } catch (_: Exception) {
        // Storage refused: a retry asks for the name again and the server
        // answers that the account already has one.
    }
}

/** What the device knows about [accountIdB64] as a guest, or `null` when it
 *  is not a guest account. Never throws. */
public fun guestAccountState(storage: MarkerStorage?, accountIdB64: String): GuestAccountState? {
    val raw = try {
        storage?.getItem(guestMarkerKey(accountIdB64))
    } catch (_: Exception) {
        null
    } ?: return null
    if (raw == MARK_GUEST) return GuestAccountState(claimedUsername = null)
    if (raw.startsWith(MARK_NAMED_PREFIX)) {
        val name = raw.removePrefix(MARK_NAMED_PREFIX)
        return GuestAccountState(claimedUsername = name.ifEmpty { null })
    }
    // An unknown value is not a guest: never offer to delete an account on the
    // strength of bytes this build did not write.
    return null
}

/** Forget that [accountIdB64] is a guest: it became an account, or it is
 *  being removed from this device. Never throws. */
public fun clearGuestAccount(storage: MarkerStorage?, accountIdB64: String) {
    try {
        storage?.removeItem(guestMarkerKey(accountIdB64))
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

/** What [keepGuestAccount] drives. */
internal interface GuestClaimPorts {
    /** The guest's recovery phrase, or `null` when it cannot be read. */
    suspend fun readGuestPhrase(): String?

    /** The username an earlier attempt already took, or `null`. */
    fun claimedUsername(): String?

    /** Bind the login handle at auth, then register the name in the
     *  directory, releasing the handle if the directory refuses.
     *  @return the normalised username. */
    suspend fun claimUsername(username: String): String

    /** Record the taken username in the marker and the persisted account. */
    fun noteUsernameClaimed(username: String)

    /** Set the password through the platform's `recover`. */
    suspend fun setPassword(phrase: String, password: ByteArray, username: String)

    /** Mark the signup phrase as owed and hand it to the phrase screen. */
    fun owePhrase(phrase: String)

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
 *   1. the phrase, or [GuestPhraseUnavailable]: it is what proves the account
 *      to the password reset and what the person is shown at the end, so
 *      nothing starts without it;
 *   2. the username, once: an account holds one and it cannot be changed, so
 *      a retry after a failed password step goes on with the name already
 *      taken and never asks the directory again;
 *   3. the password, by the platform's `recover`;
 *   4. only then the account stops being a guest: the phrase becomes owed
 *      (shown before any other screen), the marker goes, the guest phrase
 *      row goes.
 *
 * The web client deletes the device's KeyPackage between 2 and 3, because its
 * password reset re-keys the device. Android's `recover` keeps the device
 * identity (it is not wrapped under the password-derived key), so the
 * published KeyPackage stays valid and is left in place. Push registration is
 * the last step of `recover` itself here, so it also happens only once the
 * password is set.
 */
internal suspend fun keepGuestAccount(ports: GuestClaimPorts, username: String, password: ByteArray) {
    val phrase = ports.readGuestPhrase() ?: throw GuestPhraseUnavailable()
    var name = ports.claimedUsername()
    if (name == null) {
        val trimmed = username.trim()
        if (trimmed.isEmpty()) throw AuthClientArgError("username must be non-empty")
        name = ports.claimUsername(trimmed)
        ports.noteUsernameClaimed(name)
    }
    ports.setPassword(phrase, password, name)
    // The phrase screen is armed before the marker goes, so no screen sees an
    // account that is neither a guest nor owed its phrase.
    ports.owePhrase(phrase)
    ports.clearGuest()
    ports.deleteGuestPhrase()
}

// =============================================================================
// Delete
// =============================================================================

/** What [deleteGuestAccount] drives. */
internal interface GuestDiscardPorts {
    /** `DELETE /account` on the auth service. Throws on any failure. */
    suspend fun deleteAtAuth()

    /** The directory's delete-account. */
    suspend fun deleteAtDirectory()

    /** Everything on this device (the guest marker and the phrase row are in
     *  the wipe list); lands signed out. */
    fun forgetDevice()

    fun log(message: String, cause: Throwable)
}

/**
 * Delete a guest account at once.
 *
 * The auth service first, and alone decisive: once the account is gone there,
 * nothing can sign in to it. A failure throws and leaves everything in place
 * so the person can try again (and the server deletes an account without a
 * username after a day regardless). The directory is best effort: its own
 * daily sweep removes what is left of an account that has no username. Then
 * everything on this device.
 */
internal suspend fun deleteGuestAccount(ports: GuestDiscardPorts) {
    ports.deleteAtAuth()
    try {
        ports.deleteAtDirectory()
    } catch (cancel: CancellationException) {
        throw cancel
    } catch (cause: Exception) {
        ports.log("guest account: directory rows not deleted", cause)
    }
    ports.forgetDevice()
}
