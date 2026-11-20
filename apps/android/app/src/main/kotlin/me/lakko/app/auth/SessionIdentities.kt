// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context
import uniffi.lakko_ffi_uniffi.Identity

/**
 * ADR-0050 — the session's two identities.
 *
 * The ACCOUNT identity is phrase-seeded (ADR-0017): contact requests and acks,
 * the account mailbox, invite capabilities, the discovery bundle, the safety
 * number. The DEVICE identity is a per-install random seed ([DeviceIdentityStore])
 * bound to the account key by a signed binding carried as its leaf credential:
 * every conversation group and every published KeyPackage use it, so a person's
 * devices are distinct MLS leaves that all resolve to one account.
 *
 * Mirror of `installSessionIdentities` in `apps/web/src/auth/AuthContext.tsx`.
 */
public object SessionIdentities {

    /**
     * Build this device's bound conversation identity for [accountIdB64].
     *
     * Reuses the persisted binding so its `issued_at` stays stable; mints and
     * persists a new one under [account] (the conversation identity: the
     * OLDEST own predecessor after a rotation) when none is stored, the stored
     * one no longer verifies, or the core's `ownBindingVerdict` says so. Returns
     * `null` when the device seed cannot be read or created, in which case the
     * caller falls back to an unbound account-key identity (the pre-ADR-0050
     * behaviour).
     *
     * G-56: the verdict's link is what this device last read of the account's
     * own conversation link ([link], from [OwnConversationKeyStore]). A
     * `wait` mints, stores and publishes nothing: [EarlierKeysMissing] is
     * thrown (the auth gate parked the device before it got here; this is
     * defence in depth). A `mint` names the key to bind under; [seedOfKey]
     * hands the seed of the account's own identity with that key (a fresh
     * copy, zeroed here).
     *
     * @param currentKey the key the account publishes now.
     * @param predecessorKeys the own predecessor keys, oldest first.
     * @param holdsConversations whether this device is in any conversation.
     */
    public fun deviceIdentity(
        context: Context,
        accountIdB64: String,
        account: Identity,
        currentKey: ByteArray,
        predecessorKeys: List<ByteArray>,
        holdsConversations: Boolean,
        link: uniffi.lakko_ffi_uniffi.FfiOwnLink = uniffi.lakko_ffi_uniffi.FfiOwnLink.Absent,
        seedOfKey: (ByteArray) -> ByteArray? = { null },
        nowUnixSecs: Long = System.currentTimeMillis() / 1_000L,
    ): Identity? {
        val seed = try {
            DeviceIdentityStore.ensureDeviceIdentitySeed(context, accountIdB64)
        } catch (cause: Throwable) {
            android.util.Log.w("SessionIdentities", "device seed unavailable", cause)
            return null
        }
        try {
            val stored = try {
                DeviceIdentityStore.getDeviceBinding(context, accountIdB64)
            } catch (_: Throwable) {
                null
            }
            val rebuilt = stored?.let {
                try {
                    Identity.fromSeedWithBinding(seed, it)
                } catch (cause: Throwable) {
                    android.util.Log.w("SessionIdentities", "stored device binding rejected", cause)
                    null
                }
            }
            // ADR-0073 section 2.3 step 5 / ADR-0074 part 3 / G-56: the core
            // decides whether the stored binding stays, a new one is minted,
            // and under which of the account's own keys; or that this device
            // must wait for the account's earlier keys.
            val verdict = try {
                uniffi.lakko_ffi_uniffi.ownBindingVerdict(
                    rebuilt?.accountPublicKey(),
                    currentKey,
                    predecessorKeys,
                    link,
                    holdsConversations,
                )
            } catch (cause: Throwable) {
                rebuilt?.close()
                android.util.Log.w("SessionIdentities", "binding verdict unavailable; minting nothing")
                throw EarlierKeysMissing()
            }
            val mintKey: ByteArray = when (verdict) {
                uniffi.lakko_ffi_uniffi.FfiBindingVerdict.Keep ->
                    rebuilt?.let { return it } ?: throw EarlierKeysMissing()
                uniffi.lakko_ffi_uniffi.FfiBindingVerdict.KeepUnderCurrentKey -> {
                    android.util.Log.i(
                        "SessionIdentities",
                        "device binding names the current account key and this device holds conversations; kept",
                    )
                    rebuilt?.let { return it } ?: throw EarlierKeysMissing()
                }
                uniffi.lakko_ffi_uniffi.FfiBindingVerdict.Wait -> {
                    rebuilt?.close()
                    android.util.Log.w("SessionIdentities", "G-56: the account's earlier keys are missing; nothing minted")
                    throw EarlierKeysMissing()
                }
                is uniffi.lakko_ffi_uniffi.FfiBindingVerdict.Mint -> {
                    rebuilt?.close()
                    if (stored != null) {
                        android.util.Log.w("SessionIdentities", "stored device binding is not the one to keep, re-minting")
                    }
                    verdict.key
                }
            }
            // The identity to sign the binding with: [account] when it is the
            // key the core named, else the account's own identity with that key.
            val signer: Identity = if (account.signaturePublicKey().contentEquals(mintKey)) {
                account
            } else {
                val named = seedOfKey(mintKey) ?: throw EarlierKeysMissing()
                try {
                    Identity.fromSeed(named)
                } finally {
                    named.fill(0)
                }
            }
            val accountSeed = signer.ed25519Seed()
            val bound = try {
                Identity.fromSeedBound(seed, accountSeed, nowUnixSecs.toULong())
            } catch (cause: Throwable) {
                android.util.Log.w("SessionIdentities", "device binding mint failed", cause)
                return null
            } finally {
                accountSeed.fill(0)
                if (signer !== account) signer.close()
            }
            bound.deviceBinding()?.let { binding ->
                try {
                    DeviceIdentityStore.putDeviceBinding(context, accountIdB64, binding)
                } catch (cause: Throwable) {
                    android.util.Log.w("SessionIdentities", "device binding persist failed", cause)
                }
            }
            return bound
        } finally {
            seed.fill(0)
        }
    }

    /**
     * A throwaway rebuild of this device's conversation identity for a process
     * that must not write anything (the ADR-0041 background decrypt). Uses the
     * stored binding when present; never mints or persists one. `null` when the
     * device seed is absent.
     */
    public fun deviceIdentityReadOnly(context: Context, accountIdB64: String): Identity? {
        val seed = try {
            DeviceIdentityStore.getDeviceIdentitySeed(context, accountIdB64)
        } catch (_: Throwable) {
            null
        } ?: return null
        try {
            val stored = try {
                DeviceIdentityStore.getDeviceBinding(context, accountIdB64)
            } catch (_: Throwable) {
                null
            }
            if (stored != null) {
                try {
                    return Identity.fromSeedWithBinding(seed, stored)
                } catch (_: Throwable) {
                    // Fall through: decrypt only needs the leaf key.
                }
            }
            return Identity.fromSeed(seed)
        } finally {
            seed.fill(0)
        }
    }
}
