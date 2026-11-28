// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context
import kotlinx.coroutines.CancellationException
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import me.lakko.app.l10n.UiText

/*
 * ADR-0076 - choosing a username later, and changing it. Everything here is
 * a pure decision, a small store, or a machine over ports, so a host JVM
 * test runs the whole rename (rollback and the owed record included) without
 * a server or the native core.
 */

// ---------------------------------------------------------------------------
// The session's handle sync (ADR-0076 section 2)
// ---------------------------------------------------------------------------

/** `POST /auth/login-handle/check`: what the account's bound handle is,
 *  relative to the one this device derives from its local username. */
public enum class LoginHandleState(public val wire: String) {
    /** The account holds exactly this handle. */
    Same("same"),

    /** The account holds ANOTHER handle: it was renamed elsewhere. */
    Other("other"),

    /** The account holds no handle. */
    None("none"),
    ;

    public companion object {
        public fun ofWire(wire: String): LoginHandleState? = entries.firstOrNull { it.wire == wire }
    }
}

/** What the session's handle sync does. */
public enum class HandleSyncAction {
    /** The handle is bound as this device knows it. */
    Nothing,

    /** First bind (`PUT`, no grant). */
    Bind,

    /** Bind NOTHING and wait for the name from the account's own devices:
     *  a `PUT` here would revert a rename made elsewhere. */
    WaitForRegister,
}

/** The rule: bind only when the account holds no handle. */
public fun handleSyncAction(state: LoginHandleState): HandleSyncAction = when (state) {
    LoginHandleState.Same -> HandleSyncAction.Nothing
    LoginHandleState.None -> HandleSyncAction.Bind
    LoginHandleState.Other -> HandleSyncAction.WaitForRegister
}

// ---------------------------------------------------------------------------
// The own-username register between the account's devices
// ---------------------------------------------------------------------------

/** The own-username register as this device's synced state holds it. */
public data class OwnUsernameState(val ready: Boolean, val username: String?) {
    public companion object {
        public val NOT_READY: OwnUsernameState = OwnUsernameState(ready = false, username = null)
    }
}

/** What the auth side needs of the register. */
public interface OwnUsernamePort {
    public fun state(): OwnUsernameState

    /** `false` when the state is not open yet. */
    public suspend fun write(username: String?): Boolean
}

/** What a device does when the register is readable. */
public sealed interface OwnUsernameSync {
    public data object Nothing : OwnUsernameSync

    /** The register is empty and this device has a name (an account from
     *  before the register): written once from the local marker, and only
     *  after auth confirmed that name's handle in this session. */
    public data class Seed(val username: String) : OwnUsernameSync

    /** A name written by this device before the state was open: written now. */
    public data class Flush(val username: String) : OwnUsernameSync

    /** The register holds another name than the local marker: the account
     *  was renamed on another device. The marker, the session state, the
     *  certificate and the profile follow it. */
    public data class Adopt(val username: String) : OwnUsernameSync
}

/**
 * The rule. [parked] is a write this device made while the state was closed;
 * it goes first (the register's own last-writer-wins decides whether it
 * survives). [renaming] is `true` while this device's own rename is between
 * its steps: nothing is adopted or seeded then.
 *
 * [handleConfirmed]: auth confirmed the local name's handle in this session
 * (the handle check answered `same`, or this device bound the handle itself
 * on `none`). An EMPTY register is seeded only then. A stale device that has
 * not yet received a rename would otherwise write its old name with a newer
 * stamp and undo the rename on every device; on `other`, or while the check
 * has not answered, it never seeds and waits for the register.
 */
public fun ownUsernameSync(
    register: OwnUsernameState,
    localUsername: String,
    parked: String?,
    renaming: Boolean = false,
    handleConfirmed: Boolean,
): OwnUsernameSync {
    if (!register.ready || renaming) return OwnUsernameSync.Nothing
    if (parked != null && parked.isNotBlank() && parked != register.username) return OwnUsernameSync.Flush(parked)
    val held = register.username?.takeIf { it.isNotBlank() }
    val local = localUsername.trim()
    return when {
        held == null && local.isNotEmpty() && handleConfirmed -> OwnUsernameSync.Seed(local)
        held != null && held != local -> OwnUsernameSync.Adopt(held)
        else -> OwnUsernameSync.Nothing
    }
}

/** What the bind carries for the recovery email. */
public enum class RenameRecoveryEmail {
    /** No recovery email, or this device does not hold the address and the
     *  account has another second way in: the bind carries none (a recovery
     *  email is then dropped by the server). */
    None,

    /** The address: the email moves to the new name with no letter. */
    Address,

    /**
     * UX 19.1c - the email is the account's LAST second way in and this
     * device holds no address: the username screen asks for it before the
     * change ("Your recovery email moves to your new username. Type it
     * once."). Nothing is sent without it: the server would refuse the
     * change (`409 last-factor`).
     */
    AskAddress,
}

/**
 * The plan for the recovery email of a rename. [addressHeld] is an address
 * this device keeps or the person just typed. Since the email vault was
 * removed (ADR-0075, amended 2026-11-26) nothing but the address travels.
 */
public fun renameRecoveryEmail(
    hasRecoveryEmail: Boolean,
    addressHeld: Boolean,
    emailIsLastFactor: Boolean,
): RenameRecoveryEmail = when {
    !hasRecoveryEmail -> RenameRecoveryEmail.None
    addressHeld -> RenameRecoveryEmail.Address
    emailIsLastFactor -> RenameRecoveryEmail.AskAddress
    else -> RenameRecoveryEmail.None
}

// ---------------------------------------------------------------------------
// The rename: one resumable operation
// ---------------------------------------------------------------------------

/** How far a rename got. Names are not secret; nothing else is stored. */
@Serializable
public data class UsernameRenameOwed(
    val oldUsername: String,
    val newUsername: String,
    /** `rename/bind` may have reached auth (set BEFORE it is sent). */
    val bindSent: Boolean = false,
    /** Auth holds the new handle. */
    val bound: Boolean = false,
    /** Discovery holds the new name. */
    val replaced: Boolean = false,
)

/** The next thing a rename (or its resume at a landing) does. */
public enum class RenameStep { Bind, Replace, Adopt, Done }

public fun nextRenameStep(owed: UsernameRenameOwed): RenameStep = when {
    !owed.bound -> RenameStep.Bind
    !owed.replaced -> RenameStep.Replace
    else -> RenameStep.Adopt
}

/** What the effects of a rename are, behind ports. */
public interface UsernameRenamePorts {
    /**
     * `PUT /auth/login-handle`: move the account's handle from [from]'s to
     * [to]'s, with the grant and, when the device holds the address, the
     * recovery email re-keyed (the vault sealed under [to]'s handle).
     */
    public suspend fun bind(from: String, to: String)

    /** Discovery OPRF + `username/replace`; the certificate of the answer. */
    public suspend fun replace(newUsername: String): ByteArray?

    /** The marker, the session state, the register, the certificate, the
     *  local stamp, and the re-publish. */
    public suspend fun adopt(newUsername: String, certificate: ByteArray?)
}

/** How a rename ended. */
public sealed interface RenameResult {
    public data class Done(val username: String) : RenameResult

    /** Nothing changed (the bind was refused, or it was rolled back). */
    public data class Refused(
        val line: UiText,
        val needsProof: Boolean = false,
        /** UX 19.1c: the recovery email is the account's last second way in
         *  and must be typed once so it moves with the name. [line] is then
         *  the helper of the address field, not an error. */
        val needsRecoveryAddress: Boolean = false,
    ) : RenameResult

    /** A step has no answer or could not be undone; the record stays and
     *  the next landing finishes or rolls back. */
    public data class Pending(val line: UiText) : RenameResult
}

/** UX 19.1c: the rename needs the recovery address typed before anything is
 *  sent (the email is the account's last second way in). */
public class RenameNeedsRecoveryAddress : Exception("the recovery email must move with the username")

/** The line a failed rename shows. */
public fun renameFailureLine(err: Throwable): UiText = when {
    err is AuthClientArgError -> err.text ?: UiText("common.error.unavailable")
    err is AuthNetworkError || err is me.lakko.app.discovery.DiscoveryNetworkError -> UiText("common.error.offline")
    isUsernameTaken(err) -> UiText("auth.signup.error.username_taken")
    err is AuthHttpError -> authLimitText(err) ?: UiText("common.error.unavailable")
    else -> UiText("common.error.unavailable")
}

/** `409 username-taken` from discovery, `409 handle-taken` from auth. A
 *  `409` with no code reads as taken (the older bodiless answer). */
public fun isUsernameTaken(err: Throwable): Boolean = when (err) {
    is me.lakko.app.discovery.DiscoveryHttpError ->
        err.status == HTTP_CONFLICT_STATUS && (err.code == null || err.code == "username-taken")
    is AuthHttpError -> err.status == HTTP_CONFLICT_STATUS && (err.code == null || err.code == "handle-taken")
    else -> false
}

/** Whether a failed request may still have landed (no answer was read). */
private fun answerLost(err: Throwable): Boolean =
    err is AuthNetworkError || err is me.lakko.app.discovery.DiscoveryNetworkError ||
        (err is AuthHttpError && err.status >= SERVER_ERROR_STATUS) ||
        (err is me.lakko.app.discovery.DiscoveryHttpError && err.status >= SERVER_ERROR_STATUS)

/**
 * The rename machine. [start] is the screen's button; [resume] runs at a
 * landing for a record a crash left. Order: bind the new handle at auth,
 * replace the name at discovery, adopt locally. A refused `replace` binds the
 * OLD handle back; a step with no answer keeps the record.
 */
public class UsernameRenameRunner(
    private val accountIdB64: String,
    private val store: UsernameRenameStore,
    private val ports: UsernameRenamePorts,
    private val log: (String) -> Unit = {},
) {
    public fun owed(): UsernameRenameOwed? = store.load(accountIdB64)

    public suspend fun start(oldUsername: String, newUsername: String): RenameResult {
        // A rename a crash left is settled first, never overwritten.
        store.load(accountIdB64)?.let { earlier ->
            val settled = advance(earlier)
            if (settled !is RenameResult.Refused) return settled
        }
        return advance(UsernameRenameOwed(oldUsername = oldUsername, newUsername = newUsername))
    }

    /** Finish or roll back what a crash left; `null` when nothing is owed. */
    public suspend fun resume(): RenameResult? = store.load(accountIdB64)?.let { advance(it) }

    @Suppress("ReturnCount", "CyclomaticComplexMethod")
    private suspend fun advance(start: UsernameRenameOwed): RenameResult {
        var owed = start
        // A bind of an earlier run that may have landed (no answer was read).
        val bindWasSent = start.bindSent
        if (!owed.bound) {
            // Before the bind can reach the server.
            owed = save(owed.copy(bindSent = true))
            try {
                ports.bind(from = owed.oldUsername, to = owed.newUsername)
            } catch (cancel: CancellationException) {
                throw cancel
            } catch (cause: Throwable) {
                if (answerLost(cause)) {
                    // It may have landed: the same bind is asked again at
                    // the next landing (a re-bind of a held handle is a no-op).
                    return RenameResult.Pending(renameFailureLine(cause))
                }
                val needsProof = cause is FreshAuthRequired || isFreshAuthRequired(cause)
                if (bindWasSent && needsProof) {
                    // Whether the earlier bind landed is not known, and it
                    // cannot be asked again without a proof: the record
                    // stays until a run that holds one settles it.
                    return RenameResult.Pending(renameFailureLine(cause))
                }
                store.clear(accountIdB64)
                if (cause is RenameNeedsRecoveryAddress || isLastFactor(cause)) {
                    // Nothing was sent (or the server refused to drop the
                    // last second way in): ask for the address once.
                    return RenameResult.Refused(
                        line = UiText("protect.username_email"),
                        needsRecoveryAddress = true,
                    )
                }
                return RenameResult.Refused(line = renameFailureLine(cause), needsProof = needsProof)
            }
            owed = save(owed.copy(bound = true))
        }
        if (!owed.replaced) {
            val certificate = try {
                ports.replace(owed.newUsername)
            } catch (cancel: CancellationException) {
                throw cancel
            } catch (cause: Throwable) {
                if (answerLost(cause)) return RenameResult.Pending(renameFailureLine(cause))
                // Refused (taken, ...): the old handle goes back.
                val refused = RenameResult.Refused(line = renameFailureLine(cause))
                return try {
                    ports.bind(from = owed.newUsername, to = owed.oldUsername)
                    store.clear(accountIdB64)
                    refused
                } catch (cancel: CancellationException) {
                    throw cancel
                } catch (back: Throwable) {
                    log("username rename: the old handle was not bound back (${back.javaClass.simpleName})")
                    RenameResult.Pending(refused.line)
                }
            }
            owed = save(owed.copy(replaced = true))
            return adopt(owed, certificate)
        }
        return adopt(owed, null)
    }

    private suspend fun adopt(owed: UsernameRenameOwed, certificate: ByteArray?): RenameResult =
        try {
            ports.adopt(owed.newUsername, certificate)
            store.clear(accountIdB64)
            RenameResult.Done(owed.newUsername)
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (cause: Throwable) {
            log("username rename: the new name was not stored yet (${cause.javaClass.simpleName})")
            RenameResult.Pending(renameFailureLine(cause))
        }

    private fun save(next: UsernameRenameOwed): UsernameRenameOwed {
        store.save(accountIdB64, next)
        return next
    }
}

/** The owed record of a rename in progress, per account. */
public class UsernameRenameStore(private val storage: MarkerStorage) {
    private val json = Json { ignoreUnknownKeys = true }

    public fun load(accountIdB64: String): UsernameRenameOwed? = try {
        storage.get(OWED_PREFIX + accountIdB64)?.let { json.decodeFromString(UsernameRenameOwed.serializer(), it) }
    } catch (_: Throwable) {
        null
    }

    public fun save(accountIdB64: String, owed: UsernameRenameOwed) {
        try {
            me.lakko.app.account.AccountStoreGate.write(accountIdB64) { storage.set(OWED_PREFIX + accountIdB64, json.encodeToString(UsernameRenameOwed.serializer(), owed)) }
        } catch (_: Throwable) {
            // The record then lives for this run only.
        }
    }

    public fun clear(accountIdB64: String) {
        try {
            storage.remove(OWED_PREFIX + accountIdB64)
        } catch (_: Throwable) {
            // Nothing to drop.
        }
    }

    public fun clearAccount(accountIdB64: String) {
        clear(accountIdB64)
    }

    public companion object {
        public const val PREFS_NAME: String = "lakko-username-rename"
        private const val OWED_PREFIX: String = "owed:"

        public fun onDevice(context: Context): UsernameRenameStore =
            UsernameRenameStore(SharedPrefsMarkerStorage(context, PREFS_NAME))
    }
}

private const val HTTP_CONFLICT_STATUS: Int = 409
private const val SERVER_ERROR_STATUS: Int = 500
