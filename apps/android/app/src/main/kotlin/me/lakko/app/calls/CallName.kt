// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import java.util.concurrent.ConcurrentHashMap

/**
 * The name a participant gives themself for one call (`docs/ux/calls.md`
 * section 4.1, 2026-11-07; `lakko_calls::call_name`). The Kotlin twin of the
 * web `calls/call-name.ts`.
 *
 * Any participant of a group or link call may change the name the others see
 * on their tile, for this call only. The profile name never changes. The name
 * rides the sealed SFU relay, signed with the sender's MLS leaf key, and a
 * receiver takes it only from the peer whose verified announcement named that
 * leaf key (ADR-0078 section 2). A one-to-one call has no relay and no call
 * name.
 *
 * [CallNameChannel] is the per-call state: this device's own name and its
 * sequence number (re-sent after every announcement the session sends, so a
 * late joiner learns it), and the newest name received per peer.
 *
 * Pure: no native library, no network, so the rule is tested on the host JVM.
 * Not thread-safe: the session uses it from its main-confined scope.
 */

/** A received call name after verification: what [CallNameChannel] keeps. */
public data class VerifiedCallNameView(
    /** The name's sequence number; only a strictly greater one replaces it. */
    val seq: ULong,
    /** The name, or `null` when the sender went back to its default naming. */
    val name: String?,
)

/** What the channel is plugged into. */
public class CallNameChannelDeps(
    /** This device's signed call name as `selfPeerId`, numbered `seq`, or
     *  `null` when it cannot make one yet (no group). */
    public val compose: (selfPeerId: String, seq: ULong, name: String) -> ByteArray?,
    /** A call name from `from`, verified against the leaf key its
     *  announcement verified with, or `null` on any refusal. */
    public val verify: (from: String, signerKey: ByteArray, bytes: ByteArray) -> VerifiedCallNameView?,
    /** Seal and send a relay plaintext to the whole call; `false` when it
     *  could not (no relay key, no socket). */
    public val sendSealed: (plaintext: ByteArray) -> Boolean,
    /** This device's SFU peer id, once the SFU's Welcome named it. */
    public val selfPeerId: () -> String?,
    /** A peer's call name changed: the name, or `null` when they went back to
     *  their default naming. */
    public val onName: (peerId: String, name: String?) -> Unit,
)

/**
 * The call-name state of one SFU call session. Received names are held per
 * peer and bounded by the roster; a peer whose announcement has not verified
 * keeps only its latest payload.
 */
public class CallNameChannel(private val deps: CallNameChannelDeps) {
    private var ownName: String = ""
    private var ownSeq: ULong = 0uL
    private val peerKeys = HashMap<String, ByteArray>()
    private val pending = HashMap<String, ByteArray>()
    private val seqs = HashMap<String, ULong>()

    /** The name this device set for the call (`""` = none). */
    public fun ownCallName(): String = ownName

    /**
     * Set this device's name for the call. [name] is already normalised (the
     * display-name rule, trimmed); `""` goes back to the default. Returns
     * whether it changed. Sent at once when possible, and again after every
     * announcement.
     */
    public fun setOwnName(name: String): Boolean {
        // Nothing to send: the same name again, or "" before any name was set.
        if (name == ownName) return false
        ownName = name
        ownSeq += 1uL
        resend()
        return true
    }

    /** Send the current own name again (after the Welcome, a rekey, a join,
     *  the periodic announcement). Nothing before a name was ever set. */
    public fun resend() {
        if (ownSeq == 0uL) return
        val self = deps.selfPeerId() ?: return
        val wire = deps.compose(self, ownSeq, ownName) ?: return
        deps.sendSealed(wire)
    }

    /** [peerId]'s announcement verified with [signerKey]: a name it sent
     *  before is read now. */
    public fun onPeerIdentified(peerId: String, signerKey: ByteArray) {
        peerKeys[peerId] = signerKey
        val held = pending.remove(peerId) ?: return
        onReceived(peerId, held)
    }

    /** A sealed call name from [peerId] opened. */
    public fun onReceived(peerId: String, bytes: ByteArray) {
        val key = peerKeys[peerId]
        if (key == null) {
            // Before its announcement verified: keep the latest one only.
            pending[peerId] = bytes
            return
        }
        val verified = deps.verify(peerId, key, bytes) ?: return
        val stored = seqs[peerId]
        // `lakko_calls::call_name::call_name_is_newer`: strictly greater; a
        // re-send is not an update.
        if (stored != null && verified.seq <= stored) return
        seqs[peerId] = verified.seq
        deps.onName(peerId, verified.name)
    }

    /** [peerId] left: what it said goes with it (a rejoin under a new peer id
     *  starts fresh). */
    public fun onPeerLeft(peerId: String) {
        peerKeys.remove(peerId)
        pending.remove(peerId)
        seqs.remove(peerId)
    }

    /** The call is over: forget every peer. The own name stays readable. */
    public fun clear() {
        peerKeys.clear()
        pending.clear()
        seqs.clear()
    }
}

/** What the call screen can do with the current call's name. */
public fun interface CallNameControl {
    /** Save [name] as this device's name for the call. [name] already passed
     *  the display-name rule (the core's `normalizeDisplayName`); `""` goes
     *  back to the default. */
    public fun save(name: String)
}

/**
 * Names this device set, per call id (lower-case hex), for a rejoin of the
 * same call within this app run (`docs/ux/calls.md` section 4.1). In memory
 * only, never stored.
 */
public object RememberedCallNames {
    private val names = ConcurrentHashMap<String, String>()

    /** The call name this device set for [callIdHex] earlier in this run. */
    public fun get(callIdHex: String): String = names[callIdHex].orEmpty()

    /** Remember (or forget, for `""`) this device's name for [callIdHex]. */
    public fun put(callIdHex: String, name: String) {
        if (name.isEmpty()) names.remove(callIdHex) else names[callIdHex] = name
    }

    /** Forget everything (tests). */
    internal fun clear() {
        names.clear()
    }
}

/** The own tile's name: the name typed for the call, else the profile name
 *  (empty: the tile draws `call.participant.you`). */
public fun ownCallTileName(ownCallName: String, profileName: String): String =
    ownCallName.ifEmpty { profileName }
