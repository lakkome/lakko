// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import android.content.Context
import java.util.Base64
import me.lakko.app.auth.DeviceIdentityStore
import me.lakko.app.devices.sync.DeviceGroupSyncRepository
import me.lakko.app.ds.DsClient
import me.lakko.app.ds.DsCommitAnswer
import me.lakko.app.ds.DsJournalSigner
import me.lakko.app.ds.DsRegistrationUnavailable

/**
 * ADR-0081 decision 2 on Android: the ports of the DeviceGroup commit outbox
 * ([runDeviceGroupCommit]) over the snapshot store, the DS client and the
 * DeviceGroup, and the one entry point every DeviceGroup commit path goes
 * through ([commitChange]).
 *
 * Mirror of `apps/web/src/devices/device-group-commit-runtime.ts`.
 */
public object DeviceGroupCommits {

    /** What a commit path hands the outbox once it has built its commit on
     *  the throwaway handle (which is now at the commit's epoch in memory). */
    public class Built(
        public val kind: DeviceGroupCommitEntry.Kind,
        /** The journal row: the ADR-0015 envelope around the commit. */
        public val row: ByteArray,
        /** An added device's Welcome and the rendezvous it goes to. */
        public val welcome: ByteArray? = null,
        public val welcomeRendezvous: ByteArray? = null,
        /** For an add: the leaves before it (the growth re-send). */
        public val priorLeaves: List<UInt>? = null,
    )

    /** Refused: another commit took the DeviceGroup's epoch first. The person
     *  tries again (the device applies the winner's commit meanwhile). */
    public class Lost : IllegalStateException("another commit took the DeviceGroup's epoch first")

    private val b64e = Base64.getEncoder()
    private val b64d = Base64.getDecoder()

    /** The ports of the outbox for one account. */
    public fun ports(
        context: Context,
        accountIdBytes: ByteArray,
        accountIdB64: String,
        ds: DsClient,
    ): DeviceGroupCommitPorts = object : DeviceGroupCommitPorts {
        override fun saveEntry(entry: DeviceGroupCommitEntry) {
            DeviceGroupSnapshotStore.putPending(
                context, accountIdB64, DeviceGroupSnapshotStore.PendingKind.COMMIT, entry.encoded(),
            )
        }

        override fun loadEntry(): DeviceGroupCommitEntry? {
            val bytes = DeviceGroupSnapshotStore.getPending(
                context, accountIdB64, DeviceGroupSnapshotStore.PendingKind.COMMIT,
            ) ?: return null
            return DeviceGroupCommitEntry.decode(bytes) ?: run {
                // A row this build cannot read cannot be finished; dropped.
                deleteEntry()
                null
            }
        }

        override fun deleteEntry() {
            DeviceGroupSnapshotStore.deletePending(context, accountIdB64, DeviceGroupSnapshotStore.PendingKind.COMMIT)
        }

        override fun durableProof(): DsJournalSigner? {
            val ensured = try {
                loadThrowaway(context, accountIdBytes, accountIdB64)
            } catch (_: Throwable) {
                return null
            }
            return try {
                DeviceGroupJournalAccess.signerOf(ensured.group, ensured.identity)
            } finally {
                closeQuietly(ensured)
            }
        }

        override suspend fun commit(entry: DeviceGroupCommitEntry, proof: DsJournalSigner): DsCommitAnswer {
            val journalId = b64d.decode(entry.journalIdB64)
            val epoch = entry.epochValue ?: throw DsRegistrationUnavailable("a DeviceGroup commit entry does not decode")
            val root = DeviceGroupJournalAccess.rootOf(context, accountIdB64, journalId)
                ?: throw DsRegistrationUnavailable("the DeviceGroup root is not on this device")
            val epochKey = b64d.decode(entry.epochKeyB64)
            val row = b64d.decode(entry.rowB64)
            return ds.postCommit({ root.commit(journalId, epoch, epochKey, emptyList(), row) }, proof)
        }

        override fun makeCurrent(entry: DeviceGroupCommitEntry) {
            val snapshot = b64d.decode(entry.nextSnapshotB64)
            try {
                DeviceGroupSnapshotStore.putSnapshot(context, accountIdB64, snapshot)
            } finally {
                snapshot.fill(0)
            }
        }

        override fun announce(entry: DeviceGroupCommitEntry) {
            try {
                val repo = DeviceGroupSyncRepository.active ?: return
                val prior = entry.priorLeaves
                if (prior != null) {
                    repo.onLocalDeviceAdded(prior.map { it.toUInt() })
                } else {
                    repo.onLocalMembershipChanged()
                }
            } catch (cause: Throwable) {
                android.util.Log.w("DeviceGroupCommits", "DeviceGroup change announce failed (${cause.javaClass.simpleName})")
            }
        }

        override suspend fun deliverWelcome(entry: DeviceGroupCommitEntry) {
            val welcome = entry.welcomeB64?.let { b64d.decode(it) } ?: return
            val rendezvous = entry.welcomeRendezvousB64?.let { b64d.decode(it) } ?: return
            WelcomeCourier.publishWelcome(rendezvous, welcome)
        }
    }

    /** A throwaway handle over the durable DeviceGroup state (never founds). */
    private fun loadThrowaway(context: Context, accountIdBytes: ByteArray, accountIdB64: String): DeviceGroupLifecycle.Ensured {
        val seed = DeviceIdentityStore.ensureDeviceIdentitySeed(context = context, accountIdB64 = accountIdB64)
        try {
            return DeviceGroupLifecycle.ensureDeviceGroup(
                context = context,
                accountIdBytes = accountIdBytes,
                accountIdB64 = accountIdB64,
                deviceIdentitySeed = seed,
            )
        } finally {
            seed.fill(0)
        }
    }

    private fun closeQuietly(ensured: DeviceGroupLifecycle.Ensured) {
        try {
            ensured.group.close()
        } catch (_: Throwable) {
            // best-effort
        }
        try {
            ensured.identity.close()
        } catch (_: Throwable) {
            // best-effort
        }
    }

    /**
     * Step 6 for the account, under the commit lock: finish an entry a
     * previous run left. `null` when there was none. Throws when it could not
     * be finished now (the entry stays).
     */
    public suspend fun resendOwed(
        context: Context,
        accountIdBytes: ByteArray,
        accountIdB64: String,
    ): DeviceGroupCommitOutcome? =
        DeviceGroupCommitLock.withLock(accountIdB64) {
            DsClient().use { ds ->
                resendOwedDeviceGroupCommit(ports(context, accountIdBytes, accountIdB64, ds))
            }
        }

    /** What [commitChange] came to: the outcome, and [after]'s value when the
     *  commit landed. */
    public class Result<T>(public val outcome: DeviceGroupCommitOutcome, public val value: T?)

    /**
     * Every DeviceGroup commit path: under the account's commit lock, finish
     * an owed entry first (a failure there stops this commit: two commits of
     * one device never interleave), load a throwaway handle at the durable
     * epoch N (never founding), claim a founding not yet claimed, let [build]
     * create the commit on it, then run the outbox ([runDeviceGroupCommit]):
     * the entry beside the N snapshot, one `POST .../commit`, N+1 made
     * current, the Welcome, the entry deleted.
     *
     * [after] runs on the same throwaway handle (now at N+1) only when the
     * commit landed — the pairing flow seals its history, recovery and state
     * bundles there. Its failure does not undo the commit; it propagates.
     *
     * `null` when [build] decided there is nothing to commit. Throws when the
     * build refuses, when the entry cannot be written (nothing was sent), and
     * when the DS cannot be reached (the entry stays and is finished later). A
     * lost epoch is not an error: the outcome is [DeviceGroupCommitOutcome.Lost]
     * and [after] does not run.
     */
    public suspend fun <T> commitChange(
        context: Context,
        accountIdBytes: ByteArray,
        accountIdB64: String,
        build: (DeviceGroupLifecycle.Ensured) -> Built?,
        after: suspend (DeviceGroupLifecycle.Ensured) -> T,
    ): Result<T>? = DeviceGroupCommitLock.withLock(accountIdB64) {
        DsClient().use { ds ->
            val ports = ports(context, accountIdBytes, accountIdB64, ds)
            resendOwedDeviceGroupCommit(ports)
            val ensured = loadThrowaway(context, accountIdBytes, accountIdB64)
            try {
                // ADR-0057 finding 3 — a founding not yet claimed is claimed
                // first, and commits nothing until it lands.
                DeviceGroupJournalAccess.claimFoundingIfUnconfirmed(
                    context = context,
                    accountIdB64 = accountIdB64,
                    group = ensured.group,
                    identity = ensured.identity,
                    ds = ds,
                )
                val proof = DeviceGroupJournalAccess.signerOf(ensured.group, ensured.identity)
                    ?: throw DsRegistrationUnavailable("the DeviceGroup access key could not be derived")
                val built = build(ensured) ?: return@use null
                val next = DeviceGroupJournalAccess.signerOf(ensured.group, ensured.identity)
                    ?: throw DsRegistrationUnavailable("the next DeviceGroup epoch's key could not be derived")
                val snapshot = ensured.identity.snapshotStorage()
                val entry = try {
                    DeviceGroupCommitEntry(
                        kind = built.kind,
                        journalIdB64 = b64e.encodeToString(ensured.group.groupId()),
                        epoch = next.epoch.toString(),
                        epochKeyB64 = b64e.encodeToString(next.verifyingKey),
                        rowB64 = b64e.encodeToString(built.row),
                        welcomeB64 = built.welcome?.let { b64e.encodeToString(it) },
                        welcomeRendezvousB64 = built.welcomeRendezvous?.let { b64e.encodeToString(it) },
                        nextSnapshotB64 = b64e.encodeToString(snapshot),
                        priorLeaves = built.priorLeaves?.map { it.toLong() },
                        createdAtMs = System.currentTimeMillis(),
                    )
                } finally {
                    snapshot.fill(0)
                }
                val outcome = runDeviceGroupCommit(entry, proof, ports)
                if (outcome is DeviceGroupCommitOutcome.Landed) {
                    Result(outcome, after(ensured))
                } else {
                    Result(outcome, null)
                }
            } finally {
                closeQuietly(ensured)
            }
        }
    }
}
