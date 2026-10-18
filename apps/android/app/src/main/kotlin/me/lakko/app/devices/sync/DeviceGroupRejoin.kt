// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/*
 * ADR-0081 decision 4 — a device that lost or fell behind its DeviceGroup
 * rejoins through a sibling instead of re-founding.
 *
 * Mirror of `apps/web/src/devices/device-group-rejoin.ts` and iOS
 * `DeviceGroupRejoin.swift`.
 *
 * When a device rejoins:
 *
 *  - LOST: it has no usable DeviceGroup state (no snapshot, or one that does
 *    not decrypt under its wrap key or does not restore) while the account's
 *    directory lists another own device. With no other device listed it
 *    founds instead: nobody can be cut off.
 *  - BEHIND: a signed read of its DeviceGroup journal after its cursor
 *    returned no rows and nothing more, the DS names a newer epoch than its
 *    handle's, and it has no commit of its own in flight ([isBehind]). Since
 *    ADR-0081 decision 1 an epoch is registered only together with the
 *    commit row that reaches it, so a drained journal that is still ahead
 *    means the device cannot apply a row it has already passed: nothing it
 *    will ever read moves it. No grace period is needed any more; the one
 *    exception is this device's own outbox entry, which is finished first.
 *
 * The device's side: delete the local DeviceGroup state and make sure its
 * directory KeyPackage is published; build a DeviceGroup KeyPackage from the
 * same device identity (same leaf key) and post it to the DeviceGroup's knock
 * as the ADR-0056 pairing payload (login handle, KeyPackage, a fresh
 * rendezvous id), keeping the joiner's provider and the rendezvous; poll the
 * rendezvous for the Welcome and join; knock again after [REKNOCK_AFTER_MS].
 *
 * A sibling's side: once its journal is read to the head it reads the knocks
 * and admits one only when [admissionVerdict] says so (the payload names this
 * account's login handle; the key is listed in the account's directory with a
 * binding under the account key — a deleted device is tombstoned there and
 * not listed; the knock is newer than every addition of that key this device
 * saw and than when it started watching; this device is the designated
 * admitter — the lowest leaf other than the knocking key — or the knock has
 * waited [FALLBACK_ADMIT_AFTER_MS]). It removes the key's previous leaf if
 * still present, then adds the KeyPackage: two ordinary DeviceGroup commits
 * through the outbox, the Welcome to the knock's rendezvous. Under the
 * ADR-0015 policy a present previous leaf waits for the person
 * ([AdmissionVerdict.NEEDS_ATTESTATION]).
 */

/** How long a rejoining device waits for a Welcome before knocking again.
 *  Chosen, not measured. */
public const val REKNOCK_AFTER_MS: Long = 30L * 60_000L

/** How long a knock waits for the designated admitter before any sibling
 *  admits it. Chosen, not measured. */
public const val FALLBACK_ADMIT_AFTER_MS: Long = 10L * 60_000L

/** How often a sibling reads the knocks while its journal is at the head.
 *  Chosen, not measured. */
public const val KNOCK_CHECK_INTERVAL_MS: Long = 30_000L

/** At most this many handled knock ids are kept (knocks live seven days). */
public const val MAX_HANDLED_KNOCKS: Int = 256

/** Is this device behind its DeviceGroup for good? */
public fun isBehind(localEpoch: ULong, drained: Boolean, latestEpoch: ULong?, commitPending: Boolean): Boolean {
    if (commitPending || !drained || latestEpoch == null) return false
    return latestEpoch > localEpoch
}

/** What a sibling decides about one knock. */
public enum class AdmissionVerdict {
    ADMIT,
    /** Already handled by this device. */
    HANDLED,
    /** From before this device started watching the journal. */
    BEFORE_WATCHING,
    /** The key was added after the knock: someone already admitted it. */
    ALREADY_ADMITTED,
    /** The knocking key is this device's own. */
    OWN_KEY,
    /** Not listed in the directory with a verified binding. */
    NOT_LISTED,
    /** Waiting for the designated admitter. */
    NOT_MINE_YET,
    /** The previous leaf is present and the policy wants a passkey
     *  attestation for its removal. */
    NEEDS_ATTESTATION,
}

/** One device of the roster. Keys are lowercase hex. */
public data class RosterMember(val leaf: UInt, val keyHex: String)

/** Everything [admissionVerdict] reads. */
public data class AdmissionInput(
    val knockId: Long,
    val knockAtMs: Long,
    val keyHex: String,
    val ownKeyHex: String,
    val roster: List<RosterMember>,
    val handled: Set<Long>,
    val watchingSinceMs: Long,
    val lastAddedAtMs: Long?,
    val listed: Boolean,
    val policyRequiresAttestation: Boolean,
    val nowMs: Long,
)

/** The pure admission rule (see the file doc). */
public fun admissionVerdict(input: AdmissionInput): AdmissionVerdict {
    if (input.knockId in input.handled) return AdmissionVerdict.HANDLED
    if (input.keyHex == input.ownKeyHex) return AdmissionVerdict.OWN_KEY
    if (input.knockAtMs < input.watchingSinceMs) return AdmissionVerdict.BEFORE_WATCHING
    val added = input.lastAddedAtMs
    if (added != null && added >= input.knockAtMs) return AdmissionVerdict.ALREADY_ADMITTED
    if (!input.listed) return AdmissionVerdict.NOT_LISTED
    val lowest = input.roster.filter { it.keyHex != input.keyHex }.minByOrNull { it.leaf }
    val designated = lowest?.keyHex == input.ownKeyHex
    if (!designated && input.nowMs - input.knockAtMs < FALLBACK_ADMIT_AFTER_MS) return AdmissionVerdict.NOT_MINE_YET
    val present = input.roster.any { it.keyHex == input.keyHex }
    if (present && input.policyRequiresAttestation) return AdmissionVerdict.NEEDS_ATTESTATION
    return AdmissionVerdict.ADMIT
}

private val REJOIN_JSON = Json { ignoreUnknownKeys = false }

/** What a sibling keeps across sessions (the `admissions` pending row): when
 *  it started watching, when it last saw each key added, the knocks it
 *  handled. Cleared with the DeviceGroup state. */
@Serializable
public data class AdmissionRecord(
    val v: Int = 1,
    val watchingSinceMs: Long,
    val lastAddedAtMs: Map<String, Long> = emptyMap(),
    val handled: List<Long> = emptyList(),
) {
    /** The record with [keyHex] seen added at [atMs] (the later time wins). */
    public fun noteAdded(keyHex: String, atMs: Long): AdmissionRecord {
        val prior = lastAddedAtMs[keyHex]
        if (prior != null && prior >= atMs) return this
        return copy(lastAddedAtMs = lastAddedAtMs + (keyHex to atMs))
    }

    /** The record with [knockId] handled (bounded, newest kept). */
    public fun noteHandled(knockId: Long): AdmissionRecord {
        if (knockId in handled) return this
        return copy(handled = (handled + knockId).sorted().takeLast(MAX_HANDLED_KNOCKS))
    }

    public fun encoded(): ByteArray = REJOIN_JSON.encodeToString(serializer(), this).toByteArray(Charsets.UTF_8)

    public companion object {
        /** `null` for bytes this build did not write. */
        public fun decode(bytes: ByteArray): AdmissionRecord? =
            try {
                REJOIN_JSON.decodeFromString(serializer(), bytes.toString(Charsets.UTF_8)).takeIf { it.v == 1 }
            } catch (_: Throwable) {
                null
            }
    }
}

/** A rejoin in progress (the `rejoin` pending row). */
@Serializable
public data class RejoinState(
    val v: Int = 1,
    /** The rendezvous the Welcome will arrive at, standard base64. */
    val rendezvousB64: String,
    /** The joiner's DeviceIdentity provider snapshot (it holds the
     *  KeyPackage's private keys), standard base64. */
    val joinerSnapshotB64: String,
    val knockedAtMs: Long,
) {
    public fun encoded(): ByteArray = REJOIN_JSON.encodeToString(serializer(), this).toByteArray(Charsets.UTF_8)

    public companion object {
        /** `null` for bytes this build did not write. */
        public fun decode(bytes: ByteArray): RejoinState? =
            try {
                REJOIN_JSON.decodeFromString(serializer(), bytes.toString(Charsets.UTF_8)).takeIf { it.v == 1 }
            } catch (_: Throwable) {
                null
            }

        /** Whether a rejoin knocked long enough ago to knock again (or the
         *  clock went back). */
        public fun shouldKnockAgain(state: RejoinState?, nowMs: Long): Boolean =
            state == null || nowMs - state.knockedAtMs >= REKNOCK_AFTER_MS || nowMs < state.knockedAtMs
    }
}

/** What one rejoin attempt needs. */
public interface RejoinPorts {
    /** Delete the local DeviceGroup state (snapshot and pending commit). */
    public fun dropLocalState()

    /** Make sure this device's directory KeyPackage is published. */
    public suspend fun publishDirectoryKeyPackage()

    public fun loadState(): RejoinState?

    public fun saveState(state: RejoinState)

    /** Build a fresh joiner and knock with it; returns the state to keep
     *  (already saved before the knock went out). */
    public suspend fun knock(nowMs: Long): RejoinState

    /** Wait for the Welcome at [state]'s rendezvous until [deadlineMs];
     *  `null` on a timeout. */
    public suspend fun awaitWelcome(state: RejoinState, deadlineMs: Long): ByteArray?

    /** Join from the Welcome with the joiner kept in [state] and persist the
     *  joined DeviceGroup (fail-closed). */
    public suspend fun join(state: RejoinState, welcome: ByteArray)

    public fun clearState()

    public fun now(): Long
}

/** What one pass of the device's side came to. */
public enum class RejoinPass { JOINED, WAITING }

/**
 * One pass of the device side: drop the state (first pass only), make sure a
 * knock is out, wait for its Welcome until the re-knock time.
 */
public suspend fun rejoinOnce(ports: RejoinPorts, firstPass: Boolean): RejoinPass {
    if (firstPass) {
        ports.dropLocalState()
        ports.publishDirectoryKeyPackage()
    }
    var state = ports.loadState()
    if (RejoinState.shouldKnockAgain(state, ports.now())) {
        val knocked = ports.knock(ports.now())
        ports.saveState(knocked)
        state = knocked
    }
    val current = state ?: return RejoinPass.WAITING
    val welcome = ports.awaitWelcome(current, current.knockedAtMs + REKNOCK_AFTER_MS) ?: return RejoinPass.WAITING
    ports.join(current, welcome)
    ports.clearState()
    return RejoinPass.JOINED
}
