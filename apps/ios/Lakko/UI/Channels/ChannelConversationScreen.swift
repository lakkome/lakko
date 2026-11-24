// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

/// ADR-0087 section 12 / `docs/ux/group-and-channel-privacy.md` section 12 —
/// a channel opens where a group opens, in the same view: the conversation
/// header (avatar, title, one line), the timeline of bubbles with day
/// separators, the chat composer for the owner and the publishers and none
/// for anyone else, the message menu, no call buttons. What only a channel
/// has: no sender line on a post, comments under each post, `Subscribe`.
struct ChannelConversationScreen: View {
    let channelId: String
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient
    let accountIdentitySeed: Data
    var foundHandle: String = ""
    var foundName: String = ""
    var foundReadKey: Data? = nil
    var subscribeOnOpen: Bool = false
    /// `docs/ux/feed.md` section 5 — the newest readable post was on screen.
    var onPostRead: ((_ channelId: String, _ cursor: UInt64) -> Void)?
    /// The title was tapped: the channel's info (`conversation-info.md`
    /// section 1), pushed by the shell.
    var onOpenInfo: () -> Void = {}
    /// The channel ended here (removed, deleted, unsubscribed elsewhere):
    /// back to Chats, never to a stale page under it. Defaults to a pop.
    var onEndedHere: (() -> Void)?

    @StateObject private var model: ChannelConversationModel
    @StateObject private var media: ChatMediaController
    @StateObject private var caret = ComposerCaret()
    @EnvironmentObject private var sync: DeviceGroupSyncRepository
    @EnvironmentObject private var inviteIntake: InviteIntake
    @ObservedObject private var l10n = Localization.shared
    /// docs/ux/connection-state.md — "Waiting for network..." /
    /// "Connecting..." while this device cannot reach the servers.
    @ObservedObject private var connection = ConnectionState.shared
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    /// ADR-0099 step 3 — a suspended account reads; it does not post.
    @Environment(\.accountSuspended) private var accountSuspended

    @State private var editing: ChannelTimelineItem?
    @State private var editText = ""
    @State private var picker: PickerRequest?
    @State private var anonymousNote = false
    @State private var attachSheet = false
    @State private var photoPickerOpen = false
    @State private var photoItems: [PhotosPickerItem] = []
    @State private var fileImporterOpen = false
    /// ADR-0092 — the menu row picked, the editor on screen, the audio picker.
    @State private var pendingMenuItem: AttachmentMenuItem?
    @State private var contentScreen: ContentScreenRequest?
    @State private var audioOpen = false
    @State private var closeConfirm: ChannelTimelineItem?
    @State private var previewImageData: Data?
    @State private var width: CGFloat = 360
    @State private var onScreen: Set<UInt64> = []
    @State private var composerFocused = false
    /// ADR-0099 / UX 2.1 — the report sheet opened from a post's menu.
    @State private var reportRequest: ReportRequest?

    private func onEnded() {
        if let onEndedHere { onEndedHere() } else { dismiss() }
    }

    private struct RemovedNotice: Identifiable {
        let name: String
        var id: String { name }
    }

    private struct PickerRequest: Identifiable {
        let id = UUID()
        let mode: MediaPickerSheet.Mode
        let item: ChannelTimelineItem?
        /// A comment's reaction, not the post's.
        var comment: ChannelConversationModel.Comment? = nil
    }

    init(
        channelId: String, state: ChannelsUiState, client: ChannelsClient, accountIdentitySeed: Data,
        foundHandle: String = "", foundName: String = "", foundReadKey: Data? = nil,
        subscribeOnOpen: Bool = false,
        onPostRead: ((String, UInt64) -> Void)? = nil,
        onOpenInfo: @escaping () -> Void = {},
        onEndedHere: (() -> Void)? = nil
    ) {
        self.channelId = channelId
        self.state = state
        self.client = client
        self.accountIdentitySeed = accountIdentitySeed
        self.foundHandle = foundHandle
        self.foundName = foundName
        self.foundReadKey = foundReadKey
        self.subscribeOnOpen = subscribeOnOpen
        self.onPostRead = onPostRead
        self.onOpenInfo = onOpenInfo
        self.onEndedHere = onEndedHere
        let model = ChannelConversationModel(
            channelId: channelId, state: state, client: client, accountIdentitySeed: accountIdentitySeed)
        model.foundHandle = foundHandle
        model.foundName = foundName
        model.foundReadKey = foundReadKey
        _model = StateObject(wrappedValue: model)
        _media = StateObject(wrappedValue: ChatMediaController(port: ChatMediaController.Port(
            sendAttachment: { [weak model] ref, _ in
                guard let model else { throw ChannelPublishError.failed }
                try await model.publishAttachment(ref)
            },
            sessionToken: { nil },
            idleDays: ChannelConversationModel.fileIdleDays)))
    }

    // MARK: - Title

    private var channel: ChannelHandle? { state.channels.first { $0.id == channelId } }

    private var title: String {
        ChannelsFormatting.title(
            sharedName: channel?.sharedName ?? "", localName: channel?.localName ?? foundName,
            handle: channel?.handle ?? foundHandle)
    }

    /// Section 12 — "Public channel · 10k+ subscribers" / "Private channel".
    private var headerLine: String? {
        // docs/ux/connection-state.md — the connection state replaces the line.
        if let key = connection.phase.headerKey { return l10n.t(key) }
        return ChannelHeaderLine.text(
            isPublic: state.extras(channelId).isPublic,
            subscriberCount: state.subscriberCountByChannel[channelId])
    }

    private var role: ChannelPostRole { model.isPublisher ? .publisher : .reader }

    // MARK: - Body

    var body: some View {
        MessageMenuHost(closingIds: Set(model.timeline.deleted.map { Int64(clamping: $0) })) { content }
            .overlay { VideoNoteOverlay(media: media) }
            // `docs/ux/message-actions.md` section 9 — a post seen going is
            // in front of the person only while this channel is open and the
            // scene is `active`.
            .onAppear {
                model.reduceMotion = reduceMotion
                model.screenVisible = true
            }
            .onDisappear { model.screenVisible = false }
            .onChange(of: scenePhase) { _, phase in
                if phase != .active { model.endDeletedHolds() }
            }
            .onChange(of: reduceMotion) { _, on in model.reduceMotion = on }
            .environmentObject(media)
            .environmentObject(media.live)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .principal) {
                    Button(action: onOpenInfo) {
                        ChannelTitleView(
                            title: title, subtitle: headerLine, channelId: channelId)
                    }
                    .buttonStyle(.plain)
                    .accessibilityHint(ConversationInfoDestination.ofChannel(channelId: channelId).accessibilityHint)
                }
            }
            .task {
                model.attach(sync: sync)
                await model.start(subscribeOnOpen: subscribeOnOpen)
                await model.refreshSubscriberCount()
                // While the screen is up (ADR-0087 section 12, 2026-11-06):
                // every 10 s only the posts after the newest one held, then
                // the counts of the posts on screen (comment counts with
                // them), and the threads whose count moved read on (UX
                // section 13.2, 2026-11-22); the seed envelopes are read once
                // a minute (every pass while waiting to be let in). Chosen,
                // not measured; the web and Android clients use the same.
                var ticks = 0
                while !Task.isCancelled {
                    // `docs/ux/connection-state.md` section 3 — the next pass
                    // runs at once when the network comes back.
                    await ConnectionState.sleepUnlessNetworkReturns(ms: 10_000)
                    if Task.isCancelled { return }
                    ticks += 1
                    if model.access == .waiting || ticks % 6 == 0 { await model.catchUp() }
                    let arrived = await model.poll()
                    let inView = onScreen
                    if !arrived { await model.refreshCounts(only: inView) }
                    await model.refreshComments(inView)
                    await model.flushPendingReactions()
                }
            }
            .onChange(of: state.channels.contains { $0.id == channelId }) { _, listed in
                // Section 9.5 — unsubscribed (here or on another device): the
                // row and its posts are gone, so is this screen. A removal
                // shows its dialog first.
                if !listed, model.removedName == nil,
                   model.access != .notSubscribed, model.access != .subscribing { onEnded() }
            }
            .lakkoDialog(
                item: Binding(
                    get: { model.removedName.map(RemovedNotice.init) },
                    set: { if $0 == nil { onEnded() } })
            ) { notice in
                // UX group-and-channel-privacy section 9 — one OK, then Chats.
                LakkoDialogContent(
                    title: Localization.text("chat.ended.notice", texts: ["name": notice.name]),
                    message: Localization.text("channels.ended.notice_body"), field: nil,
                    actions: [.primary(Localization.text("common.ok")) { onEnded() }])
            }
            .onChange(of: onScreen) { _, _ in markNewestRead() }
            .onDisappear { media.chatHidden() }
            .sheet(item: $picker) { request in pickerSheet(request) }
            // ADR-0099 / UX 2.3 — `Unsubscribe`, when checked, goes with the
            // report.
            .reportSheet($reportRequest) { follow in
                guard follow == .unsubscribe else { return }
                ChannelUnsubscribe.stopFollowing(state: state, channelId: channelId)
                state.onUnsubscribedHere?(channelId)
                onEnded()
            }
            .sheet(item: $editing) { item in
                EditMessageSheet(
                    text: $editText,
                    onCancel: { editing = nil },
                    onSave: {
                        let text = editText
                        editing = nil
                        Task { await model.edit(item, text: text) }
                    })
            }
            .sheet(isPresented: Binding(
                get: { previewImageData != nil }, set: { if !$0 { previewImageData = nil } }
            )) {
                if let data = previewImageData, let image = UIImage(data: data) {
                    NavigationStack {
                        Image(uiImage: image).resizable().scaledToFit()
                            .toolbar {
                                ToolbarItem(placement: .topBarTrailing) {
                                    Button(l10n.t("common.done")) { previewImageData = nil }
                                }
                            }
                    }
                }
            }
            .lakkoDialog(
                isPresented: $anonymousNote, title: l10n.t("channels.reactions.anonymous")
            ) { [.primary(l10n.t("common.ok")) {}] }
            // ADR-0092 / UX section 2 — the same menu as a conversation; in
            // a channel only the owner and publishers have a paperclip.
            .sheet(isPresented: $attachSheet, onDismiss: runPendingMenuItem) {
                AttachmentMenuSheet(rows: attachmentMenu(
                    kind: .channel, role: model.isPublisher ? .owner : .member,
                    hasCamera: ContentDevice.hasCamera, hasLocation: ContentDevice.hasLocation,
                    callHoldsCamera: false)
                ) { item in
                    pendingMenuItem = item
                    attachSheet = false
                }
            }
            .fullScreenCover(item: $contentScreen) { request in channelContentScreen(request) }
            .background(Color.clear.fileImporter(
                isPresented: $audioOpen, allowedContentTypes: [.audio], allowsMultipleSelection: true
            ) { result in
                guard case .success(let urls) = result else { return }
                // UX section 6 — the same audio upload as in a chat: kind
                // audio, duration, cover as the thumb, title and artist from
                // the file's own tags, through the composer tray.
                model.tray.add(urls.map { .audio($0) })
            })
            .lakkoDialog(
                isPresented: Binding(get: { closeConfirm != nil }, set: { if !$0 { closeConfirm = nil } }),
                title: l10n.t("poll.close_confirm")
            ) {
                [.danger(l10n.t("poll.close")) {
                    if let item = closeConfirm { Task { await model.closePoll(item) } }
                 },
                 .cancel()]
            }
            .photosPicker(
                isPresented: $photoPickerOpen, selection: $photoItems, maxSelectionCount: 10,
                matching: .any(of: [.images, .videos]))
            .onChange(of: photoItems) { _, items in
                photoItems = []
                guard !items.isEmpty else { return }
                Task { await addPicked(items, to: model.tray) }
            }
            .fileImporter(
                isPresented: $fileImporterOpen, allowedContentTypes: [UTType.data],
                allowsMultipleSelection: true
            ) { result in
                guard case .success(let urls) = result else { return }
                var sources: [ComposerTraySource] = []
                for url in urls {
                    let scoped = url.startAccessingSecurityScopedResource()
                    defer { if scoped { url.stopAccessingSecurityScopedResource() } }
                    guard let bytes = try? Data(contentsOf: url) else { continue }
                    guard bytes.count <= AttachmentBridge.maxAttachmentBytes else {
                        model.tray.report(.key("attach.preview.file_too_big"))
                        continue
                    }
                    let mime = UTType(filenameExtension: url.pathExtension)?.preferredMIMEType
                    sources.append(.file(bytes, name: url.lastPathComponent, mime: mime))
                }
                model.tray.add(sources)
            }
    }

    @ViewBuilder
    private var content: some View {
        // ADR-0073 phase 2 (UX 16.6) — a channel this device owned under keys
        // a reset replaced: the notice and `Remove from this device`.
        if model.isPublisher, state.orphanedByReset(channelId: channelId) {
            ChannelOrphanedView(channelId: channelId, state: state) { dismiss() }
        } else {
            liveContent
        }
    }

    @ViewBuilder
    private var liveContent: some View {
        timelineList
            .background(Color.lakkoBackground)
            .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width = $0 }
            .safeAreaInset(edge: .bottom, spacing: 0) { bottomBar }
    }

    // MARK: - Timeline

    /// `docs/ux/message-actions.md` section 9 — a deleted post is not drawn,
    /// except one in its going (``ChannelConversationModel/heldPosts``),
    /// which reads "Message deleted" for 2 seconds.
    private var entries: [ChatTimeline.Entry] {
        let held = model.heldPosts
        return ChatTimeline.layout(
            messages: model.drawnItems.map { ChannelTimeline.message($0, deleted: held[$0.cursor] != nil) },
            nowMs: ChatRepository.nowMs(),
            calendar: .current,
            locale: l10n.locale,
            parseMs: ChatRepository.parseDsTimestampMs)
    }

    private func item(_ message: ChatMessage) -> ChannelTimelineItem? {
        model.drawnItems.first { Int64(clamping: $0.cursor) == message.messageId }
    }

    @ViewBuilder
    private var timelineList: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    if model.timeline.earlierUnavailable {
                        Text(l10n.t("channels.posts.earlier_unavailable"))
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                            .frame(maxWidth: .infinity, alignment: .center)
                            .padding(.top, 12)
                    }
                    if model.drawnItems.isEmpty {
                        emptyState
                    }
                    ForEach(entries, id: \.message.messageId) { entry in
                        if let dateLabel = entry.dateLabel { DateSeparator(label: dateLabel) }
                        if let item = item(entry.message) {
                            Group {
                                if model.heldPosts[item.cursor] != nil {
                                    heldPostRow(entry: entry)
                                } else {
                                    postRow(entry: entry, item: item)
                                }
                            }
                                .padding(.top, 10)
                                .modifier(DeletedRowCollapse(phase: model.heldPosts[item.cursor]?.phase))
                                .id(item.cursor)
                                .onAppear {
                                    onScreen.insert(item.cursor)
                                    Task { await model.loadComments(item) }
                                }
                                .onDisappear { onScreen.remove(item.cursor) }
                        }
                    }
                }
                .padding(.horizontal, 12)
                .padding(.bottom, 12)
            }
            .scrollDismissesKeyboard(.interactively)
            .refreshable { await model.refresh() }
            .onChange(of: model.timeline.items.last?.cursor) { _, cursor in
                guard let cursor else { return }
                withAnimation(.easeOut(duration: 0.15)) { proxy.scrollTo(cursor, anchor: .bottom) }
            }
        }
    }

    @ViewBuilder
    private var emptyState: some View {
        let key: String? = {
            switch model.access {
            case .publisher: return model.loading ? "common.loading" : "channels.posts.empty_owner"
            case .reading: return model.loading ? "common.loading" : "channels.posts.empty"
            case .waiting: return nil
            case .notSubscribed: return "channels.subscribe.prompt_title"
            case .subscribing, .failed: return nil
            }
        }()
        if let key {
            Text(l10n.t(key))
                .font(.subheadline)
                .foregroundStyle(Color.lakkoTextMuted)
                .frame(maxWidth: .infinity)
                .padding(.top, 40)
        }
    }

    /// Section 9 — a post seen going: "Message deleted" in the post's own
    /// shape, no reactions, no comments, no menu.
    private func heldPostRow(entry: ChatTimeline.Entry) -> some View {
        MessageRow(
            message: entry.message,
            ownIdentity: nil,
            timeLabel: entry.timeLabel,
            maxBubbleWidth: width,
            actionsAvailable: false,
            readState: nil,
            onPreview: { _ in },
            onEdit: {},
            onDelete: {},
            onReact: { _ in },
            channelPost: role
        )
    }

    @ViewBuilder
    private func postRow(entry: ChatTimeline.Entry, item: ChannelTimelineItem) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            MessageRow(
                message: entry.message,
                ownIdentity: nil,
                timeLabel: entry.timeLabel,
                // Section 12 — a post is a card with the width of the pane.
                maxBubbleWidth: width,
                actionsAvailable: (model.access == .publisher || model.access == .reading) && model.writable,
                readState: nil,
                onPreview: openPreview,
                onEdit: {
                    editText = item.text
                    editing = item
                },
                onDelete: { Task { await model.delete(item) } },
                contentContext: item.contentEnvelopeB64 == nil ? nil : channelContentContext(item),
                contentActions: channelContentActions(item),
                onContentAction: { action in
                    switch action {
                    case .editChecklist: contentScreen = .checklist(editing: ChannelTimeline.message(item))
                    case .closeChecklist:
                        if let bytes = try? ContentFFI.composeClose(target: Int64(clamping: item.cursor), tally: nil) {
                            Task { await model.publishContentOp(bytes) }
                        }
                    case .copyCoordinates:
                        if case .location(let l)? = item.contentEnvelopeB64.flatMap({
                            ContentModel.content(PersistedContent(envelopeB64: $0)) }) {
                            MessageClipboard.copy(LocationText.coordinates(latE7: l.latE7, lonE7: l.lonE7))
                        }
                    default: closeConfirm = item
                    }
                },
                onReact: { emoji in Task { await model.react(item, emoji: emoji) } },
                onMoreReactions: { picker = PickerRequest(mode: .reaction, item: item) },
                reactionRow: model.reactionRow(item),
                chipRow: model.chips(item),
                onQuickReact: {
                    Task { await model.react(item, emoji: QuickReactionSetting.shared.current) }
                },
                // Reactions spec 8.3 — who reacted cannot be known: a note.
                onShowReactors: { _ in anonymousNote = true },
                onJoinCallInvite: { inviteIntake.handle($0) },
                onOpenOwnLink: { inviteIntake.handle($0) },
                // ADR-0099 / UX 2.1 — a subscriber reports the channel from a
                // post, with that post included.
                onReport: model.access == .reading && model.writable
                    ? {
                        reportRequest = model.reportRequest(
                            title: title, includingCursor: item.cursor, subscriber: true)
                    }
                    : nil,
                channelPost: role
            )
            ChannelCommentsView(
                model: model, item: item, isPublisher: model.isPublisher,
                onMoreReactions: { comment in
                    picker = PickerRequest(mode: .reaction, item: item, comment: comment)
                })
                .padding(.leading, 4)
        }
    }

    // MARK: - ADR-0092: the content kinds in a channel

    private func runPendingMenuItem() {
        guard let item = pendingMenuItem else { return }
        pendingMenuItem = nil
        switch item {
        case .photoOrVideo: photoPickerOpen = true
        case .camera: contentScreen = .camera
        case .file: fileImporterOpen = true
        case .audio: audioOpen = true
        case .poll: contentScreen = .poll
        case .checklist: contentScreen = .checklist(editing: nil)
        case .location: contentScreen = .location
        case .contact: contentScreen = .contact
        case .article: contentScreen = .article(editing: nil)
        }
    }

    @ViewBuilder
    private func channelContentScreen(_ request: ContentScreenRequest) -> some View {
        let publish: (ContentDraft, [AttachmentRef]) -> Void = { draft, refs in
            contentScreen = nil
            Task { await model.publishContent(draft, attachments: refs) }
        }
        switch request {
        case .poll:
            PollEditorScreen(
                anonymity: pollAnonymity(kind: .channel, accounts: 0),
                onCreate: { publish($0, []) }, onCancel: { contentScreen = nil })
        case .checklist(let editing):
            ChecklistEditorScreen(
                initial: editing.flatMap { ChecklistView.make(message: $0) }.map { view in
                    ChecklistBody(
                        title: view.state.title, othersAdd: false, othersMark: false,
                        items: view.state.items.compactMap { item in
                            if case .author(let id) = item.item { return ChecklistItem(id: id, text: item.text) }
                            return nil
                        })
                },
                showsSwitches: false,
                onDone: { body in
                    if let editing {
                        contentScreen = nil
                        let signed = SignedChecklist(body: body, authorSig: nil)
                        if let bytes = try? composeChecklistEditEnvelope(target: editing.messageId, checklist: signed) {
                            Task { await model.publishContentOp(bytes) }
                        }
                    } else {
                        publish(.checklist(body), [])
                    }
                },
                onCancel: { contentScreen = nil })
        case .location:
            LocationScreen(onSend: { publish(.location($0), []) }, onCancel: { contentScreen = nil })
        case .contact:
            ContactCardPickerScreen(
                candidates: ContactCardCandidates.load(), onSend: { publish(.contactCard($0), []) },
                onCancel: { contentScreen = nil })
        case .article:
            ArticleEditorScreen(
                groupIdHex: "channel-" + channelId, editing: nil,
                upload: { bytes in
                    let stamp = String(UUID().uuidString.prefix(8))
                    guard let prepared = PhotoVideoPrep.preparePhoto(bytes, stamp: stamp), let payload = prepared.bytes
                    else { return nil }
                    return try? await AttachmentBridge.encryptAndUpload(
                        bytes: payload, mime: prepared.mime, name: prepared.name,
                        media: MediaAttachmentMeta(kind: nil, width: prepared.width, height: prepared.height),
                        idleDays: ChannelConversationModel.fileIdleDays)
                },
                onPublish: { publish(.article($0), $1) }, onCancel: { contentScreen = nil })
        case .reader(let message):
            if let stored = message.content, case .article(let article)? = ContentModel.content(stored) {
                ArticleReaderScreen(
                    article: article, attachments: message.attachments, author: title, date: message.createdAt,
                    onClose: { contentScreen = nil },
                    menu: {
                        [.action(l10n.t("common.copy"), icon: .copy) {
                            MessageClipboard.copy(ArticleReaderScreen.plainText(article))
                        }]
                    })
            }
        case .camera:
            CameraCaptureScreen(onSend: { media in
                contentScreen = nil
                model.tray.add([ComposerTraySource.captured(media)])
            }, onClose: { contentScreen = nil })
        case .results:
            EmptyView()
        }
    }

    /// The message-menu rows a content post adds (publishers close a poll,
    /// edit or close a checklist; anyone copies a location).
    private func channelContentActions(_ item: ChannelTimelineItem) -> [MessageMenuAction] {
        guard let b64 = item.contentEnvelopeB64,
              let content = ContentModel.content(PersistedContent(envelopeB64: b64)) else { return [] }
        switch content {
        case .poll: return model.isPublisher && !item.closedByOp ? [.closePoll] : []
        case .checklist: return model.isPublisher && !item.closedByOp ? [.editChecklist, .closeChecklist] : []
        case .location: return [.copyCoordinates]
        default: return []
        }
    }

    /// A channel post's card: a poll votes through the credential.
    private func channelContentContext(_ item: ChannelTimelineItem) -> ContentBubbleContext {
        ContentBubbleContext(
            groupIdHex: "channel-" + channelId,
            // Publishers tick and add as the channel (ADR-0092 section 5).
            ownAccountKey: model.isPublisher ? Data(base64Encoded: ChannelTimeline.channelKeyB64) : nil,
            nameOf: { _ in nil },
            isOwnerOrAdmin: { _ in false },
            vote: { _, choices in Task { await model.votePoll(item, choices: choices) } },
            tick: { target, ref, done in
                guard let bytes = try? composeChecklistTickEnvelope(target: target, item: ref, done: done) else { return }
                Task { await model.publishContentOp(bytes) }
            },
            addItem: { target, text in
                guard let bytes = try? composeChecklistAddEnvelope(target: target, text: text) else { return }
                Task { await model.publishContentOp(bytes) }
            },
            openArticle: { contentScreen = .reader($0) },
            openResults: { _ in },
            contactCardState: { _ in .notice("") },
            contactCardAction: { _, _ in },
            pollOverride: { _ in model.pollView(item) },
            pollSendState: model.pollSendState[item.cursor],
            pollRetry: { Task { await model.votePoll(item, choices: model.pollView(item)?.ownChoices ?? []) } })
    }

    private func markNewestRead() {
        guard let newest = model.timeline.items.last?.cursor, onScreen.contains(newest),
              UIApplication.shared.applicationState == .active else { return }
        onPostRead?(channelId, newest)
    }

    private func openPreview(_ ref: AttachmentRef) {
        Task {
            if let bytes = try? await AttachmentBridge.downloadAndDecrypt(ref: ref) { previewImageData = bytes }
        }
    }

    // MARK: - Bottom bar

    @ViewBuilder
    private var bottomBar: some View {
        if let removal = model.removal {
            // ADR-0099 step 5 — a removed channel: the posts stay, nothing is
            // written; the line says why only when the log proved it (C-98).
            ChannelRemovedLine(status: removal)
        } else {
            liveBottomBar
        }
    }

    @ViewBuilder
    private var liveBottomBar: some View {
        switch model.access {
        case .publisher where accountSuspended:
            SuspendedComposerLine()
        case .publisher:
            composer
        case .notSubscribed where accountSuspended:
            // Subscribing writes to the channels service: not offered.
            SuspendedComposerLine()
        case .notSubscribed:
            subscribeBar
        case .subscribing:
            statusBar(l10n.t("channels.subscribe.status.subscribing"), danger: false, retry: false)
        case .waiting:
            // Section 7.2 — subscribed through a private invitation.
            statusBar(l10n.t("channels.link_page.waiting"), danger: false, retry: false)
        case .failed(let copy):
            statusBar(copy.text, danger: true, retry: true)
        case .reading:
            if let reader = model.readerDeviceRole {
                // UX second-device 4a — the owner's or a publisher's device
                // that cannot post: one line in place of the composer.
                readerDeviceLine(reader)
            } else {
                // Section 12 — a subscriber has no composer: the timeline runs
                // to the bottom of the pane.
                EmptyView()
            }
        }
    }

    /// No button, no link: secondary text where the composer would be.
    private func readerDeviceLine(_ reader: ChannelReaderDeviceRole) -> some View {
        Text(l10n.t(reader.lineKey))
            .font(.footnote)
            .foregroundStyle(Color.lakkoTextMuted)
            .multilineTextAlignment(.center)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity)
            .padding(12)
            .background(Color.lakkoSurface)
            .overlay(alignment: .top) { Rectangle().fill(Color.lakkoBorder).frame(height: 1) }
    }

    private var subscribeBar: some View {
        Button {
            Task { await model.subscribe() }
        } label: {
            Text(l10n.t("channels.subscribe.action"))
                .font(.headline)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 12)
        }
        .buttonStyle(.borderedProminent)
        .tint(Color.lakkoAccent)
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .background(Color.lakkoSurface)
        .overlay(alignment: .top) { Rectangle().fill(Color.lakkoBorder).frame(height: 1) }
    }

    private func statusBar(_ text: String, danger: Bool, retry: Bool) -> some View {
        HStack(spacing: 8) {
            Text(text)
                .font(.footnote)
                .foregroundStyle(danger ? Color.lakkoDanger : Color.lakkoTextMuted)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
            if retry {
                Button(l10n.t("common.retry")) { Task { await model.subscribe() } }
                    .font(.footnote)
            }
        }
        .padding(12)
        .background(Color.lakkoSurface)
        .overlay(alignment: .top) { Rectangle().fill(Color.lakkoBorder).frame(height: 1) }
    }

    // MARK: - Composer (the chat's, for the owner and publishers)

    private var composer: some View {
        VStack(spacing: 8) {
            if let err = model.sendError ?? model.tray.problem {
                Text(err.text)
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoDanger)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            // `docs/ux/media-messages.md` section 11 — the tray, directly
            // above the message field; hidden when empty.
            if !model.tray.tray.isEmpty || model.tray.limitNotice != nil {
                ComposerTrayView(
                    store: model.tray, callActive: media.callActive,
                    fetchVideo: { [media] ref in try await media.mediaFile(for: ref) })
            }
            if let denied = media.denied {
                MediaDeniedCard(resource: denied) { media.dismissDenied() }
            }
            HStack(alignment: .bottom, spacing: 4) {
                if showsRecordingBar {
                    RecordingBar(media: media).frame(maxWidth: .infinity, minHeight: 44)
                } else {
                    Button { attachSheet = true } label: { composerIcon(.paperclip) }
                        .buttonStyle(.plain)
                        .disabled(model.sending)
                        .accessibilityLabel(l10n.t("chat.composer.attach"))
                    ComposerTextField(
                        text: $model.draft, caret: caret, disabled: model.sending,
                        onReturn: { Task { await model.sendDraft() } },
                        focus: $composerFocused,
                        onPasteMedia: { providers in
                            Task { await stagePasted(providers) }
                        })
                    Button { picker = PickerRequest(mode: .compose, item: nil) } label: {
                        composerIcon(.faceSmiling)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel(l10n.t("chat.composer.emoji_gifs"))
                }
                trailingButton
            }
        }
        .padding(8)
        .background(Color.lakkoSurface)
    }

    private var showsRecordingBar: Bool {
        switch media.recState {
        case .idle, .pressing, .requesting: return false
        case .recording, .locked, .finalizing, .preview: return true
        }
    }

    private var draftHasText: Bool {
        !model.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private var hasContent: Bool {
        draftHasText || !model.tray.tray.isEmpty
    }

    @ViewBuilder
    private var trailingButton: some View {
        switch media.recState {
        case .locked:
            Button { media.stopLocked() } label: {
                ZStack {
                    Circle().fill(Color.lakkoDanger)
                    LkIconView(.stopFill, size: 20).foregroundStyle(Color.white)
                }
                .frame(width: 40, height: 40)
                .padding(2)
            }
            .buttonStyle(.plain)
            .accessibilityLabel(l10n.t("chat.record.stop"))
        case .finalizing:
            ProgressView().frame(width: 44, height: 44)
        case .preview:
            Button { media.sendPreview() } label: { sendFace }
                .buttonStyle(.plain)
                .accessibilityLabel(l10n.t("chat.record.send"))
        case .idle, .pressing, .requesting, .recording:
            if hasContent, !media.recState.isCapturing {
                Button { Task { await model.sendDraft() } } label: { sendFace }
                    .buttonStyle(.plain)
                    .disabled(model.sending || !model.tray.tray.canSend(hasText: draftHasText))
                    .accessibilityLabel(l10n.t("channels.compose.publish"))
            } else {
                RecordButton(media: media)
            }
        }
    }

    private var sendFace: some View {
        ZStack {
            Circle().fill(Color.lakkoAccent)
            if model.sending {
                ProgressView().controlSize(.small).tint(Color.lakkoAccentForeground)
            } else {
                LkIconView(.sendFill, size: 20).foregroundStyle(Color.lakkoAccentForeground)
            }
        }
        .frame(width: 40, height: 40)
        .padding(2)
    }

    private func composerIcon(_ icon: LkIcon) -> some View {
        LkIconView(icon, size: 20, relativeTo: nil)
            .foregroundStyle(Color.lakkoTextMuted)
            .frame(width: 40, height: 40)
            .contentShape(Rectangle())
    }

    @ViewBuilder
    private func pickerSheet(_ request: PickerRequest) -> some View {
        MediaPickerSheet(
            mode: request.mode,
            media: media,
            onEmoji: { emoji in
                if let item = request.item, let comment = request.comment {
                    picker = nil
                    Task { await model.reactComment(comment, of: item, emoji: emoji) }
                } else if let item = request.item {
                    picker = nil
                    Task { await model.react(item, emoji: emoji) }
                } else {
                    let end = ComposerCaret.insert(emoji, into: &model.draft, at: caret.utf16Range)
                    caret.utf16Range = end..<end
                }
            },
            onClose: { picker = nil },
            reactionOnly: request.item == nil ? nil : {
                if case .only(let list) = model.allowed { return list }
                return nil
            }(),
            reactionEnabled: { emoji in
                guard let item = request.item else { return true }
                let verdict = request.comment.map { comment in
                    reactionTapVerdict(
                        emoji: emoji, own: model.ownCommentReactions(comment, of: item), allowed: model.allowed,
                        messageKinds: (model.countsByComment[comment.cursor] ?? []).map(\.emoji))
                } ?? reactionTapVerdict(
                    emoji: emoji, own: model.ownReactions(item), allowed: model.allowed,
                    messageKinds: (model.countsByPost[item.cursor] ?? []).map(\.emoji))
                return verdict == .add || verdict == .remove
            },
            reactionNote: request.item.flatMap { item in
                let capReached = request.comment.map { model.commentReactionRow($0, of: item).capReached }
                    ?? model.reactionRow(item).capReached
                return capReached ? l10n.t("chat.reaction.cap_reached") : nil
            }
        )
        .presentationDetents([.medium, .large])
    }

    /// `docs/ux/attachments.md` section 2.5 — a pasted photo or video joins
    /// the composer tray, as a picked one does.
    private func stagePasted(_ providers: [NSItemProvider]) async {
        for loaded in await PastedMedia.load(providers) {
            switch loaded {
            case .photo(let bytes): model.tray.add([.photo(bytes)])
            case .video(let url): model.tray.add([.video(url)])
            case .failed(let isVideo):
                model.tray.report(
                    .key(isVideo ? "chat.attachment.error.open_video" : "chat.attachment.error.open_photo"))
            }
        }
    }
}

/// ADR-0099 step 5 / UX `reporting-and-moderation.md` section 3 — where a
/// removed channel's composer or `Subscribe` was: `suspended.channel_removed`
/// once the moderation log verified the removal, else the neutral
/// `channels.unavailable`.
struct ChannelRemovedLine: View {
    let status: ChannelRemovals.Status
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        Text(l10n.t(status.lineKey))
            .font(.footnote)
            .foregroundStyle(Color.lakkoTextMuted)
            .multilineTextAlignment(.center)
            .frame(maxWidth: .infinity, alignment: .center)
            .padding(12)
            .background(Color.lakkoBackground)
            .overlay(alignment: .top) { Rectangle().fill(Color.lakkoBorder).frame(height: 1) }
            .accessibilityElement(children: .combine)
    }
}

/// Section 12 — the conversation header of a channel: its photo or
/// initials, the title, one line.
struct ChannelTitleView: View {
    let title: String
    let subtitle: String?
    let channelId: String

    var body: some View {
        HStack(spacing: 8) {
            InitialsAvatar(
                name: title, identity: channelId, size: 32,
                placeholderIcon: .megaphoneFill, photo: .group(channelId),
                // UX profile-screen section 7: the chat header plays the
                // video always on a touch platform.
                playsVideo: true)
            VStack(alignment: .leading, spacing: 0) {
                Text(title).font(.headline).lineLimit(1)
                if let subtitle {
                    Text(subtitle)
                        .font(.caption)
                        .foregroundStyle(Color.lakkoTextMuted)
                        .lineLimit(1)
                }
            }
            .frame(maxWidth: 170, alignment: .leading)
        }
        .frame(height: 36)
        .fixedSize()
        .accessibilityElement(children: .combine)
    }
}

/// Section 12 — the one line under a channel's title.
enum ChannelHeaderLine {
    /// The size band of an exact count (ADR-0059 section 4).
    static func band(_ count: Int) -> String {
        DiscoverCards.subscriberBands[Int(ConversationHandles.sizeBand(count))]
    }

    static func text(isPublic: Bool?, subscriberCount: Int?) -> String? {
        guard let isPublic else { return nil }
        if !isPublic { return Localization.text("channels.header.private") }
        if let subscriberCount {
            return Localization.text("channels.header.public_count", texts: ["band": band(subscriberCount)])
        }
        return Localization.text("channels.header.public")
    }
}

// MARK: - Orphaned by a key reset (ADR-0073 phase 2, UX 16.6)

/// The channel as the device that reset its account keys sees it: the header,
/// one notice stating what the reset did, and one control. The person creates
/// a new channel from the create menu as always; nothing is copied across,
/// because the subscribers of the old channel have no way to be told.
struct ChannelOrphanedView: View {
    let channelId: String
    @ObservedObject var state: ChannelsUiState
    let onRemoved: () -> Void
    @State private var confirming = false
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                HStack(spacing: 12) {
                    InitialsAvatar(
                        name: state.channels.first(where: { $0.id == channelId })?.title ?? "",
                        identity: channelId, size: 48, placeholderIcon: .megaphoneFill,
                        photo: .group(channelId))
                    VStack(alignment: .leading, spacing: 2) {
                        Text(state.channels.first(where: { $0.id == channelId })?.title ?? "")
                            .font(.headline)
                            .foregroundStyle(Color.lakkoText)
                        Text(l10n.t("channels.keys_reset.row_sub"))
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                }
                HStack(alignment: .top, spacing: 12) {
                    LkIconView(.shieldExclamation, size: 20)
                        .foregroundStyle(Color.lakkoTextMuted)
                        .accessibilityHidden(true)
                    Text(l10n.t("channels.keys_reset.notice"))
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoText)
                }
                .padding(12)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Color.lakkoSurfaceSunken)
                .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))
                .accessibilityElement(children: .combine)
                Button(role: .destructive) { confirming = true } label: {
                    Text(l10n.t("channels.keys_reset.remove"))
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
            }
            .padding(16)
        }
        .background(Color.lakkoBackground)
        .lakkoDialog(isPresented: $confirming, title: l10n.t("channels.keys_reset.remove")) {
            [
                .danger(l10n.t("channels.keys_reset.remove")) {
                    state.removeOrphanedChannel(channelId)
                    onRemoved()
                },
                .cancel(),
            ]
        }
    }
}

