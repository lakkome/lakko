// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import android.content.Context
import kotlinx.coroutines.CancellationException
import me.lakko.app.auth.DeviceIdentityStore
import me.lakko.app.auth.KeyRotationFiles
import me.lakko.app.auth.SealedBlobStorage
import uniffi.lakko_ffi_uniffi.AddMember
import uniffi.lakko_ffi_uniffi.DecodedPairingQr
import uniffi.lakko_ffi_uniffi.DeviceLeaf
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.loadDeviceGroupById

/*
 * G-56 (ADR-0074, "Joining after a rotation"): a device that joined the
 * account's DeviceGroup by pairing can still park on the earlier-keys screen
 * (the recovery row lacked the earlier key). Pairing again from there must not
 * add a second leaf under the same device key (the core refuses:
 * `DuplicateSignatureKey`). The device keeps its device identity and signs,
 * with that key, that its leaf may be replaced by the new KeyPackage in the
 * commit leaving the DeviceGroup's latest epoch (the ADR-0081 self-removal
 * proof); the confirming device replaces the old leaf in one commit.
 *
 * Mirror of `apps/ios/Lakko/Devices/PairingOrchestrator.swift`
 * (`prepareRejoinPairingFlow`, `replacementCommit`) and
 * `apps/ios/Lakko/Auth/OwnConversationKeyStore.swift`
 * (`JoinedDeviceGroupIdStore`). Every native call is a port, so the host JVM
 * tests drive the decisions with fakes.
 */

// ---------------------------------------------------------------------------
// The joined DeviceGroup id
// ---------------------------------------------------------------------------

/**
 * The id of the DeviceGroup this device joined by pairing, per account. A
 * device parked for the account's earlier keys cannot derive the id (it is a
 * function of the earlier key: [DeviceGroupLifecycle.lineageSeed] names none),
 * and a re-pairing from the parked screen names it in its self-removal proof
 * and its epoch probe. Sealed under the device key beside the rotation files;
 * deleted by Forget device ([KeyRotationFiles.deleteAll]) and by a key reset
 * or recall on this device.
 */
public class JoinedDeviceGroupIdStore(private val storage: SealedBlobStorage) {
    public fun load(accountIdB64: String): ByteArray? {
        val name = nameOf(accountIdB64) ?: return null
        val plain = storage.read(name) ?: return null
        return plain.takeIf { it.size == GROUP_ID_LEN }
    }

    /** Record [groupId]; `false` when it is not stored (a wrong-length id, or
     *  a signed-out or wiped account refuses the write). */
    public fun save(accountIdB64: String, groupId: ByteArray): Boolean {
        if (groupId.size != GROUP_ID_LEN) return false
        val name = nameOf(accountIdB64) ?: return false
        return try {
            me.lakko.app.account.AccountStoreGate.write(accountIdB64) { storage.write(name, groupId.copyOf()) } != null
        } catch (_: Throwable) {
            false
        }
    }

    public fun clear(accountIdB64: String) {
        nameOf(accountIdB64)?.let(storage::delete)
    }

    private fun nameOf(accountIdB64: String): String? =
        KeyRotationFiles.hexOf(accountIdB64)?.let { it + KeyRotationFiles.JOINED_GROUP_SUFFIX }

    public companion object {
        /** A DeviceGroup id is 32 bytes. */
        public const val GROUP_ID_LEN: Int = 32

        public fun onDevice(context: Context): JoinedDeviceGroupIdStore =
            JoinedDeviceGroupIdStore(KeyRotationFiles.onDevice(context))
    }
}

/**
 * Record the DeviceGroup a join just persisted. Called at both joins (the
 * pairing join for a known account and the join by name) right after the
 * snapshot write; best effort: `false` (the caller logs) when it did not land.
 */
public fun recordJoinedDeviceGroup(store: JoinedDeviceGroupIdStore, accountIdB64: String, groupId: () -> ByteArray): Boolean =
    try {
        store.save(accountIdB64, groupId())
    } catch (e: CancellationException) {
        throw e
    } catch (_: Throwable) {
        false
    }

// ---------------------------------------------------------------------------
// The joining side
// ---------------------------------------------------------------------------

/** What the joining side reads. Production: the stores, the DS and the core. */
public interface RejoinPairingPorts {
    /** The DeviceGroup id recorded at this device's join, `null` with none. */
    public fun joinedGroupId(): ByteArray?

    /** This device's epoch in [groupId] when it holds its device identity seed
     *  and a DeviceGroup snapshot that opens under it, and its own leaf is in
     *  that group's roster; `null` otherwise. */
    public fun heldEpochIfMember(groupId: ByteArray): ULong?

    /** The DeviceGroup's latest epoch at the DS (an empty knock, the ADR-0081
     *  rejoin probe); `null` when the DS names none. */
    public suspend fun latestEpoch(groupId: ByteArray): ULong?
}

/** The joining side's answer. */
public sealed interface RejoinPairingPlan {
    /** The ordinary join: a new leaf is added. */
    public data object Plain : RejoinPairingPlan

    /** This device is in the DeviceGroup already: it signs a self-removal
     *  proof for the commit leaving [epoch] of [groupId]. */
    public class Rejoin(public val groupId: ByteArray, public val epoch: ULong) : RejoinPairingPlan
}

/**
 * Whether a pairing from the parked screen replaces this device's own leaf.
 * [RejoinPairingPlan.Plain] when no id was recorded, or the device holds no
 * DeviceGroup state it can read, or its leaf is no longer in it. The epoch is
 * the DS's latest (an empty knock); this device's own epoch when the probe
 * fails or names none.
 */
public suspend fun planRejoinPairing(ports: RejoinPairingPorts): RejoinPairingPlan {
    val groupId = ports.joinedGroupId() ?: return RejoinPairingPlan.Plain
    val held = try {
        ports.heldEpochIfMember(groupId)
    } catch (e: CancellationException) {
        throw e
    } catch (_: Throwable) {
        null
    } ?: return RejoinPairingPlan.Plain
    val probed = try {
        ports.latestEpoch(groupId)
    } catch (e: CancellationException) {
        throw e
    } catch (_: Throwable) {
        // The probe failed: this device's own epoch is used.
        null
    }
    return RejoinPairingPlan.Rejoin(groupId, probed ?: held)
}

/** The ports of [planRejoinPairing] on the device. */
public fun rejoinPairingPorts(
    context: Context,
    accountIdBytes: ByteArray,
    accountIdB64: String,
): RejoinPairingPorts = object : RejoinPairingPorts {
    private val appContext = context.applicationContext

    override fun joinedGroupId(): ByteArray? = JoinedDeviceGroupIdStore.onDevice(appContext).load(accountIdB64)

    override fun heldEpochIfMember(groupId: ByteArray): ULong? {
        val seed = DeviceIdentityStore.getDeviceIdentitySeed(appContext, accountIdB64) ?: return null
        val snapshot = DeviceGroupSnapshotStore.getSnapshot(appContext, accountIdB64)
        if (snapshot == null) {
            seed.fill(0)
            return null
        }
        try {
            Identity.fromSeed(seed).use { held ->
                held.restoreStorage(snapshot)
                val group = loadDeviceGroupById(held, accountIdBytes, groupId) ?: return null
                return group.use { g ->
                    val own = held.signaturePublicKey()
                    if (g.devices().none { it.signaturePublicKey.contentEquals(own) }) null else g.epoch()
                }
            }
        } finally {
            seed.fill(0)
            snapshot.fill(0)
        }
    }

    override suspend fun latestEpoch(groupId: ByteArray): ULong? =
        me.lakko.app.ds.DsClient().use { ds -> ds.knock(groupId, emptyList()) }.epoch
}

// ---------------------------------------------------------------------------
// The confirming side
// ---------------------------------------------------------------------------

/**
 * Refused: the pairing payload carries a self-removal proof that does not
 * cover replacing the device's leaf at the DeviceGroup's current epoch (a
 * stale epoch, another group, another KeyPackage). Nothing is committed; the
 * person tries again and the joining device signs at the new epoch.
 */
public class SelfRemovalProofNotCovering :
    IllegalStateException("the pairing payload's self-removal proof does not cover this epoch")

/** What the confirming side's commit build reads and calls, on the throwaway
 *  handle the DeviceGroup commit outbox hands it. */
public interface PairingCommitPorts {
    public fun keyPackageKey(keyPackage: ByteArray): ByteArray
    public fun devices(): List<DeviceLeaf>
    public fun groupId(): ByteArray
    public fun epoch(): ULong

    /** The core's `deviceGroupSelfRemovalCovers`. */
    public fun covers(proof: ByteArray, groupId: ByteArray, epoch: ULong, removedKey: ByteArray, keyPackage: ByteArray): Boolean

    public fun replaceDeviceAt(leafIndex: UInt, keyPackage: ByteArray): AddMember
    public fun addDeviceFromWire(keyPackage: ByteArray): AddMember
}

/**
 * The commit a confirmed pairing makes, built at the epoch of the handle the
 * outbox calls it on (the epoch the commit leaves):
 *
 *  - a payload with a self-removal proof whose KeyPackage key has a leaf in
 *    the DeviceGroup: that leaf out and the new one in, in ONE commit (the
 *    ADR-0081 rejoin replacement, kind `rejoin-replace`), the proof in the
 *    journal envelope's removal-authorisation slot. Throws
 *    [SelfRemovalProofNotCovering] when the proof does not cover it there: a
 *    removal peers would refuse under the policy is never committed, and
 *    without the policy an uncovered proof is still the joining device's word
 *    only.
 *  - otherwise the ordinary add (no attestation).
 */
public fun buildPairingCommit(decoded: DecodedPairingQr, ports: PairingCommitPorts): DeviceGroupCommits.Built {
    // ADR-0058 section 5.1 — the members before the change, so the sync
    // runtime can decide whether this device re-sends.
    val before = runCatching { ports.devices() }.getOrNull()
    val proof = decoded.selfRemovalProof?.takeIf { it.isNotEmpty() }
    if (proof != null) {
        val key = ports.keyPackageKey(decoded.keyPackage)
        val previous = (before ?: ports.devices()).firstOrNull { it.signaturePublicKey.contentEquals(key) }
        if (previous != null) {
            if (!ports.covers(proof, ports.groupId(), ports.epoch(), key, decoded.keyPackage)) {
                throw SelfRemovalProofNotCovering()
            }
            val replaced = ports.replaceDeviceAt(previous.leafIndex, decoded.keyPackage)
            return DeviceGroupCommits.Built(
                kind = DeviceGroupCommitEntry.Kind.REJOIN_REPLACE,
                row = DeviceGroupJournalEnvelope.encode(replaced.commit, proof),
                welcome = replaced.welcome,
                welcomeRendezvous = decoded.rendezvousPseudonym,
                priorLeaves = before?.map { it.leafIndex },
            )
        }
    }
    val addition = ports.addDeviceFromWire(decoded.keyPackage)
    // G-13 (ADR-0015): every DeviceGroup journal entry is wrapped; an
    // add-device commit carries no attestation.
    return DeviceGroupCommits.Built(
        kind = DeviceGroupCommitEntry.Kind.ADD,
        row = DeviceGroupJournalEnvelope.encode(addition.commit, null),
        welcome = addition.welcome,
        welcomeRendezvous = decoded.rendezvousPseudonym,
        priorLeaves = before?.map { it.leafIndex },
    )
}

/** The ports of [buildPairingCommit] over the outbox's throwaway handle. */
public fun pairingCommitPorts(ensured: DeviceGroupLifecycle.Ensured): PairingCommitPorts = object : PairingCommitPorts {
    override fun keyPackageKey(keyPackage: ByteArray): ByteArray =
        uniffi.lakko_ffi_uniffi.keyPackageSignatureKey(keyPackage)

    override fun devices(): List<DeviceLeaf> = ensured.group.devices()

    override fun groupId(): ByteArray = ensured.group.groupId()

    override fun epoch(): ULong = ensured.group.epoch()

    override fun covers(
        proof: ByteArray,
        groupId: ByteArray,
        epoch: ULong,
        removedKey: ByteArray,
        keyPackage: ByteArray,
    ): Boolean = uniffi.lakko_ffi_uniffi.deviceGroupSelfRemovalCovers(
        ensured.identity, proof, groupId, epoch, removedKey, keyPackage,
    )

    override fun replaceDeviceAt(leafIndex: UInt, keyPackage: ByteArray): AddMember =
        ensured.group.replaceDeviceAt(ensured.identity, leafIndex, keyPackage)

    override fun addDeviceFromWire(keyPackage: ByteArray): AddMember =
        ensured.group.addDeviceFromWire(identity = ensured.identity, keyPackage = keyPackage)
}
