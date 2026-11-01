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
     * one no longer verifies, or [bindingVerdict] says so. Returns `null` when
     * the device seed cannot be read or created, in which case the caller
     * falls back to an unbound account-key identity (the pre-ADR-0050
     * behaviour).
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
            if (stored != null) {
                try {
                    val rebuilt = Identity.fromSeedWithBinding(seed, stored)
                    // ADR-0073 section 2.3 step 5 — a binding is signed by the
                    // account key, so after a reset of the account keys the
                    // stored one still verifies (the old key signed it) but
                    // names an account key that is no longer the account's.
                    // Re-mint rather than publish a KeyPackage bound to the
                    // old generation. ADR-0074 part 3 — a rotation is not a
                    // reset: a binding under the account's own earlier key
                    // stays, and one under the current key is minted again
                    // only while this device holds no conversation.
                    val verdict = bindingVerdict(
                        boundKey = rebuilt.accountPublicKey(),
                        currentKey = currentKey,
                        predecessorKeys = predecessorKeys,
                        holdsConversations = holdsConversations,
                    )
                    when (verdict) {
                        BindingVerdict.Keep -> return rebuilt
                        BindingVerdict.KeepUnderCurrentKey -> {
                            android.util.Log.i(
                                "SessionIdentities",
                                "device binding names the current account key and this device holds conversations; kept",
                            )
                            return rebuilt
                        }
                        BindingVerdict.Remint, BindingVerdict.Mint -> {
                            rebuilt.close()
                            android.util.Log.w(
                                "SessionIdentities",
                                "stored device binding is not under the conversation key ($verdict), re-minting",
                            )
                        }
                    }
                } catch (cause: Throwable) {
                    android.util.Log.w(
                        "SessionIdentities",
                        "stored device binding rejected, re-minting",
                        cause,
                    )
                }
            }
            val accountSeed = account.ed25519Seed()
            val bound = try {
                Identity.fromSeedBound(seed, accountSeed, nowUnixSecs.toULong())
            } catch (cause: Throwable) {
                android.util.Log.w("SessionIdentities", "device binding mint failed", cause)
                return null
            } finally {
                accountSeed.fill(0)
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
