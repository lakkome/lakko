// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// What the SFU actually gives this device of each person's video, and the
/// picture a tile therefore shows (ADR-0070 step 6); the twin of
/// `apps/web/src/calls/video-given.ts`.
///
/// This device tells the SFU what each tile wants (``VideoWants``). With
/// downlink control on, the SFU may give less when this device's downlink is
/// short, and says so in a `video-given` frame: the whole set each time, one
/// entry per person it sends video of. A track it does not give is paused on
/// purpose and resumed at a key frame.
///
///   * the picture a tile shows is the **lower** of what the tile wants and
///     what the SFU gives (``effectiveQuality(wanted:given:)``);
///   * no `video-given` ever received, or a person the latest one does not
///     name: what the tile wants, so an SFU with downlink control off behaves
///     exactly as before;
///   * a new frame replaces the whole previous set; a person who leaves is
///     forgotten (``VideoGivenBook``);
///   * what this device **asks** for never changes because of a
///     `video-given`: the SFU must keep knowing the real wish to be able to
///     give it back. Nothing here feeds ``VideoWantsBook``.
///
/// Pure.
enum VideoGiven {

    /// The picture a tile shows: the lower of what it wants and what the SFU
    /// gives. `given` is `nil` when the SFU has said nothing about the
    /// person; the tile then shows what it wants.
    static func effectiveQuality(
        wanted: TileVideoQuality, given: TileVideoQuality?
    ) -> TileVideoQuality {
        guard let given else { return wanted }
        return TileVideoQuality.lower(wanted, given)
    }

    /// ``effectiveQuality(wanted:given:)`` as the quality the tile's
    /// renderers are built for. `light` means the light track alone, which is
    /// nothing at all for a person who sends no light copy: such a tile keeps
    /// the heavy picture it wants (as ``VideoWants/askedQuality(_:hasLight:)``
    /// has it), paused or not, because there is nothing else to show.
    static func shownQuality(
        wanted: TileVideoQuality, given: TileVideoQuality?, hasLight: Bool
    ) -> TileVideoQuality {
        let effective = effectiveQuality(wanted: wanted, given: given)
        return effective == .light && !hasLight
            ? VideoWants.askedQuality(wanted, hasLight: false) : effective
    }

    /// The track a tile's heavy renderer plays, given what the tile wants
    /// and what the SFU gives: the main track at `main`; the middle track at
    /// `middle` (the main one while the person has no middle track); none at
    /// `light`, where the light track is shown alone and no heavy renderer
    /// waits for frames the SFU is not sending.
    static func heavyTrack<T>(
        wanted: TileVideoQuality, given: TileVideoQuality?, main: T?, middle: T?, hasLight: Bool
    ) -> T? {
        VideoWants.heavyTrack(
            for: shownQuality(wanted: wanted, given: given, hasLight: hasLight),
            main: main, middle: middle)
    }

    /// One person whose shown picture a `video-given` frame changed.
    struct Change: Equatable {
        let peer: String
        let wanted: TileVideoQuality
        /// What the SFU gives now; `nil` when the frame does not name them.
        let given: TileVideoQuality?
    }

    /// The people whose effective quality differs between two sets, in the
    /// order of `wants` (what this device asks of each person now).
    static func changes(
        wants: [(peer: String, quality: TileVideoQuality)],
        before: (String) -> TileVideoQuality?,
        after: (String) -> TileVideoQuality?
    ) -> [Change] {
        wants.compactMap { want in
            let now = after(want.peer)
            let was = effectiveQuality(wanted: want.quality, given: before(want.peer))
            guard was != effectiveQuality(wanted: want.quality, given: now) else { return nil }
            return Change(peer: want.peer, wanted: want.quality, given: now)
        }
    }

    /// One line for one `video-given` frame that changed the picture a tile
    /// shows: every person it changed, each as `peer= want= given=`
    /// (`given=-` when the frame no longer names them). `nil` when it
    /// changed nobody's.
    static func describe(_ label: String, _ changes: [Change]) -> String? {
        if changes.isEmpty { return nil }
        return "\(label) "
            + changes.map {
                "peer=\($0.peer) want=\($0.wanted.wireName) given=\($0.given?.wireName ?? "-")"
            }.joined(separator: " | ")
    }
}

/// The latest `video-given` set, by person.
///
/// Empty until the first frame. Each frame replaces the whole set, so a
/// person an earlier frame named and the latest does not is back to "nothing
/// said".
struct VideoGivenBook: Equatable {

    private var given: [String: TileVideoQuality] = [:]

    init() {}

    /// A `video-given` frame arrived: its entries are the whole set now. A
    /// person named twice has the later entry; an entry with an empty peer
    /// id names nobody and is left out.
    mutating func replace(_ entries: [SfuVideoGiven]) {
        var next: [String: TileVideoQuality] = [:]
        for entry in entries where !entry.peer.isEmpty { next[entry.peer] = entry.quality }
        given = next
    }

    /// A person left. Returns whether they had an entry.
    @discardableResult
    mutating func removePeer(_ peerId: String) -> Bool {
        given.removeValue(forKey: peerId) != nil
    }

    /// What the SFU gives of `peerId`; `nil` when it has not said.
    func given(of peerId: String) -> TileVideoQuality? {
        given[peerId]
    }

    /// How many people the set names.
    var count: Int { given.count }

    /// The set as it stands.
    var snapshot: [String: TileVideoQuality] { given }

    /// Forget everything: the call is over.
    mutating func clear() {
        given = [:]
    }
}
