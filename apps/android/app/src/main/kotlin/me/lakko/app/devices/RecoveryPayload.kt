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
 * Recovery-payload wire module — ADR-0018 Part 2a, factor (a)
 * ("another-device" recovery), Path B (MLS exporter).
 *
 * Mirror of `apps/web/src/devices/recovery-payload.ts` — the wire
 * format MUST stay byte-identical (cross-platform pairing: an
 * Android scanner publishes rows a web joiner opens and vice
 * versa).
 *
 * After the add-device pairing completes, the scanner (existing,
 * authenticated) device transfers BOTH halves of recovery to the
 * fresh device over the rendezvous mailbox:
 *
 *   * **ACCESS** — a single-use 32-byte device-authorization
 *     `grant_token` minted by `POST /device/authorize/start`
 *     (bearer). The fresh device redeems it anonymously at
 *     `POST /device/authorize/redeem` for a session-token triple.
 *   * **IDENTITY** — the 64-byte AccountIdentity (BIP39 phrase)
 *     seed (ADR-0017), so the fresh device never types the phrase.
 *
 * ## Crypto (Path B — MLS exporter)
 *
 * Both devices sit at the same DeviceGroup epoch once the Welcome
 * is processed (the scanner committed the add; the joiner ratcheted
 * from the Welcome), so both derive the SAME 32-byte key via
 *
 * ```text
 *   key = deviceGroup.exportSecret(
 *           identity,
 *           "lakko/device-recovery/v1",   // RECOVERY_EXPORTER_LABEL
 *           rendezvous_pseudonym,          // 32-byte, THIS pairing session
 *           32,
 *         )
 * ```
 *
 * The scanner seals `grant_token(32) || phrase_seed(64)` (96 bytes
 * fixed) with `sealWithKey(key, payload)` — ChaCha20-Poly1305, wire
 * `nonce(12) || ciphertext+tag` — and the joiner opens it with
 * `openWithKey`. The context binding to the rendezvous pseudonym
 * scopes the key to THIS pairing session.
 *
 * ## Mailbox row format (DS opacity)
 *
 * The sealed blob rides the SAME rendezvous mailbox the Welcome
 * (row 1) and the optional history bundle (next row) use — no new
 * server surface. To keep the three row types distinguishable the
 * recovery row carries a plaintext ASCII magic prefix:
 *
 * ```text
 *   row = "lakko/recovery-payload/v1" || sealed_blob
 * ```
 *
 * The magic marks only the row TYPE; everything sensitive is
 * inside the AEAD blob. The DS stores opaque bytes at a pseudonym
 * either way (invariant #2) — the phrase seed crosses the wire
 * only sealed under the exporter key, which the server cannot
 * derive (it has no DeviceGroup state).
 */
public object RecoveryPayload {

    /** MLS exporter label for the per-pairing recovery key.
     *  Pinned ASCII; the `/v1` suffix is part of the wire contract.
     *  Keep in lockstep with the web / iOS clients. */
    public const val RECOVERY_EXPORTER_LABEL: String = "lakko/device-recovery/v1"

    /** Exporter output length — the ChaCha20-Poly1305 key size. */
    public const val RECOVERY_EXPORTER_KEY_LENGTH: Int = 32

    /** Raw device-authorization grant token length
     *  (`services/auth/src/device_authorize.rs::GRANT_TOKEN_LEN`). */
    public const val RECOVERY_GRANT_TOKEN_LENGTH: Int = 32

    /** AccountIdentity (BIP39 phrase) seed length (ADR-0017). */
    public const val RECOVERY_PHRASE_SEED_LENGTH: Int = 64

    /** Fixed plaintext payload length: `grant(32) || seed(64)`. */
    public const val RECOVERY_PAYLOAD_LENGTH: Int =
        RECOVERY_GRANT_TOKEN_LENGTH + RECOVERY_PHRASE_SEED_LENGTH

    /** Plaintext ASCII magic prefix that tags a rendezvous-mailbox
     *  row as a recovery payload (vs the Welcome / history-bundle
     *  rows). Byte-identical to the web's
     *  `new TextEncoder().encode("lakko/recovery-payload/v1")` —
     *  pure ASCII, so US-ASCII == UTF-8 here. */
    private val RECOVERY_ROW_MAGIC: ByteArray =
        "lakko/recovery-payload/v1".toByteArray(Charsets.US_ASCII)

    /** Server-side long-poll budget per request, in seconds — same
     *  rationale as [WelcomeCourier]. */
    private const val PER_REQUEST_LONG_POLL_SECS: Int = 15

    /** Default soft timeout for the joiner-side recovery poll. The
     *  scanner publishes the row right after the Welcome + history
     *  bundle (one extra `/device/authorize/start` round-trip),
     *  so a short window suffices; absence (older scanner build, or
     *  the best-effort scanner half failed) is tolerated — the join
     *  completes without a recovery payload. */
    public const val RECOVERY_POLL_TIMEOUT_MS: Long = 15_000L

    // ---- payload build / parse ---------------------------------------

    /**
     * Scanner side: concatenate `grant_token(32) || phrase_seed(64)`
     * into the fixed 96-byte plaintext payload. Strict length
     * validation — a wrong-size grant or seed is a caller bug that
     * must surface before anything is sealed or published.
     *
     * The returned buffer is a fresh copy; the caller should zeroize
     * it (and its inputs) once the sealed blob is built.
     */
    public fun buildRecoveryPayload(
        grantToken: ByteArray,
        phraseSeed: ByteArray,
    ): ByteArray {
        require(grantToken.size == RECOVERY_GRANT_TOKEN_LENGTH) {
            "grant token must be $RECOVERY_GRANT_TOKEN_LENGTH bytes " +
                "(got ${grantToken.size})"
        }
        require(phraseSeed.size == RECOVERY_PHRASE_SEED_LENGTH) {
            "phrase seed must be $RECOVERY_PHRASE_SEED_LENGTH bytes " +
                "(got ${phraseSeed.size})"
        }
        val out = ByteArray(RECOVERY_PAYLOAD_LENGTH)
        grantToken.copyInto(out, destinationOffset = 0)
        phraseSeed.copyInto(out, destinationOffset = RECOVERY_GRANT_TOKEN_LENGTH)
        return out
    }

    /** Parsed recovery payload — fresh copies of both halves. */
    public class ParsedRecoveryPayload(
        /** Raw 32-byte single-use device-authorization grant token. */
        public val grantToken: ByteArray,
        /** Raw 64-byte AccountIdentity (BIP39 phrase) seed. */
        public val phraseSeed: ByteArray,
    )

    /**
     * Joiner side: split the opened 96-byte plaintext back into
     * `(grant_token, phrase_seed)`. Strict length validation — any
     * other length means a wire/protocol mismatch and is refused.
     *
     * Both returned buffers are fresh copies; the caller should
     * zeroize the input `payload` after this returns.
     */
    public fun parseRecoveryPayload(payload: ByteArray): ParsedRecoveryPayload {
        require(payload.size == RECOVERY_PAYLOAD_LENGTH) {
            "recovery payload must be $RECOVERY_PAYLOAD_LENGTH bytes " +
                "(got ${payload.size})"
        }
        return ParsedRecoveryPayload(
            grantToken = payload.copyOfRange(0, RECOVERY_GRANT_TOKEN_LENGTH),
            phraseSeed = payload.copyOfRange(
                RECOVERY_GRANT_TOKEN_LENGTH,
                RECOVERY_PAYLOAD_LENGTH,
            ),
        )
    }

    // ---- mailbox row wrap / unwrap -----------------------------------

    /** Wrap a sealed blob into the tagged mailbox row:
     *  `magic || sealed`. */
    public fun wrapRecoveryRow(sealedBlob: ByteArray): ByteArray {
        val out = ByteArray(RECOVERY_ROW_MAGIC.size + sealedBlob.size)
        RECOVERY_ROW_MAGIC.copyInto(out, destinationOffset = 0)
        sealedBlob.copyInto(out, destinationOffset = RECOVERY_ROW_MAGIC.size)
        return out
    }

    /** `true` when the mailbox row carries the recovery-payload magic
     *  prefix. Used by the joiner-side dispatch (and by the
     *  history-bundle poll to SKIP recovery rows). */
    public fun isRecoveryPayloadRow(row: ByteArray): Boolean {
        if (row.size <= RECOVERY_ROW_MAGIC.size) return false
        for (i in RECOVERY_ROW_MAGIC.indices) {
            if (row[i] != RECOVERY_ROW_MAGIC[i]) return false
        }
        return true
    }

    /** Strip the magic prefix and return the sealed blob, or `null`
     *  when the row is not a recovery-payload row. */
    public fun unwrapRecoveryRow(row: ByteArray): ByteArray? {
        if (!isRecoveryPayloadRow(row)) return null
        return row.copyOfRange(RECOVERY_ROW_MAGIC.size, row.size)
    }

    // ---- publish / poll ----------------------------------------------

    /**
     * Scanner side: PUT the sealed recovery payload to the rendezvous
     * mailbox AFTER the Welcome (and the optional history bundle).
     * Same mailbox; the FIFO journal assigns it the next id. The row
     * is wrapped with the plaintext magic so the joiner-side polls can
     * dispatch by row type regardless of FIFO position.
     */
    public suspend fun publishRecoveryPayload(
        rendezvousPseudonym: ByteArray,
        sealedBlob: ByteArray,
        ds: DsClient = DsClient(),
    ) {
        ds.use { client ->
            client.publishMailbox(
                pseudonym = rendezvousPseudonym,
                payload = wrapRecoveryRow(sealedBlob),
            )
        }
    }

    /** Discriminated outcome of [pollForRecoveryPayload]. */
    public sealed interface RecoveryPayloadOutcome {
        public data class Received(val sealedBlob: ByteArray) : RecoveryPayloadOutcome {
            override fun equals(other: Any?): Boolean {
                if (this === other) return true
                if (other !is Received) return false
                return sealedBlob.contentEquals(other.sealedBlob)
            }
            override fun hashCode(): Int = sealedBlob.contentHashCode()
        }
        public data object Timeout : RecoveryPayloadOutcome
        public data object Cancelled : RecoveryPayloadOutcome
    }

    /**
     * Joining side: poll the rendezvous mailbox past the Welcome
     * cursor for the recovery-payload row. Non-recovery rows (the
     * history bundle) are skipped by advancing the cursor; the first
     * row carrying the magic prefix wins.
     *
     * Absence is an expected outcome ([RecoveryPayloadOutcome.Timeout]):
     * an older scanner build, or a scanner whose best-effort recovery
     * half failed, publishes no row — the join then completes exactly
     * as today and the fresh device falls back to phrase entry.
     *
     * `afterCursor` is typically the Welcome's DS journal id; both
     * this poll and [WelcomeCourier.pollForHistoryBundle] start there
     * and filter disjoint row types, so they can run concurrently.
     */
    public suspend fun pollForRecoveryPayload(
        rendezvousPseudonym: ByteArray,
        afterCursor: Long,
        timeoutMillis: Long = RECOVERY_POLL_TIMEOUT_MS,
        ds: DsClient = DsClient(),
    ): RecoveryPayloadOutcome = ds.use { client ->
        val deadline = System.nanoTime() + timeoutMillis * 1_000_000L
        var cursor = afterCursor

        while (true) {
            if (!currentCoroutineContext().isActive) {
                return@use RecoveryPayloadOutcome.Cancelled
            }
            val remainingNanos = deadline - System.nanoTime()
            if (remainingNanos <= 0L) return@use RecoveryPayloadOutcome.Timeout

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
                // Same transient-error posture as the Welcome /
                // history polls — 404 means "no rows yet".
                if (http.status == 404) continue
                throw http
            } catch (_: DsNetworkError) {
                continue
            }
            val first = result.messages.firstOrNull()
            if (first != null) {
                val sealed = unwrapRecoveryRow(first.payload)
                if (sealed != null) {
                    return@use RecoveryPayloadOutcome.Received(sealed)
                }
                // Not a recovery row (e.g. the history bundle) —
                // skip it.
                cursor = first.id
                continue
            }
            cursor = result.messages.fold(cursor) { acc, msg -> maxOf(acc, msg.id) }
        }
        @Suppress("UNREACHABLE_CODE")
        RecoveryPayloadOutcome.Timeout
    }
}
