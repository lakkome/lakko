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
 *  - RESET UNDER IT (ADR-0081 third amendment, point 5): another device
 *    founded the DeviceGroup again while this one was not running, so the
 *    journal's chain is not the one this device holds: its signed reads are
 *    refused [REFUSALS_BEFORE_LOST] times in a row, or a drained read names an
 *    epoch below its own ([isJournalReset]). It is lost, and rejoins.
 *
 * The device's side: delete the local DeviceGroup state and make sure its
 * directory KeyPackage is published; build a DeviceGroup KeyPackage from the
 * same device identity (same leaf key) and post it to the DeviceGroup's knock
 * as the ADR-0056 pairing payload (login handle, KeyPackage, a fresh
 * rendezvous id), keeping the joiner's provider and the rendezvous.
 * ADR-0081 third amendment (2026-11-11): the rendezvous is read before the
 * knock goes out, the knock's own round trip `rtt` is measured, and the
 * device waits [rejoinAnswerWindowMs] for an answer. A presence note
 * (`composeRejoinPresent`) means a device is running: the device then waits
 * for the Welcome with no deadline, knocking again after [REKNOCK_AFTER_MS].
 * No presence note and no Welcome in the window means every listed device is
 * offline: the device founds the DeviceGroup itself ([RejoinPass.FOUND]).
 * A device that knows no username for its account at all founds at once:
 * no running device could match its knock.
 *
 * A sibling's side: while in the foreground it keeps a long poll on the
 * DeviceGroup's knocks; on a knock naming this account's login handle it
 * posts the presence note to the knock's rendezvous at once, before any
 * directory lookup, then admits it when [admissionVerdict] says so (the
 * payload names this account's login handle; the key is listed in the
 * account's directory with a binding under the account key — a deleted device
 * is tombstoned there and not listed; the knock is newer than every addition
 * of that key this device saw and than when it started watching). Every
 * running sibling that passes the checks admits at once: the DS commit route
 * lets one commit take the epoch, the others get `409` and drop theirs. It
 * replaces the key's previous leaf, if still present, by the KeyPackage in ONE
 * commit (`replaceDeviceAt`), or adds the KeyPackage when no previous leaf is
 * there: one ordinary DeviceGroup commit through the outbox, the Welcome to
 * the knock's rendezvous.
 *
 * Under the ADR-0015 policy a removal needs a passkey attestation OR
 * (ADR-0081 amendment) the device's own self-removal proof: the rejoining
 * device signs, with its old leaf key, that its leaf may be replaced by the
 * KeyPackage it knocks with, and the proof rides in the commit's
 * removal-authorisation slot. A knock without a proof the sibling verified,
 * whose previous leaf is present, waits for the person under the policy
 * ([AdmissionVerdict.NEEDS_ATTESTATION]): a device that no longer holds its
 * old key is a new device, its KeyPackage key matches no leaf, and it is
 * simply added.
 */

/** How long a rejoining device waits for a Welcome before knocking again.
 *  Chosen, not measured. */
public const val REKNOCK_AFTER_MS: Long = 30L * 60_000L

/** The shortest answer window, whatever the knock's round trip. Chosen, not
 *  measured (ADR-0081 third amendment). */
public const val REJOIN_ANSWER_MIN_WINDOW_MS: Long = 1_000L

/** The answer window is this many knock round trips (a DS wake, one read and
 *  one write on the sibling, and the new device's read). Chosen, not
 *  measured. */
public const val REJOIN_ANSWER_RTT_FACTOR: Long = 4L

/** How long a sibling's long poll on the knocks waits at the DS. */
public const val KNOCK_LISTEN_WAIT_SECS: Int = 25

/** A knock already on the DS when a sibling starts listening is answered
 *  with a presence note only when it is at most this old: an older one's
 *  device has stopped waiting. Chosen, not measured. */
public const val KNOCK_BACKLOG_ANSWER_MS: Long = 30_000L

/** A device whose signed DeviceGroup read is refused this many times in a row
 *  (not for its clock) holds an epoch chain the journal no longer has: it was
 *  reset under it, and the device rejoins. Chosen, not measured. */
public const val REFUSALS_BEFORE_LOST: Int = 3

/** How long the new device waits for a presence note or a Welcome after its
 *  knock took [rttMs]: `max(4 x rtt, 1 s)`. */
public fun rejoinAnswerWindowMs(rttMs: Long): Long =
    maxOf(REJOIN_ANSWER_RTT_FACTOR * rttMs.coerceAtLeast(0L), REJOIN_ANSWER_MIN_WINDOW_MS)

/** Does a sibling post a presence note for a knock it read in its first read
 *  of a session (knocked at [knockAtMs] by the DS clock, `null` unknown)? */
public fun shouldAnswerBacklogKnock(knockAtMs: Long?, nowMs: Long): Boolean =
    knockAtMs != null && kotlin.math.abs(nowMs - knockAtMs) <= KNOCK_BACKLOG_ANSWER_MS

/** What a rejoining device read on its rendezvous. */
public sealed class RendezvousAnswer {
    /** A sibling is running: wait for its Welcome. */
    public object Present : RendezvousAnswer()

    /** The Welcome itself. */
    public class Welcome(public val bytes: ByteArray) : RendezvousAnswer()
}

/** The answer in one read of the rendezvous: a Welcome wins over a presence
 *  note in the same rows; `null` when there is neither. */
public fun rendezvousAnswer(rows: List<ByteArray>, isPresent: (ByteArray) -> Boolean): RendezvousAnswer? {
    var present = false
    for (row in rows) {
        if (isPresent(row)) {
            present = true
        } else if (row.isNotEmpty()) {
            return RendezvousAnswer.Welcome(row)
        }
    }
    return if (present) RendezvousAnswer.Present else null
}

/** A drained read where the DS names an epoch below this handle's: the
 *  journal's chain is not the one this device holds (it was reset under it). */
public fun isJournalReset(localEpoch: ULong, drained: Boolean, latestEpoch: ULong?): Boolean =
    drained && latestEpoch != null && latestEpoch < localEpoch

/** The `error` code of a DS error body, when it is a short kebab-case code
 *  (never an identifier); `null` otherwise. */
public fun dsErrorCode(body: String): String? {
    val match = Regex("\"error\"\\s*:\\s*\"([a-z0-9-]{1,64})\"").find(body) ?: return null
    return match.groupValues[1]
}

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
    /** The previous leaf is present, the policy wants its removal
     *  authorised, and the knock carries no self-removal proof that covers
     *  it: a passkey attestation this device cannot obtain. */
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
    /** The knock carries a self-removal proof that covers replacing the
     *  previous leaf by its KeyPackage (`deviceGroupSelfRemovalCovers`). */
    val selfRemovalProofCovers: Boolean = false,
    /** A leaf of the DeviceGroup already carries the knock's KeyPackage leaf
     *  (`containsLeafOfKeyPackage`): another sibling answered it. A
     *  replacement keeps the key (often the leaf index too), so the roster of
     *  keys cannot tell. */
    val keyPackageAlreadyIn: Boolean = false,
)

/** The pure admission rule (see the file doc). */
public fun admissionVerdict(input: AdmissionInput): AdmissionVerdict {
    if (input.knockId in input.handled) return AdmissionVerdict.HANDLED
    if (input.keyHex == input.ownKeyHex) return AdmissionVerdict.OWN_KEY
    if (input.knockAtMs < input.watchingSinceMs) return AdmissionVerdict.BEFORE_WATCHING
    if (input.keyPackageAlreadyIn) return AdmissionVerdict.ALREADY_ADMITTED
    val added = input.lastAddedAtMs
    if (added != null && added >= input.knockAtMs) return AdmissionVerdict.ALREADY_ADMITTED
    if (!input.listed) return AdmissionVerdict.NOT_LISTED
    val present = input.roster.any { it.keyHex == input.keyHex }
    if (present && input.policyRequiresAttestation && !input.selfRemovalProofCovers) {
        return AdmissionVerdict.NEEDS_ATTESTATION
    }
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
    /** A running device answered this knock with a presence note: its
     *  Welcome is waited for with no deadline. A knock whose answer window
     *  was never seen to its end (the app stopped during it) is `false` and
     *  is made again. */
    val answered: Boolean = false,
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

        /** Whether to knock (again): no knock out, no device answered the one
         *  out, it was made long enough ago, or the clock went back. */
        public fun shouldKnockAgain(state: RejoinState?, nowMs: Long): Boolean =
            state == null || !state.answered || nowMs - state.knockedAtMs >= REKNOCK_AFTER_MS ||
                nowMs < state.knockedAtMs
    }
}

/** What a knock came to within its answer window. */
public class KnockAnswer(
    /** The rejoin state the knock was made with (saved before it went out). */
    public val state: RejoinState,
    /** `null`: no presence note and no Welcome in the window. */
    public val answer: RendezvousAnswer?,
)

/** What one rejoin attempt needs. */
public interface RejoinPorts {
    /** Delete the local DeviceGroup state (snapshot and pending commit). */
    public fun dropLocalState()

    /** Make sure this device's directory KeyPackage is published. */
    public suspend fun publishDirectoryKeyPackage()

    public fun loadState(): RejoinState?

    public fun saveState(state: RejoinState)

    /** Whether this account's login handle is known (from the session or the
     *  own-username register). Without it no device could match a knock. */
    public fun hasLoginHandle(): Boolean

    /** Build a fresh joiner, save its state, start reading its rendezvous,
     *  knock with it while timing the knock's round trip, and wait
     *  [rejoinAnswerWindowMs] of that round trip for a presence note or a
     *  Welcome. */
    public suspend fun knockAndListen(nowMs: Long): KnockAnswer

    /** Wait for the Welcome at [state]'s rendezvous until [deadlineMs],
     *  passing over presence notes; `null` on a timeout. */
    public suspend fun awaitWelcome(state: RejoinState, deadlineMs: Long): ByteArray?

    /** Join from the Welcome with the joiner kept in [state] and persist the
     *  joined DeviceGroup (fail-closed). */
    public suspend fun join(state: RejoinState, welcome: ByteArray)

    public fun clearState()

    /** Another device answered this device's knock (a presence note or a
     *  Welcome inside the answer window): recorded for the chat list
     *  (`docs/ux/identity-recovery.md` 7.2). */
    public fun recordAnswered()

    public fun now(): Long
}

/** What one pass of the device's side came to. */
public enum class RejoinPass {
    JOINED,
    WAITING,
    /** Nobody answered the knock: found the DeviceGroup on this device. */
    FOUND,
}

/**
 * One pass of the device side: drop the state (first pass only), make sure a
 * knock is out and was answered, wait for its Welcome until the re-knock
 * time. A knock nobody answered in its window is [RejoinPass.FOUND].
 */
public suspend fun rejoinOnce(ports: RejoinPorts, firstPass: Boolean): RejoinPass {
    if (firstPass) {
        ports.dropLocalState()
        ports.publishDirectoryKeyPackage()
    }
    var state = ports.loadState()
    if (RejoinState.shouldKnockAgain(state, ports.now())) {
        // No username anywhere: no running device could match a knock, so
        // nobody can answer one. Found at once (a live device then finds its
        // journal reset under it and rejoins, third amendment point 5).
        if (!ports.hasLoginHandle()) {
            ports.clearState()
            return RejoinPass.FOUND
        }
        val knocked = ports.knockAndListen(ports.now())
        when (val answer = knocked.answer) {
            null -> {
                ports.clearState()
                return RejoinPass.FOUND
            }
            is RendezvousAnswer.Welcome -> {
                ports.recordAnswered()
                ports.join(knocked.state, answer.bytes)
                ports.clearState()
                return RejoinPass.JOINED
            }
            RendezvousAnswer.Present -> {
                ports.recordAnswered()
                val answered = knocked.state.copy(answered = true)
                ports.saveState(answered)
                state = answered
            }
        }
    }
    val current = state ?: return RejoinPass.WAITING
    val welcome = ports.awaitWelcome(current, current.knockedAtMs + REKNOCK_AFTER_MS) ?: return RejoinPass.WAITING
    ports.join(current, welcome)
    ports.clearState()
    return RejoinPass.JOINED
}
