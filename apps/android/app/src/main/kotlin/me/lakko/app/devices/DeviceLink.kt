// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import me.lakko.app.l10n.UiText

/*
 * ADR-0049 section 2, the device link (amended 2026-09-28) — "confirm on
 * another device" without a camera. `docs/ux/identity-recovery.md` section 4.
 *
 * The OLD (signed-in) device chooses a six-digit code and shows it; the
 * person types it on the NEW device; the two agree on a key from it with
 * SPAKE2 (`DeviceLinkResponder` / `DeviceLinkInitiator` in the core). The new
 * device's pairing payload (the QR bytes) travels sealed under that key with
 * a key confirmation. The server relays opaque values and never sees the
 * code, and it allows ONE exchange per request, so a relay gets one guess.
 *
 * Both halves are written here against ports ([DeviceLinkApi], the two
 * crypto ports, [DeviceLinkAnswerLimit]) so every step runs on the host JVM;
 * the Compose screens only render the states.
 */

/** The six-digit code does not match (FFI `DeviceLinkWrongCode`). */
public class DeviceLinkWrongCode(message: String = "device link code does not match") : Exception(message)

/** The typed code is not six digits (FFI `DeviceLink` `invalid-code:`):
 *  nothing is spent, the person may retype. */
public class DeviceLinkInvalidCode(message: String = "not six digits") : Exception(message)

/** One open request as the old device reads it. */
public class PendingDeviceRequest(
    public val requestId: ByteArray,
    public val pakeB: ByteArray?,
    public val pakeA: ByteArray?,
    public val sealedPayload: ByteArray?,
    public val confirmA: ByteArray?,
    public val outcome: String?,
) {
    public val idKey: String get() = requestId.joinToString("") { "%02x".format(it) }
}

/** Where the new device's request stands. */
public class DeviceRequestStatus(
    public val pending: Boolean,
    public val pakeB: ByteArray?,
    public val outcome: String?,
    public val confirmB: ByteArray?,
)

/** How the old device's answer landed. */
public enum class DeviceLinkAnswer { ANSWERED, ALREADY_ANSWERED, NOT_FOUND }

/** The auth routes of the device link. */
public interface DeviceLinkApi {
    // New device (anonymous).
    public suspend fun createRequest(handle: ByteArray): ByteArray

    public suspend fun status(requestId: ByteArray): DeviceRequestStatus

    public suspend fun postPayload(requestId: ByteArray, pakeA: ByteArray, sealed: ByteArray, confirmA: ByteArray)

    // Old device (bearer).
    public suspend fun list(): PendingDeviceRequest?

    public suspend fun answer(requestId: ByteArray, pakeB: ByteArray): DeviceLinkAnswer

    public suspend fun postResult(requestId: ByteArray, accepted: Boolean, confirmB: ByteArray?)

    public suspend fun decline(requestId: ByteArray)
}

/** The new device's half of the SPAKE2 exchange (`DeviceLinkInitiator`). */
public interface DeviceLinkInitiatorPort {
    /** Throws [DeviceLinkInvalidCode] for a code that is not six digits. */
    public fun finish(pakeB: ByteArray, typedCode: String, pairingPayload: ByteArray): SealedLink

    /** Throws [DeviceLinkWrongCode] when [confirmB] does not verify. */
    public fun verifyConfirmation(confirmB: ByteArray)
}

/** What the new device posts. */
public class SealedLink(public val pakeA: ByteArray, public val sealedPayload: ByteArray, public val confirmA: ByteArray)

/** The old device's half (`DeviceLinkResponder`). */
public interface DeviceLinkResponderPort {
    public val code: String
    public val pakeMessage: ByteArray

    /** Throws [DeviceLinkWrongCode] (or anything else) when the new device's
     *  values do not verify; runs once. */
    public fun accept(pakeA: ByteArray, sealedPayload: ByteArray, confirmA: ByteArray): AcceptedLink
}

/** What the old device opens. */
public class AcceptedLink(public val pairingPayload: ByteArray, public val confirmB: ByteArray)

// -----------------------------------------------------------------
// The new device
// -----------------------------------------------------------------

/** The new device's states (spec section 4). */
public sealed interface NewDeviceLinkState {
    /** Before the other device answered: the field is disabled. */
    public data object WaitingForOtherDevice : NewDeviceLinkState

    /** The other device answered: the six-digit field is enabled. */
    public data object EnterCode : NewDeviceLinkState

    /** After Continue. */
    public data object Checking : NewDeviceLinkState

    /** One try per request; Start again posts a new request. */
    public data object WrongCode : NewDeviceLinkState

    /** The confirmation verified: trust the Welcome that follows. */
    public data object Confirmed : NewDeviceLinkState

    /** The other device said "It is not me" (or the request vanished). */
    public data object Declined : NewDeviceLinkState

    /** 10 minutes passed. */
    public data object Expired : NewDeviceLinkState
}

/** Copy, spec section 4 (new device), as catalog messages (ADR-0061). */
public val DEVICE_LINK_NEW_TITLE: UiText = UiText("device_link.code.title")
public val DEVICE_LINK_NEW_LEAD: UiText = UiText("device_link.code.intro_native")
public val DEVICE_LINK_WAITING_COPY: UiText = UiText("device_link.code.waiting")
public val DEVICE_LINK_WRONG_CODE_COPY: UiText = UiText("device_link.code.wrong")
public val DEVICE_LINK_RESTORING_COPY: UiText = UiText("device_link.code.restoring")
public val DEVICE_LINK_DECLINED_COPY: UiText = UiText("device_link.error.declined")
public val DEVICE_LINK_EXPIRED_COPY: UiText = UiText("device_link.code.expired")
public val DEVICE_LINK_SHOW_QR: UiText = UiText("device_link.qr.show")

/** A request lives this long on the server (`REQUEST_TTL_SECS`). */
public const val DEVICE_LINK_REQUEST_TTL_MS: Long = 600_000L

/**
 * The new device's exchange for ONE request. [start] opens it, [awaitAnswer]
 * waits for the old device's SPAKE2 message, [submitCode] runs the exchange
 * with the typed code and waits for the outcome. A wrong code is final for
 * this request; the person starts again with a new instance.
 */
public class NewDeviceLink(
    private val api: DeviceLinkApi,
    private val initiatorFor: (requestId: ByteArray) -> DeviceLinkInitiatorPort,
    /** The pairing payload, built when the code is sealed (after it is
     *  typed), so the card's place has had time to arrive (UX
     *  second-device.md section 2.1). */
    private val payloadForSeal: suspend () -> ByteArray,
    private val nowMs: () -> Long = System::currentTimeMillis,
    private val pollIntervalMs: Long = DEFAULT_POLL_MS,
) {
    /** A payload fixed in advance. */
    public constructor(
        api: DeviceLinkApi,
        initiatorFor: (requestId: ByteArray) -> DeviceLinkInitiatorPort,
        pairingPayload: ByteArray,
        nowMs: () -> Long = System::currentTimeMillis,
        pollIntervalMs: Long = DEFAULT_POLL_MS,
    ) : this(api, initiatorFor, { pairingPayload }, nowMs, pollIntervalMs)

    private var requestId: ByteArray? = null
    private var initiator: DeviceLinkInitiatorPort? = null
    private var pakeB: ByteArray? = null
    private var startedAtMs: Long = 0L

    /** Post the request for the account named by [handle]. */
    public suspend fun start(handle: ByteArray) {
        val id = api.createRequest(handle)
        requestId = id
        initiator = initiatorFor(id)
        startedAtMs = nowMs()
    }

    private fun expired(): Boolean = nowMs() - startedAtMs >= DEVICE_LINK_REQUEST_TTL_MS

    /** Poll until the old device posted `pake_b` ([NewDeviceLinkState.EnterCode]),
     *  or the request is gone ([NewDeviceLinkState.Declined]) or expired. */
    public suspend fun awaitAnswer(): NewDeviceLinkState {
        val id = requireNotNull(requestId) { "start first" }
        while (true) {
            if (expired()) return NewDeviceLinkState.Expired
            val status = api.status(id)
            if (!status.pending) return NewDeviceLinkState.Declined
            val b = status.pakeB
            if (b != null) {
                pakeB = b
                return NewDeviceLinkState.EnterCode
            }
            delay(pollIntervalMs)
        }
    }

    /**
     * Run the exchange with [typedCode] and post it, then poll for the old
     * device's decision. `accepted` is [NewDeviceLinkState.Confirmed] ONLY
     * when its key confirmation verifies — otherwise whoever answered did not
     * hold the code, and it is [NewDeviceLinkState.WrongCode]. Throws
     * [DeviceLinkInvalidCode] for a code that is not six digits (nothing
     * posted; retype).
     */
    public suspend fun submitCode(typedCode: String): NewDeviceLinkState {
        val id = requireNotNull(requestId) { "start first" }
        val b = requireNotNull(pakeB) { "await the answer first" }
        val init = requireNotNull(initiator)
        val sealed = init.finish(b, typedCode, payloadForSeal())
        api.postPayload(id, sealed.pakeA, sealed.sealedPayload, sealed.confirmA)
        while (true) {
            if (expired()) return NewDeviceLinkState.Expired
            val status = api.status(id)
            if (!status.pending) return NewDeviceLinkState.Declined
            when (status.outcome) {
                OUTCOME_ACCEPTED -> {
                    val confirmB = status.confirmB ?: return NewDeviceLinkState.WrongCode
                    return try {
                        init.verifyConfirmation(confirmB)
                        NewDeviceLinkState.Confirmed
                    } catch (cancellation: CancellationException) {
                        throw cancellation
                    } catch (_: Exception) {
                        NewDeviceLinkState.WrongCode
                    }
                }
                OUTCOME_WRONG_CODE -> return NewDeviceLinkState.WrongCode
            }
            delay(pollIntervalMs)
        }
    }

    public companion object {
        /** Status poll cadence. Chosen, not measured. */
        public const val DEFAULT_POLL_MS: Long = 2_000L
    }
}

// -----------------------------------------------------------------
// The old device
// -----------------------------------------------------------------

/** Copy, spec section 4 (old device) and the brief's answer limit, as
 *  catalog messages (ADR-0061). The consent step is two lines: the lead
 *  and the question. */
public val DEVICE_LINK_OLD_TITLE: UiText = UiText("device_link.request.heading")
public val DEVICE_LINK_CONSENT_LEAD: UiText = UiText("device_link.request.someone_signing_in")
public val DEVICE_LINK_CONSENT_QUESTION: UiText = UiText("device_link.request.title")
public val DEVICE_LINK_YES_ITS_ME: UiText = UiText("device_link.request.yes_its_me")
public val DEVICE_LINK_OLD_LEAD: UiText = UiText("device_link.request.enter_code")
public val DEVICE_LINK_OLD_WARNING: UiText = UiText("device_link.request.never_share")
public val DEVICE_LINK_NOT_ME: UiText = UiText("device_link.request.decline")
public val DEVICE_LINK_ADDING_COPY: UiText = UiText("device_link.request.adding")
public val DEVICE_LINK_OLD_WRONG_CODE_COPY: UiText = UiText("device_link.request.wrong_code")
public val DEVICE_LINK_OLD_DECLINED_COPY: UiText = UiText("device_link.request.declined")
public val DEVICE_LINK_HANDLED_ELSEWHERE_COPY: UiText = UiText("device_link.request.handled_elsewhere")
public val DEVICE_LINK_LIMIT_COPY: UiText = UiText("device_link.request.too_many")
public val DEVICE_LINK_ADDED_COPY: UiText = UiText("device_link.request.added")
public val DEVICE_LINK_ADD_FAILED_COPY: UiText = UiText("device_link.request.error.generic")
public val DEVICE_LINK_REQUEST_EXPIRED_COPY: UiText = UiText("device_link.request.expired")

/** What the old device's modal shows. */
public sealed interface OldDeviceLinkState {
    /** No modal. */
    public data object Idle : OldDeviceLinkState

    /** Step 1 (second security pass, finding 6): "Is this you?". Nothing has
     *  been sent for the request; the code exists only after "Yes, it's me". */
    public data object AskConsent : OldDeviceLinkState

    /** The code, formatted `47 19 03`, shown only after the answer landed. */
    public data class ShowCode(val formattedCode: String) : OldDeviceLinkState

    /** The new device entered the right code; pairing runs. */
    public data object Adding : OldDeviceLinkState

    /** A closing notice with one sentence (and Close). */
    public data class Notice(val message: UiText) : OldDeviceLinkState

    /** The fresh-auth proof ran out between two steps of an answered request
     *  (UX 17.4): the proof block is shown again and the request is kept;
     *  [OldDeviceLink.resume] continues where it stopped. */
    public data object NeedsProof : OldDeviceLinkState
}

/**
 * Obligation of the brief — an old device answers at most [MAX_ANSWERS]
 * requests per [WINDOW_MS] (chosen, not measured), persisted per device, so a
 * stream of requests cannot farm one guess each out of it forever.
 *
 * An answer is given back ([refund]) when it bought no guess at the code (no
 * code was shown) or when the code was RIGHT and the device was then not
 * added for a reason on this side (the result or the pairing failed): a right
 * code is not a guess, and the person must not run out of tries because of
 * our own failure. A wrong code is never given back.
 */
public class DeviceLinkAnswerLimit(
    private val load: () -> List<Long>,
    private val save: (List<Long>) -> Unit,
    private val nowMs: () -> Long = System::currentTimeMillis,
) {
    /** Record one answer if the budget allows and return its time, or `null`
     *  when the budget is spent. */
    @Synchronized
    public fun tryConsume(): Long? {
        val now = nowMs()
        val recent = load().filter { now - it in 0 until WINDOW_MS }
        if (recent.size >= MAX_ANSWERS) {
            save(recent)
            return null
        }
        save(recent + now)
        return now
    }

    /** Give back the answer recorded at [at] (one entry, if still kept). */
    @Synchronized
    public fun refund(at: Long) {
        val times = load().toMutableList()
        if (times.remove(at)) save(times)
    }

    public companion object {
        public const val MAX_ANSWERS: Int = 5
        public const val WINDOW_MS: Long = 24L * 60L * 60L * 1000L
    }
}

/**
 * The old device's side: [poll] once per tick while the app is in the
 * foreground. Holds the responder of the request it answered in memory; a
 * responder lost (process death) cannot be recovered, and that request can
 * then only be declined.
 *
 * Consent first (the second security pass, finding 6; UX spec section 4 "On
 * the old device"): a new request first asks "Is this you?" ([OldDeviceLinkState.AskConsent])
 * and this device sends NOTHING for it — no responder is made, no `pake_b`
 * posted — until [confirm] ("Yes, it's me"). [decline] ("It is not me")
 * deletes the request; [dismiss] closes the question and sends nothing (the
 * request expires). The daily answer budget counts answers after "Yes".
 *
 * @param responderFor builds `DeviceLinkResponder(requestId, ownLoginHandle)`.
 * @param formatCode `deviceLinkFormatCode`.
 * @param openPayload parses the opened pairing payload against this
 *   account's own login handle (`parsePairingQr`); throws when it names
 *   another account or is malformed.
 * @param pair runs the unchanged add-device pairing with the parsed payload.
 * @param addedCopy the closing line once the device is added, naming it from
 *   the card its payload carried (UX second-device.md section 2.1).
 */
public class OldDeviceLink<P>(
    private val api: DeviceLinkApi,
    private val responderFor: (requestId: ByteArray) -> DeviceLinkResponderPort,
    private val formatCode: (String) -> String,
    private val openPayload: (ByteArray) -> P,
    private val pair: suspend (P) -> Unit,
    private val limit: DeviceLinkAnswerLimit,
    private val addedCopy: (P) -> UiText = { DEVICE_LINK_ADDED_COPY },
) {
    private var answeredId: String? = null
    private var responder: DeviceLinkResponderPort? = null
    private val settled = HashSet<String>()

    /** The request "Is this you?" is asked about; nothing sent for it yet. */
    private var asking: PendingDeviceRequest? = null

    /** Requests the person said "Yes" to whose answer has not landed yet (an
     *  offline answer is retried on the next poll without asking again). */
    private val consented = HashSet<String>()

    /** What is still owed for a request whose code was right when the proof
     *  ran out mid-flow: the result (unless already posted) and the pairing. */
    private class Owed<P>(
        val requestId: ByteArray,
        val confirmB: ByteArray,
        val payload: P,
        var resultPosted: Boolean,
        /** When the answer was counted ([DeviceLinkAnswerLimit.refund]). */
        val answeredAt: Long?,
    )

    /** When the answer to the request this device answered was counted. */
    private var answeredAt: Long? = null

    /** Requests consented to, with when their answer was counted. */
    private val consentedAt = HashMap<String, Long>()

    private var owed: Owed<P>? = null

    /** The person started on the proof block of the asked request (typed, or
     *  opened the passkey sheet): a request that disappears now says so. */
    private var engaged: Boolean = false

    /** The host reports the person is working on the proof block. */
    public fun noteEngaged() {
        if (state is OldDeviceLinkState.AskConsent) engaged = true
    }

    private val mutableStates = kotlinx.coroutines.flow.MutableStateFlow<OldDeviceLinkState>(OldDeviceLinkState.Idle)

    /** The modal state as it moves (the UI collects it; [OldDeviceLinkState.Adding]
     *  is visible while the pairing runs). */
    public val states: kotlinx.coroutines.flow.StateFlow<OldDeviceLinkState> = mutableStates

    /** The current modal state. */
    public var state: OldDeviceLinkState
        get() = mutableStates.value
        private set(value) {
            mutableStates.value = value
        }

    /** One poll: read the open request and move the modal. */
    public suspend fun poll(): OldDeviceLinkState {
        val request = api.list()
        owed?.let { o ->
            // Waiting for the proof block: nothing moves until [resume],
            // unless the request is gone before its result was posted.
            if (request == null && !o.resultPosted) {
                owed = null
                state = OldDeviceLinkState.Notice(DEVICE_LINK_REQUEST_EXPIRED_COPY)
            }
            return state
        }
        if (request == null) {
            // Gone: expired or deleted. A modal still asking or showing a code
            // closes; one the person was typing a proof into says why (UX
            // 17.4).
            if (state is OldDeviceLinkState.AskConsent && engaged) {
                state = OldDeviceLinkState.Notice(DEVICE_LINK_REQUEST_EXPIRED_COPY)
            } else if (state is OldDeviceLinkState.ShowCode || state is OldDeviceLinkState.AskConsent) {
                state = OldDeviceLinkState.Idle
            }
            engaged = false
            answeredId = null
            responder = null
            asking = null
            dropConsented()
            return state
        }
        val key = request.idKey
        if (key in settled) return state
        if (answeredId != key) {
            // A request this device has not answered.
            if (request.pakeB != null) {
                // Answered elsewhere (or by a responder this process lost).
                if (asking?.idKey == key) {
                    asking = null
                    state = OldDeviceLinkState.Idle
                }
                return state
            }
            if (key in consented) return answer(request)
            if (asking?.idKey != key) {
                // A new request (or a newer one replaced the asked one): ask,
                // and send nothing.
                asking = request
                dropConsented()
                engaged = false
                state = OldDeviceLinkState.AskConsent
            }
            return state
        }
        // Ours: wait for the new device's values.
        val pakeA = request.pakeA
        val sealed = request.sealedPayload
        val confirmA = request.confirmA
        if (pakeA == null || sealed == null || confirmA == null || request.outcome != null) return state
        val r = responder ?: return state
        settled.add(key)
        val opened: Pair<AcceptedLink, P>? = try {
            val accepted = r.accept(pakeA, sealed, confirmA)
            accepted to openPayload(accepted.pairingPayload)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Exception) {
            null
        }
        if (opened == null) {
            runCatching { api.postResult(request.requestId, accepted = false, confirmB = null) }
            state = OldDeviceLinkState.Notice(DEVICE_LINK_OLD_WRONG_CODE_COPY)
            return state
        }
        val pending = Owed(request.requestId, opened.first.confirmB, opened.second, resultPosted = false, answeredAt)
        answeredAt = null
        return finish(pending)
    }

    /** Post the result (once) and run the pairing. A proof that ran out
     *  between the steps parks the rest in [owed] and shows the block again;
     *  nothing of the request is lost. */
    private suspend fun finish(pending: Owed<P>): OldDeviceLinkState {
        if (!pending.resultPosted) {
            try {
                api.postResult(pending.requestId, accepted = true, confirmB = pending.confirmB)
            } catch (_: me.lakko.app.auth.FreshAuthRequired) {
                owed = pending
                state = OldDeviceLinkState.NeedsProof
                return state
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (e: Exception) {
                // The code was right; the device is not added because the
                // result did not land. Not a guess: given back.
                giveBack(pending)
                throw e
            }
            pending.resultPosted = true
        }
        state = OldDeviceLinkState.Adding
        state = try {
            pair(pending.payload)
            owed = null
            OldDeviceLinkState.Notice(addedCopy(pending.payload))
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: me.lakko.app.auth.FreshAuthRequired) {
            owed = pending
            OldDeviceLinkState.NeedsProof
        } catch (_: Exception) {
            owed = null
            giveBack(pending)
            OldDeviceLinkState.Notice(DEVICE_LINK_ADD_FAILED_COPY)
        }
        return state
    }

    /** Forget the consented, unanswered requests; no code was shown for
     *  them, so their answers are given back. */
    private fun dropConsented() {
        consented.clear()
        consentedAt.values.forEach(limit::refund)
        consentedAt.clear()
    }

    private fun giveBack(pending: Owed<P>) {
        pending.answeredAt?.let(limit::refund)
    }

    /** The proof block of [OldDeviceLinkState.NeedsProof] was satisfied:
     *  continue with what is owed. */
    public suspend fun resume(): OldDeviceLinkState {
        val pending = owed ?: return state
        owed = null
        return finish(pending)
    }

    /**
     * "Yes, it's me" on the asked request: only now is the answer budget
     * spent, the responder made and `pake_b` posted. A network failure leaves
     * the request consented, and the next [poll] answers it without asking
     * again.
     */
    public suspend fun confirm(): OldDeviceLinkState {
        val request = asking ?: return state
        asking = null
        engaged = false
        val key = request.idKey
        val at = limit.tryConsume()
        if (at == null) {
            settled.add(key)
            runCatching { api.decline(request.requestId) }
            state = OldDeviceLinkState.Notice(DEVICE_LINK_LIMIT_COPY)
            return state
        }
        consented.add(key)
        consentedAt[key] = at
        return answer(request)
    }

    /** Make the responder and post `pake_b` for a request the person
     *  consented to. */
    private suspend fun answer(request: PendingDeviceRequest): OldDeviceLinkState {
        val key = request.idKey
        val r = responderFor(request.requestId)
        val answered = try {
            api.answer(request.requestId, r.pakeMessage)
        } catch (_: me.lakko.app.auth.FreshAuthRequired) {
            // No live proof (it ran out between the block and this call):
            // nothing was sent; ask again, with the block. "Yes" counts again.
            consented.remove(key)
            consentedAt.remove(key)?.let(limit::refund)
            asking = request
            state = OldDeviceLinkState.AskConsent
            return state
        }
        when (answered) {
            DeviceLinkAnswer.ANSWERED -> {
                consented.remove(key)
                answeredAt = consentedAt.remove(key)
                answeredId = key
                responder = r
                state = OldDeviceLinkState.ShowCode(formatCode(r.code))
            }
            DeviceLinkAnswer.ALREADY_ANSWERED -> {
                // No code was shown: not a guess, given back.
                consented.remove(key)
                consentedAt.remove(key)?.let(limit::refund)
                settled.add(key)
                state = OldDeviceLinkState.Notice(DEVICE_LINK_HANDLED_ELSEWHERE_COPY)
            }
            DeviceLinkAnswer.NOT_FOUND -> {
                consented.remove(key)
                consentedAt.remove(key)?.let(limit::refund)
                state = OldDeviceLinkState.Idle
            }
        }
        return state
    }

    /** "It is not me" (on the question or on the code): delete the request;
     *  the modal says so. */
    public suspend fun decline(): OldDeviceLinkState {
        val asked = asking
        if (asked != null) {
            asking = null
            settled.add(asked.idKey)
            runCatching { api.decline(asked.requestId) }
        }
        val id = answeredId
        val requestId = responderRequestId
        if (id != null && requestId != null) {
            settled.add(id)
            runCatching { api.decline(requestId) }
        }
        owed?.let { o ->
            // Declined at the mid-flow proof block: the request is deleted
            // when its result was never posted.
            if (!o.resultPosted) runCatching { api.decline(o.requestId) }
            owed = null
        }
        answeredId = null
        responder = null
        dropConsented()
        engaged = false
        state = OldDeviceLinkState.Notice(DEVICE_LINK_OLD_DECLINED_COPY)
        return state
    }

    /** Close a notice, or close the question without choosing: nothing is
     *  sent, the request is not asked about again and expires on its own. */
    public fun dismiss() {
        when (state) {
            is OldDeviceLinkState.Notice -> state = OldDeviceLinkState.Idle
            OldDeviceLinkState.AskConsent -> {
                asking?.let { settled.add(it.idKey) }
                asking = null
                engaged = false
                state = OldDeviceLinkState.Idle
            }
            else -> Unit
        }
    }

    private val responderRequestId: ByteArray?
        get() = answeredId?.let { hex -> ByteArray(hex.length / 2) { i -> hex.substring(2 * i, 2 * i + 2).toInt(16).toByte() } }
}

/** The status outcome names (`services/auth` `outcome_name`). */
public const val OUTCOME_ACCEPTED: String = "accepted"
public const val OUTCOME_WRONG_CODE: String = "wrong_code"

/**
 * Map a core device-link error: `DeviceLinkWrongCode` -> [DeviceLinkWrongCode],
 * `DeviceLink` `invalid-code: ...` -> [DeviceLinkInvalidCode]; anything else
 * unchanged.
 */
public fun mapDeviceLinkError(error: Throwable): Throwable = when {
    error is uniffi.lakko_ffi_uniffi.FfiException.DeviceLinkWrongCode -> DeviceLinkWrongCode()
    error is uniffi.lakko_ffi_uniffi.FfiException.DeviceLink &&
        (error.message ?: "").startsWith("invalid-code") -> DeviceLinkInvalidCode()
    else -> error
}
