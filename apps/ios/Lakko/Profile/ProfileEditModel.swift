// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore

// UX `docs/ux/profile-screen.md` — the state model of Settings > Profile,
// pure where it can be (section 8): `saved` and `pending`, the two inline
// field edits, Save's enablement, the leave decision. The view model at the
// end drives the save order of section 5 over injected ports.

// MARK: - Values

/// A picked or captured photo or video after the crop screen: square, inside
/// the ADR-0084 bounds, not yet uploaded. `id` tells one pick from another.
struct PendingAvatarMedia: Equatable {
    let id: UUID
    /// The still: a square JPEG, at most 640 px and 256 KiB.
    let stillJPEG: Data
    let stillSide: Int
    /// The profile video (square H.264 MP4, no sound), or `nil` for a photo.
    let videoFile: URL?
    let videoSide: Int?
    let videoDurationMs: Int?

    init(
        id: UUID = UUID(), stillJPEG: Data, stillSide: Int,
        videoFile: URL? = nil, videoSide: Int? = nil, videoDurationMs: Int? = nil
    ) {
        self.id = id
        self.stillJPEG = stillJPEG
        self.stillSide = stillSide
        self.videoFile = videoFile
        self.videoSide = videoFile == nil ? nil : videoSide
        self.videoDurationMs = videoFile == nil ? nil : videoDurationMs
    }
}

/// One field's pending state: unchanged, or a new value.
enum ProfileChange<Value: Equatable>: Equatable {
    case unchanged
    case changed(Value)
}

/// What is published (section 8).
struct ProfileSaved: Equatable {
    var avatar: ProfileAvatar?
    var name: String?
    var username: String?
}

/// The photo's pending change: a new picture, or removing the current one.
enum PhotoChange: Equatable {
    case unchanged
    case set(PendingAvatarMedia)
    case removed
}

/// What the person changed and has not saved (section 8).
struct ProfilePending: Equatable {
    var photo: PhotoChange = .unchanged
    var name: ProfileChange<String?> = .unchanged
    var username: ProfileChange<String> = .unchanged

    var isEmpty: Bool { photo == .unchanged && name == .unchanged && username == .unchanged }

    /// A name, normalised (blank -> `nil`), as a pending change against
    /// `saved`: back to the saved value is no change.
    mutating func setName(_ normalized: String?, saved: ProfileSaved) {
        name = normalized == saved.name ? .unchanged : .changed(normalized)
    }

    mutating func setUsername(_ canonical: String, saved: ProfileSaved) {
        username = canonical == saved.username ? .unchanged : .changed(canonical)
    }

    mutating func setPhoto(_ media: PendingAvatarMedia) { photo = .set(media) }

    /// `Remove photo`: nothing to remove when nothing is saved, so dropping a
    /// pending new picture over no saved one is no change at all.
    mutating func removePhoto(saved: ProfileSaved) {
        photo = saved.avatar == nil ? .unchanged : .removed
    }

    /// Re-check every field against a saved state that moved (a save, another
    /// device): a pending value that now equals the saved one is no change.
    mutating func normalize(against saved: ProfileSaved) {
        if case .changed(let value) = name, value == saved.name { name = .unchanged }
        if case .changed(let value) = username, value == saved.username { username = .unchanged }
        if photo == .removed, saved.avatar == nil { photo = .unchanged }
    }

    /// Whether the photo shown is "set" (saved or pending): `Remove photo`
    /// is offered (section 2.1).
    func showsPhoto(saved: ProfileSaved) -> Bool {
        switch photo {
        case .set: return true
        case .removed: return false
        case .unchanged: return saved.avatar != nil
        }
    }
}

// MARK: - Inline field edit (sections 3 and 4)

/// One row turned into a text field: open with the current text, close on
/// leaving when the text is valid, stay open while it is not, revert on
/// Escape.
struct InlineFieldEdit: Equatable {
    private(set) var isOpen = false
    var text = ""
    private(set) var before = ""

    enum Closing: Equatable {
        /// Not open: nothing happened.
        case notOpen
        /// The text is invalid: the field stays open with its reason.
        case stayedOpen
        /// Closed; the row shows this text.
        case closed(String)
    }

    mutating func open(current: String) {
        isOpen = true
        text = current
        before = current
    }

    /// Leaving the field (focus moved, Return): closes only a valid text.
    mutating func close(isValid: (String) -> Bool) -> Closing {
        guard isOpen else { return .notOpen }
        guard isValid(text) else { return .stayedOpen }
        isOpen = false
        return .closed(text)
    }

    /// Escape: closes and returns what the row held before this edit.
    mutating func revert() -> String? {
        guard isOpen else { return nil }
        isOpen = false
        text = before
        return before
    }
}

// MARK: - Save and leave rules

enum ProfileSaveRule {
    /// Save (section 5): something differs from what is saved and every open
    /// field is valid; never while a save runs.
    static func canSave(pending: ProfilePending, openFieldsValid: Bool, saving: Bool) -> Bool {
        !saving && openFieldsValid && !pending.isEmpty
    }
}

/// Section 6 — what a navigation away from the screen does.
enum ProfileLeaveDecision: Equatable {
    /// Nothing pending: go.
    case leave
    /// Something pending: Lakko's own "Save changes?" dialog first.
    case ask
    /// A save is running: stay (the button says `Saving…`).
    case stay

    static func decide(hasPending: Bool, saving: Bool) -> ProfileLeaveDecision {
        if saving { return .stay }
        return hasPending ? .ask : .leave
    }
}

/// The three answers of the dialog.
enum ProfileUnsavedChoice: Equatable { case save, discard, cancel }

// MARK: - Ports

/// What a save needs from the rest of the app.
struct ProfileSavePorts {
    /// Encrypt and upload the still and the video; returns the refs.
    var upload: @MainActor (PendingAvatarMedia) async throws -> ProfileAvatar
    /// Change the username (ADR-0076); throws ``FreshAuthRequired`` when a
    /// proof is needed, ``UsernameRenameFailed`` on a refusal. Returns the
    /// canonical new name.
    var rename: @MainActor (String) async throws -> String
    /// Publish the profile (name and photo) and re-send it everywhere.
    var publish: @MainActor (_ name: String?, _ avatar: ProfileAvatar?) async throws -> Void
    /// A rename this device started is still owed (ADR-0076).
    var renameOwed: @MainActor () -> Bool = { false }
    /// Finish an owed rename with the proof held; `true` when still owed.
    var finishOwedRename: @MainActor () async -> Bool = { false }
    /// A rename would turn the recovery email off (this device does not hold
    /// the address): the warning the old rename screen showed.
    var renameTurnsOffRecoveryEmail: @MainActor () -> Bool = { false }
}

// MARK: - View model

/// Settings > Profile.
@MainActor
final class ProfileEditModel: ObservableObject {
    @Published private(set) var saved: ProfileSaved
    @Published private(set) var pending = ProfilePending()
    @Published var nameEdit = InlineFieldEdit()
    @Published var usernameEdit = InlineFieldEdit()
    @Published private(set) var saving = false
    @Published private(set) var notice: UiText?
    @Published private(set) var error: UiText?
    /// The proof block is asked for (section 5 step 2).
    @Published private(set) var needsProof = false
    /// The dialog of section 6 is up; holds what to do once it is answered.
    @Published private(set) var leaveRequested = false
    /// The proof asked for finishes an owed rename, not a save.
    @Published private(set) var proofFinishesOwedRename = false

    private let ports: ProfileSavePorts
    private var uploaded: (id: UUID, avatar: ProfileAvatar)?
    private var afterSave: (() -> Void)?
    private var savedWatch: AnyCancellable?
    private var noticeTask: Task<Void, Never>?

    init(saved: ProfileSaved, ports: ProfileSavePorts, savedChanges: AnyPublisher<ProfileSaved, Never>? = nil) {
        self.saved = saved
        self.ports = ports
        savedWatch = savedChanges?
            .receive(on: DispatchQueue.main)
            .sink { [weak self] next in self?.savedMoved(next) }
    }

    /// The saved state moved under the screen (another device, a rename
    /// finished elsewhere): what is pending stays, re-checked against it.
    func savedMoved(_ next: ProfileSaved) {
        guard !saving, next != saved else { return }
        saved = next
        pending.normalize(against: next)
    }

    // MARK: Derived

    var hasPending: Bool { !pending.isEmpty }

    var nameReason: String? { nameEdit.isOpen ? Self.nameReason(nameEdit.text) : nil }

    var usernameReason: String? { usernameEdit.isOpen ? Self.usernameReason(usernameEdit.text) : nil }

    var openFieldsValid: Bool { nameReason == nil && usernameReason == nil }

    var canSave: Bool {
        ProfileSaveRule.canSave(pending: pending, openFieldsValid: openFieldsValid, saving: saving)
    }

    /// The name the row shows: pending, else saved; `nil` -> `Add your name`.
    var shownName: String? {
        if case .changed(let value) = pending.name { return value }
        return saved.name
    }

    var shownUsername: String? {
        if case .changed(let value) = pending.username { return value }
        return saved.username
    }

    var showsPhoto: Bool { pending.showsPhoto(saved: saved) }

    static func nameReason(_ text: String) -> String? {
        do {
            _ = try checkDisplayName(text)
            return nil
        } catch {
            return (error as? LocalizedError)?.errorDescription
                ?? Localization.text("display_name.error.unknown")
        }
    }

    /// Like at signup; an empty field is not a username either.
    static func usernameReason(_ text: String) -> String? {
        switch checkUsername(text) {
        case .valid: return nil
        case .invalid(let reason): return reason.message
        case .empty: return UsernameRuleReason.tooShort.message
        }
    }

    // MARK: Name (section 3)

    func openName() {
        guard !saving else { return }
        nameEdit.open(current: shownName ?? "")
    }

    func nameTextChanged() {
        // A valid text is pending at once, so Save follows the typing.
        guard nameEdit.isOpen, Self.nameReason(nameEdit.text) == nil else { return }
        pending.setName((try? checkDisplayName(nameEdit.text)) ?? nil, saved: saved)
    }

    func closeName() {
        if case .closed(let text) = nameEdit.close(isValid: { Self.nameReason($0) == nil }) {
            pending.setName((try? checkDisplayName(text)) ?? nil, saved: saved)
        }
    }

    func revertName() {
        if let before = nameEdit.revert() {
            pending.setName((try? checkDisplayName(before)) ?? nil, saved: saved)
        }
    }

    // MARK: Username (section 4)

    func openUsername() {
        guard !saving, saved.username != nil else { return }
        usernameEdit.open(current: shownUsername ?? "")
    }

    func usernameTextChanged() {
        // Uppercase is shown lowercased.
        let lowered = usernameEdit.text.lowercased()
        if lowered != usernameEdit.text { usernameEdit.text = lowered }
        guard usernameEdit.isOpen, let canonical = checkUsername(lowered).canonical else { return }
        pending.setUsername(canonical, saved: saved)
    }

    func closeUsername() {
        if case .closed(let text) = usernameEdit.close(isValid: { Self.usernameReason($0) == nil }),
           let canonical = checkUsername(text).canonical {
            pending.setUsername(canonical, saved: saved)
        }
    }

    func revertUsername() {
        if let before = usernameEdit.revert(), let canonical = checkUsername(before).canonical {
            pending.setUsername(canonical, saved: saved)
        }
    }

    // MARK: Photo (section 2)

    func usePhoto(_ media: PendingAvatarMedia) {
        guard !saving else { return }
        pending.setPhoto(media)
        error = nil
    }

    func removePhoto() {
        guard !saving else { return }
        pending.removePhoto(saved: saved)
    }

    /// The pending picture, when one is pending.
    var pendingMedia: PendingAvatarMedia? {
        if case .set(let media) = pending.photo { return media }
        return nil
    }

    // MARK: Save (section 5)

    func save() {
        closeName()
        closeUsername()
        guard canSave else { return }
        Task { await runSave() }
    }

    /// The steps in order; each that succeeds is saved, the first that fails
    /// stops the rest, which stays pending.
    func runSave() async {
        guard !saving else { return }
        saving = true
        error = nil
        notice = nil
        defer { saving = false }

        // 1. The photo.
        var avatarAfter = saved.avatar
        switch pending.photo {
        case .unchanged: break
        case .removed: avatarAfter = nil
        case .set(let media):
            if let uploaded, uploaded.id == media.id {
                avatarAfter = uploaded.avatar
            } else {
                do {
                    let avatar = try await ports.upload(media)
                    uploaded = (media.id, avatar)
                    avatarAfter = avatar
                } catch AttachmentBridgeError.storageFull {
                    // UX storage-full — the server has no room for new files.
                    self.error = .key("profile.save.error.full")
                    afterSave = nil
                    return
                } catch {
                    self.error = .key("profile.save.error.upload")
                    afterSave = nil
                    return
                }
            }
        }

        // 2. The username.
        if case .changed(let name) = pending.username {
            do {
                let canonical = try await ports.rename(name)
                saved.username = canonical
                pending.username = .unchanged
            } catch is FreshAuthRequired {
                needsProof = true
                return
            } catch let refused as UsernameRenameFailed {
                afterSave = nil
                switch refused.failure {
                case .taken:
                    error = .key("profile.username.taken", texts: ["username": "@\(name)"])
                    usernameEdit.open(current: name)
                case .proofNeeded:
                    needsProof = true
                default:
                    error = .key("profile.save.error.generic")
                }
                return
            } catch {
                afterSave = nil
                self.error = .key("profile.save.error.generic")
                return
            }
        }

        // 3. The profile.
        if pending.name != .unchanged || pending.photo != .unchanged {
            let nameAfter: String?
            if case .changed(let value) = pending.name { nameAfter = value } else { nameAfter = saved.name }
            do {
                try await ports.publish(nameAfter, avatarAfter)
                saved.name = nameAfter
                saved.avatar = avatarAfter
                pending.name = .unchanged
                pending.photo = .unchanged
                uploaded = nil
            } catch {
                afterSave = nil
                self.error = .key("profile.save.error.generic")
                return
            }
        }
        pending.normalize(against: saved)
        showNotice()
        if let next = afterSave {
            afterSave = nil
            next()
        }
    }

    private func showNotice() {
        notice = .key("profile.save.done")
        noticeTask?.cancel()
        noticeTask = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 4_000_000_000)
            guard !Task.isCancelled else { return }
            self?.notice = nil
        }
    }

    /// The proof block confirmed: the save goes on from the username (or the
    /// owed rename is finished).
    func proofConfirmed() {
        needsProof = false
        if proofFinishesOwedRename {
            proofFinishesOwedRename = false
            Task { await finishOwedRename(askProof: false) }
            return
        }
        Task { await runSave() }
    }

    // MARK: Owed rename (what the old rename screen left to a landing)

    var renameTurnsOffRecoveryEmail: Bool { ports.renameTurnsOffRecoveryEmail() }

    /// On open: a rename started earlier and not finished is finished here;
    /// when its rollback needs a proof, the proof block opens for it.
    func finishOwedRename(askProof: Bool = true) async {
        guard ports.renameOwed(), !saving else { return }
        saving = true
        let stillOwed = await ports.finishOwedRename()
        saving = false
        guard stillOwed else { return }
        if askProof {
            proofFinishesOwedRename = true
            needsProof = true
        } else {
            error = .key("profile.save.error.generic")
        }
    }

    /// The proof block was cancelled: the save stops, the username stays
    /// pending, no error.
    func proofCancelled() {
        needsProof = false
        proofFinishesOwedRename = false
        afterSave = nil
    }

    // MARK: Leaving (section 6)

    /// A navigation away: `true` when it may go at once; otherwise the dialog
    /// opens and `proceed` runs after `Save` (when it succeeds) or `Don't
    /// save`.
    func requestLeave(_ proceed: @escaping () -> Void) -> Bool {
        switch ProfileLeaveDecision.decide(hasPending: hasPending, saving: saving) {
        case .leave: return true
        case .stay: return false
        case .ask:
            afterSave = proceed
            leaveRequested = true
            return false
        }
    }

    func answerLeave(_ choice: ProfileUnsavedChoice) {
        leaveRequested = false
        switch choice {
        case .cancel:
            afterSave = nil
        case .discard:
            discardAll()
            let next = afterSave
            afterSave = nil
            next?()
        case .save:
            closeName()
            closeUsername()
            if canSave {
                Task { await runSave() }
            } else if !hasPending {
                let next = afterSave
                afterSave = nil
                next?()
            } else {
                // An open field is invalid: the person stays with its reason.
                afterSave = nil
            }
        }
    }

    /// `Don't save`: every pending change goes.
    func discardAll() {
        pending = ProfilePending()
        _ = nameEdit.revert()
        _ = usernameEdit.revert()
        uploaded = nil
        error = nil
    }
}

// MARK: - The guard the app shell asks

/// Section 6 — the one place the app shell asks before it navigates away
/// from Settings > Profile (another tab, a path the shell replaces). The
/// screen registers its model while it is shown.
@MainActor
final class ProfileLeaveGuard {
    static let shared = ProfileLeaveGuard()

    weak var active: ProfileEditModel?

    /// `true`: navigate now. `false`: the dialog is up, and `proceed` runs
    /// after `Save` succeeds or `Don't save`.
    func allows(_ proceed: @escaping () -> Void) -> Bool {
        active?.requestLeave(proceed) ?? true
    }
}
