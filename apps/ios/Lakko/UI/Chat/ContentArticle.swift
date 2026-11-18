// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import PhotosUI
import SwiftUI

// ADR-0090 section 8 / UX section 11 — the article editor and reader. The
// editor works in blocks (a paragraph, heading, subheading, quote, list item,
// image or divider), each with its own marks; consecutive list items become
// one list. No HTML, no Markdown: the core's closed block format only.

/// One run of a block: text with its marks (UX 11.1: Bold, Italic and Link
/// apply to the selected text).
struct ArticleDraftRun: Codable, Equatable {
    var text: String
    var bold = false
    var italic = false
    /// An `https://` link this run is the visible text of.
    var link: String?
}

/// One block in the editor (also the draft kept on the device).
struct ArticleDraftBlock: Identifiable, Codable, Equatable {
    enum Kind: String, Codable, CaseIterable {
        case paragraph, heading, subheading, quote, bullet, number, image, divider
    }

    var id = UUID()
    var kind: Kind = .paragraph
    var runs: [ArticleDraftRun] = []
    /// For `.image`: the index into the draft's images.
    var image: Int?

    /// The block's plain text (a caption is edited as plain text).
    var text: String {
        get { runs.map(\.text).joined() }
        set { runs = newValue.isEmpty ? [] : [ArticleDraftRun(text: newValue)] }
    }
}

/// The draft of an article: title, blocks and its uploaded images.
struct ArticleDraft: Codable, Equatable {
    var title = ""
    var blocks: [ArticleDraftBlock] = [ArticleDraftBlock()]
    var images: [PersistedAttachment] = []

    /// Build the wire article and its attachments; `nil` when nothing to send.
    func build() -> (Article, [AttachmentRef])? {
        let refs = images.compactMap { $0.toAttachmentRef() }
        guard refs.count == images.count else { return nil }
        var blocks: [ArticleBlock] = []
        var list: (ordered: Bool, items: [ArticleListItem])?
        func flush() {
            if let l = list { blocks.append(.listBlock(ordered: l.ordered, items: l.items)) }
            list = nil
        }
        for b in self.blocks {
            let runs = Self.runs(b)
            switch b.kind {
            case .bullet, .number:
                guard !runs.isEmpty else { continue }
                let ordered = b.kind == .number
                if list?.ordered != ordered { flush(); list = (ordered, []) }
                list?.items.append(ArticleListItem(depth: 0, runs: runs))
                continue
            default:
                flush()
            }
            switch b.kind {
            case .paragraph: if !runs.isEmpty { blocks.append(.paragraph(runs: runs)) }
            case .heading: if !runs.isEmpty { blocks.append(.heading(level: .h1, runs: runs)) }
            case .subheading: if !runs.isEmpty { blocks.append(.heading(level: .h2, runs: runs)) }
            case .quote: if !runs.isEmpty { blocks.append(.quote(runs: runs)) }
            case .image:
                if let i = b.image, i < refs.count { blocks.append(.image(attachmentIndex: UInt8(i), caption: runs)) }
            case .divider: blocks.append(.divider)
            case .bullet, .number: break
            }
        }
        flush()
        let title = self.title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !title.isEmpty, !blocks.isEmpty else { return nil }
        let article = Article(title: title, blocks: blocks)
        guard (try? validateArticle(article: article)) != nil else { return nil }
        return (article, refs)
    }

    /// The wire runs of a block: canonical (no empty run, adjacent runs with
    /// the same marks and link merged), no line break inside (a line break
    /// is a block boundary).
    static func runs(_ b: ArticleDraftBlock) -> [ArticleRun] {
        var merged: [ArticleDraftRun] = []
        for run in b.runs {
            let text = run.text.replacingOccurrences(of: "\n", with: " ")
            guard !text.isEmpty else { continue }
            let link = run.link.flatMap { articleUrlValid(url: $0) ? $0 : nil }
            if var last = merged.last, last.bold == run.bold, last.italic == run.italic, last.link == link {
                last.text += text
                merged[merged.count - 1] = last
            } else {
                merged.append(ArticleDraftRun(text: text, bold: run.bold, italic: run.italic, link: link))
            }
        }
        // Trim the block's outer white space.
        if var first = merged.first {
            first.text = String(first.text.drop { $0 == " " })
            merged[0] = first
        }
        if var last = merged.last {
            while last.text.hasSuffix(" ") { last.text.removeLast() }
            merged[merged.count - 1] = last
        }
        return merged.filter { !$0.text.isEmpty }.map { r in
            if let link = r.link { return .link(url: link, bold: r.bold, italic: r.italic, text: r.text) }
            return .text(bold: r.bold, italic: r.italic, text: r.text)
        }
    }

    /// The editor form of a received article (`Edit` on an article).
    init(article: Article, attachments: [AttachmentRef]) {
        title = article.title
        images = attachments.map(PersistedAttachment.init(from:))
        blocks = []
        func block(_ kind: ArticleDraftBlock.Kind, _ runs: [ArticleRun], image: Int? = nil) -> ArticleDraftBlock {
            var b = ArticleDraftBlock(kind: kind)
            b.image = image
            b.runs = runs.map { run in
                switch run {
                case .text(let bold, let italic, let text):
                    return ArticleDraftRun(text: text, bold: bold, italic: italic)
                case .link(let url, let bold, let italic, let text):
                    return ArticleDraftRun(text: text, bold: bold, italic: italic, link: url)
                }
            }
            return b
        }
        for b in article.blocks {
            switch b {
            case .paragraph(let runs): blocks.append(block(.paragraph, runs))
            case .heading(let level, let runs): blocks.append(block(level == .h1 ? .heading : .subheading, runs))
            case .quote(let runs): blocks.append(block(.quote, runs))
            case .listBlock(let ordered, let items):
                for item in items { blocks.append(block(ordered ? .number : .bullet, item.runs)) }
            case .image(let index, let caption): blocks.append(block(.image, caption, image: Int(index)))
            case .divider: blocks.append(ArticleDraftBlock(kind: .divider))
            }
        }
        if blocks.isEmpty { blocks = [ArticleDraftBlock()] }
    }

    init() {}

    var textBytes: Int { blocks.reduce(0) { $0 + $1.text.utf8.count } }
}

/// One draft per conversation, kept on the device (UX section 11.1).
enum ArticleDraftStore {
    static let prefix = "lakko.article-draft.v1."

    /// Per account (wiped with the account) and per conversation.
    static func key(_ groupIdHex: String) -> String {
        prefix + (AcceptedContactsStore.boundAccountIdB64 ?? "none") + "." + groupIdHex
    }

    static func load(_ groupIdHex: String) -> ArticleDraft? {
        guard let data = UserDefaults.standard.data(forKey: key(groupIdHex)) else { return nil }
        return try? JSONDecoder().decode(ArticleDraft.self, from: data)
    }

    static func save(_ draft: ArticleDraft, _ groupIdHex: String) {
        if let data = try? JSONEncoder().encode(draft) { UserDefaults.standard.set(data, forKey: key(groupIdHex)) }
    }

    static func clear(_ groupIdHex: String) { UserDefaults.standard.removeObject(forKey: key(groupIdHex)) }
}

/// UX section 11.1 — the article editor.
struct ArticleEditorScreen: View {
    let groupIdHex: String
    /// Set for `Edit` on an existing article.
    let editing: ArticleDraft?
    let upload: (Data) async -> AttachmentRef?
    let onPublish: (Article, [AttachmentRef]) -> Void
    let onCancel: () -> Void

    @ObservedObject private var l10n = Localization.shared
    @State private var draft = ArticleDraft()
    @State private var loaded = false
    @FocusState private var focused: UUID?
    @State private var linkFor: UUID?
    @State private var linkText = ""
    @State private var pickerItem: PhotosPickerItem?
    @State private var pickerOpen = false
    @State private var uploading = false
    @State private var confirmDiscard = false
    @StateObject private var rich = RichTextRegistry()

    private let limits = contentLimits()

    private var atLimit: Bool {
        draft.textBytes >= Int(limits.maxArticleTextBytes) || draft.images.count >= Int(limits.maxArticleImages)
            || draft.blocks.count >= Int(limits.maxArticleBlocks)
    }

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        TextField(l10n.t("article.title"), text: Binding(
                            get: { draft.title },
                            set: { draft.title = ContentText.clamp($0, bytes: limits.maxArticleTitleBytes) }),
                            axis: .vertical)
                            .font(.title2.weight(.semibold))
                        ForEach($draft.blocks) { $block in
                            blockEditor($block)
                        }
                    }
                    .padding(16)
                }
                if atLimit {
                    Text(l10n.t("article.at_limit")).font(.footnote).foregroundStyle(Color.lakkoWarning)
                }
                formatBar
            }
            .navigationTitle(l10n.t(editing == nil ? "article.editor.title" : "article.editor.edit_title"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(l10n.t("common.cancel")) {
                        if editing == nil, draft != ArticleDraft() { confirmDiscard = true } else { onCancel() }
                    }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(l10n.t("article.publish")) {
                        guard let built = draft.build() else { return }
                        if editing == nil { ArticleDraftStore.clear(groupIdHex) }
                        onPublish(built.0, built.1)
                    }
                    .disabled(draft.build() == nil || uploading)
                }
            }
            .photosPicker(isPresented: $pickerOpen, selection: $pickerItem, matching: .images)
            .onChange(of: pickerItem) { _, item in
                guard let item else { return }
                pickerItem = nil
                Task { await addImage(item) }
            }
            .lakkoDialog(
                isPresented: Binding(get: { linkFor != nil }, set: { if !$0 { linkFor = nil } }),
                title: l10n.t("article.link.insert"),
                field: LakkoDialogField(placeholder: l10n.t("article.link.url"), text: $linkText)
            ) {
                [.primary(l10n.t("article.link.insert"), enabled: articleUrlValid(url: linkText)) { applyLink() },
                 .cancel()]
            }
            .lakkoDialog(isPresented: $confirmDiscard, title: l10n.t("article.discard")) {
                [.danger(l10n.t("editor.discard")) { ArticleDraftStore.clear(groupIdHex); onCancel() },
                 .secondary(l10n.t("article.save_draft")) { ArticleDraftStore.save(draft, groupIdHex); onCancel() },
                 .cancel(l10n.t("editor.keep_editing"))]
            }
            .onAppear {
                guard !loaded else { return }
                loaded = true
                draft = editing ?? ArticleDraftStore.load(groupIdHex) ?? ArticleDraft()
            }
            .onChange(of: draft) { _, d in if editing == nil { ArticleDraftStore.save(d, groupIdHex) } }
        }
    }

    @ViewBuilder
    private func blockEditor(_ block: Binding<ArticleDraftBlock>) -> some View {
        let b = block.wrappedValue
        switch b.kind {
        case .divider:
            Divider().padding(.vertical, 8)
        case .image:
            VStack(alignment: .leading, spacing: 4) {
                if let i = b.image, i < draft.images.count, let ref = draft.images[i].toAttachmentRef() {
                    ArticleImage(ref: ref)
                }
                TextField(l10n.t("article.caption"), text: block.text)
                    .font(.footnote)
                    .focused($focused, equals: b.id)
            }
        default:
            HStack(alignment: .top, spacing: 6) {
                if b.kind == .bullet { Text("•") }
                if b.kind == .number { Text("\(numberIndex(of: b.id)).") }
                if b.kind == .quote { Rectangle().fill(Color.lakkoAccent).frame(width: 3) }
                RichBlockTextView(
                    runs: block.runs, blockId: b.id, fontSize: fontSize(b),
                    placeholder: l10n.t("article.body"), registry: rich,
                    onFocus: { focused = b.id }, onReturn: { insertAfter(b.id) })
            }
        }
    }

    private func numberIndex(of id: UUID) -> Int {
        guard let i = draft.blocks.firstIndex(where: { $0.id == id }) else { return 1 }
        var n = 1
        var j = i - 1
        while j >= 0, draft.blocks[j].kind == .number { n += 1; j -= 1 }
        return n
    }

    private func fontSize(_ b: ArticleDraftBlock) -> (size: CGFloat, semibold: Bool) {
        switch b.kind {
        case .heading: return (22, true)
        case .subheading: return (17, true)
        default: return (UIFont.preferredFont(forTextStyle: .body).pointSize, false)
        }
    }

    private var focusedIndex: Int? { draft.blocks.firstIndex { $0.id == focused } }

    private func mutateFocused(_ change: (inout ArticleDraftBlock) -> Void) {
        guard let i = focusedIndex else { return }
        change(&draft.blocks[i])
    }

    private func insertAfter(_ id: UUID, _ block: ArticleDraftBlock = ArticleDraftBlock()) {
        guard draft.blocks.count < Int(limits.maxArticleBlocks) else { return }
        let i = draft.blocks.firstIndex { $0.id == id } ?? (draft.blocks.count - 1)
        var next = block
        if block.kind == .paragraph, [.bullet, .number].contains(draft.blocks[i].kind) {
            next.kind = draft.blocks[i].kind
        }
        draft.blocks.insert(next, at: i + 1)
        focused = next.id
    }

    /// UX section 11.1 — the format bar above the keyboard.
    private var formatBar: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 4) {
                barButton("textformat.size", "article.format.heading") {
                    mutateFocused { b in
                        b.kind = b.kind == .heading ? .subheading : (b.kind == .subheading ? .paragraph : .heading)
                    }
                }
                barButton("bold", "article.format.bold") { rich.toggle(.traitBold) }
                barButton("italic", "article.format.italic") { rich.toggle(.traitItalic) }
                barButton("list.bullet", "article.format.bullets") {
                    mutateFocused { $0.kind = $0.kind == .bullet ? .paragraph : .bullet }
                }
                barButton("list.number", "article.format.numbers") {
                    mutateFocused { $0.kind = $0.kind == .number ? .paragraph : .number }
                }
                barButton("text.quote", "article.format.quote") {
                    mutateFocused { $0.kind = $0.kind == .quote ? .paragraph : .quote }
                }
                barButton("link", "article.format.link") {
                    guard let i = focusedIndex else { return }
                    linkText = rich.selectedLink() ?? "https://"
                    linkFor = draft.blocks[i].id
                }
                barButton("photo", "article.format.image") {
                    guard draft.images.count < Int(limits.maxArticleImages) else { return }
                    pickerOpen = true
                }
                barButton("minus", "article.format.divider") {
                    insertAfter(focused ?? draft.blocks.last?.id ?? UUID(), ArticleDraftBlock(kind: .divider))
                }
                if uploading { ProgressView().controlSize(.small).padding(.horizontal, 8) }
            }
            .padding(.horizontal, 8)
        }
        .frame(height: 44)
        .background(Color.lakkoSurface)
    }

    private func barButton(_ icon: String, _ key: String, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: icon).frame(width: 40, height: 40)
        }
        .buttonStyle(.plain)
        .foregroundStyle(Color.lakkoText)
        .accessibilityLabel(l10n.t(key))
    }

    private func applyLink() {
        guard linkFor != nil else { return }
        rich.setLink(articleUrlValid(url: linkText) ? linkText : nil)
        linkFor = nil
    }

    private func addImage(_ item: PhotosPickerItem) async {
        guard let bytes = try? await item.loadTransferable(type: Data.self) else { return }
        uploading = true
        defer { uploading = false }
        guard let ref = await upload(bytes) else { return }
        draft.images.append(PersistedAttachment(from: ref))
        var block = ArticleDraftBlock(kind: .image)
        block.image = draft.images.count - 1
        insertAfter(focused ?? draft.blocks.last?.id ?? UUID(), block)
    }
}

// MARK: - Reader

/// UX section 11.3 — the reader: full screen, the theme's text sizes,
/// headings 22/600 and 17/600, quotes with an accent rule, lists indented,
/// images full width with their caption.
struct ArticleReaderScreen: View {
    let article: Article
    let attachments: [AttachmentRef]
    let author: String
    let date: String
    let onClose: () -> Void
    let menu: () -> [LakkoMenuItem]

    @ObservedObject private var l10n = Localization.shared
    @State private var menuOpen = false

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    Text(article.title).font(.title.weight(.semibold))
                        .accessibilityAddTraits(.isHeader)
                    ForEach(Array(article.blocks.enumerated()), id: \.offset) { _, block in
                        blockView(block)
                    }
                }
                .padding(20)
                .frame(maxWidth: 720, alignment: .leading)
                .frame(maxWidth: .infinity)
            }
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button(l10n.t("common.close"), action: onClose) }
                ToolbarItem(placement: .principal) {
                    VStack(spacing: 0) {
                        Text(author).font(.footnote.weight(.semibold))
                        Text(date).font(.caption2).foregroundStyle(Color.lakkoTextMuted)
                    }
                }
                ToolbarItem(placement: .primaryAction) {
                    Button { menuOpen = true } label: { Image(systemName: "ellipsis") }
                        .accessibilityLabel(l10n.t("article.more"))
                }
            }
            .navigationBarTitleDisplayMode(.inline)
            .lakkoActionSheet(isPresented: $menuOpen, items: menu)
        }
    }

    @ViewBuilder
    private func blockView(_ block: ArticleBlock) -> some View {
        switch block {
        case .paragraph(let runs):
            Text(Self.attributed(runs))
        case .heading(let level, let runs):
            Text(Self.attributed(runs))
                .font(.system(size: level == .h1 ? 22 : 17, weight: .semibold))
                .accessibilityAddTraits(.isHeader)
        case .quote(let runs):
            HStack(spacing: 10) {
                Rectangle().fill(Color.lakkoAccent).frame(width: 3)
                Text(Self.attributed(runs)).italic()
            }
        case .listBlock(let ordered, let items):
            VStack(alignment: .leading, spacing: 6) {
                ForEach(Array(items.enumerated()), id: \.offset) { index, item in
                    HStack(alignment: .top, spacing: 8) {
                        Text(ordered ? "\(index + 1)." : "•")
                        Text(Self.attributed(item.runs))
                    }
                    .padding(.leading, CGFloat(item.depth) * 16 + 8)
                }
            }
        case .image(let index, let caption):
            VStack(alignment: .leading, spacing: 4) {
                if Int(index) < attachments.count { ArticleImage(ref: attachments[Int(index)]) }
                if !caption.isEmpty {
                    Text(Self.attributed(caption)).font(.footnote).foregroundStyle(Color.lakkoTextMuted)
                }
            }
            .accessibilityElement(children: .combine)
        case .divider:
            Divider()
        }
    }

    /// Native text from the runs: bold, italic, links in the accent colour.
    static func attributed(_ runs: [ArticleRun]) -> AttributedString {
        var out = AttributedString()
        for run in runs {
            switch run {
            case .text(let bold, let italic, let text):
                var part = AttributedString(text)
                part.inlinePresentationIntent = intent(bold: bold, italic: italic)
                out += part
            case .link(let url, let bold, let italic, let text):
                var part = AttributedString(text)
                part.inlinePresentationIntent = intent(bold: bold, italic: italic)
                part.link = URL(string: url)
                part.foregroundColor = Color.lakkoAccent
                out += part
            }
        }
        return out
    }

    private static func intent(bold: Bool, italic: Bool) -> InlinePresentationIntent? {
        var i: InlinePresentationIntent = []
        if bold { i.insert(.stronglyEmphasized) }
        if italic { i.insert(.emphasized) }
        return i.isEmpty ? nil : i
    }

    /// The plain text of an article (Copy text).
    static func plainText(_ article: Article) -> String {
        var lines = [article.title]
        func text(_ runs: [ArticleRun]) -> String {
            runs.map { if case .text(_, _, let t) = $0 { return t }; if case .link(_, _, _, let t) = $0 { return t }; return "" }
                .joined()
        }
        for b in article.blocks {
            switch b {
            case .paragraph(let r), .heading(_, let r), .quote(let r): lines.append(text(r))
            case .listBlock(_, let items): lines += items.map { "- " + text($0.runs) }
            case .image(_, let c): if !c.isEmpty { lines.append(text(c)) }
            case .divider: lines.append("")
            }
        }
        return lines.joined(separator: "\n")
    }
}

// MARK: - Rich text block (UX 11.1: marks on the selection)

/// The text view the format bar acts on (the one being edited).
@MainActor
final class RichTextRegistry: ObservableObject {
    weak var textView: UITextView?

    /// Toggle bold or italic on the selection, or for what is typed next when
    /// nothing is selected.
    func toggle(_ trait: UIFontDescriptor.SymbolicTraits) {
        guard let tv = textView else { return }
        let range = tv.selectedRange
        if range.length == 0 {
            let font = (tv.typingAttributes[.font] as? UIFont) ?? tv.font ?? .preferredFont(forTextStyle: .body)
            tv.typingAttributes[.font] = Self.flip(font, trait, on: !font.fontDescriptor.symbolicTraits.contains(trait))
            return
        }
        let text = NSMutableAttributedString(attributedString: tv.attributedText)
        var allOn = true
        text.enumerateAttribute(.font, in: range) { value, _, _ in
            if !((value as? UIFont)?.fontDescriptor.symbolicTraits.contains(trait) ?? false) { allOn = false }
        }
        text.enumerateAttribute(.font, in: range) { value, sub, _ in
            let font = (value as? UIFont) ?? tv.font ?? .preferredFont(forTextStyle: .body)
            text.addAttribute(.font, value: Self.flip(font, trait, on: !allOn), range: sub)
        }
        tv.attributedText = text
        tv.selectedRange = range
        tv.delegate?.textViewDidChange?(tv)
    }

    /// The link of the selection, if any.
    func selectedLink() -> String? {
        guard let tv = textView, tv.selectedRange.location < tv.attributedText.length else { return nil }
        return (tv.attributedText.attribute(.link, at: tv.selectedRange.location, effectiveRange: nil) as? URL)?.absoluteString
            ?? tv.attributedText.attribute(.link, at: tv.selectedRange.location, effectiveRange: nil) as? String
    }

    /// Make the selection (or the whole block when nothing is selected) a
    /// link, or remove it with `nil`.
    func setLink(_ url: String?) {
        guard let tv = textView else { return }
        var range = tv.selectedRange
        if range.length == 0 { range = NSRange(location: 0, length: tv.attributedText.length) }
        let text = NSMutableAttributedString(attributedString: tv.attributedText)
        if let url { text.addAttribute(.link, value: url, range: range) } else { text.removeAttribute(.link, range: range) }
        tv.attributedText = text
        tv.delegate?.textViewDidChange?(tv)
    }

    static func flip(_ font: UIFont, _ trait: UIFontDescriptor.SymbolicTraits, on: Bool) -> UIFont {
        var traits = font.fontDescriptor.symbolicTraits
        if on { traits.insert(trait) } else { traits.remove(trait) }
        guard let d = font.fontDescriptor.withSymbolicTraits(traits) else { return font }
        return UIFont(descriptor: d, size: font.pointSize)
    }
}

/// A paragraph-like block edited with marks on ranges (bold, italic, link),
/// stored as the draft's runs.
struct RichBlockTextView: UIViewRepresentable {
    @Binding var runs: [ArticleDraftRun]
    let blockId: UUID
    let fontSize: (size: CGFloat, semibold: Bool)
    let placeholder: String
    let registry: RichTextRegistry
    let onFocus: () -> Void
    let onReturn: () -> Void

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeUIView(context: Context) -> UITextView {
        let tv = UITextView()
        tv.isScrollEnabled = false
        tv.backgroundColor = .clear
        tv.textContainerInset = .zero
        tv.textContainer.lineFragmentPadding = 0
        tv.delegate = context.coordinator
        tv.accessibilityLabel = placeholder
        tv.attributedText = attributed(runs)
        tv.typingAttributes = [.font: baseFont, .foregroundColor: UIColor(Color.lakkoText)]
        return tv
    }

    func updateUIView(_ tv: UITextView, context: Context) {
        context.coordinator.parent = self
        if Self.runs(of: tv.attributedText, base: baseFont) != runs {
            tv.attributedText = attributed(runs)
        }
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView: UITextView, context: Context) -> CGSize? {
        let width = proposal.width ?? 320
        let size = uiView.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude))
        return CGSize(width: width, height: max(size.height, baseFont.lineHeight + 4))
    }

    var baseFont: UIFont {
        fontSize.semibold ? .systemFont(ofSize: fontSize.size, weight: .semibold) : .systemFont(ofSize: fontSize.size)
    }

    func attributed(_ runs: [ArticleDraftRun]) -> NSAttributedString {
        let out = NSMutableAttributedString()
        for run in runs {
            var font = baseFont
            if run.bold { font = RichTextRegistry.flip(font, .traitBold, on: true) }
            if run.italic { font = RichTextRegistry.flip(font, .traitItalic, on: true) }
            var attrs: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: UIColor(Color.lakkoText)]
            if let link = run.link { attrs[.link] = link }
            out.append(NSAttributedString(string: run.text, attributes: attrs))
        }
        return out
    }

    /// The runs an attributed text carries; bold and italic relative to the
    /// block's base font (a heading's own weight is not a mark).
    static func runs(of text: NSAttributedString, base: UIFont) -> [ArticleDraftRun] {
        var out: [ArticleDraftRun] = []
        let baseTraits = base.fontDescriptor.symbolicTraits
        text.enumerateAttributes(in: NSRange(location: 0, length: text.length)) { attrs, range, _ in
            let font = attrs[.font] as? UIFont
            let traits = font?.fontDescriptor.symbolicTraits ?? []
            let link = (attrs[.link] as? URL)?.absoluteString ?? attrs[.link] as? String
            out.append(ArticleDraftRun(
                text: text.attributedSubstring(from: range).string,
                bold: traits.contains(.traitBold) && !baseTraits.contains(.traitBold),
                italic: traits.contains(.traitItalic), link: link))
        }
        return out
    }

    final class Coordinator: NSObject, UITextViewDelegate {
        var parent: RichBlockTextView
        init(_ parent: RichBlockTextView) { self.parent = parent }

        func textViewDidBeginEditing(_ textView: UITextView) {
            parent.registry.textView = textView
            parent.onFocus()
        }

        func textViewDidChange(_ textView: UITextView) {
            parent.runs = RichBlockTextView.runs(of: textView.attributedText, base: parent.baseFont)
        }

        func textView(_ textView: UITextView, shouldChangeTextIn range: NSRange, replacementText text: String) -> Bool {
            // A line break is a block boundary: Return starts a new block.
            if text == "\n" { parent.onReturn(); return false }
            return true
        }
    }
}
