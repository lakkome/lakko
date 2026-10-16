// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The sealed `signal-relay` channel of an SFU call and who a peer is
/// (ADR-0078 section 2). Mirror of the web `calls/call-relay.ts` and the
/// Android `calls/CallRelay.kt`.
///
/// The SFU admits a peer by an anonymous token carrying a random peer id and
/// knows no account, so it cannot say who a peer is. Each participant says it
/// itself: it announces its own SFU peer id, signed with its MLS leaf key
/// (`composeCallPeerAnnouncement`), sealed under a key derived from the call
/// seed (`CallRelayKey`), and every receiver checks the signature against the
/// leaf in the call's MLS roster (`verifyCallPeerAnnouncement`). The leaf is
/// account-bound (ADR-0050), so a verified announcement names the person. The
/// camera on/off media state rides the same sealed channel.
///
/// The relay key changes with the call seed (every MLS epoch, ADR-0029); the
/// previous epoch's key is kept for opening.
@MainActor
final class CallRelay {
    /// What an opened relay payload carries.
    enum Opened: Equatable {
        case announcement(Data)
        case mediaState(cameraOn: Bool)
        case unknown
    }

    private var current: CallRelayKey?
    private var previous: CallRelayKey?

    /// Whether a seed has been set (a link joiner has none before its Welcome).
    var hasKey: Bool { current != nil }

    /// The call seed of a new epoch: the current key becomes the previous. A
    /// seed that is not 32 bytes leaves the relay as it was: it is a naming
    /// and camera-state channel, never the media path.
    func setSeed(_ seed: Data) {
        guard let next = try? CallRelayKey.fromCallSeed(seed: seed) else { return }
        previous = current
        current = next
    }

    /// Seal a relay payload under the current key; `nil` with no key.
    func seal(_ plaintext: Data) -> Data? {
        guard let current else { return nil }
        return try? current.seal(plaintext: plaintext)
    }

    /// Open with the current, then the previous key; `nil` when neither opens
    /// it (not a sealed relay payload, or another call).
    func open(_ sealed: Data) -> Opened? {
        for key in [current, previous].compactMap({ $0 }) {
            guard let plaintext = try? key.open(sealed: sealed) else { continue }
            switch classifyCallRelayPlaintext(plaintext: plaintext) {
            case .peerAnnouncement(let announcement): return .announcement(announcement)
            case .mediaState(let cameraOn): return .mediaState(cameraOn: cameraOn)
            case .unknown: return .unknown
            }
        }
        return nil
    }

    /// Drop both keys.
    func dispose() {
        current = nil
        previous = nil
    }
}

/// Who a verified announcement names.
struct VerifiedPeerIdentity: Equatable {
    /// The person: base64 of the 32-byte account key (ADR-0050).
    let personKeyB64: String
    /// The signer's MLS leaf signature key, lower-case hex: the handle the
    /// link call's committer election keys members on.
    let memberKeyHex: String
}

/// Composes this device's announcement and verifies everyone else's. The
/// group is read at each call, since the call's MLS group changes epoch (and a
/// link joiner has none before its Welcome).
struct CallPeerAnnouncer {
    /// This device's announcement of `selfPeerId`, or `nil` when it cannot
    /// make one yet.
    let compose: @MainActor (_ selfPeerId: String) -> Data?
    /// Who a verified announcement from `from` names, or `nil`.
    let verify: @MainActor (_ from: String, _ announcement: Data) -> VerifiedPeerIdentity?

    /// The announcer for a call whose roster is `group()`.
    static func forGroup(
        identity: Identity,
        callId: Data,
        group: @escaping @MainActor () -> Group?
    ) -> CallPeerAnnouncer {
        CallPeerAnnouncer(
            compose: { selfPeerId in
                guard group() != nil else { return nil }
                return try? composeCallPeerAnnouncement(
                    identity: identity, callId: callId, peerId: selfPeerId)
            },
            verify: { from, announcement in
                guard let g = group() else { return nil }
                return verifiedIdentity(group: g, callId: callId, from: from, announcement: announcement)
            }
        )
    }

    /// `verifyCallPeerAnnouncement`, as the identity a tile and the election
    /// read; `nil` on any refusal.
    static func verifiedIdentity(
        group: Group,
        callId: Data,
        from: String,
        announcement: Data
    ) -> VerifiedPeerIdentity? {
        guard let verified = try? verifyCallPeerAnnouncement(
            group: group, callId: callId, relayFrom: from, announcement: announcement),
            verified.accountKey.count == 32
        else { return nil }
        return VerifiedPeerIdentity(
            personKeyB64: verified.accountKey.base64EncodedString(),
            memberKeyHex: verified.signatureKey.map { String(format: "%02x", $0) }.joined())
    }
}
