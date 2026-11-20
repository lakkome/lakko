// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import me.lakko.app.l10n.UiText

/*
 * Learn the account's username after a sign-in that typed none (a passkey
 * sign-in on a new device, `docs/ux/identity-recovery.md`; ADR-0081 third
 * amendment, point 6). Without a username the device has no login handle,
 * cannot knock on the account's DeviceGroup and waits for ever.
 *
 * The rule, the same on every client:
 *  1. from the backup first: the own profile envelope's username and the
 *     own-username register a restore gave, each checked with auth
 *     (`POST /auth/login-handle/check`); on `same` it is adopted silently;
 *  2. else the prompt asks for it, and the typed name is checked the same
 *     way: `same` adopts and closes, `other` says it is not this account's,
 *     `none` closes (the account has no username), `429` says to try later,
 *     anything else is a connection failure;
 *  3. "I don't remember" shows a note once and closes; the prompt asks again
 *     at the next start of the app while the username is still empty.
 *
 * Never logs the username or its handle.
 */

/** The live session the prompt is about. */
public data class UsernamePromptSession(
    val accountIdB64: String,
    val username: String,
)

/** What [UsernamePromptController] needs of the app. */
public interface UsernamePromptPorts {
    /** The live session, or `null` when there is none the prompt may serve
     *  (signed out, a guest account, secrets of another account). */
    public fun session(): UsernamePromptSession?

    /** The usernames the account's backup gave this device (the own profile
     *  envelope, the own-username register), in that order. */
    public fun backupCandidates(accountIdB64: String): List<String>

    /** The canonical name and its login handle.
     *  @throws AuthClientArgError carrying the username rule's sentence. */
    public fun handleFor(username: String): NamedHandle

    /** `POST /auth/login-handle/check` with the session.
     *  @throws AuthHttpError on a refusal (`429` and the rest). */
    public suspend fun check(handle: ByteArray): LoginHandleState

    /** Store [username] as the account's, as a sign-in by name does. */
    public fun adopt(accountIdB64: String, username: String)
}

/** What the prompt shows. `null` from [UsernamePromptController.state] means
 *  the prompt is closed. */
public data class UsernamePromptState(
    /** A check is running: the buttons wait. */
    val busy: Boolean = false,
    /** The sentence under the field, or `null`. */
    val error: UiText? = null,
    /** "I don't remember" was chosen: the note is the prompt's last state. */
    val forgotNote: Boolean = false,
)

/** What a check of one name came to. */
public enum class UsernameCheckResult { SAME, OTHER, NONE, TOO_MANY, NETWORK }

/** [UsernameCheckResult] of a check that threw [cause]. */
public fun usernameCheckFailure(cause: Throwable): UsernameCheckResult =
    if (cause is AuthHttpError && cause.status == HTTP_TOO_MANY_REQUESTS_PROMPT) {
        UsernameCheckResult.TOO_MANY
    } else {
        UsernameCheckResult.NETWORK
    }

private const val HTTP_TOO_MANY_REQUESTS_PROMPT: Int = 429

/**
 * The username prompt and the silent adoption before it. Process-lifetime:
 * "I don't remember" holds until the process ends, so the next app start
 * asks again. Every decision runs under one lock.
 */
public class UsernamePromptController(
    private val ports: UsernamePromptPorts,
    /** Status only, never a name or a handle. */
    private val log: (String) -> Unit = {},
) {
    private val mutableState: MutableStateFlow<UsernamePromptState?> = MutableStateFlow(null)

    /** The prompt, or `null` when it is closed. */
    public val state: StateFlow<UsernamePromptState?> = mutableState.asStateFlow()

    private val lock = Mutex()

    /** Accounts auth said have no username: nothing more to do. */
    private val noUsername: MutableSet<String> = HashSet()

    /** Accounts whose prompt "I don't remember" closed in this process: the
     *  prompt is not shown again, a backup name is still adopted. */
    private val dismissed: MutableSet<String> = HashSet()

    /** Backup names auth said are not this account's, per account. */
    private val refused: MutableSet<Pair<String, String>> = HashSet()

    /**
     * A session landed, or a restore or the register gave this device
     * something new: adopt a backup name auth confirms, else open the prompt.
     */
    public suspend fun evaluate(): Unit = lock.withLock {
        val session = ports.session()
        if (session == null || session.username.isNotBlank()) {
            if (mutableState.value?.forgotNote != true) mutableState.value = null
            return@withLock
        }
        val account = session.accountIdB64
        if (account in noUsername) return@withLock
        val candidates = try {
            ports.backupCandidates(account)
        } catch (cause: Throwable) {
            if (cause is CancellationException) throw cause
            log("username prompt: backup names not read (${cause.javaClass.simpleName})")
            emptyList()
        }
        for (candidate in candidates.map { it.trim() }.filter { it.isNotEmpty() }.distinct()) {
            val named = try {
                ports.handleFor(candidate)
            } catch (cause: Throwable) {
                if (cause is CancellationException) throw cause
                continue
            }
            val key = account to named.username
            if (key in refused) {
                named.handle.fill(0)
                continue
            }
            when (checkOf(named.handle)) {
                UsernameCheckResult.SAME -> {
                    log("username prompt: the backup's name is the account's; adopted")
                    adoptOrStay(account, named.username)
                    return@withLock
                }
                UsernameCheckResult.NONE -> {
                    log("username prompt: the account has no username")
                    noUsername += account
                    if (mutableState.value?.forgotNote != true) mutableState.value = null
                    return@withLock
                }
                UsernameCheckResult.OTHER -> refused += key
                UsernameCheckResult.TOO_MANY, UsernameCheckResult.NETWORK ->
                    log("username prompt: the backup's name was not checked")
            }
        }
        if (account !in dismissed && mutableState.value == null) mutableState.value = UsernamePromptState()
    }

    /** "Continue" with [typed]. */
    public suspend fun submit(typed: String): Unit = lock.withLock {
        val current = mutableState.value ?: return@withLock
        if (current.busy || current.forgotNote) return@withLock
        val session = ports.session()
        if (session == null || session.username.isNotBlank()) {
            mutableState.value = null
            return@withLock
        }
        val named = try {
            ports.handleFor(typed)
        } catch (cause: Throwable) {
            if (cause is CancellationException) throw cause
            // The username rule's own sentence; no request is made.
            mutableState.value = current.copy(
                error = (cause as? AuthClientArgError)?.text ?: UiText("username.error.unknown"),
            )
            return@withLock
        }
        mutableState.value = current.copy(busy = true, error = null)
        when (checkOf(named.handle)) {
            UsernameCheckResult.SAME -> adoptOrStay(session.accountIdB64, named.username)
            UsernameCheckResult.NONE -> {
                noUsername += session.accountIdB64
                mutableState.value = null
            }
            UsernameCheckResult.OTHER ->
                mutableState.value = UsernamePromptState(error = UiText("auth.username_prompt.error.not_this"))
            UsernameCheckResult.TOO_MANY ->
                mutableState.value = UsernamePromptState(error = UiText("auth.username_prompt.error.too_many"))
            UsernameCheckResult.NETWORK ->
                mutableState.value = UsernamePromptState(error = UiText("auth.username_prompt.error.network"))
        }
    }

    /** "I don't remember": the note is shown, and nothing asks again in this
     *  process. */
    public suspend fun forgot(): Unit = lock.withLock {
        val current = mutableState.value ?: return@withLock
        if (current.busy) return@withLock
        ports.session()?.let { dismissed += it.accountIdB64 }
        mutableState.value = UsernamePromptState(forgotNote = true)
    }

    /** The note was read: the prompt closes. */
    public fun closeNote() {
        if (mutableState.value?.forgotNote == true) mutableState.value = null
    }

    /** The session ended or another account landed: the prompt closes. What
     *  was decided in this process stays decided. */
    public fun close() {
        mutableState.value = null
    }

    private suspend fun checkOf(handle: ByteArray): UsernameCheckResult =
        try {
            when (ports.check(handle)) {
                LoginHandleState.Same -> UsernameCheckResult.SAME
                LoginHandleState.Other -> UsernameCheckResult.OTHER
                LoginHandleState.None -> UsernameCheckResult.NONE
            }
        } catch (cause: Throwable) {
            if (cause is CancellationException) throw cause
            usernameCheckFailure(cause).also {
                log("username prompt: check failed (${cause.javaClass.simpleName})")
            }
        } finally {
            handle.fill(0)
        }

    private fun adoptOrStay(account: String, username: String) {
        try {
            ports.adopt(account, username)
            mutableState.value = null
        } catch (cause: Throwable) {
            log("username prompt: name not stored (${cause.javaClass.simpleName})")
            mutableState.value = UsernamePromptState(error = UiText("auth.username_prompt.error.network"))
        }
    }
}
