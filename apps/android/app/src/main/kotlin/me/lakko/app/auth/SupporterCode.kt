// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlin.coroutines.cancellation.CancellationException

/*
 * Supporter codes (ADR-0097, `docs/ux/supporters.md` section 1).
 *
 * A backer types a code on the "Lakko is full for now" state. The device
 * exchanges it, with no session, for one blind `beta-place` token
 * (`POST /auth/supporter-code/redeem`), checks the proof against the key of
 * `GET /auth/tokens/key/beta-place/{epoch}`, and holds the unblinded token for
 * 30 days. The first username bind of a sign-up carries it in
 * `x-lakko-anon-token`; the server spends it only when the cap would refuse.
 *
 * Everything here is pure or behind an interface, so the host JVM suite (which
 * has no native library) can drive it with fakes.
 */

/** The letters and digits a full code holds (`K7QM-2XRA-9DPF-4WNB`). */
public const val SUPPORTER_CODE_CHARS: Int = 16

/** Whether `Continue` is enabled: the field holds 16 letters and digits
 *  (UX 1.1). Spaces, dashes and case do not count. */
public fun supporterCodeComplete(text: String): Boolean =
    text.count { it in 'a'..'z' || it in 'A'..'Z' || it in '0'..'9' } == SUPPORTER_CODE_CHARS

/** What typing a code led to (UX 1.2). */
public sealed class SupporterRedeemOutcome {
    /** The place is held on this device. */
    public data class Held(val place: HeldPlace) : SupporterRedeemOutcome()

    /** "This code does not work. Check it and try again." */
    public data object Unknown : SupporterRedeemOutcome()

    /** "Too many tries. Try again later." */
    public data object Limited : SupporterRedeemOutcome()

    /** "Could not check the code. Try again." */
    public data object Failed : SupporterRedeemOutcome()
}

/** A held beta place: the token header value, the epoch it was issued
 *  for, and when this device got it. */
public data class HeldPlace(
    val headerValue: String,
    val epoch: Long,
    val heldAtMs: Long,
) {
    override fun toString(): String = "HeldPlace(epoch=$epoch, heldAtMs=$heldAtMs)"
}

/** The native core the exchange needs (UniFFI in production). */
public interface SupporterCodeCore {
    /** The canonical code, or `null` when it is not a supporter code. */
    public fun normalize(text: String): String?

    /** The `beta-place` epoch at [unixSecs] (30-day windows). */
    public fun epochAt(unixSecs: Long): Long

    /** The `beta-place` window in seconds. */
    public val windowSecs: Long

    /** Derive the deterministic request for [code] at [epoch]. */
    public fun begin(code: String, epoch: Long): SupporterCodeRequestHandle
}

/** One redeem request (wraps `SupporterCodeRequest`). */
public interface SupporterCodeRequestHandle : AutoCloseable {
    public val epoch: Long

    public fun redeemBodyJson(): String

    /** Check the proof under [publicKeyB64url] and unblind; throws on a bad
     *  proof. */
    public fun finalize(evaluationB64url: String, proofB64url: String, publicKeyB64url: String): String
}

/** What `POST /auth/supporter-code/redeem` answered. */
public sealed class SupporterRedeemWire {
    public data class Evaluated(val epoch: Long, val evaluation: String, val proof: String) : SupporterRedeemWire()

    /** 404 unknown, 410 expired, 409 mismatch, 400 malformed. */
    public data object Unknown : SupporterRedeemWire()

    /** 429 supporter-code-limited. */
    public data object Limited : SupporterRedeemWire()

    /** 400 epoch-not-issuable with the server's current epoch. */
    public data class EpochNotIssuable(val currentEpoch: Long) : SupporterRedeemWire()

    /** No connection, a 5xx, anything else. */
    public data object Failed : SupporterRedeemWire()
}

/** The two routes the exchange uses, both anonymous. */
public interface SupporterCodeApi {
    public suspend fun redeem(bodyJson: String): SupporterRedeemWire

    /** `GET /auth/tokens/key/beta-place/{epoch}`, or `null` when it failed. */
    public suspend fun publicKey(epoch: Long): String?
}

/**
 * The exchange of UX 1.2: normalise, redeem at the current epoch (once more
 * at the server's epoch when it says the client's is not issuable), check the
 * proof against the key route, unblind.
 */
public class SupporterCodeRedeemer(
    private val core: SupporterCodeCore,
    private val api: SupporterCodeApi,
    private val nowMs: () -> Long = System::currentTimeMillis,
) {
    public suspend fun redeem(text: String): SupporterRedeemOutcome {
        val code = core.normalize(text) ?: return SupporterRedeemOutcome.Unknown
        var epoch = core.epochAt(nowMs() / MS_PER_SEC)
        var retried = false
        while (true) {
            val outcome = try {
                core.begin(code, epoch).use { request -> attempt(request) }
            } catch (cancel: CancellationException) {
                throw cancel
            } catch (_: Throwable) {
                return SupporterRedeemOutcome.Failed
            }
            when (outcome) {
                is Attempt.Done -> return outcome.outcome
                is Attempt.Retry -> {
                    if (retried || outcome.epoch == epoch) return SupporterRedeemOutcome.Failed
                    retried = true
                    epoch = outcome.epoch
                }
            }
        }
    }

    private sealed class Attempt {
        data class Done(val outcome: SupporterRedeemOutcome) : Attempt()
        data class Retry(val epoch: Long) : Attempt()
    }

    private suspend fun attempt(request: SupporterCodeRequestHandle): Attempt =
        when (val wire = api.redeem(request.redeemBodyJson())) {
            SupporterRedeemWire.Unknown -> Attempt.Done(SupporterRedeemOutcome.Unknown)
            SupporterRedeemWire.Limited -> Attempt.Done(SupporterRedeemOutcome.Limited)
            SupporterRedeemWire.Failed -> Attempt.Done(SupporterRedeemOutcome.Failed)
            is SupporterRedeemWire.EpochNotIssuable -> Attempt.Retry(wire.currentEpoch)
            is SupporterRedeemWire.Evaluated -> {
                if (wire.epoch != request.epoch) {
                    Attempt.Done(SupporterRedeemOutcome.Failed)
                } else {
                    // The key always comes from the key route, never from the
                    // redeem answer (ADR-0097 section 1.3).
                    val key = api.publicKey(wire.epoch)
                    if (key == null) {
                        Attempt.Done(SupporterRedeemOutcome.Failed)
                    } else {
                        val header = request.finalize(wire.evaluation, wire.proof, key)
                        Attempt.Done(SupporterRedeemOutcome.Held(HeldPlace(header, wire.epoch, nowMs())))
                    }
                }
            }
        }

    private companion object {
        const val MS_PER_SEC: Long = 1000L
    }
}

/**
 * The held place on this device (pre-account data, so not keyed by account).
 * A place is kept for one window from when it was held, and never once its
 * epoch is older than the previous one (the server accepts the current and
 * the previous epoch only).
 */
public class SupporterPlaceStore(
    private val storage: MarkerStorage,
    private val windowSecs: () -> Long,
    private val epochAt: (unixSecs: Long) -> Long,
    private val nowMs: () -> Long = System::currentTimeMillis,
) {
    /** The live held place, or `null`; an expired or unreadable row is
     *  dropped. */
    public fun held(): HeldPlace? {
        val raw = storage.get(KEY) ?: return null
        val place = decode(raw)
        if (place == null || !live(place)) {
            storage.remove(KEY)
            return null
        }
        return place
    }

    public fun hold(place: HeldPlace) {
        storage.set(KEY, encode(place))
    }

    public fun drop() {
        storage.remove(KEY)
    }

    private fun live(place: HeldPlace): Boolean {
        val now = nowMs()
        if (now < place.heldAtMs) return true
        if (now - place.heldAtMs >= windowSecs() * MS_PER_SEC) return false
        return epochAt(now / MS_PER_SEC) <= place.epoch + 1
    }

    internal companion object {
        const val KEY: String = "beta_place_v1"
        const val MS_PER_SEC: Long = 1000L
        private const val SEP: Char = '|'

        fun encode(place: HeldPlace): String = "${place.epoch}$SEP${place.heldAtMs}$SEP${place.headerValue}"

        fun decode(raw: String): HeldPlace? {
            val parts = raw.split(SEP)
            if (parts.size != 3) return null
            val epoch = parts[0].toLongOrNull() ?: return null
            val at = parts[1].toLongOrNull() ?: return null
            if (parts[2].isEmpty()) return null
            return HeldPlace(parts[2], epoch, at)
        }
    }
}

/** What the sign-up screen needs of supporter codes. */
public interface SupporterPlaces {
    /** The live held place, or `null`. */
    public fun held(): HeldPlace?

    /** Forget the held place (spent elsewhere). */
    public fun drop()

    /** Exchange typed text; a [SupporterRedeemOutcome.Held] is already
     *  stored when returned. */
    public suspend fun redeem(text: String): SupporterRedeemOutcome

    public companion object {
        /** No supporter codes (host tests that do not exercise them). */
        public val None: SupporterPlaces = object : SupporterPlaces {
            override fun held(): HeldPlace? = null
            override fun drop() = Unit
            override suspend fun redeem(text: String): SupporterRedeemOutcome = SupporterRedeemOutcome.Failed
        }
    }
}

/** [SupporterPlaces] over a [SupporterPlaceStore] and a [SupporterCodeRedeemer]. */
public class StoredSupporterPlaces(
    private val store: SupporterPlaceStore,
    private val redeemer: SupporterCodeRedeemer,
) : SupporterPlaces {
    override fun held(): HeldPlace? = store.held()

    override fun drop() {
        store.drop()
    }

    override suspend fun redeem(text: String): SupporterRedeemOutcome {
        val outcome = redeemer.redeem(text)
        if (outcome is SupporterRedeemOutcome.Held) store.hold(outcome.place)
        return outcome
    }
}
