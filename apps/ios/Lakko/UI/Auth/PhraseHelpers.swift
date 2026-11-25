// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Pure rules behind the recovery-phrase screens: how the twelve words are
/// laid out, how pasted text becomes words, and the three-word confirmation
/// the sign-up flow asks for before it lets the user continue. No SwiftUI
/// here so the rules are unit-testable.
///
/// The grid reads ACROSS the rows (1 2 3 / 4 5 6 / ...): 3 columns x 4 rows
/// at phone width, 4 columns x 3 rows on a wide layout (iPad, landscape).
/// Display, confirmation and entry all use this one order, so a person
/// copies and types the words in the same reading order.
enum PhraseWords {

    /// The phrase length every Lakko recovery phrase has: 12 BIP39 words
    /// (128 bits of entropy). The one constant every phrase screen sizes
    /// itself from; it mirrors `lakko-mnemonic` `WORD_COUNT`, which is what
    /// actually accepts or refuses a phrase.
    static let count = 12

    /// Columns of the grid at phone width (4 rows).
    static let narrowColumns = 3

    /// Columns of the grid on a wide layout (3 rows).
    static let wideColumns = 4

    /// The column count for a layout: `wide` is a regular-width layout (iPad)
    /// or a compact-height one (a phone in landscape).
    static func gridColumns(wide: Bool) -> Int {
        wide ? wideColumns : narrowColumns
    }

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

    /// The words with their 1-based positions, in phrase order: the order
    /// they are rendered, read and focused through.
    static func numbered(_ words: [String]) -> [Cell] {
        words.enumerated().map { Cell(number: $0.offset + 1, word: $0.element) }
    }

    /// The grid as rows, row-major: row `r` holds positions
    /// `r * columns + 1` through `(r + 1) * columns`. The last row is shorter
    /// when the count does not divide evenly. This is what a `LazyVGrid`
    /// with `columns` columns produces from the flat list; it exists so the
    /// rule can be asserted.
    static func rows(_ words: [String], columns: Int = narrowColumns) -> [[Cell]] {
        let cols = max(1, columns)
        let cells = numbered(words)
        return stride(from: 0, to: cells.count, by: cols).map {
            Array(cells[$0..<min($0 + cols, cells.count)])
        }
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

    /// The confirm button of the check step is live: every asked word is
    /// typed (not only white space) and the acknowledgement is ticked
    /// (docs/ux/identity-recovery.md section 9, step 2; the web
    /// `PhraseCheckForm`). Whether the words match is judged on submit.
    static func canSubmit(entries: [String], acknowledged: Bool, count: Int = wordCount) -> Bool {
        acknowledged
            && entries.count >= count
            && !entries.prefix(count).contains { normalize($0).isEmpty }
    }

    /// `true` when every entry matches.
    static func check(challenge: Challenge, entries: [String], words: [String]) -> Bool {
        !words.isEmpty && mismatches(challenge: challenge, entries: entries, words: words).isEmpty
    }
}

/// The new-words screen after a key reset or taking the account back
/// (docs/ux/identity-recovery.md section 9.1, "Ready"): the grid and one
/// acknowledgement, no three-word check.
enum FreshPhraseAcknowledgement {
    /// Done is live: there are words to keep and the acknowledgement is
    /// ticked.
    static func canFinish(words: [String], acknowledged: Bool) -> Bool {
        acknowledged && !words.isEmpty
    }
}
