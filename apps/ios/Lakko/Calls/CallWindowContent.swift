// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// What the collapsed call window and the system picture-in-picture window
/// show (`docs/ux/calls.md` sections 4.5.1 and 4.5.4), as values.
///
/// The window is a subset of the call screen and reads it from the same
/// ``CallUiState``, so the two can never disagree: who is on the picture is the
/// shared stage rule (`call_stage`, through ``GroupCallLayout/stagePerson``),
/// and which picture of them is asked for is the shared `tile_video_quality`
/// for the window's width. Pure: no SwiftUI.
struct CallWindowShown: Equatable {
    /// The person (or shared screen) on the picture: the call screen's stage.
    /// `nil` only for a call with nobody in it yet.
    let stage: CallUiState.Participant?
    /// The label under the picture: the same one the stage's tile uses.
    let name: String
    /// Everyone else beyond the person shown and this device, in a group
    /// call; 0 means nothing is printed.
    let othersBeyond: Int
    /// This device's camera is sending and the picture is not already this
    /// device: the small own inset.
    let showsOwnInset: Bool

    /// The picture is a shared screen, which draws the window lying (16:9).
    var isScreenShare: Bool { stage?.isScreen == true }
}

enum CallWindowContent {

    /// What the window shows for a call.
    ///
    /// - Parameters:
    ///   - layoutPeople: the call's tiles as the call screen lays them out
    ///     (``CallUiState/groupLayoutPeople``: the roster, plus a shared
    ///     screen's tile).
    ///   - pinnedId: the person pinned on the call screen, if any.
    ///   - speech: who holds the floor, for the stage rule.
    ///   - screenSharerId: the roster id of whoever shares the screen now.
    ///   - groupCall: a group call or a call from a link prints `+N`; a
    ///     one-to-one never does.
    ///   - ownCameraSending: this device's camera is on and has a track.
    static func shown(
        _ rules: GroupCallRules = SharedGroupCallRules(),
        layoutPeople: [CallUiState.Participant],
        pinnedId: String?,
        speech: StageSpeech,
        screenSharerId: String?,
        groupCall: Bool,
        ownCameraSending: Bool
    ) -> CallWindowShown {
        let stage = GroupCallLayout.stagePerson(
            rules, people: layoutPeople, pinnedId: pinnedId, speech: speech)
        let name = stage.map { person in
            CallPeerName.tileLabel(displayName: person.displayName, isLocal: person.isLocal)
        } ?? ""
        // The person on the picture: for a shared screen, its sharer.
        let shownPersonId = stage.flatMap { $0.isScreen ? screenSharerId : $0.id }
        let others = groupCall
            ? layoutPeople.filter { !$0.isLocal && !$0.isScreen && $0.id != shownPersonId }.count
            : 0
        return CallWindowShown(
            stage: stage,
            name: name,
            othersBeyond: others,
            showsOwnInset: ownCameraSending && stage?.isLocal != true)
    }

    /// What ``shown(_:layoutPeople:pinnedId:speech:screenSharerId:groupCall:ownCameraSending:)``
    /// answers for a live ``CallUiState``.
    @MainActor
    static func shown(
        state: CallUiState, groupCall: Bool, rules: GroupCallRules = SharedGroupCallRules()
    ) -> CallWindowShown {
        let local = state.participants.first(where: { $0.isLocal })
        return shown(
            rules,
            layoutPeople: state.groupLayoutPeople,
            pinnedId: state.pinnedId,
            speech: state.speech,
            screenSharerId: state.screenShare?.sharerParticipantId,
            groupCall: groupCall,
            ownCameraSending: (local?.camOn ?? false) && state.localVideoTrack != nil)
    }

    /// `+16`: the count of everyone else, in the interface language's digits.
    /// `nil` when there is nobody else to count.
    static func othersLabel(_ count: Int) -> String? {
        guard count > 0 else { return nil }
        return "+" + count.formatted(.number.grouping(.never))
    }

    /// The picture the window asks the SFU for, for the person shown: the
    /// shared rule for the window's width in physical pixels, which is never
    /// counted as the stage (the window is a small picture, section 4.5.1).
    static func wantedQuality(
        _ rules: TileQualityRules = SharedTileQualityRules(),
        people: Int, windowWidthPx: UInt32, hasLight: Bool
    ) -> TileVideoQuality {
        VideoWants.askedQuality(
            VideoWants.tileQuality(
                rules, people: people, tileWidthPx: windowWidthPx, pinnedOrStage: false),
            hasLight: hasLight)
    }

    /// Whether a call in `phase` has a window at all, collapsed or in the
    /// system's picture-in-picture: placed, connecting, connected or
    /// reconnecting, never only ringing. The shared rule's answer for a call
    /// with no screen up, so the window and the system window open on exactly
    /// the same phases.
    static func hasWindow(_ phase: CallBarPhase) -> Bool {
        callBarVisible(phase: phase, callScreenUp: false)
    }
}
