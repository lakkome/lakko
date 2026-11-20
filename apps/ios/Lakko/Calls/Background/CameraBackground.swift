// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CoreGraphics
import Foundation
import LakkoCore
import UIKit
import WebRTC

/// The camera background of this app run (ADR-0089, UX
/// `docs/ux/call-backgrounds.md`): the stored choice, the own photos, the
/// processing stage every camera capture goes through, and what the call
/// screen and the settings sheet show about it.
///
/// One per process: one call at a time, one camera at a time. The stage
/// lives for the app run so the model is loaded once ("starting" happens on
/// the first use in an app run, UX section 3).
@MainActor
final class CameraBackground: ObservableObject {

    static let shared = CameraBackground()

    /// What the call shows about the background.
    @Published private(set) var phase: BackgroundPhase = .off
    /// The stored choice of the bound account (`none`, `blur`, ...).
    @Published private(set) var stored: String = ""
    /// The account's own photos, newest first.
    @Published private(set) var photos: [BackgroundPhotoStore.Photo] = []
    /// Bumped once per failure: the call screen turns the camera off and
    /// shows the line.
    @Published private(set) var failures: Int = 0
    /// A failure whose line the call screen has not shown yet (the call was
    /// collapsed into the bar when it happened).
    @Published private(set) var failureNoticePending = false
    /// Under the strip: a picked photo could not be used.
    @Published var photoUnusable = false

    /// `nil` on a device that cannot run a background at all; the section is
    /// then absent.
    var stage: BackgroundFrameStage? { CameraBackgroundStage.shared }

    private let choices: BackgroundChoiceStore
    private let photoStore: BackgroundPhotoStore
    private let accountIdB64: () -> String?

    var isAvailable: Bool { stage != nil }

    private init() {
        self.choices = BackgroundChoiceStore()
        self.photoStore = BackgroundPhotoStore()
        self.accountIdB64 = { AcceptedContactsStore.boundAccountIdB64 }
    }

    /// The stage's phase, on the main actor.
    func stageReported(_ next: BackgroundPhase) {
        phase = next
        // A background that works again, or none, owes no failure line.
        if next != .failed { failureNoticePending = false }
    }

    /// The call screen shows the failure line: `true` once per failure that
    /// is still in force, then the line is no longer owed.
    func takeFailureNotice() -> Bool {
        guard failureNoticePending else { return false }
        failureNoticePending = false
        return phase == .failed
    }

    /// The stage failed: the call screen turns the camera off and shows the
    /// line, which is announced.
    func stageFailed() {
        failureNoticePending = true
        failures += 1
        UIAccessibility.post(
            notification: .announcement,
            argument: Localization.text("call.background.failed"))
    }

    /// The UI lists what was just applied to the stage.
    func applied(stored: String, photos: [BackgroundPhotoStore.Photo]) {
        self.stored = stored
        self.photos = photos
    }

    // MARK: - Loading

    /// Read the bound account's choice and photos and hand the stage what to
    /// apply. Called before a camera starts (``LocalCameraCapture``), so the
    /// first frame is already judged by the stored choice, and when the
    /// settings sheet opens.
    ///
    /// Nonisolated and synchronous: a capture starts off the main actor and
    /// must not start before the choice is applied.
    nonisolated static func applyStoredChoice() {
        guard let stage = CameraBackgroundStage.shared else { return }
        let account = AcceptedContactsStore.boundAccountIdB64
        let stored = account.map { BackgroundChoiceStore().stored(accountIdB64: $0) } ?? ""
        let photos = BackgroundPhotoStore()
        let effect = BackgroundResolution.effect(stored: stored) { id in
            account.flatMap { photos.load(id: id, accountIdB64: $0) }
        }
        stage.setEffect(effect)
        let listed = account.map { photos.list(accountIdB64: $0) } ?? []
        DispatchQueue.main.async {
            MainActor.assumeIsolated {
                CameraBackground.shared.applied(stored: stored, photos: listed)
            }
        }
    }

    /// A camera started, was switched, or was turned back on: a background
    /// that failed is tried again (the Rust restart rule).
    nonisolated static func cameraRestarted() {
        CameraBackgroundStage.shared?.restart()
    }

    /// Reload what the UI lists.
    func refresh() {
        let account = accountIdB64()
        stored = account.map { choices.stored(accountIdB64: $0) } ?? ""
        photos = account.map { photoStore.list(accountIdB64: $0) } ?? []
    }

    // MARK: - Choosing

    /// Apply and store `next` at once (no Save, no renegotiation).
    func select(_ next: String) {
        photoUnusable = false
        guard let account = accountIdB64() else { return }
        choices.set(next, accountIdB64: account)
        stored = next
        let photoStore = self.photoStore
        stage?.setEffect(
            BackgroundResolution.effect(stored: next) { id in
                photoStore.load(id: id, accountIdB64: account)
            },
            chosen: true)
    }

    /// The stored choice read by the Rust core (an unreadable one is `blur`).
    var choice: BackgroundChoice {
        backgroundChoiceFromStored(stored: stored)
    }

    /// Add a picked photo and select it. `false` when the data is not a
    /// usable picture (the strip is left as it was).
    @discardableResult
    func addPhoto(data: Data) -> Bool {
        photoUnusable = false
        guard let account = accountIdB64(),
              let image = Self.uprightImage(from: data)
        else {
            photoUnusable = true
            return false
        }
        let current = choice
        let selectedId = current.kind == .photo ? current.id : nil
        do {
            let photo = try photoStore.add(image, accountIdB64: account, selectedId: selectedId)
            photos = photoStore.list(accountIdB64: account)
            select(BackgroundResolution.stored(photoId: photo.id))
            return true
        } catch {
            photoUnusable = true
            return false
        }
    }

    /// Remove an own photo; removing the selected one selects `None`.
    func removePhoto(id: String) {
        guard let account = accountIdB64() else { return }
        photoStore.remove(id: id, accountIdB64: account)
        photos = photoStore.list(accountIdB64: account)
        let current = choice
        if current.kind == .photo, current.id == id {
            select("none")
        }
    }

    /// A thumbnail of an own photo.
    func photoThumbnail(id: String) -> CGImage? {
        guard let account = accountIdB64() else { return nil }
        return photoStore.thumbnail(id: id, accountIdB64: account)
    }

    /// The picture a picked file holds, turned upright, or `nil`.
    static func uprightImage(from data: Data) -> CGImage? {
        guard let image = UIImage(data: data) else { return nil }
        return AvatarJPEG.upright(image)
    }

    // MARK: - Wipe

    /// The account wipe: the stored choice and every own photo.
    nonisolated static func delete(accountIdB64: String) throws {
        BackgroundChoiceStore().clear(accountIdB64: accountIdB64)
        try BackgroundPhotoStore().delete(accountIdB64: accountIdB64)
    }
}

/// The processing stage of this app run, outside the main actor: a camera
/// capture asks it from the capturer's queue. `nil` on a device with no
/// Metal (the background is then not offered).
enum CameraBackgroundStage {
    static let shared: BackgroundFrameStage? = {
        guard let compositor = CoreImageBackgroundCompositor() else { return nil }
        let stage = BackgroundFrameStage(
            makeSegmenter: { NativeSegmenter(nowMs: BackgroundClock.nowMs()) },
            compositor: compositor)
        stage.onPhase = { phase in
            DispatchQueue.main.async {
                MainActor.assumeIsolated { CameraBackground.shared.stageReported(phase) }
            }
        }
        stage.onFailure = {
            DispatchQueue.main.async {
                MainActor.assumeIsolated { CameraBackground.shared.stageFailed() }
            }
        }
        stage.onOutput = { frame in CameraBackgroundPreview.feed(frame) }
        return stage
    }()
}

/// The renderers fed every frame the stage hands on: the settings sheet's
/// preview. Any thread.
enum CameraBackgroundPreview {
    private static let lock = NSLock()
    private nonisolated(unsafe) static var renderers: [RTCVideoRenderer] = []

    static func add(_ renderer: RTCVideoRenderer) {
        lock.lock()
        defer { lock.unlock() }
        guard !renderers.contains(where: { $0 === renderer }) else { return }
        renderers.append(renderer)
    }

    static func remove(_ renderer: RTCVideoRenderer) {
        lock.lock()
        defer { lock.unlock() }
        renderers.removeAll { $0 === renderer }
    }

    static func feed(_ frame: RTCVideoFrame) {
        lock.lock()
        let now = renderers
        lock.unlock()
        for renderer in now { renderer.renderFrame(frame) }
    }
}
