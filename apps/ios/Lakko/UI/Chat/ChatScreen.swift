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
            messagesList
            Divider()
            composer
        }
        .onAppear {
            // Phase 2a3e2b — wire the cross-device sync hook
            // on first mount. Idempotent: subsequent calls
            // overwrite the weak reference but the existing
            // Combine subscriptions stay intact.
            viewModel.attachSync(deviceGroupSync)
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
                                onPreview: openAttachmentPreview
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
        guard let data = try? await item.loadTransferable(type: Data.self),
              let bytes = data
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
    /// 1.19c3 — bubble taps on attachment chips call into the
    /// screen's preview / share helpers. `onPreview` opens the
    /// inline image sheet; the `ShareLink` is wired inline.
    let onPreview: (AttachmentRef) -> Void

    var body: some View {
        switch message.direction {
        case .outgoing:
            HStack {
                Spacer(minLength: 48)
                bubble
                    .background(Color.accentColor.opacity(0.9))
                    .foregroundStyle(.white)
                    .clipShape(RoundedRectangle(cornerRadius: 16))
            }
        case .incoming:
            HStack {
                bubble
                    .background(Color(.secondarySystemBackground))
                    .clipShape(RoundedRectangle(cornerRadius: 16))
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

    @ViewBuilder
    private var bubble: some View {
        VStack(alignment: .leading, spacing: 6) {
            if !message.text.isEmpty {
                Text(message.text)
                    .font(.body)
                    .textSelection(.enabled)
            }
            if !message.attachments.isEmpty {
                attachmentChips
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
