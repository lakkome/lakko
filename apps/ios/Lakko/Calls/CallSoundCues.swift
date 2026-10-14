// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// What a call's sounds are played through. ``CallSoundController`` in the
/// app; a recorder in a test.
@MainActor
protocol CallSoundPlaying: AnyObject {
    func playOnce(_ name: CallSoundController.SoundName)
    func startLoop(_ name: CallSoundController.SoundName)
    func stopLoop(_ name: CallSoundController.SoundName)
    func stopAll()
}

extension CallSoundController: CallSoundPlaying {}

/// Which sound a call makes, and when (`docs/ux/calls.md` section 3.4).
///
/// **The outgoing tone** is decided by the shared rule
/// `lakko-ui-rules::call_group::ringback_plays` and by nothing else: it plays
/// only while a one-to-one call this device placed is waiting for the other
/// person, and stops the moment they answer. A group call rings nobody, and
/// neither does a call joined from a link. Before this every call that was not
/// yet connected looped the tone, so a person alone in a call they had just
/// started heard a phone ringing (found live 2026-10-02).
///
/// **The two short cues**: one when the call connects or someone joins, one
/// when someone leaves or the call ends. Each on the change only, so a second
/// report of the same phase cannot play one twice.
///
/// A value, held by the call screen; it touches nothing but the player it is
/// handed, so the whole rule runs in a test.
@MainActor
struct CallSoundCues {

    /// A group call, or a call joined from a link. A one-to-one otherwise.
    let groupCall: Bool
    private var lastPhase: CallBarPhase?
    private var lastRemoteIds: [String] = []

    init(groupCall: Bool) {
        self.groupCall = groupCall
    }

    /// The screen came up on a call that is already in `phase`.
    ///
    /// The tone follows the rule from the first moment. No cue is played: the
    /// call did not just connect, the screen did just appear (a call brought
    /// back from the collapsed bar is the ordinary case).
    mutating func begin(
        phase: CallBarPhase, remoteIds: [String], sounds: CallSoundPlaying, rules: GroupCallRules
    ) {
        lastPhase = phase
        lastRemoteIds = remoteIds
        applyRingback(phase: phase, sounds: sounds, rules: rules)
    }

    /// The call is now in `phase`.
    mutating func phaseChanged(
        _ phase: CallBarPhase, sounds: CallSoundPlaying, rules: GroupCallRules
    ) {
        applyRingback(phase: phase, sounds: sounds, rules: rules)
        guard phase != lastPhase else { return }
        lastPhase = phase
        switch phase {
        case .idle:
            sounds.stopAll()
        case .connected:
            sounds.playOnce(.join)
        case .ended:
            sounds.stopAll()
            sounds.playOnce(.leave)
        case .ringing, .preparing, .calling, .connecting, .reconnecting:
            break
        }
    }

    /// The other people in the call are now `remoteIds`.
    ///
    /// The id-based diff catches "Alice left, Bob joined" in the same tick.
    mutating func rosterChanged(remoteIds: [String], connected: Bool, sounds: CallSoundPlaying) {
        defer { lastRemoteIds = remoteIds }
        guard connected, !lastRemoteIds.isEmpty else { return }
        let before = Set(lastRemoteIds)
        let present = Set(remoteIds)
        for _ in remoteIds.filter({ !before.contains($0) }) { sounds.playOnce(.join) }
        for _ in lastRemoteIds.filter({ !present.contains($0) }) { sounds.playOnce(.leave) }
    }

    private func applyRingback(
        phase: CallBarPhase, sounds: CallSoundPlaying, rules: GroupCallRules
    ) {
        if rules.ringbackPlays(groupCall: groupCall, phase: phase) {
            sounds.startLoop(.ringbackOutgoing)
        } else {
            sounds.stopLoop(.ringbackOutgoing)
        }
    }
}
