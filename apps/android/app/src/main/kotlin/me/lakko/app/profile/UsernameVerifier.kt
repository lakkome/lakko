// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred

/**
 * ADR-0048 §3 — verify a claimed username against the sender's authenticated
 * signature key.
 *
 * The claim is resolved through discovery — `lookup` -> `account_id` ->
 * identity bundle -> signature public key — and the username counts as
 * VERIFIED only when that key equals the key the profile is bound to.
 * Production passes [resolveUsernameForVerification], the plain path: it does
 * NOT run the G-4 supersession / pin gate, because that gate pins a first-seen
 * key as a side effect and showing a name must not decide which key a later
 * contact request trusts.
 *
 * ## The state machine
 *
 *  - a cached VERIFIED / FAILED result for `(key, username)` is returned
 *    without a lookup — each lookup costs an anonymous token and tells discovery
 *    someone resolved that name, so it is spent once per pair;
 *  - a claim failing the username rule is FAILED without a lookup;
 *  - `Found(key)` equal to the bound key -> VERIFIED; any other key -> FAILED;
 *  - `NotRegistered` -> FAILED;
 *  - `Unavailable` (network, server, an unreadable bundle) -> [VerifyOutcome.UNAVAILABLE],
 *    and NOTHING is cached, so a later attempt retries. A transient failure is
 *    not a verdict about the person;
 *  - a CHANGED claim is a different pair, so it verifies afresh;
 *  - concurrent callers for the same pair share ONE resolution (single-flight).
 *
 * ## Never from the background
 *
 * This is not reachable from the push background-decrypt path: that path only
 * reads what the app already stored (C-59) and never performs a network
 * lookup. The verifier is owned by [ProfileDirectory], which the background
 * path does not construct.
 */

/** What resolving a claimed username through discovery produced. */
public sealed interface UsernameResolution {
    /** The username is registered to an account whose bundle names this key. */
    public data class Found(val signatureKeyB64: String) : UsernameResolution

    /** No account holds the username. */
    public data object NotRegistered : UsernameResolution

    /** The lookup could not complete; no conclusion about the person. */
    public data class Unavailable(val cause: Throwable?) : UsernameResolution
}

/** The result of one [UsernameVerifier.verify] call. */
public enum class VerifyOutcome { VERIFIED, FAILED, UNAVAILABLE }

/** Single-flight, cached verifier. Thread-safe. */
public class UsernameVerifier(
    private val store: UsernameVerificationStore,
    /** ADR-0060 — resolves a claim knowing WHOSE it is (the certificate
     *  path needs the sender's stored profile). */
    private val resolveFor: suspend (username: String, signatureKeyB64: String) -> UsernameResolution,
    private val passesRule: (username: String) -> Boolean = { name ->
        checkUsername(name) == UsernameCheck.Valid(name)
    },
) {
    /** The lookup-only shape (ADR-0048): the claim's owner is not needed. */
    public constructor(
        store: UsernameVerificationStore,
        resolve: suspend (username: String) -> UsernameResolution,
        passesRule: (username: String) -> Boolean = { name ->
            checkUsername(name) == UsernameCheck.Valid(name)
        },
    ) : this(store, { name, _ -> resolve(name) }, passesRule)

    private val lock = Any()
    private val inFlight = HashMap<String, CompletableDeferred<VerifyOutcome>>()

    /** Verify that [username] belongs to [signatureKeyB64]. */
    public suspend fun verify(signatureKeyB64: String, username: String): VerifyOutcome {
        store.get(signatureKeyB64, username)?.let { return it.toOutcome() }
        if (!passesRule(username)) {
            store.put(signatureKeyB64, username, UsernameVerification.FAILED)
            return VerifyOutcome.FAILED
        }
        val pair = UsernameVerificationStore.pairKey(signatureKeyB64, username)
        val (deferred, owner) = synchronized(lock) {
            val existing = inFlight[pair]
            if (existing != null) {
                existing to false
            } else {
                val created = CompletableDeferred<VerifyOutcome>()
                inFlight[pair] = created
                created to true
            }
        }
        if (!owner) return deferred.await()
        try {
            val outcome = decide(signatureKeyB64, username)
            deferred.complete(outcome)
            return outcome
        } catch (cancel: CancellationException) {
            deferred.complete(VerifyOutcome.UNAVAILABLE)
            throw cancel
        } catch (error: Throwable) {
            deferred.complete(VerifyOutcome.UNAVAILABLE)
            return VerifyOutcome.UNAVAILABLE
        } finally {
            synchronized(lock) { inFlight.remove(pair) }
        }
    }

    private suspend fun decide(signatureKeyB64: String, username: String): VerifyOutcome {
        val result = when (val resolution = resolveFor(username, signatureKeyB64)) {
            is UsernameResolution.Found ->
                if (resolution.signatureKeyB64 == signatureKeyB64) {
                    UsernameVerification.VERIFIED
                } else {
                    UsernameVerification.FAILED
                }
            UsernameResolution.NotRegistered -> UsernameVerification.FAILED
            is UsernameResolution.Unavailable -> return VerifyOutcome.UNAVAILABLE
        }
        store.put(signatureKeyB64, username, result)
        return result.toOutcome()
    }

    private fun UsernameVerification.toOutcome(): VerifyOutcome = when (this) {
        UsernameVerification.VERIFIED -> VerifyOutcome.VERIFIED
        UsernameVerification.FAILED -> VerifyOutcome.FAILED
    }
}
