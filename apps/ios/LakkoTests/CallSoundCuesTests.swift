// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// `docs/ux/calls.md` section 3.4 — which sound a call makes, and when.
///
/// The defect, found live on 2026-10-02: every call that was not yet connected
/// looped the outgoing tone, so a person alone in a group call they had just
/// started heard a phone ringing, and so did everyone who arrived by a link.
///
/// The decision is the REAL shared rule (`lakko-ui-rules::ringback_plays`,
/// reached through the binding); what plays is recorded by a fake player.
@MainActor
final class CallSoundCuesTests: XCTestCase {

    private final class Recorder: CallSoundPlaying {
        var looping: Set<CallSoundController.SoundName> = []
        var everLooped: [CallSoundController.SoundName] = []
        var played: [CallSoundController.SoundName] = []
        var stopAlls = 0

        func playOnce(_ name: CallSoundController.SoundName) { played.append(name) }
        func startLoop(_ name: CallSoundController.SoundName) {
            if looping.insert(name).inserted { everLooped.append(name) }
        }
        func stopLoop(_ name: CallSoundController.SoundName) { looping.remove(name) }
        func stopAll() {
            looping.removeAll()
            stopAlls += 1
        }
    }

    private let rules = SharedGroupCallRules()

    /// Every phase a call passes through on its way up, and down.
    private let everyPhase: [CallBarPhase] = [
        .idle, .preparing, .calling, .connecting, .connected, .reconnecting, .connected, .ended,
    ]

    // MARK: - The outgoing tone

    /// A group call rings nobody, and neither does a call joined from a link:
    /// the tone is never started, in any phase, from the first moment on.
    func testAGroupCallNeverStartsTheRingback() {
        let sounds = Recorder()
        var cues = CallSoundCues(groupCall: true)
        cues.begin(phase: .preparing, remoteIds: [], sounds: sounds, rules: rules)
        XCTAssertTrue(sounds.looping.isEmpty)
        for phase in everyPhase {
            cues.phaseChanged(phase, sounds: sounds, rules: rules)
            XCTAssertFalse(sounds.looping.contains(.ringbackOutgoing), "\(phase)")
        }
        XCTAssertEqual(sounds.everLooped, [], "the tone was never started at all")
    }

    func testAOneToOneCallRingsOnlyWhileItWaitsForAnAnswer() {
        let sounds = Recorder()
        var cues = CallSoundCues(groupCall: false)
        cues.begin(phase: .calling, remoteIds: [], sounds: sounds, rules: rules)
        XCTAssertTrue(sounds.looping.contains(.ringbackOutgoing), "ringing out")
        cues.phaseChanged(.connecting, sounds: sounds, rules: rules)
        XCTAssertFalse(
            sounds.looping.contains(.ringbackOutgoing),
            "the tone stops at the answer; connecting is silent")
        cues.phaseChanged(.connected, sounds: sounds, rules: rules)
        XCTAssertFalse(sounds.looping.contains(.ringbackOutgoing))
    }

    /// The callee side: the screen comes up after the answer, in `connecting`,
    /// and a call still ringing HERE is not one this device placed.
    func testTheDeviceThatAnsweredNeverHearsTheRingback() {
        let sounds = Recorder()
        var cues = CallSoundCues(groupCall: false)
        cues.begin(
            phase: CallBarPresentation.phase(oneToOne: .ringing), remoteIds: [],
            sounds: sounds, rules: rules)
        XCTAssertTrue(sounds.looping.isEmpty)
        for state in [CallSession.WebCallState.connecting, .connected, .ending, .done] {
            cues.phaseChanged(
                CallBarPresentation.phase(oneToOne: state), sounds: sounds, rules: rules)
            XCTAssertFalse(sounds.looping.contains(.ringbackOutgoing), "\(state)")
        }
        XCTAssertEqual(sounds.everLooped, [])
    }

    /// The wiring asks the rule and nothing else: whatever the rule says is
    /// what happens. (A fake rule, to prove the answer is not decided here.)
    func testTheToneFollowsTheRuleItIsHanded() {
        struct Always: GroupCallRules {
            let rings: Bool
            func callGrid(count: UInt32, width: UInt32, height: UInt32) -> CallGrid {
                CallGrid(cols: 0, rows: 0, tileWidth: 0, tileHeight: 0, gap: 0, perPage: 0, pages: 0)
            }
            func callStage(people: [StageCandidate], pinned: UInt32?) -> UInt32? { nil }
            func ringbackPlays(groupCall: Bool, phase: CallBarPhase) -> Bool { rings }
        }
        let sounds = Recorder()
        var cues = CallSoundCues(groupCall: true)
        cues.phaseChanged(.connected, sounds: sounds, rules: Always(rings: true))
        XCTAssertTrue(sounds.looping.contains(.ringbackOutgoing))
        cues.phaseChanged(.connected, sounds: sounds, rules: Always(rings: false))
        XCTAssertFalse(sounds.looping.contains(.ringbackOutgoing))
    }

    // MARK: - The two short cues

    func testConnectingPlaysTheJoinCueOnceAndEndingPlaysTheLeaveCueOnce() {
        let sounds = Recorder()
        var cues = CallSoundCues(groupCall: true)
        cues.begin(phase: .connecting, remoteIds: [], sounds: sounds, rules: rules)
        cues.phaseChanged(.connected, sounds: sounds, rules: rules)
        // A second report of the same phase is not a change.
        cues.phaseChanged(.connected, sounds: sounds, rules: rules)
        XCTAssertEqual(sounds.played, [.join])
        cues.phaseChanged(.ended, sounds: sounds, rules: rules)
        cues.phaseChanged(.ended, sounds: sounds, rules: rules)
        XCTAssertEqual(sounds.played, [.join, .leave])
    }

    /// A screen that comes up on a call already connected (a call brought back
    /// from the collapsed bar) plays nothing: the call did not just connect.
    func testAScreenThatAppearsOnAConnectedCallPlaysNoCue() {
        let sounds = Recorder()
        var cues = CallSoundCues(groupCall: true)
        cues.begin(phase: .connected, remoteIds: ["a"], sounds: sounds, rules: rules)
        cues.phaseChanged(.connected, sounds: sounds, rules: rules)
        XCTAssertEqual(sounds.played, [])
    }

    func testSomeoneJoiningAndSomeoneLeavingPlayOneCueEach() {
        let sounds = Recorder()
        var cues = CallSoundCues(groupCall: true)
        cues.begin(phase: .connected, remoteIds: ["a"], sounds: sounds, rules: rules)
        cues.rosterChanged(remoteIds: ["a", "b"], connected: true, sounds: sounds)
        XCTAssertEqual(sounds.played, [.join])
        // The same roster again is not a change.
        cues.rosterChanged(remoteIds: ["a", "b"], connected: true, sounds: sounds)
        XCTAssertEqual(sounds.played, [.join])
        cues.rosterChanged(remoteIds: ["b"], connected: true, sounds: sounds)
        XCTAssertEqual(sounds.played, [.join, .leave])
        // "Alice left, Bob joined" in the same tick.
        cues.rosterChanged(remoteIds: ["c"], connected: true, sounds: sounds)
        XCTAssertEqual(sounds.played, [.join, .leave, .join, .leave])
    }

    func testTheRosterMakesNoSoundBeforeTheCallIsConnected() {
        let sounds = Recorder()
        var cues = CallSoundCues(groupCall: true)
        cues.begin(phase: .connecting, remoteIds: ["a"], sounds: sounds, rules: rules)
        cues.rosterChanged(remoteIds: ["a", "b"], connected: false, sounds: sounds)
        XCTAssertEqual(sounds.played, [])
    }
}

/// The collapsed bar of a group call says `Connecting...` while it starts.
final class GroupCallBarWordingTests: XCTestCase {

    func testAGroupCallsBarNeverSaysCalling() {
        for phase in [CallBarPhase.preparing, .calling, .connecting] {
            let shown = CallBarPresentation.groupDisplayPhase(phase)
            XCTAssertEqual(
                callBarStatus(phase: shown, connectedForMs: 0), .connecting, "\(phase)")
        }
    }

    func testEveryOtherPhaseIsLeftAsItIs() {
        for phase in [CallBarPhase.idle, .ringing, .connected, .reconnecting, .ended] {
            XCTAssertEqual(CallBarPresentation.groupDisplayPhase(phase), phase)
        }
    }
}
