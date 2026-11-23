// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.isActive
import me.lakko.app.ds.DsClient
import me.lakko.app.ds.DsHttpError
import me.lakko.app.ds.DsNetworkError

/**
 * DS-mailbox round-trip for the add-device Welcome delivery
 * (Phase 2a3b4 Android infrastructure).
 *
 * Mirror of:
 *
 *   * `apps/web/src/devices/welcome-courier.ts`
 *   * `apps/ios/Lakko/Devices/WelcomeCourier.swift`
 *
 * The scanner-side device PUTs the Welcome via
 * [publishWelcome]; the joining-side device long-polls via
 * [pollForWelcome] until the Welcome arrives, the caller's
 * coroutine is cancelled, or the soft timeout elapses.
 *
 * No new server endpoints — the existing anonymous
 * `POST/GET /ds/mailbox/{pseudonym_hex}/messages` route is
 * exactly the rendezvous-mailbox contract.
 */
public object WelcomeCourier {

    /** Default soft timeout for [pollForWelcome] in milliseconds.
     *  Mirror of the web/iOS 60-second budget. The UI can
     *  override via the `timeoutMillis` argument. */
    public const val DEFAULT_TIMEOUT_MS: Long = 60_000L

    /** Server-side long-poll budget per request, in seconds.
     *  The DS service clamps to its own `MAX_WAIT_SECS`; we ask
     *  comfortably below that so a misclock between client
     *  and server doesn't waste a full poll. */
    private const val PER_REQUEST_LONG_POLL_SECS: Int = 15

    /** Discriminated outcome of [pollForWelcome]. */
    public sealed interface WelcomeOutcome {
        /** `welcomeCursor` is the DS journal id the Welcome row
         *  landed on; the history-bundle poll uses it as the
         *  starting `after` cursor. */
        public data class Received(
            val welcomeBytes: ByteArray,
            val welcomeCursor: Long,
        ) : WelcomeOutcome {
            override fun equals(other: Any?): Boolean {
                if (this === other) return true
                if (other !is Received) return false
                return welcomeBytes.contentEquals(other.welcomeBytes) &&
                    welcomeCursor == other.welcomeCursor
            }
            override fun hashCode(): Int {
                var h = welcomeBytes.contentHashCode()
                h = 31 * h + welcomeCursor.hashCode()
                return h
            }
        }
        public data object Timeout : WelcomeOutcome
        public data object Cancelled : WelcomeOutcome
    }

    /** Discriminated outcome of [pollForHistoryBundle]. */
    public sealed interface HistoryBundleOutcome {
        public data class Received(val sealedBytes: ByteArray) : HistoryBundleOutcome {
            override fun equals(other: Any?): Boolean {
                if (this === other) return true
                if (other !is Received) return false
                return sealedBytes.contentEquals(other.sealedBytes)
            }
            override fun hashCode(): Int = sealedBytes.contentHashCode()
        }
        public data object Timeout : HistoryBundleOutcome
        public data object Cancelled : HistoryBundleOutcome
    }

    /**
     * The plain log line for a history poll that ended without a bundle, or
     * `null` for a received one (its apply logs the row count). No
     * identifiers. Before 2026-11-23 a timeout logged nothing, so a scanner
     * that never sent its bundle looked like a join that never asked.
     */
    public fun historyOutcomeLogLine(outcome: HistoryBundleOutcome): String? =
        when (outcome) {
            is HistoryBundleOutcome.Received -> null
            HistoryBundleOutcome.Timeout -> "history catch-up: no bundle arrived before the timeout"
            HistoryBundleOutcome.Cancelled -> "history catch-up: cancelled"
        }

    /** Soft timeout for the joiner-side history poll (Phase
     *  2a3d8-ui). Short — bundle is supposed to arrive within
     *  seconds of the Welcome; a longer wait stalls the UX. */
    public const val HISTORY_BUNDLE_TIMEOUT_MS: Long = 10_000L

    /**
     * Scanner side: PUT a Welcome to the rendezvous mailbox
     * slot the joining device chose.
     *
     * Suspending — fires a single POST against the DS;
     * propagates [DsClient] errors verbatim.
     */
    public suspend fun publishWelcome(
        rendezvousPseudonym: ByteArray,
        welcomeBytes: ByteArray,
        ds: DsClient = DsClient(),
    ) {
        ds.use { client ->
            client.publishRendezvous(
                rendezvous = rendezvousPseudonym,
                payload = welcomeBytes,
            )
        }
    }

    /**
     * Joining side: poll the rendezvous mailbox until the
     * Welcome arrives, the calling coroutine is cancelled, or
     * the soft timeout elapses.
     *
     * Returns:
     *   * [WelcomeOutcome.Received] — hand the bytes to
     *     `DeviceGroup.joinFromWelcome`.
     *   * [WelcomeOutcome.Timeout] — UI surfaces "no response
     *     yet" with a retry affordance.
     *   * [WelcomeOutcome.Cancelled] — calling coroutine was
     *     cancelled (user navigated away from the pairing
     *     screen).
     *
     * Throws on transport / server failures (404 on a fresh
     * mailbox is treated as "no rows yet, keep polling" — not
     * an error; same with network blips).
     *
     * A row [accepts] says no to (anyone who learned the rendezvous
     * id can write to it) is passed over and the read goes on from
     * the next row; a throw from [accepts] counts as no. The
     * joining device passes [welcomeOpensFor].
     */
    public suspend fun pollForWelcome(
        rendezvousPseudonym: ByteArray,
        timeoutMillis: Long = DEFAULT_TIMEOUT_MS,
        ds: DsClient = DsClient(),
        afterCursor: Long = 0L,
        accepts: suspend (ByteArray) -> Boolean = { true },
    ): WelcomeOutcome = ds.use { client ->
        val deadline = System.nanoTime() + timeoutMillis * 1_000_000L
        // `after=0` is the canonical "give me everything from
        // the beginning of this slot" cursor. The slot is fresh
        // per pairing, so the first published id is 1.
        var cursor = afterCursor

        while (true) {
            if (!currentCoroutineContext().isActive) {
                return@use WelcomeOutcome.Cancelled
            }
            val remainingNanos = deadline - System.nanoTime()
            if (remainingNanos <= 0L) return@use WelcomeOutcome.Timeout

            val remainingSecs = (remainingNanos / 1_000_000_000L).toInt().coerceAtLeast(1)
            val waitSecs = minOf(PER_REQUEST_LONG_POLL_SECS, remainingSecs)

            val result = try {
                client.fetchRendezvous(
                    rendezvous = rendezvousPseudonym,
                    after = cursor,
                    limit = 1,
                    waitSecs = waitSecs,
                )
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (http: DsHttpError) {
                if (http.status == 404) {
                    // No rows yet — keep polling within the
                    // overall deadline.
                    continue
                }
                throw http
            } catch (_: DsNetworkError) {
                // Transient network blip — try again until
                // the deadline elapses.
                continue
            }
            val first = result.messages.firstOrNull()
            if (first != null) {
                if (!rowAccepted(first.payload, accepts)) {
                    // Not a Welcome this device can open: pass over it.
                    cursor = maxOf(cursor, first.id)
                    continue
                }
                return@use WelcomeOutcome.Received(
                    welcomeBytes = first.payload,
                    welcomeCursor = first.id,
                )
            }
            // Advance cursor by the highest id seen so the next
            // poll doesn't re-fetch the same empty range. Happy
            // path returns above; this branch is defensive.
            cursor = result.messages.fold(cursor) { acc, msg -> maxOf(acc, msg.id) }
        }
        // Unreachable — every branch above either returns or
        // continues. Kotlin's flow analysis on `while (true)`
        // inside a lambda requires an explicit terminator.
        @Suppress("UNREACHABLE_CODE")
        WelcomeOutcome.Timeout
    }

    // ----------------------------------------------------------------
    // Phase 2a3d8-ui — history-bundle courier piggybacks the
    // rendezvous mailbox after the Welcome.
    // ----------------------------------------------------------------

    /**
     * Scanner side: PUT the sealed history bundle to the
     * rendezvous slot AFTER the Welcome. FIFO journal assigns
     * the next id (id=2 if Welcome was id=1).
     */
    public suspend fun publishHistoryBundle(
        rendezvousPseudonym: ByteArray,
        sealedBytes: ByteArray,
        ds: DsClient = DsClient(),
    ) {
        ds.use { client ->
            client.publishRendezvous(
                rendezvous = rendezvousPseudonym,
                payload = sealedBytes,
            )
        }
    }

    /**
     * Joining side: poll the rendezvous mailbox past the
     * Welcome cursor for the sealed history bundle. Soft
     * timeout (default 10 s) — the scanner side either
     * publishes within that window or never (no prior history
     * to send).
     *
     * A row [accepts] says no to (the bundle does not open under the
     * joined group's history secret) is passed over like the recovery
     * and state rows; a throw from [accepts] counts as no.
     */
    public suspend fun pollForHistoryBundle(
        rendezvousPseudonym: ByteArray,
        welcomeCursor: Long,
        timeoutMillis: Long = HISTORY_BUNDLE_TIMEOUT_MS,
        ds: DsClient = DsClient(),
        accepts: suspend (ByteArray) -> Boolean = { true },
    ): HistoryBundleOutcome = ds.use { client ->
        val deadline = System.nanoTime() + timeoutMillis * 1_000_000L
        var cursor = welcomeCursor

        while (true) {
            if (!currentCoroutineContext().isActive) {
                return@use HistoryBundleOutcome.Cancelled
            }
            val remainingNanos = deadline - System.nanoTime()
            if (remainingNanos <= 0L) return@use HistoryBundleOutcome.Timeout

            val remainingSecs = (remainingNanos / 1_000_000_000L).toInt().coerceAtLeast(1)
            val waitSecs = minOf(PER_REQUEST_LONG_POLL_SECS, remainingSecs)

            val result = try {
                client.fetchRendezvous(
                    rendezvous = rendezvousPseudonym,
                    after = cursor,
                    limit = 1,
                    waitSecs = waitSecs,
                )
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (http: DsHttpError) {
                if (http.status == 404) continue
                throw http
            } catch (_: DsNetworkError) {
                continue
            }
            val first = result.messages.firstOrNull()
            if (first != null) {
                // ADR-0018 factor (a): the rendezvous mailbox can also
                // carry a tagged recovery-payload row (after the
                // Welcome, in either FIFO order relative to the
                // bundle). It is NOT the history bundle — skip it and
                // keep polling. Mirror of the web's
                // `welcome-courier.ts` skip.
                if (RecoveryRow.isRecoveryRow(first.payload) ||
                    StateBundle.isStateRow(first.payload) ||
                    !rowAccepted(first.payload, accepts)
                ) {
                    cursor = maxOf(cursor, first.id)
                    continue
                }
                return@use HistoryBundleOutcome.Received(first.payload)
            }
            cursor = result.messages.fold(cursor) { acc, msg -> maxOf(acc, msg.id) }
        }
        @Suppress("UNREACHABLE_CODE")
        HistoryBundleOutcome.Timeout
    }

    /** Whether [accepts] takes [row]; a throw (other than a cancellation)
     *  is a no. */
    internal suspend fun rowAccepted(row: ByteArray, accepts: suspend (ByteArray) -> Boolean): Boolean =
        try {
            accepts(row)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            false
        }

    /**
     * Whether [welcome] is a Welcome sealed to a KeyPackage [identity]
     * holds: it is staged (decrypted) with `peekWelcomeFounder`, which
     * writes nothing, so the real join of the same bytes still works. A
     * sender whose device binding does not verify still staged: the join
     * decides that case. Anything else (not a Welcome, not for this device)
     * is `false`.
     */
    public fun welcomeOpensFor(welcome: ByteArray, identity: uniffi.lakko_ffi_uniffi.Identity): Boolean =
        try {
            uniffi.lakko_ffi_uniffi.peekWelcomeFounder(welcome, identity)
            true
        } catch (_: uniffi.lakko_ffi_uniffi.FfiException.MembershipDeviceBindingInvalid) {
            true
        } catch (_: Throwable) {
            false
        }
}
