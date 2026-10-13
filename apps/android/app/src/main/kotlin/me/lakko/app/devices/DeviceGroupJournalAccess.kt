// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import me.lakko.app.ds.DsClient
import me.lakko.app.ds.DsJournalSigner
import me.lakko.app.ds.FfiDsJournalSigner
import me.lakko.app.ds.DsStatementSigning
import me.lakko.app.ds.FfiDsStatementSigning
import me.lakko.app.ds.registerEpochBeforePublish
import me.lakko.app.ds.KeyResetRootEvidence
import me.lakko.app.ds.withKeyResetEvidence
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
 * The DeviceGroup's commit paths ([PairingOrchestrator], [DeviceRemoval]) each
 * open their own short-lived handle, so they take the signers from that handle
 * explicitly: [signerOf] before the commit (epoch N) and after it (N+1), then
 * [registerNext] before anything is persisted or published.
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
                    keyResetEvidence = { rootKey -> pendingResetEvidence(context, accountIdB64, groupId, rootKey) },
                )
                if (outcome != me.lakko.app.ds.JournalClaimOutcome.ALREADY_CLAIMED) {
                    onJournalRootLanded(context, accountIdB64)
                }
                outcome
            },
            clear = { clearFoundingUnconfirmed(context, accountIdB64) },
        )
    }

    /**
     * ADR-0073 section 5 / ADR-0074 T5 item 21 - what a founding of this
     * account's DeviceGroup must carry on its `Reset`, because the journal's
     * chain on the DS belongs to the OLD root: the key-reset attestation and
     * the NEW account key's endorsement of [rootKey] (the reset's signer).
     * Present while a reset's DS step is still owed and the attestation is
     * within its 30-day bound. `null` otherwise (an ordinary same-root
     * re-founding), and `null` when the new seed is not on this device (the
     * DS then answers the uniform 401 and the step stays owed).
     */
    fun pendingResetEvidence(
        context: android.content.Context,
        accountIdB64: String,
        journalId: ByteArray,
        rootKey: ByteArray,
    ): KeyResetRootEvidence? {
        val owed = me.lakko.app.auth.KeyResetOwedStore.onDevice(context).load(accountIdB64) ?: return null
        if (owed.deviceGroupReset) return null
        val age = me.lakko.app.auth.attestationAgeAt(
            signedAtMs = owed.signedAtMs,
            nowMs = System.currentTimeMillis(),
            signedInAgainMs = null,
        )
        if (age != me.lakko.app.auth.AttestationAge.Fresh) return null
        val seed = me.lakko.app.auth.AccountIdentitySeedStore.getAccountIdentitySeed(context, accountIdB64)
            ?: return null
        return try {
            rootEvidence(seed, owed.attestationB64, owed.keyGeneration, journalId, rootKey)
        } finally {
            seed.fill(0)
        }
    }

    /**
     * The attestation with the endorsement of [rootKey] by the account
     * identity [accountIdentitySeed] derives (the key the attestation
     * names). `null` when the endorsement cannot be signed.
     */
    fun rootEvidence(
        accountIdentitySeed: ByteArray,
        attestationB64: String,
        keyGeneration: Long,
        journalId: ByteArray,
        rootKey: ByteArray,
    ): KeyResetRootEvidence? =
        try {
            val endorsement = Identity.fromSeed(accountIdentitySeed).use { identity ->
                val ed = identity.ed25519Seed()
                try {
                    uniffi.lakko_ffi_uniffi.signDsRootEndorsement(ed, journalId, keyGeneration.toULong(), rootKey)
                } finally {
                    ed.fill(0)
                }
            }
            KeyResetRootEvidence(attestationB64, me.lakko.app.auth.base64Encode(endorsement))
        } catch (_: Throwable) {
            null
        }

    /** A claim or an attested reset of this account's DeviceGroup journal
     *  answered 204: the reset's DS step, when one was owed, is done. */
    fun onJournalRootLanded(context: android.content.Context, accountIdB64: String) {
        me.lakko.app.auth.KeyResetOwedStore.onDevice(context)
            .markStep(accountIdB64, me.lakko.app.auth.KeyResetStep.DeviceGroupReset)
    }

    /**
     * ADR-0073 section 2.3 step 4 / section 5 — the DeviceGroup after a reset
     * of the account keys, on the device that reset them. The journal id is
     * unchanged (derived from the account id) but the chain on the DS belongs
     * to the OLD root, and the DeviceGroup this device holds was founded
     * under the old seed. So: drop that DeviceGroup, found a new one under the
     * same id with the SAME device identity (the conversation leaves are
     * unaffected), and re-root the journal with a `Reset` signed by the NEW
     * root carrying the attestation. The founding stays unconfirmed until the
     * reset answers 204 (no Welcome may leave before that); a later open claims
     * again with the stored attestation ([pendingResetEvidence]).
     *
     * Returns what the DS said; nothing is minted again on any outcome.
     * [accountIdentitySeed] is the NEW phrase seed (read, never kept).
     */
    suspend fun reRootAfterKeyReset(
        context: android.content.Context,
        accountIdBytes: ByteArray,
        accountIdB64: String,
        accountIdentitySeed: ByteArray,
        attestationB64: String,
        keyGeneration: Long,
        ds: DsClient,
    ): me.lakko.app.auth.KeyResetStepOutcome {
        val deviceSeed = me.lakko.app.auth.DeviceIdentityStore.ensureDeviceIdentitySeed(context, accountIdB64)
        val ensured = try {
            DeviceGroupLifecycle.ensureDeviceGroup(
                context = context,
                accountIdBytes = accountIdBytes,
                accountIdB64 = accountIdB64,
                deviceIdentitySeed = deviceSeed,
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
            // ADR-0074 T5 item 21 - the DS refuses the attestation without
            // the new account key's endorsement of the resetting root.
            val evidence = rootEvidence(accountIdentitySeed, attestationB64, keyGeneration, groupId, root.publicKey)
                ?: throw me.lakko.app.ds.DsRegistrationUnavailable("the root endorsement could not be signed")
            val error: Throwable? = try {
                ds.putStatement {
                    root.reset(groupId, signer.epoch, signer.verifyingKey)
                        .withKeyResetEvidence(evidence)
                }
                null
            } catch (cancel: kotlinx.coroutines.CancellationException) {
                throw cancel
            } catch (cause: Throwable) {
                cause
            }
            val probeConfirmed = error is me.lakko.app.ds.DsHttpError && error.status == HTTP_CONFLICT &&
                try {
                    me.lakko.app.ds.probeRoot(groupId, root) { make -> ds.putStatement(make) }
                } catch (cancel: kotlinx.coroutines.CancellationException) {
                    throw cancel
                } catch (_: Throwable) {
                    false
                }
            val outcome = me.lakko.app.auth.deviceGroupResetOutcome(error, probeConfirmed)
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

    private const val HTTP_CONFLICT: Int = 409

    /** [rootOf] over an account identity seed already in hand. */
    fun rootOfSeed(accountIdentitySeed: ByteArray, groupId: ByteArray): DsStatementSigning? =
        try {
            Identity.fromSeed(accountIdentitySeed).use { FfiDsStatementSigning.root(it, groupId) }
        } catch (_: Throwable) {
            null
        }

    /**
     * ADR-0057 obligation 5 — register epoch N+1's key ([after]) with the
     * account's journal [root] and return [before], the signer the commit POST
     * is signed with. Throws when the registration cannot be made or is
     * refused (no root on this device included); the caller must then publish
     * nothing.
     */
    suspend fun registerNext(
        ds: DsClient,
        groupId: ByteArray,
        before: DsJournalSigner?,
        after: DsJournalSigner?,
        root: DsStatementSigning?,
    ): DsJournalSigner =
        registerEpochBeforePublish(
            journalId = groupId,
            before = before,
            after = after,
            statements = root,
            revoke = emptyList(),
            put = { make -> ds.putStatement(make) },
        )
}
