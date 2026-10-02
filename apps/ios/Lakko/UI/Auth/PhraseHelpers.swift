// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Pure rules behind the recovery-phrase screens (UX spec section 5.6):
/// how the twelve words are laid out in columns, how pasted text becomes words,
/// and the three-word confirmation the sign-up flow asks for before it
/// lets the user continue. No SwiftUI here so the rules are unit-testable.
enum PhraseWords {

    /// The phrase length every Lakko recovery phrase has: 12 BIP39 words
    /// (128 bits of entropy, ADR-0064 decision 3). The one constant every
    /// phrase screen sizes itself from; it mirrors `lakko-mnemonic`
    /// `WORD_COUNT`, which is what actually accepts or refuses a phrase.
    static let count = 12

    /// How many columns the display grid and the entry grid both use, so a
    /// word sits at the same place when it is shown and when it is typed.
    static let gridColumns = 2

    /// One cell of the phrase grid: the 1-based position and the word.
    struct Cell: Equatable {
        let number: Int
        let word: String
    }

    /// Split a space-joined phrase into its words (any whitespace, empties
    /// dropped).
    static func words(of phrase: String) -> [String] {
        phrase.split(whereSeparator: { $0.isWhitespace }).map(String.init)
    }

    /// Lay the words out column-major in `columns` columns: the first column
    /// holds positions 1...ceil(n/columns), the next continues from there,
    /// so a 12-word phrase reads 1-6 on the left and 7-12 on the right.
    /// The last columns can be one shorter when the count does not divide.
    static func columns(_ words: [String], columns: Int = gridColumns) -> [[Cell]] {
        guard columns > 0, !words.isEmpty else { return [] }
        let perColumn = Int((Double(words.count) / Double(columns)).rounded(.up))
        var result: [[Cell]] = []
        var start = 0
        while start < words.count {
            let end = min(start + perColumn, words.count)
            result.append((start..<end).map { Cell(number: $0 + 1, word: words[$0]) })
            start = end
        }
        return result
    }

    /// The 0-based index ranges of the entry grid's columns, column-major
    /// like ``columns(_:columns:)``: the same split of ``count`` positions,
    /// so entry and display number their cells identically.
    static func entryColumnRanges(columns: Int = gridColumns) -> [Range<Int>] {
        guard columns > 0, count > 0 else { return [] }
        let perColumn = Int((Double(count) / Double(columns)).rounded(.up))
        return stride(from: 0, to: count, by: perColumn).map { $0..<min($0 + perColumn, count) }
    }

    /// Turn pasted or typed text into words: numbering such as `1.`, `12)`
    /// or `3:` is dropped, commas and line breaks count as separators, and
    /// every word is lower-cased so it matches the word list.
    static func parsePasted(_ text: String) -> [String] {
        let separators = CharacterSet.whitespacesAndNewlines.union(CharacterSet(charactersIn: ",;"))
        return text
            .components(separatedBy: separators)
            .map { token -> String in
                var t = token.trimmingCharacters(in: .whitespacesAndNewlines)
                // A leading number with a trailing `.`, `)` or `:` is a position label.
                let digits = t.prefix(while: { $0.isNumber })
                if !digits.isEmpty {
                    let rest = t.dropFirst(digits.count)
                    if let first = rest.first, ".):".contains(first) {
                        t = String(rest.dropFirst())
                    } else if rest.isEmpty {
                        t = ""
                    }
                }
                return t.lowercased()
            }
            .filter { !$0.isEmpty }
    }

    /// The words padded or trimmed to exactly ``count`` entries, for the
    /// entry grid.
    static func padded(_ words: [String]) -> [String] {
        var out = Array(words.prefix(count))
        while out.count < count { out.append("") }
        return out
    }
}

/// The sign-up confirmation step: the user re-enters three words at random
/// positions before continuing (client-side only; the phrase never leaves
/// the device). ``pick`` chooses the positions, ``check`` judges the entries.
enum ThreeWordConfirmation {

    /// How many words the user is asked for.
    static let wordCount = 3

    /// The 0-based positions asked for, ascending, all distinct.
    struct Challenge: Equatable {
        let positions: [Int]

        /// 1-based labels for the UI ("Word 7").
        var numbers: [Int] { positions.map { $0 + 1 } }
    }

    /// Pick ``wordCount`` distinct positions in `0..<total`, ascending.
    /// The generator is injected so a test can make the pick deterministic.
    static func pick<G: RandomNumberGenerator>(
        total: Int = PhraseWords.count,
        count: Int = wordCount,
        using generator: inout G
    ) -> Challenge {
        let n = max(0, min(count, total))
        var chosen = Set<Int>()
        while chosen.count < n {
            chosen.insert(Int.random(in: 0..<total, using: &generator))
        }
        return Challenge(positions: chosen.sorted())
    }

    /// Pick with the system generator.
    static func pick(total: Int = PhraseWords.count, count: Int = wordCount) -> Challenge {
        var generator = SystemRandomNumberGenerator()
        return pick(total: total, count: count, using: &generator)
    }

    /// Normalise one entry: trimmed, lower-cased.
    static func normalize(_ entry: String) -> String {
        entry.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    }

    /// The 0-based challenge positions whose entry does not match the
    /// phrase word there. Empty means every entry is right. A missing entry
    /// or a position outside the phrase counts as a mismatch.
    static func mismatches(
        challenge: Challenge,
        entries: [String],
        words: [String]
    ) -> [Int] {
        challenge.positions.enumerated().compactMap { index, position in
            guard position < words.count, index < entries.count else { return position }
            return normalize(entries[index]) == words[position].lowercased() ? nil : position
        }
    }

    /// `true` when every entry matches.
    static func check(challenge: Challenge, entries: [String], words: [String]) -> Bool {
        !words.isEmpty && mismatches(challenge: challenge, entries: entries, words: words).isEmpty
    }
}
