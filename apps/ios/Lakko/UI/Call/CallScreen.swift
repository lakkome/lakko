// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import LakkoCore
import SwiftUI
import WebRTC

/// The call screen (`docs/ux/calls.md` section 4).
///
/// ## What the redesign changed
///
/// The old screen was a debugger's view of a call: a header bar with a hairline,
/// a scrolling tile grid (for a call with one other person in it), a floating
/// phase badge repeating the word the status line already carried, and a row of
/// nine loose buttons welded to the bottom edge — among them a `More` menu
/// holding the two controls people reach for mid-sentence, and no speaker button
/// at all. It also opened, for a *voice* call, on a settings card asking the
/// user to check a camera that was not on.
///
/// What it is now:
///
///   * **a voice call is a portrait** — the other person's initials large and
///     centred over a blurred, darkened ground built from their own colour,
///     their name in the largest type on the screen, and one quiet line under it
///     with the state or the running time;
///   * **a video call fills the screen** with the other person, with the
///     self-view a small draggable rectangle in a corner and the flip-camera
///     button on that rectangle, where the thing it changes is visible;
///   * **one row of equal round buttons** in a single rounded translucent bar
///     floating above the bottom safe area — Microphone, Camera, Settings, End
///     (a group call adds Participants), with no `More` menu on any call;
///   * **the off state is the loud one**, so a muted microphone is solid white
///     and unmissable rather than tinted in the accent colour, which read as
///     "this is on".
///
/// **A group call** (`docs/ux/group-call-grid.md`) is one of two layouts and
/// never a scroll: a grid of equal tiles that fills the space between the
/// status line and the control bar, with pages when there are more people than
/// a page holds, or the speaker / pinned person large in the middle with
/// everyone else in a row above. Its controls are one row with no menu behind
/// it: the chat, the settings sheet and the overflow button that held them are
/// gone, because the chat sent nothing anywhere.
///
/// What decides the shape — which controls exist, which can be pressed, what
/// the status line says, when the row hides itself — is
/// ``CallScreenPresentation``, not this file, so the rules are testable.

// MARK: - QualityDot

extension CallUiState.QualityTier {
    /// The tier as a word in the interface language (`call.quality.tier.*`).
    var word: String {
        switch self {
        case .excellent: return Localization.text("call.quality.tier.excellent")
        case .good: return Localization.text("call.quality.tier.good")
        case .fair: return Localization.text("call.quality.tier.fair")
        case .poor: return Localization.text("call.quality.tier.poor")
        case .bad: return Localization.text("call.quality.tier.bad")
        }
    }
}

private struct QualityDot: View {
    @ObservedObject var l10n = Localization.shared
    let tier: CallUiState.QualityTier
    let interactive: Bool
    let onTap: () -> Void

    var body: some View {
        Circle()
            .fill(tier.colour)
            .frame(width: 8, height: 8)
            .contentShape(Rectangle().inset(by: -8))
            .onTapGesture {
                if interactive { onTap() }
            }
            .accessibilityLabel(l10n.t("call.quality.a11y", texts: ["tier": tier.word]))
    }
}

// MARK: - EncryptionChip

/// `docs/ux/calls.md` sections 4 and 5 — a lock and one word, next to the
/// status line. It replaces the strip of sixteen digits that used to run across
/// the top of every call: unreadable, meaningless to anyone who has not been
/// told what it is, and occupying the most valuable line on the screen. The
/// digits now live one tap away, in the encryption sheet, where there is room to
/// say what they are for.
///
/// It appears only when the call's own encryption is armed. There is no
/// "Securing..." state: an unarmed call has nothing true to claim, and a 1x1
/// native call refuses to connect at all rather than run unencrypted
/// (invariant #4).
private struct EncryptionChip: View {
    @ObservedObject var l10n = Localization.shared
    let onTap: () -> Void

    var body: some View {
        Button(action: onTap) {
            HStack(spacing: 5) {
                Image(systemName: "lock.fill")
                    .font(.caption2)
                Text(l10n.t("call.encryption.chip"))
                    .font(.caption2)
                    .fontWeight(.semibold)
            }
            .foregroundStyle(.white)
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .background(Capsule().fill(Color.white.opacity(0.16)))
        }
        .buttonStyle(.plain)
        .accessibilityLabel(l10n.t("call.encryption.chip_a11y"))
    }
}

// MARK: - EncryptionSheet

/// `docs/ux/calls.md` section 5 — what the chip opens: the title, one plain
/// sentence naming the other person, the safety number in four groups of four,
/// and the one line that says what comparing them proves.
private struct EncryptionSheet: View {
    @ObservedObject var l10n = Localization.shared
    let peerName: String?
    let safetyNumber: String?
    let onDone: () -> Void

    @State private var copied = false

    var body: some View {
        VStack(spacing: 20) {
            VStack(spacing: 8) {
                Image(systemName: "lock.fill")
                    .font(.title2)
                    .foregroundColor(Color.lakkoSuccess)
                Text(l10n.t("call.encryption.title"))
                    .font(.title3)
                    .fontWeight(.semibold)
                    .foregroundColor(Color.lakkoText)
                Text(CallPeerName.encryptionSentence(peerName: peerName))
                    .font(.subheadline)
                    .foregroundColor(Color.lakkoTextMuted)
                    .multilineTextAlignment(.center)
            }

            if let safetyNumber {
                Text(safetyNumber)
                    .font(.system(.title2, design: .monospaced).weight(.medium))
                    .foregroundColor(Color.lakkoText)
                    .multilineTextAlignment(.center)
                    .padding(.vertical, 16)
                    .frame(maxWidth: .infinity)
                    .background(Color.lakkoSurfaceSunken)
                    .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card, style: .continuous))
                    // Section 8 — read in groups of four, not as one number.
                    .accessibilityLabel(l10n.t(
                        "call.encryption.number_a11y",
                        texts: ["groups": Localization.joinList(
                            safetyNumber.split(separator: " ").map(String.init))]))

                Text(l10n.t("call.encryption.compare_hint"))
                    .font(.footnote)
                    .foregroundColor(Color.lakkoTextMuted)
                    .multilineTextAlignment(.center)
            }

            Spacer(minLength: 0)

            HStack(spacing: 12) {
                if let safetyNumber {
                    Button {
                        UIPasteboard.general.string = safetyNumber
                        copied = true
                    } label: {
                        Text(l10n.t(copied ? "common.copied" : "common.copy"))
                            .font(.headline)
                            .foregroundColor(Color.lakkoText)
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 12)
                            .background(Color.lakkoSurfaceSunken)
                            .clipShape(
                                RoundedRectangle(cornerRadius: LakkoRadius.control, style: .continuous))
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel(l10n.t("call.encryption.copy_a11y"))
                }
                Button(action: onDone) {
                    Text(l10n.t("common.done"))
                        .font(.headline)
                        .foregroundColor(Color.lakkoAccentForeground)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 12)
                        .background(Color.lakkoAccent)
                        .clipShape(
                            RoundedRectangle(cornerRadius: LakkoRadius.control, style: .continuous))
                }
                .buttonStyle(.plain)
                .accessibilityLabel(l10n.t("common.close"))
            }
        }
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(Color.lakkoBackground.ignoresSafeArea())
        .presentationDetents([.medium])
    }
}

// MARK: - CallStatusLine

/// The one line the whole screen agrees on (`docs/ux/calls.md` section 4): the
/// name, then the state or the running time — never both, and never repeated in
/// a badge floating somewhere else. The old screen printed `Connecting...` in a
/// coloured pill at the top AND had a status line underneath it; this is the one
/// line, and ``CallStatusText`` decides its words.
private struct CallStatusLine: View {
    @ObservedObject var l10n = Localization.shared
    let name: String
    let phase: CallUiState.CallPhase
    let connectedAt: Date?
    let e2eeActive: Bool
    let notice: String?
    /// Large, centred type for the portrait; compact for the overlay on video.
    let prominent: Bool
    /// A group call, or a call joined from a link: nobody is being called, so
    /// its first seconds read `Connecting...` (section 3.4).
    var groupCall: Bool = false
    let onEncryptionTap: () -> Void

    var body: some View {
        VStack(spacing: prominent ? 8 : 2) {
            Text(name)
                .font(prominent ? .system(size: 34, weight: .semibold) : .headline)
                .foregroundStyle(.white)
                .lineLimit(1)
                .minimumScaleFactor(0.6)

            HStack(spacing: 8) {
                statusText
                if e2eeActive {
                    EncryptionChip(onTap: onEncryptionTap)
                }
            }

            if let notice {
                Text(notice)
                    .font(.footnote)
                    .foregroundStyle(Color.white.opacity(0.6))
                    .multilineTextAlignment(.center)
            }
        }
        .multilineTextAlignment(.center)
    }

    @ViewBuilder
    private var statusText: some View {
        if case .connected = phase, let connectedAt {
            // One tick a second while the call runs; the view is the only thing
            // that needs a clock, so it owns it.
            TimelineView(.periodic(from: connectedAt, by: 1)) { context in
                let elapsed = Int64(max(0, context.date.timeIntervalSince(connectedAt)) * 1000)
                line(
                    CallStatusText.line(phase: phase, durationMs: elapsed, groupCall: groupCall),
                    spoken: CallStatusText.spokenLine(
                        phase: phase, durationMs: elapsed, groupCall: groupCall))
            }
        } else {
            line(
                CallStatusText.line(phase: phase, durationMs: nil, groupCall: groupCall),
                spoken: CallStatusText.spokenLine(
                    phase: phase, durationMs: nil, groupCall: groupCall))
        }
    }

    @ViewBuilder
    private func line(_ text: String?, spoken: String?) -> some View {
        if let text {
            Text(text)
                .font(prominent ? .callout : .subheadline)
                .foregroundStyle(Color.white.opacity(0.7))
                .monospacedDigit()
                .accessibilityLabel(spoken ?? text)
        }
    }
}

// MARK: - QualityIndicator (dot + popover)

private struct QualityIndicator: View {
    @ObservedObject var state: CallUiState
    @ObservedObject var l10n = Localization.shared

    var body: some View {
        ZStack(alignment: .topTrailing) {
            QualityDot(
                tier: state.localQualityTier,
                interactive: true,
                onTap: { state.setQualityPopoverOpen(!state.qualityPopoverOpen) }
            )
            if state.qualityPopoverOpen {
                popover
                    .offset(x: 0, y: 16)
                    .zIndex(25)
            }
        }
    }

    private var popover: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(l10n.t("call.quality.title", texts: ["tier": state.localQualityTier.word]))
                    .fontWeight(.semibold)
                Spacer()
                Text(connectionTypeLabel)
                    .foregroundColor(connectionTypeColour)
            }
            Divider().background(Color.lakkoBorder)
            detailRow(
                l10n.t("call.quality.round_trip"),
                state.connectionDetail.rttMs.map {
                    l10n.t("call.quality.ms", texts: ["value": formatNumber(Double($0), decimals: 0)])
                } ?? "—")
            detailRow(l10n.t("call.quality.down"), formatBps(state.connectionDetail.downBps))
            detailRow(l10n.t("call.quality.up"), formatBps(state.connectionDetail.upBps))
        }
        .font(.caption)
        .padding(10)
        .foregroundColor(Color.lakkoText)
        .background(Color.lakkoSurface)
        .overlay(
            RoundedRectangle(cornerRadius: 8).stroke(Color.lakkoBorder, lineWidth: 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: 8))
        .frame(minWidth: 180, alignment: .leading)
    }

    private func detailRow(_ label: String, _ value: String) -> some View {
        HStack(spacing: 4) {
            Text(label).foregroundColor(Color.lakkoTextMuted)
            Text(value)
        }
    }

    private var connectionTypeLabel: String {
        switch state.connectionDetail.connectionType {
        case .direct: return l10n.t("call.quality.direct")
        case .relay: return l10n.t("call.quality.relay")
        case .unknown: return "—"
        }
    }

    private var connectionTypeColour: Color {
        switch state.connectionDetail.connectionType {
        case .direct: return Color.lakkoSuccess
        case .relay: return Color.lakkoWarning
        case .unknown: return Color.lakkoTextMuted
        }
    }

    /// A number in the interface language's own digits and separators; the
    /// unit is its own message (batch 5 convention).
    private func formatNumber(_ value: Double, decimals: Int) -> String {
        value.formatted(
            .number.precision(.fractionLength(decimals)).locale(l10n.locale))
    }

    private func formatBps(_ bps: Int?) -> String {
        guard let bps = bps else { return "—" }
        if bps >= 1_000_000 {
            return l10n.t("call.quality.mbps", texts: ["value": formatNumber(Double(bps) / 1_000_000, decimals: 1)])
        }
        if bps >= 1_000 {
            return l10n.t("call.quality.kbps", texts: ["value": formatNumber(Double(bps / 1_000), decimals: 0)])
        }
        return l10n.t("call.quality.bps", texts: ["value": formatNumber(Double(bps), decimals: 0)])
    }
}

// MARK: - ParticipantTile (group calls)

/// One person's tile in a group call (`docs/ux/group-call-grid.md` section
/// 7). A 1x1 call has no tiles at all: it is a portrait or a full-screen
/// video.
///
/// The tile is drawn at exactly the size it is given: the shared grid rule
/// decides the size, and a tile that kept a shape of its own is what made the
/// old grid scroll.
private struct ParticipantTile: View {
    @ObservedObject var l10n = Localization.shared
    let participant: CallUiState.Participant
    let size: CGSize
    @ObservedObject var state: CallUiState
    /// Pin or unpin, or `nil` for this device's own tile, which cannot be
    /// pinned (section 5.3).
    let onTap: (() -> Void)?

    private static let corner: CGFloat = 12
    /// Section 7.3: the ring is a border inside the tile edge, so the tile
    /// does not change size when someone starts to speak.
    private static let ringWidth: CGFloat = 3

    var isPinned: Bool { state.pinnedId == participant.id }

    /// This participant's media cannot be decrypted (a decryptor that failed to
    /// arm). The tile is blank for that reason and not for the ordinary one.
    var isUndecryptable: Bool { state.undecryptable.contains(participant.id) }

    var body: some View {
        ZStack {
            videoSurface
            if isUndecryptable {
                // Said at the top of the tile, not in the label row: the tile
                // is empty, and an empty tile with a name under it is exactly
                // what a peer with no bandwidth looks like.
                VStack {
                    HStack(spacing: 6) {
                        Image(systemName: "lock.slash.fill")
                            .font(.caption)
                        Text(UndecryptableMedia.tileLabel)
                            .font(.caption)
                            .lineLimit(1)
                    }
                    .foregroundColor(Color.lakkoDanger)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 6)
                    .background(Color.lakkoBackground.opacity(0.65))
                    .clipShape(Capsule())
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel(
                        UndecryptableMedia.spokenTileLabel(name: participant.displayName))
                    Spacer(minLength: 0)
                }
                .padding(.top, 8)
            }
            if participant.isLocal && state.videoPublishState == .starting {
                HStack(spacing: 8) {
                    ProgressView()
                        .progressViewStyle(.circular)
                        .tint(Color.lakkoText)
                    Text(l10n.t("call.self.starting_camera"))
                        .font(.caption)
                        .foregroundColor(Color.lakkoText)
                        .lineLimit(1)
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
                .background(Color.lakkoBackground.opacity(0.55))
                .clipShape(Capsule())
            }
            VStack(spacing: 0) {
                Spacer(minLength: 0)
                nameStrip
            }
        }
        .frame(width: size.width, height: size.height)
        .background(Color.lakkoSurface)
        .clipShape(RoundedRectangle(cornerRadius: Self.corner, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: Self.corner, style: .continuous)
                .strokeBorder(
                    participant.speaking ? Color.lakkoAccent : Color.lakkoBorder,
                    lineWidth: participant.speaking ? Self.ringWidth : 1))
        .contentShape(Rectangle())
        // A tap pins, a tap on the pinned tile unpins (section 6.2). A drag
        // that turns the page moves too far to be a tap, so a swipe is not
        // one.
        .onTapGesture { onTap?() }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(onTap == nil ? [] : .isButton)
    }

    /// The scrim along the bottom edge: mic-off glyph, name, connection, pin.
    /// Anything in its normal state is absent (section 7.2): the strip of a
    /// healthy, unmuted, unpinned person is only their name.
    private var nameStrip: some View {
        HStack(spacing: 6) {
            if !participant.micOn {
                Image(systemName: "mic.slash.fill")
                    .font(.caption)
                    .foregroundColor(Color.lakkoDanger)
                    .accessibilityLabel(l10n.t("call.participant.muted_a11y"))
            }
            Text(CallScreenPresentation.tileName(displayLabel, tileWidth: size.width))
                .lineLimit(1)
                .truncationMode(.tail)
                .frame(maxWidth: .infinity, alignment: .leading)
            // A connection is drawn only when it is fair or worse. A dot on
            // every healthy tile says nothing.
            if CallScreenPresentation.connectionIsWorthShowing(participant.quality) {
                QualityDot(tier: participant.quality, interactive: false, onTap: {})
            }
            if isPinned {
                Image(systemName: "pin.fill")
                    .font(.caption2)
                    .foregroundColor(Color.lakkoAccent)
                    .accessibilityLabel(l10n.t("call.participant.pinned"))
            }
        }
        .font(.caption)
        .foregroundColor(Color.lakkoText)
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .background(Color.lakkoBackground.opacity(0.45))
    }

    @ViewBuilder
    private var videoSurface: some View {
        if participant.camOn {
            if participant.isLocal, let track = state.localVideoTrack {
                LocalVideoPreviewView(
                    track: track,
                    mirrored: state.settings.mirrorLocal,
                    contentMode: .scaleAspectFill)
            } else if !participant.isLocal,
                let track = state.remoteVideoTracks[participant.id]
                    ?? (participant.id == state.soleRemoteParticipantId
                        ? state.remoteVideoTrack
                        : nil) {
                // Remote video is never mirrored: the viewer wants the other
                // party's true orientation.
                LocalVideoPreviewView(track: track, mirrored: false, contentMode: .scaleAspectFill)
            } else {
                Color.lakkoSurfaceSunken
            }
        } else {
            // No picture: the initials, scaled to the tile so a strip tile and
            // a full-screen tile both read, and lifted clear of the name strip.
            ZStack {
                Color.lakkoSurfaceSunken
                InitialsAvatar(
                    name: displayLabel, identity: participant.id,
                    size: CallScreenPresentation.tileInitialsDiameter(tileSize: size),
                    placeholderSymbol: "person.fill")
                    .padding(.bottom, min(24, size.height * 0.2))
            }
        }
    }

    /// `docs/ux/calls.md` section 4 — who this tile is. "Unknown" is never an
    /// answer (see ``CallPeerName``).
    private var displayLabel: String {
        CallPeerName.tileLabel(
            displayName: participant.displayName, isLocal: participant.isLocal)
    }
}

// MARK: - GroupCallArea (group calls)

/// The tiles of a group call (`docs/ux/group-call-grid.md` sections 3 to 6).
///
/// Two layouts and no scrolling. **Grid**: everyone the same size, filling the
/// space between the status line and the control bar; more people than a page
/// holds means pages. **Focus**: the person speaking, or the pinned one, large
/// in the middle, and everyone else in a row above.
///
/// The geometry and the choice of the stage are the shared rules
/// (``GroupCallLayout`` over `lakko-ui-rules`); this view measures its own
/// area and draws the answer. Tiles are keyed by participant id, so a tile is
/// moved when the grid changes shape and redrawn when its speaking ring
/// changes, never rebuilt: a rebuilt tile restarts its video.
private struct GroupCallArea: View {
    @ObservedObject var state: CallUiState
    @ObservedObject var l10n = Localization.shared
    let rules: GroupCallRules
    /// A finished drag that did not turn the page (the collapse swipe).
    let onOtherDrag: (CGSize) -> Void

    @State private var page = 0
    /// Section 4.4: no slide under reduced motion.
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    /// The room the page dots take under the grid.
    private static let dotsHeight: CGFloat = 20

    var body: some View {
        GeometryReader { geo in
            let people = state.participants
            let kind = GroupCallLayout.kind(
                choice: state.layoutChoice, pinnedId: state.pinnedId, people: people)
            Group {
                switch kind {
                case .grid: grid(people: people, area: geo.size)
                case .focus: focus(people: people, area: geo.size)
                }
            }
            .frame(width: geo.size.width, height: geo.size.height)
        }
    }

    // MARK: Grid

    @ViewBuilder
    private func grid(people: [CallUiState.Participant], area: CGSize) -> some View {
        // Whether the call spills over a page depends on the count and the
        // width only, so it is asked first and the dots' room taken out after.
        let whole = GroupCallLayout.gridLayout(
            rules, count: people.count, width: area.width, height: area.height)
        let paged = whole.pages > 1
        let gridArea = CGSize(
            width: area.width, height: paged ? max(0, area.height - Self.dotsHeight) : area.height)
        let layout = paged
            ? GroupCallLayout.gridLayout(
                rules, count: people.count, width: gridArea.width, height: gridArea.height)
            : whole
        let shown = GroupCallLayout.gridPage(people, perPage: Int(layout.perPage), page: page)
        let frames = GroupCallLayout.gridFrames(
            count: shown.tiles.count, grid: layout, area: gridArea)

        VStack(spacing: 0) {
            ZStack(alignment: .topLeading) {
                ForEach(Array(shown.tiles.enumerated()), id: \.element.id) { index, person in
                    if frames.indices.contains(index) {
                        tile(person, size: frames[index].size)
                            .position(x: frames[index].midX, y: frames[index].midY)
                    }
                }
            }
            .frame(width: gridArea.width, height: gridArea.height, alignment: .topLeading)
            .contentShape(Rectangle())
            // A horizontal swipe turns the page (section 4.5). Recognised
            // alongside the tiles' own tap, never instead of it.
            .simultaneousGesture(
                DragGesture(minimumDistance: 20).onEnded { value in
                    if let next = GroupCallLayout.pageAfterSwipe(
                        translation: value.translation, page: shown.page, pages: shown.pages) {
                        page = next
                    } else {
                        onOtherDrag(value.translation)
                    }
                })
            .accessibilityAction(named: Text(l10n.t("call.grid.next_a11y"))) {
                page = min(max(shown.pages - 1, 0), shown.page + 1)
            }
            .accessibilityAction(named: Text(l10n.t("call.grid.prev_a11y"))) {
                page = max(0, shown.page - 1)
            }
            if paged {
                pageDots(page: shown.page, pages: shown.pages)
                    .frame(height: Self.dotsHeight)
            }
        }
        .animation(reduceMotion ? nil : .easeOut(duration: 0.2), value: shown.tiles.map(\.id))
    }

    /// One dot per page under the grid, the current one filled. Each dot is
    /// also a control.
    private func pageDots(page: Int, pages: Int) -> some View {
        HStack(spacing: 8) {
            ForEach(0..<pages, id: \.self) { index in
                Button {
                    self.page = index
                } label: {
                    Circle()
                        .fill(Color.white.opacity(index == page ? 0.95 : 0.3))
                        .frame(width: 8, height: 8)
                        .frame(width: 16, height: Self.dotsHeight)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(l10n.t(
                    "call.grid.page_a11y",
                    texts: [
                        "page": (index + 1).formatted(.number.locale(l10n.locale)),
                        "total": pages.formatted(.number.locale(l10n.locale)),
                    ]))
                .accessibilityAddTraits(index == page ? .isSelected : [])
            }
        }
    }

    // MARK: Focus

    @ViewBuilder
    private func focus(people: [CallUiState.Participant], area: CGSize) -> some View {
        let stage = GroupCallLayout.stagePerson(
            rules, people: people, pinnedId: state.pinnedId, speech: state.speech)
        let strip = GroupCallLayout.stripPeople(people, stage: stage)
        let metrics = GroupCallLayout.stripMetrics(areaWidth: area.width)
        let stageSize = GroupCallLayout.stageSize(
            rules, areaWidth: area.width, areaHeight: area.height)

        VStack(spacing: metrics.gap) {
            // The strip is the one place scrolling is allowed: its tiles are
            // secondary, and the stage never moves when it scrolls.
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: metrics.gap) {
                    ForEach(strip) { person in
                        tile(
                            person,
                            size: CGSize(width: metrics.tileWidth, height: metrics.tileHeight))
                    }
                }
                .frame(minWidth: area.width)
            }
            .frame(width: area.width, height: metrics.tileHeight)
            if let stage {
                tile(stage, size: stageSize)
                    .id(stage.id)
            }
        }
        .frame(width: area.width, height: area.height)
        .contentShape(Rectangle())
        .simultaneousGesture(
            DragGesture(minimumDistance: 20).onEnded { value in
                onOtherDrag(value.translation)
            })
    }

    // MARK: A tile

    private func tile(_ person: CallUiState.Participant, size: CGSize) -> some View {
        ParticipantTile(
            participant: person,
            size: size,
            state: state,
            // Never this device's own tile (section 5.3).
            onTap: person.isLocal
                ? nil
                : { state.pin(state.pinnedId == person.id ? nil : person.id) })
    }
}

// MARK: - CallLayoutGlyph

/// The layout button's picture: four equal tiles for the grid, a row of small
/// tiles over one large one for Focus. Drawn here so the two read as a pair.
private struct CallLayoutGlyph: Shape {
    let kind: GroupLayoutKind

    func path(in rect: CGRect) -> Path {
        // Drawn on a 24 x 24 grid, the same numbers as the web icons.
        let unit = min(rect.width, rect.height) / 24
        let origin = CGPoint(
            x: rect.midX - 12 * unit, y: rect.midY - 12 * unit)
        func box(_ x: CGFloat, _ y: CGFloat, _ w: CGFloat, _ h: CGFloat, _ r: CGFloat) -> Path {
            Path(
                roundedRect: CGRect(
                    x: origin.x + x * unit, y: origin.y + y * unit,
                    width: w * unit, height: h * unit),
                cornerRadius: r * unit)
        }
        var path = Path()
        switch kind {
        case .grid:
            path.addPath(box(3.5, 4.5, 7.5, 6.5, 1.5))
            path.addPath(box(13, 4.5, 7.5, 6.5, 1.5))
            path.addPath(box(3.5, 13, 7.5, 6.5, 1.5))
            path.addPath(box(13, 13, 7.5, 6.5, 1.5))
        case .focus:
            path.addPath(box(3.5, 4.5, 4.5, 3.5, 1))
            path.addPath(box(9.75, 4.5, 4.5, 3.5, 1))
            path.addPath(box(16, 4.5, 4.5, 3.5, 1))
            path.addPath(box(3.5, 10, 17, 9.5, 1.5))
        }
        return path
    }
}

// MARK: - ParticipantList drawer (group calls)

private struct ParticipantListDrawer: View {
    @ObservedObject var state: CallUiState
    @ObservedObject var l10n = Localization.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text(l10n.t(
                    "call.participants.title",
                    texts: ["count": state.participants.count.formatted(.number.locale(l10n.locale))]))
                    .font(.headline)
                    .foregroundColor(Color.lakkoText)
                Spacer()
                Button(action: { state.setParticipantListOpen(false) }) {
                    Image(systemName: "xmark")
                        .foregroundColor(Color.lakkoTextMuted)
                }
                .accessibilityLabel(l10n.t("call.participants.close_a11y"))
            }
            .padding(16)

            Divider().background(Color.lakkoBorder)

            ScrollView {
                VStack(spacing: 0) {
                    ForEach(state.participants) { p in
                        HStack(spacing: 10) {
                            if CallScreenPresentation.connectionIsWorthShowing(p.quality) {
                                QualityDot(tier: p.quality, interactive: false, onTap: {})
                            }
                            Text(CallPeerName.tileLabel(
                                displayName: p.displayName, isLocal: p.isLocal))
                                .foregroundColor(Color.lakkoText)
                                .frame(maxWidth: .infinity, alignment: .leading)
                            if !p.micOn {
                                Image(systemName: "mic.slash.fill")
                                    .foregroundColor(Color.lakkoDanger)
                                    .accessibilityLabel(l10n.t("call.participant.muted_a11y"))
                            }
                        }
                        .padding(.vertical, 10)
                        .padding(.horizontal, 16)
                        Divider().background(Color.lakkoBorder)
                    }
                }
            }
        }
        .frame(width: 320)
        .frame(maxHeight: .infinity)
        .background(Color.lakkoSurface)
    }
}

// MARK: - CallEndedOverlay

private struct CallEndedOverlay: View {
    @ObservedObject var l10n = Localization.shared
    /// How long media flowed, or `nil` for a call that never connected — which
    /// is the difference between a call and an attempt, and is why the duration
    /// is not simply printed as `0:00`.
    let durationMs: Int64?
    /// Why the call closed, when somebody else closed it: the starter of a
    /// group call ended it for everyone (section 4.1.2). `nil` otherwise.
    var reason: String? = nil
    let onDismiss: () -> Void

    var body: some View {
        ZStack {
            Color.black.opacity(0.75).ignoresSafeArea()
            VStack(spacing: 6) {
                Text(l10n.t("call.status.ended"))
                    .font(.title2)
                    .fontWeight(.semibold)
                    .foregroundStyle(.white)
                if let reason {
                    Text(reason)
                        .font(.callout)
                        .foregroundStyle(Color.white.opacity(0.85))
                        .multilineTextAlignment(.center)
                        .padding(.horizontal, 32)
                }
                if let durationMs {
                    Text(CallTranscriptRow.durationLabel(ms: durationMs))
                        .font(.callout)
                        .foregroundStyle(Color.white.opacity(0.7))
                        .monospacedDigit()
                        .accessibilityLabel(CallTranscriptRow.spokenDuration(ms: durationMs))
                }
            }
        }
        .task {
            // Section 4's state table: the screen closes itself after 2 s.
            try? await Task.sleep(nanoseconds: 2_000_000_000)
            onDismiss()
        }
    }
}

// MARK: - CallScreen top-level

public struct CallScreen: View {
    @StateObject private var state = CallUiState()
    /// Re-renders the screen's words on a language switch (ADR-0061).
    @ObservedObject private var l10n = Localization.shared
    /// One sound controller per CallScreen lifetime; deallocated with the view.
    @StateObject private var sounds = CallSoundsObservable()
    /// Which sound this call makes and when: the outgoing tone by the shared
    /// rule, and the two short cues on the change only (``CallSoundCues``).
    @State private var cues: CallSoundCues
    /// The shared rules a group call's screen asks (`lakko-ui-rules`).
    private let rules: GroupCallRules = SharedGroupCallRules()
    private let session: CallSession?
    @State private var sessionBindings: [AnyCancellable] = []
    /// `true` when the screen renders an externally-owned `CallUiState` (the
    /// SFU group-call path, where `SfuCallUi` drives the state).
    private let externallyDriven: Bool
    let onExit: () -> Void
    /// A camera start this screen refused because camera access is denied.
    @State private var permissionMessage: UiText?
    /// When media started flowing, for the status line's running time. Seeded
    /// from the call's own moment (`docs/ux/calls.md` section 4.5 — collapsing
    /// leaves the timer untouched), so a call brought back from the bar carries
    /// on counting instead of restarting at `0:00`.
    @State private var connectedAt: Date?
    /// How long media flowed, captured as the call ends so the "Call ended"
    /// screen can say it (section 4's state table). `nil` for a call that never
    /// connected.
    @State private var endedDurationMs: Int64?
    /// The other person's name, resolved by the call's owner.
    private let peerName: String?
    /// One plain line about this call's media (today: the no-camera downgrade).
    private let notice: UiText?
    /// The link to THIS call, for `Invite` (`docs/ux/call-links.md` section 3).
    /// `nil` on every call that cannot offer one: a 1x1 call, a group call in a
    /// chat, and a link call this device joined rather than founded.
    private let invite: CallInviteOffer?
    /// End the call for every person in it (`docs/ux/calls.md` section 4.1.2).
    /// Given only to the person who started a group or link call; while anyone
    /// else is in the call, `End` then opens a choice between leaving and
    /// this. `nil` on every other call, a 1x1 call included.
    private let onEndForEveryone: (() -> Void)?
    /// The line under `Call ended` when the call's starter ended it for
    /// everyone, or `nil`.
    private let endedLine: String?
    /// The choice behind `End` is open.
    @State private var endMenuOpen = false

    /// Section 4 — the controls fade out on a connected video call and come
    /// back on a tap; on a voice call they never hide.
    @State private var controlsVisible = true
    @State private var hideTask: Task<Void, Never>?
    /// The call's settings sheet is open (`docs/ux/calls.md` section 4.1).
    @State private var settingsOpen = false
    /// Where the user dragged the self-view to.
    @State private var selfViewOffset: CGSize = .zero
    @GestureState private var selfViewDrag: CGSize = .zero

    /// `docs/ux/calls.md` section 4.5 — put the call away without ending it, or
    /// `nil` on a surface that has nothing to put it away into (the design
    /// preview). The chevron and the downward swipe are hidden when it is `nil`
    /// rather than dead.
    private let onCollapse: (() -> Void)?
    /// A collapse is in flight, so ``onDisappear`` must not tear the call's state
    /// down. Without this the screen going away would run its ordinary
    /// dismissal — and on the group / link path `state` is the HOST's object, so
    /// `state.reset()` would wipe the live call's phase, roster, encryption and
    /// tracks on the way out of a collapse that changed nothing about the call.
    @State private var collapsing = false

    /// `true` when the camera controls drive a real call, so turning the camera
    /// on must settle the camera permission first.
    private var gatesCamera: Bool { session != nil || externallyDriven }

    public init(
        session: CallSession? = nil,
        peerName: String? = nil,
        notice: UiText? = nil,
        connectedAt: Date? = nil,
        onCollapse: (() -> Void)? = nil,
        onExit: @escaping () -> Void
    ) {
        self.session = session
        self.externallyDriven = false
        self.peerName = peerName
        self.notice = notice
        self.invite = nil
        self.onEndForEveryone = nil
        self.endedLine = nil
        self.onCollapse = onCollapse
        _connectedAt = State(initialValue: connectedAt)
        _cues = State(initialValue: CallSoundCues(groupCall: false))
        self.onExit = onExit
    }

    /// SFU group-call init: render a `CallUiState` that an external owner
    /// (`SfuCallUi`) already drives. No session — the call is already
    /// connecting when presented.
    public init(
        externalState: CallUiState,
        invite: CallInviteOffer? = nil,
        connectedAt: Date? = nil,
        endedLine: String? = nil,
        onEndForEveryone: (() -> Void)? = nil,
        onCollapse: (() -> Void)? = nil,
        onExit: @escaping () -> Void
    ) {
        _state = StateObject(wrappedValue: externalState)
        self.session = nil
        self.externallyDriven = true
        self.peerName = nil
        self.notice = nil
        self.invite = invite
        self.onEndForEveryone = onEndForEveryone
        self.endedLine = endedLine
        self.onCollapse = onCollapse
        _connectedAt = State(initialValue: connectedAt)
        // A group call, or a call joined from a link: it rings nobody.
        _cues = State(initialValue: CallSoundCues(groupCall: true))
        self.onExit = onExit
    }

    // MARK: - What the screen is showing

    /// Every call is dialled before this screen is presented, so the session's
    /// brief `.preparing` is never something to draw: it reads as
    /// `Calling...`, which is what is happening.
    private var displayPhase: CallUiState.CallPhase {
        if case .preparing = state.phase { return .calling }
        return state.phase
    }

    private var remoteParticipants: [CallUiState.Participant] {
        state.participants.filter { !$0.isLocal }
    }

    private var soleRemoteVideoTrack: RTCVideoTrack? {
        guard let sole = state.soleRemoteParticipantId else { return nil }
        return state.remoteVideoTracks[sole] ?? state.remoteVideoTrack
    }

    /// There is a picture only when a frame has actually been rendered. The
    /// track handle arrives with the SDP and proves nothing — see
    /// ``CallScreenPresentation/hasRemotePicture(trackPresent:framesArrived:)``.
    private var hasRemotePicture: Bool {
        CallScreenPresentation.hasRemotePicture(
            trackPresent: soleRemoteVideoTrack != nil,
            framesArrived: state.remoteVideoFlowing)
    }

    private var layout: CallScreenLayout {
        CallScreenPresentation.layout(
            // The SFU group call drives this screen from outside; the 1x1
            // `CallSession` path never does.
            groupCall: externallyDriven,
            remoteCount: remoteParticipants.count,
            remotePicture: hasRemotePicture)
    }

    /// The three numbers that decide the whole shape of the screen, printed on
    /// every change. A group call that could not show video for a structural
    /// reason looked, from the outside, exactly like a group call whose media
    /// never arrived; one line saying `remotes=1 track=true flowing=false` would
    /// have ended that in a second.
    private func logLayout() {
        NSLog(
            "CallScreen: layout=%@ remotes=%d track=%@ flowing=%@",
            "\(layout)", remoteParticipants.count,
            soleRemoteVideoTrack != nil ? "true" : "false",
            state.remoteVideoFlowing ? "true" : "false")
    }

    private var local: CallUiState.Participant? {
        state.participants.first(where: { $0.isLocal })
    }

    /// `docs/ux/calls.md` section 4.1 — the same four controls on both kinds of
    /// call, because the camera button is what turns a voice call into a video
    /// one. Nothing about the row depends on what the call is carrying right
    /// now. A group call has no Speaker, and adds `Participants` where it
    /// leads somewhere.
    private var controls: [CallControl] {
        CallScreenPresentation.controls(
            layout: layout, canInvite: invite != nil, hasOthers: !remoteParticipants.isEmpty)
    }

    /// Where the call is, as the shared rules read it. A 1x1 call reads its
    /// session, because the session tells a call ringing HERE from one this
    /// device placed and the screen's own phase does not.
    private var barPhase: CallBarPhase {
        if let session {
            return CallBarPresentation.phase(oneToOne: session.state)
        }
        return CallBarPresentation.phase(external: state.phase)
    }

    private var displayName: String {
        CallPeerName.callLabel(profileName: state.peerName, groupCall: externallyDriven)
    }

    /// The identity the ground and the avatar are tinted from. The peer's name
    /// when there is one, so a call and that person's chat row agree.
    private var tintIdentity: String {
        state.peerName ?? "lakko.call"
    }

    public var body: some View {
        ZStack {
            // docs/ux/calls.md section 3: the screen's first state IS the call,
            // for both kinds and both directions. A camera check stood here for
            // an outgoing video call until 2026-09-21.
            callView
        }
        .onChange(of: state.phase) { _, newPhase in
            // Section 4 — the running time starts when media does, and only
            // then ("Connected only when media flows").
            if case .connected = newPhase {
                if connectedAt == nil { connectedAt = Date() }
            } else {
                if let connectedAt {
                    endedDurationMs = Int64(max(0, Date().timeIntervalSince(connectedAt)) * 1000)
                }
                connectedAt = nil
            }
            // A call that is over has nothing left to choose about.
            if case .ended = newPhase { endMenuOpen = false }
            // Section 3.4: the outgoing tone asks the shared rule, and a group
            // call never rings. The two short cues play on the change only.
            cues.phaseChanged(barPhase, sounds: sounds.controller, rules: rules)
            restartAutoHide()
        }
        // A call becomes a video call when the first remote frame lands, not
        // when its phase changes — so the fade-out has to be armed from the
        // layout too, or the row of a connected video call would never hide.
        .onChange(of: layout) { _, _ in
            logLayout()
            restartAutoHide()
        }
        // Someone joined, someone left: one short cue each.
        .onChange(of: state.participants.map(\.id)) { _, _ in
            var connected = false
            if case .connected = state.phase { connected = true }
            cues.rosterChanged(
                remoteIds: remoteParticipants.map(\.id), connected: connected,
                sounds: sounds.controller)
        }
        .onAppear {
            if let session, sessionBindings.isEmpty {
                sessionBindings = state.driveFrom(session: session, peerName: peerName)
            }
            if let notice { state.setMediaNotice(notice) }
            // The tone follows the rule from the first moment; no cue is
            // played for a call that was already where it is.
            cues.begin(
                phase: barPhase, remoteIds: remoteParticipants.map(\.id),
                sounds: sounds.controller, rules: rules)
            logLayout()
            restartAutoHide()
        }
        // A name that resolves mid-call reaches the status line straight away.
        .onChange(of: peerName) { _, newValue in
            guard !externallyDriven else { return }
            state.setPeerName(newValue)
        }
        .onDisappear {
            hideTask?.cancel()
            // Stopped either way: a looping ringback outlives nothing, and the
            // sound controller goes away with this view.
            sounds.controller.stopAll()
            sessionBindings.removeAll()
            // A collapse is not a dismissal. `state` is the host's own object on
            // the group / link path, so resetting it here would end the live
            // call's UI on the way into the bar.
            if !collapsing { state.reset() }
        }
        .alert(
            l10n.t("call.permission.camera.title"),
            isPresented: Binding(
                get: { permissionMessage != nil },
                set: { if !$0 { permissionMessage = nil } }
            ),
            presenting: permissionMessage
        ) { _ in
            Button(l10n.t("common.ok"), role: .cancel) { permissionMessage = nil }
        } message: { message in
            Text(message.text)
        }
        // `Invite contacts`, when this call has a link to offer.
        .modifier(CallInvitePicker(offer: invite))
        // Calls are dark by design on every platform (UX spec section 4).
        .preferredColorScheme(.dark)
    }

    // MARK: - The call

    private var callView: some View {
        GeometryReader { geo in
            if layout == .gallery {
                groupCallView(size: geo.size)
            } else {
                callView(width: geo.size.width)
            }
        }
    }

    /// The bar's margin from each screen edge.
    private static let barMargin: CGFloat = 12

    /// The tap target under a 1x1 call's controls, and the downward swipe that
    /// puts any call away.
    ///
    /// It is a layer of its own rather than a gesture on the ZStack because
    /// the thing underneath is a `UIView`: `RTCMTLVideoView` fills the screen
    /// on a video call. A live call found the controls faded after five
    /// seconds and tapping the middle of the screen, which is the video,
    /// brought nothing back, leaving no way to mute or hang up. Sitting ABOVE
    /// the content and BELOW the self-view and the controls, this is hit
    /// first and still lets their own buttons through.
    private var tapLayer: some View {
        Color.clear
            .contentShape(Rectangle())
            .onTapGesture {
                if state.qualityPopoverOpen { state.setQualityPopoverOpen(false) }
                showControls()
            }
            // Section 4.5's other way back — a downward swipe on the screen,
            // the idiom iOS already teaches for putting something away.
            .simultaneousGesture(
                DragGesture(minimumDistance: 20)
                    .onEnded { value in collapseIfSwipedDown(value.translation) }
            )
    }

    private func collapseIfSwipedDown(_ translation: CGSize) {
        guard let onCollapse, CallBarPresentation.isCollapseSwipe(translation) else { return }
        collapsing = true
        onCollapse()
    }

    /// A portrait or a full-screen video: the 1x1 call.
    private func callView(width: CGFloat) -> some View {
        ZStack {
            CallBackdrop(identity: tintIdentity)

            switch layout {
            case .portrait:
                portrait
            case .video, .gallery:
                remoteVideo
            }

            tapLayer

            if local?.camOn == true, let track = state.localVideoTrack {
                selfView(track: track)
            }

            VStack(spacing: 0) {
                if layout == .portrait {
                    // A portrait keeps the collapse chevron and the quality dot
                    // and nothing else up there: the name and the state belong
                    // in the middle of the screen, with the avatar, not in a
                    // header bar.
                    HStack {
                        collapseChevron
                        Spacer()
                        QualityIndicator(state: state)
                    }
                    .padding(.horizontal, 20)
                    .padding(.top, 12)
                } else {
                    videoOverlayHeader
                }
                Spacer(minLength: 0)
                if controlsVisible {
                    controlsBar(width: width - 2 * Self.barMargin)
                        .padding(.bottom, layout == .portrait ? 56 : 24)
                        .transition(.opacity)
                }
            }

            if case .ended = displayPhase {
                CallEndedOverlay(durationMs: endedDurationMs, onDismiss: onExit).zIndex(30)
            }
        }
        .animation(.easeInOut(duration: 0.2), value: controlsVisible)
        .modifier(encryptionSheet)
    }

    /// The two sheets every call has: the safety number, only when asked for
    /// (section 5), and the call's settings (section 4.1).
    private var encryptionSheet: some ViewModifier {
        CallSheetsModifier(state: state, settingsOpen: $settingsOpen)
    }

    // MARK: - A group call

    /// A group call (`docs/ux/group-call-grid.md`): the status line, the tile
    /// area, the control bar, top to bottom with nothing overlapping. The
    /// tile area ends above the bar and starts below the status line, so no
    /// tile is ever drawn under either, and the bar never hides.
    private func groupCallView(size: CGSize) -> some View {
        let margin = GroupCallLayout.sideMargin(screenWidth: size.width)
        let fit = CallScreenPresentation.fitGroupControlBar(
            availableWidth: size.width - 2 * Self.barMargin, screenSize: size,
            count: controls.count)
        // The compact bar hugs the bottom edge; the roomy one floats above it.
        let barBottom: CGFloat = fit.showsLabels ? 24 : 12
        return ZStack {
            CallBackdrop(identity: tintIdentity)

            // Under the tiles here: a tap on a tile pins it, and this layer
            // only catches the taps and the downward swipe that land beside
            // them.
            tapLayer

            InviteScope(offer: invite) { offer in
                ZStack {
                    VStack(spacing: 0) {
                        groupHeader(inviteLine: offer?.statusLine)
                        GroupCallArea(
                            state: state, rules: rules, onOtherDrag: collapseIfSwipedDown
                        )
                        .padding(.horizontal, margin)
                        .padding(.vertical, 8)
                        groupControlsBar(fit: fit, offer: offer)
                            .padding(.bottom, barBottom)
                    }

                    // Participants: a menu of the app's own above the bar,
                    // over a scrim that closes it. Never the system's share
                    // sheet.
                    if let offer, offer.menuOpen {
                        Color.black.opacity(0.001)
                            .ignoresSafeArea()
                            .contentShape(Rectangle())
                            .onTapGesture { offer.closeMenu() }
                            .accessibilityHidden(true)
                            .zIndex(12)
                        VStack {
                            Spacer(minLength: 0)
                            CallParticipantsMenu(
                                offer: offer,
                                hasOthers: !remoteParticipants.isEmpty,
                                onParticipants: {
                                    offer.closeMenu()
                                    state.setParticipantListOpen(true)
                                })
                                .padding(.bottom, fit.barHeight + barBottom + 10)
                        }
                        .zIndex(13)
                    }
                }
            }

            // End, for the person who started the call: leave, or end it for
            // everyone (section 4.1.2). The same shape as the Participants
            // menu, over a scrim that closes it and does nothing else; the
            // first press never ends the call.
            if endMenuOpen, let onEndForEveryone {
                Color.black.opacity(0.001)
                    .ignoresSafeArea()
                    .contentShape(Rectangle())
                    .onTapGesture { endMenuOpen = false }
                    .accessibilityHidden(true)
                    .zIndex(14)
                VStack {
                    Spacer(minLength: 0)
                    CallEndMenu(
                        onLeave: {
                            endMenuOpen = false
                            endCall()
                        },
                        onEndForEveryone: {
                            endMenuOpen = false
                            onEndForEveryone()
                        })
                        .padding(.bottom, fit.barHeight + barBottom + 10)
                }
                .zIndex(15)
            }

            if state.participantListOpen {
                HStack {
                    Spacer()
                    ParticipantListDrawer(state: state)
                        .transition(.move(edge: .trailing))
                }
                .zIndex(20)
            }

            if case .ended = displayPhase {
                CallEndedOverlay(
                    durationMs: endedDurationMs, reason: endedLine, onDismiss: onExit
                ).zIndex(30)
            }
        }
        .animation(.easeInOut(duration: 0.2), value: state.participantListOpen)
        .modifier(encryptionSheet)
    }

    /// The status line of a group call, with the collapse chevron at the top
    /// left and its twin, the layout button, at the top right.
    private func groupHeader(inviteLine: String?) -> some View {
        HStack(alignment: .top) {
            collapseChevron
                .padding(.top, 4)
            Spacer(minLength: 8)
            CallStatusLine(
                name: displayName,
                phase: displayPhase,
                connectedAt: connectedAt,
                e2eeActive: state.e2ee.active,
                // One line under the status row: where the call's link went,
                // else what the call did on the person's behalf.
                notice: inviteLine ?? state.mediaNotice?.text,
                prominent: false,
                groupCall: true,
                onEncryptionTap: { state.setEncryptionSheetOpen(true) })
            Spacer(minLength: 8)
            layoutSwitch
                .padding(.top, 4)
        }
        .padding(.horizontal, 16)
        .padding(.top, 10)
        .padding(.bottom, 2)
    }

    /// Switch between the two group layouts, top right; the twin of the
    /// collapse chevron (`docs/ux/group-call-grid.md` section 3).
    ///
    /// It shows the layout a press leads to. While someone is pinned it
    /// unpins and returns to the grid. Absent while alone: there is nobody to
    /// put in a strip. A clear spacer of the same size keeps the status line
    /// centred either way.
    @ViewBuilder
    private var layoutSwitch: some View {
        let kind = GroupCallLayout.kind(
            choice: state.layoutChoice, pinnedId: state.pinnedId, people: state.participants)
        if remoteParticipants.isEmpty {
            Color.clear.frame(width: 44, height: 44)
        } else {
            Button {
                let next = GroupCallLayout.afterLayoutSwitch(from: kind)
                if next.clearsPin { state.pin(nil) }
                state.setLayoutChoice(next.choice)
            } label: {
                CallLayoutGlyph(kind: kind == .grid ? .focus : .grid)
                    .stroke(Color.white, style: StrokeStyle(lineWidth: 1.6, lineJoin: .round))
                    .frame(width: 22, height: 22)
                    .frame(width: 44, height: 44)
                    .background(Circle().fill(Color.white.opacity(0.12)))
                    .contentShape(Circle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(
                l10n.t(kind == .grid ? "call.layout.focus_a11y" : "call.layout.grid_a11y"))
        }
    }

    /// The group row: one line, sized by its own rule, with no menu behind
    /// it.
    private func groupControlsBar(fit: CallControlBarFit, offer: CallInviteOffer?) -> some View {
        CallControlsBar(explicitFit: fit) {
            ForEach(controls) { control in
                if control == .end { CallControlsEndSpacer() }
                controlButton(control, offer: offer)
            }
        }
    }

    /// A voice call: the avatar large and centred in the upper half, the name
    /// under it in the largest type on the screen, and one quiet line under that.
    private var portrait: some View {
        VStack(spacing: 24) {
            Spacer(minLength: 0)
            InitialsAvatar(
                name: state.peerName ?? "", identity: tintIdentity, size: 132,
                placeholderSymbol: "person.fill")
            CallStatusLine(
                name: displayName,
                phase: displayPhase,
                connectedAt: connectedAt,
                e2eeActive: state.e2ee.active,
                // A stream that cannot be decrypted outranks the camera
                // downgrade: one is why this call has no picture at all, the
                // other is a note about how it started.
                notice: state.undecryptableNotice ?? state.mediaNotice?.text,
                prominent: true,
                onEncryptionTap: { state.setEncryptionSheetOpen(true) })
            Spacer(minLength: 0)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 32)
    }

    /// A video call: the other person, full screen.
    @ViewBuilder
    private var remoteVideo: some View {
        if hasRemotePicture, let track = soleRemoteVideoTrack {
            LocalVideoPreviewView(track: track, mirrored: false, contentMode: .scaleAspectFill)
                .ignoresSafeArea()
                .accessibilityHidden(true)
        }
    }

    /// `docs/ux/calls.md` section 4.5 — put the call away without ending it. A
    /// chevron at the top left, which is where iOS puts "down, out of the way",
    /// and the one affordance the bar's tap is the counterpart of.
    ///
    /// Nothing is drawn when there is nowhere to collapse into, so the design
    /// preview does not offer a dead control.
    @ViewBuilder
    private var collapseChevron: some View {
        if let onCollapse {
            Button {
                collapsing = true
                onCollapse()
            } label: {
                Image(systemName: "chevron.down")
                    .font(.system(size: 17, weight: .semibold))
                    .foregroundStyle(.white)
                    .frame(width: 44, height: 44)
                    .background(Circle().fill(Color.white.opacity(0.12)))
                    .contentShape(Circle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(l10n.t("call.collapse_a11y"))
        }
    }

    /// Over video, the name and the state float at the top on a soft shade so
    /// they stay readable — the same one line, smaller.
    private var videoOverlayHeader: some View {
        HStack(alignment: .top) {
            collapseChevron
                .padding(.top, 4)
            CallStatusLine(
                name: displayName,
                phase: displayPhase,
                connectedAt: connectedAt,
                e2eeActive: state.e2ee.active,
                // The header carries nothing ordinarily; an undecryptable
                // stream is the exception, since the video behind it is blank
                // and nothing else on this layout would say why.
                notice: state.undecryptableNotice,
                prominent: false,
                onEncryptionTap: { state.setEncryptionSheetOpen(true) })
            Spacer(minLength: 12)
            QualityIndicator(state: state)
                .padding(.top, 6)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .background(
            LinearGradient(
                colors: [Color.black.opacity(0.55), .clear],
                startPoint: .top, endPoint: .bottom)
                .ignoresSafeArea(edges: .top))
    }

    /// The self-view: a small rounded rectangle about a quarter of the width,
    /// draggable, with the flip-camera button in its own corner — where the
    /// thing it changes is visible (`docs/ux/calls.md` section 4.1).
    private func selfView(track: RTCVideoTrack) -> some View {
        GeometryReader { geo in
            let width = geo.size.width * 0.26
            ZStack(alignment: .bottomTrailing) {
                LocalVideoPreviewView(
                    track: track,
                    mirrored: state.settings.mirrorLocal,
                    contentMode: .scaleAspectFill)
                if session?.canFlipCamera == true {
                    Button {
                        Task {
                            await session?.flipCamera()
                            // The flip is a choice of camera like the one in
                            // the settings sheet, so the sheet shows it.
                            LocalCameraCapture.preferredDeviceId =
                                LocalCameraCapture.active?.deviceId
                        }
                    } label: {
                        Image(systemName: "arrow.triangle.2.circlepath.camera")
                            .font(.system(size: 13, weight: .semibold))
                            .foregroundStyle(.white)
                            .frame(width: 30, height: 30)
                            .background(Circle().fill(Color.black.opacity(0.45)))
                    }
                    .buttonStyle(.plain)
                    .padding(6)
                    .accessibilityLabel(l10n.t("call.self.flip_a11y"))
                }
            }
            .frame(width: width, height: width * 4 / 3)
            .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .strokeBorder(Color.white.opacity(0.16), lineWidth: 1))
            .shadow(color: .black.opacity(0.35), radius: 10, y: 4)
            .offset(
                x: geo.size.width - width - 16 + selfViewOffset.width + selfViewDrag.width,
                y: 90 + selfViewOffset.height + selfViewDrag.height)
            .gesture(
                DragGesture()
                    .updating($selfViewDrag) { value, drag, _ in drag = value.translation }
                    .onEnded { value in
                        selfViewOffset.width += value.translation.width
                        selfViewOffset.height += value.translation.height
                    })
        }
        .accessibilityLabel(l10n.t("call.self.camera_a11y"))
    }

    // MARK: - Controls

    private func controlsBar(width: CGFloat) -> some View {
        CallControlsBar(availableWidth: width, count: controls.count) {
            ForEach(controls) { control in
                if control == .end { CallControlsEndSpacer() }
                controlButton(control, offer: nil)
            }
        }
    }

    @ViewBuilder
    private func controlButton(_ control: CallControl, offer: CallInviteOffer?) -> some View {
        let enabled = CallScreenPresentation.isEnabled(control, phase: displayPhase)
        switch control {
        case .microphone:
            let on = local?.micOn ?? true
            CallControlButton(
                control: .microphone,
                systemImage: on ? "mic.fill" : "mic.slash.fill",
                on: on,
                enabled: enabled,
                action: {
                    setMic(!on)
                    showControls()
                })
        case .camera:
            let on = local?.camOn ?? false
            CallControlButton(
                control: .camera,
                systemImage: on ? "video.fill" : "video.slash.fill",
                on: on,
                enabled: enabled,
                busy: state.videoPublishState == .starting,
                action: {
                    setCamera(!on)
                    showControls()
                })
        case .participants:
            // One control about the people of a group call. On a call that
            // can hand out a link it opens the app's own menu above the bar
            // (who is in the call, copy the link, invite contacts); on a call
            // placed inside a conversation it opens the list of people
            // directly.
            let press = CallScreenPresentation.participantsPress(
                canInvite: offer != nil, hasOthers: !remoteParticipants.isEmpty)
            CallControlButton(
                control: .participants,
                systemImage: "person.2.fill",
                on: state.participantListOpen,
                enabled: enabled,
                // While the call's link is being made the control shows that
                // and waits, rather than paying for a second link.
                busy: offer?.busy == true,
                spokenLabel: CallStatusText.spokenParticipantsLabel(
                    press: press, listOpen: state.participantListOpen),
                action: {
                    switch press {
                    case .menu: offer?.toggleMenu()
                    case .participants:
                        state.setParticipantListOpen(!state.participantListOpen)
                    case .nothing: break
                    }
                })
        case .settings:
            // Which microphone, camera and speaker the call uses. Live from
            // the first second: a person who cannot be heard wants it before
            // anyone answers.
            CallControlButton(
                control: .settings,
                systemImage: "gearshape.fill",
                on: false,
                enabled: enabled,
                action: {
                    settingsOpen = true
                    showControls()
                })
        case .end:
            CallControlButton(
                control: .end,
                systemImage: "phone.down.fill",
                on: false,
                enabled: true,
                action: pressEnd)
        }
    }

    /// `End`. For the person who started a group call, while anyone else is in
    /// it, the press opens the choice (or closes it again) and never ends the
    /// call; for everyone else, and alone, it ends at once.
    private func pressEnd() {
        if EndForEveryone.endOpensMenu(
            isStarter: onEndForEveryone != nil, othersInCall: remoteParticipants.count) {
            endMenuOpen.toggle()
            return
        }
        endCall()
    }

    private func endCall() {
        if let session {
            session.hangUp()
        } else {
            state.endCall()
        }
    }

    private func setMic(_ on: Bool) {
        if let session {
            session.setMicrophoneEnabled(on)
        } else {
            state.setMic(on)
        }
    }

    private func setCamera(_ on: Bool) {
        guard on, gatesCamera else {
            applyCamera(on)
            return
        }
        // Camera on: settle the permission first. A denial leaves the camera
        // off and explains why.
        Task { @MainActor in
            guard await CameraPermission.ensureGranted() else {
                permissionMessage = CameraPermission.deniedText
                return
            }
            applyCamera(true)
        }
    }

    /// Drive the camera choice onto whatever owns the call.
    private func applyCamera(_ on: Bool) {
        guard let session else {
            // Mock route, or an externally driven SFU call whose owner observes
            // `camOn` on the state and drives its session.
            state.setCamera(on)
            return
        }
        guard on else {
            session.setCameraEnabled(false)
            return
        }
        // Mid-call camera-on: `addLocalVideo` acquires + adds a video track +
        // renegotiates the first time, and re-enables an existing sender after.
        // On a capture failure revert the toggle and log the reason — the call
        // stays up audio-only (invariant #4).
        Task {
            do {
                try await session.addLocalVideo()
            } catch {
                state.setCamera(false)
                NSLog("CallScreen: addLocalVideo failed: %@", String(describing: error))
            }
        }
    }

    // MARK: - Auto-hide

    private func showControls() {
        controlsVisible = true
        restartAutoHide()
    }

    /// Section 4 — after five seconds of a connected video call the row fades
    /// out; a voice call's row never hides, because there is nothing underneath
    /// it to look at and hiding it only makes the user tap.
    private func restartAutoHide() {
        hideTask?.cancel()
        // Alone means nobody else's tile is on the screen, so there is nothing
        // underneath the row to reveal by hiding it - only a person left
        // without a way out.
        let alone = state.participants.filter { !$0.isLocal }.isEmpty
        guard CallScreenPresentation.autoHides(
            layout: layout, phase: displayPhase, alone: alone)
        else {
            controlsVisible = true
            return
        }
        hideTask = Task { @MainActor in
            try? await Task.sleep(
                nanoseconds: UInt64(CallScreenPresentation.autoHideAfter * 1_000_000_000))
            guard !Task.isCancelled else { return }
            controlsVisible = false
        }
    }
}

// MARK: - Observing the invite

/// Draws `content` with the call's invite offer observed, or with `nil` on a
/// call that has none.
///
/// The offer is optional and an `@ObservedObject` cannot be: the inner view is
/// what observes it, and this one is what decides whether there is anything
/// to observe.
private struct InviteScope<Content: View>: View {
    let offer: CallInviteOffer?
    @ViewBuilder let content: (CallInviteOffer?) -> Content

    var body: some View {
        if let offer {
            Observed(offer: offer, content: content)
        } else {
            content(nil)
        }
    }

    private struct Observed: View {
        @ObservedObject var offer: CallInviteOffer
        let content: (CallInviteOffer?) -> Content

        var body: some View { content(offer) }
    }
}

// MARK: - The call's two sheets

/// `docs/ux/calls.md` section 5 — the safety number, only when asked for; and
/// section 4.1 — the call's settings. One modifier, so the 1x1 screen and the
/// group one present the same two sheets.
private struct CallSheetsModifier: ViewModifier {
    @ObservedObject var state: CallUiState
    @Binding var settingsOpen: Bool

    func body(content: Content) -> some View {
        content
            .sheet(
                isPresented: Binding(
                    get: { state.encryptionSheetOpen },
                    set: { state.setEncryptionSheetOpen($0) })
            ) {
                EncryptionSheet(
                    peerName: state.peerName,
                    safetyNumber: state.e2ee.shortFingerprint,
                    onDone: { state.setEncryptionSheetOpen(false) })
            }
            .sheet(isPresented: $settingsOpen) {
                CallSettingsSheet(state: state, onDone: { settingsOpen = false })
            }
    }
}
