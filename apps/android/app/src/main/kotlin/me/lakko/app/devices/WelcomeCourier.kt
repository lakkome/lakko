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
        public data class Received(val welcomeBytes: ByteArray) : WelcomeOutcome {
            override fun equals(other: Any?): Boolean {
                if (this === other) return true
                if (other !is Received) return false
                return welcomeBytes.contentEquals(other.welcomeBytes)
            }
            override fun hashCode(): Int = welcomeBytes.contentHashCode()
        }
        public data object Timeout : WelcomeOutcome
        public data object Cancelled : WelcomeOutcome
    }

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
            client.publishMailbox(
                pseudonym = rendezvousPseudonym,
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
     */
    public suspend fun pollForWelcome(
        rendezvousPseudonym: ByteArray,
        timeoutMillis: Long = DEFAULT_TIMEOUT_MS,
        ds: DsClient = DsClient(),
    ): WelcomeOutcome = ds.use { client ->
        val deadline = System.nanoTime() + timeoutMillis * 1_000_000L
        // `after=0` is the canonical "give me everything from
        // the beginning of this slot" cursor. The slot is fresh
        // per pairing, so the first published id is 1.
        var cursor = 0L

        while (true) {
            if (!currentCoroutineContext().isActive) {
                return@use WelcomeOutcome.Cancelled
            }
            val remainingNanos = deadline - System.nanoTime()
            if (remainingNanos <= 0L) return@use WelcomeOutcome.Timeout

            val remainingSecs = (remainingNanos / 1_000_000_000L).toInt().coerceAtLeast(1)
            val waitSecs = minOf(PER_REQUEST_LONG_POLL_SECS, remainingSecs)

            val result = try {
                client.fetchMailbox(
                    pseudonym = rendezvousPseudonym,
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
                return@use WelcomeOutcome.Received(first.payload)
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
}
