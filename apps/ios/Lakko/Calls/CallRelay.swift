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
/// camera and microphone media state rides the same sealed channel.
///
/// The relay key changes with the call seed (every MLS epoch, ADR-0029); the
/// previous epoch's key is kept for opening.
@MainActor
final class CallRelay {
    /// What an opened relay payload carries.
    enum Opened: Equatable {
        case announcement(Data)
        case mediaState(cameraOn: Bool, microphoneOn: Bool)
        /// A signed screen-share state (ADR-0083), still to be verified with
        /// ``CallPeerAnnouncer/verifyScreenShare``.
        case screenShare(Data)
        /// A signed call name (`docs/ux/calls.md` section 4.1), still to be
        /// verified with ``CallPeerAnnouncer/verifyCallName``.
        case callName(Data)
        /// A signed call-chat message (`docs/ux/calls.md` section 4.1.4),
        /// still to be verified with ``CallPeerAnnouncer/verifyCallChat``.
        case callChat(Data)
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
            case .mediaState(let cameraOn, let microphoneOn):
                return .mediaState(cameraOn: cameraOn, microphoneOn: microphoneOn)
            case .screenShare(let state): return .screenShare(state)
            case .callName(let name): return .callName(name)
            case .callChat(let message): return .callChat(message)
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
    /// The same leaf signature key, as bytes: what a screen-share state from
    /// this peer must be signed by (ADR-0083 section 1). Empty when built by a
    /// caller that did not have it.
    var signatureKey: Data = Data()
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
    /// A screen-share state from `from`, verified against the call's group
    /// (ADR-0083 section 1): the call id, `from` as the stamped peer id,
    /// `signerKey` (the leaf key `from`'s verified announcement named), the
    /// signature. `nil` on any refusal, and when there is no group.
    var verifyScreenShare: @MainActor (
        _ from: String, _ signerKey: Data, _ state: Data
    ) -> VerifiedCallScreenShare? = { _, _, _ in nil }
    /// This device's own leaf signature key: the fold's own key.
    var ownSignatureKey: Data = Data()
    /// This device's signed name for the call (`docs/ux/calls.md` section
    /// 4.1) as `selfPeerId`, numbered `seq`; `nil` when it cannot make one
    /// (no group yet, a name the display-name rule refuses).
    var composeCallName: @MainActor (
        _ selfPeerId: String, _ seq: UInt64, _ name: String
    ) -> Data? = { _, _, _ in nil }
    /// A call name from `from`, verified against the call's group with
    /// `signerKey` (the leaf key `from`'s verified announcement named). `nil`
    /// on any refusal, and when there is no group.
    var verifyCallName: @MainActor (
        _ from: String, _ signerKey: Data, _ name: Data
    ) -> CallNameChannel.Verified? = { _, _, _ in nil }
    /// This device's signed call-chat message (`docs/ux/calls.md` section
    /// 4.1.4) as `selfPeerId`, numbered `seq`; `nil` when it cannot make one
    /// (no group yet, a text the rule refuses).
    var composeCallChat: @MainActor (
        _ selfPeerId: String, _ seq: UInt64, _ text: String
    ) -> Data? = { _, _, _ in nil }
    /// A call-chat message from `from`, verified against the call's group
    /// with `signerKey` (the leaf key `from`'s verified announcement named).
    /// `nil` on any refusal, and when there is no group.
    var verifyCallChat: @MainActor (
        _ from: String, _ signerKey: Data, _ message: Data
    ) -> CallChatChannel.Verified? = { _, _, _ in nil }
    /// This device's signed call-chat picture (`docs/ux/calls.md` section
    /// 4.1.4.1) as `selfPeerId`, numbered `seq`; `nil` when it cannot make
    /// one (no group yet, a picture out of bounds).
    var composeCallChatPicture: @MainActor (
        _ selfPeerId: String, _ seq: UInt64, _ picture: CallChatPicture
    ) -> Data? = { _, _, _ in nil }

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
            },
            verifyScreenShare: { from, signerKey, state in
                guard let g = group() else { return nil }
                return try? verifyCallScreenShare(
                    group: g, callId: callId, peerId: from, peerSignerKey: signerKey, state: state)
            },
            ownSignatureKey: identity.signaturePublicKey(),
            composeCallName: { selfPeerId, seq, name in
                guard group() != nil else { return nil }
                return try? LakkoCore.composeCallName(
                    identity: identity, callId: callId, peerId: selfPeerId, seq: seq, name: name)
            },
            verifyCallName: { from, signerKey, name in
                guard let g = group(),
                      let verified = try? LakkoCore.verifyCallName(
                          group: g, callId: callId, peerId: from, peerSignerKey: signerKey, name: name)
                else { return nil }
                return CallNameChannel.Verified(seq: verified.seq, name: verified.name)
            },
            composeCallChat: { selfPeerId, seq, text in
                guard group() != nil else { return nil }
                return try? LakkoCore.composeCallChat(
                    identity: identity, callId: callId, peerId: selfPeerId, seq: seq, text: text)
            },
            verifyCallChat: { from, signerKey, message in
                guard let g = group(),
                      let verified = try? LakkoCore.verifyCallChat(
                          group: g, callId: callId, peerId: from, peerSignerKey: signerKey,
                          message: message),
                      verified.peerId == from
                else { return nil }
                return CallChatChannel.Verified(
                    seq: verified.seq, text: verified.text, picture: verified.picture)
            },
            composeCallChatPicture: { selfPeerId, seq, picture in
                guard group() != nil else { return nil }
                return try? LakkoCore.composeCallChatPicture(
                    identity: identity, callId: callId, peerId: selfPeerId, seq: seq, picture: picture)
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
            memberKeyHex: verified.signatureKey.map { String(format: "%02x", $0) }.joined(),
            signatureKey: verified.signatureKey)
    }
}
