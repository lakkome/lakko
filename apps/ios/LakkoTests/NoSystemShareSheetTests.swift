// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

/// The guard that the system share sheet does not come back.
///
/// A Lakko screen never hands the person to an operating-system surface when
/// the app can draw its own. The share sheet opened from the call's Invite
/// control and from four link rows (found live 2026-10-02): a foreign
/// surface, offering a dozen destinations where a link needs two: copy it, or
/// send it to a contact. Each was replaced with a control of the app's own,
/// and a file that leaves the app goes through the save picker instead
/// (`FileExportPicker`). This test keeps it that way; the twin of the web
/// `no-system-share.test.ts`.
///
/// The scan reads the app's sources from the checkout this test was built
/// from. The simulator runs on the same machine, so the path the compiler
/// recorded is readable; where it is not (a test run on another machine than
/// the build) the test is skipped rather than passed.
final class NoSystemShareSheetTests: XCTestCase {

    private func sourceRoot() throws -> URL {
        let root = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()  // LakkoTests
            .deletingLastPathComponent()  // apps/ios
            .appendingPathComponent("Lakko", isDirectory: true)
        guard FileManager.default.isReadableFile(atPath: root.path) else {
            throw XCTSkip("the app's sources are not readable from this test run")
        }
        return root
    }

    private func swiftFiles(under root: URL) -> [URL] {
        guard let walker = FileManager.default.enumerator(
            at: root, includingPropertiesForKeys: nil) else { return [] }
        return walker.compactMap { $0 as? URL }.filter { $0.pathExtension == "swift" }
    }

    /// The source with comments and string literals removed, so a sentence
    /// about the forbidden call (this header included) is not a hit.
    private func code(_ source: String) -> String {
        var out = source
        for pattern in [
            "/\\*[\\s\\S]*?\\*/",
            "//[^\\n]*",
            "\"(?:\\\\.|[^\"\\\\\\n])*\"",
        ] {
            out = out.replacingOccurrences(
                of: pattern, with: " ", options: .regularExpression)
        }
        return out
    }

    func testNoScreenOpensTheSystemShareSheet() throws {
        let files = swiftFiles(under: try sourceRoot())
        XCTAssertGreaterThan(files.count, 100, "the scan must see the app")
        XCTAssertTrue(files.contains { $0.lastPathComponent == "CallScreen.swift" })
        let forbidden = ["ShareLink", "UIActivityViewController", "UIDocumentInteractionController"]
        var hits: [String] = []
        for file in files {
            guard let source = try? String(contentsOf: file, encoding: .utf8) else { continue }
            let stripped = code(source)
            for word in forbidden where stripped.contains(word) {
                hits.append("\(file.lastPathComponent): \(word)")
            }
        }
        XCTAssertEqual(hits, [], "a link is copied or sent in-app; a file goes through the save picker")
    }
}
