// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.longOrNull

/*
 * ADR-0098 section 4 (UX `docs/ux/reporting-and-moderation.md` section 4) -
 * a suspended account, seen by its owner.
 *
 * Auth answers a suspended account `403 account-suspended`. On a landing
 * route and on the refresh route the body carries a restricted session (an
 * `s1` credential, as long-lived as an access token) and an ordinary `r2`
 * refresh credential; on every other bearer route it carries nothing, and
 * the client refreshes to get them. The `s1` opens exactly: reading the
 * suspension, one appeal, deleting the account, signing out.
 *
 * This device then runs in suspended mode: the suspended screen, or the app
 * read-only after "Read my chats". The mode is kept per account (it survives
 * a restart), the refresh route is asked again on every app open and every
 * [SUSPENSION_PROBE_INTERVAL_MS] while the app is in front, and a normal
 * pair ends the mode without a word.
 *
 * Nothing here names the account in a log line.
 */

/** The `error` code of every suspension refusal. */
public const val ACCOUNT_SUSPENDED: String = "account-suspended"

/** How often a suspended device asks the refresh route again while the app
 *  is in front (chosen, not measured; the `s1` lives as long as an access
 *  token, so this also keeps it live). */
public const val SUSPENSION_PROBE_INTERVAL_MS: Long = 10L * 60L * 1000L

/** The longest appeal, in characters (Unicode code points), after trimming. */
public const val APPEAL_MAX_CHARS: Int = 1000

/**
 * What a `403 account-suspended` from a landing or the refresh route hands
 * the client. Never printed: [toString] redacts every credential.
 */
public class SuspendedCredentials(
    /** Standard base64 of the 16-byte account id (the client's spelling). */
    public val accountIdB64: String,
    /** The `s1` restricted session. */
    public val restrictedToken: String,
    /** Unix milliseconds; the `s1` is unusable at or after this. */
    public val restrictedExpiresAtMs: Long,
    /** An ordinary `r2` refresh credential. */
    public val refreshToken: String,
    /** Unix milliseconds. */
    public val refreshExpiresAtMs: Long,
    /** The key generation both were issued under. */
    public val keyGeneration: Long,
    /** On the password and passkey sign-ins only: the fresh-auth grant, so a
     *  suspended account without a passkey can still delete itself. */
    public val freshGrant: String? = null,
    public val freshGrantExpiresAtMs: Long? = null,
) {
    /**
     * The pair this device serves while suspended: the `s1` in the access
     * slot (every request made with it is either one of the routes it opens
     * or refused), the `r2` in the refresh slot.
     */
    public fun asSessionPair(): SessionPair = SessionPair(
        accessToken = restrictedToken,
        accessExpiresAtMs = restrictedExpiresAtMs,
        refreshToken = refreshToken,
        refreshExpiresAtMs = refreshExpiresAtMs,
        keyGeneration = keyGeneration,
    )

    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is SuspendedCredentials) return false
        return accountIdB64 == other.accountIdB64 &&
            restrictedToken == other.restrictedToken &&
            restrictedExpiresAtMs == other.restrictedExpiresAtMs &&
            refreshToken == other.refreshToken &&
            refreshExpiresAtMs == other.refreshExpiresAtMs &&
            keyGeneration == other.keyGeneration &&
            freshGrant == other.freshGrant &&
            freshGrantExpiresAtMs == other.freshGrantExpiresAtMs
    }

    override fun hashCode(): Int {
        var h = accountIdB64.hashCode()
        h = 31 * h + restrictedToken.hashCode()
        h = 31 * h + refreshToken.hashCode()
        h = 31 * h + keyGeneration.hashCode()
        return h
    }

    override fun toString(): String =
        "SuspendedCredentials(restricted=<redacted>, refresh=<redacted>, keyGeneration=$keyGeneration, " +
            "grant=${freshGrant != null})"
}

/** The version prefix of a restricted session. */
public const val RESTRICTED_TOKEN_PREFIX: String = "s1."

private val lenientJson: Json = Json { ignoreUnknownKeys = true }

private fun objectOf(text: String?): JsonObject? {
    if (text.isNullOrBlank()) return null
    return try {
        lenientJson.parseToJsonElement(text) as? JsonObject
    } catch (_: Throwable) {
        null
    }
}

private fun JsonObject.string(name: String): String? = (get(name) as? JsonPrimitive)?.contentOrNull

private fun JsonObject.long(name: String): Long? = (get(name) as? JsonPrimitive)?.longOrNull

/**
 * The credentials of a `403 account-suspended` body, or `null` when [text]
 * is not one carrying all of them (the bare refusal of an ordinary bearer
 * route carries none). The account id arrives URL-safe and unpadded; it is
 * returned in the standard spelling the rest of the client uses.
 */
public fun parseSuspendedBody(text: String?): SuspendedCredentials? {
    val obj = objectOf(text) ?: return null
    if (obj.string("error") != ACCOUNT_SUSPENDED) return null
    val account = obj.string("account_id_b64")?.let(::standardAccountId) ?: return null
    val restricted = obj.string("restricted_token")?.takeIf { it.startsWith(RESTRICTED_TOKEN_PREFIX) } ?: return null
    val restrictedExpires = obj.long("restricted_expires_at_ms") ?: return null
    val refresh = obj.string("refresh_token")?.takeIf { it.startsWith(REFRESH_TOKEN_PREFIX) } ?: return null
    val refreshExpires = obj.long("refresh_expires_at_ms") ?: return null
    val generation = obj.long("key_generation") ?: return null
    return SuspendedCredentials(
        accountIdB64 = account,
        restrictedToken = restricted,
        restrictedExpiresAtMs = restrictedExpires,
        refreshToken = refresh,
        refreshExpiresAtMs = refreshExpires,
        keyGeneration = generation,
        freshGrant = obj.string("fresh_grant")?.takeIf { it.isNotEmpty() },
        freshGrantExpiresAtMs = obj.long("fresh_grant_expires_at_ms"),
    )
}

/** Whether a response is a suspension refusal (with or without credentials). */
public fun isAccountSuspended(status: Int, errorCode: String?): Boolean =
    status == HTTP_FORBIDDEN && errorCode == ACCOUNT_SUSPENDED

/** URL-safe or standard base64 of 16 bytes, in the standard padded spelling. */
internal fun standardAccountId(text: String): String? {
    val bytes = try {
        java.util.Base64.getUrlDecoder().decode(text.replace('+', '-').replace('/', '_').trimEnd('='))
    } catch (_: IllegalArgumentException) {
        return null
    }
    if (bytes.size != SUSPENDED_ACCOUNT_ID_LEN) return null
    return java.util.Base64.getEncoder().encodeToString(bytes)
}

private const val SUSPENDED_ACCOUNT_ID_LEN: Int = 16
private const val HTTP_FORBIDDEN: Int = 403

// ---------------------------------------------------------------------------
// GET /account/suspension
// ---------------------------------------------------------------------------

/** Where an appeal stands (`appeal` of `GET /account/suspension`). */
public enum class AppealState { NONE, SENT, DECLINED }

/** What the suspended screen shows. */
public data class SuspensionDetails(
    /** The kebab reason name (`spam`, `child-safety`, ...), or `null`. */
    val reasonName: String?,
    /** Unix milliseconds the suspension ends, or `null` for no end date. */
    val untilMs: Long?,
    val appeal: AppealState,
)

/** The answer of `GET /account/suspension`. */
public sealed interface SuspensionStatusAnswer {
    public data class Suspended(val details: SuspensionDetails) : SuspensionStatusAnswer
    public data object NotSuspended : SuspensionStatusAnswer
    public data object Unreachable : SuspensionStatusAnswer
}

/** Parse a `GET /account/suspension` body; [SuspensionStatusAnswer.Unreachable]
 *  for anything that is not its shape. */
public fun parseSuspensionStatus(text: String?): SuspensionStatusAnswer {
    val obj = objectOf(text) ?: return SuspensionStatusAnswer.Unreachable
    val suspended = (obj["suspended"] as? JsonPrimitive)?.booleanOrNull ?: return SuspensionStatusAnswer.Unreachable
    if (!suspended) return SuspensionStatusAnswer.NotSuspended
    val appeal = when (obj.string("appeal")) {
        "sent" -> AppealState.SENT
        "declined" -> AppealState.DECLINED
        else -> AppealState.NONE
    }
    return SuspensionStatusAnswer.Suspended(
        SuspensionDetails(reasonName = obj.string("reason_name"), untilMs = obj.long("until_ms"), appeal = appeal),
    )
}

/** The reason names auth and the report sheet share (ADR-0098 section 2.2). */
public val SUSPENSION_REASON_NAMES: Set<String> = setOf(
    "spam", "harassment", "hate", "violence", "sexual", "child-safety", "impersonation", "other",
)

/**
 * The catalog key of the reason the suspended screen names: the report
 * sheet's `report.reason.<name>` text (kebab becomes snake), `other` for a
 * name this build does not know or none.
 */
public fun suspendedReasonKey(reasonName: String?): String {
    val name = reasonName?.trim()?.lowercase()?.takeIf { it in SUSPENSION_REASON_NAMES } ?: "other"
    return "report.reason." + name.replace('-', '_')
}

// ---------------------------------------------------------------------------
// The appeal sheet
// ---------------------------------------------------------------------------

/** The appeal's length in characters, as the core counts them (after trim). */
public fun appealLength(text: String): Int {
    val trimmed = text.trim()
    return trimmed.codePointCount(0, trimmed.length)
}

/** [text] cut to [APPEAL_MAX_CHARS] code points (the field stops there). */
public fun clampAppeal(text: String): String {
    if (text.codePointCount(0, text.length) <= APPEAL_MAX_CHARS) return text
    return text.substring(0, text.offsetByCodePoints(0, APPEAL_MAX_CHARS))
}

public enum class AppealPhase { EDITING, SENDING, FAILED, UNAVAILABLE }

/**
 * The appeal field's counter, "{n} / 1000", both numbers in the interface
 * language's digits with the region's grouping, as every count of the app
 * is written ([me.lakko.app.l10n.DeviceFormats.integer]).
 */
public fun appealCounterText(length: Int, words: java.util.Locale, region: java.util.Locale): String =
    me.lakko.app.l10n.DeviceFormats.integer(length.toLong(), words, region) + " / " +
        me.lakko.app.l10n.DeviceFormats.integer(APPEAL_MAX_CHARS.toLong(), words, region)

/** The appeal sheet. */
public data class AppealSheetState(val text: String = "", val phase: AppealPhase = AppealPhase.EDITING) {
    val length: Int get() = appealLength(text)
    val canSend: Boolean get() = phase != AppealPhase.SENDING && length in 1..APPEAL_MAX_CHARS
    public fun withText(next: String): AppealSheetState =
        copy(text = clampAppeal(next), phase = if (phase == AppealPhase.SENDING) phase else AppealPhase.EDITING)
}

/** What sending an appeal came to. */
public enum class AppealAnswer {
    /** Stored (`204`), or the server already holds one (`409 appeal-already-sent`). */
    SENT,

    /** The account is not suspended any more (`409 not-suspended`). */
    NOT_SUSPENDED,

    /** No moderator list served, or no pinned root in this build. */
    UNAVAILABLE,

    /** Anything else; the sheet says to try again. */
    FAILED,
}

/** The sheet phase after [answer]; `null` when the sheet closes. */
public fun appealPhaseAfter(answer: AppealAnswer): AppealPhase? = when (answer) {
    AppealAnswer.SENT, AppealAnswer.NOT_SUSPENDED -> null
    AppealAnswer.UNAVAILABLE -> AppealPhase.UNAVAILABLE
    AppealAnswer.FAILED -> AppealPhase.FAILED
}

// ---------------------------------------------------------------------------
// The mode
// ---------------------------------------------------------------------------

/** The details line of the suspended screen. */
public sealed interface SuspensionDetailsPhase {
    public data object Loading : SuspensionDetailsPhase
    public data class Loaded(val details: SuspensionDetails) : SuspensionDetailsPhase
    public data object Failed : SuspensionDetailsPhase
}

/** What the app root draws while suspended. */
public data class SuspensionUi(
    val accountIdB64: String,
    /** `true` after "Read my chats": the app read-only, the screen behind a
     *  row at the top of the chat list. */
    val reading: Boolean,
    /** Whether this device holds the account's chats (a fresh device does
     *  not: it shows the screen alone). */
    val canRead: Boolean,
    val details: SuspensionDetailsPhase,
)

/** The screen's second button (UX section 4). */
public enum class SuspendedSecondaryAction {
    /** The device holds the account's chats: open them read-only. */
    READ_CHATS,

    /** Nothing to read here (signed in afresh) and Settings cannot be
     *  reached: the usual Delete account confirmation instead. */
    DELETE_ACCOUNT,
}

/** "Read my chats" exactly when the chats can be opened, else "Delete account". */
public fun suspendedSecondaryAction(ui: SuspensionUi): SuspendedSecondaryAction =
    if (ui.canRead) SuspendedSecondaryAction.READ_CHATS else SuspendedSecondaryAction.DELETE_ACCOUNT

/** The answer of the refresh route to a suspended device. */
public sealed interface SuspensionRefresh {
    /** Still suspended: fresh credentials. */
    public data class Suspended(val credentials: SuspendedCredentials) : SuspensionRefresh

    /** No longer suspended: a normal pair. */
    public data class Resumed(val pair: SessionPair) : SuspensionRefresh

    /** The refresh route refused the credential (`401`): the session is gone. */
    public data class Refused(val refusal: SessionRefusal) : SuspensionRefresh

    /** Offline, 5xx: nothing learned. */
    public data object Unreachable : SuspensionRefresh
}

/** What the controller needs from the outside (the network, the disk). */
public interface SuspensionPorts {
    /** `POST /auth/session/refresh` with [refreshToken]. */
    public suspend fun refresh(refreshToken: String): SuspensionRefresh

    /** `GET /account/suspension` with the `s1`. */
    public suspend fun status(restrictedToken: String): SuspensionStatusAnswer

    /** Seal [text] to the moderators and `POST /account/suspension/appeal`. */
    public suspend fun appeal(restrictedToken: String, text: String): AppealAnswer

    /** Persist [credentials] for their account (survives a restart). */
    public fun save(credentials: SuspendedCredentials)

    /** Forget the suspension of [accountIdB64]. */
    public fun clear(accountIdB64: String)
}

/** What the controller tells the repository. */
public interface SuspensionEvents {
    /** New credentials (entering, or a probe answered still suspended):
     *  serve the `s1` / `r2` pair for the account. */
    public fun onCredentials(credentials: SuspendedCredentials)

    /** The suspension ended: [pair] is the account's normal session. */
    public fun onResumed(accountIdB64: String, pair: SessionPair)

    /** The refresh route refused the credential while suspended. */
    public fun onRefused(accountIdB64: String, refusal: SessionRefusal)
}

/**
 * The suspended mode of the one account this device serves. Pure over its
 * [ports] and [events]; every rule is pinned by `SuspensionControllerTest`:
 *
 *   * [enter] (a landing `403`, or a bearer `403` whose refresh answered the
 *     credentials) persists the credentials, hands them to the repository and
 *     shows the screen; entering again with newer credentials keeps whether
 *     the person was reading;
 *   * [probe] asks the refresh route: still suspended -> new credentials and
 *     the details again; a normal pair -> the mode ends silently;
 *   * [keepChecking] probes now and every [SUSPENSION_PROBE_INTERVAL_MS]
 *     until the mode ends (the caller runs it while the app is in front);
 *   * [sendAppeal] sends once: after `SENT` the screen shows the sent line
 *     and the Appeal button is gone.
 */
public class SuspensionController(
    private val ports: SuspensionPorts,
    private val events: SuspensionEvents,
    private val log: (String) -> Unit = {},
) {
    private val mutableUi: MutableStateFlow<SuspensionUi?> = MutableStateFlow(null)

    /** `null` while the account is not suspended. */
    public val ui: StateFlow<SuspensionUi?> = mutableUi.asStateFlow()

    @Volatile
    private var held: SuspendedCredentials? = null

    private val probeLock = Mutex()

    /** The credentials held now (`null` when not suspended). */
    public fun credentials(): SuspendedCredentials? = held

    /** Whether the mode is on. */
    public val active: Boolean get() = held != null

    /**
     * Enter (or stay in) suspended mode with [credentials]. [canRead]: this
     * device holds the account's chats. A device already suspended for the
     * same account keeps its reading flag and its details.
     */
    public fun enter(credentials: SuspendedCredentials, canRead: Boolean) {
        val before = mutableUi.value?.takeIf { it.accountIdB64 == credentials.accountIdB64 }
        held = credentials
        try {
            ports.save(credentials)
        } catch (cause: Throwable) {
            log("suspension record persist failed: ${cause.javaClass.simpleName}")
        }
        events.onCredentials(credentials)
        mutableUi.value = SuspensionUi(
            accountIdB64 = credentials.accountIdB64,
            reading = (before?.reading ?: false) && canRead,
            canRead = canRead,
            details = before?.details ?: SuspensionDetailsPhase.Loading,
        )
    }

    /** "Read my chats": the app read-only. A no-op where there is nothing to read. */
    public fun readChats() {
        mutableUi.value = mutableUi.value?.let { if (it.canRead) it.copy(reading = true) else it }
    }

    /** Back to the suspended screen (the row at the top of the chat list). */
    public fun showScreen() {
        mutableUi.value = mutableUi.value?.copy(reading = false)
    }

    /** Sign out, a wipe, an account change: forget the mode (and the record). */
    public fun clear() {
        val acct = held?.accountIdB64 ?: mutableUi.value?.accountIdB64
        held = null
        mutableUi.value = null
        if (acct != null) {
            try {
                ports.clear(acct)
            } catch (cause: Throwable) {
                log("suspension record delete failed: ${cause.javaClass.simpleName}")
            }
        }
    }

    /** Load the screen's details with the `s1`. */
    public suspend fun loadDetails() {
        val creds = held ?: return
        val current = mutableUi.value ?: return
        if (current.details is SuspensionDetailsPhase.Failed) {
            mutableUi.value = current.copy(details = SuspensionDetailsPhase.Loading)
        }
        val answer = try {
            ports.status(creds.restrictedToken)
        } catch (cause: CancellationException) {
            throw cause
        } catch (_: Throwable) {
            SuspensionStatusAnswer.Unreachable
        }
        if (held?.accountIdB64 != creds.accountIdB64) return
        when (answer) {
            is SuspensionStatusAnswer.Suspended -> update { it.copy(details = SuspensionDetailsPhase.Loaded(answer.details)) }
            // The suspension ended between the refresh and this read: the
            // refresh decides; probe to leave.
            SuspensionStatusAnswer.NotSuspended -> probe()
            SuspensionStatusAnswer.Unreachable -> update {
                if (it.details is SuspensionDetailsPhase.Loaded) it else it.copy(details = SuspensionDetailsPhase.Failed)
            }
        }
    }

    /**
     * Ask the refresh route whether the account is still suspended. Returns
     * `true` while the mode stays on.
     */
    public suspend fun probe(): Boolean = probeLock.withLock {
        val creds = held ?: return@withLock false
        val answer = try {
            ports.refresh(creds.refreshToken)
        } catch (cause: CancellationException) {
            throw cause
        } catch (_: Throwable) {
            SuspensionRefresh.Unreachable
        }
        if (held !== creds) return@withLock held != null
        when (answer) {
            is SuspensionRefresh.Suspended -> {
                val next = answer.credentials
                if (next.accountIdB64 != creds.accountIdB64) {
                    log("suspension probe answered another account; ignored")
                    return@withLock true
                }
                // The grant of the landing stays; a refresh never mints one.
                val merged = if (next.freshGrant == null && creds.freshGrant != null) {
                    SuspendedCredentials(
                        accountIdB64 = next.accountIdB64,
                        restrictedToken = next.restrictedToken,
                        restrictedExpiresAtMs = next.restrictedExpiresAtMs,
                        refreshToken = next.refreshToken,
                        refreshExpiresAtMs = next.refreshExpiresAtMs,
                        keyGeneration = next.keyGeneration,
                        freshGrant = creds.freshGrant,
                        freshGrantExpiresAtMs = creds.freshGrantExpiresAtMs,
                    )
                } else {
                    next
                }
                held = merged
                try {
                    ports.save(merged)
                } catch (cause: Throwable) {
                    log("suspension record persist failed: ${cause.javaClass.simpleName}")
                }
                events.onCredentials(merged)
                true
            }
            is SuspensionRefresh.Resumed -> {
                log("suspension ended")
                val acct = creds.accountIdB64
                held = null
                mutableUi.value = null
                try {
                    ports.clear(acct)
                } catch (cause: Throwable) {
                    log("suspension record delete failed: ${cause.javaClass.simpleName}")
                }
                events.onResumed(acct, answer.pair)
                false
            }
            is SuspensionRefresh.Refused -> {
                log("suspension probe: the refresh credential is not accepted")
                val acct = creds.accountIdB64
                held = null
                mutableUi.value = null
                try {
                    ports.clear(acct)
                } catch (cause: Throwable) {
                    log("suspension record delete failed: ${cause.javaClass.simpleName}")
                }
                events.onRefused(acct, answer.refusal)
                false
            }
            SuspensionRefresh.Unreachable -> true
        }
    }

    /**
     * Probe now, load the details, and again every
     * [SUSPENSION_PROBE_INTERVAL_MS] until the mode ends. Run while the app
     * is in front; cancelling it stops it.
     */
    public suspend fun keepChecking(wait: suspend (Long) -> Unit = { delay(it) }) {
        while (held != null) {
            if (!probe()) return
            loadDetails()
            if (held == null) return
            wait(SUSPENSION_PROBE_INTERVAL_MS)
        }
    }

    /** Send the one appeal. */
    public suspend fun sendAppeal(text: String): AppealAnswer {
        val creds = held ?: return AppealAnswer.NOT_SUSPENDED
        val length = appealLength(text)
        if (length < 1 || length > APPEAL_MAX_CHARS) return AppealAnswer.FAILED
        val sent = mutableUi.value?.details as? SuspensionDetailsPhase.Loaded
        if (sent != null && sent.details.appeal != AppealState.NONE) return AppealAnswer.SENT
        val answer = try {
            ports.appeal(creds.restrictedToken, text.trim())
        } catch (cause: CancellationException) {
            throw cause
        } catch (_: Throwable) {
            AppealAnswer.FAILED
        }
        when (answer) {
            AppealAnswer.SENT -> update { ui ->
                val details = (ui.details as? SuspensionDetailsPhase.Loaded)?.details
                    ?: SuspensionDetails(reasonName = null, untilMs = null, appeal = AppealState.NONE)
                ui.copy(details = SuspensionDetailsPhase.Loaded(details.copy(appeal = AppealState.SENT)))
            }
            AppealAnswer.NOT_SUSPENDED -> probe()
            AppealAnswer.UNAVAILABLE, AppealAnswer.FAILED -> Unit
        }
        return answer
    }

    private fun update(change: (SuspensionUi) -> SuspensionUi) {
        mutableUi.value = mutableUi.value?.let(change)
    }
}

/**
 * The one switch the transport reads: while it is on, nothing is written to
 * the Delivery Service or the channels service from this device (ADR-0098
 * section 4: clients honour the suspension; the DS does not know accounts).
 * The UI reads [readOnly] to hide the composer, calls and adding contacts.
 */
public object SuspensionGate {
    private val mutableReadOnly: MutableStateFlow<Boolean> = MutableStateFlow(false)

    /** `true` while this device's account is suspended. */
    public val readOnly: StateFlow<Boolean> = mutableReadOnly.asStateFlow()

    public fun set(suspended: Boolean) {
        mutableReadOnly.value = suspended
    }

    /**
     * Whether a request is refused before it leaves the device: any method
     * other than GET / HEAD to a base URL in [blockedBases], while suspended.
     */
    public fun refuses(method: String, url: String, blockedBases: List<String>): Boolean {
        if (!mutableReadOnly.value) return false
        if (method.equals("GET", ignoreCase = true) || method.equals("HEAD", ignoreCase = true)) return false
        return blockedBases.any { base -> base.isNotEmpty() && url.startsWith(base.trimEnd('/')) }
    }
}

/** A write the suspension gate refused before it left the device. An
 *  [java.io.IOException], so every client treats it as a request that could
 *  not be made. */
public class SuspendedWriteRefused : java.io.IOException("refused while the account is suspended")
