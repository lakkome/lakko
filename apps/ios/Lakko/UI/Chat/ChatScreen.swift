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
    /// Leave this screen (pop it). Navigation back is the system back button
    /// and edge swipe; this is only for the case the chat was deleted from
    /// the members sheet, when there is nothing left to show.
    public var onBack: () -> Void

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

    /// Which picker is open: the composer's (Emoji + GIF), or the reaction
    /// bar's `+` for one message (Emoji only).
    private struct PickerRequest: Identifiable {
        let id = UUID()
        let mode: MediaPickerSheet.Mode
        let message: ChatMessage?
    }

    public init(
        viewModel: ChatViewModel,
        onBack: @escaping () -> Void
    ) {
        self.viewModel = viewModel
        self.onBack = onBack
        _media = StateObject(wrappedValue: ChatMediaController(port: viewModel.mediaPort()))
    }

    /// ADR-0047 §6 — the call owns the audio session and the camera.
    private var callLive: Bool {
        viewModel.activeCall != nil || viewModel.anyCallInProgress
    }

    public var body: some View {
#if DEBUG
        let _ = BodyEvaluationCounter.tick("ChatScreen")
#endif
        chatBody
            .overlay { VideoNoteOverlay(media: media) }
            .environmentObject(media)
            .environmentObject(media.live)
            .onAppear {
                media.updateTimeline(viewModel.messages)
                media.callActive = callLive
            }
            .onChange(of: viewModel.messages) { _, messages in
                media.updateTimeline(messages)
            }
            .onChange(of: callLive) { _, live in
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
            // No call is presented here: the 1x1 ringing banner and every
            // full-screen call (1x1, group, call link) are presented app-wide
            // from the root view (`CallHostOverlay`), so a call rings on any
            // screen and survives leaving this chat. Returning to the chat of a
            // live group call shows that call, never a second one.
            // A live group call on this chat that this device is not in.
            // Liveness is time-based (a call nobody joined stops being
            // joinable after a minute, any call after four hours), so the
            // banner re-evaluates on a timer rather than only on a new
            // journal row.
            if !viewModel.membershipHasEnded {
                TimelineView(.periodic(from: .now, by: 10)) { context in
                    GroupCallBanner(
                        affordance: viewModel.groupCallAffordance(
                            nowMs: Int64(context.date.timeIntervalSince1970 * 1000)
                        ),
                        busy: viewModel.groupCallStarting,
                        // One call at a time: a live call anywhere withdraws Join.
                        joinAvailable: !viewModel.anyCallInProgress,
                        onJoin: { video in viewModel.joinOngoingGroupCall(video: video) }
                    )
                }
            }
            messagesList
            Divider()
            composer
        }
        .background(Color.lakkoBackground)
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
        .navigationTitle(headline)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            // UX spec section 5.2: system back, avatar + name in the middle,
            // then call, video call and an overflow menu. At most two trailing
            // items plus the menu (section 4).
            ToolbarItem(placement: .principal) {
                // Tapping the title opens the members / group info sheet.
                Button {
                    viewModel.toggleMembersPanel()
                } label: {
                    ChatTitleView(title: headline, subtitle: subtitle, identity: viewModel.groupIdHex,
                                  isGroup: viewModel.presentsAsGroup)
                }
                .buttonStyle(.plain)
                .accessibilityHint(viewModel.presentsAsGroup ? "Opens group info" : "Opens members")
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
                Button {
                    // A live group call this device is not in is JOINED; else
                    // a group chat (>2 members) starts an SFU call (multi-party
                    // + SFrame) and a 1×1 chat stays P2P.
                    viewModel.onCallButton(video: false)
                } label: {
                    Image(systemName: "phone")
                }
                .accessibilityLabel(callButtonLabel(video: false))
                .disabled(viewModel.chatCallAction == .unavailable)
            }
            // Video-call button — starts the call with the local
            // camera published (`config.video`). Group chats publish
            // SFrame-sealed video through the SFU; a 1×1 chat opens a
            // P2P video call. Withdrawn with its sibling above once the
            // membership has ended (§6.2.8 item 4).
            ToolbarItem(placement: .navigationBarTrailing) {
                Button {
                    viewModel.onCallButton(video: true)
                } label: {
                    Image(systemName: "video")
                }
                .accessibilityLabel(callButtonLabel(video: true))
                .disabled(viewModel.chatCallAction == .unavailable)
            }
            }
            // A4 — the Members sheet (roster + add / remove) lives in the
            // overflow. Available on any chat (a 1×1 becomes a group by adding
            // the first member).
            ToolbarItem(placement: .navigationBarTrailing) {
                Menu {
                    Button {
                        viewModel.toggleMembersPanel()
                    } label: {
                        Label(viewModel.presentsAsGroup ? "Group info" : "Members", systemImage: "person.2")
                    }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
                .accessibilityLabel("More")
            }
        }
        .sheet(isPresented: $viewModel.membersPanelOpen) {
            // ADR-0045 UX §6.2.4 — a deleted chat's row is gone from the
            // registry, so the screen it was opened from has nothing left to
            // show. Route back the same way the Back button does.
            MembersSheet(viewModel: viewModel, onDeleted: onBack)
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
            "Attach",
            isPresented: $attachActionSheetPresented,
            titleVisibility: .visible
        ) {
            Button("Photo Library") {
                // Trigger the PhotosPicker by giving it a fresh
                // empty selection — the PhotosPicker `Button`
                // below opens immediately when bound to a
                // populated $photoPickerItems. SwiftUI's
                // `PhotosPicker` doesn't have a programmatic
                // show-trigger; we route through a transparent
                // overlay (`photoPickerTrigger` below).
                photoPickerOpenRequested = true
            }
            Button("File") {
                fileImporterPresented = true
            }
            Button("Cancel", role: .cancel) {}
        }
        // PhotosPicker — multi-select, resolved on `.onChange`.
        .photosPicker(
            isPresented: $photoPickerOpenRequested,
            selection: $photoPickerItems,
            maxSelectionCount: 10,
            matching: .images
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
                Task {
                    if let payload = await resolvePhoto(item) {
                        await viewModel.onAttachmentPicked(
                            bytes: payload.bytes,
                            mime: payload.mime,
                            name: payload.name
                        )
                    }
                }
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
                                Button("Done") { previewImageData = nil }
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
            locale: .current,
            parseMs: ChatRepository.parseDsTimestampMs)
    }

    /// The last row the timeline draws, for the scroll-to-bottom.
    private var lastDrawnMessageId: Int64? {
        viewModel.messages.last(where: { !ChatTimeline.isHiddenSystemRow($0) })?.messageId
    }

    @ViewBuilder
    private var messagesList: some View {
        GeometryReader { geometry in
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
                                    maxBubbleWidth: geometry.size.width * 0.78,
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
                                    onPreview: openAttachmentPreview,
                                    onEdit: { viewModel.beginEdit(entry.message) },
                                    onDelete: { viewModel.deleteMessage(entry.message) },
                                    onReact: { emoji in
                                        viewModel.react(to: entry.message, emoji: emoji)
                                    },
                                    // ADR-0047 §4 — the reaction bar's `+`.
                                    onMoreReactions: {
                                        picker = PickerRequest(mode: .reaction, message: entry.message)
                                    }
                                )
                                // Tighter inside a run, a clear gap between runs.
                                .padding(.top, entry.startsRun ? 10 : 2)
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
            }
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
                Text("No messages yet. Say hi.")
                    .font(.body)
                    .foregroundStyle(Color.lakkoTextMuted)
                    .frame(maxWidth: .infinity, alignment: .center)
                    .padding(.top, 40)
            } else {
                Text("Loading...")
                    .font(.body)
                    .foregroundStyle(.secondary)
                Text("This conversation is still being set up on this device.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
        }
    }

    /// VoiceOver label for a toolbar call button — says "Join" when the tap
    /// joins the live group call rather than starting a new one.
    private func callButtonLabel(video: Bool) -> String {
        if case .joinable = viewModel.groupCallAffordance() {
            return video ? "Join group call with video" : "Join group call"
        }
        return video ? "Video call" : "Call"
    }

    /// "Group call in progress" — shown while this chat has a live group call
    /// this device is not in. Join connects to that call id; nothing here can
    /// start a second call.
    private struct GroupCallBanner: View {
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
                            Text("Group call in progress")
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
                        Button("Join") { onJoin(false) }
                            .buttonStyle(.borderedProminent)
                            .tint(.white)
                            .foregroundStyle(.green)
                        Button("Join with video") { onJoin(true) }
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
            case 0: return "Starting — nobody has joined yet"
            case 1: return "1 person in the call"
            default: return "\(count) people in the call"
            }
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
                Text(err)
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
                    .accessibilityLabel("Attach")
                    ComposerTextField(text: $viewModel.draft, caret: caret, disabled: viewModel.sending)
                    // ADR-0047 §4 — the smiley opens the picker on Emoji.
                    Button {
                        picker = PickerRequest(mode: .compose, message: nil)
                    } label: {
                        composerIcon("face.smiling")
                    }
                    .buttonStyle(.plain)
                    .disabled(viewModel.sending)
                    .accessibilityLabel("Emoji and GIFs")
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
        case .idle, .requesting: return false
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
            .accessibilityLabel("Stop recording")
        case .finalizing:
            ProgressView()
                .frame(width: 44, height: 44)
        case .preview:
            Button { media.sendPreview() } label: {
                sendFace { Image(systemName: "paperplane.fill") }
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Send recording")
        case .idle, .requesting, .recording:
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
                .accessibilityLabel("Send")
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
                        Image(systemName: pendingChipIcon(ref))
                            .foregroundStyle(.secondary)
                        VStack(alignment: .leading, spacing: 0) {
                            Text(ref.name ?? "(unnamed)")
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
                        .accessibilityLabel("Remove attachment")
                    }
                    .padding(.horizontal, 8)
                    .padding(.vertical, 6)
                    .background(Color(.secondarySystemBackground))
                    .clipShape(RoundedRectangle(cornerRadius: 10))
                }
                if viewModel.uploading {
                    HStack(spacing: 6) {
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
        if AttachmentBridge.isInlinePreviewable(ref) {
            return "photo"
        }
        return "doc"
    }

    /// Map [``ChatViewModel/UploadStage``] to the caption shown
    /// in the composer tray's progress chip. `nil` collapses to
    /// a generic "Encrypting…" — matches the pre-polish copy so
    /// the brief race window doesn't show an empty caption.
    private func uploadCaption(_ stage: ChatViewModel.UploadStage?) -> String {
        switch stage {
        case .none:
            return "Encrypting…"
        case .encrypting(let fileName):
            return "Encrypting \(fileName)…"
        case .uploading(let fileName):
            return "Uploading \(fileName)…"
        }
    }

    /// PhotosPicker handoff — resolve a single
    /// `PhotosPickerItem` to `(bytes, mime, name)`. iOS 17
    /// gives us `loadTransferable(type: Data.self)` directly +
    /// `loadTransferable(type: ImageInfo.self)` for richer
    /// metadata; the plain Data path is enough for 1.19c3.
    private func resolvePhoto(
        _ item: PhotosPickerItem
    ) async -> AttachmentPayload? {
        // `loadTransferable(type: Data.self)` returns
        // `Data?` directly in recent SDKs (was double-
        // Optional in an earlier signature where one level
        // came from `try?`). The single guard-let unwraps
        // both the throw-or-nil result of `try?` and the
        // optional-payload Data in one step.
        guard let bytes = try? await item.loadTransferable(type: Data.self),
              !bytes.isEmpty
        else { return nil }
        // PhotosPickerItem doesn't expose a filename, and the
        // MIME comes from `supportedContentTypes` only on a
        // separate transferable type. For 1.19c3 we sniff the
        // type from the first bytes — JPEG vs PNG vs HEIC vs
        // GIF — and use a placeholder filename. The recipient
        // surfaces the MIME-icon fallback when none of these
        // match.
        let mime = sniffImageMime(bytes) ?? "image/jpeg"
        let name = "photo-\(Int(Date().timeIntervalSince1970))"
            + (mime == "image/png" ? ".png" : ".jpg")
        return AttachmentPayload(bytes: bytes, mime: mime, name: name)
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

    /// Magic-byte sniff for the four web image formats. Used
    /// as a fallback when the `PhotosPickerItem` transferable
    /// path doesn't carry an explicit MIME. Returns `nil` for
    /// non-image bytes so the recipient sees no MIME → no
    /// inline-preview hint.
    private func sniffImageMime(_ bytes: Data) -> String? {
        guard bytes.count >= 12 else { return nil }
        let b = [UInt8](bytes.prefix(12))
        // JPEG SOI: FF D8 FF
        if b[0] == 0xFF && b[1] == 0xD8 && b[2] == 0xFF { return "image/jpeg" }
        // PNG signature: 89 50 4E 47 0D 0A 1A 0A
        if b[0] == 0x89 && b[1] == 0x50 && b[2] == 0x4E && b[3] == 0x47 {
            return "image/png"
        }
        // GIF87a / GIF89a: "GIF8(7|9)a"
        if b[0] == 0x47 && b[1] == 0x49 && b[2] == 0x46 && b[3] == 0x38
            && (b[4] == 0x37 || b[4] == 0x39) && b[5] == 0x61 {
            return "image/gif"
        }
        // WebP: "RIFF????WEBP"
        if b[0] == 0x52 && b[1] == 0x49 && b[2] == 0x46 && b[3] == 0x46
            && b[8] == 0x57 && b[9] == 0x45 && b[10] == 0x42 && b[11] == 0x50 {
            return "image/webp"
        }
        return nil
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

private struct MessageRow: View {
    let message: ChatMessage
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

    /// ADR-0023 — emoji palette for the quick reaction picker.
    private static let reactionPalette = ["👍", "❤️", "😂", "😮", "😢", "🙏"]

    private static let avatarSize: CGFloat = 28

    /// ADR-0047 §3 — a message that is exactly one video note renders as the
    /// circle itself, with no bubble chrome.
    private var isBareVideoNote: Bool {
        guard !message.deleted, message.text.isEmpty, message.attachments.count == 1,
              let ref = message.attachments.first else { return false }
        return InlineMedia.decision(for: ref).bubble == .videoNote
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

    private var isOutgoing: Bool { message.direction == .outgoing }

    var body: some View {
        switch message.direction {
        case .outgoing:
            HStack(alignment: .bottom, spacing: 0) {
                Spacer(minLength: 0)
                bubbleColumn(alignment: .trailing) {
                    if isBareVideoNote {
                        VStack(alignment: .trailing, spacing: 2) {
                            bareVideoNote
                            metaRow
                        }
                        .contextMenu { menuItems }
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
                        VStack(alignment: .leading, spacing: 2) {
                            bareVideoNote
                            metaRow
                        }
                        .contextMenu { menuItems }
                    } else {
                        bubble
                    }
                }
                .frame(maxWidth: maxBubbleWidth, alignment: .leading)
                Spacer(minLength: 0)
            }
        case .system:
            // Small and centred (UX spec section 5.2).
            Text(message.text)
                .font(.caption)
                .foregroundStyle(Color.lakkoTextMuted)
                .multilineTextAlignment(.center)
                .frame(maxWidth: .infinity, alignment: .center)
                .padding(.vertical, 4)
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

    @ViewBuilder
    private var menuItems: some View {
        if canReact {
            // Quick-react palette — a sub-menu of the six core
            // emoji; tapping one toggles it on the message.
            Menu {
                ForEach(Self.reactionPalette, id: \.self) { emoji in
                    Button(emoji) { onReact(emoji) }
                }
                Button { onMoreReactions() } label: {
                    Label("More reactions", systemImage: "plus")
                }
            } label: {
                Label("React", systemImage: "face.smiling")
            }
        }
        if canEditOrDelete {
            Button { onEdit() } label: {
                Label("Edit", systemImage: "pencil")
            }
            Button(role: .destructive) { onDelete() } label: {
                Label("Delete", systemImage: "trash")
            }
        }
    }

    /// The bubble: radius 18, incoming on the sunken surface, outgoing on the
    /// accent; the time (and the delivery glyph on own messages) sits in the
    /// lower trailing corner, beside the last line of text.
    @ViewBuilder
    private var bubble: some View {
        HStack(alignment: .bottom, spacing: 8) {
            VStack(alignment: .leading, spacing: 6) {
                if message.deleted {
                    // Muted tombstone — no original text / attachments.
                    Label("Message deleted", systemImage: "trash")
                        .font(.body.italic())
                        .opacity(0.7)
                } else {
                    if !message.text.isEmpty {
                        Text(message.text)
                            .font(.body)
                            .textSelection(.enabled)
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
        .contextMenu { menuItems }
    }

    /// "(edited)", the time, and — on own messages — the delivery glyph.
    @ViewBuilder
    private var metaRow: some View {
        HStack(spacing: 3) {
            if message.edited, !message.deleted {
                Text("edited")
                    .font(.caption2)
            }
            if let timeLabel {
                Text(timeLabel)
                    .font(.caption2)
                    .monospacedDigit()
            }
            if isOutgoing, !message.deleted {
                deliveryGlyph
            }
        }
        .foregroundStyle(isOutgoing ? Color.lakkoAccentForeground : Color.lakkoTextMuted)
        .opacity(isOutgoing ? 0.85 : 1)
        .padding(.bottom, 1)
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
        return names.isEmpty ? "" : "Reacted by " + names.joined(separator: ", ")
    }

    /// ADR-0048 — the long-press list of who reacted with `emoji`.
    @ViewBuilder
    private func reactedBy(_ emoji: String) -> some View {
        let names = reactorNames(emoji)
        if !names.isEmpty {
            Section("Reacted by") {
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
                Text("\(tally.count)")
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
                .accessibilityLabel("Sent")
        case .read:
            ZStack(alignment: .leading) {
                Image(systemName: "checkmark")
                Image(systemName: "checkmark")
                    .offset(x: 4)
            }
            .font(.system(size: 10, weight: .semibold))
            .padding(.trailing, 4)
            .accessibilityLabel("Read")
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

    var body: some View {
        NavigationStack {
            Form {
                Section("Message") {
                    TextField(
                        "Message",
                        text: $viewModel.editText,
                        axis: .vertical
                    )
                    .lineLimit(1...6)
                }
            }
            .navigationTitle("Edit message")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button("Cancel") { viewModel.cancelEdit() }
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Save") { viewModel.commitEdit() }
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

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: chipIcon).font(.caption)
            VStack(alignment: .leading, spacing: 0) {
                Text(ref.name ?? "(unnamed)")
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
                Button("Open") { onPreview() }
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
                Text("Save").font(.caption)
            }
        } else if resolving {
            ProgressView().controlSize(.small)
        } else {
            Button("Save") {
                Task {
                    resolving = true
                    resolveError = nil
                    do {
                        resolvedBytes = try await AttachmentBridge
                            .downloadAndDecrypt(ref: ref)
                    } catch {
                        resolveError = (error as? LocalizedError)?
                            .errorDescription ?? "\(error)"
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

    @State private var draft: String = ""
    @State private var seededFrom: String? = nil
    @State private var busy = false
    @State private var error: String?
    @FocusState private var focused: Bool

    /// UX spec §2 — one line, at most 64 characters.
    private static let maxLength = 64

    private var dirty: Bool {
        draft.trimmingCharacters(in: .whitespacesAndNewlines) != (viewModel.groupName ?? "")
    }

    var body: some View {
        Section("Group name") {
            if viewModel.canRenameGroup {
                HStack {
                    TextField("Group name", text: $draft)
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
                    Button(busy ? "Saving..." : "Save") {
                        Task { await save() }
                    }
                    .disabled(busy || !dirty)
                }
                if let error {
                    Text(error)
                        .font(.footnote)
                        .foregroundStyle(.red)
                }
            } else {
                Text(viewModel.groupName ?? "No name yet")
                    .foregroundStyle(viewModel.groupName == nil ? Color.lakkoTextMuted : Color.lakkoText)
                    .accessibilityLabel(
                        viewModel.groupName.map { "Group name \($0)" } ?? "No group name yet")
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
            error = "Only admins can rename this group"
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

private struct MembersSheet: View {
    @ObservedObject var viewModel: ChatViewModel

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

    var body: some View {
        NavigationStack {
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
                // ADR-0051 §3 — the shared group name: read-only for a plain
                // member, editable with Save for the owner and admins (read
                // from the live group). For a group, and for any conversation
                // that already carries a shared name (a two-person one renamed
                // from another client is still keyed as a chat here); a plain
                // 1x1 chat is titled by the other person.
                if viewModel.presentsAsGroup || viewModel.groupName != nil {
                    GroupNameSection(viewModel: viewModel)
                }
                // ADR-0022 — only owner/admin (or an open/legacy group) sees
                // the add affordance; a plain member's add would be refused.
                if viewModel.canManageMembers {
                    Section("Add member") {
                        HStack {
                            TextField("@username", text: $viewModel.addMemberInput)
                                .textInputAutocapitalization(.never)
                                .autocorrectionDisabled(true)
                                .disabled(viewModel.membershipBusy)
                            Button("Add") { viewModel.onAddMember() }
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
                            Text(err)
                                .font(.footnote)
                                .foregroundStyle(.red)
                        }
                        if let notice = viewModel.membershipNotice {
                            Text(notice)
                                .font(.footnote)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                // ADR-0028 (G-3) — owner/admin "create invite link" affordance.
                // Mints a /g/<slug> link conveying this group's id; a non-member
                // opens it to request to join, approved via the pending list.
                if viewModel.canManageMembers {
                    Section("Invite link") {
                        Button("Create invite link") { viewModel.onCreateInviteLink() }
                            .disabled(viewModel.membershipBusy)
                        if let link = viewModel.inviteLink {
                            Text(link)
                                .font(.system(.footnote, design: .monospaced))
                                .textSelection(.enabled)
                            Button("Copy link") { UIPasteboard.general.string = link }
                            Button("Revoke link", role: .destructive) {
                                viewModel.onRevokeInviteLink()
                            }
                            .disabled(viewModel.membershipBusy)
                        }
                    }
                }
                // ADR-0027 (G-3) — pending external-join requests. Owner/admin
                // only (same gate as add/remove); hidden when there are none.
                // Approve drives the role-gated MLS approve + posts the commit
                // & Welcome rendezvous; Reject drops the retained proposal.
                if viewModel.canManageMembers,
                    !viewModel.pendingJoins.isEmpty || viewModel.unkeptJoinRequest
                {
                    Section("Join requests (\(viewModel.pendingJoins.count))") {
                        // ADR-0045 S7 residual 2 — the quarantine's one statable
                        // property. `Group::pending_external_joins` is in-memory:
                        // it is not in the provider snapshot, and the poll cursor
                        // has already advanced past the proposals that populated
                        // it, so a restart loses the list and no re-fetch brings
                        // it back. Ageing rows out of the panel instead would be
                        // strictly worse — the request stays live for the joiner,
                        // who goes on polling.
                        Text(joinRequestsNotSaved)
                            .font(.caption2)
                            .foregroundStyle(.secondary)
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
                                    Text(viewModel.personName(signatureKeyB64: base64Encode(ref))
                                        ?? "Someone")
                                        .font(.footnote)
                                    // ADR-0028 OQ2 — which member's link this
                                    // request arrived on + whether they are still
                                    // in the roster (advisory).
                                    if let attr = viewModel.joinAttributions[ref] {
                                        Text(
                                            attr.issuerInRoster
                                                ? "via a member's invite link"
                                                : "via the link of someone who is no longer a member"
                                        )
                                        .font(.caption2)
                                        .foregroundColor(attr.issuerInRoster ? .secondary : .red)
                                    } else {
                                        Text("invite source unknown")
                                            .font(.caption2)
                                            .foregroundColor(.secondary)
                                    }
                                }
                                Spacer()
                                Button("Approve") {
                                    viewModel.onApproveJoin(proposalRef: ref)
                                }
                                .buttonStyle(.borderless)
                                .disabled(viewModel.membershipBusy)
                                Button(role: .destructive) {
                                    viewModel.onRejectJoin(proposalRef: ref)
                                } label: {
                                    Text("Reject")
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
                    Section("Waiting to be added (\(viewModel.waitingToBeAddedRequests.count))") {
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
                                Button("Dismiss") {
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
                            // Owner-only promote / demote toggle; never the
                            // owner's own row (ADR-0022).
                            if viewModel.isOwner, !viewModel.memberIsOwner(member) {
                                let isAdmin = viewModel.memberIsAdmin(member)
                                Button {
                                    viewModel.onToggleAdmin(
                                        member: member,
                                        makeAdmin: !isAdmin
                                    )
                                } label: {
                                    Text(isAdmin ? "Remove admin" : "Make admin")
                                }
                                .buttonStyle(.borderless)
                                .disabled(viewModel.membershipBusy)
                            }
                            // Remove only when we may manage members (owner /
                            // admin / open group) and it is not our own leaf.
                            if viewModel.canManageMembers,
                                viewModel.ownLeafIndex != member.leafIndex {
                                Button(role: .destructive) {
                                    // ADR-0045 UX §6 — the confirmation is the
                                    // LAST exit: a published request cannot be
                                    // withdrawn, and a direct commit lands at
                                    // once. Never remove on a single tap.
                                    removeConfirmLeaf = member.leafIndex
                                } label: {
                                    Text("Remove")
                                }
                                .buttonStyle(.borderless)
                                .disabled(viewModel.membershipBusy)
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
                        Button("Transfer ownership") { viewModel.openTransferStep() }
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
                            Text("Leave group")
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
                            Text("Leave group")
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
                "Leave this group?",
                isPresented: $leaveConfirmPresented,
                titleVisibility: .visible
            ) {
                Button("Leave", role: .destructive) { viewModel.onLeaveGroup() }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text(
                    "You will keep receiving messages until an owner or an admin applies "
                        + "your request. You will need a new invite to rejoin."
                )
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
                Button("Delete", role: .destructive) {
                    // Close the sheet and leave the thread only when the delete
                    // actually landed; a refusal leaves the panel open with the
                    // error slot filled, rather than dropping the user out of a
                    // chat that is still there.
                    if viewModel.onDeleteChat() {
                        viewModel.membersPanelOpen = false
                        onDeleted()
                    }
                }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text(deleteChatBody)
            }
            .navigationTitle("Members")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    if viewModel.membershipBusy {
                        ProgressView().controlSize(.small)
                    }
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { viewModel.membersPanelOpen = false }
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
              let bytes = base64Decode(b64) else { return "Pending member" }
        return ProfileDirectory.shared.resolve(signatureKey: bytes).listLabel
    }

    /// The same identity treatment as ``pendingAddLabel(_:)``, reachable from a
    /// bare signature key — a `Not applied` row has left the pending set, so it
    /// no longer has a `PendingRequest` to carry it.
    @MainActor
    private static func unappliedLabel(_ item: UnappliedAdd) -> String {
        guard let bytes = base64Decode(item.addSignatureKeyB64) else { return "Pending member" }
        return ProfileDirectory.shared.resolve(signatureKey: bytes).listLabel
    }

    private var memberCountLabel: String {
        let n = viewModel.members.count
        return "\(n) member" + (n == 1 ? "" : "s")
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
            Section("New owner") {
                ForEach(viewModel.transferPickerRows) { row in
                    Button {
                        viewModel.transferPick = row.signatureKeyB64
                    } label: {
                        HStack {
                            Text(row.label)
                            if row.isAdmin {
                                Text("Admin")
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
                    .accessibilityLabel(row.isAdmin ? "\(row.label), admin" : row.label)
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
            Section("Your role afterwards") {
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
                    .accessibilityLabel("\(option.label). \(option.consequence)")
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
                    Text(err)
                        .font(.footnote)
                        .foregroundStyle(.red)
                }
            }
        }
        .navigationTitle("Transfer ownership")
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
                Button("Cancel") { viewModel.cancelTransferStep() }
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
        }
        .accessibilityElement(children: .combine)
    }
}
