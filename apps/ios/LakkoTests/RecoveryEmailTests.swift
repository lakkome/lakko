// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

// ADR-0075 / docs/ux/identity-recovery.md section 19 — the recovery email,
// the account without a saved phrase, the notices, and the phrase arm of
// "take the account back". Every flow runs over scripted ports: nothing
// here reaches a server.

@MainActor
final class RecoveryEmailTests: XCTestCase {

    private func status(
        available: Bool = true, enabled: Bool, totp: Bool? = false
    ) -> RecoveryEmailStatus {
        RecoveryEmailStatus(available: available, enabled: enabled, totp: totp)
    }

    private func defaults(_ name: String = #function) -> UserDefaults {
        let suite = "lakko.tests.recovery-email.\(name)"
        UserDefaults().removePersistentDomain(forName: suite)
        return UserDefaults(suiteName: suite)!
    }

    // MARK: - The account's mode

    func testTheStoreKeepsFlagsOnlyAndTheWipeClearsThem() {
        let defaults = defaults()
        let store = AccountProtectionStore(defaults: defaults)
        XCTAssertFalse(store.phraseSaved(accountIdB64: "acct"))
        XCTAssertNil(store.status(accountIdB64: "acct"))
        XCTAssertNil(store.noticeCursor(accountIdB64: "acct"), "a device that never kept a cursor")

        store.setStatus(status(enabled: true, totp: true), accountIdB64: "acct")
        store.setNoticeCursor(12, accountIdB64: "acct")
        store.markOwnAction(.emailAdded, atMs: 99, accountIdB64: "acct")
        // The status says nothing about the phrase any more (the server
        // keeps no flag): only the account's published protection does.
        XCTAssertFalse(store.phraseSaved(accountIdB64: "acct"))
        store.setPhraseSaved(true, accountIdB64: "acct")
        XCTAssertTrue(store.phraseSaved(accountIdB64: "acct"))
        XCTAssertEqual(store.status(accountIdB64: "acct")?.enabled, true)
        XCTAssertEqual(store.status(accountIdB64: "acct")?.totp, true)
        XCTAssertEqual(store.noticeCursor(accountIdB64: "acct"), 12)
        XCTAssertEqual(store.ownActions(accountIdB64: "acct"), ["recovery-email-added": 99])
        XCTAssertFalse(store.phraseSaved(accountIdB64: "other"), "per account")

        // Nothing stored could hold an address: every value is a flag, a
        // number, or the booleans of the status.
        for key in AccountProtectionStore.keys(accountIdB64: "acct") {
            let value = defaults.object(forKey: key)
            if let data = value as? Data {
                XCTAssertFalse(String(decoding: data, as: UTF8.self).contains("@"))
            }
        }

        store.clear(accountIdB64: "acct")
        for key in AccountProtectionStore.keys(accountIdB64: "acct") {
            XCTAssertNil(defaults.object(forKey: key), key)
        }
    }

    // MARK: - Three words only for a phrase-protected account

    func testNoThreeWordSurfaceWithoutASavedPhrase() {
        XCTAssertTrue(ThreeWords.offers(
            cachedUsername: "alice", phraseOnDevice: true, keysOnDevice: true, suspended: false,
            phraseSaved: true))
        // The device holds the words (it always does), but the person never
        // saw them: no row.
        XCTAssertFalse(ThreeWords.offers(
            cachedUsername: "alice", phraseOnDevice: true, keysOnDevice: true, suspended: false,
            phraseSaved: false))
    }

    func testThePasswordGateOpensForTenMinutesAfterItWasNotMe() {
        let now: Int64 = 1_000_000
        XCTAssertFalse(PasswordChangeGate.allowed(
            holdsLiveProof: false, threeWordsPassedAtMs: nil, noticeAnsweredAtMs: nil, nowMs: now))
        XCTAssertTrue(PasswordChangeGate.allowed(
            holdsLiveProof: false, threeWordsPassedAtMs: nil, noticeAnsweredAtMs: now - 1000, nowMs: now))
        XCTAssertFalse(PasswordChangeGate.allowed(
            holdsLiveProof: false, threeWordsPassedAtMs: nil,
            noticeAnsweredAtMs: now - PasswordChangeGate.threeWordsWindowMs, nowMs: now))
    }

    // MARK: - Notices

    private func notice(_ id: UInt64, _ kind: AccountNoticeKind, at ms: Int64 = 1_000) -> AccountNotice {
        AccountNotice(id: id, kind: kind.rawValue, atMs: ms)
    }

    func testEachKindIsShownOnceAndTheCursorCoversEverythingFetched() {
        let fetched = [
            notice(3, .emailAdded), notice(4, .passwordChangedByEmail), notice(5, .emailAdded),
            AccountNotice(id: 6, kind: "a-kind-of-the-future", atMs: 1),
            notice(2, .phraseSaved),
        ]
        let outcome = NoticeSelection.select(fetched: fetched, cursor: 2, ownActions: [:])
        // Below or at the cursor: seen. One per kind, the newest; oldest first.
        XCTAssertEqual(outcome.shown.map(\.id), [4, 5])
        XCTAssertEqual(outcome.cursor, 6, "an unknown kind is counted as seen")

        let nothing = NoticeSelection.select(fetched: [], cursor: 9, ownActions: [:])
        XCTAssertEqual(nothing.shown, [])
        XCTAssertEqual(nothing.cursor, 9)
    }

    func testADeviceWithoutACursorAsksForThirtyDaysAndShowsWhatComesBack() {
        let now: Int64 = 100 * 24 * 60 * 60 * 1000
        XCTAssertEqual(NoticeQuery.lookbackMs, 30 * 24 * 60 * 60 * 1000)
        XCTAssertEqual(NoticeQuery.first(cursor: nil, nowMs: now), .sinceMs(now - NoticeQuery.lookbackMs))
        XCTAssertEqual(NoticeQuery.first(cursor: 7, nowMs: now), .after(7))
        XCTAssertEqual(NoticeQuery.first(cursor: 0, nowMs: now), .after(0), "a kept cursor of zero is a cursor")
        XCTAssertEqual(NoticeQuery.sinceMs(5).queryItem, "since_ms=5")
        XCTAssertEqual(NoticeQuery.after(5).queryItem, "after=5")

        // A device with no cursor is new: nothing older is shown (D29).
        let outcome = NoticeSelection.select(
            fetched: [notice(41, .passwordChangedByEmail), notice(40, .emailAdded)], cursor: nil, ownActions: [:])
        XCTAssertEqual(outcome.shown, [])
        XCTAssertEqual(outcome.cursor, 41, "the newest id becomes its cursor")
        // Nothing came back: the cursor is kept all the same.
        XCTAssertEqual(NoticeSelection.select(fetched: [], cursor: nil, ownActions: [:]).cursor, 0)
    }

    func testADeviceIsNotShownItsOwnActionAndAMarkerIsSpentOnOneNotice() {
        let window = NoticeSelection.ownActionWindowMs
        XCTAssertEqual(window, 10 * 60 * 1000, "ten minutes")
        let fetched = [
            notice(1, .emailAdded, at: 50_000),
            notice(2, .passwordChangedByEmail, at: 50_000),
            notice(3, .emailRemoved, at: 50_000 + window + 1),
            // A second "added" inside the window: someone else's.
            notice(4, .emailAdded, at: 60_000),
        ]
        let outcome = NoticeSelection.select(
            fetched: fetched, cursor: 0,
            ownActions: [
                AccountNoticeKind.emailAdded.rawValue: 49_000,
                // It removed an email too, but long before this notice.
                AccountNoticeKind.emailRemoved.rawValue: 50_000,
            ])
        // One marker, one notice: the first "added" is this device's, the
        // second is shown.
        XCTAssertEqual(outcome.shown.map(\.id), [2, 3, 4])
        XCTAssertEqual(outcome.spentMarkers, [AccountNoticeKind.emailAdded.rawValue])
        XCTAssertEqual(outcome.cursor, 4, "its own notice is seen, not shown")
        // Exactly at the edge of the window it is still this device's.
        XCTAssertTrue(NoticeSelection.isOwn(
            notice(9, .emailAdded, at: 49_000 + window),
            ownActions: [AccountNoticeKind.emailAdded.rawValue: 49_000]))
        // The marker is per kind.
        XCTAssertFalse(NoticeSelection.isOwn(
            notice(9, .emailRemoved, at: 49_000),
            ownActions: [AccountNoticeKind.emailAdded.rawValue: 49_000]))
    }

    func testEveryKindHasItsCopyAndTheSecureScreenItsActions() {
        for kind in AccountNoticeKind.allCases {
            let copy = AccountNoticeCopy.of(kind)
            XCTAssertNotEqual(Localization.text(copy.titleKey), copy.titleKey, "\(kind) has a string")
            if let body = copy.bodyKey {
                XCTAssertTrue(Localization.text(body, texts: ["date": "D"]).contains("D"))
            }
        }
        XCTAssertNotNil(AccountNoticeCopy.of(.passwordChangedByEmail).bodyKey)
        XCTAssertNil(AccountNoticeCopy.of(.emailAdded).bodyKey)
        // The "account opened by email" notice went with the email vault.
        XCTAssertNil(AccountNoticeKind(rawValue: "account-opened-by-email"))

        // In order: a new password, remove the email, save a phrase.
        XCTAssertEqual(
            SecureAccountActions.offered(status(enabled: true), phraseSaved: false),
            [.newPassword, .removeEmail, .savePhrase])
        XCTAssertEqual(
            SecureAccountActions.offered(status(enabled: true), phraseSaved: true),
            [.newPassword, .removeEmail], "no way back from a saved phrase, nothing to save")
        XCTAssertEqual(
            SecureAccountActions.offered(status(enabled: false), phraseSaved: false),
            [.newPassword, .savePhrase])
    }

    // MARK: - The centre

    private final class Server {
        var status: Result<RecoveryEmailStatus, Error>
        var pages: [NoticeQuery: AccountNoticePage] = [:]
        var now: Int64 = 1_000_000
        var statusReads = 0
        var noticeReads: [NoticeQuery] = []
        /// The account's published protection, as a look at it answers.
        var seals: AccountSeals? = AccountSeals.passwordOnly
        var sealLooks: [SealChange] = []
        var hasPasskey: Bool? = false
        var hasUsername = true
        var afterKeyReset = false
        init(_ status: RecoveryEmailStatus) { self.status = .success(status) }
    }

    private func center(
        _ server: Server, store: AccountProtectionStore, session: Bool = true
    ) -> AccountProtectionCenter {
        AccountProtectionCenter(
            ports: .init(
                session: {
                    session
                        ? .init(accountIdB64: "acct", sessionToken: "tok", hasUsername: server.hasUsername) : nil
                },
                fetchStatus: { _ in
                    server.statusReads += 1
                    return try server.status.get()
                },
                fetchNotices: { _, query in
                    server.noticeReads.append(query)
                    return server.pages[query] ?? AccountNoticePage(notices: [], nextAfter: 0, more: false)
                },
                hasPasskey: { server.hasPasskey },
                ensureSeals: { change in
                    server.sealLooks.append(change)
                    return server.seals
                },
                afterKeyReset: { server.afterKeyReset },
                nowMs: { server.now }),
            store: store,
            sealsStore: AccountSealsStore(defaults: defaults("seals-\(#function)")))
    }

    func testTheStatusIsReadAtTheLandingAndKeptForTheNextOne() async {
        let store = AccountProtectionStore(defaults: defaults())
        let server = Server(status(enabled: true, totp: false))
        let center = center(server, store: store)
        XCTAssertNil(center.rows)

        await center.land(accountIdB64: "acct")
        XCTAssertEqual(center.status?.enabled, true)
        XCTAssertEqual(store.status(accountIdB64: "acct")?.enabled, true)
        XCTAssertEqual(center.factors, AccountFactors(email: true, authenticator: false, passkey: false))
        XCTAssertFalse(center.factorStepRequired)
        XCTAssertEqual(server.sealLooks, [.landing], "the account's protection is looked at once")

        // The next landing shows the stored status at once; a server that
        // does not answer keeps it.
        server.status = .failure(URLError(.notConnectedToInternet))
        let next = self.center(server, store: store)
        await next.land(accountIdB64: "acct")
        XCTAssertEqual(next.status?.enabled, true)
    }

    func testNoticesAreShownOnceAndAnsweredOnesAreNotFetchedAgain() async {
        let store = AccountProtectionStore(defaults: defaults())
        let server = Server(status(enabled: true))
        // A kept cursor below the notices: they are new to this device.
        store.setNoticeCursor(0, accountIdB64: "acct")
        server.pages[.after(0)] = AccountNoticePage(
            notices: [notice(1, .emailAdded), notice(2, .passwordChangedByEmail)], nextAfter: 2, more: true)
        server.pages[.after(2)] = AccountNoticePage(
            notices: [notice(3, .emailRemoved)], nextAfter: 3, more: false)
        let center = center(server, store: store)
        await center.land(accountIdB64: "acct")

        XCTAssertEqual(server.noticeReads, [.after(0), .after(2)])
        XCTAssertEqual(center.notices.map(\.id), [1, 2, 3], "shown, not adopted silently")
        // Nothing answered yet: all of them are read again at the next
        // landing, and a cursor is kept from here on.
        XCTAssertEqual(store.noticeCursor(accountIdB64: "acct"), 0)

        center.acknowledge(center.notices[0])
        XCTAssertEqual(store.noticeCursor(accountIdB64: "acct"), 1)
        XCTAssertFalse(center.secureRequested)

        // `It was not me` answers the notice and opens the secure screen.
        center.wasNotMe(center.notices[0])
        XCTAssertTrue(center.secureRequested)
        XCTAssertEqual(center.notices.map(\.id), [3])
        XCTAssertEqual(store.noticeCursor(accountIdB64: "acct"), 2)

        center.acknowledge(center.notices[0])
        XCTAssertEqual(center.notices, [])
        XCTAssertEqual(store.noticeCursor(accountIdB64: "acct"), 3)
    }

    func testANewDeviceShowsNothingOlderAndKeepsTheNewestIdAsItsCursor() async {
        let store = AccountProtectionStore(defaults: defaults())
        let server = Server(status(enabled: true))
        let first = NoticeQuery.sinceMs(server.now - NoticeQuery.lookbackMs)
        server.pages[first] = AccountNoticePage(
            notices: [notice(1, .emailAdded), notice(2, .emailReplaced)], nextAfter: 2, more: true)
        server.pages[.after(2)] = AccountNoticePage(
            notices: [notice(3, .emailRemoved)], nextAfter: 3, more: false)
        let center = center(server, store: store)
        await center.land(accountIdB64: "acct")

        XCTAssertEqual(center.notices, [])
        XCTAssertEqual(store.noticeCursor(accountIdB64: "acct"), 3)

        // The next one after the cursor is a real notice.
        server.pages[.after(3)] = AccountNoticePage(
            notices: [notice(4, .emailRemoved)], nextAfter: 4, more: false)
        await center.loadNotices()
        XCTAssertEqual(center.notices.map(\.id), [4])
    }

    func testARecoveryOnAFreshDeviceIsNotToldAboutItself() async {
        let store = AccountProtectionStore(defaults: defaults())
        let server = Server(status(enabled: true))
        server.pages[.sinceMs(server.now - NoticeQuery.lookbackMs)] = AccountNoticePage(
            notices: [notice(1, .passwordChangedByEmail, at: server.now + 2_000)], nextAfter: 1, more: false)
        let center = center(server, store: store)

        // The account is not known yet when the password is changed.
        center.noteOwnAction(.passwordChangedByEmail)
        XCTAssertEqual(store.ownActions(accountIdB64: "acct"), [:])
        await center.land(accountIdB64: "acct")

        XCTAssertEqual(center.notices, [], "this device did it")
        XCTAssertEqual(store.noticeCursor(accountIdB64: "acct"), 1)
        XCTAssertEqual(store.ownActions(accountIdB64: "acct"), [:], "the marker is spent")
    }

    /// UX 19.5 — the server keeps no notice for a saved phrase since
    /// 2026-11-26: a kept device says it once when the account's published
    /// protection first lists the phrase.
    func testAKeptDeviceLearnsThatAPhraseWasSavedFromTheAccountsProtection() async {
        let store = AccountProtectionStore(defaults: defaults())
        let server = Server(status(enabled: true))
        let center = center(server, store: store)
        await center.land(accountIdB64: "acct")
        XCTAssertFalse(center.phraseSaved)
        XCTAssertEqual(center.notices, [])

        server.seals = AccountSeals(phraseSaved: true, passkeyTagsB64: [])
        await center.readSeals(.landing)
        XCTAssertTrue(center.phraseSaved)
        XCTAssertEqual(center.notices.map(\.kind), [AccountNoticeKind.phraseSaved.rawValue])
        // Looked at again, it is not said twice; answered, it is gone.
        await center.readSeals(.landing)
        XCTAssertEqual(center.notices.count, 1)
        center.acknowledge(center.notices[0])
        XCTAssertEqual(center.notices, [])
    }

    func testTheDeviceThatSavedThePhraseIsNotToldAboutIt() async {
        let store = AccountProtectionStore(defaults: defaults())
        let server = Server(status(enabled: true))
        let center = center(server, store: store)
        await center.land(accountIdB64: "acct")

        // The three-word check passed; the look publishes the phrase seal.
        server.seals = AccountSeals(phraseSaved: true, passkeyTagsB64: [])
        let saved = await center.savePhraseSeal()
        XCTAssertTrue(saved)
        XCTAssertEqual(server.sealLooks.last, .phraseSaved)
        XCTAssertTrue(store.phraseSaved(accountIdB64: "acct"))
        XCTAssertEqual(center.notices, [], "this device did it")
    }

    func testASaveThatDidNotPublishSavedNothing() async {
        let store = AccountProtectionStore(defaults: defaults())
        let server = Server(status(enabled: true))
        let center = center(server, store: store)
        await center.land(accountIdB64: "acct")
        // The look could not publish: the announcement still lists none.
        server.seals = nil
        let saved = await center.savePhraseSeal()
        XCTAssertFalse(saved)
        XCTAssertFalse(store.phraseSaved(accountIdB64: "acct"))
        XCTAssertEqual(store.ownActions(accountIdB64: "acct"), [:], "no marker is left behind")
    }

    // UX 19.1c — the landing rule.

    func testAnAccountWithAUsernameAndNoSecondWayInIsShownTheStepBeforeTheApp() async {
        let server = Server(status(enabled: false, totp: false))
        let center = center(server, store: AccountProtectionStore(defaults: defaults()))
        await center.land(accountIdB64: "acct")
        XCTAssertTrue(center.factorStepRequired)
        XCTAssertFalse(center.passwordOnlyDialogDue, "the step comes first")

        // `Continue` is refused while none of the three is there...
        center.factorStepDone()
        XCTAssertTrue(center.factorStepRequired)
        // ...adding one does not take the step away by itself (the person
        // may add another), `Continue` does.
        center.authenticatorAdded()
        XCTAssertTrue(center.factorStepRequired)
        center.factorStepDone()
        XCTAssertFalse(center.factorStepRequired)
    }

    /// Alignment item G — the landing settles once the factor read
    /// answered (the app's content waits for it, at most three seconds), and
    /// the step is decided only from an answer: a read that failed shows no
    /// step from the stored status.
    func testTheLandingSettlesOnTheFactorReadAndAFailedReadShowsNoStep() async {
        let server = Server(status(enabled: false, totp: false))
        let landing = center(server, store: AccountProtectionStore(defaults: defaults()))
        XCTAssertNil(landing.landingSettled)
        await landing.land(accountIdB64: "acct")
        XCTAssertEqual(landing.landingSettled, "acct")
        XCTAssertTrue(landing.factorStepRequired)

        let store = AccountProtectionStore(defaults: defaults("stored-none"))
        let first = center(Server(status(enabled: false, totp: false)), store: store)
        await first.land(accountIdB64: "acct")
        XCTAssertTrue(first.factorStepRequired, "stored: no second way in")
        let offline = Server(status(enabled: false, totp: false))
        offline.status = .failure(URLError(.notConnectedToInternet))
        let second = center(offline, store: store)
        await second.land(accountIdB64: "acct")
        XCTAssertEqual(second.landingSettled, "acct", "a failed read settles too: the app shows")
        XCTAssertFalse(second.factorStepRequired, "the step only on an answer")

        XCTAssertLessThanOrEqual(AccountProtectionCenter.landingWaitNanos, 3_000_000_000)
    }

    func testTheStepIsNotShownToAGuestNorWhileTheFactorsAreUnknown() async {
        let guest = Server(status(enabled: false, totp: false))
        guest.hasUsername = false
        let first = center(guest, store: AccountProtectionStore(defaults: defaults()))
        await first.land(accountIdB64: "acct")
        XCTAssertFalse(first.factorStepRequired)

        // The passkey list could not be read: nothing is decided.
        let unknown = Server(status(enabled: false, totp: false))
        unknown.hasPasskey = nil
        let second = center(unknown, store: AccountProtectionStore(defaults: defaults("unknown")))
        await second.land(accountIdB64: "acct")
        XCTAssertFalse(second.factorStepRequired)

        // A passkey is one of the three.
        let passkey = Server(status(enabled: false, totp: false))
        passkey.hasPasskey = true
        let third = center(passkey, store: AccountProtectionStore(defaults: defaults("passkey")))
        await third.land(accountIdB64: "acct")
        XCTAssertFalse(third.factorStepRequired)
    }

    // UX 19.1d place 3 — the once-per-device dialog.

    func testThePasswordOnlyDialogIsDueOnceOnADeviceAndNotAfterAReset() async {
        let seals = "dialog-seals"
        func make(_ server: Server) -> AccountProtectionCenter {
            AccountProtectionCenter(
                ports: .init(
                    session: { .init(accountIdB64: "acct", sessionToken: "tok") },
                    fetchStatus: { _ in try server.status.get() },
                    fetchNotices: { _, _ in AccountNoticePage(notices: [], nextAfter: 0, more: false) },
                    hasPasskey: { server.hasPasskey },
                    ensureSeals: { _ in server.seals },
                    afterKeyReset: { server.afterKeyReset },
                    nowMs: { server.now }),
                store: AccountProtectionStore(defaults: defaults("dialog-store")),
                sealsStore: AccountSealsStore(defaults: defaults(seals)))
        }
        // A second way in, no phrase, no passkey that keeps the chats.
        let server = Server(status(enabled: true))
        let first = make(server)
        await first.land(accountIdB64: "acct")
        XCTAssertTrue(first.passwordOnly)
        XCTAssertTrue(first.passwordOnlyDialogDue)
        first.closePasswordOnlyDialog()
        XCTAssertFalse(first.passwordOnlyDialogDue)

        // Closed for good on this device.
        let again = AccountProtectionCenter(
            ports: .init(
                session: { .init(accountIdB64: "acct", sessionToken: "tok") },
                fetchStatus: { _ in try server.status.get() },
                fetchNotices: { _, _ in AccountNoticePage(notices: [], nextAfter: 0, more: false) },
                hasPasskey: { server.hasPasskey },
                ensureSeals: { _ in server.seals },
                nowMs: { server.now }),
            store: AccountProtectionStore(defaults: UserDefaults(suiteName: "lakko.tests.recovery-email.dialog-store")!),
            sealsStore: AccountSealsStore(defaults: UserDefaults(suiteName: "lakko.tests.recovery-email.\(seals)")!))
        await again.land(accountIdB64: "acct")
        XCTAssertTrue(again.passwordOnly, "the notice in Settings stays")
        XCTAssertFalse(again.passwordOnlyDialogDue)

        // Not after a key reset: its own screens said what was kept.
        let reset = Server(status(enabled: true))
        reset.afterKeyReset = true
        let afterReset = center(reset, store: AccountProtectionStore(defaults: defaults("after-reset")))
        await afterReset.land(accountIdB64: "acct")
        XCTAssertFalse(afterReset.passwordOnlyDialogDue)

        // Not when a phrase is saved, nor when a passkey keeps the chats.
        let kept = Server(status(enabled: true))
        kept.seals = AccountSeals(phraseSaved: true, passkeyTagsB64: [])
        let keptCenter = center(kept, store: AccountProtectionStore(defaults: defaults("kept")))
        await keptCenter.land(accountIdB64: "acct")
        XCTAssertFalse(keptCenter.passwordOnly)
        XCTAssertFalse(keptCenter.passwordOnlyDialogDue)
    }

    func testAPasskeyThatGaveItsSecretIsPublishedAndARemovedOneIsTakenOut() async {
        let server = Server(status(enabled: true))
        let center = center(server, store: AccountProtectionStore(defaults: defaults()))
        await center.land(accountIdB64: "acct")
        let id = Data([1, 2, 3]), secret = Data(repeating: 9, count: 32)

        // No secret: the passkey signs in and keeps nothing.
        center.passkeyAdded()
        XCTAssertEqual(center.hasPasskey, true)
        XCTAssertEqual(server.sealLooks, [.landing])

        server.seals = AccountSeals(
            phraseSaved: false, passkeyTagsB64: [resetSealPasskeyTag(credentialId: id).base64EncodedString()])
        center.passkeyAdded(credentialId: id, prfOutput: secret)
        await settle { server.sealLooks.count == 2 }
        XCTAssertEqual(server.sealLooks.last, .passkeyAdded(credentialId: id, prfOutput: secret))
        XCTAssertTrue(center.keepsChats)
        XCTAssertFalse(center.passwordOnly)

        server.seals = AccountSeals.passwordOnly
        center.passkeyRemoved(credentialId: id, remaining: 0)
        await settle { server.sealLooks.count == 3 }
        XCTAssertEqual(server.sealLooks.last, .passkeyRemoved(credentialId: id))
        XCTAssertEqual(center.hasPasskey, false)
        XCTAssertTrue(center.passwordOnly)
    }

    /// A secret that could not be published when the passkey gave it (the
    /// directory was out, or the account's first row was not there yet) is
    /// published at the next look that succeeds, once.
    func testAPasskeySecretThatCouldNotBePublishedIsPublishedAtTheNextLook() async {
        let server = Server(status(enabled: true))
        let center = center(server, store: AccountProtectionStore(defaults: defaults()))
        await center.land(accountIdB64: "acct")
        let id = Data([5, 5]), secret = Data(repeating: 7, count: 32)
        let added = SealChange.passkeyAdded(credentialId: id, prfOutput: secret)

        server.seals = nil
        center.passkeyAdded(credentialId: id, prfOutput: secret)
        await settle { server.sealLooks.count == 2 }
        XCTAssertEqual(server.sealLooks, [.landing, added])
        XCTAssertFalse(center.keepsChats)

        server.seals = AccountSeals(
            phraseSaved: false, passkeyTagsB64: [resetSealPasskeyTag(credentialId: id).base64EncodedString()])
        await center.readSeals(.landing)
        XCTAssertEqual(server.sealLooks, [.landing, added, .landing, added])
        XCTAssertTrue(center.keepsChats)
        await center.readSeals(.landing)
        XCTAssertEqual(server.sealLooks.filter { $0 == added }.count, 2, "published once")

        // Leaving the required step looks again by itself (a sign-up's
        // landing can run before the account's first directory row exists).
        let looks = server.sealLooks.count
        center.factorStepDone()
        await settle { server.sealLooks.count == looks + 1 }
        XCTAssertEqual(server.sealLooks.last, .landing)
    }

    private func settle(_ done: @escaping () -> Bool) async {
        for _ in 0..<200 where !done() { await Task.yield() }
        // One more turn: the look's result is applied after it returned.
        for _ in 0..<20 { await Task.yield() }
    }

    func testWithoutASessionNothingIsAsked() async {
        let server = Server(status(enabled: false))
        let center = center(server, store: AccountProtectionStore(defaults: defaults()), session: false)
        await center.refresh()
        await center.loadNotices()
        await center.readSeals(.landing)
        XCTAssertEqual(server.statusReads, 0)
        XCTAssertEqual(server.noticeReads, [])
        XCTAssertEqual(server.sealLooks, [])
    }

    // MARK: - The recover step machine

    func testStepTwoIsTheSameWhateverWasTyped() {
        // The transition takes nothing that could tell a match from a miss.
        let first = EmailRecover.started(EmailRecover.State())
        XCTAssertEqual(first.step, .code)
        XCTAssertNil(first.line)
        // `Send again` from the code step with a line on screen: the same.
        var withLine = first
        withLine.line = .codeWrong
        XCTAssertEqual(EmailRecover.started(withLine), first)
    }

    func testTheCodeStepRefusals() {
        let code = EmailRecover.started(EmailRecover.State())
        // A wrong code and one never sent: one line, the step stays.
        let wrong = EmailRecover.verifyRefused(code, .codeRefused)
        XCTAssertEqual(wrong.step, .code)
        XCTAssertEqual(wrong.line, .codeWrong)
        XCTAssertEqual(wrong.line?.key, "recovery_email.code.wrong")
        // Five tries per code: back to the address step.
        let usedUp = EmailRecover.verifyRefused(wrong, .tooManyAttempts(retryAfterSecs: nil))
        XCTAssertEqual(usedUp.step, .start)
        XCTAssertEqual(usedUp.line?.key, "recovery_email.code.used_up")
        XCTAssertEqual(EmailRecover.verifyRefused(code, .codeExpired).line?.key, "recovery_email.code.expired")
        XCTAssertEqual(EmailRecover.verifyRefused(code, .offline).line?.key, "common.error.offline")
        XCTAssertEqual(EmailRecover.verifyRefused(code, .unavailable).line?.key, "recovery_email.unavailable")
        // Step 1's refusals are about what was typed, never whose it is.
        XCTAssertEqual(
            EmailRecover.startRefused(EmailRecover.State(), .addressInvalid).line?.key, "recovery_email.invalid")
        let limited = EmailRecover.startRefused(code, .lettersLimited(retryAfterSecs: 45))
        XCTAssertEqual(limited.step, .code, "`Send again` keeps the step")
        XCTAssertEqual(limited.line, .limited(retryAfterSecs: 45))
        XCTAssertNil(limited.line?.key, "the countdown draws it")
        XCTAssertEqual(
            RecoveryEmailLine.limited(retryAfterSecs: nil).key, "common.error.too_many_attempts")
    }

    func testTheAuthenticatorStepOnlyWhenTheAccountHasOne() {
        let code = EmailRecover.started(EmailRecover.State())
        XCTAssertEqual(EmailRecover.verified(code, totpRequired: true).step, .totp)
        XCTAssertEqual(EmailRecover.verified(code, totpRequired: false).step, .newPassword)

        let totp = EmailRecover.verified(code, totpRequired: true)
        XCTAssertEqual(EmailRecover.totpAccepted(totp).step, .newPassword)
        let wrong = EmailRecover.ticketRouteRefused(totp, .codeRefused)
        XCTAssertEqual(wrong.step, .totp)
        XCTAssertEqual(wrong.line, .codeWrong)
        // There is no way past it: the password route sends the person
        // back to it.
        let saving = EmailRecover.saving(EmailRecover.verified(code, totpRequired: false))
        XCTAssertEqual(EmailRecover.ticketRouteRefused(saving, .totpRequired).step, .totp)
        // A ticket that ran out: a new letter.
        let expired = EmailRecover.ticketRouteRefused(totp, .ticketRefused)
        XCTAssertEqual(expired.step, .start)
        XCTAssertEqual(expired.line, .codeExpired)
    }

    func testEveryRefusalOfTheRoutesIsMapped() {
        func of(_ status: Int, _ code: String, retry: Int? = nil) -> RecoveryEmailRefusal {
            let retryField = retry.map { #","retry_after_secs":\#($0)"# } ?? ""
            return RecoveryEmailRefusal.of(status: status, body: #"{"error":"\#(code)"\#(retryField)}"#)
        }
        XCTAssertEqual(of(401, "code-refused"), .codeRefused)
        XCTAssertEqual(of(401, "ticket-refused"), .ticketRefused)
        XCTAssertEqual(of(410, "code-expired"), .codeExpired)
        XCTAssertEqual(of(403, "totp-required"), .totpRequired)
        XCTAssertEqual(of(403, "fresh-auth-required"), .freshAuthRequired)
        XCTAssertEqual(of(400, "address-invalid"), .addressInvalid)
        XCTAssertEqual(of(429, "email-rate-limited", retry: 30), .lettersLimited(retryAfterSecs: 30))
        XCTAssertEqual(of(429, "too-many-attempts"), .tooManyAttempts(retryAfterSecs: nil))
        XCTAssertEqual(of(409, "handle-mismatch"), .handleMismatch)
        XCTAssertEqual(of(409, "username-required"), .usernameRequired)
        XCTAssertEqual(of(409, "last-factor"), .lastFactor)
        // The codes of the removed email vault are no longer told apart.
        XCTAssertEqual(of(409, "phrase-saved"), .failed)
        XCTAssertEqual(of(409, "wrong-mode"), .failed)
        XCTAssertEqual(of(502, "mail-send-failed"), .mailSendFailed)
        XCTAssertEqual(of(503, "recovery-email-disabled"), .unavailable)
        XCTAssertEqual(of(503, "rate-limiter-unavailable"), .failed)
        XCTAssertEqual(of(500, "internal"), .failed)
        XCTAssertEqual(
            RecoveryEmailRefusal.of(AuthClientError.network(endpoint: "/x", underlying: URLError(.timedOut))),
            .offline)
        XCTAssertEqual(RecoveryEmailRefusal.of(FreshAuthRequired()), .freshAuthRequired)
    }

    // MARK: - The recover model

    private final class RecoverWorld {
        var startAnswer: Result<EmailChallenge, Error> = .success(
            EmailChallenge(challengeId: "ch", resendAfterSecs: 45, expiresInSecs: 600))
        var verifyAnswer: Result<EmailTicket, Error> = .success(
            EmailTicket(ticket: "tk", totpRequired: false, expiresInSecs: 600))
        var totpError: Error?
        var saveError: Error?
        var calls: [String] = []
        var started: [(handle: Data, address: String)] = []
        var kept: [String] = []
        var changed: [(username: String, ticket: String, password: Data)] = []
        var own: [AccountNoticeKind] = []
    }

    private struct Scripted: Error {}

    private func http(_ status: Int, _ code: String) -> AuthClientError {
        .http(status: status, endpoint: "/recovery-email", body: #"{"error":"\#(code)"}"#)
    }

    private func recoverModel(_ world: RecoverWorld, cachedUsername: String? = nil) -> EmailRecoverModel {
        EmailRecoverModel(
            ports: .init(
                normalize: { try normalizeRecoveryEmail(address: $0) },
                handle: { try LoginName.make($0, emptyError: Scripted()) },
                start: { handle, address in
                    world.calls.append("start")
                    world.started.append((handle, address))
                    return try world.startAnswer.get()
                },
                verify: { _, _ in
                    world.calls.append("verify")
                    return try world.verifyAnswer.get()
                },
                totp: { _, _ in
                    world.calls.append("totp")
                    if let error = world.totpError { throw error }
                },
                changePassword: { username, ticket, password in
                    world.calls.append("password")
                    if let error = world.saveError { throw error }
                    world.changed.append((username, ticket, password))
                },
                noteOwnAction: { world.own.append($0) },
                keepAddress: { world.kept.append($0) }),
            cachedUsername: cachedUsername,
            nowMs: { 1_000 })
    }

    private func typeAndStart(_ model: EmailRecoverModel, address: String = " Alice@Example.com ") async {
        model.username = "alice"
        model.address = address
        await model.start()
    }

    func testTheModelShowsStepTwoIdenticallyForAnyAnswerAndNeverPrefillsTheAddress() async {
        let world = RecoverWorld()
        let model = recoverModel(world, cachedUsername: "alice")
        XCTAssertEqual(model.username, "alice", "the username is pre-filled")
        XCTAssertEqual(model.address, "", "the address never is")

        await typeAndStart(model)
        let matched = model.state
        XCTAssertEqual(matched.step, .code)
        // The normalised address and the username's handle went out.
        XCTAssertEqual(world.started.first?.address, "alice@example.com")
        XCTAssertEqual(world.started.first?.handle, try? loginHandle(username: "alice"))
        XCTAssertTrue(model.resend.waiting, "Send again counts down")

        // A different pair: the server answers the same shape, the screen is the same.
        let other = recoverModel(world)
        other.username = "nobody_here"
        other.address = "someone@else.example"
        await other.start()
        XCTAssertEqual(other.state, matched)
    }

    /// A mailed code changes the password and nothing else (the email vault
    /// is gone since 2026-11-26): no route opens anything.
    func testTheCodeChangesThePasswordUnderTheTicketAndBringsNoKeys() async {
        let world = RecoverWorld()
        world.verifyAnswer = .success(EmailTicket(ticket: "tk-7", totpRequired: true, expiresInSecs: 600))
        let model = recoverModel(world)
        await typeAndStart(model)
        model.code = "123456"
        await model.verify()

        // The authenticator step comes only after the mailed code.
        XCTAssertEqual(model.state.step, .totp)
        XCTAssertEqual(world.calls, ["start", "verify"])

        world.totpError = http(401, "code-refused")
        model.totpCode = "000000"
        await model.submitTotp()
        XCTAssertEqual(model.state.step, .totp)
        XCTAssertEqual(model.state.line, .codeWrong)

        world.totpError = nil
        model.totpCode = "654321"
        await model.submitTotp()
        XCTAssertEqual(model.state.step, .newPassword)
        XCTAssertEqual(world.calls, ["start", "verify", "totp", "totp"])

        // The password rule, before anything is sent.
        model.password = "short"
        model.confirmPassword = "short"
        await model.save()
        XCTAssertNotNil(model.passwordError)
        XCTAssertFalse(world.calls.contains("password"))

        model.password = "long enough"
        model.confirmPassword = "long enough"
        await model.save()
        XCTAssertEqual(world.calls.last, "password")
        XCTAssertEqual(world.calls.filter { $0 == "password" }.count, 1)
        XCTAssertEqual(world.changed.first?.ticket, "tk-7")
        XCTAssertEqual(world.changed.first?.username, "alice")
        XCTAssertEqual(world.own, [.passwordChangedByEmail])
    }

    func testWrongCodesAndLimitsOnTheModel() async {
        let world = RecoverWorld()
        let model = recoverModel(world)

        // An address that does not parse is refused before any request.
        model.username = "alice"
        model.address = "not an address"
        XCTAssertFalse(model.canStart)
        await model.start()
        XCTAssertEqual(world.calls, [])
        XCTAssertEqual(model.state.line, .addressInvalid)

        // A username that breaks the rule: its reason, no request.
        model.username = "A B"
        model.address = "alice@example.com"
        await model.start()
        XCTAssertNotNil(model.usernameError)
        XCTAssertEqual(world.calls, [])

        await typeAndStart(model)
        world.verifyAnswer = .failure(http(401, "code-refused"))
        model.code = "111111"
        await model.verify()
        XCTAssertEqual(model.state.step, .code)
        XCTAssertEqual(model.state.line, .codeWrong)
        XCTAssertEqual(model.code, "", "the field clears")

        // Five tries per code: back to step 1 with a new-letter line.
        world.verifyAnswer = .failure(AuthClientError.http(
            status: 429, endpoint: "/v", body: #"{"error":"too-many-attempts","retry_after_secs":60}"#))
        model.code = "222222"
        await model.verify()
        XCTAssertEqual(model.state.step, .start)
        XCTAssertEqual(model.state.line, .codeUsedUp)
        XCTAssertTrue(model.wait.waiting)

        // Mail is not set up on this server.
        let down = RecoverWorld()
        down.startAnswer = .failure(http(503, "recovery-email-disabled"))
        let unavailable = recoverModel(down)
        await typeAndStart(unavailable)
        XCTAssertEqual(unavailable.state.step, .start)
        XCTAssertEqual(unavailable.state.line?.key, "recovery_email.unavailable")
    }

    // MARK: - The enrol machine and model

    func testTheEnrolMachine() {
        let code = EmailEnrol.started(EmailEnrol.State())
        XCTAssertEqual(code.step, .code)

        var state = code
        for _ in 0..<(EmailEnrol.triesPerCode - 1) {
            state = EmailEnrol.finishRefused(state, .codeRefused)
            XCTAssertEqual(state.step, .code)
            XCTAssertEqual(state.line, .codeWrong)
        }
        // The fifth wrong code: back to the address step.
        state = EmailEnrol.finishRefused(state, .codeRefused)
        XCTAssertEqual(state.step, .address)
        XCTAssertEqual(state.line, .codeUsedUp)

        let expired = EmailEnrol.finishRefused(code, .codeExpired)
        XCTAssertEqual(expired.step, .code, "Send again is on this step")
        XCTAssertEqual(expired.line, .codeExpired)

        // The proof ran out: the block appears, with no error line.
        let proof = EmailEnrol.startRefused(EmailEnrol.State(), .freshAuthRequired)
        XCTAssertTrue(proof.proofNeeded)
        XCTAssertNil(proof.line)
        XCTAssertEqual(
            EmailEnrol.startRefused(EmailEnrol.State(), .mailSendFailed).line?.key, "recovery_email.send_failed")
        XCTAssertEqual(
            EmailEnrol.startRefused(EmailEnrol.State(), .usernameRequired).line?.key,
            "recovery_email.needs_username")
        XCTAssertEqual(
            EmailEnrol.startRefused(EmailEnrol.State(), .lettersLimited(retryAfterSecs: 9)).line,
            .limited(retryAfterSecs: 9))
        XCTAssertEqual(EmailEnrol.finished(code).step, .done)
        XCTAssertEqual(EmailEnrol.otherAddress(code), EmailEnrol.State())

        // `Send code` is disabled until the address parses.
        XCTAssertTrue(EmailEnrol.canSend(addressValid: true, busy: false, waiting: false, proofReady: true))
        XCTAssertFalse(EmailEnrol.canSend(addressValid: false, busy: false, waiting: false, proofReady: true))
        XCTAssertFalse(EmailEnrol.canSend(addressValid: true, busy: true, waiting: false, proofReady: true))
        XCTAssertFalse(EmailEnrol.canSend(addressValid: true, busy: false, waiting: true, proofReady: true))
        XCTAssertFalse(EmailEnrol.canSend(addressValid: true, busy: false, waiting: false, proofReady: false))

        XCTAssertEqual(EmailEnrol.normalizedCode(" 123 456 "), "123456")
        XCTAssertNil(EmailEnrol.normalizedCode("12345"))
        XCTAssertNil(EmailEnrol.normalizedCode("12345a"))
    }

    private final class EnrolWorld {
        var handle: Data? = Data(repeating: 7, count: 32)
        var grant: String? = "grant"
        var startError: Error?
        var finishErrors: [Error] = []
        var removeError: Error?
        var calls: [String] = []
        var started: [(grant: String, handle: Data, address: String)] = []
        var finished: [(challengeId: String, code: String)] = []
        var enrolled: [(EmailEnrolled, Bool)] = []
        var keptAddresses: [String] = []
        var removed = 0
        var proofExpired = 0
    }

    private func enrolModel(_ world: EnrolWorld, replacing: Bool = false) -> RecoveryEmailModel {
        RecoveryEmailModel(
            ports: .init(
                loginHandle: { world.handle },
                sessionToken: { "tok" },
                grant: { world.grant },
                normalize: { try normalizeRecoveryEmail(address: $0) },
                start: { _, grant, handle, address in
                    world.calls.append("start")
                    if let error = world.startError { throw error }
                    world.started.append((grant, handle, address))
                    return EmailChallenge(challengeId: "ch", resendAfterSecs: 45, expiresInSecs: 600)
                },
                finish: { _, challengeId, code in
                    world.calls.append("finish")
                    if !world.finishErrors.isEmpty { throw world.finishErrors.removeFirst() }
                    world.finished.append((challengeId, code))
                    return EmailEnrolled(enabled: true)
                },
                remove: { _, _ in
                    world.calls.append("remove")
                    if let error = world.removeError { throw error }
                },
                enrolled: { result, replaced, address in
                    world.enrolled.append((result, replaced))
                    world.keptAddresses.append(address)
                },
                removed: { world.removed += 1 },
                proofExpired: { world.proofExpired += 1 }),
            replacing: replacing,
            nowMs: { 1_000 })
    }

    func testEnrolStatesOnTheModel() async {
        // A guest account: nothing is sent.
        let guest = EnrolWorld()
        guest.handle = nil
        let guestModel = enrolModel(guest)
        XCTAssertTrue(guestModel.needsUsername)
        guestModel.address = "alice@example.com"
        await guestModel.send()
        XCTAssertEqual(guest.calls, [])
        XCTAssertEqual(guestModel.state.line, .needsUsername)

        // No live proof: the block appears, nothing is sent.
        let noProof = EnrolWorld()
        noProof.grant = nil
        let noProofModel = enrolModel(noProof)
        noProofModel.address = "alice@example.com"
        await noProofModel.send()
        XCTAssertEqual(noProof.calls, [])
        XCTAssertTrue(noProofModel.state.proofNeeded)
        XCTAssertEqual(noProof.proofExpired, 1)

        // The server says the proof ran out on the way.
        let expired = EnrolWorld()
        expired.startError = http(403, "fresh-auth-required")
        let expiredModel = enrolModel(expired)
        expiredModel.address = "alice@example.com"
        await expiredModel.send()
        XCTAssertTrue(expiredModel.state.proofNeeded)
        XCTAssertNil(expiredModel.state.line)
        XCTAssertEqual(expired.proofExpired, 1)

        // Too many letters: the countdown.
        let limited = EnrolWorld()
        limited.startError = AuthClientError.http(
            status: 429, endpoint: "/e", body: #"{"error":"email-rate-limited","retry_after_secs":30}"#)
        let limitedModel = enrolModel(limited)
        limitedModel.address = "alice@example.com"
        await limitedModel.send()
        XCTAssertEqual(limitedModel.state.step, .address)
        XCTAssertTrue(limitedModel.wait.waiting)

        // A wrong code clears the field and keeps the step; the letter that
        // ran out offers Send again.
        let wrong = EnrolWorld()
        wrong.finishErrors = [http(401, "code-refused"), http(410, "code-expired")]
        let wrongModel = enrolModel(wrong)
        wrongModel.address = "alice@example.com"
        await wrongModel.send()
        wrongModel.code = "111111"
        await wrongModel.confirm()
        XCTAssertEqual(wrongModel.state.line, .codeWrong)
        XCTAssertEqual(wrongModel.code, "")
        wrongModel.code = "222222"
        await wrongModel.confirm()
        XCTAssertEqual(wrongModel.state.step, .code)
        XCTAssertEqual(wrongModel.state.line, .codeExpired)

        // `Use a different address`.
        wrongModel.useOtherAddress()
        XCTAssertEqual(wrongModel.state, EmailEnrol.State())
        XCTAssertNil(wrongModel.sentTo)
    }

    /// The enrolment is two requests and nothing else: no phrase is sealed
    /// for the address since the email vault was removed (2026-11-26).
    func testAnEnrolmentSendsTheAddressThenTheCodeAndKeepsTheAddressOnTheDevices() async {
        let world = EnrolWorld()
        let model = enrolModel(world)
        model.address = " Alice@Example.com "
        await model.send()
        XCTAssertEqual(world.started.first?.address, "alice@example.com")
        XCTAssertEqual(world.started.first?.grant, "grant")
        XCTAssertEqual(model.state.step, .code)
        model.code = "123 456"
        await model.confirm()
        XCTAssertEqual(world.calls, ["start", "finish"])
        XCTAssertEqual(world.finished.first?.challengeId, "ch")
        XCTAssertEqual(world.finished.first?.code, "123456")
        XCTAssertEqual(model.state.step, .done)
        XCTAssertEqual(world.enrolled.first?.0, EmailEnrolled(enabled: true))
        XCTAssertEqual(world.enrolled.first?.1, false)
        XCTAssertEqual(world.keptAddresses, ["alice@example.com"])
    }

    /// UX 19.1c — the server refuses to remove the last second way in.
    func testTheLastSecondWayInIsNotRemoved() async {
        let world = EnrolWorld()
        world.removeError = http(409, "last-factor")
        let model = enrolModel(world, replacing: true)
        let removed = await model.remove()
        XCTAssertFalse(removed)
        XCTAssertEqual(world.removed, 0)
        XCTAssertEqual(model.state.line, .lastFactor)
        XCTAssertEqual(model.state.line?.key, "protect.last_factor")
    }

    func testRemovingTheEmail() async {
        let world = EnrolWorld()
        let model = enrolModel(world, replacing: true)
        let removed = await model.remove()
        XCTAssertTrue(removed)
        XCTAssertEqual(world.removed, 1)

        world.removeError = http(403, "fresh-auth-required")
        let refused = await model.remove()
        XCTAssertFalse(refused)
        XCTAssertEqual(world.removed, 1)
        XCTAssertEqual(world.proofExpired, 1)
    }

    // MARK: - Taking the account back with the phrase

    func testTheRecallWireCarriesTheHandleAndTheProof() throws {
        let status = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(
            RecallStatusRequest(statementB64: "st", handle: "hd"))) as? [String: Any])
        XCTAssertEqual(status["handle"] as? String, "hd")
        let plain = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(
            RecallStatusRequest(statementB64: "st"))) as? [String: Any])
        XCTAssertNil(plain["handle"], "an old device names the account in the statement")

        let answer = try JSONDecoder().decode(
            RecallStatusResponse.self,
            from: Data(#"{"anchor_seq":2,"expires_at_ms":900,"changed_at_ms":100,"attempts_remaining":4,"account_id_b64":"AAAAAAAAAAAAAAAAAAAAAA","phrase_arm":true}"#.utf8))
        XCTAssertTrue(answer.window.phraseArm)
        XCTAssertEqual(answer.accountIdB64, "AAAAAAAAAAAAAAAAAAAAAA")
        // An answer without the field (an older server) offers no phrase arm.
        let old = try JSONDecoder().decode(
            RecallStatusResponse.self,
            from: Data(#"{"anchor_seq":2,"expires_at_ms":900,"changed_at_ms":100,"attempts_remaining":4}"#.utf8))
        XCTAssertFalse(old.window.phraseArm)

        func start(_ proof: String?) throws -> [String: Any] {
            try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(RecallStartRequest(
                accountIdB64: "a", anchorSeq: 1, statementB64: "s", loginRequest: "l", proof: proof,
                passwordRequest: "p", recoveryRequest: "r"))) as? [String: Any])
        }
        XCTAssertEqual(try start("phrase")["proof"] as? String, "phrase")
        XCTAssertNil(try start(nil)["proof"], "the password arm sends no field")
    }

    func testThePhrasePromptIsOfferedOnlyWhileTheAnchorAcceptsThePhrase() {
        func window(arm: Bool, left: Int = 20, expires: Int64 = 5_000) -> RecallWindow {
            RecallWindow(
                anchorSeq: 1, expiresAtMs: expires, changedAtMs: 100, attemptsRemaining: left, phraseArm: arm)
        }
        XCTAssertTrue(PhraseRecallOffer.offered(window(arm: true), nowMs: 1_000))
        // An account that had an email vault: the phrase proves nothing.
        XCTAssertFalse(PhraseRecallOffer.offered(window(arm: false), nowMs: 1_000))
        XCTAssertFalse(PhraseRecallOffer.offered(window(arm: true, left: 0), nowMs: 1_000))
        XCTAssertFalse(PhraseRecallOffer.offered(window(arm: true), nowMs: 5_000), "after the month")

        // Only a refusal of the words asks the server.
        XCTAssertTrue(PhraseRecallRule.asksServer(
            afterRecoverFailure: AuthClientError.http(status: 401, endpoint: "/p", body: "")))
        XCTAssertFalse(PhraseRecallRule.asksServer(
            afterRecoverFailure: AuthClientError.http(status: 429, endpoint: "/p", body: "")))
        XCTAssertFalse(PhraseRecallRule.asksServer(
            afterRecoverFailure: AuthClientError.network(endpoint: "/p", underlying: URLError(.timedOut))))
        XCTAssertFalse(PhraseRecallRule.asksServer(afterRecoverFailure: AuthClientError.argument("bad words")))
    }

    func testTheTakeBackFormWithThePhraseAsTheProof() {
        let words = Array(repeating: "abandon", count: 12).joined(separator: " ")
        XCTAssertTrue(RecallForm.canSubmit(
            phrase: words, newPassword: "long enough", repeated: "long enough", status: .idle, waiting: false))
        XCTAssertFalse(RecallForm.canSubmit(
            phrase: "abandon abandon", newPassword: "long enough", repeated: "long enough",
            status: .idle, waiting: false), "all twelve words")
        XCTAssertFalse(RecallForm.canSubmit(
            phrase: words, newPassword: "long enough", repeated: "different", status: .idle, waiting: false))
        XCTAssertFalse(RecallForm.canSubmit(
            phrase: words, newPassword: "long enough", repeated: "long enough", status: .working, waiting: false))
        // The wrong-proof line names the words, not a password.
        XCTAssertEqual(
            RecallCopy.formErrorKey(.wrongPassword(triesLeft: nil), phraseProof: true), RecallCopy.wrongPhrase)
        XCTAssertEqual(
            RecallCopy.formErrorKey(.wrongPassword(triesLeft: nil)), RecallCopy.wrongPassword)
        for key in [RecallCopy.usePhrase, RecallCopy.phraseChanged, RecallCopy.phraseTakeBack] {
            XCTAssertNotEqual(Localization.text(key, texts: ["date": "D"]), key, key)
        }
    }

    // MARK: - New words only for a phrase-protected account

    func testNewWordsAreShownOnlyToAPhraseProtectedAccount() {
        // A key reset or a recall.
        XCTAssertTrue(NewWordsRule.shown(phraseProtected: true))
        XCTAssertFalse(NewWordsRule.shown(phraseProtected: false))
        // A rotation: only on a phrase-protected account (saving a phrase
        // is no rotation since 2026-11-26).
        XCTAssertTrue(KeyRotationPlan.showsNewWords(phraseProtected: true))
        XCTAssertFalse(KeyRotationPlan.showsNewWords(phraseProtected: false))
        // The sheet has no state for it: it closes as done.
        XCTAssertNil(RotateSheetRules.state(for: .rotatedNoWords))
        // The recall record keeps the flag across the staged wipe; an older
        // record reads as not saved.
        let kept = RecallRecord(
            accountIdB64: "AAAAAAAAAAAAAAAAAAAAAA==", oldSeed: Data(count: 32), username: "a", learnedAtMs: 1,
            phraseSaved: true)
        let decoded = try? JSONDecoder().decode(RecallRecord.self, from: JSONEncoder().encode(kept))
        XCTAssertEqual(decoded?.phraseSaved, true)
        let old = try? JSONDecoder().decode(
            RecallRecord.self,
            from: Data(#"{"account_id":"AAAAAAAAAAAAAAAAAAAAAA==","old_seed":"AAAA","username":"a","learned_at_ms":1}"#.utf8))
        XCTAssertNotNil(old)
        XCTAssertNil(old?.phraseSaved)
    }

    func testThePublishOwedScreenNamesNoWordsForAnAccountWithoutASavedPhrase() {
        XCTAssertEqual(
            KeysChangedCopy.publishOwedKey(showsWords: false, needsSignIn: false), "account.keys_changed.finishing")
        XCTAssertEqual(
            KeysChangedCopy.publishOwedKey(showsWords: true, needsSignIn: false),
            "recovery_phrase.fresh.publish_owed")
        XCTAssertEqual(
            KeysChangedCopy.publishOwedKey(showsWords: false, needsSignIn: true),
            "recovery_phrase.fresh.sign_in_to_finish")
        XCTAssertNotEqual(Localization.text("account.keys_changed.finishing"), "account.keys_changed.finishing")
    }

    // MARK: - Restore your keys

    func testTheStatusCarriesTheAuthenticatorFlag() throws {
        let with = try JSONDecoder().decode(
            RecoveryEmailStatus.self,
            from: Data(#"{"available":true,"enabled":true,"totp":true}"#.utf8))
        XCTAssertEqual(with.totp, true)
        let without = try JSONDecoder().decode(
            RecoveryEmailStatus.self,
            from: Data(#"{"available":true,"enabled":true}"#.utf8))
        XCTAssertNil(without.totp)
        // Unknown factors decide nothing (the landing rule, the last-factor
        // dialog): a guess is never acted on.
        XCTAssertNil(AccountFactors.of(status: without, hasPasskey: true))
        XCTAssertNil(AccountFactors.of(status: with, hasPasskey: nil))
        XCTAssertEqual(
            AccountFactors.of(status: with, hasPasskey: false),
            AccountFactors(email: true, authenticator: true, passkey: false))
    }

    // MARK: - Strings

    func testEveryKeyTheseScreensUseIsInTheCatalog() {
        let keys = [
            "protect.title", "protect.title_required", "protect.body", "protect.email.title",
            "protect.email.badge", "protect.email.body", "protect.totp.title", "protect.totp.body",
            "protect.passkey.title", "protect.passkey.body", "protect.passkey.cancelled", "protect.optional",
            "protect.phrase.title_optional", "protect.phrase.body", "protect.continue",
            "protect.continue_needs", "protect.totp_only.warning", "protect.totp_only.warning_phrase",
            "protect.totp_only.remove_notice", "protect.totp_only.remove_notice_phrase",
            "protect.last_factor", "protect.username_email", "protect.status.phrase",
            "protect.status.passkey_seal", "protect.status.password_only", "protect.password_only.title",
            "protect.password_only.body", "protect.password_only.save_phrase",
            "protect.password_only.add_passkey", "protect.password_only.passkey_no_seal",
            "settings.passkeys.no_seal", "protect.phrase.save.body",
            "protect.phrase.save.show", "protect.phrase.save.failed",
            "settings.recovery.totp.scan", "settings.recovery.totp.open_app",
            "settings.recovery.totp.on", "settings.recovery.totp.done", "settings.recovery.totp.error.wrong",
            "settings.recovery.totp.error.generic", "settings.recovery.totp.already_enabled",
            "settings.recovery.totp.set_up", "proof.add_totp.help",
            "recovery_email.title", "recovery_email.body", "recovery_email.field", "recovery_email.invalid",
            "recovery_email.not_kept", "recovery_email.mailbox_warning", "recovery_email.send",
            "recovery_email.code.title", "recovery_email.code.wrong", "recovery_email.code.expired",
            "recovery_email.code.used_up", "recovery_email.code.resend", "recovery_email.code.other_address",
            "recovery_email.send_failed", "recovery_email.unavailable", "recovery_email.needs_username",
            "recovery_email.status.on", "recovery_email.status.off", "recovery_email.replace",
            "recovery_email.remove", "recovery_email.remove.confirm.title",
            "recovery_email.remove.confirm.body",
            "recovery.email.row.title", "recovery.email.row.body", "recovery.email.start.body",
            "recovery.email.check.title", "recovery.email.check.body", "recovery.email.totp.title",
            "recovery.email.totp.body", "recovery.email.totp.lost",
            "recovery.email.save", "notice.was_me", "notice.was_not_me",
            "proof.recovery_email.help", "proof.save_phrase.help", "notice.secure.title",
            "notice.secure.password_saved",
            "recall.phrase.wrong", "recovery_email.totp_warning", "common.saving",
        ]
        for key in keys {
            XCTAssertNotEqual(Localization.text(key), key, "\(key) is missing from the catalog")
        }
        // Removed with the email vault, the banner and "Not now": no screen
        // may name them any more.
        for key in [
            "protect.skip", "protect.banner.none", "protect.banner.action", "protect.status.none",
            "protect.status.email", "recovery_email.add_again.title", "recovery.email.restoring",
            "recovery.email.failed", "notice.secure.new_keys", "protect.phrase.save.effect",
            "protect.phrase.save.progress", "protect.phrase.save.finishing",
            "restore_identity.reset.effect_messages",
        ] {
            XCTAssertEqual(Localization.text(key), key, "\(key) was removed from the catalog")
        }
        XCTAssertTrue(Localization.text(
            "recovery_email.code.sent_to", texts: ["address": "a@b.example"]).contains("a@b.example"))
        XCTAssertTrue(Localization.text("recovery_email.code.resend_in", texts: ["time": "0:45"]).contains("0:45"))
        XCTAssertTrue(Localization.text("settings.recovery.totp.key", texts: ["key": "ABCD"]).contains("ABCD"))
        for line in [
            RecoveryEmailLine.addressInvalid, .codeWrong, .codeExpired, .codeUsedUp,
            .limited(retryAfterSecs: nil), .sendFailed, .unavailable, .lastFactor, .needsUsername,
            .offline, .failed,
        ] {
            let key = try? XCTUnwrap(line.key)
            XCTAssertNotEqual(key.map { Localization.text($0) }, key, "\(line)")
        }
    }
}
