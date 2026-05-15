// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Shamir social-recovery glue (Phase 1.21c2).
///
/// Mirror of `apps/web/src/recovery/shamir.ts` (1.21c). Joins
/// three pieces from the UniFFI surface (1.21b):
///
///   1. ``RecoveryMnemonic`` — `toEntropy()` extracts the 32-byte
///      BIP39 entropy from a freshly-generated signup phrase
///      or a user-typed recovery phrase. `fromEntropy(_:)`
///      rebuilds the canonical phrase from raw entropy.
///   2. ``split(secret:threshold:nShares:)`` + ``encodeShare(share:)``
///      — produce N human-typable share strings of the form
///      `<index>:<base64>`. Default Lakko configuration is
///      3-of-5.
///   3. ``decodeShare(encoded:)`` + ``combine(shares:)`` — the
///      inverse path: K strings → reconstructed entropy →
///      `RecoveryMnemonic.fromEntropy(...)` → standard recovery
///      flow against `lakko-svc-auth`.
public enum ShamirBridge {

    /// Default Lakko social-recovery configuration.
    public static let defaultThreshold: UInt8 = 3
    public static let defaultShareCount: UInt8 = 5

    /// Errors surfaced by the split / combine paths.
    public enum BridgeError: Error, LocalizedError {
        /// `Mnemonic.fromPhrase` failed (bad checksum, wrong
        /// word count, unknown wordlist entry — all elided into
        /// one variant per BIP39's no-probing property).
        case phraseInvalid(Error)
        /// `lakko_shamir::split` rejected the threshold / share
        /// count combo.
        case splitFailed(Error)
        /// `decodeShare` failed on one of the input strings
        /// (wrong shape, bad base64, index 0).
        case decodeFailed(Error)
        /// `combine` returned an error — duplicate indices,
        /// mismatched lengths, fewer than 2 shares.
        case combineFailed(Error)
        /// `Mnemonic.fromEntropy` rejected the combined bytes
        /// (wrong length). Should not happen if `split` was
        /// driven from a valid mnemonic; surfaces a "wrong
        /// shares" condition by collapsing to a syntactically-
        /// invalid entropy length.
        case entropyInvalid(Error)

        public var errorDescription: String? {
            switch self {
            case .phraseInvalid(let e): return "invalid recovery phrase: \(e)"
            case .splitFailed(let e):   return "share split failed: \(e)"
            case .decodeFailed(let e):  return "couldn't decode share: \(e)"
            case .combineFailed(let e): return "couldn't combine shares: \(e)"
            case .entropyInvalid(let e): return "reconstructed entropy invalid: \(e)"
            }
        }
    }

    /// Run the full split flow: parse the phrase, extract
    /// entropy, split into `n` shares with threshold `k`,
    /// encode each share to its human-typable wire form.
    ///
    /// - Parameters:
    ///   - phrase: The 24-word BIP39 mnemonic — typically the
    ///             `recoveryPhrase` from ``AuthRepository.signup``
    ///             or a pasted phrase.
    ///   - threshold: Minimum shares to reconstruct (default `3`).
    ///   - shares: Total shares to produce (default `5`).
    public static func splitMnemonic(
        phrase: String,
        threshold: UInt8 = defaultThreshold,
        shares: UInt8 = defaultShareCount
    ) throws -> [String] {
        let mnemonic: RecoveryMnemonic
        do {
            mnemonic = try RecoveryMnemonic.fromPhrase(phrase: phrase)
        } catch {
            throw BridgeError.phraseInvalid(error)
        }
        var entropy = mnemonic.toEntropy()
        defer { entropy.resetBytes(in: 0..<entropy.count) }

        let splitShares: [Share]
        do {
            splitShares = try split(
                secret: entropy,
                threshold: threshold,
                nShares: shares
            )
        } catch {
            throw BridgeError.splitFailed(error)
        }
        return splitShares.map { encodeShare(share: $0) }
    }

    /// Run the full combine flow: decode each share string,
    /// combine to the 32-byte entropy, rebuild the canonical
    /// mnemonic phrase, return it.
    ///
    /// **Information-theoretic property**: with fewer than the
    /// original split's threshold, `combine` returns garbage
    /// entropy. `from_entropy` accepts any 32-byte buffer
    /// (BIP39's 8-bit checksum is computed at the constructor,
    /// not verified), so this helper returns a syntactically-
    /// valid 24-word phrase that just doesn't OPAQUE-authenticate
    /// against the server. Indistinguishable from a wrong-share
    /// failure by design.
    public static func combineSharesToMnemonic(
        encodedShares: [String]
    ) throws -> String {
        var decoded: [Share] = []
        decoded.reserveCapacity(encodedShares.count)
        for line in encodedShares {
            let trimmed = line.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !trimmed.isEmpty else { continue }
            do {
                let share = try decodeShare(encoded: trimmed)
                decoded.append(share)
            } catch {
                throw BridgeError.decodeFailed(error)
            }
        }

        var entropy: Data
        do {
            entropy = try combine(shares: decoded)
        } catch {
            throw BridgeError.combineFailed(error)
        }
        defer { entropy.resetBytes(in: 0..<entropy.count) }

        let mnemonic: RecoveryMnemonic
        do {
            mnemonic = try RecoveryMnemonic.fromEntropy(entropy: entropy)
        } catch {
            throw BridgeError.entropyInvalid(error)
        }
        return mnemonic.phrase()
    }
}
