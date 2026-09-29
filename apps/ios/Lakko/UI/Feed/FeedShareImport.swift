// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI
import UniformTypeIdentifiers

/// Share a saved feed (`docs/ux/feed.md` section 4.1): a link that carries
/// the rule in its fragment, never its path, and a `.lakkofeed` file, both
/// through the share sheet. Specific conversations are left out. Mirror of
/// the web share dialog in `routes/Feed.tsx`.
struct FeedShareSheet: View {
    let saved: SavedFeedView

    @Environment(\.dismiss) private var dismiss
    @State private var copied = false

    private struct Shared {
        let link: String
        let fileURL: URL?
    }

    private var shared: Shared? {
        guard let bytes = try? FeedDraft.exportBytes(saved.rule) else { return nil }
        let link = FeedDraft.shareLink(origin: ApiEndpoints.webOrigin, bytes: bytes)
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent(FeedDraft.shareFileName(saved.rule))
        let fileURL = (try? bytes.write(to: url, options: .atomic)) != nil ? url : nil
        return Shared(link: link, fileURL: fileURL)
    }

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 16) {
                if let shared {
                    Text("Specific conversations are left out: they mean nothing to anyone else.")
                        .font(.subheadline)
                        .foregroundStyle(Color.lakkoTextMuted)
                    Text(shared.link)
                        .font(.footnote.monospaced())
                        .lineLimit(3)
                        .textSelection(.enabled)
                        .padding(10)
                        .background(RoundedRectangle(cornerRadius: LakkoRadius.control).fill(Color.lakkoSurfaceSunken))
                    Button(copied ? "Copied" : "Copy link") {
                        UIPasteboard.general.string = shared.link
                        copied = true
                    }
                    .buttonStyle(.borderedProminent)
                    if let url = URL(string: shared.link) {
                        ShareLink("Share link", item: url)
                    }
                    if let fileURL = shared.fileURL {
                        ShareLink("Share file", item: fileURL)
                    }
                } else {
                    Text("This feed can't be shared as it is.")
                        .foregroundStyle(Color.lakkoDanger)
                }
                Spacer()
            }
            .padding(16)
            .navigationTitle("Share \"\(saved.rule.name)\"")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
        }
        .presentationDetents([.medium, .large])
    }
}

/// Import a shared feed (`docs/ux/feed.md` section 4.1): from a link, whose
/// fragment carries the rule, or from a `.lakkofeed` file. The person sees a
/// preview of the rules before anything is saved; anything the core refuses
/// is refused whole. Mirror of web `routes/FeedImport.tsx`.
struct FeedImportScreen: View {
    @ObservedObject var model: FeedViewModel
    let request: FeedImportRequest

    @Environment(\.dismiss) private var dismiss
    @State private var rule: FeedRule?
    @State private var error: String?
    @State private var picking = false

    static let fileType = UTType(filenameExtension: "lakkofeed") ?? .data

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 16) {
                if let rule {
                    Text(rule.name).font(.title3.weight(.semibold))
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(FeedDraft.summary(rule), id: \.self) { line in
                            Label(line, systemImage: "circle.fill")
                                .labelStyle(SummaryLabelStyle())
                        }
                    }
                    HStack {
                        Button("Add to my feeds") { save(rule) }
                            .buttonStyle(.borderedProminent)
                        Button("Cancel") { dismiss() }
                    }
                } else {
                    Text("Open a feed someone shared with you as a .lakkofeed file.")
                        .foregroundStyle(Color.lakkoTextMuted)
                    Button("Choose file") { picking = true }
                        .buttonStyle(.bordered)
                }
                if let error {
                    Text(error)
                        .foregroundStyle(Color.lakkoDanger)
                        .accessibilityAddTraits(.isStaticText)
                }
                Spacer()
            }
            .padding(16)
            .navigationTitle("Add a shared feed")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
            .fileImporter(
                isPresented: $picking,
                allowedContentTypes: [Self.fileType, .data]
            ) { result in
                if case .success(let url) = result { openFile(url) }
            }
        }
        .onAppear {
            if request.refused {
                error = FeedDraft.refusedImport
            } else if let bytes = request.bytes {
                open(bytes)
            }
        }
    }

    private func open(_ bytes: Data) {
        do {
            rule = try decodeFeedRule(bytes: bytes)
            error = nil
        } catch {
            rule = nil
            self.error = FeedDraft.refusedImport
        }
    }

    private func openFile(_ url: URL) {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        // The core refuses anything larger than a rule can be; reading far
        // more than that is pointless, so the file is cut off well above it.
        let limit = Int(feedRuleMaxLen()) * 2
        let size = (try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0
        guard size <= limit, let bytes = try? Data(contentsOf: url), bytes.count <= limit else {
            rule = nil
            error = FeedDraft.refusedImport
            return
        }
        open(bytes)
    }

    private func save(_ rule: FeedRule) {
        if model.addImported(rule) {
            dismiss()
        } else {
            error = "Couldn't save the feed. Try again."
        }
    }
}

private struct SummaryLabelStyle: LabelStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            configuration.icon
                .font(.system(size: 5))
                .foregroundStyle(Color.lakkoTextMuted)
            configuration.title
                .font(.subheadline)
        }
    }
}
