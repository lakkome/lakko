// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// PoW solver glue - drives the UniFFI chunked solver (`powSolveChunk`) from
/// Swift async code.
///
/// Mirror of:
///
///   * `apps/web/src/auth/pow.ts` (1.17d4)
///   * `apps/android/.../auth/Pow.kt` (1.15d2)
///
/// Since ADR-0054 S4 the only caller is signup (`/signup/start`): it is
/// the one deliberately heavy wait in the app, and iOS has no JIT for the
/// solver (an app may not map executable memory), so it runs off the main
/// thread and is CANCELLABLE.
///
/// ADR-0054 S5: the solve is a loop of short chunks (`powSolveChunk`, at most
/// `maxAttempts` nonces per call, `nil` when none produced a proof) on a
/// background task, and the cancel flag is checked between chunks. `Cancel`
/// and leaving the signup screen therefore stop the CPU work itself within one
/// chunk, not only release the caller. The caller waits for that one chunk to
/// return (it is bounded by ``PowChunkSizer/targetNanos``) and then gets
/// `CancellationError`; no proof is ever spent, so the server holds nothing.
public func solvePow(
    seedB64: String,
    effortBits: Int,
    chunk: @escaping PowChunkSolver = corePowSolveChunk
) async throws -> PowProofPayload {
    // UniFFI surfaces `effort_bits` as `UInt8` per the Rust-side `u8`;
    // the wire DTO stays `Int` to mirror the server JSON shape
    // (`serde` emits a JSON number). Convert at the FFI boundary.
    let bits = UInt8(clamping: effortBits)
    try Task.checkCancellation()
    let worker = Task.detached(priority: .userInitiated) { () throws -> PowProofPayload in
        var sizer = PowChunkSizer()
        while true {
            // The cancel flag, between chunks: a cancelled worker starts no
            // further chunk.
            try Task.checkCancellation()
            let attempts = sizer.nextAttempts
            let started = DispatchTime.now().uptimeNanoseconds
            if let proof = try chunk(seedB64, bits, attempts) {
                return proof
            }
            sizer.record(
                attempts: attempts,
                elapsedNanos: DispatchTime.now().uptimeNanoseconds &- started
            )
        }
    }
    return try await withTaskCancellationHandler {
        try await worker.value
    } onCancel: {
        worker.cancel()
    }
}

/// One chunk of a solve: at most `maxAttempts` nonces, `nil` when none of them
/// produced a proof. Throws for a bad seed. A seam so a test can count chunks.
public typealias PowChunkSolver = @Sendable (
    _ seedB64: String,
    _ effortBits: UInt8,
    _ maxAttempts: UInt32
) throws -> PowProofPayload?

/// The core's chunked solver (`powSolveChunk`), as a ``PowChunkSolver``.
public let corePowSolveChunk: PowChunkSolver = { seedB64, effortBits, maxAttempts in
    // Swift ARC releases the UniFFI proof handle when it leaves scope.
    guard let proof = try powSolveChunk(
        seedB64: seedB64, effortBits: effortBits, maxAttempts: maxAttempts)
    else { return nil }
    return PowProofPayload(
        seed: proof.seedB64(),
        nonce: proof.nonceB64(),
        solution: proof.solutionB64()
    )
}

/// How many attempts the next chunk may take.
///
/// The chunk size is measured, not fixed: a fixed count cannot serve both a
/// device without the JIT (iOS, where one attempt is the interpreter's cost)
/// and a simulator or a future JIT build, which are much faster. The first
/// chunk is one attempt; after each chunk the per-attempt time just observed
/// sets the next chunk so it takes about ``targetNanos`` (100 ms) - short
/// enough that `Cancel` feels immediate, long enough that the per-call
/// overhead is negligible. Growth is capped at 4x per step so one unusually
/// fast chunk does not produce a long one, and the size stays within
/// ``minAttempts`` ... ``maxAttempts``.
struct PowChunkSizer: Equatable {
    /// The time one chunk should take.
    static let targetNanos: UInt64 = 100_000_000
    static let minAttempts: UInt32 = 1
    static let maxAttempts: UInt32 = 4096
    /// The most a chunk may grow over the previous one.
    static let maxGrowth: UInt32 = 4

    private(set) var nextAttempts: UInt32 = PowChunkSizer.minAttempts

    /// Record a finished chunk of `attempts` that took `elapsedNanos`.
    mutating func record(attempts: UInt32, elapsedNanos: UInt64) {
        let perAttempt = max(1, elapsedNanos / UInt64(max(1, attempts)))
        let fitting = Self.targetNanos / perAttempt
        let grown = UInt64(attempts) * UInt64(Self.maxGrowth)
        let next = min(fitting, grown, UInt64(Self.maxAttempts))
        nextAttempts = UInt32(max(UInt64(Self.minAttempts), next))
    }
}
