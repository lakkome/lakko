// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI
import UIKit

/// `docs/ux/message-actions.md` sections 2, 3 and 7 — what one message offers
/// and what the surface offering it looks like.
///
/// **Why this is a custom presentation and not `.contextMenu`.** Section 7 says
/// it outright: SwiftUI's own context menu offers no way to put a horizontal
/// reaction row above the lifted bubble, so the reactions ended up behind a
/// `React >` disclosure that opened a VERTICAL submenu — two taps, a
/// navigation and a column to read for the commonest thing a person does to a
/// message. The card also landed on top of the next message, so the person lost
/// track of which message they were acting on. Neither is fixable inside
/// `.contextMenu`; both are what this file replaces it with: a long press, a
/// receding backdrop, the bubble drawn at its own position, the reaction row
/// above it and the action card below it.
///
/// **What lives here and what does not.** Everything in this file is either a
/// pure rule (which actions, where the stack goes, how long a press must last)
/// or a view that draws one. It knows nothing about MLS, about the view model or
/// about a specific message: the caller hands it a ``MessageMenuRequest``
/// carrying the bubble's own view value and the three closures that answer it.
/// That is what lets the UI-test fixture mount the same production surface the
/// chat screen mounts.

// MARK: - The gesture

/// The press that opens the menu.
///
/// `client-redesign.md` settles the gesture and this does not reopen it: a long
/// press on iOS, never a hover, reachable from anywhere on the bubble including
/// over its own controls, and once the menu has opened the control underneath
/// receives nothing more from that press.
///
/// The threshold lives here, in ONE place, because two controls now answer to
/// it: the bubble opens the menu at it, and the controls inside the bubble
/// decline a press that outlives it (``VoiceSeekGate``). Two copies of the
/// number would drift, and the drift is silent in the worst direction — a seek
/// gate that fires at 0.5 s against a menu that opens at 0.4 s means one press
/// does both things.
public enum MessageLongPress {

    /// UIKit's own long-press threshold, which is what `.contextMenu` used
    /// before this file existed. Kept identical so the gesture a person has
    /// learned is unchanged.
    public static let thresholdSeconds: TimeInterval = 0.5
}

// MARK: - Which actions

/// One row of the action card.
///
/// Section 3 lists nine actions; eight of them exist today (Pin is one word
/// with two rows). Read by needs surface that is not built (section 6 is the
/// handover to the ADR), so it is deliberately absent rather than present and
/// dead: a row that cannot work is worse than no row. Select has its surface
/// since section 8 was built (``MessageSelection`` and the chat screen's
/// selection mode).
public enum MessageMenuAction: String, CaseIterable, Identifiable, Sendable {

    /// ADR-0052 decision 1 — arms the composer with a quote of this message
    /// (section 3.1). Any message with an id.
    case reply

    /// ADR-0052 decision 2 — opens the conversation picker; picking a row
    /// sends this message there (section 5.1). Any landed message that is not
    /// a tombstone: a row still sending or one that failed has nothing that
    /// can be forwarded yet.
    case forward

    /// ADR-0052 decision 3 — opens the pin sheet (section 4.1): `Pin for me`
    /// always, `Pin for everyone` when this device has the standing. Any
    /// landed message that is not a tombstone.
    case pin

    /// ADR-0052 decision 3 — the same row on a message already pinned in a
    /// scope this device can clear: acts at once in one scope, asks which in
    /// two.
    case unpin

    /// Puts the message text on the clipboard. Nothing leaves the device.
    case copy

    /// Section 8 — enters selection mode with this message already selected,
    /// from which Delete and Forward act on the set. Offered on the rows that
    /// are selectable at all: landed, not a tombstone, send complete (the
    /// Forward gate), while this device can still act.
    case select

    /// Own, not deleted (ADR-0023).
    case edit

    /// Own, not deleted (ADR-0023). Destructive.
    case delete

    public var id: String { rawValue }

    public var title: String {
        switch self {
        case .reply: return "Reply"
        case .forward: return "Forward"
        case .pin: return "Pin"
        case .unpin: return "Unpin"
        case .copy: return "Copy"
        case .select: return "Select"
        case .edit: return "Edit"
        case .delete: return "Delete"
        }
    }

    public var systemImage: String {
        switch self {
        case .reply: return "arrowshape.turn.up.left"
        case .forward: return "arrowshape.turn.up.right"
        case .pin: return "pin"
        case .unpin: return "pin.slash"
        case .copy: return "doc.on.doc"
        case .select: return "checkmark.circle"
        case .edit: return "pencil"
        case .delete: return "trash"
        }
    }

    /// Whether the platform's destructive colour applies, and whether the card
    /// separates this row from the ones above it (section 2, "the destructive
    /// one, separated").
    public var isDestructive: Bool { self == .delete }

    /// The actions this message offers, in section 2's order.
    ///
    /// **The order is the rule, not the list.** Section 2: the new-message
    /// actions come first (Reply, Forward), then the ones that only read
    /// (Copy, Select), then the ones that change this message (Pin, Edit), and
    /// the destructive one comes last and separated. Pin used to sit right
    /// after Forward on section 4.1's sentence; with Select in the card it
    /// takes section 2's slot, with Edit, so the two read-only rows stay
    /// together: Reply, Forward, Copy, Select, Pin, Edit, Delete.
    ///
    /// React is NOT in this list, which is the whole point of section 2: it is a
    /// row of its own above the bubble, not an item in this card.
    public static func offered(
        canReply: Bool, canForward: Bool, canEditOrDelete: Bool, hasText: Bool,
        pin: PinMenuState = .unavailable,
        canSelect: Bool = false
    ) -> [MessageMenuAction] {
        var actions: [MessageMenuAction] = []
        // Section 3.1 — any message with an id; a tombstone and a row that
        // has not landed have nothing to point at.
        if canReply { actions.append(.reply) }
        // Section 5.1 — any landed message that is not a tombstone; a row
        // still sending, or one whose send failed, cannot be forwarded yet.
        if canForward { actions.append(.forward) }
        // Section 3 — "any message with text". A message with only an
        // attachment has nothing to put on the clipboard, and a tombstone has
        // nothing left at all.
        if hasText { actions.append(.copy) }
        // Section 8 — on every row that is selectable in the mode it opens.
        if canSelect { actions.append(.select) }
        // Section 4.1 — `Pin` on any message whose send is complete and that
        // is not deleted; `Unpin` when it is pinned in a scope this device can
        // clear.
        switch pin {
        case .pin: actions.append(.pin)
        case .unpin: actions.append(.unpin)
        case .unavailable: break
        }
        if canEditOrDelete {
            actions.append(.edit)
            actions.append(.delete)
        }
        return actions
    }
}

/// Section 3 — Copy. "Puts the text on the clipboard. Nothing leaves the
/// device."
///
/// One function, so there is one place where a message's text reaches
/// `UIPasteboard` and one place a test has to look. It takes a string rather
/// than a message: what is copied is the text a person can see, and deciding
/// which text that is belongs to the row that drew it.
public enum MessageClipboard {

    /// Put `text` on the general pasteboard. A no-op for an empty string, so a
    /// mis-wired caller cannot silently clear what the person had copied
    /// earlier.
    public static func copy(_ text: String) {
        guard !text.isEmpty else { return }
        UIPasteboard.general.string = text
    }
}

// MARK: - Where the stack goes

/// Section 2 — "the message being acted on stays fully legible in place", and
/// the card sits under the bubble.
///
/// Both halves constrain the geometry, and they can conflict: a bubble near the
/// bottom of the screen has no room under it for a card, and one near the top
/// has no room above it for the reaction row. The rule below resolves that by
/// moving the whole stack — row, bubble and card together — the smallest
/// distance that brings it inside the visible area, so the bubble stays where it
/// was whenever it can and the relationship between the three surfaces never
/// changes.
///
/// Pure arithmetic on heights, which is why it is testable without a screen.
public enum MessageMenuLayout {

    /// Between the row and the bubble, and between the bubble and the card.
    public static let gap: CGFloat = 8

    /// The least the stack may come to the edge of the safe area.
    public static let margin: CGFloat = 12

    public struct Resolved: Equatable {

        /// Where the top of the whole stack goes, in the container's own space.
        public let stackTop: CGFloat

        /// How far the bubble moved from where it was drawn. `0` when the stack
        /// fitted, which is the common case and the one section 2 cares about.
        public let bubbleShift: CGFloat
    }

    /// Place the stack.
    ///
    /// A zero `rowHeight` or `cardHeight` means that surface is absent (a
    /// message that cannot be reacted to, or one with no actions) and its gap
    /// goes with it — so a menu with only a reaction row does not reserve space
    /// under the bubble for a card that is not there.
    ///
    /// When the stack is taller than the space available the TOP wins: the
    /// reaction row and the bubble stay on screen and the card runs off the
    /// bottom, because the row is the action being optimised for and the bubble
    /// is what tells the person which message they are acting on. Losing the
    /// bottom of the card is recoverable; losing the bubble is not.
    public static func resolve(
        bubbleTop: CGFloat,
        bubbleHeight: CGFloat,
        rowHeight: CGFloat,
        cardHeight: CGFloat,
        containerHeight: CGFloat,
        safeTop: CGFloat,
        safeBottom: CGFloat
    ) -> Resolved {
        let above = rowHeight > 0 ? rowHeight + gap : 0
        let below = cardHeight > 0 ? cardHeight + gap : 0
        let total = above + bubbleHeight + below

        let lowest = safeTop + margin
        let highest = containerHeight - safeBottom - margin - total

        let wanted = bubbleTop - above
        // `max(highest, lowest)` first, so a stack that does not fit at all
        // clamps to the top rather than to a negative ceiling.
        let top = max(lowest, min(wanted, max(highest, lowest)))
        return Resolved(stackTop: top, bubbleShift: (top + above) - bubbleTop)
    }
}

// MARK: - Where the bubble is

/// A message row's last known on-screen frame.
///
/// A reference type so that a row can record a frame that changes on every
/// scroll tick without invalidating its own body each time. It is held in
/// `@State` by the row, which keeps the same box across the row struct's
/// re-creations; `.zero` means nothing has been measured yet and the menu
/// declines to open rather than drawing itself at the window's corner.
final class BubbleFrameBox {
    var frame: CGRect = .zero
}

// MARK: - The request

/// The one message the menu is open on, and everything the overlay needs to
/// draw and answer it.
///
/// It carries the bubble's own view VALUE (`bubble`) rather than a description
/// of it. The overlay then draws the production bubble — the same view the row
/// drew, with the same text wrapping and the same media — instead of a
/// reconstruction that would be free to drift from it. It is captured without
/// the row's gesture and geometry modifiers, so the copy neither opens a second
/// menu nor reports its own frame back over the original's.
struct MessageMenuRequest: Identifiable {

    /// ``ChatMessage/id``, so opening the menu on another message replaces it.
    let id: Int64

    /// Where the bubble is on screen, in `.global` space.
    let bubbleFrame: CGRect

    /// Which side the bubble is on: the row and the card align to it (section
    /// 2, "aligned to the bubble's own side").
    let isOutgoing: Bool

    /// Section 3's actions, already narrowed to this message.
    let actions: [MessageMenuAction]

    /// Whether the reaction row is offered at all.
    let canReact: Bool

    /// The production bubble, drawn again at its own position.
    let bubble: AnyView

    let onReact: (String) -> Void
    let onMoreReactions: () -> Void
    let perform: (MessageMenuAction) -> Void
}

/// Holds the open menu, if any.
///
/// A reference type reached through the environment rather than a `Binding`
/// threaded down through the timeline: a message row is deep inside a
/// `LazyVStack` inside a `ScrollView`, and the alternative is one more closure
/// on every row for something only one row at a time ever uses.
@MainActor
final class MessageMenuPresenter: ObservableObject {

    @Published private(set) var request: MessageMenuRequest?

    var isOpen: Bool { request != nil }

    /// Open the menu on a message.
    ///
    /// A second open while one is up is ignored rather than allowed to replace
    /// it: the timeline is not interactive while the menu is on screen, so a
    /// second request can only come from a gesture that was already in flight.
    func open(_ request: MessageMenuRequest) {
        guard self.request == nil else { return }
        // The feedback `.contextMenu` used to give at the moment it opened. A
        // long press with no answer until the screen changes reads as a
        // hesitation.
        UIImpactFeedbackGenerator(style: .medium).impactOccurred()
        withAnimation(.easeOut(duration: 0.14)) { self.request = request }
    }

    func close() {
        withAnimation(.easeOut(duration: 0.12)) { request = nil }
    }
}

private struct MessageMenuPresenterKey: EnvironmentKey {
    static let defaultValue: MessageMenuPresenter? = nil
}

extension EnvironmentValues {

    /// The open message menu, or `nil` where no ``MessageMenuHost`` is above.
    ///
    /// Optional on purpose: a row rendered outside a host — a preview, a future
    /// screen that reuses it — then offers no menu rather than trapping at
    /// render time, which is what an `@EnvironmentObject` with no provider does.
    var messageMenuPresenter: MessageMenuPresenter? {
        get { self[MessageMenuPresenterKey.self] }
        set { self[MessageMenuPresenterKey.self] = newValue }
    }
}

// MARK: - The host

/// Wraps a conversation and presents the message menu over it.
///
/// Section 2 — "the rest of the screen recedes". The content and the menu are
/// SIBLINGS in one `ZStack` rather than content plus an `.overlay`, so the
/// `.disabled` that makes the conversation recede cannot reach down into the
/// menu and leave every one of its buttons dead.
///
/// `.disabled` is also the second half of `client-redesign.md`'s rule that the
/// control underneath receives nothing more from the press that opened the
/// menu: the press lands on a control whose whole subtree stopped being
/// interactive at the moment the menu appeared. `.contextMenu` used to get that
/// from UIKit, which cancels the touches in the views below it when it opens.
/// The first half of the rule stays where it was — with the controls, which
/// decline a press that outlives ``MessageLongPress/thresholdSeconds``.
struct MessageMenuHost<Content: View>: View {

    @StateObject private var presenter = MessageMenuPresenter()

    private let content: Content

    init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    var body: some View {
        ZStack {
            content
                .environment(\.messageMenuPresenter, presenter)
                .disabled(presenter.isOpen)
            if let request = presenter.request {
                MessageActionMenu(request: request) { presenter.close() }
                    .transition(.opacity)
            }
        }
    }
}

// MARK: - The menu

/// The whole presentation: the backdrop, the reaction row, the bubble at its
/// own position, and the action card.
struct MessageActionMenu: View {

    let request: MessageMenuRequest
    let dismiss: () -> Void

    /// Measured rather than assumed, because the layout rule needs both heights
    /// and neither is a constant: the row grows with the Dynamic Type size and
    /// the card grows with the number of actions.
    @State private var rowSize: CGSize = .zero
    @State private var cardSize: CGSize = .zero

    var body: some View {
        GeometryReader { proxy in
            // The request's frame is in `.global` space and this view may not
            // start at the window's origin (a navigation bar above it, a
            // sidebar beside it), so the bubble is converted into this view's
            // own space rather than assumed to share it.
            let origin = proxy.frame(in: .global).origin
            let bubble = CGRect(
                x: request.bubbleFrame.minX - origin.x,
                y: request.bubbleFrame.minY - origin.y,
                width: request.bubbleFrame.width,
                height: request.bubbleFrame.height
            )
            let layout = MessageMenuLayout.resolve(
                bubbleTop: bubble.minY,
                bubbleHeight: bubble.height,
                rowHeight: request.canReact ? rowSize.height : 0,
                cardHeight: request.actions.isEmpty ? 0 : cardSize.height,
                containerHeight: proxy.size.height,
                safeTop: proxy.safeAreaInsets.top,
                safeBottom: proxy.safeAreaInsets.bottom
            )
            ZStack(alignment: .topLeading) {
                backdrop
                stack(bubble: bubble, containerWidth: proxy.size.width)
                    .offset(y: layout.stackTop)
                    // The heights arrive one layout pass after the first, so
                    // until they do the rule is placing a stack whose ends it
                    // cannot see. Held invisible for that pass rather than drawn
                    // in the wrong place and corrected, which reads as a jump.
                    .opacity(measured ? 1 : 0)
            }
        }
        .ignoresSafeArea()
        // The menu is modal: while it is up, VoiceOver should offer it and not
        // the conversation behind it.
        .accessibilityAddTraits(.isModal)
    }

    /// Whether every surface the layout rule needs a height for has reported
    /// one.
    private var measured: Bool {
        (!request.canReact || rowSize.height > 0)
            && (request.actions.isEmpty || cardSize.height > 0)
    }

    /// Section 2 — the conversation behind the menu recedes. A tap on it
    /// dismisses, which is the gesture every platform's context menu answers
    /// and the one a person tries first.
    private var backdrop: some View {
        Rectangle()
            .fill(Color.black.opacity(0.32))
            .ignoresSafeArea()
            .contentShape(Rectangle())
            .onTapGesture { dismiss() }
            // Not a control: a full-screen button here would be the first thing
            // VoiceOver landed on and would read as an action.
            .accessibilityHidden(true)
    }

    /// The row, the bubble and the card as one column aligned to the bubble's
    /// side, placed so the bubble lands where it was drawn.
    ///
    /// One column rather than three placed elements, because the three must not
    /// drift apart: whatever the layout rule decides to do about a crowded
    /// screen, it does to all three at once.
    @ViewBuilder
    private func stack(bubble: CGRect, containerWidth: CGFloat) -> some View {
        let outgoing = request.isOutgoing
        VStack(alignment: outgoing ? .trailing : .leading, spacing: MessageMenuLayout.gap) {
            if request.canReact {
                MessageReactionRow(
                    onReact: { emoji in
                        dismiss()
                        request.onReact(emoji)
                    },
                    onMore: {
                        dismiss()
                        request.onMoreReactions()
                    }
                )
                .onGeometryChange(for: CGSize.self) { $0.size } action: { rowSize = $0 }
            }
            // Drawn at the width it had, so the text wraps exactly as it did in
            // the timeline; the height follows from that. Not interactive: the
            // bubble is what the person is looking AT, and its own controls
            // belong to the conversation that has receded.
            request.bubble
                .frame(width: bubble.width, alignment: outgoing ? .trailing : .leading)
                .allowsHitTesting(false)
            if !request.actions.isEmpty {
                MessageActionCard(actions: request.actions) { action in
                    dismiss()
                    request.perform(action)
                }
                .onGeometryChange(for: CGSize.self) { $0.size } action: { cardSize = $0 }
            }
        }
        // The bubble's own side: the column's near edge lands on the bubble's
        // near edge, so the row and the card hang off the same side as the
        // message.
        .padding(.trailing, outgoing ? max(0, containerWidth - bubble.maxX) : 0)
        .padding(.leading, outgoing ? 0 : max(0, bubble.minX))
        .frame(width: containerWidth, alignment: outgoing ? .trailing : .leading)
    }
}

// MARK: - The reaction row

/// Section 2 — "a row of emoji sits above the bubble and each one is one tap
/// away, with a trailing control that opens the full picker".
///
/// A `HStack`, and the shape is the requirement rather than a preference: the
/// defect this replaces put the same six emoji in a `Menu`, which SwiftUI draws
/// as a vertical list behind a `React >` disclosure. One tap, in a row, is what
/// section 2 exists to forbid the alternative of.
struct MessageReactionRow: View {

    /// ADR-0023's six. The trailing `+` reaches every other emoji through the
    /// picker (ADR-0047 section 4), so this is a shortcut list and not a limit.
    static let palette = ["👍", "❤️", "😂", "😮", "😢", "🙏"]

    /// Each emoji's tap target. Seven of these plus the padding is the row's
    /// width, and it has to stay inside a phone's screen: at 36 pt the row is
    /// about 288 pt wide, which fits the narrowest device Lakko supports with
    /// room for the bubble's own inset.
    static let target: CGFloat = 36

    let onReact: (String) -> Void
    let onMore: () -> Void

    var body: some View {
        HStack(spacing: 4) {
            ForEach(Self.palette, id: \.self) { emoji in
                Button {
                    onReact(emoji)
                } label: {
                    Text(emoji)
                        .font(.system(size: 22))
                        .frame(width: Self.target, height: Self.target)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("React with \(emoji)")
            }
            Button(action: onMore) {
                Image(systemName: "plus")
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(Color.lakkoTextMuted)
                    .frame(width: Self.target, height: Self.target)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("More reactions")
        }
        .padding(.horizontal, 6)
        .padding(.vertical, 3)
        .background(Color.lakkoSurface, in: Capsule())
        .overlay(Capsule().stroke(Color.lakkoBorder, lineWidth: 0.5))
        // Its own size, never the space it is offered.
        .fixedSize()
    }
}

// MARK: - The action card

/// Section 2 — "the action card is sized by its own content. It is a separate
/// surface from the reaction row and must not inherit its width."
///
/// `fixedSize(horizontal: true, vertical: false)` is what enforces that, and it
/// is the whole mechanism: with it the card is offered no width and resolves to
/// the width of its widest row, so putting it in a column with a 288 pt
/// reaction row leaves it at about a third of that. Without it the card
/// stretches to whatever the column is, which is the defect on web that section
/// 2 names — "the action rows are stretched to the width of the reaction row
/// above them, because both live in one card".
struct MessageActionCard: View {

    let actions: [MessageMenuAction]
    let perform: (MessageMenuAction) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(actions.enumerated()), id: \.element.id) { index, action in
                Button {
                    perform(action)
                } label: {
                    Label(action.title, systemImage: action.systemImage)
                        .font(.body)
                        .foregroundStyle(action.isDestructive ? Color.lakkoDanger : Color.lakkoText)
                        .padding(.horizontal, 14)
                        .padding(.vertical, 11)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                // Section 2 — the destructive one, separated. Drawn as an
                // OVERLAY on its own row rather than as a `Divider()` between
                // the two: a divider is a view that wants to fill the width it
                // is offered, and this card is offered no width at all
                // (`fixedSize` below), so a divider in the column would be
                // deciding the card's width instead of the actions. An overlay
                // never contributes to its parent's size.
                //
                // It belongs to the destructive row rather than sitting between
                // every pair, so adding Reply and Forward above does not produce
                // a card of hairlines.
                .overlay(alignment: .top) {
                    if action.isDestructive, index > 0 {
                        Rectangle()
                            .fill(Color.lakkoBorder)
                            .frame(height: 0.5)
                    }
                }
            }
        }
        .background(
            Color.lakkoSurface,
            in: RoundedRectangle(cornerRadius: LakkoRadius.card, style: .continuous)
        )
        .overlay(
            RoundedRectangle(cornerRadius: LakkoRadius.card, style: .continuous)
                .stroke(Color.lakkoBorder, lineWidth: 0.5)
        )
        .fixedSize(horizontal: true, vertical: false)
        .accessibilityIdentifier(MessageActionCard.accessibilityID)
    }

    /// Names the card in the accessibility tree. NOT how its width is
    /// measured: an accessibility frame is whatever the bridge reports,
    /// and for this container it reported the union of its rows - narrow
    /// - while the drawn card spanned the screen. The UI test reads
    /// pixels instead.
    static let accessibilityID = "message-action-card"
}
