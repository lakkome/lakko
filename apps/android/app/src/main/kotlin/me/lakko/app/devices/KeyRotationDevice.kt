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
 * [KeyRotationPorts] over the device (ADR-0066 part 3, Stage A): auth,
 * discovery, the DS, the account's DeviceGroup, the chat layer and the
 * backup. The machine and every decision are in [KeyRotationRunner]; this
 * only connects them.
 *
 * What Stage A does NOT move on Android (stated, not hidden): the journals
 * of the groups this account owns and the owner keys of its channels stay
 * under the PREDECESSOR key, and the reach keys stay bound to the
 * predecessor's mailbox. Those two ports answer "done" so the rotation
 * completes; see [moveOwnedJournals] and [moveOwnedChannels].
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
    ): KeyRotationRunner? {
        val secrets = authRepository.identitySecrets.value ?: return null
        val app = context.applicationContext
        val accountIdB64 = base64Encode(secrets.accountId)
        val store = KeyRotationOwedStore.onDevice(app)
        val target = deviceId ?: store.load(accountIdB64)?.deviceIdB64Url?.let {
            try {
                base64UrlDecodeNoPad(it)
            } catch (_: IllegalArgumentException) {
                null
            }
        } ?: return null

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

            override suspend fun commit(freshGrant: String, revokeDeviceId: ByteArray): CommitAnswer.Committed =
                authRepository.rotationCommit(freshGrant, revokeDeviceId)

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

            override suspend fun moveDeviceGroupRoot(attestationB64: String?, keyGeneration: Long?): Boolean =
                authRepository.rotationMoveDeviceGroupRoot(attestationB64, keyGeneration)

            override suspend fun moveOwnedJournals(): Boolean {
                // NOT BUILT on Android: the root of an owned group's journal
                // and its registrar seeds derive from the account seed the
                // chat layer acts under, which Stage A keeps at the
                // predecessor. They stay rooted under the predecessor key.
                Log.w(TAG, "owned group journals stay rooted under the previous account key (not moved)")
                return true
            }

            override suspend fun moveOwnedChannels(): Boolean {
                // NOT BUILT on Android: a channel's owner proofs are signed
                // by the identity its PublisherGroup runs on, which stays the
                // predecessor (no leaf is moved in place). The owner key is
                // not handed over.
                Log.w(TAG, "owned channels keep their owner key under the previous account key (not handed over)")
                return true
            }

            override suspend fun rekeyReach(): Boolean {
                // The mailbox of the published key is claimed by the chat
                // layer as soon as the session carries the new seed
                // (`ChatRepository.bootstrap` on the same account). The reach
                // keys stay bound to the predecessor's mailbox, where the
                // Welcomes for the bindings that exist still arrive.
                return true
            }

            override suspend fun sealBackup(): Boolean {
                // The phrase on this device is the new one: the next backup
                // run seals under it and mints a new head.
                me.lakko.app.backup.AutoBackupStore.markDirty(app, accountIdB64, System.currentTimeMillis())
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
