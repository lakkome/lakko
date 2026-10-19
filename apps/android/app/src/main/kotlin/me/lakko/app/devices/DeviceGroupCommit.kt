// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import me.lakko.app.ds.DsCommitAnswer
import me.lakko.app.ds.DsCommitConflictError
import me.lakko.app.ds.DsJournalSigner

/*
 * ADR-0081 decision 2 — the DeviceGroup commit outbox.
 *
 * Mirror of `apps/web/src/devices/device-group-commit.ts` and iOS
 * `DeviceGroupCommit.swift`.
 *
 * A DeviceGroup commit (add a device by pairing or by the ADR-0049
 * confirmation, remove a device, enable attested removals, and the two commits
 * a rejoin admission makes) is built on a throwaway handle restored from the
 * durable snapshot at epoch N. Before this file the device then registered N+1
 * on the DS, persisted N+1 and published the commit, and three failures
 * stranded the account: a crash between the registration and the persist (the
 * DS held N+1, every device N, the commit nowhere), a failed publish after the
 * persist (nothing retried it), and a persist failure that was swallowed (the
 * commit published while the device stayed at N).
 *
 * The order now, with the DeviceGroup's current snapshot staying at N until
 * the DS has answered:
 *
 *   1. build the commit on a throwaway handle (the caller);
 *   2. write an entry — the journal row (the ADR-0015 envelope, so the
 *      removal attestation rides in it), the Welcome and its rendezvous, the
 *      N+1 epoch and its access key, the N+1 snapshot — BESIDE the N snapshot
 *      ([DeviceGroupSnapshotStore.PendingKind.COMMIT]). A failed write aborts:
 *      nothing has left the device;
 *   3. `POST /ds/group/<hex>/commit`: the N+1 registration (signed by the
 *      account's root) and the row in one transaction (decision 1);
 *   4. on success make the entry's N+1 snapshot current (fail-closed), tell
 *      the sync runtime its handle is stale, deliver the Welcome, delete the
 *      entry;
 *   5. on `409` delete the entry: the device stays at N and applies the
 *      winner's commit from the journal;
 *   6. on any other failure, or a restart, the entry is retried
 *      ([resendOwedDeviceGroupCommit]). The re-send is idempotent on the DS,
 *      so a crash after the DS answered completes on the retry.
 *
 * An entry is re-sent before any new DeviceGroup commit is built, so two
 * commits of one device never interleave: the callers hold
 * [DeviceGroupCommitLock] across "re-send what is owed, build, run". At most
 * one entry exists per account.
 *
 * Step 4 makes the snapshot current only while the durable state is still
 * below the entry's epoch: a retry may find N+1 already current (an earlier
 * run made it current and then failed on the Welcome), and writing it again
 * would put back an N+1 state the sync runtime has since moved on from (older
 * sender ratchet generations: a key and nonce used twice).
 *
 * Everything that decides is here; the network, the store and the MLS calls
 * are ports, so the host tests drive each failure path.
 */

/** One DeviceGroup commit in flight. Stored as JSON inside the sealed pending
 *  row (it carries MLS private state). */
@Serializable
public data class DeviceGroupCommitEntry(
    val v: Int = 1,
    val kind: Kind,
    /** The DeviceGroup journal id, standard base64. */
    val journalIdB64: String,
    /** The epoch the commit moves the DeviceGroup to (decimal `u64`). */
    val epoch: String,
    /** That epoch's access verifying key (32 bytes), standard base64. */
    val epochKeyB64: String,
    /** The journal row: the ADR-0015 envelope around the commit. */
    val rowB64: String,
    /** The Welcome for an added device, and the rendezvous it goes to. */
    val welcomeB64: String? = null,
    val welcomeRendezvousB64: String? = null,
    /** The DeviceIdentity provider snapshot at the commit's epoch. */
    val nextSnapshotB64: String,
    /** For an add: the leaves before it (the growth re-send). */
    val priorLeaves: List<Long>? = null,
    val createdAtMs: Long,
) {
    /** Which DeviceGroup change an entry carries. */
    @Serializable
    public enum class Kind {
        @SerialName("add") ADD,
        @SerialName("remove") REMOVE,
        @SerialName("policy") POLICY,
        @SerialName("rejoin-remove") REJOIN_REMOVE,
        @SerialName("rejoin-add") REJOIN_ADD,
        /** ADR-0081 amendment — the previous leaf removed and the new one
         *  added in one commit (the rejoin under the attested-removal policy). */
        @SerialName("rejoin-replace") REJOIN_REPLACE,
    }

    /** The epoch as a number, `null` when the stored text is not one. */
    val epochValue: ULong? get() = epoch.toULongOrNull()

    public fun encoded(): ByteArray = JSON.encodeToString(serializer(), this).toByteArray(Charsets.UTF_8)

    public companion object {
        private val JSON = Json { ignoreUnknownKeys = false }

        /** Decode a stored entry, or `null` when it is not one this build
         *  wrote (a row it cannot read is treated as absent and dropped by
         *  the caller). */
        public fun decode(bytes: ByteArray): DeviceGroupCommitEntry? {
            val entry = try {
                JSON.decodeFromString(serializer(), bytes.toString(Charsets.UTF_8))
            } catch (_: Throwable) {
                return null
            }
            if (entry.v != 1 || entry.epochValue == null) return null
            if ((entry.welcomeB64 == null) != (entry.welcomeRendezvousB64 == null)) return null
            val b64 = java.util.Base64.getDecoder()
            val fields = listOfNotNull(
                entry.journalIdB64, entry.epochKeyB64, entry.rowB64, entry.nextSnapshotB64,
                entry.welcomeB64, entry.welcomeRendezvousB64,
            )
            if (fields.any { runCatching { b64.decode(it) }.isFailure }) return null
            return entry
        }
    }
}

/** What one commit attempt came to. */
public sealed class DeviceGroupCommitOutcome {
    /** The commit is on the journal and its epoch is this device's. */
    public data class Landed(val messageId: Long) : DeviceGroupCommitOutcome()

    /** Another commit owns the epoch: nothing of this one is on the DS, the
     *  entry is gone, the device stays where it was. */
    public object Lost : DeviceGroupCommitOutcome()
}

/** The ports the outbox runs through. */
public interface DeviceGroupCommitPorts {
    public fun saveEntry(entry: DeviceGroupCommitEntry)

    public fun loadEntry(): DeviceGroupCommitEntry?

    public fun deleteEntry()

    /** The proof signer of the durable state's epoch, or `null` when no
     *  durable DeviceGroup state can be loaded. */
    public fun durableProof(): DsJournalSigner?

    /** `POST .../commit` with the root's statement for [entry], proven by
     *  [proof]; throws [DsCommitConflictError] on a lost epoch. */
    public suspend fun commit(entry: DeviceGroupCommitEntry, proof: DsJournalSigner): DsCommitAnswer

    /** Write the entry's snapshot as the current DeviceGroup state. Throws on
     *  a failed write. */
    public fun makeCurrent(entry: DeviceGroupCommitEntry)

    /** The DeviceGroup changed through another handle: the sync runtime
     *  reopens. Never throws. */
    public fun announce(entry: DeviceGroupCommitEntry)

    /** Deliver the entry's Welcome to its rendezvous. Throws on failure. */
    public suspend fun deliverWelcome(entry: DeviceGroupCommitEntry)
}

/** Refused: the proof is not of the epoch the commit leaves. */
public class DeviceGroupProofEpochMismatch :
    IllegalArgumentException("DeviceGroup commit: the proof is not of the epoch the commit leaves")

/**
 * Steps 2-5 for a commit just built: write the entry beside the N snapshot (a
 * failure aborts with nothing sent), then send it. [proof] is the signer of
 * epoch N, taken from the handle the commit was built on BEFORE the create
 * call. Throws on any failure other than a lost epoch; the entry then stays
 * and [resendOwedDeviceGroupCommit] finishes it.
 */
public suspend fun runDeviceGroupCommit(
    entry: DeviceGroupCommitEntry,
    proof: DsJournalSigner,
    ports: DeviceGroupCommitPorts,
): DeviceGroupCommitOutcome {
    val epoch = entry.epochValue ?: throw DeviceGroupProofEpochMismatch()
    if (proof.epoch + 1uL != epoch) throw DeviceGroupProofEpochMismatch()
    ports.saveEntry(entry)
    return finishDeviceGroupCommit(entry, proof, ports)
}

/**
 * Step 6 — finish the entry a previous run left, if any, before anything else
 * touches the DeviceGroup. `null` when there was none. Throws when the DS
 * cannot be reached or a write fails (the entry stays).
 */
public suspend fun resendOwedDeviceGroupCommit(ports: DeviceGroupCommitPorts): DeviceGroupCommitOutcome? {
    val entry = ports.loadEntry() ?: return null
    val proof = ports.durableProof()
    if (proof == null) {
        // No durable DeviceGroup to prove from: the state this entry was
        // built on is gone (deleted for a rejoin). Nothing can make it
        // current; drop it.
        ports.deleteEntry()
        return DeviceGroupCommitOutcome.Lost
    }
    return finishDeviceGroupCommit(entry, proof, ports)
}

private suspend fun finishDeviceGroupCommit(
    entry: DeviceGroupCommitEntry,
    proof: DsJournalSigner,
    ports: DeviceGroupCommitPorts,
): DeviceGroupCommitOutcome {
    val answer = try {
        ports.commit(entry, proof)
    } catch (_: DsCommitConflictError) {
        // Step 5: another commit owns the epoch. Whether or not this entry's
        // snapshot was ever made current, deleting it is right: below the
        // epoch the device applies the winner's commit; at or above it the
        // entry's work is already durable.
        ports.deleteEntry()
        return DeviceGroupCommitOutcome.Lost
    }
    // Step 4: the commit is on the journal. Make its epoch this device's,
    // unless a previous run already did (see the file doc).
    val epoch = entry.epochValue
    if (epoch != null && proof.epoch < epoch) ports.makeCurrent(entry)
    ports.announce(entry)
    if (entry.welcomeB64 != null && entry.welcomeRendezvousB64 != null) ports.deliverWelcome(entry)
    ports.deleteEntry()
    return DeviceGroupCommitOutcome.Landed(answer.id)
}

/**
 * Serialisation of the device's DeviceGroup commits: a commit path runs
 * "re-send what is owed, build, run" after every DeviceGroup commit of the
 * same account started before it (serializing, never coalescing), so a second
 * commit is never built on an epoch a first one is still taking.
 */
public object DeviceGroupCommitLock {
    private val locks = HashMap<String, Mutex>()

    private fun lockOf(accountIdB64: String): Mutex = synchronized(locks) {
        locks.getOrPut(accountIdB64) { Mutex() }
    }

    public suspend fun <T> withLock(accountIdB64: String, body: suspend () -> T): T =
        lockOf(accountIdB64).withLock { body() }
}
