// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

/// SwiftUI per-thread chat screen.
///
/// Mirror of:
///
///   * `apps/web/src/chat/ChatRoute.tsx` (1.17f4b)
///   * `apps/android/.../ui/chat/ChatScreen.kt` (1.15f4b)
///
/// Renders the messages list (outgoing right-aligned, incoming
/// left-aligned, system rows centred as a faded separator) + a
/// composer with a multi-line `TextField` and a Send button.
/// Long-poll loop runs while the screen is on-screen via SwiftUI's
/// `.task` modifier (the standard auto-cancel-on-disappear shape).
public struct ChatScreen: View {

    @ObservedObject public var viewModel: ChatViewModel
    /// The contacts the signed-in shell holds. Read only to answer one
    /// question — who is on the other side of a conversation between two
    /// people — so tapping the name opens THAT person
    /// (`docs/ux/conversation-info.md` section 1).
    @ObservedObject public var contacts: ContactsViewModel
    /// ADR-0061 — observed so a language switch re-renders the screen.
    @ObservedObject private var l10n = Localization.shared
    /// Leave this screen (pop it). Navigation back is the system back button
    /// and edge swipe; this is only for the case the chat was deleted from
    /// the members sheet, when there is nothing left to show.
    public var onBack: () -> Void
    /// Open the contact card for the person on the other side, pushed the way
    /// the Contacts list pushes it: one card (`docs/ux/conversation-info.md`
    /// section 3, "Except a person").
    public var onOpenContact: (Int64) -> Void
    /// ADR-0052 decision 2 — open another conversation, pushed the way the
    /// Chats list pushes it: after a forward, the conversation it went to;
    /// on a tapped attribution, the origin, with the message to scroll to.
    public var onOpenConversation: (_ groupIdHex: String, _ jumpTo: Int64?) -> Void

    /// ADR-0052 — the pinned / muted / archived flags and read cursors the
    /// "Forward to" picker's rows are built with, exactly as the Chats tab
    /// builds its own. Read only inside the picker sheet.
    @EnvironmentObject private var deviceGroupSync: DeviceGroupSyncRepository

    // 1.19c3 — picker state. Swift's `PhotosPicker` returns
    // `PhotosPickerItem`s asynchronously (we resolve them to
    // `Data` + MIME inside the `.onChange`); the file
    // importer is a one-shot URL handoff that we read in the
    // result callback. Both routes funnel into
    // ``ChatViewModel/onAttachmentPicked(bytes:mime:name:)``.
    @State private var photoPickerItems: [PhotosPickerItem] = []
    @State private var fileImporterPresented: Bool = false
    @State private var attachActionSheetPresented: Bool = false
    // Inline preview of an image attachment — set when a
    // bubble-chip "Open" tap resolves the bytes. Sheet
    // dismisses on `nil`.
    @State private var previewImageData: Data?

    /// ADR-0047 — the per-chat media runtime (recording, the media send queue,
    /// playback, decrypted-media temp files). A `@StateObject`, so it lives
    /// exactly as long as this screen and goes away — taking its temp files
    /// with it — when the chat closes.
    @StateObject private var media: ChatMediaController
    /// ADR-0047 §4 — the caret the emoji picker inserts at.
    @StateObject private var caret = ComposerCaret()
    /// ADR-0047 §4 — the open picker, if any.
    @State private var picker: PickerRequest?

    /// The composer field's focus. Owned here so the view model can ask for
    /// the keyboard (choosing Reply focuses the composer,
    /// `docs/ux/message-actions.md` section 3.1).
    @FocusState private var composerFocused: Bool

    /// ADR-0052 / section 3.1 "tap to jump" — the row a tapped quote pointed
    /// at, highlighted for about a second and a half, then `nil`.
    @State private var highlightedMessageId: Int64?

    /// A quote tap the timeline's `ScrollViewReader` has yet to act on. A
    /// counter rides along so two taps on the same quote both scroll.
    @State private var quoteJump: QuoteJump?

    private struct QuoteJump: Equatable {
        let messageId: Int64
        let nonce: Int
    }

    /// How long a jumped-to row stays highlighted (section 3.1: "about a
    /// second and a half").
    static let jumpHighlightSeconds: Double = 1.5

    /// `docs/ux/message-actions.md` section 8 — the selection, while the
    /// screen is in selection mode; `nil` otherwise. Screen state and not
    /// view-model state on purpose: section 8 says nothing about a selection
    /// is persisted and that navigating to another conversation leaves the
    /// mode, and a `@State` on the screen dies with the screen.
    @State private var selection: MessageSelection?

    /// Section 8 — the "Forward to" picker opened from the action bar (the
    /// single forward's picker is driven by the view model's
    /// `forwardingMessage`; the bulk one has no single message to hang on).
    @State private var bulkForwardOpen = false

    /// Section 8 — "Delete N messages?" is up.
    @State private var confirmBulkDelete = false

    /// Section 8 — what the action bar says after a run stopped short
    /// ("Deleted 5 of 12."). `nil` until a run fails; cleared on the next tap.
    @State private var bulkNotice: String?

    /// Section 8 — a run is in flight; the bar's buttons wait for it.
    @State private var bulkRunning = false

    /// Which picker is open: the composer's (Emoji + GIF), or the reaction
    /// bar's `+` for one message (Emoji only).
    private struct PickerRequest: Identifiable {
        let id = UUID()
        let mode: MediaPickerSheet.Mode
        let message: ChatMessage?
    }

    public init(
        viewModel: ChatViewModel,
        contacts: ContactsViewModel,
        onBack: @escaping () -> Void,
        onOpenContact: @escaping (Int64) -> Void = { _ in },
        onOpenConversation: @escaping (String, Int64?) -> Void = { _, _ in }
    ) {
        self.viewModel = viewModel
        self.contacts = contacts
        self.onBack = onBack
        self.onOpenContact = onOpenContact
        self.onOpenConversation = onOpenConversation
        _media = StateObject(wrappedValue: ChatMediaController(port: viewModel.mediaPort()))
    }

    /// `docs/ux/conversation-info.md` section 1 — which info the name opens,
    /// decided by what this conversation is. The rule is
    /// ``ConversationInfoDestination``; this only feeds it the two facts the
    /// client already holds.
    private var infoDestination: ConversationInfoDestination {
        ConversationInfoDestination.of(
            kind: viewModel.conversationKind,
            contactMessageId: viewModel.presentsAsGroup
                ? nil
                : contacts.contact(forOneToOneChat: viewModel.groupIdHex)?.request.messageId
        )
    }

    /// ADR-0047 §6 — the call owns the audio session and the camera.
    ///
    /// This used to be `activeCall != nil || anyCallInProgress`, which answers a
    /// different question: a ringing offer takes the call slot and takes no
    /// microphone, so the recorder was refused at exactly the moment a voice
    /// message was the only reply that fitted. ``CallCapture`` and the shared
    /// rule tell the two apart.
    private var callOwnsCapture: Bool {
        viewModel.callOwnsCapture
    }

    public var body: some View {
#if DEBUG
        let _ = BodyEvaluationCounter.tick("ChatScreen")
#endif
        // `docs/ux/message-actions.md` §2 — the per-message menu is a custom
        // presentation (§7: `.contextMenu` cannot put a reaction row above the
        // bubble), so the conversation is wrapped in the host that dims it and
        // draws the menu over it. Inside the `.environmentObject` calls below,
        // deliberately: the menu re-draws the production bubble, and a media
        // bubble reads the controller out of the environment.
        MessageMenuHost { chatBody }
            .overlay { VideoNoteOverlay(media: media) }
            .environmentObject(media)
            .environmentObject(media.live)
            .onAppear {
                media.updateTimeline(viewModel.messages)
                media.callActive = callOwnsCapture
            }
            .onChange(of: viewModel.messages) { _, messages in
                media.updateTimeline(messages)
            }
            .onChange(of: callOwnsCapture) { _, live in
                media.callActive = live
            }
            .onDisappear { media.chatHidden() }
            .sheet(item: $picker) { request in
                MediaPickerSheet(
                    mode: request.mode,
                    media: media,
                    onEmoji: { emoji in
                        if let message = request.message {
                            viewModel.reactFromPicker(to: message, emoji: emoji)
                            picker = nil
                        } else {
                            let end = ComposerCaret.insert(emoji, into: &viewModel.draft, at: caret.utf16Range)
                            caret.utf16Range = end..<end
                        }
                    },
                    onClose: { picker = nil }
                )
                .presentationDetents([.medium, .large])
            }
    }

    @ViewBuilder
    private var chatBody: some View {
        VStack(spacing: 0) {
            // ADR-0052 decision 3 / `docs/ux/message-actions.md` section 4 —
            // the pinned bar, under the header: one pin at a time, newest
            // first, shared before personal. A child of this VStack, so the
            // list starts lower while there is a pin and nothing is covered.
            // No pins, no bar; the header does not move.
            if let entry = viewModel.pinnedBarEntry {
                let entries = viewModel.pinnedEntries
                let lines = viewModel.pinnedLines(for: entry)
                PinnedBarView(
                    entry: entry,
                    index: PinnedBar.clamped(viewModel.pinBarIndex, count: entries.count),
                    count: entries.count,
                    author: lines.author,
                    excerpt: lines.excerpt,
                    onTap: {
                        // Section 4 — scroll to the shown pin with the reply
                        // jump's highlight, then advance (wrapping).
                        if let messageId = viewModel.tapPinnedBar() {
                            jumpToQuoted(messageId)
                        }
                    },
                    onList: { viewModel.pinListOpen = true }
                )
            }
            // `docs/ux/calls.md` section 2.1.1 — the ONE call surface this
            // screen owns: a call arriving in a MUTED conversation, offered
            // here rather than thrown across the screen. It is a child of this
            // VStack and not an overlay, so the message list below simply
            // starts lower while the offer stands and nothing already on screen
            // is covered. Bounded by the host's ring timer, not by anything
            // here.
            if let quietCall = viewModel.quietIncomingCall {
                QuietIncomingCallPanel(
                    call: quietCall,
                    onAccept: {
                        viewModel.acceptQuietIncomingCall(withVideo: quietCall.video)
                    },
                    onAcceptWithoutVideo: {
                        viewModel.acceptQuietIncomingCall(withVideo: false)
                    },
                    onDecline: { viewModel.declineQuietIncomingCall() }
                )
            }
            // No other call is presented here: every full-screen call (1x1,
            // group, call link) and the full-screen incoming call are presented
            // app-wide from the root view (`CallHostOverlay`), so a call rings
            // on any screen and survives leaving this chat. Returning to the
            // chat of a live group call shows that call, never a second one.
            // A live group call on this chat that this device is not in.
            // Liveness is time-based (a call nobody joined stops being
            // joinable after a minute, any call after four hours), so the
            // banner re-evaluates on a timer rather than only on a new
            // journal row.
            if !viewModel.membershipHasEnded {
                GroupCallBanner(
                    affordance: viewModel.groupCallAffordance(
                        nowMs: Int64(now.timeIntervalSince1970 * 1000)
                    ),
                    busy: viewModel.groupCallStarting,
                    // One call at a time: a live call anywhere withdraws Join.
                    joinAvailable: !viewModel.anyCallInProgress,
                    onJoin: { video in viewModel.joinOngoingGroupCall(video: video) }
                )
            }
            messagesList
            Divider()
            // Section 8 — the composer is REPLACED by the action bar while
            // selecting, the way the header is replaced by the selection bar.
            if let selection {
                selectionActionBar(selection)
            } else {
                composer
            }
        }
        .onGeometryChange(for: CGFloat.self) { proxy in
            proxy.size.width
        } action: { width in
            messagesWidth = width
        }
        .background(Color.lakkoBackground)
        .onReceive(Self.affordanceClock) { date in
            // The time input of the call affordances (banner + toolbar
            // labels): a call nobody joined stops being joinable after a
            // minute, any call after four hours. A plain timer feeding one
            // state value, on purpose: see `now`.
            now = date
        }
        .onAppear {
            // Phase 2a3e2b — the cross-device draft / read-cursor sync is
            // attached where the view model is made (`SessionRoot`), so this
            // screen observes nothing but its own view model.
            // ADR-0024 — mark the thread visible: arms read-receipt
            // publishing for the loaded timeline.
            viewModel.screenVisible = true
            // ADR-0042 — provision opaque push routing for this thread
            // (owner mints + broadcasts the `push_routing_secret`; everyone
            // subscribes under the derived `push_gid`, or defers). Mirror of
            // the web `Chat.tsx` onMount `provisionPushRouting`.
            viewModel.ensurePushRouting()
        }
        .onDisappear {
            // ADR-0024 — leaving the thread stops advertising typing.
            viewModel.screenVisible = false
        }
        .onChange(of: viewModel.composerFocusRequest) { _, _ in
            // ADR-0052 — choosing Reply puts the keyboard on the composer.
            composerFocused = true
        }
        // ADR-0052 decision 2 — a forward's attribution tapped elsewhere asked
        // this conversation to show the original: consumed once, on appear
        // (the screen was pushed for it) or on change (it was already up).
        .onAppear { consumePendingJump() }
        .onChange(of: viewModel.pendingJump) { _, _ in consumePendingJump() }
        // ADR-0052 decision 2 / section 5.1 — the "Forward to" picker, a
        // sheet over the chat. Picking sends at once and dismisses, then
        // opens that conversation; Cancel sends nothing.
        .sheet(
            isPresented: Binding(
                get: { viewModel.forwardingMessage != nil },
                set: { open in if !open { viewModel.cancelForward() } }
            )
        ) {
            if let message = viewModel.forwardingMessage {
                ForwardPickerScreen(
                    rows: viewModel.forwardTargets(
                        flags: deviceGroupSync.flags,
                        readCursors: deviceGroupSync.readCursors),
                    onPick: { row in
                        viewModel.forward(message, to: row.id)
                        onOpenConversation(row.id, nil)
                    },
                    onCancel: { viewModel.cancelForward() }
                )
            }
        }
        // Section 8 — the same picker, titled the same, one target, opened
        // from the selection's action bar. Picking sends one forward per
        // selected message, oldest first; Cancel returns to the selection as
        // it was.
        .sheet(isPresented: $bulkForwardOpen) {
            ForwardPickerScreen(
                rows: viewModel.forwardTargets(
                    flags: deviceGroupSync.flags,
                    readCursors: deviceGroupSync.readCursors),
                onPick: { row in
                    bulkForwardOpen = false
                    runBulkForward(to: row.id)
                },
                onCancel: { bulkForwardOpen = false }
            )
        }
        // Section 8 — a bulk delete is irreversible and multiplied, so it
        // confirms where the single delete asks nothing.
        .alert(
            MessageSelection.confirmDeleteLabel(selection?.count ?? 0),
            isPresented: $confirmBulkDelete
        ) {
            Button(l10n.t("common.delete"), role: .destructive) { runBulkDelete() }
            Button(l10n.t("common.cancel"), role: .cancel) {}
        }
        // ADR-0052 decision 3 / section 4.1 — the pin sheet: who is this pin
        // for. A row acts at once and dismisses; Cancel does nothing.
        .sheet(
            item: Binding(
                get: { viewModel.pinSheet },
                set: { request in if request == nil { viewModel.cancelPinSheet() } }
            )
        ) { request in
            PinScopeSheet(
                options: request.options,
                onChoose: { choice in viewModel.choosePin(choice, for: request.message) },
                onCancel: { viewModel.cancelPinSheet() }
            )
        }
        // Section 4.1 — every pin as rows; a row jumps to its message and
        // closes the list.
        .sheet(isPresented: $viewModel.pinListOpen) {
            PinnedListSheet(
                entries: viewModel.pinnedEntries,
                lines: { viewModel.pinnedLines(for: $0) },
                onPick: { entry in
                    viewModel.pinListOpen = false
                    Task { @MainActor in
                        try? await Task.sleep(for: .milliseconds(350))
                        jumpToQuoted(entry.messageId)
                    }
                },
                onClose: { viewModel.pinListOpen = false }
            )
        }
        .navigationTitle(headline)
        .navigationBarTitleDisplayMode(.inline)
        // Section 8 — while selecting, the header IS the selection bar: a
        // leading Cancel where Back was, the count in the middle, nothing
        // trailing. Back is hidden rather than left beside Cancel, because
        // section 8 says Cancel leaves the mode and Back would leave the
        // conversation.
        .navigationBarBackButtonHidden(selection != nil)
        .toolbar {
            if let selection {
                ToolbarItem(placement: .navigationBarLeading) {
                    Button(l10n.t("common.cancel")) { leaveSelection() }
                        .disabled(bulkRunning)
                        .accessibilityIdentifier("selection-cancel")
                }
                ToolbarItem(placement: .principal) {
                    Text(selection.countLabel)
                        .font(.headline)
                        .foregroundStyle(Color.lakkoText)
                        .accessibilityIdentifier("selection-count")
                }
            } else {
            // UX spec section 5.2: system back, avatar + name in the middle,
            // then call and video call. The overflow menu is gone with the
            // duplicate route it held (`docs/ux/conversation-info.md`
            // section 1).
            ToolbarItem(placement: .principal) {
                // `docs/ux/conversation-info.md` section 1 — the name is the
                // ONLY way into this conversation's info, and what opens
                // follows from what the conversation is.
                Button {
                    // A person opens the one contact card, pushed; a group
                    // opens its layer. A one-to-one whose record cannot be
                    // resolved (a roster that cannot be read) opens nothing:
                    // that is not a state with a screen.
                    switch infoDestination {
                    case .person(let contactMessageId?):
                        onOpenContact(contactMessageId)
                    case .group:
                        viewModel.openConversationInfo()
                    case .person(nil), .channel:
                        break
                    }
                } label: {
                    ChatTitleView(title: headline, subtitle: subtitle, identity: viewModel.groupIdHex,
                                  isGroup: viewModel.presentsAsGroup)
                }
                .buttonStyle(.plain)
                .accessibilityHint(infoDestination.accessibilityHint)
            }
            // Phase 3a9 — audio Call button. Hidden when a call
            // overlay is up (the overlay carries the
            // Accept / Decline / Hang up affordances).
            //
            // ADR-0045 UX §6.2.8 item 4 — and GONE once this device's
            // membership has ended. The per-call SFrame seed comes from
            // `exportSecret("lakko/call/v1", …)`, which is gated on
            // `is_active()` and now raises, so the button fails closed — which
            // is correct, and is not enough. A button that always errors is the
            // shape §6.1.1 rejects, and §6.2.11 forbids any live indicator on a
            // dead group for a stronger reason: a call control asserts a live
            // E2EE session, and there is not one.
            if !viewModel.membershipHasEnded {
            ToolbarItem(placement: .navigationBarTrailing) {
                // No `TimelineView` in a toolbar item, ever: one inside a bar
                // item host re-rendered on every layout pass and dirtied the
                // navigation stack's hosting view, which laid the bar items
                // out again -- a layout feedback loop that held the main
                // thread at full CPU on every chat with content (UIKit's
                // `-UIViewLayoutFeedbackLoopDebuggingThreshold` named the bar
                // item hosts). The join-or-start decision is taken at tap
                // time anyway; the label follows `now`.
                callButton(video: false)
            }
            // Video-call button — starts the call with the local
            // camera published (`config.video`). Group chats publish
            // SFrame-sealed video through the SFU; a 1×1 chat opens a
            // P2P video call. Withdrawn with its sibling above once the
            // membership has ended (§6.2.8 item 4).
            ToolbarItem(placement: .navigationBarTrailing) {
                callButton(video: true)
            }
            }
            // `docs/ux/conversation-info.md` section 1 — the overflow menu's
            // `Members` / `Group info` item is GONE. It was a second route to
            // the same screen, and the whole menu was that one item, so the
            // menu goes with it. Anything the menu later carries that is not
            // info belongs back here.
            }
        }
        .sheet(isPresented: $viewModel.conversationInfoOpen) {
            // Section 3 — a layer over the conversation, which stays where it
            // is behind it. On iOS that is a sheet, which is what this already
            // was.
            ConversationInfoSheet(
                destination: infoDestination,
                chat: viewModel,
                // ADR-0045 UX 6.2.4 - a deleted chat's row is gone from the
                // registry, so the screen it was opened from has nothing left
                // to show. Route back the same way the Back button does.
                onDeleted: onBack
            )
        }
        .task {
            // Continuous foreground polling. SwiftUI's `.task`
            // ties the lifecycle to the view: the loop starts on
            // appear + cancels automatically on disappear. Mirror
            // of the Compose `LaunchedEffect(Unit) { while
            // (isActive) }` pattern from Android 1.15e7.
            // ADR-0045 UX §6.2.8 item 1 — and stop entirely once this device's
            // membership has ended. The repository refuses the fetch, so
            // `pollOnceLongPoll` would return IMMEDIATELY instead of parking on
            // its ~30 s long poll, and this loop would spin. The condition is
            // checked before each iteration rather than only once, because the
            // eviction can land on a poll this loop itself drove.
            //
            // While this loop runs the chat is "open": the app-wide foreground
            // receive skips it (it polls itself) and its messages do not count
            // as unread. Opened and closed on the same model instance, captured
            // here, so the pair always balances even if the screen's model were
            // ever swapped while the task lives on.
            let openedModel = viewModel
            openedModel.chatScreenOpened()
            while !Task.isCancelled, viewModel.pollLoopShouldContinue {
                await viewModel.pollOnceLongPoll()
                await Task.yield()
            }
            openedModel.chatScreenClosed()
        }
        // 1.19c3 — Attach action sheet → Photo / Document.
        // Compactly hosts the two picker entry points without
        // crowding the composer toolbar with two buttons.
        .confirmationDialog(
            l10n.t("chat.composer.attach"),
            isPresented: $attachActionSheetPresented,
            titleVisibility: .visible
        ) {
            Button(l10n.t("chat.composer.photo_library")) {
                // Trigger the PhotosPicker by giving it a fresh
                // empty selection — the PhotosPicker `Button`
                // below opens immediately when bound to a
                // populated $photoPickerItems. SwiftUI's
                // `PhotosPicker` doesn't have a programmatic
                // show-trigger; we route through a transparent
                // overlay (`photoPickerTrigger` below).
                photoPickerOpenRequested = true
            }
            Button(l10n.t("chat.media.file")) {
                fileImporterPresented = true
            }
            Button(l10n.t("common.cancel"), role: .cancel) {}
        }
        // PhotosPicker — multi-select, resolved on `.onChange`.
        .photosPicker(
            isPresented: $photoPickerOpenRequested,
            selection: $photoPickerItems,
            maxSelectionCount: 10,
            // UX spec section 10 — photos and videos both become media bubbles.
            matching: .any(of: [.images, .videos])
        )
        .onChange(of: photoPickerItems) { _, newItems in
            // Resolve each picked photo on a Task → bytes +
            // MIME → hand to the view model. Clear the binding
            // after kicking off the work so the same photo can
            // be re-picked later if the user wants to attach it
            // again.
            let items = newItems
            photoPickerItems = []
            for item in items {
                Task { await sendPicked(item) }
            }
        }
        // File importer for arbitrary files.
        .fileImporter(
            isPresented: $fileImporterPresented,
            allowedContentTypes: [UTType.data],
            allowsMultipleSelection: true
        ) { result in
            switch result {
            case .success(let urls):
                for url in urls {
                    Task {
                        if let payload = readFileURL(url) {
                            await viewModel.onAttachmentPicked(
                                bytes: payload.bytes,
                                mime: payload.mime,
                                name: payload.name
                            )
                        }
                    }
                }
            case .failure:
                // Cancelled or denied — silent.
                break
            }
        }
        // Image preview sheet — populated by the bubble's
        // "Open" tap on a previewable attachment chip.
        .sheet(
            isPresented: Binding(
                get: { previewImageData != nil },
                set: { open in if !open { previewImageData = nil } }
            )
        ) {
            if let data = previewImageData,
               let image = UIImage(data: data) {
                NavigationStack {
                    Image(uiImage: image)
                        .resizable()
                        .scaledToFit()
                        .toolbar {
                            ToolbarItem(placement: .topBarTrailing) {
                                Button(l10n.t("common.done")) { previewImageData = nil }
                            }
                        }
                }
            }
        }
        // ADR-0023 — edit sheet. Presented when the context menu's
        // "Edit" sets `editingMessage`; pre-filled with the current
        // body. Save publishes the edit op; Cancel discards.
        .sheet(
            isPresented: Binding(
                get: { viewModel.editingMessage != nil },
                set: { open in if !open { viewModel.cancelEdit() } }
            )
        ) {
            EditMessageSheet(viewModel: viewModel)
        }
    }

    /// Workaround for `PhotosPicker` lacking a programmatic
    /// `isPresented` modifier — there's a sibling
    /// `.photosPicker(isPresented:…)` modifier that requires
    /// its own backing `@State`. The confirmation dialog flips
    /// this on, the modifier opens, the user picks, the items
    /// land in `photoPickerItems`, the `.onChange` fires, the
    /// modifier auto-closes on selection. iOS 17+.
    @State private var photoPickerOpenRequested: Bool = false

    /// ADR-0048 — a 1x1 chat is titled with the other person's resolved
    /// name (rename, profile name, verified username). A group is titled from
    /// its members ("Anna, Ivan, Maria +2"), never from the group id (UX spec
    /// section 5.1).
    private var headline: String {
        viewModel.conversationTitle
    }

    /// "@username" in a 1x1 chat, "N members" in a group, and "typing..."
    /// while a typing indicator is live (UX spec section 5.2).
    ///
    /// ADR-0024 — the indicator auto-hides when nobody is typing (the VM nils
    /// the text). ADR-0045 UX §6.2.11 — and never on a dead group: no new
    /// typing signal can arrive, so what would render is whatever was in
    /// flight at the moment of the eviction.
    private var subtitle: String? {
        if let typing = viewModel.typingIndicatorText, !viewModel.membershipHasEnded {
            return typing
        }
        return viewModel.conversationSubtitle
    }

    /// The rows the timeline draws: every message except the bookkeeping
    /// system rows, laid out into days and runs (UX spec section 5.2).
    private var timelineEntries: [ChatTimeline.Entry] {
        ChatTimeline.layout(
            messages: viewModel.messages.filter { !ChatTimeline.isHiddenSystemRow($0) },
            nowMs: ChatRepository.nowMs(),
            calendar: .current,
            locale: l10n.locale,
            parseMs: ChatRepository.parseDsTimestampMs)
    }

    /// The last row the timeline draws, for the scroll-to-bottom.
    private var lastDrawnMessageId: Int64? {
        viewModel.messages.last(where: { !ChatTimeline.isHiddenSystemRow($0) })?.messageId
    }

    /// See `messagesList`.
    @State private var messagesWidth: CGFloat = 0

    /// The time the call affordances are evaluated at. Driven by a timer
    /// into one state value rather than by `TimelineView`s: the banner's
    /// was harmless, the toolbar's caused the layout loop described at the
    /// call buttons, and one clock for both keeps them in step.
    @State private var now = Date()
    private static let affordanceClock = Timer.publish(every: 10, on: .main, in: .common).autoconnect()

    @ViewBuilder
    private var messagesList: some View {
        // The width the bubbles size against, read with `onGeometryChange` on
        // the screen's outer column (`chatBody`) so it is written only when it
        // actually changes, and not on the list, whose own width moves with
        // its rows. Until the first read arrives the rows size against a
        // conservative width.
        messagesColumn(width: messagesWidth > 0 ? messagesWidth : 360)
    }

    @ViewBuilder
    private func messagesColumn(width: CGFloat) -> some View {
        ScrollViewReader { scrollProxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    if viewModel.messages.isEmpty {
                        emptyHint
                    } else {
                        let entries = timelineEntries
                        ForEach(entries, id: \.message.messageId) { entry in
                            if let dateLabel = entry.dateLabel {
                                DateSeparator(label: dateLabel)
                            }
                            // Section 8 — while selecting, each row gets a
                            // mark at its leading edge and the whole row is
                            // the toggle; a row that cannot be acted on shows
                            // no mark and takes no tap.
                            let rowSelection = selectionRowState(for: entry.message)
                            SelectableMessageRow(
                                state: rowSelection,
                                accessibilityName: MessageSelection.accessibilityName(
                                    text: entry.message.deleted ? "" : entry.message.text,
                                    attachmentCount: entry.message.attachments.count),
                                onToggle: { toggleSelection(entry.message.messageId) }
                            ) {
                            MessageRow(
                                message: entry.message,
                                ownIdentity: viewModel.ownIdentity,
                                // ADR-0048 — who wrote an incoming group
                                // message, resolved from the identity stored
                                // at receipt (C-68). A 1x1 needs no label;
                                // a run shows it once.
                                authorName: viewModel.presentsAsGroup
                                    && entry.message.direction == .incoming
                                    ? entry.message.authorKeyB64.flatMap {
                                        viewModel.personName(signatureKeyB64: $0)
                                    }
                                    : nil,
                                showsSenderColumn: viewModel.presentsAsGroup
                                    && entry.message.direction == .incoming,
                                startsRun: entry.startsRun,
                                timeLabel: entry.timeLabel,
                                // UX spec section 5.2 — about 78 percent of
                                // the timeline width.
                                maxBubbleWidth: width * 0.78,
                                reactorNames: { emoji in
                                    entry.message.reactions
                                        .filter { $0.emoji == emoji }
                                        .compactMap { viewModel.personName(signatureKeyB64: $0.reactor) }
                                },
                                // ADR-0045 UX §6.2.5 — the same predicate the
                                // composer is gated on.
                                actionsAvailable: viewModel.messageActionsAvailable,
                                // ADR-0024 — "Sent" / "Read" glyph on our own
                                // messages, derived from the other members'
                                // read watermarks.
                                readState: viewModel.readState(of: entry.message),
                                onRetrySend: { viewModel.onRetrySend(localId: $0) },
                                onPreview: openAttachmentPreview,
                                onEdit: { viewModel.beginEdit(entry.message) },
                                onDelete: { viewModel.deleteMessage(entry.message) },
                                onReact: { emoji in
                                    viewModel.react(to: entry.message, emoji: emoji)
                                },
                                // ADR-0047 §4 — the reaction bar's `+`.
                                onMoreReactions: {
                                    picker = PickerRequest(mode: .reaction, message: entry.message)
                                },
                                // `docs/ux/calls.md` section 6 — tapping a
                                // call row calls back, the same way the
                                // header's phone button does.
                                onCallBack: { viewModel.onCallButton(video: false) },
                                // ADR-0052 — Reply arms the composer with a
                                // snapshot of this message.
                                onReply: { viewModel.beginReply(entry.message) },
                                // Section 3.1 "tap to jump" — the quote's
                                // target, by message id.
                                onQuoteTap: jumpToQuoted,
                                // ADR-0052 decision 2 — Forward opens the
                                // picker; the attribution line leads to the
                                // original when this device holds it.
                                onForward: { viewModel.beginForward(entry.message) },
                                onForwardTap: jumpToForwardOrigin,
                                // ADR-0052 decision 3 / section 4.1 — `Pin`
                                // or `Unpin` after Forward; the sheet decides
                                // who it is for.
                                pinState: viewModel.pinMenuState(for: entry.message),
                                onPin: { viewModel.beginPin(entry.message) },
                                // Section 8 — the menu's Select enters the
                                // mode with this message; while in it the
                                // menu does not open.
                                selecting: selection != nil,
                                onSelect: { enterSelection(with: entry.message.messageId) }
                            )
                            }
                            // Tighter inside a run, a clear gap between runs.
                            .padding(.top, entry.startsRun ? 10 : 2)
                            // Section 3.1 — the jumped-to row is tinted with
                            // the surface used for a pressed row, briefly;
                            // section 8 — a selected row keeps that tint.
                            .background(
                                highlightedMessageId == entry.message.messageId
                                    || rowSelection == .selected
                                    ? Color.lakkoSurfaceSelected : Color.clear
                            )
                            .id(entry.message.messageId)
                        }
                    }
                    // ADR-0047 — outgoing media still uploading (or failed):
                    // shown in the timeline immediately with a determinate
                    // ring, or `Not sent · Retry`. Nothing has reached the
                    // other side until the row is replaced by the sent message.
                    ForEach(media.pendingSends) { pending in
                        PendingMediaRow(pending: pending) {
                            media.retry(sendId: pending.id)
                        }
                        .padding(.top, 10)
                        .id("pending-\(pending.id)")
                    }
                    // ADR-0045 UX §6.2.5 — the terminal row.
                    //
                    // It goes in the TIMELINE, not in a banner: it is an
                    // outcome that happened at a definite point in the
                    // transcript. A row marks WHERE the history stops,
                    // survives scrollback and needs no dismissal state. It
                    // renders after the list because it is not a
                    // `ChatMessage` (nothing produced it on the wire) and
                    // the state is terminal. Carried by TEXT, never by hue
                    // or an icon, and never announced over what the reader
                    // is doing.
                    if let row = viewModel.terminalRowCopy {
                        VStack(spacing: 4) {
                            Text(row.headline)
                            Text(row.consequence)
                        }
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: .infinity, alignment: .center)
                        .padding(.top, 12)
                        .accessibilityElement(children: .combine)
                    }
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
            }
            .onAppear {
                // Open at the newest message. The view model is handed the
                // thread's messages before this view exists (`sync(state:)`
                // runs in its init), so the count below never changes on
                // open and a long chat used to appear at the top of its
                // history. Not `defaultScrollAnchor(.bottom)`: that also
                // bottom-aligns a chat shorter than the screen, which web
                // does not do. The second call runs after the lazy stack has
                // laid its rows out, when the first one may have had nothing
                // to measure yet.
                guard let last = lastDrawnMessageId else { return }
                scrollProxy.scrollTo(last, anchor: .bottom)
                Task { @MainActor in
                    scrollProxy.scrollTo(last, anchor: .bottom)
                }
            }
            .onChange(of: viewModel.messages.count) { _, _ in
                // Scroll to the latest message whenever the list grows.
                if let last = lastDrawnMessageId {
                    withAnimation {
                        scrollProxy.scrollTo(last, anchor: .bottom)
                    }
                }
            }
            .onChange(of: media.pendingSends.count) { old, new in
                guard new > old, let last = media.pendingSends.last else { return }
                withAnimation {
                    scrollProxy.scrollTo("pending-\(last.id)", anchor: .bottom)
                }
            }
            .onChange(of: quoteJump) { _, jump in
                // ADR-0052 / section 3.1 — scroll to the quoted row (the same
                // anchor ids the list uses to reach the bottom) and highlight
                // it for about a second and a half.
                guard let jump else { return }
                withAnimation {
                    scrollProxy.scrollTo(jump.messageId, anchor: .center)
                    highlightedMessageId = jump.messageId
                }
                Task { @MainActor in
                    try? await Task.sleep(for: .seconds(Self.jumpHighlightSeconds))
                    if highlightedMessageId == jump.messageId {
                        withAnimation { highlightedMessageId = nil }
                    }
                }
            }
        }
    }

    /// Section 3.1 "tap to jump" — act only when the quoted message is in the
    /// timeline this device holds; otherwise do nothing and say nothing ("a
    /// quote is not a promise that the original exists").
    private func jumpToQuoted(_ messageId: Int64) {
        guard viewModel.messages.contains(where: {
            $0.messageId == messageId && !ChatTimeline.isHiddenSystemRow($0)
        }) else { return }
        quoteJump = QuoteJump(messageId: messageId, nonce: (quoteJump?.nonce ?? 0) + 1)
    }

    /// ADR-0052 decision 2 / section 5.1 "tap to jump" — the attribution of a
    /// forwarded message was tapped. When the original is in THIS
    /// conversation it is the same scroll a quote makes; when it is in
    /// another one this device holds, that conversation is opened with the
    /// message to show; otherwise nothing happens and nothing is said.
    private func jumpToForwardOrigin(_ forward: ForwardRef) {
        guard let target = viewModel.forwardOriginJump(of: forward) else { return }
        if target.groupIdHex == viewModel.groupIdHex {
            jumpToQuoted(target.messageId)
        } else {
            onOpenConversation(target.groupIdHex, target.messageId)
        }
    }

    /// Take the jump another screen asked for, once the timeline has had a
    /// moment to lay out — a `scrollTo` issued in the same tick the list
    /// appears has nothing to scroll yet.
    private func consumePendingJump() {
        guard let messageId = viewModel.takePendingJump() else { return }
        Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(350))
            jumpToQuoted(messageId)
        }
    }

    @ViewBuilder
    private var emptyHint: some View {
        VStack(alignment: .leading, spacing: 8) {
            // ADR-0045 UX §6.2 — the empty-timeline copy invites a send, and
            // once the membership has ended that is an invitation the composer
            // below can no longer accept. The terminal row that follows carries
            // the whole of what there is to say.
            if viewModel.membershipHasEnded {
                EmptyView()
            } else if viewModel.threadKnown {
                Text(l10n.t("chat.timeline.empty"))
                    .font(.body)
                    .foregroundStyle(Color.lakkoTextMuted)
                    .frame(maxWidth: .infinity, alignment: .center)
                    .padding(.top, 40)
            } else {
                Text(l10n.t("common.loading"))
                    .font(.body)
                    .foregroundStyle(.secondary)
                Text(l10n.t("chat.timeline.setting_up"))
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
        }
    }

    /// VoiceOver label for a toolbar call button — says "Join" when the tap
    /// joins the live group call rather than starting a new one.
    @ViewBuilder
    private func callButton(video: Bool) -> some View {
        Button {
            // A live group call this device is not in is JOINED; else
            // a group chat (>2 members) starts an SFU call (multi-party
            // + SFrame) and a 1×1 chat stays P2P.
            viewModel.onCallButton(video: video)
        } label: {
            Image(systemName: video ? "video" : "phone")
        }
        .accessibilityLabel(callButtonLabel(video: video))
        .disabled(viewModel.chatCallAction == .unavailable)
    }

    private func callButtonLabel(video: Bool) -> String {
        if case .joinable = viewModel.groupCallAffordance() {
            return l10n.t(video ? "chat.call.join_video" : "chat.call.join")
        }
        return l10n.t(video ? "chat.call.video" : "chat.call.voice")
    }

    /// "Group call in progress" — shown while this chat has a live group call
    /// this device is not in. Join connects to that call id; nothing here can
    /// start a second call.
    private struct GroupCallBanner: View {
        @ObservedObject private var l10n = Localization.shared
        let affordance: GroupCallAffordance
        let busy: Bool
        let joinAvailable: Bool
        let onJoin: (Bool) -> Void

        var body: some View {
            if case .joinable(_, let joinedCount, _) = affordance {
                VStack(alignment: .leading, spacing: 8) {
                    HStack(spacing: 10) {
                        Image(systemName: "phone.connection.fill")
                            .foregroundStyle(.white)
                            .accessibilityHidden(true)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(l10n.t("chat.call.group_in_progress"))
                                .font(.headline)
                                .foregroundStyle(.white)
                            Text(Self.joinedText(joinedCount))
                                .font(.caption2)
                                .foregroundStyle(.white.opacity(0.85))
                        }
                        Spacer()
                        if busy {
                            ProgressView().tint(.white)
                        }
                    }
                    HStack(spacing: 8) {
                        Button(l10n.t("chat.call.join_short")) { onJoin(false) }
                            .buttonStyle(.borderedProminent)
                            .tint(.white)
                            .foregroundStyle(.green)
                        Button(l10n.t("chat.call.join_video")) { onJoin(true) }
                            .buttonStyle(.bordered)
                            .tint(.white)
                    }
                    .disabled(busy || !joinAvailable)
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Color.green)
                .accessibilityElement(children: .contain)
            }
        }

        static func joinedText(_ count: Int) -> String {
            switch count {
            case 0: return Localization.text("chat.call.group_starting")
            default:
                return Localization.text("chat.call.group_people", counts: ["count": UInt64(max(0, count))])
            }
        }
    }

    // MARK: - `docs/ux/message-actions.md` section 8 — selection mode

    /// Section 8's row gate, over the same facts the single actions gate on
    /// (own is not among them: a row is selectable whether or not it is own,
    /// Delete's own-only rule is read over the whole selection at the bar).
    private func isSelectable(_ message: ChatMessage) -> Bool {
        MessageSelection.canSelect(
            actionsAvailable: viewModel.messageActionsAvailable,
            isSystemRow: message.direction == .system,
            hasId: message.messageId > 0,
            deleted: message.deleted,
            sendComplete: message.ack == .sent)
    }

    /// What the row draws for `message`: nothing outside the mode, no mark for
    /// a row that cannot be acted on, else its mark.
    private func selectionRowState(for message: ChatMessage) -> SelectionRowState {
        guard let selection else { return .notSelecting }
        guard isSelectable(message) else { return .notSelectable }
        return selection.contains(message.messageId) ? .selected : .unselected
    }

    /// Section 8, "Entering" — with the message the card was opened on
    /// already selected. The menu is closed by the time this runs.
    private func enterSelection(with messageId: Int64) {
        bulkNotice = nil
        selection = MessageSelection(initial: messageId)
    }

    /// Section 8, "Leaving" — Cancel, a completed action; the selection is
    /// cleared with the mode.
    private func leaveSelection() {
        selection = nil
        bulkNotice = nil
        confirmBulkDelete = false
        bulkForwardOpen = false
    }

    private func toggleSelection(_ messageId: Int64) {
        guard selection != nil, !bulkRunning else { return }
        bulkNotice = nil
        selection?.toggle(messageId)
    }

    /// The selected messages in the timeline's order, oldest first, over the
    /// rows the timeline draws now (a message that stopped being selectable
    /// since it was tapped is dropped rather than acted on).
    private var selectedMessagesOldestFirst: [ChatMessage] {
        guard let selection else { return [] }
        let drawn = viewModel.messages.filter { !ChatTimeline.isHiddenSystemRow($0) && isSelectable($0) }
        let ordered = selection.selectedInOrder(of: drawn.map(\.messageId))
        let byId = Dictionary(drawn.map { ($0.messageId, $0) }, uniquingKeysWith: { first, _ in first })
        return ordered.compactMap { byId[$0] }
    }

    /// Section 8 — Delete's own-only gate, read once for the whole selection.
    private var selectionIsAllOwn: Bool {
        selectedMessagesOldestFirst.allSatisfy { $0.direction == .outgoing }
    }

    /// Section 8 — the bar that takes the composer's slot: `Forward` and
    /// `Delete`, each with its icon and label, and the helper line under it
    /// when Delete is disabled for someone else's message (or the progress
    /// copy after a run stopped short).
    @ViewBuilder
    private func selectionActionBar(_ selection: MessageSelection) -> some View {
        let allOwn = selectionIsAllOwn
        let helper = bulkNotice ?? selection.deleteHelper(allOwn: allOwn)
        VStack(spacing: 6) {
            HStack(spacing: 0) {
                Button {
                    bulkNotice = nil
                    bulkForwardOpen = true
                } label: {
                    Label(l10n.t("chat.message.forward"), systemImage: "arrowshape.turn.up.right")
                        .font(.body.weight(.medium))
                        .frame(maxWidth: .infinity, minHeight: 44)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .foregroundStyle(Color.lakkoAccent)
                .disabled(!selection.forwardEnabled || bulkRunning)
                .opacity(selection.forwardEnabled && !bulkRunning ? 1 : 0.4)
                .accessibilityIdentifier("selection-forward")
                Button {
                    bulkNotice = nil
                    confirmBulkDelete = true
                } label: {
                    Label(l10n.t("common.delete"), systemImage: "trash")
                        .font(.body.weight(.medium))
                        .frame(maxWidth: .infinity, minHeight: 44)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .foregroundStyle(Color.lakkoDanger)
                .disabled(!selection.deleteEnabled(allOwn: allOwn) || bulkRunning)
                .opacity(selection.deleteEnabled(allOwn: allOwn) && !bulkRunning ? 1 : 0.4)
                .accessibilityIdentifier("selection-delete")
            }
            if let helper {
                Text(helper)
                    .font(.footnote)
                    .foregroundStyle(bulkNotice == nil ? Color.lakkoTextMuted : Color.lakkoDanger)
                    .frame(maxWidth: .infinity, alignment: .center)
                    .accessibilityIdentifier("selection-helper")
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(Color.lakkoSurface)
        .accessibilityIdentifier("selection-action-bar")
    }

    /// Section 8 — on confirmation, one delete per message, oldest first,
    /// stopping at the first failure with the unsent ones still selected;
    /// full success leaves the mode.
    private func runBulkDelete() {
        let messages = selectedMessagesOldestFirst
        guard !messages.isEmpty, !bulkRunning else { return }
        bulkRunning = true
        Task { @MainActor in
            let outcome = await viewModel.bulkDelete(messages)
            bulkRunning = false
            finish(outcome, kind: .delete, onSuccess: {})
        }
    }

    /// Section 8 — one forward per message, oldest first; on full success
    /// leave the mode and go to the target, where the forwards are the newest
    /// bubbles in their original order; on a failure stay here with the unsent
    /// ones still selected.
    private func runBulkForward(to targetGroupIdHex: String) {
        let messages = selectedMessagesOldestFirst
        guard !messages.isEmpty, !bulkRunning else { return }
        bulkRunning = true
        Task { @MainActor in
            let outcome = await viewModel.bulkForward(messages, to: targetGroupIdHex)
            bulkRunning = false
            finish(outcome, kind: .forward, onSuccess: { onOpenConversation(targetGroupIdHex, nil) })
        }
    }

    private func finish(
        _ outcome: MessageSelection.BulkOutcome<ChatMessage>,
        kind: MessageSelection.BulkKind,
        onSuccess: () -> Void
    ) {
        if outcome.succeeded {
            leaveSelection()
            onSuccess()
        } else {
            selection = MessageSelection(remaining: outcome.remaining.map(\.messageId))
            bulkNotice = MessageSelection.progressLabel(
                kind: kind, done: outcome.done, total: outcome.total)
        }
    }

    /// ADR-0045 UX §6.2.5 — the composer is REPLACED, not disabled, once this
    /// device's membership has ended.
    ///
    /// §5.3 argues a pending row's `Remove` must stay visible-but-disabled,
    /// because hiding it would read as the permission having vanished when it
    /// is merely suspended. The permission here has vanished, permanently and
    /// by design, so the rule inverts: disable while the state is TRANSIENT,
    /// replace when it is TERMINAL. A disabled Send button on a group you will
    /// never be in again is a control promising a future it does not have.
    @ViewBuilder
    private var composer: some View {
        switch viewModel.composerSlot {
        case .composer:
            liveComposer
        case .withdrawn(let notice, let draft, let draftLabel):
            withdrawnComposer(notice: notice, draft: draft, draftLabel: draftLabel)
        }
    }

    /// The bar that occupies the composer's own slot so the layout does not
    /// jump. Static text inside that region rather than a live announcement, so
    /// a VoiceOver user reaching the place they type hears why they cannot,
    /// rather than discovering an absence.
    @ViewBuilder
    private func withdrawnComposer(
        notice: String,
        draft: String?,
        draftLabel: String?
    ) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            // The unsent draft, preserved because the commit can land
            // mid-sentence and silently discarding what the user typed is a
            // small data loss with no upside.
            //
            // `Text` + `.textSelection(.enabled)`, NEVER a disabled `TextField`:
            // this is the iOS expression of the cross-platform shape §6.2.5
            // specifies — SELECTABLE, non-editable text. A disabled SwiftUI
            // `TextField` is not selectable at all, so the user would see their
            // own words and be unable to copy them out, which is exactly why
            // the spec names a shape rather than a widget. The treatment
            // matches the invite-link row in the members sheet, which is
            // already selectable-not-editable for the same reason.
            if let draft, let draftLabel {
                VStack(alignment: .leading, spacing: 2) {
                    Text(draftLabel)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                    Text(draft)
                        .font(.footnote)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
            Text(notice)
                .font(.footnote)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(12)
        .background(Color(.systemBackground))
    }

    @ViewBuilder
    private var liveComposer: some View {
        VStack(spacing: 8) {
            if let err = viewModel.sendError {
                Text(err.text)
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoDanger)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            // 1.19c3 — pending-attachments tray. Renders only
            // when ≥1 file is staged or an upload is in
            // flight. Each chip shows `name · size` with a ×
            // to drop it; the chips don't preview locally
            // because the picked bytes were freed after encrypt
            // (re-decrypt would round-trip through storage,
            // overkill for a composer scratch row).
            if !viewModel.pendingAttachments.isEmpty || viewModel.uploading {
                pendingAttachmentsTray
            }
            // ADR-0047 §6 — the permission-denied card, above the composer.
            if let denied = media.denied {
                MediaDeniedCard(resource: denied) { media.dismissDenied() }
            }
            if let hint = media.hint {
                Text(hint)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .transition(.opacity)
            }
            // ADR-0052 / `docs/ux/message-actions.md` section 3.1 — the reply
            // strip: what the recipient will see, taken when Reply was chosen.
            if let armed = viewModel.replyingTo {
                ReplyStrip(reply: armed.snapshot) { viewModel.cancelReply() }
            }
            // UX spec section 5.2: attach, the growing field, the emoji / GIF
            // picker, then a trailing button that is the microphone with an
            // empty draft and the send arrow otherwise.
            HStack(alignment: .bottom, spacing: 4) {
                if showsRecordingBar {
                    // ADR-0047 §2 — the recording bar stands in for the text
                    // field (and its buttons) while a take is live.
                    RecordingBar(media: media)
                        .frame(maxWidth: .infinity, minHeight: 44)
                } else {
                    // Attach button — opens the Photo / File
                    // confirmationDialog wired at the body level.
                    Button {
                        attachActionSheetPresented = true
                    } label: {
                        if viewModel.uploading {
                            ProgressView().controlSize(.small)
                                .frame(width: 40, height: 40)
                        } else {
                            composerIcon("paperclip")
                        }
                    }
                    .buttonStyle(.plain)
                    .disabled(viewModel.uploading || viewModel.sending)
                    .accessibilityLabel(l10n.t("chat.composer.attach"))
                    ComposerTextField(
                        text: $viewModel.draft, caret: caret, disabled: viewModel.sending,
                        focus: $composerFocused)
                    // ADR-0047 §4 — the smiley opens the picker on Emoji.
                    Button {
                        picker = PickerRequest(mode: .compose, message: nil)
                    } label: {
                        composerIcon("face.smiling")
                    }
                    .buttonStyle(.plain)
                    .disabled(viewModel.sending)
                    .accessibilityLabel(l10n.t("chat.composer.emoji_gifs"))
                }
                composerTrailingButton
            }
        }
        .animation(.easeInOut(duration: 0.15), value: media.hint)
        .padding(.horizontal, 8)
        .padding(.vertical, 8)
        .background(Color.lakkoSurface)
    }

    /// A composer icon button's face: 40 pt square, muted.
    private func composerIcon(_ systemName: String) -> some View {
        Image(systemName: systemName)
            .font(.system(size: 22))
            .foregroundStyle(Color.lakkoTextMuted)
            .frame(width: 40, height: 40)
            .contentShape(Rectangle())
    }

    /// The trailing button's send face: a 40 pt accent circle.
    private func sendFace<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        ZStack {
            Circle().fill(Color.lakkoAccent)
            content()
                .foregroundStyle(Color.lakkoAccentForeground)
        }
        .frame(width: 40, height: 40)
        .padding(2)
    }

    /// ADR-0047 — the recording bar replaces the text field once a take is
    /// running, locked, finishing or in preview.
    private var showsRecordingBar: Bool {
        switch media.recState {
        // UX spec §3.1 — a press whose tap-or-hold verdict has not arrived
        // yet has started nothing, so the composer stays put: a tap must not
        // make the text field flicker out and back.
        case .idle, .pressing, .requesting: return false
        case .recording, .locked, .finalizing, .preview: return true
        }
    }

    /// ADR-0047 §1 — Send while the field or the tray has content, otherwise
    /// the hold-to-record button (mic or camera). While a take is locked the
    /// slot is Stop; in preview it is Send.
    @ViewBuilder
    private var composerTrailingButton: some View {
        switch media.recState {
        case .locked:
            Button { media.stopLocked() } label: {
                ZStack {
                    Circle().fill(Color.lakkoDanger)
                    Image(systemName: "stop.fill")
                        .foregroundStyle(Color.white)
                }
                .frame(width: 40, height: 40)
                .padding(2)
            }
            .buttonStyle(.plain)
            .accessibilityLabel(l10n.t("chat.record.stop"))
        case .finalizing:
            ProgressView()
                .frame(width: 44, height: 44)
        case .preview:
            Button { media.sendPreview() } label: {
                sendFace { Image(systemName: "paperplane.fill") }
            }
            .buttonStyle(.plain)
            .accessibilityLabel(l10n.t("chat.record.send"))
        // `.pressing` belongs here and nowhere else: SwiftUI drops a gesture
        // whose view leaves the hierarchy, so the button deciding tap from
        // hold has to be the same button throughout the press.
        case .idle, .pressing, .requesting, .recording:
            if composerHasContent, !media.recState.isCapturing {
                Button(action: viewModel.onSendClicked) {
                    sendFace {
                        if viewModel.sending {
                            ProgressView().controlSize(.small)
                                .tint(Color.lakkoAccentForeground)
                        } else {
                            Image(systemName: "paperplane.fill")
                        }
                    }
                }
                .buttonStyle(.plain)
                .disabled(viewModel.sending || viewModel.uploading)
                .accessibilityLabel(l10n.t("chat.composer.send"))
            } else {
                RecordButton(media: media)
            }
        }
    }

    private var composerHasContent: Bool {
        !viewModel.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            || !viewModel.pendingAttachments.isEmpty
            || viewModel.uploading
    }

    @ViewBuilder
    private var pendingAttachmentsTray: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(
                    Array(viewModel.pendingAttachments.enumerated()),
                    id: \.offset
                ) { idx, ref in
                    HStack(spacing: 6) {
                        if let thumb = ref.thumb.flatMap({
                            MediaSessionCache.shared.cachedData(MediaSessionCache.key(for: $0))
                        }), let image = UIImage(data: thumb) {
                            TrayThumbnail(image: image)
                        } else {
                            Image(systemName: pendingChipIcon(ref))
                                .foregroundStyle(.secondary)
                        }
                        VStack(alignment: .leading, spacing: 0) {
                            Text(ref.name ?? l10n.t("chat.attachment.unnamed"))
                                .font(.caption)
                                .lineLimit(1)
                            Text(AttachmentBridge.formatSize(ref.size))
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                        Button {
                            viewModel.onRemovePendingAttachment(at: idx)
                        } label: {
                            Image(systemName: "xmark.circle.fill")
                                .foregroundStyle(.secondary)
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel(l10n.t("chat.composer.remove_attachment"))
                    }
                    .padding(.horizontal, 8)
                    .padding(.vertical, 6)
                    .background(Color(.secondarySystemBackground))
                    .clipShape(RoundedRectangle(cornerRadius: 10))
                }
                if viewModel.uploading {
                    HStack(spacing: 6) {
                        if let data = viewModel.uploadingThumbnail, let image = UIImage(data: data) {
                            TrayThumbnail(image: image)
                        }
                        ProgressView().controlSize(.small)
                        // Polish 1.x — surface the active pick's
                        // file name + stage instead of a generic
                        // "Encrypting…" caption. Falls back to
                        // the generic copy when the stage hasn't
                        // landed yet (the brief race between
                        // setting `uploading = true` and the
                        // first `uploadStage` mutation).
                        Text(uploadCaption(viewModel.uploadStage))
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                    .padding(.horizontal, 8)
                    .padding(.vertical, 6)
                }
            }
            .padding(.horizontal, 2)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func pendingChipIcon(_ ref: AttachmentRef) -> String {
        switch InlineMedia.decision(for: ref).bubble {
        case .video: return "video"
        case .photo: return "photo"
        default: return AttachmentBridge.isInlinePreviewable(ref) ? "photo" : "doc"
        }
    }

    /// Map [``ChatViewModel/UploadStage``] to the caption shown
    /// in the composer tray's progress chip. `nil` collapses to
    /// a generic "Encrypting…" — matches the pre-polish copy so
    /// the brief race window doesn't show an empty caption.
    private func uploadCaption(_ stage: ChatViewModel.UploadStage?) -> String {
        switch stage {
        case .none:
            return l10n.t("chat.upload.working")
        case .encrypting(let fileName):
            return l10n.t("chat.upload.encrypting", texts: ["name": fileName])
        case .uploading(let fileName):
            return l10n.t("chat.upload.uploading", texts: ["name": fileName])
        }
    }

    /// UX spec section 10 — a picked library item becomes a photo or video
    /// bubble: the video is exported to MP4 and the photo converted to an
    /// inline format (HEIC to JPEG) off the main actor, each with its size and
    /// a preview. An image ImageIO cannot decode is sent as a plain file.
    private func sendPicked(_ item: PhotosPickerItem) async {
        let stamp = Self.pickStamp()
        let isVideo = item.supportedContentTypes.contains { $0.conforms(to: .movie) }
        if isVideo {
            guard let movie = try? await item.loadTransferable(type: PickedMovie.self) else {
                viewModel.reportAttachmentProblem(.key("chat.attachment.error.open_video"))
                return
            }
            let prepared = await PhotoVideoPrep.prepareVideo(movie.url, stamp: stamp)
            try? FileManager.default.removeItem(at: movie.url.deletingLastPathComponent())
            guard let prepared else {
                viewModel.reportAttachmentProblem(
                    .key(
                        "chat.attachment.error.prepare_video",
                        texts: ["limit": AttachmentBridge.formatSize(50_000_000)]))
                return
            }
            await viewModel.onMediaPrepared(prepared)
            return
        }
        guard let bytes = try? await item.loadTransferable(type: Data.self), !bytes.isEmpty else {
            viewModel.reportAttachmentProblem(.key("chat.attachment.error.open_photo"))
            return
        }
        let prepared = await Task.detached(priority: .userInitiated) {
            PhotoVideoPrep.preparePhoto(bytes, stamp: stamp)
        }.value
        if let prepared {
            await viewModel.onMediaPrepared(prepared)
        } else {
            let mime = PhotoVideoPrep.sniffInlineImageMime(bytes)
            await viewModel.onAttachmentPicked(bytes: bytes, mime: mime, name: "file-\(stamp)")
        }
    }

    private static func pickStamp() -> String {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyyMMdd-HHmmss"
        return f.string(from: Date()) + "-" + String(UUID().uuidString.prefix(4)).lowercased()
    }

    /// File-importer handoff — read the URL's bytes + sniff
    /// MIME from the security-scoped resource. The URL is
    /// granted to us with a coordinated access scope; we wrap
    /// the read in `startAccessingSecurityScopedResource()` so
    /// iCloud / file-provider paths work without an
    /// `NSFileCoordinator` ceremony.
    private func readFileURL(_ url: URL) -> AttachmentPayload? {
        let didStart = url.startAccessingSecurityScopedResource()
        defer { if didStart { url.stopAccessingSecurityScopedResource() } }
        guard let bytes = try? Data(contentsOf: url) else { return nil }
        // UTType from the path extension — iOS keeps the
        // canonical type registry mapping (e.g. `.txt` →
        // `public.plain-text`). `preferredMIMEType` then
        // returns `text/plain`.
        let utt = UTType(filenameExtension: url.pathExtension)
        let mime = utt?.preferredMIMEType
        let name = url.lastPathComponent
        return AttachmentPayload(bytes: bytes, mime: mime, name: name)
    }

    /// Trigger the download + decrypt flow for an incoming
    /// attachment chip's "Open" tap. Routes the bytes into
    /// `previewImageData` so the body-level `.sheet` modifier
    /// surfaces them inline.
    fileprivate func openAttachmentPreview(_ ref: AttachmentRef) {
        Task {
            do {
                let bytes = try await AttachmentBridge.downloadAndDecrypt(
                    ref: ref
                )
                previewImageData = bytes
            } catch {
                // Best-effort; the bubble has already shown the
                // chip, surface a console log + leave the UI
                // alone.
                print("preview download failed: \(error)")
            }
        }
    }
}

/// UX spec section 10 — the preview of a photo or video in the composer's
/// attachment tray.
private struct TrayThumbnail: View {
    let image: UIImage

    var body: some View {
        Image(uiImage: image)
            .resizable()
            .scaledToFill()
            .frame(width: 32, height: 32)
            .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
            .accessibilityHidden(true)
    }
}

/// One picked-and-resolved attachment, ready to hand to the
/// view model.
private struct AttachmentPayload {
    let bytes: Data
    let mime: String?
    let name: String?
}

// MARK: - Message row

/// A centred capsule opening a day: "Today", "Yesterday", "16 September".
private struct DateSeparator: View {
    let label: String

    var body: some View {
        Text(label)
            .font(.caption)
            .fontWeight(.medium)
            .foregroundStyle(Color.lakkoTextMuted)
            .padding(.horizontal, 10)
            .padding(.vertical, 4)
            .background(Color.lakkoSurfaceSunken)
            .clipShape(Capsule())
            .frame(maxWidth: .infinity, alignment: .center)
            .padding(.vertical, 8)
            .accessibilityAddTraits(.isHeader)
    }
}

/// One message in the timeline.
///
/// Internal rather than private so the DEBUG-only UI-test fixture
/// (`UiTestFixtures.swift`) can mount the shipping row instead of a copy of it.
/// Nothing else outside this file constructs one.
/// `docs/ux/message-actions.md` section 8 — a timeline row in selection mode:
/// the mark at the leading edge and the whole row as the tap target.
///
/// A wrapper around the production `MessageRow` rather than a branch inside it,
/// for the same reason the menu host wraps the conversation: the overlay that
/// takes the row's taps and long presses sits ABOVE the bubble's own controls,
/// so a play disc or a photo under the finger receives nothing while selecting
/// and the menu's long press never fires. Outside the mode it draws the row
/// unchanged, with no leading inset.
/// What a timeline row draws for the selection mode.
enum SelectionRowState: Equatable {
    /// Not in selection mode: the row as it always was.
    case notSelecting
    /// In the mode, but this row cannot be acted on: no mark, no tap. The
    /// leading inset is kept so the bubbles stay in one column.
    case notSelectable
    case unselected
    case selected

    /// The mark's column: the glyph plus its breathing room.
    static let markColumnWidth: CGFloat = 40
}

struct SelectableMessageRow<Row: View>: View {

    typealias State = SelectionRowState

    let state: State
    /// Section 8 — the toggle is named by the message's text or kind.
    let accessibilityName: String
    let onToggle: () -> Void
    @ViewBuilder let row: () -> Row

    var body: some View {
        switch state {
        case .notSelecting:
            row()
        case .notSelectable:
            HStack(spacing: 0) {
                Color.clear.frame(width: SelectionRowState.markColumnWidth)
                row()
            }
            // Not acted on, not tapped: the row's own controls keep working,
            // but nothing here opens a menu (the row is told it is selecting).
        case .unselected, .selected:
            let selected = state == .selected
            HStack(alignment: .center, spacing: 0) {
                Image(systemName: selected ? "checkmark.circle.fill" : "circle")
                    .font(.system(size: 22))
                    .foregroundStyle(selected ? Color.lakkoAccent : Color.lakkoTextMuted)
                    .frame(width: SelectionRowState.markColumnWidth)
                    .accessibilityHidden(true)
                row()
            }
            .contentShape(Rectangle())
            // The overlay is what makes the WHOLE row the target and what
            // keeps the press from reaching the bubble's controls or the
            // menu: a hit-testable layer above the row, answering a tap and a
            // long press the same way (section 8, "a long press ... toggles
            // it like a tap").
            .overlay {
                Color.clear
                    .contentShape(Rectangle())
                    .onTapGesture { onToggle() }
                    .onLongPressGesture(minimumDuration: MessageLongPress.thresholdSeconds) {
                        onToggle()
                    }
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(accessibilityName)
            .accessibilityValue(MessageSelection.accessibilityValue(selected: selected))
            .accessibilityAddTraits(.isButton)
            .accessibilityAction { onToggle() }
        }
    }
}

struct MessageRow: View {
    let message: ChatMessage
    /// ADR-0061 — observed so a language switch re-renders the row.
    @ObservedObject var l10n = Localization.shared
    /// ADR-0023 — the local member's IDENTITY (their signature key),
    /// used to highlight the reaction chips this user contributed. Never
    /// their MLS leaf: a leaf is reassigned to the next member added.
    let ownIdentity: MemberIdentity?
    /// ADR-0048 — the resolved name of an incoming message's author in a group,
    /// drawn above the first bubble of a run. `nil` for a 1x1, an outgoing
    /// row, or an author the receive path could not attribute.
    var authorName: String? = nil
    /// Whether the row reserves the avatar column (incoming rows in a group).
    var showsSenderColumn: Bool = false
    /// Whether this message opens a run (name + avatar are drawn on it).
    var startsRun: Bool = true
    /// The bubble-corner time, `nil` when the server time is unknown.
    var timeLabel: String? = nil
    /// The widest a bubble may grow (about 78 percent of the timeline).
    var maxBubbleWidth: CGFloat = .infinity
    /// ADR-0048 — the resolved names of who reacted with an emoji.
    var reactorNames: (String) -> [String] = { _ in [] }
    /// ADR-0045 UX §6.2.5 — whether Edit / Delete / React are offered at all.
    /// `false` once this device's membership has ended: all three compose an
    /// application message, so all three are refused by the same core gate that
    /// refuses Send. Passed in rather than read here so the row stays a pure
    /// renderer of what `MembershipEnded.swift` decided.
    let actionsAvailable: Bool
    /// ADR-0024 — read state for our OWN outgoing messages
    /// (sent / read). `nil` for incoming / system rows and
    /// pre-ack outgoing rows (no glyph rendered).
    let readState: OwnMessageReadState?
    /// Send this row again, by its client-side id. Only ever reached from a
    /// row whose send failed.
    var onRetrySend: (Int64) -> Void = { _ in }
    /// 1.19c3 — bubble taps on attachment chips call into the
    /// screen's preview / share helpers. `onPreview` opens the
    /// inline image sheet; the `ShareLink` is wired inline.
    let onPreview: (AttachmentRef) -> Void
    /// ADR-0023 — context-menu actions. Edit / Delete only on own
    /// outgoing messages (gated below); React on any message.
    let onEdit: () -> Void
    let onDelete: () -> Void
    let onReact: (String) -> Void
    /// ADR-0047 §4 — the reaction bar's trailing `+`: any single emoji.
    var onMoreReactions: () -> Void = {}
    /// `docs/ux/calls.md` section 6 — tapping a call row calls back.
    var onCallBack: () -> Void = {}
    /// ADR-0052 — the menu's Reply: arm the composer with this message.
    var onReply: () -> Void = {}
    /// ADR-0052 / section 3.1 — the quote block was tapped; the argument is
    /// the quoted message's id. The screen decides whether it can jump.
    var onQuoteTap: (Int64) -> Void = { _ in }
    /// ADR-0052 decision 2 — the menu's Forward: open the picker for this
    /// message.
    var onForward: () -> Void = {}
    /// ADR-0052 decision 2 / section 5.1 — the "Forwarded from" line was
    /// tapped. The screen decides whether it leads anywhere.
    var onForwardTap: (ForwardRef) -> Void = { _ in }
    /// ADR-0052 decision 3 / section 4.1 — which word the card shows for this
    /// message (`Pin`, `Unpin`, or no row). Decided by the view model, which
    /// holds the two pin stores and this device's standing; the row is a pure
    /// renderer of it.
    var pinState: PinMenuState = .unavailable
    /// ADR-0052 decision 3 — the menu's `Pin` / `Unpin`: open the sheet, or
    /// act at once when there is only one way to unpin.
    var onPin: () -> Void = {}
    /// `docs/ux/message-actions.md` section 8 — the screen is in selection
    /// mode: the reaction row and the action card do not open (the row's
    /// long press toggles the selection instead, handled by the wrapper).
    var selecting: Bool = false
    /// Section 8 — the menu's `Select`: enter the mode with this message.
    var onSelect: () -> Void = {}

    /// `docs/ux/message-actions.md` — where the menu goes when it opens, `nil`
    /// outside a ``MessageMenuHost``.
    @Environment(\.messageMenuPresenter) private var menuPresenter

    /// Where this row's bubble is on screen.
    ///
    /// A reference held in `@State` rather than a `CGRect` in `@State`, and the
    /// difference is not stylistic: the frame changes on every scroll tick, so a
    /// value in `@State` would invalidate this row's body on every one of them,
    /// for every visible row, to serve a gesture that happens once. `@State`
    /// keeps the same box across the row's re-creations; writing through it
    /// invalidates nothing.
    @State private var bubbleFrame = BubbleFrameBox()

    private static let avatarSize: CGFloat = 28

    /// ADR-0047 §3 — a message that is exactly one video note renders as the
    /// circle itself, with no bubble chrome.
    ///
    /// ADR-0052 — never for a reply: the quote block needs a bubble to sit in,
    /// so a video note sent as a reply draws inside one.
    ///
    /// Nor for a forward (ADR-0052 decision 2): the attribution line sits
    /// inside the bubble, above the content.
    private var isBareVideoNote: Bool {
        guard !message.deleted, message.reply == nil, message.forward == nil, message.text.isEmpty,
              message.attachments.count == 1,
              let ref = message.attachments.first else { return false }
        return InlineMedia.decision(for: ref).bubble == .videoNote
    }

    /// UX spec section 10 — a message that is exactly one photo or video
    /// renders as the media itself, the time in a pill on top of it.
    ///
    /// ADR-0052 — never for a reply, for the same reason as ``isBareVideoNote``.
    private var bareVisualMedia: AttachmentRef? {
        guard !message.deleted, message.reply == nil, message.forward == nil, message.text.isEmpty,
              message.attachments.count == 1,
              let ref = message.attachments.first else { return nil }
        switch InlineMedia.decision(for: ref).bubble {
        case .photo, .video: return ref
        default: return nil
        }
    }

    @ViewBuilder
    private func visualMedia(_ ref: AttachmentRef) -> some View {
        menuTarget(visualMediaContent(ref))
    }

    private func visualMediaContent(_ ref: AttachmentRef) -> some View {
        Group {
            if InlineMedia.decision(for: ref).bubble == .video {
                VideoBubble(ref: ref, overlay: AnyView(mediaMetaPill))
            } else {
                PhotoBubble(ref: ref, overlay: AnyView(mediaMetaPill))
            }
        }
    }

    /// The bare video note plus its time, which is the whole bubble for that
    /// kind of message.
    ///
    /// One definition for both directions: it used to be written out twice, once
    /// in each arm of ``body``, and the menu now has to be attached to it — so a
    /// second copy would be a second place to forget.
    private func videoNoteColumn(alignment: HorizontalAlignment) -> some View {
        VStack(alignment: alignment, spacing: 2) {
            bareVideoNote
            metaRow
        }
    }

    /// Edit / Delete are offered only on our own outgoing
    /// messages that carry a server-assigned id and are not
    /// already deleted (the receive-side author gate is the real
    /// enforcement — this is just the affordance).
    private var canEditOrDelete: Bool {
        actionsAvailable
            && message.direction == .outgoing
            && message.messageId > 0
            && !message.deleted
    }

    /// React is offered on any non-system message with an id.
    private var canReact: Bool {
        actionsAvailable
            && message.direction != .system
            && message.messageId > 0
            && !message.deleted
    }

    /// ADR-0052 / section 3.1 — Reply is offered on any message with an id,
    /// which is the same set React is: not a system row, landed, not a
    /// tombstone, and only while this device can still send.
    private var canReply: Bool { canReact }

    /// ADR-0052 decision 2 / section 5.1 — Forward is offered on any landed
    /// message that is not a tombstone, and only once its send is complete:
    /// a row still sending or one that failed has nothing on the server to
    /// forward yet.
    private var canForward: Bool { canReact && message.ack == .sent }

    /// Section 8 — Select is offered on the rows the mode can select, which is
    /// the Forward gate (landed, not a tombstone, send complete).
    private var canSelect: Bool {
        MessageSelection.canSelect(
            actionsAvailable: actionsAvailable,
            isSystemRow: message.direction == .system,
            hasId: message.messageId > 0,
            deleted: message.deleted,
            sendComplete: message.ack == .sent)
    }

    private var isOutgoing: Bool { message.direction == .outgoing }

    var body: some View {
        switch message.direction {
        case .outgoing:
            HStack(alignment: .bottom, spacing: 0) {
                Spacer(minLength: 0)
                bubbleColumn(alignment: .trailing) {
                    if isBareVideoNote {
                        menuTarget(videoNoteColumn(alignment: .trailing))
                    } else if let ref = bareVisualMedia {
                        visualMedia(ref)
                    } else {
                        bubble
                    }
                }
                .frame(maxWidth: maxBubbleWidth, alignment: .trailing)
            }
        case .incoming:
            HStack(alignment: .top, spacing: 8) {
                if showsSenderColumn {
                    // The avatar once per run; the rows after it keep the column.
                    if startsRun {
                        InitialsAvatar(
                            name: authorName ?? "",
                            identity: message.authorKeyB64,
                            size: Self.avatarSize)
                    } else {
                        Color.clear.frame(width: Self.avatarSize, height: 1)
                    }
                }
                bubbleColumn(alignment: .leading) {
                    if startsRun, let authorName {
                        Text(authorName)
                            .font(.caption)
                            .fontWeight(.medium)
                            .foregroundStyle(Color.lakkoAccent)
                            .lineLimit(1)
                            .padding(.leading, 4)
                    }
                    if isBareVideoNote {
                        menuTarget(videoNoteColumn(alignment: .leading))
                    } else if let ref = bareVisualMedia {
                        visualMedia(ref)
                    } else {
                        bubble
                    }
                }
                .frame(maxWidth: maxBubbleWidth, alignment: .leading)
                Spacer(minLength: 0)
            }
        case .system:
            if let call = ChatTimeline.callRow(message) {
                callRow(call)
            } else {
                // Small and centred (UX spec section 5.2).
                Text(message.text)
                    .font(.caption)
                    .foregroundStyle(Color.lakkoTextMuted)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: .infinity, alignment: .center)
                    .padding(.vertical, 4)
            }
        }
    }

    /// `docs/ux/calls.md` section 6 — a finished call, centred like the other
    /// system rows but tappable: a tap calls back. The arrow says which way
    /// the call went, so the direction does not rest on the wording alone
    /// (section 8).
    @ViewBuilder
    private func callRow(_ call: CallTranscriptRow) -> some View {
        Button(action: onCallBack) {
            HStack(spacing: 6) {
                Image(systemName: glyph(for: call))
                    .font(.caption2)
                Text(call.text)
                    .font(.caption)
                if let timeLabel {
                    Text(timeLabel)
                        .font(.caption2)
                        .foregroundStyle(Color.lakkoTextMuted)
                }
            }
            .foregroundStyle(tint(for: call))
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .background(Color.lakkoSurfaceSunken)
            .clipShape(Capsule())
        }
        .buttonStyle(.plain)
        .frame(maxWidth: .infinity, alignment: .center)
        .padding(.vertical, 4)
        .accessibilityLabel(call.accessibilityLabel)
    }

    private func glyph(for call: CallTranscriptRow) -> String {
        switch call.outcome {
        case .answered:
            return call.direction == .outgoing ? "phone.arrow.up.right" : "phone.arrow.down.left"
        case .missed, .declined:
            return "phone.down"
        }
    }

    /// A missed call is the one outcome the reader may want to act on, so it
    /// carries the danger tint; everything else stays muted.
    private func tint(for call: CallTranscriptRow) -> Color {
        switch call.outcome {
        case .missed: return Color.lakkoDanger
        case .answered, .declined: return Color.lakkoTextMuted
        }
    }

    /// Wrap the bubble + the reaction-chip row in a vertical
    /// column aligned to the bubble's side, so chips sit just
    /// under their message.
    @ViewBuilder
    private func bubbleColumn<Content: View>(
        alignment: HorizontalAlignment,
        @ViewBuilder _ content: () -> Content
    ) -> some View {
        VStack(alignment: alignment, spacing: 4) {
            content()
            if !message.deleted, !message.reactions.isEmpty {
                reactionChips
            }
        }
    }

    /// `docs/ux/message-actions.md` §3 — Copy, on "any message with text".
    ///
    /// A tombstone has no text left and a lone attachment never had any. Gated on
    /// `actionsAvailable` like the other two even though Copy composes nothing and
    /// sends nothing: ADR-0045 UX §6.2.5 withdraws the per-message menu WHOLESALE
    /// when this device's membership ends, and Copy lives in that menu. Offering a
    /// one-item menu on a dead group would quietly reinterpret "withdrawn", which
    /// is a decision for the spec and not for this row.
    private var canCopy: Bool {
        actionsAvailable
            && message.direction != .system
            && !message.deleted
            && !message.text.isEmpty
    }

    /// `docs/ux/message-actions.md` §3 — the card's rows for THIS message.
    private var menuActions: [MessageMenuAction] {
        MessageMenuAction.offered(
            canReply: canReply, canForward: canForward, canEditOrDelete: canEditOrDelete,
            hasText: canCopy,
            // Section 4.1 — on any message whose send is complete and that
            // is not deleted, which is the set Forward is offered on.
            pin: canForward ? pinState : .unavailable,
            canSelect: canSelect)
    }

    /// Make a bubble the thing the menu opens on.
    ///
    /// `client-redesign.md` — the gesture is a long press, it reaches the menu
    /// from anywhere on the bubble INCLUDING over the bubble's own controls, and
    /// once the menu has opened the control underneath receives nothing more
    /// from that press.
    ///
    /// `simultaneousGesture` is what buys the first half. A plain
    /// `.onLongPressGesture` here would lose SwiftUI's arbitration to whatever
    /// control the press started on — the play disc, the waveform, a photo — and
    /// the menu would then be reachable only through the padding no control
    /// covers, which is the exact defect `client-redesign.md` records Android
    /// having had. A simultaneous gesture is recognised alongside the control's
    /// own instead of competing with it.
    ///
    /// The second half is not here: it is `MessageMenuHost`'s `.disabled` plus
    /// each control's own rule about a press that outlives
    /// ``MessageLongPress/thresholdSeconds`` (``VoiceSeekGate``).
    ///
    /// `content` is captured for the menu WITHOUT these two modifiers, so the
    /// copy the menu draws neither reports its own frame over this row's nor
    /// carries a second long press.
    private func menuTarget<Content: View>(_ content: Content) -> some View {
        content
            .onGeometryChange(for: CGRect.self) { proxy in
                proxy.frame(in: .global)
            } action: { frame in
                bubbleFrame.frame = frame
            }
            .simultaneousGesture(
                LongPressGesture(minimumDuration: MessageLongPress.thresholdSeconds)
                    .onEnded { _ in openMenu(AnyView(content)) }
            )
    }

    private func openMenu(_ bubble: AnyView) {
        guard let menuPresenter else { return }
        // Section 8 — "the reaction row and the action card do not open while
        // selecting". The wrapper already takes the press; this is the row's
        // own half of the rule.
        guard !selecting else { return }
        let frame = bubbleFrame.frame
        // Nothing has reported a frame yet, so there is nowhere to draw the
        // menu. Opening it anyway would put the bubble copy at the window's
        // corner.
        guard frame != .zero else { return }
        let actions = menuActions
        // A menu with no reaction row and no actions is an empty card over a
        // dimmed screen. The case is real: a tombstone offers neither.
        guard canReact || !actions.isEmpty else { return }
        menuPresenter.open(
            MessageMenuRequest(
                id: message.id,
                bubbleFrame: frame,
                isOutgoing: isOutgoing,
                actions: actions,
                canReact: canReact,
                bubble: bubble,
                onReact: onReact,
                onMoreReactions: onMoreReactions,
                perform: { action in
                    switch action {
                    case .reply:
                        onReply()
                    case .forward:
                        onForward()
                    case .pin, .unpin:
                        onPin()
                    case .copy:
                        // Decided here because this is what knows which text the
                        // person can see.
                        MessageClipboard.copy(message.text)
                    case .select:
                        onSelect()
                    case .edit:
                        onEdit()
                    case .delete:
                        onDelete()
                    }
                }
            )
        )
    }

    /// The bubble: radius 18, incoming on the sunken surface, outgoing on the
    /// accent; the time (and the delivery glyph on own messages) sits in the
    /// lower trailing corner, beside the last line of text.
    @ViewBuilder
    private var bubble: some View {
        menuTarget(bubbleContent)
    }

    @ViewBuilder
    private var bubbleContent: some View {
        HStack(alignment: .bottom, spacing: 8) {
            VStack(alignment: .leading, spacing: 6) {
                if message.deleted {
                    // Muted tombstone — no original text / attachments.
                    Label(l10n.t("chat.message.deleted"), systemImage: "trash")
                        .font(.body.italic())
                        .opacity(0.7)
                } else {
                    // ADR-0052 decision 2 / section 5.1 — one line above the
                    // content: "Forwarded from <name>", drawn from what the
                    // envelope carried and dressed as nothing more than a
                    // label.
                    if let forward = message.forward {
                        ForwardedLine(forward: forward, outgoing: isOutgoing) {
                            onForwardTap(forward)
                        }
                    }
                    // ADR-0052 / section 3.1 — the quote block above the text,
                    // drawn from the snapshot the envelope carried and never
                    // from the quoted message itself.
                    if let reply = message.reply {
                        ReplyQuoteBlock(reply: reply, outgoing: isOutgoing) {
                            onQuoteTap(reply.target)
                        }
                    }
                    if !message.text.isEmpty {
                        // No `.textSelection(.enabled)`. It installs UIKit's own
                        // long press for a selection loupe over the same pixels
                        // and the same duration as the gesture that opens the
                        // message menu, and `.contextMenu` used to be the thing
                        // that won that arbitration. With a custom presentation
                        // the two would both answer one press — a selection
                        // loupe under a menu. `docs/ux/message-actions.md` §3
                        // replaces what selection was being used for with Copy,
                        // which takes the whole message in one tap.
                        Text(message.text)
                            .font(.body)
                    }
                    if !message.attachments.isEmpty {
                        attachmentChips
                    }
                }
            }
            metaRow
        }
        .padding(.vertical, 8)
        .padding(.horizontal, 12)
        .background(isOutgoing ? Color.lakkoAccent : Color.lakkoSurfaceSunken)
        .foregroundStyle(isOutgoing ? Color.lakkoAccentForeground : Color.lakkoText)
        .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.bubble, style: .continuous))
    }

    /// "(edited)", the time, and — on own messages — the delivery glyph.
    @ViewBuilder
    private var metaRow: some View {
        metaContent
            .foregroundStyle(isOutgoing ? Color.lakkoAccentForeground : Color.lakkoTextMuted)
            .opacity(isOutgoing ? 0.85 : 1)
            .padding(.bottom, 1)
    }

    /// UX spec section 10 — the time, ticks and edited marker on a photo or
    /// video, in a small translucent pill.
    private var mediaMetaPill: some View {
        metaContent
            .foregroundStyle(Color.white)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(Capsule().fill(Color.black.opacity(0.5)))
            .padding(6)
    }

    @ViewBuilder
    private var metaContent: some View {
        HStack(spacing: 3) {
            if message.edited, !message.deleted {
                Text(l10n.t("chat.message.edited"))
                    .font(.caption2)
            }
            if let timeLabel {
                Text(timeLabel)
                    .font(.caption2)
                    .monospacedDigit()
            }
            // The action sits next to the message it belongs to, which is the
            // only place that can say WHICH message failed.
            if message.ack == .failed {
                Button(l10n.t("common.retry")) { onRetrySend(message.localId) }
                    .font(.caption2)
                    .buttonStyle(.plain)
                    .underline()
            }
            if isOutgoing, !message.deleted {
                deliveryGlyph
            }
        }
    }

    @ViewBuilder
    private var attachmentChips: some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(
                Array(message.attachments.enumerated()),
                id: \.offset
            ) { _, ref in
                // ADR-0047 — the bubble shape comes from `kind`, but only when
                // the core's MIME allowlist agrees on the media class; every
                // other ref (no kind, unknown kind, mismatched or refused MIME)
                // stays a file chip.
                let decision = InlineMedia.decision(for: ref)
                if decision.bubble == .file {
                    AttachmentChip(
                        ref: ref,
                        direction: message.direction,
                        onPreview: { onPreview(ref) }
                    )
                } else {
                    MediaAttachmentView(
                        ref: ref,
                        bubble: decision.bubble,
                        mediaKey: ChatMediaController.key(for: ref),
                        incoming: message.direction == .incoming
                    )
                }
            }
        }
    }

    @ViewBuilder
    private var bareVideoNote: some View {
        if let ref = message.attachments.first {
            MediaAttachmentView(
                ref: ref,
                bubble: .videoNote,
                mediaKey: ChatMediaController.key(for: ref),
                incoming: message.direction == .incoming
            )
        }
    }

    /// ADR-0023 — reaction chips under the bubble. Each chip is
    /// `emoji ×count`; tapping a chip the local user contributed
    /// toggles it back off (re-sends the same emoji op).
    ///
    /// ADR-0045 UX §6.2.5 — a chip carries BOTH a fact and an affordance, and
    /// the two separate here: the count is transcript, which §6.2.6 keeps
    /// whole, while tapping it is the React op, which dies with Send. So once
    /// the membership has ended the chip renders as static content rather than
    /// as a `Button`. Not `.disabled`, for the same reason the composer is
    /// replaced rather than disabled.
    @ViewBuilder
    private var reactionChips: some View {
        HStack(spacing: 4) {
            ForEach(message.reactionSummary(own: ownIdentity)) { tally in
                if actionsAvailable {
                    Button { onReact(tally.emoji) } label: { chipLabel(tally) }
                        .buttonStyle(.plain)
                        .contextMenu { reactedBy(tally.emoji) }
                        .accessibilityHint(reactedByLine(tally.emoji))
                } else {
                    chipLabel(tally)
                        .contextMenu { reactedBy(tally.emoji) }
                        .accessibilityHint(reactedByLine(tally.emoji))
                }
            }
        }
    }

    /// ADR-0048 — "Reacted by Alice, Bob" for a chip, by resolved name.
    private func reactedByLine(_ emoji: String) -> String {
        let names = reactorNames(emoji)
        return names.isEmpty
            ? ""
            : l10n.t("chat.reaction.reacted_by", texts: ["names": Localization.joinList(names)])
    }

    /// ADR-0048 — the long-press list of who reacted with `emoji`.
    @ViewBuilder
    private func reactedBy(_ emoji: String) -> some View {
        let names = reactorNames(emoji)
        if !names.isEmpty {
            Section(l10n.t("chat.reaction.reacted_by_title")) {
                ForEach(Array(names.enumerated()), id: \.offset) { _, name in
                    Text(name)
                }
            }
        }
    }

    /// The chip's content, shared by the interactive and the frozen arm so the
    /// two cannot come to look different.
    private func chipLabel(_ tally: ReactionTally) -> some View {
        HStack(spacing: 2) {
            Text(tally.emoji).font(.caption)
            if tally.count > 1 {
                Text(tally.count.formatted(.number.locale(l10n.locale)))
                    .font(.caption2)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
        }
        .padding(.horizontal, 7)
        .padding(.vertical, 3)
        .background(tally.reactedByMe ? Color.lakkoAccentSoft : Color.lakkoSurfaceSunken)
        .overlay(
            Capsule()
                .stroke(tally.reactedByMe ? Color.lakkoAccent : Color.clear, lineWidth: 1)
        )
        .clipShape(Capsule())
    }

    /// ADR-0024 — the delivery glyph in an own bubble's corner (UX spec
    /// section 5.2): one check sent, two checks read. No word; the colour is
    /// the bubble's foreground token. `nil` read state renders nothing
    /// (pre-ack rows).
    @ViewBuilder
    private var deliveryGlyph: some View {
        switch readState {
        case .none:
            EmptyView()
        case .sent:
            Image(systemName: "checkmark")
                .font(.system(size: 10, weight: .semibold))
                .accessibilityLabel(l10n.t("chat.message.sent"))
        case .read:
            ZStack(alignment: .leading) {
                Image(systemName: "checkmark")
                Image(systemName: "checkmark")
                    .offset(x: 4)
            }
            .font(.system(size: 10, weight: .semibold))
            .padding(.trailing, 4)
            .accessibilityLabel(l10n.t("chat.message.read"))
        }
    }
}

/// ADR-0047 — an outgoing media message still in the send queue: the bubble
/// renders from the local take (voice / video note play before upload), with a
/// determinate upload ring, or `Not sent · Retry` after a failure.
private struct PendingMediaRow: View {
    let pending: ChatMediaController.PendingSend
    let onRetry: () -> Void

    var body: some View {
        let bubble = InlineMedia.decision(for: pending.ref).bubble
        let upload: MediaUploadState = pending.failed ? .failed : .uploading(pending.progress)
        HStack {
            Spacer(minLength: 48)
            if bubble == .videoNote {
                MediaAttachmentView(
                    ref: pending.ref,
                    bubble: bubble,
                    mediaKey: pending.localKey,
                    incoming: false,
                    upload: upload,
                    localReady: pending.localReady,
                    onRetry: onRetry
                )
            } else {
                MediaAttachmentView(
                    ref: pending.ref,
                    bubble: bubble,
                    mediaKey: pending.localKey,
                    incoming: false,
                    upload: upload,
                    localReady: pending.localReady,
                    onRetry: onRetry
                )
                .padding(.vertical, 8)
                .padding(.horizontal, 12)
                .background(Color.lakkoAccent.opacity(pending.failed ? 0.55 : 1))
                .foregroundStyle(Color.lakkoAccentForeground)
                .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.bubble, style: .continuous))
            }
        }
    }
}

/// ADR-0023 — the edit sheet. A small form pre-filled with the
/// message's current text; Save publishes the edit op + applies
/// it locally, Cancel discards.
private struct EditMessageSheet: View {
    @ObservedObject var viewModel: ChatViewModel
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        NavigationStack {
            Form {
                Section(l10n.t("chat.composer.placeholder")) {
                    TextField(
                        l10n.t("chat.composer.placeholder"),
                        text: $viewModel.editText,
                        axis: .vertical
                    )
                    .lineLimit(1...6)
                }
            }
            .navigationTitle(l10n.t("chat.edit.title"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button(l10n.t("common.cancel")) { viewModel.cancelEdit() }
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button(l10n.t("common.save")) { viewModel.commitEdit() }
                        .disabled(
                            viewModel.editText.trimmingCharacters(
                                in: .whitespacesAndNewlines
                            ).isEmpty
                        )
                }
            }
        }
    }
}

/// Single attachment chip rendered inside a bubble. Hosts the
/// "Save" share affordance (resolves the bytes to a temp file +
/// hands to `ShareLink`) and the "Open" tap that funnels to the
/// screen-level inline preview sheet (only on image MIMEs).
private struct AttachmentChip: View {
    let ref: AttachmentRef
    let direction: Direction
    let onPreview: () -> Void

    @State private var resolvedBytes: Data?
    @State private var resolving: Bool = false
    @State private var resolveError: String?
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: chipIcon).font(.caption)
            VStack(alignment: .leading, spacing: 0) {
                Text(ref.name ?? l10n.t("chat.attachment.unnamed"))
                    .font(.caption)
                    .lineLimit(1)
                Text(AttachmentBridge.formatSize(ref.size))
                    .font(.caption2)
                    .opacity(0.75)
            }
            Spacer(minLength: 4)
            // "Open" surfaces only for image MIMEs; the
            // body-level `.sheet` does the actual rendering.
            if AttachmentBridge.isInlinePreviewable(ref) {
                Button(l10n.t("chat.attachment.open")) { onPreview() }
                    .font(.caption)
                    .buttonStyle(.borderless)
            }
            // "Save" — resolve on first tap (download +
            // decrypt + write to a temp file URL), then hand
            // off to `ShareLink`. The two-step is needed
            // because `ShareLink` wants a transferable up
            // front and Data → file is what the iOS share
            // sheet expects for "Save to Files".
            saveControl
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .background(chipBackground)
        .clipShape(RoundedRectangle(cornerRadius: 10))
    }

    @ViewBuilder
    private var saveControl: some View {
        if let bytes = resolvedBytes,
           let url = writeTempCopy(bytes: bytes, ref: ref) {
            ShareLink(item: url) {
                Text(l10n.t("common.save")).font(.caption)
            }
        } else if resolving {
            ProgressView().controlSize(.small)
        } else {
            Button(l10n.t("common.save")) {
                Task {
                    resolving = true
                    resolveError = nil
                    do {
                        resolvedBytes = try await AttachmentBridge
                            .downloadAndDecrypt(ref: ref)
                    } catch {
                        NSLog("attachment save failed: %@", String(describing: error))
                        resolveError = Localization.text("chat.attachment.error.download")
                    }
                    resolving = false
                }
            }
            .font(.caption)
            .buttonStyle(.borderless)
        }
    }

    private var chipIcon: String {
        if AttachmentBridge.isInlinePreviewable(ref) {
            return "photo"
        }
        return "doc"
    }

    private var chipBackground: Color {
        switch direction {
        case .outgoing:
            return Color.white.opacity(0.18)
        case .incoming, .system:
            return Color(.tertiarySystemBackground)
        }
    }

    /// Write `bytes` to a temp file with the ref's name (or a
    /// blob-id-derived fallback) so `ShareLink(item: URL)`
    /// surfaces a "Save to Files" / share-sheet flow with a
    /// meaningful filename.
    private func writeTempCopy(bytes: Data, ref: AttachmentRef) -> URL? {
        // `ref.name` is sender-controlled: sanitize it, and write into a fresh
        // per-share directory so no existing file can be overwritten.
        let baseName = AttachmentBridge.safeFileName(
            ref.name,
            fallback: "attachment-\(AttachmentBridge.hex(ref.blobId).prefix(8))"
        )
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("shared-attachments", isDirectory: true)
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        let url = dir.appendingPathComponent(baseName, isDirectory: false)
        do {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            try bytes.write(to: url, options: .atomic)
            return url
        } catch {
            return nil
        }
    }
}

/// A4 — the group-membership sheet: add a member by `@username` (G-4-verified)
/// and view / remove the live MLS roster. Names resolve through the accepted
/// contacts list (ADR-0021), falling back to a short key fingerprint.
/// ADR-0051 §3 — the `Group name` row of the members panel.
///
/// The draft is local to the row and re-seeded from the shared name whenever
/// it changes underneath (an envelope from another admin arriving while the
/// panel is open), unless the user is mid-edit — then their text stays and
/// Save decides, with the newer stamp winning on every device. An empty Save
/// clears the name (the title falls back to the members). The title only
/// changes after the send succeeded; a failed send shows the error line and
/// leaves the draft in place.
private struct GroupNameSection: View {
    @ObservedObject var viewModel: ChatViewModel
    @ObservedObject private var l10n = Localization.shared

    /// Whether the row is editable rather than read-only — `InfoSection.renameGroup`,
    /// passed in rather than re-read here.
    ///
    /// The row used to ask `viewModel.canRenameGroup` itself, which made this
    /// view the second place deciding who may rename a group. It is handed the
    /// shared rule's answer now, for the same reason every other block in the
    /// panel is: one copy of a rule cannot diverge from itself.
    let canRename: Bool

    @State private var draft: String = ""
    @State private var seededFrom: String? = nil
    @State private var busy = false
    @State private var error: UiText?
    @FocusState private var focused: Bool

    /// UX spec §2 — one line, at most 64 characters.
    private static let maxLength = 64

    private var dirty: Bool {
        draft.trimmingCharacters(in: .whitespacesAndNewlines) != (viewModel.groupName ?? "")
    }

    var body: some View {
        Section(l10n.t("chat.group_name.label")) {
            if canRename {
                HStack {
                    TextField(l10n.t("chat.group_name.label"), text: $draft)
                        .focused($focused)
                        .disabled(busy)
                        .onChange(of: draft) { _, value in
                            if value.count > Self.maxLength {
                                draft = String(value.prefix(Self.maxLength))
                            }
                            if value.contains(where: \.isNewline) {
                                draft = value.filter { !$0.isNewline }
                            }
                        }
                    Button(l10n.t(busy ? "common.saving" : "common.save")) {
                        Task { await save() }
                    }
                    .disabled(busy || !dirty)
                }
                if let error {
                    Text(error.text)
                        .font(.footnote)
                        .foregroundStyle(.red)
                }
            } else {
                Text(viewModel.groupName ?? l10n.t("chat.group_name.empty"))
                    .foregroundStyle(viewModel.groupName == nil ? Color.lakkoTextMuted : Color.lakkoText)
                    .accessibilityLabel(l10n.t("chat.group_name.label"))
                    .accessibilityValue(viewModel.groupName ?? l10n.t("chat.group_name.empty"))
            }
        }
        .onAppear { reseedIfIdle() }
        .onChange(of: viewModel.groupName) { _, _ in reseedIfIdle() }
    }

    /// Seed the draft from the shared name unless the user is editing a
    /// different value than the one this row last seeded from.
    private func reseedIfIdle() {
        let shared = viewModel.groupName ?? ""
        if !focused || draft == (seededFrom ?? "") {
            draft = shared
        }
        seededFrom = shared
    }

    private func save() async {
        if busy { return }
        busy = true
        error = nil
        defer { busy = false }
        // A stale control: the role can change while the panel is open. The
        // core's peers would refuse the envelope anyway; say why here.
        guard viewModel.canRenameGroup else {
            error = .key("chat.group_name.only_admins")
            return
        }
        if let message = await viewModel.saveGroupName(draft) {
            error = message
        } else {
            focused = false
            reseedIfIdle()
        }
    }
}

/// The group's info (`docs/ux/conversation-info.md` section 2.2): the shared
/// name, the roster with roles and add / remove, the invite link, transfer
/// ownership and `Leave group`.
///
/// Reached only through ``ConversationInfoSheet``, and only for a group —
/// which is the fix: this panel used to open on any conversation, so a chat
/// between two people was shown a group with the group-only parts hidden,
/// which is not the same thing as a screen about a person.
struct MembersSheet: View {
    @ObservedObject var viewModel: ChatViewModel
    /// ADR-0061 — observed so a language switch re-renders the panel.
    @ObservedObject var l10n = Localization.shared

    /// ADR-0045 (UX §6) — the `Leave group` confirmation. Local to the sheet:
    /// it is a presentation concern with no bearing on the request, and nothing
    /// outside this view may open it.
    @State private var leaveConfirmPresented = false

    /// ADR-0045 (UX §6) — the `Remove this member?` confirmation, holding the
    /// leaf awaiting it. A nullable leaf rather than a `Bool` beside a
    /// separately-stored target, because the confirmation names WHICH member:
    /// two pieces of state that must agree is how a confirmation ends up
    /// removing somebody the user did not look at.
    @State private var removeConfirmLeaf: UInt32?

    /// ADR-0045 UX §6.2.4 — the `Delete this chat` confirmation. Separate state
    /// from the leave confirmation rather than a shared flag, because the two
    /// controls are never offered at the same time and sharing one would make
    /// that an assumption rather than a fact.
    @State private var deleteConfirmPresented = false

    /// What the screen does after the chat is gone: the row it was reached from
    /// no longer exists, so staying on it would leave the user looking at a
    /// thread the registry has forgotten. Supplied by the presenting screen.
    var onDeleted: () -> Void = {}

    /// Which blocks this panel holds — `docs/ux/conversation-info.md` section
    /// 2.2, answered by the shared rule in `crates/lakko-ui-rules` rather than
    /// by this view.
    ///
    /// Every field is a capability this client already computed; none is
    /// re-derived here, and the rule never re-derives one either. The panel
    /// used to branch on those capabilities directly at each block, which is
    /// how the same paragraph of the same spec came to be answered three
    /// different ways on three clients.
    ///
    /// `canRename` and `canMintInvite` are fed the reads this client already
    /// made for exactly those two questions — `canRenameGroup` is a live
    /// `ownRole` read and `canManageMembers` a published one, and handing each
    /// block its own existing read is what keeps this a routing change rather
    /// than a behaviour change.
    ///
    /// `canLeave` is the same pair of conditions ``ChatViewModel/exitBlockState``
    /// hides the whole exit block on: ADR-0045's member exit is a `Leave`
    /// PROPOSAL, so a group carrying no ADR-0022 role table has nothing for the
    /// control to do, and a torn own-leaf read is not a licence to offer an
    /// exit either.
    private var groupSections: [InfoSection] {
        groupInfoSections(
            caps: GroupInfoCapabilities(
                canManageMembers: viewModel.canManageMembers,
                canRename: viewModel.canRenameGroup,
                canMintInvite: viewModel.canManageMembers,
                isOwner: viewModel.isOwner,
                canLeave: viewModel.usesProposalPath && viewModel.ownLeafIndex != nil,
                membershipEnded: viewModel.membershipHasEnded))
    }

    var body: some View {
        // Once per render, never per row: this crosses the FFI, and the
        // roster below is a `ForEach` that asks about `.roles` on every member.
        let sections = groupSections
        return NavigationStack {
            List {
                // ADR-0045 UX §6.1.9 — tell the new owner, best-effort and
                // fail-open. Nothing on the wire announces a transfer, so this
                // is an inference from an observed role-table change and can
                // never fire on a device that first sees the table already
                // naming it owner. It must therefore never be the only way a
                // user learns their role — the panel's affordances stay the
                // primary signal, exactly as they are for admin today.
                //
                // §8 argues AGAINST telling someone they hold the committer
                // duty, because it cannot be discharged. That argument does not
                // carry here, and the difference is the whole reason: ownership
                // CAN be discharged, by the flow directly below.
                if let banner = viewModel.newOwnerBanner {
                    Section {
                        Text(banner.headline).font(.footnote)
                        // The second line names the exit deliberately: it is the
                        // one sentence that stops the new owner from arriving
                        // where the old one was.
                        Text(banner.body)
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                        Button(banner.dismiss) { viewModel.dismissNewOwnerNotice() }
                    }
                    // UX §13 asks for a POLITE, non-interrupting announcement.
                    // SwiftUI has no `aria-live="polite"` equivalent: the only
                    // announcement API is `AccessibilityNotification.Announcement`,
                    // which speaks over whatever the user is reading — exactly
                    // what §13 forbids. So the honest treatment is the one the
                    // pending notice above already gets: a grouped container the
                    // reader reaches in order, whose children stay individually
                    // focusable so `Dismiss` is still a real button. Nothing
                    // here claims a mechanism the platform does not have.
                    .accessibilityElement(children: .contain)
                }
                // ADR-0051 §3 — the shared group name: `.groupName` decides the
                // row, `.renameGroup` decides whether it is editable with Save
                // or read-only.
                //
                // The gate used to be `presentsAsGroup || groupName != nil`,
                // which is unconditionally true HERE: this panel is reached
                // only through ``ConversationInfoSheet``'s `.group` arm, and
                // that arm is `infoTarget(.group)`. So `.groupName` is the
                // same answer, asked of the rule that owns it — and it also
                // stays true once the membership has ended, which is right:
                // the name is still what the group was called.
                if sections.contains(.groupName) {
                    GroupNameSection(
                        viewModel: viewModel,
                        canRename: sections.contains(.renameGroup))
                }
                // `docs/ux/who-can-find-me.md` section 5 — present only while
                // the setting includes groups at all.
                MembersCanFindMeSection(groupIdHex: viewModel.groupIdHex)
                // ADR-0022 — only owner/admin (or an open/legacy group) sees
                // the add affordance; a plain member's add would be refused.
                if sections.contains(.addMember) {
                    Section(l10n.t("chat.members.add.action")) {
                        HStack {
                            TextField(l10n.t("chat.members.add.placeholder"), text: $viewModel.addMemberInput)
                                .textInputAutocapitalization(.never)
                                .autocorrectionDisabled(true)
                                .disabled(viewModel.membershipBusy)
                            Button(l10n.t("chat.members.add.submit")) { viewModel.onAddMember() }
                                .disabled(
                                    viewModel.membershipBusy
                                        || !checkUsername(viewModel.addMemberInput).allowsLookup)
                        }
                        // ADR-0048 §4 — the reason, before any lookup.
                        UsernameRuleHint(input: viewModel.addMemberInput)
                    }
                }
                // ADR-0045 (UX §14) — the membership error/notice slot is
                // UNGATED, and that is a fix rather than a layout choice. It
                // used to live inside the `canManageMembers` `Add member`
                // section, so a plain member's membership error was invisible.
                // That was tolerable only while a plain member had no membership
                // action at all; `Leave group` is one, so the old placement
                // would have swallowed exactly the errors belonging to the users
                // who can now act.
                if viewModel.membershipError != nil || viewModel.membershipNotice != nil {
                    Section {
                        if let err = viewModel.membershipError {
                            Text(err.text)
                                .font(.footnote)
                                .foregroundStyle(.red)
                        }
                        if let notice = viewModel.membershipNotice {
                            Text(notice.text)
                                .font(.footnote)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                // ADR-0028 (G-3) — owner/admin "create invite link" affordance.
                // Mints a /g/<slug> link conveying this group's id; a non-member
                // opens it to request to join, approved via the pending list.
                if sections.contains(.inviteLink) {
                    Section(l10n.t("chat.invite_link.label")) {
                        Button(l10n.t("chat.invite_link.create")) { viewModel.onCreateInviteLink() }
                            .disabled(viewModel.membershipBusy)
                        if let link = viewModel.inviteLink {
                            Text(link)
                                .font(.system(.footnote, design: .monospaced))
                                .textSelection(.enabled)
                            Button(l10n.t("chat.invite_link.copy")) { UIPasteboard.general.string = link }
                            Button(l10n.t("chat.invite_link.revoke"), role: .destructive) {
                                viewModel.onRevokeInviteLink()
                            }
                            .disabled(viewModel.membershipBusy)
                        }
                    }
                }
                // ADR-0027 (G-3) — pending external-join requests. Owner/admin
                // only (same gate as add/remove); hidden when there are none.
                // Approve publishes the role-gated `Add` proposal (the sweep then
                // sends the Welcome to the joiner's mailbox); Reject drops the
                // retained proposal. Requests arrive as knocks (ADR-0053).
                if viewModel.canManageMembers,
                    !viewModel.pendingJoins.isEmpty || viewModel.unkeptJoinRequest
                {
                    Section(
                        l10n.t(
                            "chat.members.join.heading",
                            texts: ["count": viewModel.pendingJoins.count.formatted(.number.locale(Localization.currentLocale))])
                    ) {
                        // ADR-0027 / UX §7.1 — a request the quarantine turned
                        // away. It is in no row below, because the core never
                        // kept it, and it gets no Approve for the same reason:
                        // there is nothing to approve. Informational only,
                        // which is the most `externalJoinAffordance` allows.
                        if viewModel.unkeptJoinRequest {
                            Text(joinRequestNotKeptMessage)
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                        ForEach(viewModel.pendingJoins, id: \.self) { ref in
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    // ADR-0048 — the one resolver: a contact's
                                    // rename or profile name when known, else the
                                    // fingerprint without `@`.
                                    // ADR-0053 — resolved through the joiner's
                                    // account key, so a contact is named.
                                    Text(viewModel.pendingJoinLabel(ref))
                                        .font(.footnote)
                                    // ADR-0028 OQ2 — which member's link this
                                    // request arrived on + whether they are still
                                    // in the roster (advisory).
                                    if let attr = viewModel.joinAttributions[ref] {
                                        Text(
                                            l10n.t(
                                                attr.issuerInRoster
                                                    ? "chat.members.join.via_member"
                                                    : "chat.members.join.via_former",
                                                texts: ["name": attr.issuerFingerprint])
                                        )
                                        .font(.caption2)
                                        .foregroundColor(attr.issuerInRoster ? .secondary : .red)
                                    } else {
                                        Text(l10n.t("chat.members.join.link_unknown"))
                                            .font(.caption2)
                                            .foregroundColor(.secondary)
                                    }
                                }
                                Spacer()
                                Button(l10n.t("chat.members.join.approve")) {
                                    viewModel.onApproveJoin(proposalRef: ref)
                                }
                                .buttonStyle(.borderless)
                                .disabled(viewModel.membershipBusy)
                                Button(role: .destructive) {
                                    viewModel.onRejectJoin(proposalRef: ref)
                                } label: {
                                    Text(l10n.t("chat.members.join.reject"))
                                }
                                .buttonStyle(.borderless)
                                .disabled(viewModel.membershipBusy)
                            }
                        }
                    }
                }
                // ADR-0045 S4a (UX §5.1) — the pending notice. One line, and it
                // states INACTION rather than a deadline: decision 2 gives a
                // waiting request no expiry, so there is no moment at which the
                // client learns something new to say. Rendered only past the
                // quiet window, because the common case — the owner sweeping its
                // own request — lands before anyone could look, and a notice
                // that appears and vanishes teaches the user that membership is
                // slow when it is not.
                if let notice = viewModel.pendingNoticeText {
                    Section {
                        Text(notice.count).font(.footnote)
                        Text(notice.liveness)
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                }
                // ADR-0045 S4a — a requested member is NOT a member and must not
                // appear in the roster below. Their own section says exactly
                // what is true: they are waiting to be added.
                if !viewModel.waitingToBeAddedRequests.isEmpty {
                    Section(
                        l10n.t(
                            "chat.members.pending.adds_heading",
                            texts: ["count": viewModel.waitingToBeAddedRequests.count.formatted(.number.locale(Localization.currentLocale))])
                    ) {
                        ForEach(viewModel.waitingToBeAddedRequests) { request in
                            Text(Self.pendingAddLabel(request))
                                .font(.system(.footnote, design: .monospaced))
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                // ADR-0045 S7 residual 1 — `Not applied ({n})`.
                //
                // A sweep does not necessarily carry everything it finds, and an
                // excluded request used to vanish without a trace: OpenMLS
                // empties the proposal store on every merged commit whether or
                // not the commit carried the proposal. For an approved join it
                // is worse, because a successful approve CONSUMED the retained
                // request — the admin cannot even re-approve.
                //
                // This section says the one thing that is knowable, and no more:
                // the request did not apply. The core reports no reason for an
                // exclusion (`CreateCommitError` names no proposal), so neither
                // does the copy. Sits directly under `Waiting to be added`,
                // because it is where those rows go when they stop waiting.
                if !viewModel.unappliedAddRows.isEmpty {
                    Section(unappliedHeading(viewModel.unappliedAddRows.count)) {
                        Text(unappliedExplanation)
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                        ForEach(viewModel.unappliedAddRows, id: \.addSignatureKeyB64) { item in
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(Self.unappliedLabel(item))
                                        .font(.system(.footnote, design: .monospaced))
                                    Text(unappliedLine(item))
                                        .font(.caption2)
                                        .foregroundStyle(.secondary)
                                }
                                Spacer()
                                Button(l10n.t("common.dismiss")) {
                                    viewModel.onDismissUnapplied(
                                        addSignatureKeyB64: item.addSignatureKeyB64)
                                }
                                .buttonStyle(.borderless)
                            }
                        }
                    }
                }
                // ADR-0045 UX §6.2.7 — the roster stays, READ-ONLY, and carries
                // one line saying so.
                //
                // It stays because it is the last thing you knew about who you
                // were in a group with, it is genuinely informative next to a
                // frozen transcript, and it is still readable: `members()` is a
                // plain read of the public group with no state gate.
                //
                // The line is a statement about the LIST, not about any row in
                // it, and that is load-bearing rather than stylistic. A
                // `Remove` rewrites no `GroupContext`, so the role table is
                // untouched and a removed admin's frozen roster still names
                // them an admin — nothing here may present that as a live role.
                // "Stopped updating" is the literal truth (the local view is
                // frozen at the removing epoch and never moves again) rather
                // than a euphemism for it, and it is deliberately neutral
                // between having left and having been removed.
                if viewModel.membershipHasEnded {
                    Section {
                        Text(frozenRosterHeader)
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                }
                // `docs/ux/conversation-info.md` §2.2 — the roster. On a group
                // the rule always returns it, ended membership included (the
                // read-only line above says so); asking anyway is what keeps
                // this panel's blocks one list rather than one list and an
                // exception.
                if sections.contains(.memberList) {
                    Section(memberCountLabel) {
                        ForEach(viewModel.members, id: \.leafIndex) { member in
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(viewModel.memberLabel(member))
                                    // ADR-0045 S4a — NO optimistic update: a member
                                    // with a pending removal stays in the roster and
                                    // is annotated. Dropping the row would assert
                                    // they are gone while they are still in the
                                    // group and still decrypting every message.
                                    if let annotation = viewModel.pendingAnnotation(member) {
                                        Text(annotation)
                                            .font(.caption2)
                                            .foregroundStyle(.secondary)
                                    }
                                }
                                Spacer()
                                // ADR-0022 — the promote / demote toggle. The rule
                                // makes `.roles` OWNER-only, not owner-or-admin:
                                // a role-table change is owner-only and every peer
                                // refuses an admin's, so an admin offered this
                                // would be offered a button whose every press is
                                // rejected.
                                //
                                // `memberIsOwner` stays a separate, per-ROW test.
                                // The rule answers whether this reader gets the
                                // control at all; which rows carry one is about a
                                // member, which the rule says nothing about.
                                if sections.contains(.roles), !viewModel.memberIsOwner(member) {
                                    let isAdmin = viewModel.memberIsAdmin(member)
                                    Button {
                                        viewModel.onToggleAdmin(
                                            member: member,
                                            makeAdmin: !isAdmin
                                        )
                                    } label: {
                                        Text(l10n.t(isAdmin ? "chat.members.admin.remove" : "chat.members.admin.make"))
                                    }
                                    .buttonStyle(.borderless)
                                    .disabled(viewModel.membershipBusy)
                                }
                                // Remove only when we may manage members (owner /
                                // admin / open group) and it is not our own leaf.
                                //
                                // NOT routed through `sections`: the shared rule
                                // has no section for removing a member. `.addMember`
                                // names adding somebody and nothing else, and
                                // borrowing it here would assert an equivalence
                                // the rule does not make. The capability is the
                                // same one `.addMember` was computed from, read
                                // directly, which is what the rule expects of a
                                // control it does not name.
                                if viewModel.canManageMembers,
                                    viewModel.ownLeafIndex != member.leafIndex {
                                    Button(role: .destructive) {
                                        // ADR-0045 UX §6 — the confirmation is the
                                        // LAST exit: a published request cannot be
                                        // withdrawn, and a direct commit lands at
                                        // once. Never remove on a single tap.
                                        removeConfirmLeaf = member.leafIndex
                                    } label: {
                                        Text(l10n.t("common.remove"))
                                    }
                                    .buttonStyle(.borderless)
                                    .disabled(viewModel.membershipBusy)
                                }
                            }
                        }
                    }
                }
                // ADR-0045 (UX §5 item 6 / §6.1.1) — THE EXIT BLOCK: bottom of
                // the panel, one block whose meaning is *the exit*, with two
                // ROLE-DISJOINT controls of which exactly one is ever live.
                //
                // The owner's `Leave group` used to be HIDDEN, and this reverses
                // that — not because the old argument (a control that always
                // refuses is worse than no control) was wrong, but because it
                // was right only while transfer was unreachable. A disabled
                // control whose reason names the action directly above it is a
                // signposted route, and it is the same visible-but-disabled
                // pattern §5.3 already uses for a pending row's `Remove`. Hiding
                // it now would leave an owner looking for the exit finding
                // nothing at all about leaving.
                //
                // `.hidden` only on the proposal path: a legacy group's core has
                // no self-remove route and no owner to transfer from.
                //
                // ## Why `sections` is NOT the gate here, alone in this panel
                //
                // Every other block above asks `groupInfoSections` whether it
                // belongs. This one does not, and the reason is arithmetic
                // rather than taste: `group_info_sections` returns `.deleteChat`
                // on every ended membership and `.leaveGroup` on every live path
                // where `canLeave` holds — and `canLeave` is fed the SAME pair of
                // conditions `exitBlock` hides on. So wrapping this `switch` in
                // a test of those sections would be a condition true on exactly
                // the inputs that already reach a non-`.hidden` arm: a gate in
                // shape only, reading as protection it does not provide.
                //
                // Per-arm gating adds nothing either, because the two agree arm
                // for arm. `.ended` fires on the `membershipEnded` that produces
                // `.deleteChat`. `.owner` fires on the `isOwner` that produces
                // `.transferOwnership` — and cannot fire without a role table,
                // since `ownRole` and `usesProposalPath` are the same
                // `groupRoles()` read, so `isOwner && !usesProposalPath` cannot
                // hold. `.hidden` fires on exactly `!canLeave`, where the rule
                // returns neither control.
                //
                // What the machine decides ON TOP of that — which of the two
                // role-disjoint controls is live, and the sentence explaining
                // why the other is not — the section list says nothing about,
                // and that is the whole of ADR-0045 UX §6.1. It stays here.
                switch viewModel.exitBlockState {
                case .hidden:
                    EmptyView()
                // ADR-0045 UX §6.2.4 / §6.2.7 — once the membership has ended,
                // both of the other controls are gone (one names a group the
                // reader is not in, the other is owner-only and they are not)
                // and the block holds `Delete this chat` instead.
                //
                // This slot is the right home by §6.1.1's own argument: the
                // block's meaning is *the exit*, and once you are out the only
                // exit-shaped action left is closing the door behind you. A
                // permanently dead row with no action available is its own
                // defect — everything else about this state is "you cannot",
                // and this is the one thing they still can.
                //
                // It is also the ONE control in this whole design that destroys
                // user data on request. Everything else in §6.2 removes a claim
                // the UI should not have been making; this removes a transcript
                // that exists nowhere else, on one confirmation, with no undo
                // and no export. The confirmation says so in as many words, and
                // that is the only protection there is.
                case .ended:
                    Section {
                        Button(role: .destructive) {
                            deleteConfirmPresented = true
                        } label: {
                            Text(deleteChatControl)
                        }
                        .disabled(viewModel.membershipBusy)
                    }
                case .owner(let transfer, let leave):
                    Section {
                        // NOT destructive-styled at rest. Red would file it with
                        // `Remove` and `Leave group`, and an owner scanning for
                        // a way out should not have to read a warning colour to
                        // find one. Transfer is not a destruction; it is a
                        // handover that happens to be final, and finality is a
                        // property of the step below, not of the control that
                        // opens it.
                        Button(l10n.t("chat.transfer.action")) { viewModel.openTransferStep() }
                            .disabled(viewModel.membershipBusy || !transfer.enabled)
                        if let reason = transfer.disabledReason {
                            Text(reason)
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                        Button(role: .destructive) {
                            // Unreachable — the control is disabled for as long
                            // as this device is the owner. The action is left
                            // wired rather than stubbed so the two arms of this
                            // switch stay the same control in the same slot.
                            leaveConfirmPresented = true
                        } label: {
                            Text(l10n.t("chat.leave.action"))
                        }
                        .disabled(true)
                        // The reason is adjacent TEXT rather than a decoration:
                        // a VoiceOver reader who cannot see the greyed control
                        // must still hear why (UX §13).
                        if let reason = leave.disabledReason {
                            Text(reason)
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                    }
                case .member(let leave):
                    Section {
                        Button(role: .destructive) {
                            leaveConfirmPresented = true
                        } label: {
                            Text(l10n.t("chat.leave.action"))
                        }
                        // UX §5.3 — visible but disabled once we have asked.
                        // Hiding it then would read as the permission having
                        // vanished, where our own row's `Waiting to leave.`
                        // annotation explains a disabled control.
                        .disabled(viewModel.membershipBusy || !leave.enabled)
                        if let reason = leave.disabledReason {
                            Text(reason)
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
            }
            // ADR-0045 UX §6.1.11 / §14 — the transfer step is a PUSHED
            // destination inside the members sheet's own `NavigationStack`,
            // never a sheet over a sheet: one modal surface owns the whole
            // choice, because splitting it would put the warning and the button
            // on different screens.
            .navigationDestination(isPresented: $viewModel.transferStepOpen) {
                TransferOwnershipStep(viewModel: viewModel)
            }
            // UX §3 / §6 — the confirmation is the LAST EXIT, not a courtesy: a
            // published request cannot be withdrawn (there is no wire withdraw,
            // and evicting it locally would leave it live in every peer's
            // store). The body carries the one fact the user needs — that they
            // remain in the group, receiving messages, until someone applies it.
            // ADR-0045 UX §6 — the same idiom as `Leave this group?` directly
            // below, deliberately: the two sit in the same panel and a user
            // reading one after the other should be answering the same kind of
            // question. The copy comes from `removeMemberConfirmation` rather
            // than being inlined, so the three clients cannot drift on the
            // sentence that tells a user whether the person they are removing
            // can still read their messages. The body is ABSENT on a
            // direct-commit group, where that sentence would be false.
            .confirmationDialog(
                removeMemberTitle,
                isPresented: Binding(
                    get: { removeConfirmLeaf != nil },
                    set: { if !$0 { removeConfirmLeaf = nil } }
                ),
                titleVisibility: .visible
            ) {
                Button(removeMemberConfirmLabel, role: .destructive) {
                    if let leaf = removeConfirmLeaf {
                        removeConfirmLeaf = nil
                        viewModel.onRemoveMember(leafIndex: leaf)
                    }
                }
                Button(removeMemberCancelLabel, role: .cancel) { removeConfirmLeaf = nil }
            } message: {
                if let body = removeMemberConfirmation(
                    usesProposalPath: viewModel.usesProposalPath
                ).body {
                    Text(body)
                }
            }
            .confirmationDialog(
                l10n.t("chat.leave.title"),
                isPresented: $leaveConfirmPresented,
                titleVisibility: .visible
            ) {
                Button(l10n.t("chat.leave.confirm"), role: .destructive) { viewModel.onLeaveGroup() }
                Button(l10n.t("common.cancel"), role: .cancel) {}
            } message: {
                Text(l10n.t("chat.leave.body"))
            }
            // ADR-0045 UX §6.2.4 — the body is exact rather than sweeping, and
            // the precision is load-bearing: the messages really are gone, and
            // the dead MLS handle is not. `Group` exposes no delete, so the
            // inactive group's state stays in the provider snapshot as inert
            // bytes — inert in the strong sense (it cannot send, decrypt,
            // export or process) but not nothing.
            .confirmationDialog(
                deleteChatTitle,
                isPresented: $deleteConfirmPresented,
                titleVisibility: .visible
            ) {
                Button(l10n.t("common.delete"), role: .destructive) {
                    // Close the sheet and leave the thread only when the delete
                    // actually landed; a refusal leaves the panel open with the
                    // error slot filled, rather than dropping the user out of a
                    // chat that is still there.
                    if viewModel.onDeleteChat() {
                        viewModel.closeConversationInfo()
                        onDeleted()
                    }
                }
                Button(l10n.t("common.cancel"), role: .cancel) {}
            } message: {
                Text(deleteChatBody)
            }
            // Section 2.2 — the panel is the group's info, not only its
            // roster, and it is now only ever opened for a group.
            .navigationTitle(l10n.t("chat.info.group"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    if viewModel.membershipBusy {
                        ProgressView().controlSize(.small)
                    }
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button(l10n.t("common.done")) { viewModel.closeConversationInfo() }
                }
            }
            .onAppear { viewModel.refreshMembers() }
        }
    }

    /// A prospective member has no roster row yet, so the label comes from the
    /// one resolver over the signature key the request carries (ADR-0048): a
    /// rename or a profile already known for that key, else the fingerprint
    /// without `@`.
    @MainActor
    private static func pendingAddLabel(_ request: PendingRequest) -> String {
        guard let b64 = request.addSignatureKeyB64,
              let bytes = base64Decode(b64) else { return Localization.text("chat.members.unnamed") }
        return ProfileDirectory.shared.resolve(signatureKey: bytes).listLabel
    }

    /// The same identity treatment as ``pendingAddLabel(_:)``, reachable from a
    /// bare signature key — a `Not applied` row has left the pending set, so it
    /// no longer has a `PendingRequest` to carry it.
    @MainActor
    private static func unappliedLabel(_ item: UnappliedAdd) -> String {
        guard let bytes = base64Decode(item.addSignatureKeyB64) else {
            return Localization.text("chat.members.unnamed")
        }
        return ProfileDirectory.shared.resolve(signatureKey: bytes).listLabel
    }

    private var memberCountLabel: String {
        let n = viewModel.members.count
        return l10n.t("chat.members.heading", texts: ["count": n.formatted(.number.locale(l10n.locale))])
    }

}

/// ADR-0045 UX §6.1.4 / §6.1.5 — the ownership-transfer step.
///
/// **There is NO second dialog behind this, and that is the design rather than a
/// shortcut**: this step IS the confirmation, which happens to require two
/// choices to be well-formed. Stacking an `Are you sure?` on a surface that
/// already asked two questions buys friction of the kind that gets dismissed
/// reflexively, and a sheet over a sheet is idiomatic on none of the three
/// platforms. What replaces it is friction that cannot be discharged without
/// reading — the action names the person, is disabled until one is picked, and
/// uses the picked row's label verbatim.
///
/// **Ordering inside the step is load-bearing**: the irreversibility statement
/// sits ABOVE the role choice. Below it, the pre-selected option would read as
/// the safe one and the warning would read as being about the other one. It is
/// also what puts *"This cannot be undone"* ahead of the control that does it in
/// VoiceOver's focus order (UX §13).
private struct TransferOwnershipStep: View {
    @ObservedObject var viewModel: ChatViewModel
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        List {
            Section {
                Text(transferIrreversibleHeadline)
                    .font(.footnote.weight(.semibold))
                Text(transferIrreversibleBody)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            // §6.1.3 — every member except the owner, in the roster's own order,
            // with a muted `Admin` MARKER rather than a filter. An admins-only
            // picker is EMPTY in the commonest small group (owner plus one
            // member, no admins ever appointed) — the exact dead end this
            // feature exists to close, reappearing as a UI rule. The selection
            // VALUE is the 32-byte signature key, never the leaf: OpenMLS hands
            // a vacated leaf to the next Add.
            Section(l10n.t("chat.transfer.new_owner_legend")) {
                ForEach(viewModel.transferPickerRows) { row in
                    Button {
                        viewModel.transferPick = row.signatureKeyB64
                    } label: {
                        HStack {
                            Text(row.label)
                            if row.isAdmin {
                                Text(l10n.t("chat.members.admin.marker"))
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                            Spacer()
                            if viewModel.transferPick == row.signatureKeyB64 {
                                Image(systemName: "checkmark")
                            }
                        }
                    }
                    .buttonStyle(.plain)
                    // A single-select GROUP, not a list of buttons, and the row
                    // announces the roster label plus `, admin` where the marker
                    // shows (UX §13).
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel(
                        row.isAdmin
                            ? l10n.t("chat.transfer.candidate_admin", texts: ["name": row.label])
                            : row.label)
                    .accessibilityAddTraits(
                        viewModel.transferPick == row.signatureKeyB64
                            ? [.isButton, .isSelected] : .isButton)
                }
            }
            // §6.1.4 — a real decision, pre-selected but never defaulted. A
            // single-select control, NEVER a checkbox: a checkbox has an
            // unstated default state and this has none. The core refuses to
            // default it because either silent choice is wrong in one direction,
            // and the UI must not undo with a layout choice what the core
            // protected with a type.
            Section(l10n.t("chat.transfer.role_legend")) {
                ForEach(outgoingOwnerOptions) { option in
                    Button {
                        viewModel.transferOutgoing = option.value
                    } label: {
                        HStack(alignment: .top) {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(option.label)
                                Text(option.consequence)
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                            Spacer()
                            if viewModel.transferOutgoing == option.value {
                                Image(systemName: "checkmark")
                            }
                        }
                    }
                    .buttonStyle(.plain)
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel(option.label)
                    .accessibilityHint(option.consequence)
                    .accessibilityAddTraits(
                        viewModel.transferOutgoing == option.value
                            ? [.isButton, .isSelected] : .isButton)
                }
            }
            Section {
                // Note APPLIED, not NOW: the transfer is a proposal, and
                // asserting the exit is open before the commit lands would be
                // the optimistic update §3 forbids.
                Text(transferUnlocksLeaveMessage)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                Button(role: .destructive) {
                    viewModel.onTransferOwnership()
                } label: {
                    Text(viewModel.transferActionState.label)
                }
                .disabled(viewModel.membershipBusy || !viewModel.transferActionState.enabled)
                if let err = viewModel.membershipError {
                    Text(err.text)
                        .font(.footnote)
                        .foregroundStyle(.red)
                }
            }
        }
        .navigationTitle(l10n.t("chat.transfer.action"))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                if viewModel.membershipBusy {
                    // §14's busy convention on this platform: a spinner plus
                    // `disabled`, never an `-ing…` label in the button.
                    ProgressView().controlSize(.small)
                }
            }
            ToolbarItem(placement: .topBarTrailing) {
                // `Cancel` is a PRE-PUBLISH exit, and it is the last one.
                // Nothing here is a withdraw: once the proposal is published it
                // is in every member's store and dies only when some commit
                // lands, so no client may offer a cancel afterwards —
                // including, and especially, for a transfer.
                Button(l10n.t("common.cancel")) { viewModel.cancelTransferStep() }
                    .disabled(viewModel.membershipBusy)
            }
        }
    }
}

/// The chat header's principal item (UX spec section 5.2): a small avatar,
/// the name, and one muted line under it.
private struct ChatTitleView: View {
    let title: String
    let subtitle: String?
    /// Hashed for the avatar tint only; never shown.
    let identity: String
    let isGroup: Bool

    /// The bar item's height, fixed on purpose. As a `.principal` toolbar
    /// item this view is measured by the navigation bar at a fitting width,
    /// and a height that depends on that width (two truncating lines beside
    /// an avatar) gives the bar something to re-measure on every layout pass.
    /// That re-measuring was one leg of the layout feedback loop the toolbar
    /// call buttons describe (the `TimelineView`s inside bar items were the
    /// driver; removing them ended the loop on every chat). The fixed size is
    /// kept so the bar has nothing width-dependent to measure here at all.
    static let barHeight: CGFloat = 36
    /// The most the two text lines may take. With the width capped here and
    /// the whole item sized to its own ideal (`fixedSize` below), the bar's
    /// proposals cannot change the item's size.
    static let maxTextWidth: CGFloat = 170

    var body: some View {
        HStack(spacing: 8) {
            InitialsAvatar(name: title, identity: identity, size: 32, isGroup: isGroup)
            VStack(alignment: .leading, spacing: 0) {
                Text(title)
                    .font(.headline)
                    .lineLimit(1)
                if let subtitle {
                    Text(subtitle)
                        .font(.caption)
                        .foregroundStyle(Color.lakkoTextMuted)
                        .lineLimit(1)
                }
            }
            .frame(maxWidth: Self.maxTextWidth, alignment: .leading)
        }
        .frame(height: Self.barHeight)
        .fixedSize()
        .accessibilityElement(children: .combine)
    }
}
