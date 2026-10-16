// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import uniffi.lakko_ffi_uniffi.CallRelayKey
import uniffi.lakko_ffi_uniffi.CallRelayPayload
import uniffi.lakko_ffi_uniffi.Group
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.classifyCallRelayPlaintext
import uniffi.lakko_ffi_uniffi.composeCallPeerAnnouncement
import uniffi.lakko_ffi_uniffi.verifyCallPeerAnnouncement

/**
 * The sealed `signal-relay` channel of an SFU call and who a peer is
 * (ADR-0078 section 2). Mirror of the web `calls/call-relay.ts` and the iOS
 * `Calls/CallRelay.swift`.
 *
 * The SFU admits a peer by an anonymous token carrying a random peer id and
 * knows no account, so it cannot say who a peer is. Each participant says it
 * itself: it announces its own SFU peer id, signed with its MLS leaf key
 * (`composeCallPeerAnnouncement`), sealed under a key derived from the call
 * seed (`CallRelayKey`), and every receiver checks the signature against the
 * leaf in the call's MLS roster (`verifyCallPeerAnnouncement`). The leaf is
 * account-bound (ADR-0050), so a verified announcement names the person. The
 * camera on/off media state rides the same sealed channel.
 *
 * The relay key changes with the call seed (every MLS epoch, ADR-0029); the
 * previous epoch's key is kept for opening. Not thread-safe: the session uses
 * it from its main-confined scope.
 */
public class CallRelay {
    /** What an opened relay payload carries. */
    public sealed interface Opened {
        public class Announcement(public val bytes: ByteArray) : Opened
        public data class MediaState(val cameraOn: Boolean) : Opened
        public data object Unknown : Opened
    }

    private var current: CallRelayKey? = null
    private var previous: CallRelayKey? = null

    /** Whether a seed has been set (a link joiner has none before its Welcome). */
    public val hasKey: Boolean get() = current != null

    /**
     * The call seed of a new epoch: the current key becomes the previous. A
     * seed the core refuses (not 32 bytes) leaves the relay as it was: it is a
     * naming and camera-state channel, never the media path.
     */
    public fun setSeed(seed: ByteArray) {
        val next = try {
            CallRelayKey.fromCallSeed(seed)
        } catch (_: Throwable) {
            return
        }
        previous?.close()
        previous = current
        current = next
    }

    /** Seal a relay payload under the current key; `null` with no key. */
    public fun seal(plaintext: ByteArray): ByteArray? = try {
        current?.seal(plaintext)
    } catch (_: Throwable) {
        null
    }

    /** Open with the current, then the previous key; `null` when neither
     *  opens it (not a sealed relay payload, or another call). */
    public fun open(sealed: ByteArray): Opened? {
        for (key in listOfNotNull(current, previous)) {
            val plaintext = try {
                key.open(sealed)
            } catch (_: Throwable) {
                continue
            }
            return when (val payload = classifyCallRelayPlaintext(plaintext)) {
                is CallRelayPayload.PeerAnnouncement -> Opened.Announcement(payload.announcement)
                is CallRelayPayload.MediaState -> Opened.MediaState(payload.cameraOn)
                CallRelayPayload.Unknown -> Opened.Unknown
            }
        }
        return null
    }

    /** Drop both keys. */
    public fun dispose() {
        current?.close()
        previous?.close()
        current = null
        previous = null
    }
}

/** A verified peer announcement: who the peer is. */
public data class VerifiedPeerIdentity(
    /** The person: base64 of the 32-byte account key (ADR-0050). */
    val personKeyB64: String,
    /** The signer's MLS leaf signature key, lower-case hex: the handle the
     *  link call's committer election keys members on. */
    val memberKeyHex: String,
)

/**
 * Composes this device's announcement and verifies everyone else's. The
 * session holds one; `null` in tests that name nobody.
 */
public class CallPeerAnnouncer(
    /** This device's announcement for its own SFU peer id, or `null` when it
     *  cannot be composed yet (no group). */
    public val compose: (selfPeerId: String) -> ByteArray?,
    /** A peer's announcement verified against the roster, or `null`. */
    public val verify: (from: String, announcement: ByteArray) -> VerifiedPeerIdentity?,
) {
    public companion object {
        /** The announcer for a call whose roster is `group()`. */
        public fun forGroup(identity: Identity, callId: ByteArray, group: () -> Group?): CallPeerAnnouncer =
            CallPeerAnnouncer(
                compose = { selfPeerId ->
                    if (group() == null) {
                        null
                    } else {
                        try {
                            composeCallPeerAnnouncement(identity, callId, selfPeerId)
                        } catch (_: Throwable) {
                            null
                        }
                    }
                },
                verify = { from, announcement ->
                    group()?.let { verifiedIdentity(it, callId, from, announcement) }
                },
            )

        /** `verifyCallPeerAnnouncement`, as the identity a tile and the
         *  election read; `null` on any refusal. */
        public fun verifiedIdentity(
            group: Group,
            callId: ByteArray,
            from: String,
            announcement: ByteArray,
        ): VerifiedPeerIdentity? {
            val verified = try {
                verifyCallPeerAnnouncement(group, callId, from, announcement)
            } catch (_: Throwable) {
                return null
            }
            if (verified.accountKey.size != 32) return null
            return VerifiedPeerIdentity(
                personKeyB64 = java.util.Base64.getEncoder().encodeToString(verified.accountKey),
                memberKeyHex = verified.signatureKey.joinToString("") { "%02x".format(it.toInt() and 0xff) },
            )
        }
    }
}
