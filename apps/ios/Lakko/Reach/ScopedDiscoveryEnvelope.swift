// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0058 — the five "who can find me" service envelopes that ride MLS
/// application messages, classified by the core's dispatcher
/// (`dispatchIncomingEnvelope`). None is ever rendered as a message.
public enum ScopedDiscoveryEnvelope: Equatable {
    /// A contact's introduction record (sent in a one-to-one conversation).
    case introductionOffer(record: Data)
    /// A contact's padded bundle of their contacts' records.
    case introductions(FfiIntroductionsBundle)
    /// The owner's "who can find me" setting (on the owner's DeviceGroup).
    case visibility(FfiVisibilitySetting)
    /// The owner's reach-key version counters (on the owner's DeviceGroup).
    case reachState(FfiReachState)
    /// A proposer's committer delegations for the person its Add names.
    case committerDelegation(FfiCommitterDelegationEnvelope)

    /// First byte of every ADR-0058 domain (`lakko/...`): keeps an FFI call
    /// off the path for a chat envelope (which opens with `{`). Decides nothing.
    private static let firstByte: UInt8 = 0x6C

    /// The envelope `bytes` carries, or `nil` for anything else (a chat body,
    /// another service envelope, or one of these five failing its bounds —
    /// which the core refuses as a whole).
    public static func decode(_ bytes: Data) -> ScopedDiscoveryEnvelope? {
        guard bytes.first == firstByte,
              let env = try? dispatchIncomingEnvelope(plaintext: bytes) else { return nil }
        return from(env)
    }

    /// Map a dispatched envelope; `nil` for any other kind.
    static func from(_ env: FfiIncomingEnvelope) -> ScopedDiscoveryEnvelope? {
        switch env.kind {
        case .introductionOffer:
            return env.introductionRecord.map { .introductionOffer(record: $0) }
        case .introductions:
            return env.introductions.map { .introductions($0) }
        case .visibility:
            return env.visibility.map { .visibility($0) }
        case .reachState:
            return env.reachState.map { .reachState($0) }
        case .committerDelegation:
            return env.committerDelegation.map { .committerDelegation($0) }
        default:
            return nil
        }
    }
}
