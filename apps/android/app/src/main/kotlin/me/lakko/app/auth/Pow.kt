// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import java.util.concurrent.Executor
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CompletableDeferred
import kotlin.coroutines.cancellation.CancellationException
import uniffi.lakko_ffi_uniffi.PowProof
import uniffi.lakko_ffi_uniffi.powSolveChunk

/**
 * The one proof of work left (ADR-0054 S4): a single solve before
 * `/signup/start`, the challenge from auth's `POST /pow/challenge`. Nothing
 * else in the app pays one.
 *
 * ## Cancellable (ADR-0054 S5)
 *
 * The solve runs OFF the main thread on its own executor as a loop of short
 * native chunks (`powSolveChunk`, at most [chunkAttempts] nonces each), and
 * the loop checks a cancel flag between chunks. Cancelling the caller (the UX
 * `Cancel`, leaving the screen) therefore does two things: the caller's wait
 * returns at once, and the CPU work stops at the end of the chunk in flight
 * rather than running on to a proof nobody will send. The server holds
 * nothing until a proof is spent, so a cancelled solve leaves no half-created
 * account.
 */
public suspend fun solvePow(
    seedB64: String,
    effortBits: Int,
    solver: PowSolver = UniffiPowSolver,
    executor: Executor = powExecutor,
    chunkAttempts: Int = POW_CHUNK_ATTEMPTS,
): PowProofPayload {
    val result = CompletableDeferred<PowProofPayload>()
    val cancelled = AtomicBoolean(false)
    executor.execute {
        try {
            while (!cancelled.get() && !result.isCompleted) {
                val proof = solver.solveChunk(seedB64, effortBits, chunkAttempts)
                if (proof != null) {
                    result.complete(proof)
                    return@execute
                }
            }
        } catch (t: Throwable) {
            result.completeExceptionally(t)
        }
    }
    try {
        return result.await()
    } catch (e: CancellationException) {
        // Stop the loop at the next chunk boundary, and make a late proof a
        // no-op: nobody will read it.
        cancelled.set(true)
        result.cancel(e)
        throw e
    }
}

/**
 * Nonces per native chunk.
 *
 * Chosen from the only per-attempt figure measured so far: about 16 ms per
 * Equix attempt with the JIT on the Android emulator (2026-09-27, the
 * `lakko-pow` JIT change). Four attempts is then about 64 ms, inside the
 * roughly 100 ms a Cancel should take to stop the CPU work, with headroom for
 * a slower phone. Not measured on a physical phone. A smaller chunk only adds
 * per-call overhead (each call sets up its solver memory once); the search is
 * the same, since every chunk draws fresh random nonces.
 */
public const val POW_CHUNK_ATTEMPTS: Int = 4

/** The native solver, behind an interface so the loop and its cancellation are
 *  host-testable. */
public fun interface PowSolver {
    /** Blocking. At most [maxAttempts] nonces; `null` when none of them
     *  produced a proof. */
    public fun solveChunk(seedB64: String, effortBits: Int, maxAttempts: Int): PowProofPayload?
}

/** [PowSolver] over the UniFFI `powSolveChunk`. */
public object UniffiPowSolver : PowSolver {
    override fun solveChunk(seedB64: String, effortBits: Int, maxAttempts: Int): PowProofPayload? {
        // UniFFI surfaces `effort_bits` as `UByte` (the Rust `u8`) and the
        // attempt budget as `UInt`.
        val proof: PowProof = powSolveChunk(seedB64, effortBits.toUByte(), maxAttempts.toUInt())
            ?: return null
        try {
            return PowProofPayload(
                seed = proof.seedB64(),
                nonce = proof.nonceB64(),
                solution = proof.solutionB64(),
            )
        } finally {
            proof.close()
        }
    }
}

/** Daemon threads, so a solve winding down after a cancel never holds the
 *  process open, and a retry never queues behind it. */
private val powExecutor: Executor = Executors.newCachedThreadPool { runnable ->
    Thread(runnable, "lakko-pow").apply { isDaemon = true }
}
