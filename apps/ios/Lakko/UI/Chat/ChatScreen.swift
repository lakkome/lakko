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
    public var onBack: () -> Void

    /// Phase 2a3e2b-iOS — cross-device sync runtime. Attached
    /// to the VM on first appearance so the composer's `draft`
    /// seeds from sibling-device drafts and rebroadcasts every
    /// change (debounced).
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

    public init(
        viewModel: ChatViewModel,
        onBack: @escaping () -> Void
    ) {
        self.viewModel = viewModel
        self.onBack = onBack
    }

    public var body: some View {
        VStack(spacing: 0) {
            // G3-1-call-present — the call host observes the
            // live `CallSession` (so state transitions update
            // the UI), renders the ringing banner inline, and
            // presents the full `CallScreen` over the chat as a
            // `fullScreenCover` once the call goes live.
            if let call = viewModel.activeCall {
                CallHostView(session: call, viewModel: viewModel)
            }
            messagesList
            // ADR-0024 — typing indicator, just above the input bar.
            // Auto-hides when nobody is typing (the VM nils the text).
            if let typing = viewModel.typingIndicatorText {
                HStack(spacing: 6) {
                    Text(typing)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Spacer()
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 4)
                .transition(.opacity)
            }
            Divider()
            composer
        }
        .animation(.easeInOut(duration: 0.15), value: viewModel.typingIndicatorText)
        .onAppear {
            // Phase 2a3e2b — wire the cross-device sync hook
            // on first mount. Idempotent: subsequent calls
            // overwrite the weak reference but the existing
            // Combine subscriptions stay intact.
            viewModel.attachSync(deviceGroupSync)
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
        // SFU group call — present the in-call UI over the chat,
        // driven by the `CallUiState` the `SfuCallUi` adapter updates.
        // Dismissing (or the call ending) hangs the SFU session up.
        .fullScreenCover(
            isPresented: Binding(
                get: { viewModel.activeSfuCall != nil },
                set: { presented in
                    if !presented { viewModel.hangUpGroupCall() }
                }
            )
        ) {
            CallScreen(externalState: viewModel.sfuCallUiState) {
                viewModel.hangUpGroupCall()
            }
        }
        .navigationTitle(headline)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .navigationBarLeading) {
                Button(action: onBack) {
                    Label("Back", systemImage: "chevron.backward")
                }
            }
            ToolbarItem(placement: .navigationBarTrailing) {
                Button(action: viewModel.onRefreshClicked) {
                    if viewModel.refreshing {
                        ProgressView().controlSize(.small)
                    } else {
                        Image(systemName: "arrow.clockwise")
                    }
                }
                .disabled(viewModel.refreshing)
            }
            // A4 — Members sheet: roster + add / remove. Available on
            // any chat (a 1×1 becomes a group by adding the first member).
            ToolbarItem(placement: .navigationBarTrailing) {
                Button {
                    viewModel.toggleMembersPanel()
                } label: {
                    Image(systemName: "person.2.fill")
                }
            }
            // Phase 3a9 — audio Call button. Hidden when a call
            // overlay is up (the overlay carries the
            // Accept / Decline / Hang up affordances).
            ToolbarItem(placement: .navigationBarTrailing) {
                Button {
                    // A group chat (>2 members) routes through the SFU
                    // (multi-party + SFrame); a 1×1 chat stays P2P.
                    if viewModel.isGroupChat {
                        viewModel.startGroupCall(video: false)
                    } else {
                        viewModel.startCall(audio: true, video: false)
                    }
                } label: {
                    Image(systemName: "phone.fill")
                }
                .disabled(viewModel.activeCall != nil || viewModel.activeSfuCall != nil)
            }
            // Video-call button — starts the call with the local
            // camera published (`config.video`). Group chats publish
            // SFrame-sealed video through the SFU; a 1×1 chat opens a
            // P2P video call.
            ToolbarItem(placement: .navigationBarTrailing) {
                Button {
                    if viewModel.isGroupChat {
                        viewModel.startGroupCall(video: true)
                    } else {
                        viewModel.startCall(audio: true, video: true)
                    }
                } label: {
                    Image(systemName: "video.fill")
                }
                .disabled(viewModel.activeCall != nil || viewModel.activeSfuCall != nil)
            }
        }
        .sheet(isPresented: $viewModel.membersPanelOpen) {
            MembersSheet(viewModel: viewModel)
        }
        .task {
            // Continuous foreground polling. SwiftUI's `.task`
            // ties the lifecycle to the view: the loop starts on
            // appear + cancels automatically on disappear. Mirror
            // of the Compose `LaunchedEffect(Unit) { while
            // (isActive) }` pattern from Android 1.15e7.
            while !Task.isCancelled {
                await viewModel.pollOnceLongPoll()
                await Task.yield()
            }
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

    private var headline: String {
        // For 1.14f3 we surface the first 16 chars of the hex
        // group id as the screen title. 1.14f4 will populate
        // the per-thread peer username (from
        // `AcceptedContact.request.fromPubkey`) — needs a
        // reverse-lookup keyed by pubkey, which lands when the
        // discovery side gains the inverse-of-OPRF endpoint.
        let prefix = viewModel.groupIdHex.prefix(16)
        return "Chat • \(prefix)"
    }

    @ViewBuilder
    private var messagesList: some View {
        ScrollViewReader { scrollProxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 8) {
                    if viewModel.messages.isEmpty {
                        emptyHint
                    } else {
                        ForEach(viewModel.messages) { message in
                            MessageRow(
                                message: message,
                                ownLeaf: viewModel.ownLeaf,
                                // ADR-0024 — "Sent" / "Read" check on
                                // our own messages, derived from the
                                // other members' read watermarks.
                                readState: viewModel.readState(of: message),
                                onPreview: openAttachmentPreview,
                                onEdit: { viewModel.beginEdit(message) },
                                onDelete: { viewModel.deleteMessage(message) },
                                onReact: { emoji in
                                    viewModel.react(to: message, emoji: emoji)
                                }
                            )
                            .id(message.messageId)
                        }
                    }
                }
                .padding(16)
            }
            .onChange(of: viewModel.messages.count) { _, _ in
                // Scroll to the latest message whenever the list
                // grows. Mirror of the Android `rememberLazyListState`
                // + `LaunchedEffect(messages.size)` shape.
                if let last = viewModel.messages.last {
                    withAnimation {
                        scrollProxy.scrollTo(last.messageId, anchor: .bottom)
                    }
                }
            }
        }
    }

    @ViewBuilder
    private var emptyHint: some View {
        VStack(alignment: .leading, spacing: 8) {
            if viewModel.threadKnown {
                Text("Say hi 👋")
                    .font(.body)
                    .foregroundStyle(.secondary)
            } else {
                Text("Bootstrap pending")
                    .font(.body)
                    .foregroundStyle(.secondary)
                Text(
                    "The session is still rebuilding MLS state. " +
                    "If you just signed in, give it a moment; if it " +
                    "persists, the snapshot may be missing on this " +
                    "device — sending a message will surface a " +
                    "diagnostic error."
                )
                .font(.footnote)
                .foregroundStyle(.secondary)
            }
        }
    }

    /// G3-1-call-present — host for the active call. Observes
    /// the live `CallSession` (so state transitions drive the
    /// UI — the previous `callOverlay` read `call.state` without
    /// observing the session, so it could go stale). While the
    /// call is `ringing` (incoming, pre-accept) it shows a
    /// lightweight inline banner with Accept / Decline; once the
    /// call goes live (outgoing / connecting / connected /
    /// ending / done) it presents the full `CallScreen` over the
    /// chat as a `fullScreenCover`, driven from the session via
    /// `CallUiState.driveFrom`. The cover is torn down when the
    /// call clears (`clearFinishedCall` sets `activeCall = nil`,
    /// which removes this view from the chat body).
    private struct CallHostView: View {
        @ObservedObject var session: CallSession
        @ObservedObject var viewModel: ChatViewModel

        var body: some View {
            ringingBanner
                .fullScreenCover(
                    isPresented: Binding(
                        get: { Self.isLive(session.state) },
                        set: { _ in }
                    )
                ) {
                    CallScreen(session: session) {
                        viewModel.clearFinishedCall()
                    }
                }
        }

        @ViewBuilder
        private var ringingBanner: some View {
            if session.state == .ringing {
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Incoming call")
                            .font(.headline)
                            .foregroundStyle(.white)
                        Text("End-to-end encrypted")
                            .font(.caption2)
                            .foregroundStyle(.white.opacity(0.85))
                    }
                    Spacer()
                    HStack(spacing: 8) {
                        Button("Accept") { viewModel.acceptIncomingCall() }
                            .buttonStyle(.borderedProminent)
                            .tint(.white)
                            .foregroundStyle(.green)
                        Button("Decline", role: .destructive) {
                            viewModel.declineIncomingCall()
                        }
                        .buttonStyle(.bordered)
                        .tint(.white)
                    }
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 10)
                .background(Color.orange)
            }
        }

        /// `true` for any non-idle, non-ringing state — the
        /// full call screen owns the experience from the moment
        /// an outgoing call is placed or an incoming one is
        /// accepted, through the terminal `done` (which shows
        /// the "Call ended" overlay until it auto-dismisses).
        private static func isLive(_ s: CallSession.WebCallState) -> Bool {
            switch s {
            case .outgoing, .connecting, .connected, .ending, .done:
                return true
            case .idle, .ringing:
                return false
            }
        }
    }

    @ViewBuilder
    private var composer: some View {
        VStack(spacing: 8) {
            if let err = viewModel.sendError {
                Text(err)
                    .font(.footnote)
                    .foregroundStyle(.red)
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
            HStack(spacing: 8) {
                // Attach button — opens the Photo / File
                // confirmationDialog wired at the body level.
                Button {
                    attachActionSheetPresented = true
                } label: {
                    if viewModel.uploading {
                        ProgressView().controlSize(.small)
                    } else {
                        Image(systemName: "paperclip")
                    }
                }
                .buttonStyle(.bordered)
                .controlSize(.large)
                .disabled(viewModel.uploading || viewModel.sending)
                TextField("Message", text: $viewModel.draft, axis: .vertical)
                    .textFieldStyle(.roundedBorder)
                    .lineLimit(1...4)
                    .disabled(viewModel.sending)
                Button(action: viewModel.onSendClicked) {
                    if viewModel.sending {
                        ProgressView().controlSize(.small)
                    } else {
                        Image(systemName: "paperplane.fill")
                    }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .disabled(
                    viewModel.sending || viewModel.uploading
                    || (
                        viewModel.draft.trimmingCharacters(
                            in: .whitespacesAndNewlines
                        ).isEmpty
                        && viewModel.pendingAttachments.isEmpty
                    )
                )
            }
        }
        .padding(12)
        .background(Color(.systemBackground))
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

private struct MessageRow: View {
    let message: ChatMessage
    /// ADR-0023 — the local member's MLS leaf, used to highlight
    /// reaction chips this user contributed.
    let ownLeaf: UInt32?
    /// ADR-0024 — read state for our OWN outgoing messages
    /// ("Sent" / "Read"). `nil` for incoming / system rows and
    /// pre-ack outgoing rows (no check rendered).
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

    /// ADR-0023 — emoji palette for the quick reaction picker.
    private static let reactionPalette = ["👍", "❤️", "😂", "😮", "😢", "🙏"]

    /// Edit / Delete are offered only on our own outgoing
    /// messages that carry a server-assigned id and are not
    /// already deleted (the receive-side author gate is the real
    /// enforcement — this is just the affordance).
    private var canEditOrDelete: Bool {
        message.direction == .outgoing
            && message.messageId > 0
            && !message.deleted
    }

    /// React is offered on any non-system message with an id.
    private var canReact: Bool {
        message.direction != .system
            && message.messageId > 0
            && !message.deleted
    }

    var body: some View {
        switch message.direction {
        case .outgoing:
            HStack {
                Spacer(minLength: 48)
                bubbleColumn(alignment: .trailing) {
                    bubble
                        .background(Color.accentColor.opacity(0.9))
                        .foregroundStyle(.white)
                        .clipShape(RoundedRectangle(cornerRadius: 16))
                        .contextMenu { menuItems }
                    // ADR-0024 — "Sent" / "Read" receipt under our own
                    // bubble. Hidden on a deleted tombstone.
                    if !message.deleted { readReceiptCheck }
                }
            }
        case .incoming:
            HStack {
                bubbleColumn(alignment: .leading) {
                    bubble
                        .background(Color(.secondarySystemBackground))
                        .clipShape(RoundedRectangle(cornerRadius: 16))
                        .contextMenu { menuItems }
                }
                Spacer(minLength: 48)
            }
        case .system:
            HStack {
                Spacer()
                Text(message.text)
                    .font(.caption)
                    .foregroundStyle(.tertiary)
                Spacer()
            }
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

    @ViewBuilder
    private var bubble: some View {
        VStack(alignment: .leading, spacing: 6) {
            if message.deleted {
                // Muted tombstone — no original text / attachments.
                Label("message deleted", systemImage: "trash")
                    .font(.body.italic())
                    .foregroundStyle(.secondary)
            } else {
                if !message.text.isEmpty {
                    Text(message.text)
                        .font(.body)
                        .textSelection(.enabled)
                }
                if !message.attachments.isEmpty {
                    attachmentChips
                }
                if message.edited {
                    Text("(edited)")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .padding(.vertical, 8)
        .padding(.horizontal, 12)
    }

    @ViewBuilder
    private var attachmentChips: some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(
                Array(message.attachments.enumerated()),
                id: \.offset
            ) { _, ref in
                AttachmentChip(
                    ref: ref,
                    direction: message.direction,
                    onPreview: { onPreview(ref) }
                )
            }
        }
    }

    /// ADR-0023 — reaction chips under the bubble. Each chip is
    /// `emoji ×count`; tapping a chip the local user contributed
    /// toggles it back off (re-sends the same emoji op).
    @ViewBuilder
    private var reactionChips: some View {
        HStack(spacing: 4) {
            ForEach(message.reactionSummary(ownLeaf: ownLeaf)) { tally in
                Button {
                    onReact(tally.emoji)
                } label: {
                    HStack(spacing: 2) {
                        Text(tally.emoji).font(.caption)
                        if tally.count > 1 {
                            Text("\(tally.count)")
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                    }
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(
                        tally.reactedByMe
                            ? Color.accentColor.opacity(0.18)
                            : Color(.secondarySystemBackground)
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: 10)
                            .stroke(
                                tally.reactedByMe
                                    ? Color.accentColor.opacity(0.6)
                                    : Color.clear,
                                lineWidth: 1
                            )
                    )
                    .clipShape(RoundedRectangle(cornerRadius: 10))
                }
                .buttonStyle(.plain)
            }
        }
    }

    /// ADR-0024 — the "Sent" / "Read" receipt label under an own
    /// outgoing bubble. `nil` read state renders nothing (incoming,
    /// system, or pre-ack rows). "Sent" is a single check; "Read" is a
    /// filled double check, both with a tiny caption.
    @ViewBuilder
    private var readReceiptCheck: some View {
        switch readState {
        case .none:
            EmptyView()
        case .sent:
            HStack(spacing: 3) {
                Image(systemName: "checkmark")
                Text("Sent")
            }
            .font(.caption2)
            .foregroundStyle(.secondary)
        case .read:
            HStack(spacing: 3) {
                Image(systemName: "checkmark.circle.fill")
                Text("Read")
            }
            .font(.caption2)
            .foregroundStyle(Color.accentColor)
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
        let baseName: String = ref.name ?? "attachment-\(AttachmentBridge.hex(ref.blobId).prefix(8))"
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(baseName)
        do {
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
private struct MembersSheet: View {
    @ObservedObject var viewModel: ChatViewModel

    /// ADR-0045 (UX §6) — the `Leave group` confirmation. Local to the sheet:
    /// it is a presentation concern with no bearing on the request, and nothing
    /// outside this view may open it.
    @State private var leaveConfirmPresented = false

    private var trimmedInput: String {
        viewModel.addMemberInput.trimmingCharacters(in: .whitespacesAndNewlines)
    }

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
                                .disabled(viewModel.membershipBusy || trimmedInput.isEmpty)
                        }
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
                if viewModel.canManageMembers, !viewModel.pendingJoins.isEmpty {
                    Section("Join requests (\(viewModel.pendingJoins.count))") {
                        ForEach(viewModel.pendingJoins, id: \.self) { ref in
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(Self.shortFingerprint(ref))
                                        .font(.system(.footnote, design: .monospaced))
                                    // ADR-0028 OQ2 — which member's link this
                                    // request arrived on + whether they are still
                                    // in the roster (advisory).
                                    if let attr = viewModel.joinAttributions[ref] {
                                        Text(
                                            "via \(String(attr.issuerFingerprint.prefix(11)))"
                                                + (attr.issuerInRoster
                                                    ? " · member" : " · not a current member")
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
                                    viewModel.onRemoveMember(leafIndex: member.leafIndex)
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

    /// A prospective member has no roster row and no contact entry to resolve
    /// against yet, so the only honest label is a fingerprint of the signature
    /// key the request carries.
    private static func pendingAddLabel(_ request: PendingRequest) -> String {
        guard let b64 = request.addSignatureKeyB64,
              let bytes = base64Decode(b64) else { return "Pending member" }
        return "Member " + bytes.prefix(4).map { String(format: "%02x", $0) }.joined()
    }

    private var memberCountLabel: String {
        let n = viewModel.members.count
        return "\(n) member" + (n == 1 ? "" : "s")
    }

    /// ADR-0027 (G-3) — a short hex fingerprint of a joiner's signature key for
    /// the pending-request row (the full key has no human name yet — the joiner
    /// is a non-member). Mirrors the roster's fingerprint posture.
    private static func shortFingerprint(_ data: Data) -> String {
        data.prefix(8).map { String(format: "%02x", $0) }.joined()
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
