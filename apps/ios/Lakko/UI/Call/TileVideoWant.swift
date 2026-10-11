// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// One tile's side of asking the call for a picture
/// (`docs/ux/group-call-grid.md` section 7.6): its own identity for the
/// call's book of wants, and its width in physical pixels once measured and
/// settled. The rules themselves are ``VideoWants`` and the shared
/// `tile_video_quality`; this is what stands between them and a SwiftUI
/// view.
@MainActor
final class TileVideoWant: ObservableObject {

    /// The tile's width in physical pixels, settled; `nil` until the tile
    /// was measured.
    @Published private(set) var widthPx: UInt32?
    private var settled: SettledWidth?

    /// The tile was laid out at `px` physical pixels. A tile not laid out
    /// yet has no width to ask about.
    func measured(_ px: UInt32) {
        guard px > 0 else { return }
        if settled == nil {
            settled = SettledWidth { [weak self] width in
                guard let self, self.widthPx != width else { return }
                self.widthPx = width
            }
        }
        settled?.observe(px)
    }
}

/// Tells the live SFU call which picture of a person this view shows, and
/// withdraws it when the view goes away or stops showing one (`quality`
/// `nil`). The call asks the SFU for the highest quality any view of a person
/// reports, and for the light picture alone of a person no view reports.
struct TileVideoWantReport: ViewModifier {

    let state: CallUiState
    let participantId: String
    let quality: TileVideoQuality?

    /// Lives as long as this view does; its identity is the tile's key in
    /// the call's book.
    @StateObject private var token = Token()

    final class Token: ObservableObject {}

    func body(content: Content) -> some View {
        content
            .onAppear { report(quality) }
            .onChange(of: quality) { _, next in report(next) }
            // The same view now shows someone else: its entry in the book is
            // replaced, since the book is keyed by the view.
            .onChange(of: participantId) { _, _ in report(quality) }
            .onDisappear { report(nil) }
    }

    private func report(_ quality: TileVideoQuality?) {
        state.reportTileVideoQuality(
            tile: ObjectIdentifier(token), participantId: participantId, quality: quality)
    }
}

/// The one tile of a group call whose controls a tap brought up
/// (`docs/ux/group-call-grid.md` section 6.2): ``TileControlsReveal`` as a
/// SwiftUI view reads it.
@MainActor
final class TileControlsRevealModel: ObservableObject {

    /// The tile whose controls are up, or `nil`.
    @Published private(set) var tileId: String?
    private lazy var reveal = TileControlsReveal { [weak self] id in self?.tileId = id }

    /// A tap on a tile: its controls come up for a few seconds. Nothing
    /// else happens; in particular nobody is pinned.
    func tap(_ tileId: String) {
        reveal.tap(tileId)
    }

    /// A tap landed beside every tile, or the pin control was used.
    func hide() {
        reveal.hide()
    }
}
