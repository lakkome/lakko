// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// The pure rules behind the Phase C contacts and settings screens
/// (docs/ux/client-redesign.md sections 5.3 and 5.5): alphabetical grouping
/// and search, request ordering, the safety-number layout, the invite-link
/// rows and the device row title.
final class ContactsPresentationTests: XCTestCase {

    private typealias Item = ContactListPresentation.Item<Int>

    private func item(_ id: Int, _ name: String, username: String? = nil, placeholder: Bool = false) -> Item {
        Item(id: id, name: name, username: username, isPlaceholder: placeholder)
    }

    // MARK: - Grouping

    func testIndexLetterIsTheUppercasedFirstLetterOrTheCatchAll() {
        XCTAssertEqual(ContactListPresentation.indexLetter(for: "anna"), "A")
        XCTAssertEqual(ContactListPresentation.indexLetter(for: "  Boris"), "B")
        XCTAssertEqual(ContactListPresentation.indexLetter(for: "@carol"), "C", "a username-only label sorts by its letter")
        XCTAssertEqual(ContactListPresentation.indexLetter(for: "Ольга"), "О")
        XCTAssertEqual(ContactListPresentation.indexLetter(for: "42"), "#")
        XCTAssertEqual(ContactListPresentation.indexLetter(for: ""), "#")
        XCTAssertEqual(ContactListPresentation.indexLetter(for: "Member", isPlaceholder: true), "#",
                       "a placeholder never sorts under M")
    }

    func testSectionsAreAlphabeticalWithTheCatchAllLast() {
        let sections = ContactListPresentation.sections([
            item(1, "boris"),
            item(2, "Anna"),
            item(3, "@carol", username: "carol"),
            item(4, "Member", placeholder: true),
            item(5, "Bella"),
            item(6, "42"),
        ])
        XCTAssertEqual(sections.map(\.letter), ["A", "B", "C", "#"])
        XCTAssertEqual(sections[0].ids, [2])
        XCTAssertEqual(sections[1].ids, [5, 1], "case-insensitive within a section")
        XCTAssertEqual(sections[2].ids, [3])
        XCTAssertEqual(sections[3].ids, [6, 4], "a placeholder sorts after every named person")
    }

    func testEmptyInputGivesNoSections() {
        XCTAssertEqual(ContactListPresentation.sections([Item]()), [])
    }

    func testSearchMatchesNameOrUsernameCaseInsensitively() {
        let anna = item(1, "Anna Petrova", username: "anna_p")
        XCTAssertTrue(ContactListPresentation.matches(anna, query: ""))
        XCTAssertTrue(ContactListPresentation.matches(anna, query: "   "))
        XCTAssertTrue(ContactListPresentation.matches(anna, query: "petr"))
        XCTAssertTrue(ContactListPresentation.matches(anna, query: "ANNA_"))
        XCTAssertTrue(ContactListPresentation.matches(anna, query: "@anna"))
        XCTAssertFalse(ContactListPresentation.matches(anna, query: "boris"))
        XCTAssertFalse(ContactListPresentation.matches(item(2, "Member", placeholder: true), query: "x"))
    }

    // MARK: - Requests

    func testRequestsSortNewestFirstByMessageId() {
        let sorted = RequestSorting.newestFirst([(5, "a"), (9, "b"), (1, "c")], messageId: { Int64($0.0) })
        XCTAssertEqual(sorted.map(\.1), ["b", "a", "c"])
    }

    // MARK: - Safety number

    func testSafetyNumberSplitsIntoTwelveGroupsOfFiveInFourRows() {
        let number = "12345 67890 12345 67890 12345 67890 12345 67890 12345 67890 12345 67890"
        let groups = SafetyNumberFormat.groups(number)
        XCTAssertEqual(groups.count, 12)
        XCTAssertTrue(groups.allSatisfy { $0.count == 5 })
        XCTAssertEqual(groups.first, "12345")
        let rows = SafetyNumberFormat.rows(number)
        XCTAssertEqual(rows.count, 4)
        XCTAssertEqual(rows.map(\.count), [3, 3, 3, 3])
    }

    func testSafetyNumberIgnoresIncomingSpacingAndKeepsAPartialTail() {
        XCTAssertEqual(SafetyNumberFormat.groups("1234567890"), ["12345", "67890"])
        XCTAssertEqual(SafetyNumberFormat.groups("12345678"), ["12345", "678"], "a malformed input stays visible")
        XCTAssertEqual(SafetyNumberFormat.groups(""), [])
        XCTAssertEqual(SafetyNumberFormat.rows(""), [])
    }

    // MARK: - Invite links

    private func active(
        slug: String, expiryMs: UInt64, maxUses: UInt32, used: UInt32, createdAt: String, label: String = ""
    ) -> ActiveInvite {
        ActiveInvite(
            blobIdSlug: slug, expiryMs: expiryMs, maxUses: maxUses, usedCount: used,
            label: label, bootstrapReferral: false, createdAt: createdAt)
    }

    private func generated(slug: String, expiryMs: Int64, createdAtMs: Int64) -> GeneratedInvite {
        GeneratedInvite(
            blobId: Data(repeating: 1, count: 6), blobIdSlug: slug,
            url: "https://lakko.me/i/\(slug)", label: "", expiryMs: expiryMs,
            maxUses: 0, createdAtMs: createdAtMs)
    }

    func testUsesLeftCopyFollowsTheSentinels() {
        XCTAssertEqual(InviteLinkRows.usesLeftCopy(maxUses: 0, usedCount: 0), "1 use left")
        XCTAssertEqual(InviteLinkRows.usesLeftCopy(maxUses: 0, usedCount: 1), "Used")
        XCTAssertEqual(InviteLinkRows.usesLeftCopy(maxUses: 10, usedCount: 7), "3 of 10 left")
        XCTAssertEqual(InviteLinkRows.usesLeftCopy(maxUses: 10, usedCount: 12), "0 of 10 left", "never negative")
        XCTAssertNil(InviteLinkRows.usesLeftCopy(maxUses: UInt32.max, usedCount: 3), "unlimited shows no count")
    }

    func testMergeKeepsServerRowsFirstAddsUnknownSessionRowsAndDropsExpired() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let nowMs = UInt64(now.timeIntervalSince1970 * 1000)
        let rows = InviteLinkRows.merge(
            active: [
                active(slug: "old", expiryMs: nowMs - 1, maxUses: 0, used: 0, createdAt: "2026-09-01T10:00:00Z"),
                active(slug: "srv", expiryMs: nowMs + 60_000, maxUses: 10, used: 2,
                       createdAt: "2026-09-10T10:00:00Z", label: "Team"),
            ],
            sessionGenerated: [
                generated(slug: "srv", expiryMs: Int64(nowMs) + 60_000, createdAtMs: 0),
                generated(slug: "new", expiryMs: Int64(nowMs) + 60_000,
                          createdAtMs: Int64(now.timeIntervalSince1970 * 1000)),
                generated(slug: "gone", expiryMs: Int64(nowMs) - 5, createdAtMs: 0),
            ],
            now: now)
        XCTAssertEqual(rows.map(\.slug), ["new", "srv"], "newest first; the server copy wins over the session copy")
        XCTAssertEqual(rows[1].title, "Team")
        XCTAssertEqual(rows[1].usesLeft, "8 of 10 left")
        XCTAssertEqual(rows[1].origin, .server)
        // The link must point at the stack the account lives on, so the
        // prefix comes from configuration (`ApiEndpoints.webOrigin`) and a
        // hardcoded production host here failed every Debug run - which is
        // worse than it sounds: three red tests are how a real failure hides.
        XCTAssertEqual(rows[1].url, ContactsRepository.inviteUrlPrefix + "srv")
        XCTAssertEqual(rows[0].title, "Invite link")
        XCTAssertEqual(rows[0].usesLeft, "1 use left")
        if case .session(let blobId) = rows[0].origin {
            XCTAssertEqual(blobId.count, 6)
        } else {
            XCTFail("a session-only link revokes through its blob id")
        }
    }

    func testCreatedAtParsesIsoWithAndWithoutFractionsAndSqlStyle() {
        // 2026-09-10T10:00:00Z: 1767225600 (2026-01-01) + 252 days + 10 h.
        XCTAssertEqual(
            InviteLinkRows.parseCreatedAt("2026-09-10T10:00:00Z")?.timeIntervalSince1970, 1_789_034_400)
        XCTAssertEqual(
            InviteLinkRows.parseCreatedAt("2026-09-10T10:00:00.250Z")?.timeIntervalSince1970 ?? 0,
            1_789_034_400.25, accuracy: 0.001)
        XCTAssertEqual(
            InviteLinkRows.parseCreatedAt("2026-09-10 10:00:00")?.timeIntervalSince1970, 1_789_034_400)
        XCTAssertNil(InviteLinkRows.parseCreatedAt("yesterday"))
    }

    func testExpiryCopy() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        XCTAssertEqual(InviteLinkRows.expiryCopy(expiryMs: 0, now: now), "Never expires")
        XCTAssertEqual(InviteLinkRows.expiryCopy(expiryMs: 1_800_000_000_000, now: now), "Expired")
        XCTAssertTrue(
            InviteLinkRows.expiryCopy(expiryMs: 1_800_000_000_000 + 6 * 86_400_000, now: now)
                .hasPrefix("Expires "))
    }

    // MARK: - Devices

    func testDeviceRowTitleNeverShowsAKey() {
        // ADR-0058: the title comes from the catalog, the number passed as
        // text (so the formatter isolates it).
        let isolates = CharacterSet(charactersIn: "\u{2066}\u{2067}\u{2068}\u{2069}")
        func plain(_ text: String) -> String {
            String(text.unicodeScalars.filter { !isolates.contains($0) })
        }
        XCTAssertEqual(DeviceRowPresentation.title(isSelf: true, position: 1), "This device")
        XCTAssertEqual(plain(DeviceRowPresentation.title(isSelf: false, position: 3)), "Device 3")
    }
}
