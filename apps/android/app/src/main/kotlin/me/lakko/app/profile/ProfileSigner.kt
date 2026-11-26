// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import uniffi.lakko_ffi_uniffi.FfiProfile
import uniffi.lakko_ffi_uniffi.Identity

/**
 * ADR-0101 section 8 — profile envelope v6 is signed by the ACCOUNT identity
 * (ADR-0050: the key the account's conversation leaves are bound under,
 * never a device leaf). Every place that composes a profile envelope goes
 * through [compose], so none of them needs to find the identity itself.
 *
 * Two sources, set once by `AppRepositories`:
 *  - [held]: the identity the chat layer holds for the account (borrowed,
 *    never closed here);
 *  - [sessionSeed]: the session's phrase seed, used when the chat layer holds
 *    none yet (an identity is derived for the one call and closed).
 *
 * An account id of `null` means the active account. With neither source set
 * (the JVM host tests) [compose] throws, which every caller treats like any
 * other refusal of the composer.
 */
public object ProfileSigner {
    @Volatile
    public var held: ((accountId: ByteArray?) -> Identity?)? = null

    @Volatile
    public var sessionSeed: ((accountId: ByteArray?) -> ByteArray?)? = null

    /** [profile] as a signed envelope of [accountId]'s account. */
    public fun compose(profile: FfiProfile, accountId: ByteArray?): ByteArray {
        held?.invoke(accountId)?.let {
            return uniffi.lakko_ffi_uniffi.composeProfileEnvelope(profile, it)
        }
        val seed = sessionSeed?.invoke(accountId)
            ?: throw IllegalStateException("no account identity to sign the profile with")
        return Identity.fromSeed(seed).use { uniffi.lakko_ffi_uniffi.composeProfileEnvelope(profile, it) }
    }
}
