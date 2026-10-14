// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import android.util.Log
import kotlinx.coroutines.launch

/*
 * ADR-0060 section 4 — contacts of contacts, the receiving half. The Android
 * twin of the `introduction-offer` / `introductions` arms of
 * `apps/web/src/reach/scoped-intake.ts`.
 *
 * The chat repository already applied the carrier rule (only from the other
 * participant of a one-to-one, the sender resolved at receipt, never from
 * this account). What is left is the decode: an offer is kept as the record
 * it carries (newest per contact), a bundle as received when strictly newer
 * than the one held (`bundleIsNewer`). A malformed envelope authorises and
 * shows nothing.
 */

/** What one envelope turns into. */
internal sealed interface IntroIntake {
    /** Keep [record] as the sender's offer. */
    class Offer(val record: ByteArray) : IntroIntake

    /** Keep the bundle [wire] stamped [updatedAtMs]. */
    class Bundle(val wire: ByteArray, val updatedAtMs: ULong) : IntroIntake

    /** Drop it. */
    data object Drop : IntroIntake
}

/** Decode one envelope of [kind]; the decoders are passed in. */
internal fun decideIntroIntake(
    kind: ScopedEnvelopeKind,
    bytes: ByteArray,
    decodeOffer: (ByteArray) -> ByteArray,
    decodeBundleStamp: (ByteArray) -> ULong,
): IntroIntake = try {
    when (kind) {
        ScopedEnvelopeKind.INTRODUCTION_OFFER -> IntroIntake.Offer(decodeOffer(bytes))
        ScopedEnvelopeKind.INTRODUCTIONS -> IntroIntake.Bundle(bytes, decodeBundleStamp(bytes))
        else -> IntroIntake.Drop
    }
} catch (_: Throwable) {
    IntroIntake.Drop
}

/**
 * One background intake for the account that was bound when the envelope was
 * handed over: [keep] and then [then], each only while [stillBound].
 */
internal suspend fun runIntakeFor(
    stillBound: () -> Boolean,
    keep: suspend () -> Unit,
    then: () -> Unit,
) {
    if (!stillBound()) return
    keep()
    if (stillBound()) then()
}

/** The production intake: decode with the core, keep in [reach]. */
public object IntroductionIntake {
    /** Keep an offer or a bundle [senderKey] sent. Never throws. */
    public suspend fun keep(
        reach: ReachManager,
        senderKey: ByteArray,
        kind: ScopedEnvelopeKind,
        bytes: ByteArray,
        /** The account the envelope was received for; `null` = whoever is bound. */
        boundAs: ReachManager.BoundAccount? = null,
    ) {
        when (
            val decided = decideIntroIntake(
                kind = kind,
                bytes = bytes,
                decodeOffer = { uniffi.lakko_ffi_uniffi.decodeIntroductionOffer(it) },
                decodeBundleStamp = { uniffi.lakko_ffi_uniffi.decodeIntroductionsBundle(it).updatedAtMs },
            )
        ) {
            is IntroIntake.Offer -> reach.keepIntroductionOffer(senderKey, decided.record, boundAs)
            is IntroIntake.Bundle ->
                reach.keepIntroductionsBundle(senderKey, decided.wire, decided.updatedAtMs, boundAs)
            IntroIntake.Drop -> Log.i(LOG_TAG, "${kind.name.lowercase()} envelope dropped")
        }
    }

    /** Outlives the receive loop that handed the envelope over. */
    private val scope = kotlinx.coroutines.CoroutineScope(
        kotlinx.coroutines.SupervisorJob() + kotlinx.coroutines.Dispatchers.IO,
    )

    /**
     * [keep] in the background, then [then] (the relay re-run).
     *
     * The envelope is one of a contact of the account bound NOW (the caller
     * is that account's receive loop). The account is remembered here, on the
     * caller's thread, and not read when the launched work gets to run: by
     * then it can be signed out and another signed in, and a contact's
     * introductions kept for that one would hand it another person's
     * contacts.
     */
    public fun keepInBackground(
        reach: ReachManager,
        senderKey: ByteArray,
        kind: ScopedEnvelopeKind,
        bytes: ByteArray,
        then: () -> Unit,
    ) {
        val boundAs = reach.boundAccount() ?: return
        scope.launch {
            try {
                runIntakeFor(
                    stillBound = { reach.isBound(boundAs) },
                    keep = { keep(reach, senderKey, kind, bytes, boundAs) },
                    then = then,
                )
            } catch (cancel: kotlinx.coroutines.CancellationException) {
                throw cancel
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "introduction envelope not kept: ${cause.javaClass.simpleName}")
            }
        }
    }

    private const val LOG_TAG = "Reach"
}
