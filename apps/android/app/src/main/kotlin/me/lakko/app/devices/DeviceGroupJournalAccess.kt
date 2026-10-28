// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import me.lakko.app.ds.DsClient
import me.lakko.app.ds.DsJournalSigner
import me.lakko.app.ds.FfiDsJournalSigner
import me.lakko.app.ds.DsStatementSigning
import me.lakko.app.ds.FfiDsStatementSigning
import uniffi.lakko_ffi_uniffi.DeviceGroup
import uniffi.lakko_ffi_uniffi.Identity

/**
 * ADR-0053 — the DeviceGroup journal is claimed like any group journal, under
 * the per-epoch access key `DeviceGroup.dsAccessSigner` derives.
 *
 * ADR-0057 — the DeviceGroup's owner is the ACCOUNT, so every device of it
 * derives the same journal ROOT from the account identity seed and registers
 * every epoch with it; there are no registrars (and nothing to revoke).
 *
 * The DeviceGroup's commit paths ([PairingOrchestrator], [DeviceRemoval]) go
 * through the commit outbox ([DeviceGroupCommits], ADR-0081): the N+1
 * registration and the commit row reach the DS in one request, proven under
 * N. Nothing here registers an epoch on its own.
 */
internal object DeviceGroupJournalAccess {
    /** The [me.lakko.app.ds.DsSignerRegistry] owner tag of the sync
     *  repository's installed source. */
    const val REGISTRY_OWNER: String = "devicegroup"

    /** [group]'s journal signer at its current epoch, or `null` when the
     *  handle cannot derive one. */
    fun signerOf(group: DeviceGroup, identity: Identity): DsJournalSigner? =
        try {
            FfiDsJournalSigner(group.dsAccessSigner(identity))
        } catch (_: Throwable) {
            null
        }

    /**
     * ADR-0057 — the DeviceGroup journal's root signer, derived from this
     * account's identity seed (Keystore-wrapped on the device), or `null` when
     * the seed is not on this device.
     */
    fun rootOf(context: android.content.Context, accountIdB64: String, groupId: ByteArray): DsStatementSigning? {
        val seed = me.lakko.app.auth.AccountIdentitySeedStore.getAccountIdentitySeed(context, accountIdB64)
            ?: return null
        return try {
            rootOfSeed(seed, groupId)
        } finally {
            seed.fill(0)
        }
    }

    private const val FOUNDING_PREFS: String = "me.lakko.devicegroup.founding"
    private const val FOUNDING_KEY_PREFIX: String = "unconfirmed-v1:"

    /**
     * ADR-0057 second security pass, finding 3 — a DeviceGroup founded on
     * this device whose journal claim has not landed yet. Set by
     * [DeviceGroupLifecycle] BEFORE the founding snapshot is persisted, cleared
     * once a claim (or a root reset) answers 204. While it is set no Welcome
     * may leave for the DeviceGroup ([claimFoundingIfUnconfirmed]); the
     * founding is kept because it can be claimed on retry without any Welcome
     * having left. Account ids only, nothing secret.
     */
    fun markFoundingUnconfirmed(context: android.content.Context, accountIdB64: String) {
        foundingPrefs(context).edit().putBoolean(FOUNDING_KEY_PREFIX + accountIdB64, true).commit()
    }

    fun isFoundingUnconfirmed(context: android.content.Context, accountIdB64: String): Boolean =
        try {
            foundingPrefs(context).getBoolean(FOUNDING_KEY_PREFIX + accountIdB64, false)
        } catch (_: Throwable) {
            false
        }

    fun clearFoundingUnconfirmed(context: android.content.Context, accountIdB64: String) {
        foundingPrefs(context).edit().remove(FOUNDING_KEY_PREFIX + accountIdB64).apply()
    }

    private fun foundingPrefs(context: android.content.Context): android.content.SharedPreferences =
        context.applicationContext.getSharedPreferences(FOUNDING_PREFS, android.content.Context.MODE_PRIVATE)

    /**
     * Finding 3 — before a DeviceGroup commit (and so before its Welcome) on a
     * founding whose claim has not landed: claim with the account's root (a
     * `409` resets the chain an older DeviceGroup under the same id left) and
     * require the 204. Throws when it does not land; the caller then commits
     * and publishes nothing.
     */
    suspend fun claimFoundingIfUnconfirmed(
        context: android.content.Context,
        accountIdB64: String,
        group: DeviceGroup,
        identity: Identity,
        ds: DsClient,
    ) {
        me.lakko.app.ds.claimFoundingIfUnconfirmed(
            unconfirmed = isFoundingUnconfirmed(context, accountIdB64),
            claim = {
                val groupId = group.groupId()
                val signer = signerOf(group, identity)
                    ?: throw me.lakko.app.ds.DsRegistrationUnavailable("the DeviceGroup access key could not be derived")
                val root = rootOf(context, accountIdB64, groupId)
                    ?: throw me.lakko.app.ds.DsRegistrationUnavailable("the DeviceGroup root is not on this device")
                val outcome = me.lakko.app.ds.claimOrReset(
                    journalId = groupId,
                    epoch = signer.epoch,
                    epochKey = signer.verifyingKey,
                    statements = root,
                    refounded = true,
                    put = { make -> ds.putStatement(make) },
                )
                if (outcome != me.lakko.app.ds.JournalClaimOutcome.ALREADY_CLAIMED) {
                    onJournalRootLanded(context, accountIdB64)
                }
                outcome
            },
            clear = { clearFoundingUnconfirmed(context, accountIdB64) },
        )
    }

    /** A claim of this account's DeviceGroup journal answered 204: the
     *  reset's DS step, when one was owed, is done. */
    fun onJournalRootLanded(context: android.content.Context, accountIdB64: String) {
        me.lakko.app.auth.KeyResetOwedStore.onDevice(context)
            .markStep(accountIdB64, me.lakko.app.auth.KeyResetStep.DeviceGroupReset)
    }

    /**
     * ADR-0073 section 2.3 step 4 — the DeviceGroup after a reset of the
     * account keys, on the device that reset them. ADR-0078 section 6: the
     * DeviceGroup id is derived from the account seed, so the NEW seed names
     * a NEW journal (the reset cleared the own predecessors). The caller
     * dropped the old DeviceGroup ([DeviceGroupLifecycle.discardForKeyReset]);
     * this founds the new one with the SAME device identity (the
     * conversation leaves are unaffected) and claims its journal with the
     * NEW root, a plain claim (a `409` from an earlier attempt of this device
     * resets the chain the same root left). The founding stays unconfirmed
     * until the claim answers 204 (no Welcome may leave before that).
     *
     * Returns what the DS said. [accountIdentitySeed] is the NEW phrase seed
     * (read, never kept).
     */
    suspend fun refoundAfterKeyReset(
        context: android.content.Context,
        accountIdBytes: ByteArray,
        accountIdB64: String,
        accountIdentitySeed: ByteArray,
        ds: DsClient,
    ): me.lakko.app.auth.KeyResetStepOutcome {
        val deviceSeed = me.lakko.app.auth.DeviceIdentityStore.ensureDeviceIdentitySeed(context, accountIdB64)
        val ensured = try {
            DeviceGroupLifecycle.ensureDeviceGroup(
                context = context,
                accountIdBytes = accountIdBytes,
                accountIdB64 = accountIdB64,
                deviceIdentitySeed = deviceSeed,
                accountSeed = accountIdentitySeed,
                // ADR-0081 decision 4 — a key reset is one of the two cases
                // that found (the new seed names a new journal nobody holds).
                found = true,
            )
        } finally {
            deviceSeed.fill(0)
        }
        try {
            val groupId = ensured.group.groupId()
            val signer = signerOf(ensured.group, ensured.identity)
                ?: throw me.lakko.app.ds.DsRegistrationUnavailable("the DeviceGroup access key could not be derived")
            val root = rootOfSeed(accountIdentitySeed, groupId)
                ?: throw me.lakko.app.ds.DsRegistrationUnavailable("the DeviceGroup root could not be derived")
            val error: Throwable? = try {
                me.lakko.app.ds.requireClaim {
                    me.lakko.app.ds.claimOrReset(
                        journalId = groupId,
                        epoch = signer.epoch,
                        epochKey = signer.verifyingKey,
                        statements = root,
                        refounded = true,
                        put = { make -> ds.putStatement(make) },
                    )
                }
                null
            } catch (cancel: kotlinx.coroutines.CancellationException) {
                throw cancel
            } catch (cause: Throwable) {
                cause
            }
            val outcome = me.lakko.app.auth.deviceGroupResetOutcome(error, rootProbeConfirmed = false)
            if (me.lakko.app.auth.keyResetStepLanded(outcome)) {
                clearFoundingUnconfirmed(context, accountIdB64)
                onJournalRootLanded(context, accountIdB64)
            }
            return outcome
        } finally {
            ensured.group.close()
            ensured.identity.close()
        }
    }

    /** [rootOf] over an account identity seed already in hand. */
    fun rootOfSeed(accountIdentitySeed: ByteArray, groupId: ByteArray): DsStatementSigning? =
        try {
            me.lakko.app.auth.SessionSecretWipe.identity(accountIdentitySeed).use { FfiDsStatementSigning.root(it, groupId) }
        } catch (_: Throwable) {
            null
        }

}
