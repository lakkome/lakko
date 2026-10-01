// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0047 §3 / UX spec §4 — the emoji picker's data model, search, the
/// per-account Recent row and skin-tone defaults, and the one-grapheme check a
/// reaction must pass before it is sent. Mirror of
/// `apps/web/src/media/emoji/emoji-index.ts`.
///
/// The dataset is `Resources/Emoji/emoji-data.json`, a byte-identical copy of
/// the web client's file generated offline from pinned Unicode 15.1 + CLDR 45
/// sources by `apps/web/scripts/gen-emoji-data.mjs`, bundled with the app.
/// Nothing here touches the network.
public struct EmojiEntry: Equatable, Hashable {
    public let emoji: String
    public let name: String
    public let keywords: [String]
    public let group: Int
    /// light, medium-light, medium, medium-dark, dark — present only when all
    /// five exist.
    public let tones: [String]?
}

public struct EmojiDataset {
    public let groups: [String]
    public let entries: [EmojiEntry]
    /// Every base emoji AND every tone variant, mapped to its base entry.
    public let byEmoji: [String: EmojiEntry]

    public static let empty = EmojiDataset(groups: [], entries: [], byEmoji: [:])

    /// Build the dataset from the generator's JSON, skipping any row that does
    /// not have the expected shape rather than failing the picker. Row shape:
    /// `[emoji, name, "kw|kw", group, [five tones]?]`.
    public static func parse(_ json: Data) -> EmojiDataset {
        guard let root = try? JSONSerialization.jsonObject(with: json) as? [String: Any],
              let rawGroups = root["groups"] as? [Any],
              let rawRows = root["emoji"] as? [Any] else {
            return .empty
        }
        let groups = rawGroups.compactMap { $0 as? String }
        var entries: [EmojiEntry] = []
        var byEmoji: [String: EmojiEntry] = [:]
        for case let row as [Any] in rawRows {
            guard row.count >= 4,
                  let emoji = row[0] as? String,
                  let name = row[1] as? String,
                  let keywords = row[2] as? String,
                  let group = row[3] as? Int,
                  group >= 0, group < groups.count else {
                continue
            }
            var tones: [String]?
            if row.count >= 5, let list = row[4] as? [Any] {
                let strings = list.compactMap { $0 as? String }
                if strings.count == 5, list.count == 5 { tones = strings }
            }
            let entry = EmojiEntry(
                emoji: emoji,
                name: name,
                keywords: keywords.isEmpty ? [] : keywords.split(separator: "|").map(String.init),
                group: group,
                tones: tones
            )
            entries.append(entry)
            byEmoji[emoji] = entry
            for t in tones ?? [] { byEmoji[t] = entry }
        }
        return EmojiDataset(groups: groups, entries: entries, byEmoji: byEmoji)
    }

    /// The bundled dataset, loaded once. Empty (the picker shows no emoji)
    /// if the resource is missing, never a crash.
    public static let bundled: EmojiDataset = {
        guard let url = Bundle.main.url(forResource: "emoji-data", withExtension: "json"),
              let data = try? Data(contentsOf: url) else {
            return .empty
        }
        return parse(data)
    }()
}

public enum EmojiIndex {

    private static let separators = CharacterSet(charactersIn: " \t\n:,.()-_'\u{2019}")

    static func words(_ text: String) -> [String] {
        text.lowercased()
            .components(separatedBy: separators)
            .filter { !$0.isEmpty }
    }

    /// Entries matching `query`, best first. Every query token must be a prefix
    /// of a word in the name or of a keyword. Ranking: exact name, then a name
    /// that starts with the query, then a name-word match, then keyword-only
    /// matches; ties keep the dataset's CLDR order. An empty query matches
    /// nothing (the picker shows categories instead).
    public static func search(_ dataset: EmojiDataset, query: String, limit: Int = 200) -> [EmojiEntry] {
        let q = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !q.isEmpty else { return [] }
        let tokens = words(q)
        guard !tokens.isEmpty else { return [] }
        var scored: [(entry: EmojiEntry, score: Int, order: Int)] = []
        for (order, entry) in dataset.entries.enumerated() {
            let name = entry.name.lowercased()
            let nameWords = words(name)
            let keywordWords = entry.keywords.flatMap(words)
            var allInName = true
            var matches = true
            for t in tokens {
                let inName = nameWords.contains { $0.hasPrefix(t) }
                if !inName { allInName = false }
                if !inName && !keywordWords.contains(where: { $0.hasPrefix(t) }) {
                    matches = false
                    break
                }
            }
            guard matches else { continue }
            let score: Int
            if name == q { score = 0 } else if name.hasPrefix(q) { score = 1 } else if allInName { score = 2 } else { score = 3 }
            scored.append((entry, score, order))
        }
        scored.sort { $0.score != $1.score ? $0.score < $1.score : $0.order < $1.order }
        return scored.prefix(max(0, limit)).map(\.entry)
    }

    // MARK: - Per-account preferences

    /// §4 — the Recent row holds up to this many.
    public static let maxRecent = 24
    /// The device-wide keys used before the Recent row and the tone choices
    /// were per account (2026-10-01). Never read; removed by
    /// ``clear(accountIdB64:_:)``.
    public static let recentKey = "lakko.emoji.recent"
    public static let tonesKey = "lakko.emoji.tones"

    /// Where `accountIdB64`'s Recent row lives.
    public static func recentKey(accountIdB64: String) -> String { recentKey + "." + accountIdB64 }

    /// Where `accountIdB64`'s skin-tone choices live.
    public static func tonesKey(accountIdB64: String) -> String { tonesKey + "." + accountIdB64 }

    /// Move `emoji` to the front, de-duplicated, capped at `max`.
    public static func pushRecent(_ list: [String], _ emoji: String, max: Int = maxRecent) -> [String] {
        Array(([emoji] + list.filter { $0 != emoji }).prefix(Swift.max(0, max)))
    }

    /// The Recent row of `accountIdB64` (account data, maintainer rule
    /// 2026-09-30): empty with no account.
    public static func loadRecent(_ defaults: UserDefaults = .standard, accountIdB64: String?) -> [String] {
        guard let accountIdB64 else { return [] }
        let stored = defaults.stringArray(forKey: recentKey(accountIdB64: accountIdB64)) ?? []
        return Array(stored.filter(isSingleEmojiGrapheme).prefix(maxRecent))
    }

    /// Persist `accountIdB64`'s Recent row; a no-op with no account.
    public static func saveRecent(
        _ list: [String], _ defaults: UserDefaults = .standard, accountIdB64: String?
    ) {
        guard let accountIdB64 else { return }
        defaults.set(Array(list.prefix(maxRecent)), forKey: recentKey(accountIdB64: accountIdB64))
    }

    /// The skin-tone choices of `accountIdB64`: none with no account.
    public static func loadTonePrefs(
        _ defaults: UserDefaults = .standard, accountIdB64: String?
    ) -> [String: String] {
        guard let accountIdB64 else { return [:] }
        return (defaults.dictionary(forKey: tonesKey(accountIdB64: accountIdB64)) ?? [:])
            .compactMapValues { $0 as? String }
    }

    /// Persist `accountIdB64`'s skin-tone choices; a no-op with no account.
    public static func saveTonePrefs(
        _ prefs: [String: String], _ defaults: UserDefaults = .standard, accountIdB64: String?
    ) {
        guard let accountIdB64 else { return }
        defaults.set(prefs, forKey: tonesKey(accountIdB64: accountIdB64))
    }

    /// Forget device / Delete account: drop `accountIdB64`'s Recent row and
    /// tone choices, and the old device-wide ones. Other accounts' stay.
    public static func clear(accountIdB64: String, _ defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: recentKey(accountIdB64: accountIdB64))
        defaults.removeObject(forKey: tonesKey(accountIdB64: accountIdB64))
        defaults.removeObject(forKey: recentKey)
        defaults.removeObject(forKey: tonesKey)
    }

    /// Record `variant` as the default for `entry`. Choosing the base emoji
    /// itself (or anything that is not one of its tones) clears the preference.
    public static func withTonePref(
        _ prefs: [String: String],
        entry: EmojiEntry,
        variant: String
    ) -> [String: String] {
        var next = prefs
        if variant == entry.emoji || !(entry.tones ?? []).contains(variant) {
            next.removeValue(forKey: entry.emoji)
        } else {
            next[entry.emoji] = variant
        }
        return next
    }

    /// What the grid shows (and inserts) for `entry` for this account.
    public static func displayEmoji(_ entry: EmojiEntry, prefs: [String: String]) -> String {
        if let pref = prefs[entry.emoji], (entry.tones ?? []).contains(pref) { return pref }
        return entry.emoji
    }

    // MARK: - Reaction validation

    /// `lakko-attachments::MAX_EMOJI_BYTES` — the wire bound on a reaction.
    public static let maxEmojiBytes = 64

    /// UX spec §4 — "A reaction is exactly one emoji grapheme". True when
    /// `text` is one extended grapheme cluster (`Character`) that is an emoji
    /// — a pictographic sequence, a regional-indicator flag or a keycap — and
    /// fits the wire bound. A letter, two emoji, an emoji with trailing text or
    /// an empty string are all refused.
    public static func isSingleEmojiGrapheme(_ text: String) -> Bool {
        guard !text.isEmpty, text.utf8.count <= maxEmojiBytes, text.count == 1 else { return false }
        let scalars = Array(text.unicodeScalars)
        // A two-letter regional-indicator flag.
        if scalars.count == 2, scalars.allSatisfy({ (0x1F1E6...0x1F1FF).contains($0.value) }) {
            return true
        }
        // A keycap: [#*0-9] FE0F? 20E3.
        if let first = scalars.first, "#*0123456789".unicodeScalars.contains(first),
           scalars.last?.value == 0x20E3,
           scalars.count == 2 || (scalars.count == 3 && scalars[1].value == 0xFE0F) {
            return true
        }
        var pictographic = false
        for s in scalars {
            let props = s.properties
            let isRegionalIndicator = (0x1F1E6...0x1F1FF).contains(s.value)
            // A lone regional indicator is half a flag, not an emoji (the flag
            // case returned above).
            if isRegionalIndicator { return false }
            if props.isEmoji {
                // `isEmoji` is also true for ASCII digits, `#` and `*` (only
                // emoji inside a keycap, handled above) and for the skin-tone
                // modifiers, which are components rather than emoji alone.
                if s.value > 0x7F, !props.isEmojiModifier { pictographic = true }
                continue
            }
            switch props.generalCategory {
            case .uppercaseLetter, .lowercaseLetter, .titlecaseLetter, .modifierLetter,
                 .otherLetter, .decimalNumber, .letterNumber, .otherNumber:
                // A pictograph glued to letters or digits in one cluster is not
                // an emoji the picker could produce. (Emoji that Unicode files
                // under a letter category, such as U+2139, were admitted above.)
                return false
            default:
                break
            }
        }
        return pictographic
    }
}
