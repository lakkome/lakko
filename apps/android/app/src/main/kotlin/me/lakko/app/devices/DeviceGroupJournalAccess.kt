// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import me.lakko.app.ds.DsClient
import me.lakko.app.ds.DsJournalSigner
import me.lakko.app.ds.FfiDsJournalSigner
import me.lakko.app.ds.DsStatementSigning
import me.lakko.app.ds.FfiDsStatementSigning
import me.lakko.app.ds.registerEpochBeforePublish
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
                me.lakko.app.ds.claimOrReset(
                    journalId = groupId,
                    epoch = signer.epoch,
                    epochKey = signer.verifyingKey,
                    statements = root,
                    refounded = true,
                    put = { make -> ds.putStatement(make) },
                )
            },
            clear = { clearFoundingUnconfirmed(context, accountIdB64) },
        )
    }

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
