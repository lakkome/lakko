// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.isActive
import me.lakko.app.ds.DsClient
import me.lakko.app.ds.DsHttpError
import me.lakko.app.ds.DsNetworkError
import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.FfiRecoveryPredecessor

/**
 * The "another device" transfer (ADR-0018 factor (a), Path B), recovery
 * payload **v3** (ADR-0074, "Joining after a rotation", 2026-11-15; G-56).
 *
 * After a pairing lands, the confirming device sends the fresh device, over
 * the rendezvous mailbox:
 *
 *   * ACCESS: a single-use 32-byte device-authorisation grant, redeemed
 *     anonymously at `POST /device/authorize/redeem`;
 *   * KEYS: the 16-byte entropy of the current 12-word phrase, and the
 *     account's own earlier phrase seeds (0 to 8, oldest first), so a device
 *     paired after a key rotation binds under the conversation key at once.
 *
 * The codec, the exporter label (`lakko/device-recovery/v3`) and the row magic
 * (`lakko/recovery-payload/v3`) are the core's (`composeRecoveryPayload`,
 * `decodeRecoveryPayload`, `wrapRecoveryRow`, `unwrapRecoveryRow`); nothing
 * here re-implements them. The seal stays here: key =
 * `deviceGroup.exportSecret(identity, label, rendezvous, keyLen)`, then
 * `sealWithKey` / `openWithKey`. v2 is refused (flag day): its row is a
 * recovery row the core will not unwrap, so the joiner stops and says it
 * could not take the keys.
 */
public object RecoveryRow {

    /** Server-side long-poll budget per request, in seconds (as
     *  [WelcomeCourier]). */
    private const val PER_REQUEST_LONG_POLL_SECS: Int = 15

    /** Default soft timeout for the joiner-side recovery poll. */
    public const val POLL_TIMEOUT_MS: Long = 15_000L

    /** A recovery row refused by the core (another version, or empty). */
    public class Refused(cause: Throwable? = null) : IllegalStateException("recovery row refused", cause)

    /** An opened payload. Secrets: [zero] when done. */
    public class Parsed(
        public val grantToken: ByteArray,
        public val phraseEntropy: ByteArray,
        /** The account's own earlier seeds, oldest first. */
        public val predecessors: List<StateBundle.OwnPredecessorSeed>,
    ) {
        public fun zero() {
            grantToken.fill(0)
            phraseEntropy.fill(0)
            predecessors.forEach { it.seed.fill(0) }
        }

        override fun toString(): String = "RecoveryRow.Parsed(<redacted>, ${predecessors.size} earlier keys)"
    }

    /** The core's calls, a port so host tests run without the native library. */
    public interface Codec {
        public fun exporterLabel(): String

        public fun exporterKeyLen(): UInt

        /** The plaintext; throws on a bad length, a duplicate, too many. */
        public fun compose(
            grant: ByteArray,
            phraseEntropy: ByteArray,
            predecessors: List<StateBundle.OwnPredecessorSeed>,
        ): ByteArray

        /** The opened plaintext parsed; throws when the core refuses it. */
        public fun decode(plaintext: ByteArray): Parsed

        public fun wrap(sealed: ByteArray): ByteArray

        /** The sealed blob, `null` for a row that is not a recovery row;
         *  throws [Refused] for a recovery row of another version. */
        public fun unwrap(row: ByteArray): ByteArray?
    }

    /** The production [Codec]: the native core. */
    public object Ffi : Codec {
        override fun exporterLabel(): String = uniffi.lakko_ffi_uniffi.recoveryExporterLabel()

        override fun exporterKeyLen(): UInt = uniffi.lakko_ffi_uniffi.recoveryExporterKeyLen()

        override fun compose(
            grant: ByteArray,
            phraseEntropy: ByteArray,
            predecessors: List<StateBundle.OwnPredecessorSeed>,
        ): ByteArray = uniffi.lakko_ffi_uniffi.composeRecoveryPayload(
            grant,
            phraseEntropy,
            predecessors.map { FfiRecoveryPredecessor(it.seed, it.adoptedAtMs.toULong()) },
        )

        override fun decode(plaintext: ByteArray): Parsed {
            val decoded = uniffi.lakko_ffi_uniffi.decodeRecoveryPayload(plaintext)
            return Parsed(
                grantToken = decoded.grant,
                phraseEntropy = decoded.phraseEntropy,
                predecessors = decoded.predecessors.map {
                    StateBundle.OwnPredecessorSeed(it.seed, it.adoptedAtMs.toLong())
                },
            )
        }

        override fun wrap(sealed: ByteArray): ByteArray = uniffi.lakko_ffi_uniffi.wrapRecoveryRow(sealed)

        override fun unwrap(row: ByteArray): ByteArray? =
            try {
                uniffi.lakko_ffi_uniffi.unwrapRecoveryRow(row)
            } catch (e: FfiException) {
                throw Refused(e)
            }
    }

    /** Whether [row] is a recovery row of any version (the other polls skip
     *  it). */
    public fun isRecoveryRow(row: ByteArray, codec: Codec = Ffi): Boolean =
        try {
            codec.unwrap(row) != null
        } catch (_: Refused) {
            true
        }

    /** The confirming side's payload: the grant, the phrase entropy and the
     *  own earlier keys [predecessors] (oldest first, at most 8). */
    public fun compose(
        grant: ByteArray,
        phraseEntropy: ByteArray,
        predecessors: List<me.lakko.app.auth.OwnPredecessor>,
        codec: Codec = Ffi,
    ): ByteArray {
        val carried = predecessors.map { StateBundle.OwnPredecessorSeed(it.phraseSeed.copyOf(), it.supersededAtMs) }
        return try {
            codec.compose(grant, phraseEntropy, carried)
        } finally {
            carried.forEach { it.seed.fill(0) }
        }
    }

    /**
     * Sender side: PUT the sealed payload to the rendezvous mailbox after the
     * Welcome (and the optional history bundle).
     */
    public suspend fun publish(
        rendezvousPseudonym: ByteArray,
        sealedBlob: ByteArray,
        ds: DsClient = DsClient(),
        codec: Codec = Ffi,
    ) {
        ds.use { client ->
            client.publishRendezvous(
                rendezvous = rendezvousPseudonym,
                payload = codec.wrap(sealedBlob),
            )
        }
    }

    /** Outcome of [poll]. */
    public sealed interface Outcome {
        public class Received(public val sealedBlob: ByteArray) : Outcome

        /** Only recovery rows the core refused (v2, or empty) came before
         *  the timeout: the keys could not be handed over. */
        public data object Refused : Outcome

        public data object Timeout : Outcome

        public data object Cancelled : Outcome
    }

    /**
     * Joining side: poll the rendezvous mailbox past the Welcome cursor for
     * the recovery row; rows that are not one are skipped. Anyone who learned
     * the rendezvous id can write to it, so a recovery row the core refuses,
     * or whose sealed blob [accepts] says no to (it does not open under the
     * joined group's key), is passed over too and the read goes on; a throw
     * from [accepts] counts as no. [Outcome.Refused] is the answer only when
     * the time ran out after a refused row and no row that opens.
     */
    public suspend fun poll(
        rendezvousPseudonym: ByteArray,
        afterCursor: Long,
        timeoutMillis: Long = POLL_TIMEOUT_MS,
        ds: DsClient = DsClient(),
        codec: Codec = Ffi,
        accepts: (ByteArray) -> Boolean = { true },
    ): Outcome = ds.use { client ->
        val deadline = System.nanoTime() + timeoutMillis * 1_000_000L
        var cursor = afterCursor
        var sawRefused = false
        while (true) {
            if (!currentCoroutineContext().isActive) return@use Outcome.Cancelled
            val remainingNanos = deadline - System.nanoTime()
            if (remainingNanos <= 0L) return@use if (sawRefused) Outcome.Refused else Outcome.Timeout
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
                val sealed = try {
                    codec.unwrap(first.payload)
                } catch (_: RecoveryRow.Refused) {
                    sawRefused = true
                    null
                }
                if (sealed != null && acceptsRow(sealed, accepts)) return@use Outcome.Received(sealed)
                cursor = maxOf(cursor, first.id)
                continue
            }
            cursor = result.messages.fold(cursor) { acc, msg -> maxOf(acc, msg.id) }
        }
        @Suppress("UNREACHABLE_CODE")
        Outcome.Timeout
    }

    private fun acceptsRow(sealed: ByteArray, accepts: (ByteArray) -> Boolean): Boolean =
        try {
            accepts(sealed)
        } catch (_: Throwable) {
            false
        }

    /** Whether [sealed] opens and parses under [exporterKey] ([open] is
     *  `openWithKey`). Everything opened is zeroed. */
    public fun opensUnder(
        exporterKey: ByteArray,
        sealed: ByteArray,
        open: (ByteArray, ByteArray) -> ByteArray,
        codec: Codec = Ffi,
    ): Boolean =
        try {
            this.open(exporterKey, sealed, open, codec).zero()
            true
        } catch (_: Throwable) {
            false
        }

    /** Open a sealed blob under the exporter key [exporterKey] with
     *  [open] (`openWithKey`) and parse it. The opened plaintext is zeroed. */
    public fun open(
        exporterKey: ByteArray,
        sealed: ByteArray,
        open: (ByteArray, ByteArray) -> ByteArray,
        codec: Codec = Ffi,
    ): Parsed {
        val plaintext = open(exporterKey, sealed)
        return try {
            codec.decode(plaintext)
        } finally {
            plaintext.fill(0)
        }
    }
}
