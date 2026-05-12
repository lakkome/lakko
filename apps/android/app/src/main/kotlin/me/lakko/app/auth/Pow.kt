// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import uniffi.lakko_ffi_uniffi.powSolve

/**
 * PoW solver glue — wraps the UniFFI `powSolve` free function into
 * a coroutine-friendly suspend helper.
 *
 * Mirror of `apps/web/src/auth/pow.ts`. The Rust solver is
 * **blocking**: at the server-default `effort_bits = 8` it takes
 * 100–500 ms on a 2024+ phone; at higher difficulties it can take
 * seconds. We push the call to `Dispatchers.Default` (CPU-bound
 * worker pool) so the main thread never blocks during a signup /
 * login flow.
 *
 * The function is parameterised by [dispatcher] for two reasons:
 *
 *   1. Tests can pass `Dispatchers.Unconfined` (or a `TestDispatcher`)
 *      to keep `runTest` deterministic.
 *   2. Future-us may want to swap in a dedicated solver pool if
 *      the default-dispatcher concurrency cap (one thread per
 *      core, shared with every other CPU-bound coroutine in the
 *      app) becomes a bottleneck. Today's signup / login flows
 *      call this exactly twice and once respectively per
 *      operation, so the default is fine.
 */
public suspend fun solvePow(
    seedB64: String,
    effortBits: Int,
    dispatcher: CoroutineDispatcher = Dispatchers.Default,
): PowProofPayload =
    withContext(dispatcher) {
        // UniFFI surfaces `effort_bits` as `kotlin.UByte` per the
        // Rust-side `u8`. The wire DTO stays `Int` to mirror the
        // server JSON shape (`serde` emits a JSON number); convert
        // at the FFI boundary.
        val proof = powSolve(seedB64, effortBits.toUByte())
        try {
            PowProofPayload(
                seed = proof.seedB64(),
                nonce = proof.nonceB64(),
                solution = proof.solutionB64(),
            )
        } finally {
            // UniFFI `Object`-typed handles hold a Rust-side `Arc`
            // that the auto-cleaner will release eventually, but
            // PoW proofs are tiny short-lived objects we don't
            // want backing up the cleaner's queue under heavy
            // signup load. Closing here is a small but explicit
            // gesture.
            proof.close()
        }
    }
