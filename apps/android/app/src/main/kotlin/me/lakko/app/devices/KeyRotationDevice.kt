// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import android.content.Context
import android.util.Log
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.base64Encode
import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.chat.ChatRepository
import me.lakko.app.chat.KeyPackageRepublishOutcome
import me.lakko.app.devices.sync.DeviceGroupSyncRepository

/**
 * [KeyRotationPorts] over the device (ADR-0074 part 3, Stage A): auth,
 * discovery, the DS, the account's DeviceGroup, the chat layer and the
 * backup. The machine and every decision are in [KeyRotationRunner]; this
 * only connects them.
 *
 * No port answers "done" for work it did not do: a step that could not run
 * answers `false` or throws, and the runner keeps it owed.
 */
internal object KeyRotationDevice {
    private const val TAG = "KeyRotation"

    /**
     * The runner for the signed-in account, or `null` when no session with
     * the account's keys is live. [deviceId] is the device being removed;
     * `null` continues the owed rotation's own device. [attestation] is the
     * ADR-0015 removal attestation the passkey gesture yielded, if any.
     */
    fun runner(
        context: Context,
        authRepository: AuthRepository,
        chatRepository: ChatRepository?,
        deviceId: ByteArray? = null,
        attestation: ByteArray? = null,
        onStep: (RotationUiStep) -> Unit = {},
        /** ADR-0075 section 3: the runner of "Save a recovery phrase", a
         *  rotation that removes no device. */
        savingPhrase: Boolean = false,
    ): KeyRotationRunner? {
        val secrets = authRepository.identitySecrets.value ?: return null
        val app = context.applicationContext
        val accountIdB64 = base64Encode(secrets.accountId)
        val store = KeyRotationOwedStore.onDevice(app)
        val owedNow = store.load(accountIdB64)
        val noDevice = savingPhrase || (deviceId == null && owedNow != null && !owedNow.removesDevice)
        val named: ByteArray? = deviceId ?: owedNow?.takeIf { it.removesDevice }?.deviceIdB64Url?.let {
            try {
                base64UrlDecodeNoPad(it)
            } catch (_: IllegalArgumentException) {
                null
            }
        }
        if (named == null && !noDevice) return null
        // A rotation that removes no device never asks the removal ports.
        val target: ByteArray by lazy {
            named ?: throw IllegalStateException("this rotation removes no device")
        }

        fun sync(): DeviceGroupSyncRepository =
            DeviceGroupSyncRepository.active ?: throw IllegalStateException("the device list is not open")

        val ports = object : KeyRotationPorts {
            override fun freshGrant(): String? = authRepository.freshGrant()

            override suspend fun epochIfInRoster(): ULong? = sync().epochIfInRoster(target)

            override suspend fun removeDevice(): RemovalResult {
                val removal = DeviceRemovalRunner.onDevice(app, authRepository, chatRepository)
                    ?: throw IllegalStateException("no session")
                return removal.remove(target, attestation)
            }

            override suspend fun standing(): RemovalStanding = sync().removalStanding(target)

            override suspend fun seal(envelope: ByteArray): Pair<ByteArray, ULong> = sync().sealKeyRotation(envelope)

            override suspend fun publish(wire: ByteArray) = sync().publishKeyRotation(wire)

            override fun prepare(nowMs: Long) = authRepository.rotationPrepare(nowMs)

            override fun envelope(): ByteArray = authRepository.rotationEnvelope()

            override suspend fun commit(
                freshGrant: String,
                revokeDeviceId: ByteArray?,
                phraseSaved: Boolean,
            ): CommitAnswer.Committed = authRepository.rotationCommit(freshGrant, revokeDeviceId, phraseSaved)

            override suspend fun resolveLostCommit(): LostCommit = authRepository.rotationResolveLostCommit()

            override suspend fun adoptLocally(attestationB64: String?, keyGeneration: Long?) =
                authRepository.rotationAdopt()

            override suspend fun publishIdentity(attestationB64: String?, withProof: Boolean): IdentityPublish =
                authRepository.rotationPublishIdentity(attestationB64, withProof)

            override suspend fun publishKeyPackage(): Boolean {
                val chats = chatRepository ?: return false
                val outcome = chats.republishKeyPackage()
                return outcome == KeyPackageRepublishOutcome.UPLOADED || outcome == KeyPackageRepublishOutcome.COVERED
            }

            override suspend fun moveDeviceGroupRoot(): Boolean =
                authRepository.rotationMoveDeviceGroupRoot()

            override suspend fun moveOwnedJournals(): Boolean {
                // ADR section 3.5, "DS": every journal this account roots
                // goes from the old seed's root to the new seed's through
                // the ADR-0057 takeover (root transfer by the old root, the
                // registrar set replaced, the registrar seeds re-sent). The
                // step is done once each old root is KEPT on this device and
                // its takeover scheduled: from there the chat layer retries
                // it durably by itself. No chat layer, or a root that could
                // not be kept, leaves the step owed.
                val chats = chatRepository ?: return false
                val (oldSeed, newSeed) = authRepository.rotationSeedPair() ?: return false
                try {
                    chats.transferOwnedJournalRoots(oldSeed, newSeed)
                } finally {
                    oldSeed.fill(0)
                    newSeed.fill(0)
                }
                return true
            }

            override suspend fun moveOwnedChannels(): Boolean =
                // ADR section 3.5, "Channels": `PUT /v1/channels/{id}/owner`
                // for every channel this device publishes to. `false` (owed)
                // while any channel has no verdict.
                authRepository.rotationMoveOwnedChannels()

            override suspend fun rekeyReach(): Boolean {
                // ADR section 3.5, "Reach": the reach keys and the mailbox
                // they are registered on derive from the account seed, so
                // both move to the new one; done only once the new mailbox
                // holds the key set. The predecessor's mailbox keeps being
                // read for the Welcomes of the bindings that exist.
                val current = authRepository.identitySecrets.value ?: return false
                val reach = me.lakko.app.reach.ReachManager.get(app)
                uniffi.lakko_ffi_uniffi.Identity.fromSeed(current.accountIdentitySeed).use { identity ->
                    // The new mailbox is claimed before anything is
                    // registered on it (idempotent; a refusal throws and
                    // the step stays owed).
                    val ffi = uniffi.lakko_ffi_uniffi.dsMailboxSigner(identity)
                    me.lakko.app.ds.DsClient().use { ds ->
                        ds.claimMailbox(ffi.mailboxAddress(), me.lakko.app.ds.FfiDsJournalSigner(ffi))
                    }
                    reach.bind(accountIdB64, identity)
                }
                return reach.sync()
            }

            override suspend fun resendProfile(): Boolean {
                // ADR section 3.5a rule 3: every one-to-one and group holds
                // grants for the OLD mailbox; the profile carrying the ones
                // for the new mailbox goes out through the chat layer's
                // profile publisher, awaited. No chat layer, or any
                // conversation that did not take it, leaves the step owed.
                val chats = chatRepository ?: return false
                return chats.resendProfileAfterKeyChange()
            }

            override suspend fun spendOldGrants(): Boolean {
                // ADR section 3.5a rule 5: the links the old key issued are
                // revoked at the ledger and the unanswered contact requests
                // re-sent from the new key, through the contacts layer. No
                // contacts layer leaves the step owed.
                val chats = chatRepository ?: return false
                val spend = chats.spendPreRotationGrants ?: return false
                return spend()
            }

            override suspend fun sealBackup(): Boolean {
                // ADR section 3.5, "Backup": re-sealed under the new phrase
                // and uploaded, awaited. The backup carries no MLS state:
                // contacts, the synced state and the conversations' ids. A
                // chat layer that is not ready leaves the step owed: an
                // envelope listing no conversations would overwrite the one
                // the server holds.
                val chats = chatRepository ?: return false
                val state = chats.state.value
                if (!state.bootstrapped) return false
                authRepository.rotationSealBackup(
                    knownGroupIdsHex = state.threads.map { it.groupIdHex },
                    syncedState = DeviceGroupSyncRepository.active?.currentSyncedStateBytes(),
                )
                return true
            }

            override fun onPhraseReleased() = authRepository.rotationPhraseReleased()

            override fun discardPrepared() = authRepository.rotationDiscardPrepared()

            override suspend fun pause(ms: Long) = kotlinx.coroutines.delay(ms)
        }
        return KeyRotationRunner(
            accountIdB64 = accountIdB64,
            store = store,
            ports = ports,
            log = { message -> Log.w(TAG, message) },
            onStep = onStep,
        )
    }
}
