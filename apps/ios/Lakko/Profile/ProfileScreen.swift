// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import PhotosUI
import SwiftUI
import UIKit

/// Settings > Profile (UX `docs/ux/profile-screen.md`): the avatar, the name
/// and the username, each edited in place, and Save. One centred column.
struct ProfileScreen: View {
    @StateObject private var model: ProfileEditModel
    @StateObject private var proof: FreshAuthProofModel
    @ObservedObject private var l10n = Localization.shared
    @Environment(\.dismiss) private var dismiss

    /// The own account's id, hashed for the initials tint only.
    let tintIdentity: String

    @State private var menuOpen = false
    @State private var libraryOpen = false
    @State private var pickedItem: PhotosPickerItem?
    @State private var cropInput: AvatarCropInput?
    @State private var cameraOpen = false
    @State private var pickError: UiText?
    @FocusState private var focus: Field?

    enum Field: Hashable { case name, username }

    init(
        model: @autoclosure @escaping () -> ProfileEditModel,
        proof: @autoclosure @escaping () -> FreshAuthProofModel,
        tintIdentity: String
    ) {
        self._model = StateObject(wrappedValue: model())
        self._proof = StateObject(wrappedValue: proof())
        self.tintIdentity = tintIdentity
    }

    var body: some View {
        ScrollView {
            VStack(spacing: 20) {
                avatar
                    .padding(.top, 24)
                nameRow
                usernameRow
                saveBlock
            }
            .frame(maxWidth: 480)
            .padding(.horizontal, 16)
            .frame(maxWidth: .infinity)
        }
        .background(Color.lakkoBackground)
        .navigationTitle(l10n.t("settings.section.profile"))
        .navigationBarTitleDisplayMode(.inline)
        // Section 6: while something is pending the system back (and its
        // swipe) gives way to one that asks first.
        .navigationBarBackButtonHidden(model.hasPending || model.saving)
        .toolbar {
            if model.hasPending || model.saving {
                ToolbarItem(placement: .navigationBarLeading) {
                    Button {
                        if model.requestLeave({ dismiss() }) { dismiss() }
                    } label: {
                        LkIconView(.chevronBack, size: 24)
                    }
                    .accessibilityLabel(l10n.t("common.back"))
                }
            }
        }
        .onAppear {
            ProfileLeaveGuard.shared.active = model
        }
        .onDisappear {
            if ProfileLeaveGuard.shared.active === model { ProfileLeaveGuard.shared.active = nil }
        }
        .task {
            await proof.opened()
            // ADR-0076 — a rename started earlier is finished here first.
            await model.finishOwedRename()
        }
        .onChange(of: focus) { old, _ in
            // Leaving a field closes it when its text is valid (sections 3, 4).
            if old == .name { model.closeName() }
            if old == .username { model.closeUsername() }
        }
        .sheet(isPresented: $menuOpen) { photoMenu }
        .photosPicker(
            isPresented: $libraryOpen, selection: $pickedItem,
            matching: .any(of: [.images, .videos]), preferredItemEncoding: .current)
        .onChange(of: pickedItem) { _, item in
            guard let item else { return }
            pickedItem = nil
            Task { await loadPicked(item) }
        }
        .fullScreenCover(item: $cropInput) { input in
            AvatarCropScreen(
                input: input,
                onUse: { media in
                    cropInput = nil
                    model.usePhoto(media)
                },
                onCancel: { cropInput = nil })
        }
        .fullScreenCover(isPresented: $cameraOpen) {
            AvatarCameraScreen(
                onCaptured: { input in
                    cameraOpen = false
                    // The crop opens once the camera cover is gone.
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) { cropInput = input }
                },
                onClose: { cameraOpen = false })
        }
        .overlay {
            if model.leaveRequested {
                unsavedDialog
            } else if model.needsProof {
                proofDialog
            }
        }
    }

    // MARK: - Avatar (section 2)

    private var avatar: some View {
        ZStack(alignment: .bottomTrailing) {
            avatarPicture
                .frame(width: 112, height: 112)
            Button { menuOpen = true } label: {
                LkIconView(.pencil, size: 14, relativeTo: nil)
                    .foregroundStyle(Color.lakkoAccentForeground)
                    .frame(width: 32, height: 32)
                    .background(Circle().fill(Color.lakkoAccent))
                    .overlay(Circle().stroke(Color.lakkoBackground, lineWidth: 2))
            }
            .buttonStyle(.plain)
            .disabled(model.saving)
            .accessibilityLabel(l10n.t("profile.avatar.change"))
        }
    }

    @ViewBuilder
    private var avatarPicture: some View {
        let initialsName = model.shownName ?? model.shownUsername ?? ""
        switch model.pending.photo {
        case .set(let media):
            PendingAvatarPicture(media: media, size: 112)
        case .removed:
            InitialsAvatar(name: initialsName, identity: tintIdentity, size: 112, placeholderIcon: .personFill)
        case .unchanged:
            InitialsAvatar(
                name: initialsName, identity: tintIdentity, size: 112, placeholderIcon: .personFill,
                photo: model.saved.avatar == nil ? nil : .own, playsVideo: true)
        }
    }

    private var photoMenu: some View {
        VStack(spacing: 0) {
            menuRow(l10n.t("profile.avatar.menu.library"), icon: .photoStack) {
                menuOpen = false
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.35) { libraryOpen = true }
            }
            if AvatarCameraScreen.hasCamera {
                Divider()
                menuRow(l10n.t("profile.avatar.menu.camera"), icon: .camera) {
                    menuOpen = false
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.35) { cameraOpen = true }
                }
            }
            if model.showsPhoto {
                Divider()
                menuRow(l10n.t("profile.avatar.menu.remove"), icon: .trash, destructive: true) {
                    menuOpen = false
                    model.removePhoto()
                }
            }
        }
        .padding(.vertical, 8)
        .presentationDetents([.height(model.showsPhoto ? 200 : 140)])
        .presentationDragIndicator(.visible)
    }

    private func menuRow(
        _ title: String, icon: LkIcon, destructive: Bool = false, action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack(spacing: 12) {
                LkIconView(icon, size: 20).frame(width: 24)
                Text(title)
                Spacer()
            }
            .font(.body)
            .foregroundStyle(destructive ? Color.lakkoDanger : Color.lakkoText)
            .padding(.horizontal, 20)
            .frame(minHeight: 52)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    // MARK: - Name (section 3)

    @ViewBuilder
    private var nameRow: some View {
        if model.nameEdit.isOpen {
            VStack(spacing: 6) {
                TextField(l10n.t("settings.profile.name.label"), text: $model.nameEdit.text)
                    .font(.title2.weight(.semibold))
                    .multilineTextAlignment(.center)
                    .textInputAutocapitalization(.words)
                    .submitLabel(.done)
                    .focused($focus, equals: .name)
                    .onSubmit { model.closeName(); if !model.nameEdit.isOpen { focus = nil } }
                    .onChange(of: model.nameEdit.text) { _, _ in model.nameTextChanged() }
                    .onKeyPress(.escape) {
                        model.revertName()
                        focus = nil
                        return .handled
                    }
                    .padding(.vertical, 8)
                    .padding(.horizontal, 12)
                    .background(RoundedRectangle(cornerRadius: LakkoRadius.control).fill(Color.lakkoSurface))
                if let reason = model.nameReason {
                    Text(reason).font(.footnote).foregroundStyle(Color.lakkoDanger)
                }
            }
        } else {
            Button(action: openName) {
                HStack(spacing: 8) {
                    if let name = model.shownName {
                        Text(name)
                            .font(.title2.weight(.semibold))
                            .foregroundStyle(Color.lakkoText)
                    } else {
                        Text(l10n.t("profile.name.add"))
                            .font(.title2.weight(.semibold))
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    LkIconView(.pencil, size: 20, label: l10n.t("profile.name.change"))
                        .foregroundStyle(Color.lakkoAccent)
                }
                .multilineTextAlignment(.center)
            }
            .buttonStyle(.plain)
            .disabled(model.saving)
        }
    }

    private func openName() {
        model.openName()
        focus = .name
        // The current text is selected (section 3).
        DispatchQueue.main.async {
            UIApplication.shared.sendAction(#selector(UIResponder.selectAll(_:)), to: nil, from: nil, for: nil)
        }
    }

    // MARK: - Username (section 4)

    @ViewBuilder
    private var usernameRow: some View {
        if model.saved.username == nil {
            Text(l10n.t("profile.username.none"))
                .font(.body)
                .foregroundStyle(Color.lakkoTextMuted)
        } else if model.usernameEdit.isOpen {
            VStack(spacing: 6) {
                HStack(spacing: 2) {
                    Text("@").foregroundStyle(Color.lakkoTextMuted)
                    TextField(l10n.t("auth.username.label"), text: $model.usernameEdit.text)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .textContentType(.username)
                        .submitLabel(.done)
                        .focused($focus, equals: .username)
                        .onSubmit { model.closeUsername(); if !model.usernameEdit.isOpen { focus = nil } }
                        .onChange(of: model.usernameEdit.text) { _, _ in model.usernameTextChanged() }
                        .onKeyPress(.escape) {
                            model.revertUsername()
                            focus = nil
                            return .handled
                        }
                }
                .padding(.vertical, 8)
                .padding(.horizontal, 12)
                .background(RoundedRectangle(cornerRadius: LakkoRadius.control).fill(Color.lakkoSurface))
                if let reason = model.usernameReason {
                    Text(reason).font(.footnote).foregroundStyle(Color.lakkoDanger)
                }
                if model.renameTurnsOffRecoveryEmail {
                    Text(l10n.t("recovery_email.username_change"))
                        .font(.footnote).foregroundStyle(Color.lakkoDanger)
                        .multilineTextAlignment(.center)
                }
            }
        } else {
            Button {
                model.openUsername()
                if model.usernameEdit.isOpen { focus = .username }
            } label: {
                HStack(spacing: 8) {
                    usernameLabel
                    LkIconView(.pencil, size: 20, label: l10n.t("profile.username.change"))
                        .foregroundStyle(Color.lakkoAccent)
                }
            }
            .buttonStyle(.plain)
            .disabled(model.saving)
        }
    }

    private var usernameLabel: some View {
        Text("@\(model.shownUsername ?? "")")
            .font(.body)
            .foregroundStyle(Color.lakkoTextMuted)
    }

    // MARK: - Save (section 5)

    private var saveBlock: some View {
        VStack(spacing: 8) {
            AuthPrimaryButton(
                title: l10n.t("common.save"),
                busy: model.saving,
                busyTitle: l10n.t("profile.save.saving"),
                disabled: !model.canSave,
                action: {
                    focus = nil
                    model.save()
                })
            if let notice = model.notice {
                Text(notice.text).font(.footnote).foregroundStyle(Color.lakkoTextMuted)
                    .multilineTextAlignment(.center)
            }
            if let error = model.error ?? pickError {
                Text(error.text).font(.footnote).foregroundStyle(Color.lakkoDanger)
                    .multilineTextAlignment(.center)
            }
        }
        .padding(.top, 8)
    }

    // MARK: - Library pick

    private func loadPicked(_ item: PhotosPickerItem) async {
        pickError = nil
        let isVideo = item.supportedContentTypes.contains { $0.conforms(to: .movie) }
        if isVideo {
            guard let movie = try? await item.loadTransferable(type: PickedMovie.self),
                  let source = await AvatarVideoSource.load(movie.url) else {
                pickError = .key("profile.video.unusable")
                return
            }
            cropInput = .video(source)
        } else {
            guard let data = try? await item.loadTransferable(type: Data.self),
                  let image = UIImage(data: data), let cg = AvatarJPEG.upright(image) else {
                pickError = .key("profile.save.error.generic")
                return
            }
            cropInput = .photo(cg)
        }
    }

    // MARK: - Dialogs (in-app, never the system's)

    private var unsavedDialog: some View {
        LakkoDialog(onOutside: { model.answerLeave(.cancel) }) {
            Text(l10n.t("profile.unsaved.title"))
                .font(.headline).foregroundStyle(Color.lakkoText)
            Text(l10n.t("profile.unsaved.body"))
                .font(.subheadline).foregroundStyle(Color.lakkoTextMuted)
                .multilineTextAlignment(.center)
            AuthPrimaryButton(title: l10n.t("profile.unsaved.save")) { model.answerLeave(.save) }
            AuthSecondaryButton(title: l10n.t("profile.unsaved.discard")) { model.answerLeave(.discard) }
            Button(l10n.t("profile.unsaved.cancel")) { model.answerLeave(.cancel) }
                .foregroundStyle(Color.lakkoAccent)
                .frame(minHeight: 44)
        }
    }

    private var proofDialog: some View {
        LakkoDialog(onOutside: { cancelProof() }) {
            Text(l10n.t("profile.username.change"))
                .font(.headline).foregroundStyle(Color.lakkoText)
            FreshAuthBlock(model: proof, onSubmit: confirmProof)
            AuthPrimaryButton(
                title: l10n.t("common.continue"),
                busy: proof.layout.working,
                disabled: !proof.layout.ready && !proof.layout.mainOpensPasskey,
                action: confirmProof)
            Button(l10n.t("common.cancel")) { cancelProof() }
                .foregroundStyle(Color.lakkoAccent)
                .frame(minHeight: 44)
        }
    }

    private func confirmProof() {
        Task {
            guard await proof.ensure() else { return }
            model.proofConfirmed()
        }
    }

    private func cancelProof() {
        proof.closed()
        model.proofCancelled()
    }
}

/// A pending picture before Save (section 2.4): the video plays muted and
/// looped, else the still.
struct PendingAvatarPicture: View {
    let media: PendingAvatarMedia
    let size: CGFloat

    var body: some View {
        Group {
            if let url = media.videoFile {
                LoopingVideoView(url: url)
            } else if let image = UIImage(data: media.stillJPEG) {
                Image(uiImage: image).resizable().scaledToFill()
            } else {
                Color.lakkoSurfaceSunken
            }
        }
        .frame(width: size, height: size)
        .clipShape(Circle())
        .accessibilityHidden(true)
    }
}
