// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import kotlinx.coroutines.CancellationException
import me.lakko.app.auth.AuthClientArgError
import me.lakko.app.auth.DeviceAuthorizeRedeemResult
import me.lakko.app.auth.LoginHandleSource
import me.lakko.app.auth.NamedHandle
import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.auth.namedHandleOrRefuse
import me.lakko.app.l10n.UiText
import uniffi.lakko_ffi_uniffi.DecodedPairingQr
import uniffi.lakko_ffi_uniffi.FfiException

/*
 * ADR-0056 - a fresh device names its account by the login handle of the
 * username the person types, never by account id. Nothing before a session
 * carries the account id: the pairing payload holds the handle, the confirming
 * device checks it against the handle of its own username, and the fresh
 * device learns its account id from the grant redeem response.
 *
 * The decisions here take their native dependencies as parameters so the host
 * JVM tests run without the native library.
 */

/** Shown in place of the pairing affordances on an account with no username
 *  (a guest): neither side of a pairing can name the account. */
public val PAIRING_NO_USERNAME_COPY: UiText = UiText("device_link.pairing.needs_username")

/** A scanned code whose login handle is not this account's. */
public val PAIRING_WRONG_ACCOUNT_COPY: UiText = UiText("device_link.pairing.wrong_account")

/** A code from a different pairing version (the other device runs another Lakko version). */
public val PAIRING_UNSUPPORTED_COPY: UiText = UiText("device_link.pairing.unsupported")

/** Any other refusal of a scanned code (a payload version this build does not
 *  read, a malformed payload). */
public val PAIRING_FAILED_COPY: UiText = UiText("device_link.request.error.generic")

/** The typed-nothing refusal on the fresh-device side. */
public const val PAIRING_NO_NAME_COPY: String = "Enter your username first."

/**
 * The name a fresh device's pairing payload carries: the [typed] name when
 * there is one, else [fallbackUsername] (the account cached on this device).
 *
 * @throws AuthClientArgError with [PAIRING_NO_NAME_COPY] when there is neither,
 *   or with the username rule's own copy when the name fails the rule.
 */
public fun pairingNameFor(
    typed: String,
    fallbackUsername: String?,
    source: LoginHandleSource,
): NamedHandle {
    val trimmed = typed.trim()
    val name = when {
        trimmed.isNotEmpty() -> trimmed
        !fallbackUsername.isNullOrBlank() -> fallbackUsername
        else -> throw AuthClientArgError(PAIRING_NO_NAME_COPY, UiText("recovery.username_required"))
    }
    return namedHandleOrRefuse(source, name)
}

/**
 * The login handle of this account's own [username], or `null` for an account
 * with no username (a guest) or a stored name the rule refuses: such an account
 * can neither show nor confirm a pairing code.
 */
public fun ownPairingHandle(username: String?, source: LoginHandleSource): ByteArray? {
    if (username.isNullOrBlank()) return null
    return try {
        source.handleFor(username).handle
    } catch (e: CancellationException) {
        throw e
    } catch (_: Exception) {
        null
    }
}

/** What a confirming device makes of a scanned code. */
public sealed interface PairingScanDecision {
    /** The code is for this account; confirm, then run the scan flow. */
    public class Accepted(public val decoded: DecodedPairingQr) : PairingScanDecision

    /** Refused before any MLS commit; [text] is what the scanner shows. */
    public data class Refused(public val text: UiText) : PairingScanDecision
}

/**
 * Decode a scanned pairing payload against the handle of this account's own
 * username. [decode] is `parsePairingQr(wire, expectedLoginHandle)`.
 *
 * A handle mismatch (the core's `Group` error, "login_handle mismatch") shows
 * [PAIRING_WRONG_ACCOUNT_COPY]; an account with no username shows
 * [PAIRING_NO_USERNAME_COPY]; everything else (a version other than 2, a
 * malformed payload) shows [PAIRING_FAILED_COPY].
 */
public fun decodeForOwnAccount(
    scanned: ByteArray,
    ownUsername: String?,
    source: LoginHandleSource,
    decode: (wire: ByteArray, expectedLoginHandle: ByteArray) -> DecodedPairingQr,
): PairingScanDecision {
    val handle = ownPairingHandle(ownUsername, source)
        ?: return PairingScanDecision.Refused(PAIRING_NO_USERNAME_COPY)
    return try {
        PairingScanDecision.Accepted(decode(scanned, handle))
    } catch (e: CancellationException) {
        throw e
    } catch (e: FfiException.Group) {
        if (e.message.orEmpty().contains(LOGIN_HANDLE_MISMATCH)) {
            PairingScanDecision.Refused(PAIRING_WRONG_ACCOUNT_COPY)
        } else {
            PairingScanDecision.Refused(PAIRING_FAILED_COPY)
        }
    } catch (_: FfiException.Unsupported) {
        PairingScanDecision.Refused(PAIRING_UNSUPPORTED_COPY)
    } catch (_: Exception) {
        PairingScanDecision.Refused(PAIRING_FAILED_COPY)
    } finally {
        handle.fill(0)
    }
}

/** The core's error body for a payload made for another account's name. */
internal const val LOGIN_HANDLE_MISMATCH: String = "login_handle mismatch"

/** Result of [completeJoinByName]. */
public sealed interface JoinByNameResult {
    /** The grant redeemed and the joined group is that account's DeviceGroup. */
    public class Joined(
        public val accountId: ByteArray,
        /** ADR-0073: the pair the redeem issued. */
        public val session: me.lakko.app.auth.SessionPair,
    ) : JoinByNameResult {
        public val sessionToken: String get() = session.accessToken

        public val sessionExpiresAtMs: Long get() = session.accessExpiresAtMs
    }

    /** The redeem failed (expired, used, network): no account id, no session. */
    public data object RedeemFailed : JoinByNameResult
}

/**
 * The fresh device's last step before it knows its account: redeem the grant
 * (the request carries the grant only). Nothing is persisted before this
 * returns [JoinByNameResult.Joined].
 *
 * ADR-0078 section 6: the DeviceGroup id is derived from the account seed,
 * which this device does not hold yet, so the group it joined cannot be
 * checked against the redeemed account by id. The binding is the recovery
 * row sealed under the joined group's exporter, and the restore checks the
 * row's phrase against the published identity before anything is persisted.
 *
 * @param redeem `POST /device/authorize/redeem` with the base64url grant.
 */
public suspend fun completeJoinByName(
    grantToken: ByteArray,
    redeem: suspend (grantTokenB64: String) -> DeviceAuthorizeRedeemResult,
): JoinByNameResult {
    val redeemed = try {
        redeem(base64UrlEncodeNoPad(grantToken))
    } catch (e: CancellationException) {
        throw e
    } catch (_: Exception) {
        return JoinByNameResult.RedeemFailed
    }
    return JoinByNameResult.Joined(
        accountId = redeemed.accountId,
        session = redeemed.session,
    )
}

/**
 * The native and network steps of a join by name, in the order
 * [runJoinByName] calls them. The production implementation sits in
 * [PairingOrchestrator.JoinSession] over the plain-`Group` join; host tests
 * pass a fake.
 */
public interface JoinByNameSteps {
    /** The scanner's sealed recovery row, or `null` when it never came. */
    public suspend fun pollRecoveryRow(): ByteArray?

    /** Open the row under the joined group's exporter key and parse it. */
    public fun openRecoveryRow(sealed: ByteArray): RecoveryPayload.ParsedRecoveryPayload

    /** The MLS group id of the group the Welcome joined. */
    public fun joinedGroupId(): ByteArray

    /** `POST /device/authorize/redeem` with the base64url grant only. */
    public suspend fun redeem(grantTokenB64: String): DeviceAuthorizeRedeemResult

    /** Bind the joined group (under its own id, [joinedGroupId]) to
     *  [accountId] and fetch the history and state bundles, all in memory:
     *  NOTHING is written. Called only after the redeem named the account.
     *  Throws [JoinWrongAccount] when the joined group is not in the
     *  identity's storage. The returned [PendingJoin] is the one place the
     *  join reaches the disk. */
    public suspend fun bindAs(accountId: ByteArray): PendingJoin
}

/**
 * ADR-0056 security pass — what a join by name received, held until the
 * restore verified the transferred keys against the published identity and
 * checked the typed name at discovery. [persist] writes the DeviceIdentity
 * seed, the DeviceGroup snapshot, the history rows and the state bundle
 * under the redeemed account; never calling it leaves the device as it was.
 */
public interface PendingJoin {
    /** Write everything held. Runs once; each write is best-effort. */
    public suspend fun persist()
}

/**
 * The ordering of a join by name (ADR-0056): open the recovery row, redeem the
 * grant, and bind the joined group to the account the redeem named, in
 * memory (ADR-0078 section 6: by the group's own id; nothing derives the
 * DeviceGroup id from the account id any more).
 *
 * Nothing is persisted here (ADR-0056 security pass): the returned outcome
 * carries [PairingOrchestrator.JoinRecoveryOutcome.persistJoin], which the
 * restore runs only after the transferred keys verified and the typed name
 * resolved at discovery to the redeemed account ([runCheckedDeviceRestore]).
 *
 * @return `null` when the recovery row never came; an outcome with no
 *   session and no account id when the redeem failed (the caller refuses a
 *   keys-only restore); the full outcome, whose `persistJoin` writes what
 *   [JoinByNameSteps.bindAs] held.
 * @throws JoinWrongAccount when the joined group is not in the identity's
 *   storage.
 */
public suspend fun runJoinByName(steps: JoinByNameSteps): PairingOrchestrator.JoinRecoveryOutcome? {
    val sealed = steps.pollRecoveryRow() ?: return null
    val parsed = steps.openRecoveryRow(sealed)
    var phraseEntropy: ByteArray? = parsed.phraseEntropy
    try {
        val result = completeJoinByName(
            grantToken = parsed.grantToken,
            redeem = steps::redeem,
        )
        val outcome = when (result) {
            // No account id, so nothing can be persisted: the caller refuses
            // a keys-only outcome and the person retries with a fresh code.
            JoinByNameResult.RedeemFailed ->
                PairingOrchestrator.JoinRecoveryOutcome(parsed.phraseEntropy, null, null)
            is JoinByNameResult.Joined -> {
                val pending = steps.bindAs(result.accountId)
                PairingOrchestrator.JoinRecoveryOutcome(
                    phraseEntropy = parsed.phraseEntropy,
                    session = result.session,
                    accountId = result.accountId,
                    persistJoin = { pending.persist() },
                )
            }
        }
        phraseEntropy = null
        return outcome
    } finally {
        phraseEntropy?.fill(0)
        parsed.grantToken.fill(0)
    }
}

/** The fresh device's refusal when the group it joined is not in its
 *  storage to bind to the redeemed account. */
public const val JOIN_WRONG_ACCOUNT_COPY: String =
    "Could not confirm the account on the other device. Try again."

/**
 * The fresh device's refusal when the group it joined is not in its storage
 * to bind to the redeemed account. Its own kind so the screen renders it from the catalog
 * (`device_link.error.wrong_account`, ADR-0061 section 6) rather than from
 * this English [message], which stays for logs only. Still an
 * [IllegalStateException] for callers that catch that.
 */
public class JoinWrongAccount : IllegalStateException(JOIN_WRONG_ACCOUNT_COPY)
