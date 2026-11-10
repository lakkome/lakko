// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The rule for which seed the call's shared SFrame keys
/// (``SFrameCallKeys``) are at, kept apart from the keys so it can be
/// tested (G-18, ADR-0029). Android holds the same rule in `CallKeysSlot`.
///
/// - An arm with no keys yet creates them at its seed.
/// - An arm at the seed the keys are at uses them.
/// - An arm at a NEW seed rekeys the keys to it first, so no transform is
///   made at a seed other than the keys' current one.
/// - An arm at a seed the keys were at BEFORE (an arm that read the seed
///   just before a rekey, or a publish that was deferred past one) uses the
///   current keys and does not rekey back: rolling a live call back to an
///   older epoch would make every encryptor seal under it. The previous key
///   stays in the ring during the overlap, so nothing a peer sent is lost.
struct CallKeysSeeds {
    enum Decision: Equatable {
        case create
        case use
        case rekey
    }

    /// How many earlier seeds are remembered. A call moves one epoch per
    /// membership commit; an arm older than this is not expected.
    static let maxSuperseded = 16

    private(set) var current: Data?
    private var superseded: [Data] = []

    /// What an arm at `seed` must do.
    func decide(armAt seed: Data) -> Decision {
        guard let current else { return .create }
        if current == seed || superseded.contains(seed) { return .use }
        return .rekey
    }

    /// The keys are now at `seed` (created or rekeyed).
    mutating func moved(to seed: Data) {
        if let current, current != seed {
            superseded.append(current)
            if superseded.count > Self.maxSuperseded {
                superseded.removeFirst(superseded.count - Self.maxSuperseded)
            }
        }
        current = seed
    }

    /// Forget every seed (the call ended).
    mutating func clear() {
        current = nil
        superseded = []
    }
}
