// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import me.lakko.app.backup.PHRASE_ENTROPY_LENGTH
import uniffi.lakko_ffi_uniffi.RecoveryMnemonic

/** Length of the AccountIdentity (BIP39 phrase) seed, ADR-0017. */
public const val ACCOUNT_IDENTITY_SEED_LENGTH: Int = 64

/**
 * What the another-device transfer (recovery payload v2) gives the receiving
 * device once its 16-byte entropy is expanded: the canonical 12-word phrase
 * and the 64-byte AccountIdentity seed. The holder owns zeroing [seed].
 */
public class TransferredKeys(
    public val phrase: String,
    public val seed: ByteArray,
)

/**
 * Rebuild the phrase and the seed from transferred [entropy].
 *
 * CONSUMES [entropy]: it is zeroed on every path. Refuses any length other
 * than 16 bytes (a v1 payload carried a 64-byte seed here; there is no dual
 * accept) and a core answer whose seed is not 64 bytes, zeroing that seed.
 * Nothing is written anywhere; the caller checks the seed against the
 * published identity before any store.
 *
 * [expand] is the core: `RecoveryMnemonic.fromEntropy(e)` to its phrase and
 * `toSeed()`.
 */
public fun keysFromTransferredEntropy(
    entropy: ByteArray,
    expand: (ByteArray) -> Pair<String, ByteArray> = ::expandEntropyWithCore,
): TransferredKeys {
    try {
        if (entropy.size != PHRASE_ENTROPY_LENGTH) {
            throw IllegalArgumentException(
                "transferred phrase entropy must be $PHRASE_ENTROPY_LENGTH bytes (got ${entropy.size})",
            )
        }
        val (phrase, seed) = expand(entropy)
        if (seed.size != ACCOUNT_IDENTITY_SEED_LENGTH) {
            seed.fill(0)
            throw IllegalStateException("the transferred phrase gave a seed of the wrong length")
        }
        return TransferredKeys(phrase = phrase, seed = seed)
    } finally {
        entropy.fill(0)
    }
}

private fun expandEntropyWithCore(entropy: ByteArray): Pair<String, ByteArray> =
    RecoveryMnemonic.fromEntropy(entropy).use { mnemonic -> mnemonic.phrase() to mnemonic.toSeed() }
