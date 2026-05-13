// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation

/// State + dispatcher for ``ContactsScreen``.
///
/// Mirror of:
///
///   * `apps/web/src/contacts/ContactsRoute.tsx` (1.17e6)
///   * `apps/android/.../ui/contacts/ContactsViewModel.kt`
///     (1.15e6 + 1.15e7's `pollOnceLongPoll`)
///
/// **Phase 1.14e4 scope**: search/send + refresh + accept/decline +
/// requires-reauth panel. Invite generate/revoke surface (1.15e10
/// on Android) is wired here in 1.14e7. Continuous polling
/// `pollOnceLongPoll` lands in 1.14e5.
///
/// `@MainActor` because every mutation writes to a `@Published`
/// slot that SwiftUI reads on the main runloop. The repository's
/// own `@Published var state` is mirrored here through a Combine
/// subscription so the view can observe one ObservableObject; the
/// VM-owned form fields (`usernameInput`, `introInput`) round out
/// the picture.
@MainActor
public final class ContactsViewModel: ObservableObject {

    // MARK: - Form fields

    @Published public var usernameInput: String = ""
    @Published public var introInput: String = ""

    // MARK: - Ephemeral status

    @Published public private(set) var sending: Bool = false
    @Published public private(set) var sendError: String?
    @Published public private(set) var generatingInvite: Bool = false
    @Published public private(set) var inviteError: String?
    /// The last invite link the user generated this session.
    /// Surfaced as a copyable / shareable bottom-sheet on the
    /// Contacts screen; cleared once the user dismisses or
    /// generates another one.
    @Published public var lastGeneratedInvite: GeneratedInvite?

    // MARK: - Mirror of the repository's state

    /// Mirrored from ``ContactsRepository/state``. SwiftUI's
    /// `@ObservedObject` traverses one ObservableObject at a time;
    /// having the VM expose the repository's snapshot here means
    /// the view can subscribe to just the VM and still see every
    /// repository update.
    @Published public private(set) var repositoryState: ContactsState = ContactsState()

    // MARK: - Wiring

    private let repository: ContactsRepository
    private var cancellables: Set<AnyCancellable> = []

    public init(repository: ContactsRepository) {
        self.repository = repository
        self.repositoryState = repository.state
        repository.$state
            .receive(on: RunLoop.main)
            .sink { [weak self] newState in
                self?.repositoryState = newState
            }
            .store(in: &cancellables)
    }

    // MARK: - Actions

    /// Send a contact request. Surfaces success by clearing the
    /// form; failures land in ``sendError``.
    public func onSendClicked() {
        guard !sending else { return }
        let username = usernameInput.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !username.isEmpty else {
            sendError = "Type a username first."
            return
        }

        sending = true
        sendError = nil
        let intro = introInput

        Task {
            do {
                _ = try await repository.sendContactRequest(
                    recipientUsername: username,
                    introMessage: intro
                )
                sending = false
                sendError = nil
                usernameInput = ""
                introInput = ""
            } catch let err as ContactsError {
                sending = false
                sendError = userFacing(err)
            } catch {
                sending = false
                sendError = (error as? LocalizedError)?.errorDescription
                    ?? "Send failed."
            }
        }
    }

    public func onRefreshClicked() {
        Task { await repository.refreshInbox() }
    }

    // MARK: - Continuous foreground polling (1.14e5)

    /// One long-poll tick — used by ``ContactsScreen``'s
    /// `.task { … }` modifier in a `while !Task.isCancelled` loop.
    /// Foreground-only continuous polling; BGTaskScheduler-driven
    /// background fetch + APNs push land in 1.14e5b/c.
    ///
    /// `waitSecs` is passed straight to the DS long-poll budget;
    /// the server clamps to its own `MAX_WAIT_SECS`. 30 matches
    /// the canonical chat-app cadence + the Android side's
    /// `DEFAULT_LONG_POLL_SECS`.
    public func pollOnceLongPoll(waitSecs: Int = Self.defaultLongPollSecs) async {
        await repository.refreshInbox(waitSecs: waitSecs)
    }

    /// DS long-poll budget for the foreground-poll loop. Mirrors
    /// `ContactsViewModel.DEFAULT_LONG_POLL_SECS` on Android.
    public static let defaultLongPollSecs: Int = 30

    /// Accept a pending request. **1.14e4 ships local-state-only**
    /// — the MLS chat-side handshake lives in 1.14f when
    /// `ChatRepository` arrives. The Accept row gets a `nil`
    /// `groupIdHex` until then; the UI surfaces an "Open chat
    /// (coming in 1.14f)" hint.
    public func onAcceptClicked(messageId: Int64) {
        repository.acceptContactRequest(messageId: messageId, groupIdHex: nil)
    }

    public func onDeclineClicked(messageId: Int64) {
        repository.declineContactRequest(messageId: messageId)
    }

    // MARK: - Invite link actions (1.14e7)

    /// Generate a fresh single-use 7-day invite link. Surfaces the
    /// resulting ``GeneratedInvite`` in ``lastGeneratedInvite``;
    /// failures land in ``inviteError``.
    public func onGenerateInviteClicked() {
        guard !generatingInvite else { return }
        generatingInvite = true
        inviteError = nil
        Task {
            do {
                let invite = try await repository.generateInviteLink()
                generatingInvite = false
                inviteError = nil
                lastGeneratedInvite = invite
            } catch let err as ContactsError {
                generatingInvite = false
                inviteError = userFacing(err)
            } catch {
                generatingInvite = false
                inviteError = (error as? LocalizedError)?.errorDescription
                    ?? "Invite generation failed."
            }
        }
    }

    /// Revoke an outstanding invite by `blobId`. On success the
    /// row is dropped from ``ContactsState/outstandingInvites``
    /// automatically by ``ContactsRepository/revokeInviteLink(blobId:)``.
    public func onRevokeInviteClicked(blobId: Data) {
        Task {
            do {
                _ = try await repository.revokeInviteLink(blobId: blobId)
            } catch let err as ContactsError {
                inviteError = userFacing(err)
            } catch {
                inviteError = (error as? LocalizedError)?.errorDescription
                    ?? "Revoke failed."
            }
        }
    }

    /// Dismiss the "just generated this link" sheet without
    /// affecting the persistent invite list.
    public func dismissGeneratedInvite() {
        lastGeneratedInvite = nil
    }

    // MARK: - Error mapping

    private func userFacing(_ err: ContactsError) -> String {
        switch err {
        case .usernameUnknown(let normalised):
            return "@\(normalised) is not registered on Lakko."
        case .noIdentityBundle:
            return "That account has no identity bundle yet — ask them to open the app once."
        case .wrapped(let cause):
            return (cause as? LocalizedError)?.errorDescription
                ?? String(describing: cause)
        }
    }
}
