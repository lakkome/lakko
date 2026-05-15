// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore

/// State + dispatcher for the per-thread ``ChatScreen``.
///
/// Mirror of:
///
///   * `apps/web/src/chat/ChatRoute.tsx` (1.17f4b)
///   * `apps/android/.../ui/chat/ChatViewModel.kt` (1.15f4b)
///
/// Combines the ``ChatRepository``-side thread (looked up by
/// `groupIdHex`) with VM-local composer state (the current draft
/// input + the send-busy / send-error flags) into a single set of
/// `@Published` slots SwiftUI observes.
///
/// Continuous foreground polling lives on the screen via
/// `.task { while !Task.isCancelled { … } }`, same pattern as
/// 1.14e5's contacts inbox loop.
@MainActor
public final class ChatViewModel: ObservableObject {

    // MARK: - Constructor inputs

    public let groupIdHex: String

    // MARK: - Form fields

    @Published public var draft: String = ""

    /// Phase 1.19c3 — attachments staged in the composer. The
    /// user picks photo / file; each call to
    /// ``onAttachmentPicked(bytes:mime:name:)`` encrypts +
    /// uploads to storage immediately so the ref is ready by
    /// the time Send is tapped. Cleared on successful Send, on
    /// per-chip remove, and on screen dismount.
    @Published public private(set) var pendingAttachments: [AttachmentRef] = []

    // MARK: - Ephemeral status

    @Published public private(set) var sending: Bool = false
    @Published public private(set) var sendError: String?
    @Published public private(set) var refreshing: Bool = false
    /// `true` while one or more picked items are being
    /// encrypted + uploaded. The composer disables Send and the
    /// picker buttons until it flips back.
    @Published public private(set) var uploading: Bool = false

    /// Polish 1.x — per-pick upload-progress detail. `nil` when
    /// no upload is in flight; otherwise carries the file name
    /// + which stage of the encrypt / upload pipeline the
    /// in-flight pick is at, so the composer tray can render a
    /// specific "Uploading hello.png…" caption instead of a
    /// generic "Uploading…". The user picks one file at a time
    /// (Apple's photo picker UX is single-image-per-pick by
    /// default in the ChatScreen), so no file-index ornament —
    /// the index is always 1.
    @Published public private(set) var uploadStage: UploadStage?

    /// Stage of a single pick's encrypt+upload pipeline. Mirror
    /// of the web client's `uploadProgress.stage` discriminator.
    public enum UploadStage: Equatable {
        case encrypting(fileName: String)
        case uploading(fileName: String)
    }

    // MARK: - Mirror of the repository's per-thread state

    /// The messages list for this group's thread. Reactive — every
    /// repository `state` update lands here via Combine
    /// subscription.
    @Published public private(set) var messages: [ChatMessage] = []
    /// `false` when the repository has no thread for this groupIdHex
    /// — e.g. immediately after process restart before snapshot
    /// rehydration has rebuilt the per-group handles. The UI still
    /// renders the composer (sendMessage surfaces a clear error if
    /// the handle is missing).
    @Published public private(set) var threadKnown: Bool = false

    // MARK: - Wiring

    private let repository: ChatRepository
    private var cancellables: Set<AnyCancellable> = []

    public init(repository: ChatRepository, groupIdHex: String) {
        self.repository = repository
        self.groupIdHex = groupIdHex
        sync(state: repository.state)
        repository.$state
            .receive(on: RunLoop.main)
            .sink { [weak self] newState in
                self?.sync(state: newState)
            }
            .store(in: &cancellables)
    }

    private func sync(state: ChatRepositoryState) {
        let thread = state.threads.first { $0.groupIdHex == groupIdHex }
        self.messages = thread?.messages ?? []
        self.threadKnown = thread != nil
    }

    // MARK: - Actions

    public func onSendClicked() {
        guard !sending else { return }
        let trimmed = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        let stagedAttachments = pendingAttachments
        // 1.19c3: attachment-only messages are valid (empty
        // text + ≥1 attachment); pre-1.19 text-only messages
        // still valid (≥1 char text + zero attachments). The
        // only invalid input is the all-empty composer.
        guard !trimmed.isEmpty || !stagedAttachments.isEmpty else { return }

        sending = true
        sendError = nil
        Task {
            do {
                try await repository.sendMessage(
                    groupIdHex: groupIdHex,
                    plaintext: trimmed,
                    attachments: stagedAttachments
                )
                sending = false
                sendError = nil
                draft = ""
                pendingAttachments = []
            } catch {
                sending = false
                // 1.19c3: leave `pendingAttachments` populated
                // on send failure so the user can retry without
                // re-picking the same files.
                sendError = (error as? LocalizedError)?.errorDescription
                    ?? "Send failed."
            }
        }
    }

    /// Drive the encrypt + upload flow for one picked item.
    /// Called by the screen layer after a photo / document
    /// picker resolves to raw bytes + MIME + filename.
    /// Sequential — one picked item at a time, called from the
    /// screen's `Task { … }` per pick — keeps the upload-state
    /// flag honest.
    public func onAttachmentPicked(
        bytes: Data,
        mime: String?,
        name: String?
    ) async {
        uploading = true
        sendError = nil
        // Drive `uploadStage` immediately so the composer tray
        // doesn't show the generic spinner for the brief encrypt
        // window. `AttachmentBridge.encryptAndUpload` flips the
        // stage to `.uploading` once it reaches the PUT path.
        let displayName = (name?.isEmpty ?? true) ? "attachment" : name!
        uploadStage = .encrypting(fileName: displayName)
        defer {
            uploading = false
            uploadStage = nil
        }
        do {
            let ref = try await AttachmentBridge.encryptAndUpload(
                bytes: bytes,
                mime: mime,
                name: name,
                onStage: { [weak self] stage in
                    // Hop back to MainActor for the published
                    // mutation — `onStage` may run on whatever
                    // thread the URLSession completion lands on.
                    Task { @MainActor in
                        switch stage {
                        case .encrypting:
                            self?.uploadStage = .encrypting(fileName: displayName)
                        case .uploading:
                            self?.uploadStage = .uploading(fileName: displayName)
                        }
                    }
                }
            )
            pendingAttachments.append(ref)
        } catch let err as AttachmentBridgeError {
            sendError = err.errorDescription
        } catch {
            sendError = "Attachment failed: \(error)"
        }
    }

    /// Drop a staged attachment from the pending tray. Index
    /// matches the order in ``pendingAttachments``.
    public func onRemovePendingAttachment(at index: Int) {
        guard pendingAttachments.indices.contains(index) else { return }
        pendingAttachments.remove(at: index)
    }

    public func onRefreshClicked() {
        refreshing = true
        Task {
            defer { refreshing = false }
            do {
                try await repository.pollGroup(groupIdHex: groupIdHex)
            } catch {
                // best-effort
            }
        }
    }

    /// One long-poll tick — used by ``ChatScreen``'s
    /// `.task { … }` modifier in a `while !Task.isCancelled` loop.
    /// Silent (does NOT flip `refreshing`) so the spinner doesn't
    /// stay on for the full 30s long-poll window.
    public func pollOnceLongPoll(waitSecs: Int = Self.defaultLongPollSecs) async {
        do {
            try await repository.pollGroup(
                groupIdHex: groupIdHex,
                waitSecs: waitSecs
            )
        } catch {
            // best-effort
        }
    }

    /// DS long-poll budget — same value as
    /// ``ContactsViewModel/defaultLongPollSecs``.
    public static let defaultLongPollSecs: Int = 30
}
