// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.recovery

import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.RecoveryMnemonic
import uniffi.lakko_ffi_uniffi.Share
import uniffi.lakko_ffi_uniffi.combine
import uniffi.lakko_ffi_uniffi.decodeShare
import uniffi.lakko_ffi_uniffi.encodeShare
import uniffi.lakko_ffi_uniffi.split

/**
 * Shamir social-recovery glue (Phase 1.21c3).
 *
 * Mirror of:
 *
 *   * `apps/web/src/recovery/shamir.ts` (1.21c)
 *   * `apps/ios/Lakko/Recovery/ShamirBridge.swift` (1.21c2)
 *
 * Joins three pieces from the UniFFI surface (1.21b):
 *
 *   1. [RecoveryMnemonic] — `toEntropy()` extracts the 32-byte
 *      BIP39 entropy. `fromEntropy(...)` rebuilds the canonical
 *      phrase from raw entropy.
 *   2. [split] + [encodeShare] — produce N human-typable share
 *      strings of the form `<index>:<base64>`. Default Lakko
 *      config is 3-of-5.
 *   3. [decodeShare] + [combine] — the inverse path: K strings →
 *      reconstructed entropy → `RecoveryMnemonic.fromEntropy(...)`
 *      → standard recovery flow against `lakko-svc-auth`.
 */
public object ShamirBridge {

    /** Default Lakko social-recovery configuration. */
    public const val DEFAULT_THRESHOLD: UByte = 3u
    public const val DEFAULT_SHARE_COUNT: UByte = 5u

    /**
     * Run the full split flow: parse the phrase, extract
     * entropy, split into [shares] shares with [threshold]
     * threshold, encode each share to the human-typable wire
     * form.
     */
    public fun splitMnemonic(
        phrase: String,
        threshold: UByte = DEFAULT_THRESHOLD,
        shares: UByte = DEFAULT_SHARE_COUNT,
    ): List<String> {
        val mnemonic: RecoveryMnemonic = try {
            RecoveryMnemonic.fromPhrase(phrase)
        } catch (e: FfiException) {
            throw ShamirBridgeException("invalid recovery phrase", e)
        }
        val entropy: ByteArray = try {
            mnemonic.toEntropy()
        } finally {
            mnemonic.close()
        }
        return try {
            val splitShares: List<Share> = try {
                split(secret = entropy, threshold = threshold, nShares = shares)
            } catch (e: FfiException) {
                throw ShamirBridgeException("share split failed", e)
            }
            splitShares.map { encodeShare(it) }
        } finally {
            entropy.fill(0)
        }
    }

    /**
     * Run the full combine flow: decode each share string,
     * combine to the 32-byte entropy, rebuild the canonical
     * mnemonic phrase, return it.
     *
     * **Information-theoretic property**: with fewer than the
     * original split's threshold, `combine` returns garbage
     * entropy. `from_entropy` accepts any 32-byte buffer (BIP39
     * checksum is computed at the constructor, not verified),
     * so this helper returns a syntactically-valid 24-word
     * phrase that just doesn't OPAQUE-authenticate against the
     * server. Indistinguishable from a wrong-share failure by
     * design.
     */
    public fun combineSharesToMnemonic(encodedShares: List<String>): String {
        val decoded: List<Share> = encodedShares
            .map { it.trim() }
            .filter { it.isNotEmpty() }
            .map { line ->
                try {
                    decodeShare(line)
                } catch (e: FfiException) {
                    throw ShamirBridgeException("couldn't decode share", e)
                }
            }
        val entropy: ByteArray = try {
            combine(decoded)
        } catch (e: FfiException) {
            throw ShamirBridgeException("couldn't combine shares", e)
        }
        return try {
            val mnemonic = try {
                RecoveryMnemonic.fromEntropy(entropy)
            } catch (e: FfiException) {
                throw ShamirBridgeException(
                    "reconstructed entropy invalid",
                    e,
                )
            }
            try {
                mnemonic.phrase()
            } finally {
                mnemonic.close()
            }
        } finally {
            entropy.fill(0)
        }
    }
}

/** Errors surfaced by the Shamir bridge. The `cause` carries
 *  the underlying [FfiException] for diagnostics. */
public class ShamirBridgeException(
    message: String,
    cause: Throwable,
) : RuntimeException(message, cause)
