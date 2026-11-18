// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import LakkoCore
import SwiftUI
import UniformTypeIdentifiers

// `docs/ux/attachments.md` sections 3 and 4 — the send preview of Photo or
// Video, Camera and File: thumbnails (or file chips) in a strip, the current
// item large, a caption, `Send as file`, a delete control per item, `Send`.
// Several items send as several messages, in the order shown, the caption on
// the first.

/// One picked item.
struct SendPreviewItem: Identifiable {
    enum Source {
        case photo(Data)
        case video(URL)
        case file(Data, name: String, mime: String?)
    }

    let id = UUID()
    let source: Source
    var thumbnail: UIImage?
    /// The catalog key of why this item cannot be sent as it is, if so.
    var problemKey: String?

    var isMedia: Bool {
        if case .file = source { return false }
        return true
    }

    var displayName: String {
        switch source {
        case .photo: return Localization.text("chat.media.photo")
        case .video: return Localization.text("chat.media.video")
        case .file(_, let name, _): return name
        }
    }

    /// The bytes as picked (the `Send as file` path).
    var originalBytes: (Data, String, String?)? {
        switch source {
        case .photo(let data): return (data, "photo.jpg", PhotoVideoPrep.sniffInlineImageMime(data) ?? "image/jpeg")
        case .video(let url):
            guard let data = try? Data(contentsOf: url) else { return nil }
            return (data, url.lastPathComponent, UTType(filenameExtension: url.pathExtension)?.preferredMIMEType)
        case .file(let data, let name, let mime): return (data, name, mime)
        }
    }
}

/// The pure video rule (UX section 3): MP4 with H.264, at most the cap.
enum SendPreviewRules {
    static func videoAcceptable(isMP4: Bool, isH264: Bool, bytes: Int) -> Bool {
        isMP4 && isH264 && bytes <= AttachmentBridge.maxAttachmentBytes
    }

    static func fileAcceptable(bytes: Int) -> Bool { bytes <= AttachmentBridge.maxAttachmentBytes }

    /// Reads a picked video's container, codec and size.
    static func inspectVideo(_ url: URL) async -> Bool {
        // An H.264 QuickTime file (what the iPhone camera and library give)
        // counts: it is remuxed into MP4 without re-encoding.
        let type = UTType(filenameExtension: url.pathExtension)
        let isMP4 = type?.conforms(to: .mpeg4Movie) == true || type?.conforms(to: .quickTimeMovie) == true
        let size = ((try? FileManager.default.attributesOfItem(atPath: url.path))?[.size] as? NSNumber)?.intValue ?? .max
        var isH264 = false
        if let track = try? await AVURLAsset(url: url).loadTracks(withMediaType: .video).first,
           let formats = try? await track.load(.formatDescriptions) {
            isH264 = formats.contains { CMFormatDescriptionGetMediaSubType($0) == kCMVideoCodecType_H264 }
        }
        return videoAcceptable(isMP4: isMP4, isH264: isH264, bytes: size)
    }
}

/// UX sections 3 and 4 — the send preview.
struct SendPreviewScreen: View {
    @State var items: [SendPreviewItem]
    /// UX section 5 — after the camera: `Retake` in place of the strip.
    var onRetake: (() -> Void)? = nil
    let onSend: (_ items: [SendPreviewItem], _ caption: String, _ asFile: Bool) -> Void
    let onCancel: () -> Void

    @ObservedObject private var l10n = Localization.shared
    @State private var current: UUID?
    @State private var caption = ""
    @State private var asFile = false

    private var hasMedia: Bool { items.contains { $0.isMedia } }
    private var sendable: [SendPreviewItem] { items.filter { asFile || $0.problemKey == nil } }

    var body: some View {
        NavigationStack {
            VStack(spacing: 12) {
                if let item = items.first(where: { $0.id == (current ?? items.first?.id) }) {
                    large(item).frame(maxHeight: .infinity)
                }
                if let onRetake {
                    Button(l10n.t("camera.retake"), action: onRetake)
                } else if items.count > 1 || !hasMedia {
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 8) {
                            ForEach(items) { item in strip(item) }
                        }
                        .padding(.horizontal, 16)
                    }
                    .frame(height: hasMedia ? 72 : 60)
                }
                if hasMedia {
                    Toggle(l10n.t("attach.preview.send_as_file"), isOn: $asFile)
                        .tint(Color.lakkoAccent)
                        .padding(.horizontal, 16)
                }
                HStack(spacing: 8) {
                    TextField(l10n.t("attach.preview.caption"), text: $caption, axis: .vertical)
                        .textFieldStyle(.roundedBorder)
                    Button {
                        onSend(sendable, caption.trimmingCharacters(in: .whitespacesAndNewlines), asFile)
                    } label: {
                        Text(l10n.t("attach.preview.send")).frame(minHeight: 36)
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(Color.lakkoAccent)
                    .disabled(sendable.isEmpty)
                }
                .padding(.horizontal, 16)
                .padding(.bottom, 12)
            }
            .navigationTitle(l10n.t("attach.preview.title"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button(l10n.t("common.cancel"), action: onCancel) }
            }
        }
    }

    @ViewBuilder
    private func large(_ item: SendPreviewItem) -> some View {
        VStack(spacing: 8) {
            if let image = item.thumbnail {
                Image(uiImage: image).resizable().scaledToFit()
            } else {
                LkIconView(item.isMedia ? .video : .doc, size: 56, relativeTo: nil)
                    .foregroundStyle(Color.lakkoTextMuted)
                Text(item.displayName).font(.footnote)
            }
            if let key = item.problemKey, !asFile {
                Text(l10n.t(key)).font(.footnote).foregroundStyle(Color.lakkoDanger)
                    .multilineTextAlignment(.center).padding(.horizontal, 16)
            }
        }
    }

    private func strip(_ item: SendPreviewItem) -> some View {
        ZStack(alignment: .topTrailing) {
            Button { current = item.id } label: {
                Group {
                    if let image = item.thumbnail {
                        Image(uiImage: image).resizable().scaledToFill().frame(width: 64, height: 64)
                    } else {
                        VStack(spacing: 2) {
                            LkIconView(item.isMedia ? .video : .doc, size: 20)
                            Text(item.displayName).font(.caption2).lineLimit(1)
                        }
                        .frame(width: item.isMedia ? 64 : 120, height: item.isMedia ? 64 : 52)
                        .background(Color.lakkoSurfaceSunken)
                    }
                }
                .clipShape(RoundedRectangle(cornerRadius: 8))
                .overlay(RoundedRectangle(cornerRadius: 8).stroke(
                    item.problemKey != nil && !asFile ? Color.lakkoDanger : Color.clear, lineWidth: 2))
            }
            .buttonStyle(.plain)
            Button {
                items.removeAll { $0.id == item.id }
                if items.isEmpty { onCancel() }
            } label: {
                LkIconView(.xmarkCircleFill, size: 20)
                    .foregroundStyle(.white)
                    .background(Circle().fill(Color.black.opacity(0.6)))
            }
            .accessibilityLabel(l10n.t("attach.preview.remove", texts: ["name": item.displayName]))
        }
    }
}

extension SendPreviewItem {
    /// What the camera took, as a preview item.
    static func captured(_ media: CapturedMedia) async -> SendPreviewItem {
        switch media {
        case .photo(let data):
            var item = SendPreviewItem(source: .photo(data))
            item.thumbnail = UIImage(data: data)
            return item
        case .video(let url):
            // H.264 in a QuickTime container: remuxed to MP4 by the media
            // prep without re-encoding when sent.
            var item = SendPreviewItem(source: .video(url))
            let generator = AVAssetImageGenerator(asset: AVURLAsset(url: url))
            generator.appliesPreferredTrackTransform = true
            generator.maximumSize = CGSize(width: 640, height: 640)
            if case let (image, _)? = try? await generator.image(at: .zero) { item.thumbnail = UIImage(cgImage: image) }
            return item
        }
    }
}
