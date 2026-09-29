// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import uniffi.lakko_ffi_uniffi.FfiIncomingEnvelope
import uniffi.lakko_ffi_uniffi.FfiIncomingEnvelopeKind
import uniffi.lakko_ffi_uniffi.FfiReachState
import uniffi.lakko_ffi_uniffi.FfiReachStateEntry
import uniffi.lakko_ffi_uniffi.FfiReachStateMerge
import uniffi.lakko_ffi_uniffi.FfiVisibilitySetting

/*
 * ADR-0060 section 7 — the "who can find me" setting and the reach-key version
 * counters travel between the account's own devices as a DeviceGroup
 * `ServiceEnvelope` body carrying a `lakko-contacts` envelope verbatim.
 *
 * The rules, host-testable (every native call is passed in):
 *   - a received envelope is dispatched, and only Visibility and ReachState
 *     are kept from it; anything else is dropped;
 *   - a Visibility is kept only when STRICTLY newer than the one held
 *     (`visibilityIsNewer`), so an equal stamp changes nothing; a ReachState
 *     is folded by the core's `mergeReachState` (versions by maximum, the
 *     stamp deciding who re-sends);
 *   - a re-send carries the ORIGINAL stamp: the stored wire is sent verbatim,
 *     never recomposed (the ADR-0044 lesson: a re-stamped stale value would
 *     arrive newer than the live one).
 */

/** What a DeviceGroup service envelope is to this device. */
internal sealed interface ReceivedDeviceSync {
    /** The setting, with the bytes it arrived as. */
    class Visibility(val setting: FfiVisibilitySetting, val wire: ByteArray) : ReceivedDeviceSync

    /** The reach-key version counters, with the bytes they arrived as. */
    class ReachState(val state: FfiReachState, val wire: ByteArray) : ReceivedDeviceSync

    /** Any other envelope kind, or one that does not decode: dropped. */
    data object Foreign : ReceivedDeviceSync
}

/** Classify [wire] with [dispatch] (`dispatchIncomingEnvelope`); a dispatch
 *  that throws is [ReceivedDeviceSync.Foreign]. */
internal fun classifyDeviceSync(
    wire: ByteArray,
    dispatch: (ByteArray) -> FfiIncomingEnvelope,
): ReceivedDeviceSync {
    val dispatched = try {
        dispatch(wire)
    } catch (_: Throwable) {
        return ReceivedDeviceSync.Foreign
    }
    return when (dispatched.kind) {
        FfiIncomingEnvelopeKind.VISIBILITY ->
            dispatched.visibility?.let { ReceivedDeviceSync.Visibility(it, wire.copyOf()) }
                ?: ReceivedDeviceSync.Foreign
        FfiIncomingEnvelopeKind.REACH_STATE ->
            dispatched.reachState?.let { ReceivedDeviceSync.ReachState(it, wire.copyOf()) }
                ?: ReceivedDeviceSync.Foreign
        else -> ReceivedDeviceSync.Foreign
    }
}

/** Whether a received value stamped [incomingStamp] replaces the one held
 *  ([storedStamp], `null` when none: compared as 0, the default's stamp). */
internal fun keepsIncoming(
    incomingStamp: ULong,
    storedStamp: ULong?,
    isNewer: (incoming: ULong, stored: ULong) -> Boolean,
): Boolean = isNewer(incomingStamp, storedStamp ?: 0uL)

/** The stamp for a value this device changes LOCALLY now (a rotation, a
 *  setting change): strictly after the one it replaces, even when the wall
 *  clock went backwards. A merge with a sibling's state is stamped by the
 *  core ([adoptReachStateMerge]), never here. */
internal fun nextSyncStamp(nowMs: Long, previous: ULong?): ULong {
    val now = nowMs.coerceAtLeast(0L).toULong()
    val floor = (previous ?: 0uL) + 1uL
    return if (now >= floor) now else floor
}

/** The version counters a ReachState carries, as slots. Entries whose class
 *  keys no scope keep an empty scope, as [ReachManager] stores them. */
internal fun reachStateVersions(state: FfiReachState): Map<ReachSlot, Int> =
    state.entries.associate { entry ->
        val kind = entry.`class`.toKind()
        ReachSlot(kind, if (kind.scopeless) "" else reachHex(entry.scopeId)) to entry.version.toInt()
    }

/** The version counters as ReachState entries (the scopeless classes carry
 *  an empty scope), in a stable order. */
internal fun reachStateEntries(versions: Map<ReachSlot, Int>): List<FfiReachStateEntry> =
    versions.entries
        .sortedWith(compareBy({ it.key.kind.ordinal }, { it.key.scopeHex }))
        .map { (slot, version) ->
            FfiReachStateEntry(
                slot.kind.toFfi(),
                if (slot.kind.scopeless) ByteArray(0) else reachUnhex(slot.scopeHex) ?: ByteArray(0),
                version.coerceAtLeast(0).toUInt(),
            )
        }

/**
 * What this device does with a ReachState from a sibling, as the core's
 * `mergeReachState` decided it (ADR-0060 §7; the one rule, in
 * `lakko_contacts::visibility::merge_reach_state`): counters by maximum per
 * slot, and the stamp deciding who re-sends.
 *
 * [versions] and [storeWire] are set when the merged state must be persisted
 * (its stamp included, exactly as returned); [sendWire] when it must go to the
 * account's other devices, as returned and never re-stamped here. Both wires
 * are the same composed envelope of that one state.
 */
internal class ReachStateAdoption(
    val versions: Map<ReachSlot, Int>?,
    val storeWire: ByteArray?,
    val sendWire: ByteArray?,
)

/** Run the core merge ([merge]: `mergeReachState`) of [incoming] into
 *  [stored] and turn its answer into [ReachStateAdoption]; [compose] is
 *  `composeReachStateEnvelope`. A merge the core refuses changes nothing. */
internal fun adoptReachStateMerge(
    stored: FfiReachState,
    incoming: FfiReachState,
    nowMs: Long,
    merge: (stored: FfiReachState, incoming: FfiReachState, nowMs: ULong) -> FfiReachStateMerge,
    compose: (FfiReachState) -> ByteArray,
): ReachStateAdoption {
    val decided = try {
        merge(stored, incoming, nowMs.coerceAtLeast(0L).toULong())
    } catch (_: Throwable) {
        return ReachStateAdoption(null, null, null)
    }
    if (!decided.store && !decided.send) return ReachStateAdoption(null, null, null)
    val wire = try {
        compose(decided.state)
    } catch (_: Throwable) {
        return ReachStateAdoption(null, null, null)
    }
    return ReachStateAdoption(
        versions = if (decided.store) reachStateVersions(decided.state) else null,
        storeWire = if (decided.store) wire else null,
        sendWire = if (decided.send) wire else null,
    )
}

/** What a device admitting a new one to the DeviceGroup sends it: the stored
 *  Visibility and ReachState envelopes, verbatim (each with its original
 *  stamp), those this device holds at all. */
internal fun deviceSyncEnvelopesToSend(visibilityHex: String?, reachStateHex: String?): List<ByteArray> =
    listOfNotNull(visibilityHex?.let(::reachUnhex), reachStateHex?.let(::reachUnhex))
