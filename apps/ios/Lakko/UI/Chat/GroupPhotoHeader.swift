// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import PhotosUI
import SwiftUI

/// `docs/ux/group-and-channel-privacy.md` section 4.5.2 — the group info
/// header: the 96 pt photo (or initials), and for whoever may set it the
/// profile photo's edit button, menu, crop and camera screens. There is no
/// Save: `Use` on the crop screen sends it.
/// ADR-0087 section 18 / UX 4.5.2 — what the photo header of an info screen
/// needs, for a group (``ChatViewModel``) and a channel
/// (``ChannelPhotoModel``) alike.
@MainActor
protocol ConversationPhotoModel: ObservableObject {
    var photoTitle: String { get }
    var photoIdHex: String { get }
    var photoIsGroup: Bool { get }
    var canSetPhoto: Bool { get }
    var currentPhoto: ProfileAvatar? { get }
    var photoStatus: ChatViewModel.GroupPhotoStatus { get set }
    var photoRemoveTitleKey: String { get }
    var photoRemoveBodyKey: String { get }
    var photoNotAllowedKey: String { get }
    func usePhoto(_ media: PendingAvatarMedia)
    func retryPhoto()
    func removePhoto()
}

extension ChatViewModel: ConversationPhotoModel {
    var photoTitle: String { conversationTitle }
    var photoIdHex: String { groupIdHex }
    var photoIsGroup: Bool { true }
    var canSetPhoto: Bool { canSetGroupPhoto }
    var currentPhoto: ProfileAvatar? { groupPhoto }
    var photoStatus: GroupPhotoStatus {
        get { groupPhotoStatus }
        set { groupPhotoStatus = newValue }
    }
    var photoRemoveTitleKey: String { "group_photo.remove.title" }
    var photoRemoveBodyKey: String { "group_photo.remove.body" }
    var photoNotAllowedKey: String { "group_photo.error.not_allowed" }
    func usePhoto(_ media: PendingAvatarMedia) { useGroupPhoto(media) }
    func retryPhoto() { retryGroupPhoto() }
    func removePhoto() { removeGroupPhoto() }
}

struct GroupPhotoHeader<Model: ConversationPhotoModel>: View {
    @ObservedObject var viewModel: Model
    @ObservedObject private var l10n = Localization.shared
    @ObservedObject private var images = AvatarImages.shared

    @State private var menuOpen = false
    @State private var libraryOpen = false
    @State private var pickedItem: PhotosPickerItem?
    @State private var cropInput: AvatarCropInput?
    @State private var cameraOpen = false
    @State private var removeConfirm = false

    private static var size: CGFloat { 96 }

    var body: some View {
        VStack(spacing: 8) {
            ZStack(alignment: .bottomTrailing) {
                picture
                    .overlay {
                        if viewModel.photoStatus == .uploading {
                            Circle()
                                .stroke(Color.lakkoAccent, style: StrokeStyle(lineWidth: 3, dash: [6, 4]))
                                .frame(width: Self.size + 6, height: Self.size + 6)
                        }
                    }
                    .onTapGesture { if viewModel.canSetPhoto { menuOpen = true } }
                if viewModel.canSetPhoto {
                    Button { menuOpen = true } label: {
                        LkIconView(.pencil, size: 14, relativeTo: nil)
                            .foregroundStyle(Color.lakkoAccentForeground)
                            .frame(width: 32, height: 32)
                            .background(Circle().fill(Color.lakkoAccent))
                            .overlay(Circle().stroke(Color.lakkoBackground, lineWidth: 2))
                    }
                    .buttonStyle(.plain)
                    .disabled(viewModel.photoStatus == .uploading)
                    .accessibilityLabel(l10n.t("profile.avatar.change"))
                }
            }
            statusLine
        }
        .frame(maxWidth: .infinity)
        .sheet(isPresented: $menuOpen) { menu }
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
                    viewModel.usePhoto(media)
                },
                onCancel: { cropInput = nil })
        }
        .fullScreenCover(isPresented: $cameraOpen) {
            AvatarCameraScreen(
                onCaptured: { input in
                    cameraOpen = false
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) { cropInput = input }
                },
                onClose: { cameraOpen = false })
        }
        .lakkoDialog(
            isPresented: $removeConfirm,
            title: l10n.t(viewModel.photoRemoveTitleKey),
            message: l10n.t(viewModel.photoRemoveBodyKey)
        ) {
            [.danger(l10n.t("group_photo.remove.confirm")) { viewModel.removePhoto() }, .cancel()]
        }
    }

    private var picture: some View {
        InitialsAvatar(
            name: viewModel.photoTitle, identity: viewModel.photoIdHex, size: Self.size,
            isGroup: viewModel.photoIsGroup, placeholderIcon: viewModel.photoIsGroup ? nil : .megaphoneFill,
            photo: .group(viewModel.photoIdHex), playsVideo: true)
    }

    @ViewBuilder
    private var statusLine: some View {
        switch viewModel.photoStatus {
        case .idle, .uploading:
            // UX 4.5.2 — the photo in effect is gone (expired or deleted):
            // initials for everyone, and this line for whoever may set it.
            if viewModel.canSetPhoto, let photo = viewModel.currentPhoto,
               AvatarImages.shared.isGone(photo) {
                line(l10n.t("group_photo.gone"))
            }
        case .failed:
            VStack(spacing: 4) {
                line(l10n.t("group_photo.error.upload"))
                Button(l10n.t("common.try_again")) { viewModel.retryPhoto() }
                    .foregroundStyle(Color.lakkoAccent)
            }
        case .limited:
            line(l10n.t("group_photo.error.limit"))
        case .full:
            line(l10n.t("group_photo.error.full"))
        case .owed:
            line(l10n.t("group_photo.owed"))
        case .notAllowed:
            line(l10n.t(viewModel.photoNotAllowedKey))
        }
    }

    private func line(_ text: String) -> some View {
        Text(text)
            .font(.footnote)
            .foregroundStyle(Color.lakkoTextMuted)
            .multilineTextAlignment(.center)
    }

    private var menu: some View {
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
            if viewModel.currentPhoto != nil {
                Divider()
                menuRow(l10n.t("profile.avatar.menu.remove"), icon: .trash, destructive: true) {
                    menuOpen = false
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.35) { removeConfirm = true }
                }
            }
        }
        .padding(.vertical, 8)
        .presentationDetents([.height(viewModel.currentPhoto != nil ? 200 : 140)])
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

    private func loadPicked(_ item: PhotosPickerItem) async {
        let isVideo = item.supportedContentTypes.contains { $0.conforms(to: .movie) }
        if isVideo {
            guard let movie = try? await item.loadTransferable(type: PickedMovie.self),
                  let source = await AvatarVideoSource.load(movie.url) else {
                viewModel.photoStatus = .failed
                return
            }
            cropInput = .video(source)
        } else {
            guard let data = try? await item.loadTransferable(type: Data.self),
                  let image = UIImage(data: data), let cg = AvatarJPEG.upright(image) else {
                viewModel.photoStatus = .failed
                return
            }
            cropInput = .photo(cg)
        }
    }
}

/// Reactions-and-emoji spec section 9 — the `Reactions` screen of a group's
/// info, for the owner and admins: All / Some / None, the chosen grid in
/// order (a tap removes one; drag to reorder), `Add` opening the picker,
/// `Save` enabled on a change.
/// What the Reactions screen reads and saves, for a group
/// (``ChatViewModel``) and a channel (``ChannelReactionsSetting``).
@MainActor
protocol AllowedReactionsModel: ObservableObject {
    var allowedReactions: FfiAllowedReactions { get }
    func saveAllowedReactions(_ value: FfiAllowedReactions) async -> Bool
}

extension ChatViewModel: AllowedReactionsModel {}

struct AllowedReactionsScreen<Model: AllowedReactionsModel>: View {
    @ObservedObject var viewModel: Model
    let onClose: () -> Void

    @ObservedObject private var l10n = Localization.shared
    @State private var choice: Choice
    @State private var list: [String]
    @State private var adding = false
    @State private var saving = false
    @State private var failed = false
    @State private var leaveAsk = false
    private let initial: FfiAllowedReactions

    enum Choice: Hashable { case all, some, none }

    init(viewModel: Model, onClose: @escaping () -> Void) {
        self.viewModel = viewModel
        self.onClose = onClose
        let current = viewModel.allowedReactions
        initial = current
        switch current {
        case .all:
            _choice = State(initialValue: .all)
            _list = State(initialValue: [])
        case .off:
            _choice = State(initialValue: .none)
            _list = State(initialValue: [])
        case .only(let emoji):
            _choice = State(initialValue: .some)
            _list = State(initialValue: emoji)
        }
    }

    private var value: FfiAllowedReactions {
        switch choice {
        case .all: return .all
        case .none: return .off
        case .some: return .only(emoji: list)
        }
    }

    private var max: Int { Int(allowedReactionsMax()) }
    private var changed: Bool { value != initial }
    private var canSave: Bool { changed && !saving && (choice != .some || !list.isEmpty) }

    var body: some View {
        content
            .navigationBarBackButtonHidden(true)
    }

    private var content: some View {
            List {
                Section {
                    option(.all, l10n.t("group_reactions.all"), l10n.t("group_reactions.all_sub"))
                    option(.some, l10n.t("group_reactions.some"), l10n.t("group_reactions.some_sub"))
                    option(.none, l10n.t("group_reactions.none"), l10n.t("group_reactions.none_sub"))
                }
                if choice == .some {
                    Section {
                        if list.isEmpty {
                            Text(l10n.t("group_reactions.choose_one"))
                                .font(.footnote)
                                .foregroundStyle(Color.lakkoTextMuted)
                        }
                        ForEach(list, id: \.self) { emoji in
                            Button { list.removeAll { $0 == emoji } } label: {
                                HStack {
                                    Text(emoji).font(.title2)
                                    Spacer()
                                    LkIconView(.minusCircle, size: 20)
                                        .foregroundStyle(Color.lakkoTextMuted)
                                }
                                .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .accessibilityLabel(emoji)
                        }
                        .onMove { list.move(fromOffsets: $0, toOffset: $1) }
                        if list.count >= max {
                            Text(l10n.t("group_reactions.max"))
                                .font(.footnote)
                                .foregroundStyle(Color.lakkoTextMuted)
                        } else {
                            Button(l10n.t("group_reactions.add")) { adding = true }
                                .foregroundStyle(Color.lakkoAccent)
                        }
                    }
                }
                if failed {
                    Section {
                        Text(l10n.t("group_reactions.save_failed"))
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoDanger)
                    }
                }
            }
            .environment(\.editMode, .constant(choice == .some ? .active : .inactive))
            .navigationTitle(l10n.t("group_reactions.title"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    // Pushed (section 9): leaving with unsaved changes asks.
                    Button {
                        if changed { leaveAsk = true } else { onClose() }
                    } label: {
                        LkIconView(.chevronBack, size: 24)
                    }
                    .accessibilityLabel(l10n.t("common.back"))
                }
                ToolbarItem(placement: .topBarTrailing) {
                    if saving {
                        ProgressView()
                    } else {
                        Button(l10n.t("common.save")) { save() }.disabled(!canSave)
                    }
                }
            }
            .sheet(isPresented: $adding) {
                NavigationStack {
                    EmojiTab(
                        onPick: { emoji in
                            if !list.contains(emoji), list.count < max { list.append(emoji) }
                        },
                        enabled: { list.count < max || list.contains($0) })
                    .navigationTitle(l10n.t("group_reactions.add"))
                    .navigationBarTitleDisplayMode(.inline)
                    .toolbar {
                        ToolbarItem(placement: .topBarTrailing) {
                            Button(l10n.t("common.done")) { adding = false }
                        }
                    }
                }
                .presentationDetents([.medium, .large])
            }
            .lakkoDialog(
                isPresented: $leaveAsk,
                title: l10n.t("profile.unsaved.title"),
                message: l10n.t("group_reactions.unsaved_body")
            ) {
                [
                    .primary(l10n.t("profile.unsaved.save")) { save() },
                    .secondary(l10n.t("profile.unsaved.discard")) { onClose() },
                    .cancel(),
                ]
            }
    }

    private func option(_ value: Choice, _ title: String, _ sub: String) -> some View {
        Button {
            choice = value
            // The first time Some is picked the grid starts with the defaults.
            if value == .some, list.isEmpty { list = MessageReactionRow.palette }
        } label: {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).foregroundStyle(Color.lakkoText)
                    Text(sub).font(.footnote).foregroundStyle(Color.lakkoTextMuted)
                }
                Spacer()
                if choice == value {
                    LkIconView(.checkmark, size: 20).foregroundStyle(Color.lakkoAccent)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(choice == value ? .isSelected : [])
    }

    private func save() {
        guard canSave else { return }
        saving = true
        failed = false
        let next = value
        Task { @MainActor in
            let ok = await viewModel.saveAllowedReactions(next)
            saving = false
            if ok { onClose() } else { failed = true }
        }
    }
}

/// UX 4.5.3 — the avatar slot of the New group form: the initials of the
/// typed name (a group glyph before any), the same edit button and menu
/// without `Remove photo` until one is chosen. The photo is uploaded when
/// Create is pressed, before anything is created.
struct NewGroupPhotoSlot: View {
    let name: String
    @Binding var media: PendingAvatarMedia?
    let disabled: Bool
    @ObservedObject private var l10n = Localization.shared

    @State private var menuOpen = false
    @State private var libraryOpen = false
    @State private var pickedItem: PhotosPickerItem?
    @State private var cropInput: AvatarCropInput?
    @State private var cameraOpen = false

    var body: some View {
        ZStack(alignment: .bottomTrailing) {
            Group {
                if let media {
                    PendingAvatarPicture(media: media, size: 96)
                } else {
                    InitialsAvatar(name: name, identity: name, size: 96, isGroup: true)
                }
            }
            .onTapGesture { if !disabled { menuOpen = true } }
            Button { menuOpen = true } label: {
                LkIconView(.pencil, size: 14, relativeTo: nil)
                    .foregroundStyle(Color.lakkoAccentForeground)
                    .frame(width: 32, height: 32)
                    .background(Circle().fill(Color.lakkoAccent))
                    .overlay(Circle().stroke(Color.lakkoBackground, lineWidth: 2))
            }
            .buttonStyle(.plain)
            .disabled(disabled)
            .accessibilityLabel(l10n.t("profile.avatar.change"))
        }
        .frame(maxWidth: .infinity)
        .sheet(isPresented: $menuOpen) {
            VStack(spacing: 0) {
                row(l10n.t("profile.avatar.menu.library"), .photoStack) {
                    menuOpen = false
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.35) { libraryOpen = true }
                }
                if AvatarCameraScreen.hasCamera {
                    Divider()
                    row(l10n.t("profile.avatar.menu.camera"), .camera) {
                        menuOpen = false
                        DispatchQueue.main.asyncAfter(deadline: .now() + 0.35) { cameraOpen = true }
                    }
                }
                if media != nil {
                    Divider()
                    row(l10n.t("profile.avatar.menu.remove"), .trash, destructive: true) {
                        menuOpen = false
                        media = nil
                    }
                }
            }
            .padding(.vertical, 8)
            .presentationDetents([.height(media != nil ? 200 : 140)])
            .presentationDragIndicator(.visible)
        }
        .photosPicker(
            isPresented: $libraryOpen, selection: $pickedItem,
            matching: .any(of: [.images, .videos]), preferredItemEncoding: .current)
        .onChange(of: pickedItem) { _, item in
            guard let item else { return }
            pickedItem = nil
            Task { await load(item) }
        }
        .fullScreenCover(item: $cropInput) { input in
            AvatarCropScreen(
                input: input,
                onUse: { picked in
                    cropInput = nil
                    media = picked
                },
                onCancel: { cropInput = nil })
        }
        .fullScreenCover(isPresented: $cameraOpen) {
            AvatarCameraScreen(
                onCaptured: { input in
                    cameraOpen = false
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) { cropInput = input }
                },
                onClose: { cameraOpen = false })
        }
    }

    private func row(_ title: String, _ icon: LkIcon, destructive: Bool = false, action: @escaping () -> Void) -> some View {
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

    private func load(_ item: PhotosPickerItem) async {
        if item.supportedContentTypes.contains(where: { $0.conforms(to: .movie) }) {
            if let movie = try? await item.loadTransferable(type: PickedMovie.self),
               let source = await AvatarVideoSource.load(movie.url) {
                cropInput = .video(source)
            }
        } else if let data = try? await item.loadTransferable(type: Data.self),
                  let image = UIImage(data: data), let cg = AvatarJPEG.upright(image) {
            cropInput = .photo(cg)
        }
    }
}
