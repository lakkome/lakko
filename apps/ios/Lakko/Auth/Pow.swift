// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// PoW solver glue — wraps the UniFFI `powSolve` free function into
/// a Swift-async-friendly helper.
///
/// Mirror of:
///
///   * `apps/web/src/auth/pow.ts` (1.17d4)
///   * `apps/android/.../auth/Pow.kt` (1.15d2)
///
/// The Rust solver is **blocking**: at the server-default
/// `effort_bits = 8` it takes 100–500 ms on a 2024+ phone; at higher
/// difficulties it can take seconds. We hop onto a detached task at
/// `userInitiated` priority so the main thread never blocks during
/// a signup / login flow.
///
/// The Kotlin side takes a `CoroutineDispatcher` parameter so tests
/// can pass `Dispatchers.Unconfined`; Swift's Structured Concurrency
/// model doesn't expose dispatchers at the same shape, so the test
/// path uses `Task.detached(priority:)` and relies on
/// `withCheckedThrowingContinuation` for synchronization. For tests
/// that need deterministic ordering (1.14d4+ unit-test scaffold),
/// the harness uses `withDiscardingTaskGroup` instead of a custom
/// dispatcher injection.
public func solvePow(
    seedB64: String,
    effortBits: Int
) async throws -> PowProofPayload {
    // UniFFI surfaces `effort_bits` as `UInt8` per the Rust-side `u8`;
    // the wire DTO stays `Int` to mirror the server JSON shape
    // (`serde` emits a JSON number). Convert at the FFI boundary.
    let bits = UInt8(clamping: effortBits)
    let proof = try await Task.detached(priority: .userInitiated) {
        try powSolve(seedB64: seedB64, effortBits: bits)
    }.value
    // Swift ARC releases the UniFFI handle when the local goes out of
    // scope at end-of-function. No explicit `.close()` needed — the
    // pattern the Kotlin side uses (`try { … } finally { proof.close() }`)
    // is only required there because Kotlin lacks deterministic
    // destruction on object handles.
    return PowProofPayload(
        seed: proof.seedB64(),
        nonce: proof.nonceB64(),
        solution: proof.solutionB64()
    )
}
