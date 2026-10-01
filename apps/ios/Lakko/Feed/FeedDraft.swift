// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// A feed rule for the editor, sharing and import (`docs/ux/feed.md`
/// section 4.1). Mirror of web `feed/feed-draft.ts`.
///
/// On iOS the core's `FeedRule` is already a plain value, so the editor
/// holds it directly. The rule itself — its bounds, its codec, what "valid"
/// means — is `lakko-feed` in the core; this only turns the core's frozen
/// refusal codes into sentences and moves bytes to and from links and files.
/// It decides no bound of its own: every check is the core's
/// `validateFeedRule`.
enum FeedDraft {

    /// A blank feed: "Latest", named.
    static func blank(name: String = "My feed") -> FeedRule {
        var rule = feedPresetRule(preset: .latest)
        rule.name = name
        return rule
    }

    /// A preset as a starting point for a new feed.
    static func preset(_ key: FeedPresetKey, name: String) -> FeedRule {
        var rule = feedPresetRule(preset: key.preset)
        rule.name = name
        return rule
    }

    /// The frozen core code carried by a refusal, or `nil`.
    static func refusalCode(_ error: Error) -> String? {
        guard case FfiError.FeedRule(let message) = error else { return nil }
        guard let colon = message.firstIndex(of: ":") else { return message }
        return String(message[..<colon])
    }

    /// The editor's line for a refusal code.
    static func problemText(_ code: String?) -> String {
        switch code {
        case "name": return "Give the feed a name of up to 64 characters."
        case "noKinds": return "Choose groups, channels or both."
        case "weightOutOfRange": return "A slider is out of range."
        case "windowOutOfRange": return "Look back between 1 and 30 days."
        case "conversations": return "Too many conversations are picked."
        case "strippedWithSources": return "This shared feed can't also name conversations."
        case "words": return "Check the word lists: one word per line, up to 32 words."
        case "tooLarge": return "This feed is too large to save."
        default: return "This feed can't be saved as it is."
        }
    }

    /// Why the rule cannot be saved, in one sentence, or `nil` when it can.
    /// Encoding is part of the check, since the size bound is the encoder's.
    static func problem(_ rule: FeedRule) -> String? {
        do {
            try validateFeedRule(rule: rule)
            _ = try encodeFeedRule(rule: rule)
            return nil
        } catch {
            return problemText(refusalCode(error))
        }
    }

    /// One word per line, each through the core's word rule; the ones it
    /// refuses and repeats are dropped.
    static func parseWords(_ text: String) -> [String] {
        var out: [String] = []
        for line in text.split(separator: "\n", omittingEmptySubsequences: false) {
            if let w = normalizeFeedWord(raw: String(line)), !out.contains(w) { out.append(w) }
        }
        return out
    }

    // MARK: - Sharing

    /// The shareable bytes: the rule with its conversations stripped, since
    /// they mean nothing to anyone else and naming them would tell what the
    /// sharer is in.
    static func exportBytes(_ rule: FeedRule) throws -> Data {
        try encodeFeedRule(rule: exportFeedRule(rule: rule))
    }

    /// The link that carries a shared feed in its fragment, never its path,
    /// so no server sees the rule.
    static func shareLink(origin: String, bytes: Data) -> String {
        "\(origin)/feed/import#\(base64UrlNoPad(bytes))"
    }

    /// The bytes a share link's fragment carries, or `nil` when there is none.
    static func bytes(fromFragment fragment: String?) -> Data? {
        guard var raw = fragment else { return nil }
        if raw.hasPrefix("#") { raw.removeFirst() }
        guard !raw.isEmpty,
              raw.unicodeScalars.allSatisfy({ CharacterSet.alphanumerics.contains($0) && $0.isASCII
                  || $0 == "-" || $0 == "_" })
        else { return nil }
        var b64 = raw.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        while b64.count % 4 != 0 { b64.append("=") }
        return Data(base64Encoded: b64)
    }

    /// The file name a shared feed saves as.
    static func shareFileName(_ rule: FeedRule) -> String {
        let kept = rule.name.unicodeScalars.filter {
            CharacterSet.letters.contains($0) || CharacterSet.decimalDigits.contains($0)
                || $0 == " " || $0 == "_" || $0 == "-"
        }
        let base = String(String.UnicodeScalarView(kept))
            .trimmingCharacters(in: .whitespaces)
            .split(whereSeparator: { $0 == " " })
            .joined(separator: "-")
        return "\(base.isEmpty ? "feed" : base).lakkofeed"
    }

    /// The importer's preview of a shared feed's rules (UX spec section 4.1).
    static func summary(_ rule: FeedRule) -> [String] {
        var lines: [String] = []
        let kinds: String
        if rule.groups && rule.channels {
            kinds = "all groups and channels"
        } else if rule.groups {
            kinds = "all groups"
        } else {
            kinds = "all channels"
        }
        lines.append("Sources: \(kinds)")
        if rule.includeMuted { lines.append("Includes muted conversations") }
        let w = rule.weights
        let ranked = [
            ("reactions", w.reactions),
            ("replies", w.replies),
            ("your contacts", w.fromContacts),
            ("mentions of you", w.mentionsMe),
            ("photos and video", w.hasMedia),
        ].filter { $0.1 > 0 }.map(\.0)
        lines.append(ranked.isEmpty ? "Newest first" : "Ranks by recency and \(ranked.joined(separator: ", "))")
        lines.append("Looks back \(rule.windowDays) day\(rule.windowDays == 1 ? "" : "s")")
        if rule.forMe { lines.append("Only what is about you") }
        if rule.mediaOnly { lines.append("Only with photos or video") }
        if rule.minReactions > 0 { lines.append("At least \(rule.minReactions) reactions") }
        if rule.hideOwn { lines.append("Hides your own messages") }
        if !rule.hideWords.isEmpty { lines.append("Hides \(rule.hideWords.count) words") }
        if !rule.onlyWords.isEmpty {
            lines.append("Only messages with: \(rule.onlyWords.joined(separator: ", "))")
        }
        return lines
    }

    static func base64UrlNoPad(_ bytes: Data) -> String {
        bytes.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    /// The file name extension of a shared feed (UX spec section 4.1).
    static let fileExtension = "lakkofeed"

    /// `true` for a file the system handed over as a shared feed.
    static func isSharedFeedFile(_ url: URL) -> Bool {
        url.isFileURL && url.pathExtension.lowercased() == fileExtension
    }

    /// The most bytes read from a shared file: the core refuses anything
    /// larger than a rule can be, so reading far more is pointless.
    static var fileReadLimit: Int { Int(feedRuleMaxLen()) * 2 }

    /// What the import screen opens with for `bytes` read from a file:
    /// refused when nothing could be read or it is larger than a rule can be.
    /// Whether the bytes are a rule is the core's to say on the screen.
    static func importRequest(fileBytes: Data?, limit: Int) -> FeedImportRequest {
        guard let fileBytes, !fileBytes.isEmpty, fileBytes.count <= limit else {
            return FeedImportRequest(bytes: nil, refused: true)
        }
        return FeedImportRequest(bytes: fileBytes, refused: false)
    }

    /// Read a shared file the system opened in Lakko (security-scoped when it
    /// is opened in place) and turn it into an import request. A copy the
    /// system made in the app's Inbox is removed once read.
    static func importRequest(fromFile url: URL) -> FeedImportRequest {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        let limit = fileReadLimit
        let size = (try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0
        let bytes = size <= limit ? try? Data(contentsOf: url) : nil
        if url.pathComponents.contains("Inbox"),
           url.standardizedFileURL.path.hasPrefix(URL(fileURLWithPath: NSHomeDirectory()).standardizedFileURL.path)
        {
            try? FileManager.default.removeItem(at: url)
        }
        return importRequest(fileBytes: bytes, limit: limit)
    }

    /// What the refused-import state says, for any file or link the core
    /// refuses (UX spec section 7).
    static let refusedImport = "This file isn't a feed Lakko can open."
}
