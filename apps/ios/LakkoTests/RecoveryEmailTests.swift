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
        available: Bool = true, enabled: Bool, vault: Bool, phraseSaved: Bool
    ) -> RecoveryEmailStatus {
        RecoveryEmailStatus(available: available, enabled: enabled, vault: vault, phraseSaved: phraseSaved)
    }

    private func defaults(_ name: String = #function) -> UserDefaults {
        let suite = "lakko.tests.recovery-email.\(name)"
        UserDefaults().removePersistentDomain(forName: suite)
        return UserDefaults(suiteName: suite)!
    }

    // MARK: - The account's mode

    func testTheAccountModeIsOneRuleOverThreeFacts() {
        // A saved phrase wins, whatever the email is.
        XCTAssertEqual(accountProtection(enabled: false, vault: false, phraseSaved: true), .phrase)
        XCTAssertEqual(accountProtection(enabled: true, vault: false, phraseSaved: true), .phrase)
        XCTAssertEqual(accountProtection(enabled: true, vault: true, phraseSaved: true), .phrase)
        // No phrase: the email protects only while the server holds the vault.
        XCTAssertEqual(accountProtection(enabled: true, vault: true, phraseSaved: false), .email)
        XCTAssertEqual(accountProtection(enabled: true, vault: false, phraseSaved: false), .none)
        XCTAssertEqual(accountProtection(enabled: false, vault: false, phraseSaved: false), .none)
        // A vault without an email does not exist; it protects nothing.
        XCTAssertEqual(accountProtection(enabled: false, vault: true, phraseSaved: false), .none)
    }

    func testAnEmailWithoutAVaultAsksToBeAddedAgain() {
        // After a key reset or a recall the server dropped the vault.
        let dropped = status(enabled: true, vault: false, phraseSaved: false)
        XCTAssertTrue(dropped.emailNeedsAddingAgain)
        XCTAssertEqual(dropped.protection, .none)
        XCTAssertTrue(ProtectBanner.shows(protection: dropped.protection, dismissedAtMs: nil, nowMs: 1))
        XCTAssertEqual(ProtectionRows.decide(dropped).email, .addAgain)
        XCTAssertEqual(ProtectBanner.copy(dropped).titleKey, "recovery_email.add_again.title")
        XCTAssertEqual(ProtectBanner.copy(dropped).bodyKey, "recovery_email.add_again.body")
        // Otherwise the ordinary banner.
        let never = status(enabled: false, vault: false, phraseSaved: false)
        XCTAssertEqual(ProtectBanner.copy(never).titleKey, "protect.banner.none")
        XCTAssertNil(ProtectBanner.copy(never).bodyKey)
        XCTAssertEqual(ProtectionRows.decide(never).email, .off)
        XCTAssertEqual(
            ProtectionRows.decide(status(enabled: true, vault: false, phraseSaved: true)).email, .on,
            "a phrase-protected account's email never has a vault")
        // Not for a phrase-protected account: its email never has a vault.
        XCTAssertFalse(status(enabled: true, vault: false, phraseSaved: true).emailNeedsAddingAgain)
        XCTAssertFalse(status(enabled: true, vault: true, phraseSaved: false).emailNeedsAddingAgain)
    }

    func testTheSettingsRowsFollowTheStatus() {
        let none = ProtectionRows.decide(status(enabled: false, vault: false, phraseSaved: false))
        XCTAssertEqual(none.statusKey, "protect.status.none")
        XCTAssertEqual(none.email, .off)
        XCTAssertEqual(none.phrase, .save)
        XCTAssertFalse(none.showsThreeWords)

        let email = ProtectionRows.decide(status(enabled: true, vault: true, phraseSaved: false))
        XCTAssertEqual(email.statusKey, "protect.status.email")
        XCTAssertEqual(email.email, .on)
        XCTAssertEqual(email.phrase, .save, "no Show phrase without a saved phrase")
        XCTAssertFalse(email.showsThreeWords)

        let phrase = ProtectionRows.decide(status(enabled: false, vault: false, phraseSaved: true))
        XCTAssertEqual(phrase.statusKey, "protect.status.phrase")
        XCTAssertEqual(phrase.phrase, .show)
        XCTAssertTrue(phrase.showsThreeWords)

        // Mail is not set up on this server: no row, unless an email is on.
        XCTAssertEqual(
            ProtectionRows.decide(status(available: false, enabled: false, vault: false, phraseSaved: false)).email,
            .absent)
        XCTAssertEqual(
            ProtectionRows.decide(status(available: false, enabled: true, vault: true, phraseSaved: false)).email,
            .on)
        // The sign-up step offers the email only with mail set up and a username.
        XCTAssertTrue(ProtectionRows.signupOffersEmail(available: true, hasUsername: true))
        XCTAssertFalse(ProtectionRows.signupOffersEmail(available: false, hasUsername: true))
        XCTAssertFalse(ProtectionRows.signupOffersEmail(available: true, hasUsername: false))
    }

    func testTheBannerIsOnlyForAnUnprotectedAccountAndReturnsAfterThirtyDays() {
        let day: Int64 = 24 * 60 * 60 * 1000
        XCTAssertTrue(ProtectBanner.shows(protection: .none, dismissedAtMs: nil, nowMs: 0))
        XCTAssertFalse(ProtectBanner.shows(protection: .email, dismissedAtMs: nil, nowMs: 0))
        XCTAssertFalse(ProtectBanner.shows(protection: .phrase, dismissedAtMs: nil, nowMs: 0))
        XCTAssertFalse(ProtectBanner.shows(protection: .none, dismissedAtMs: 1000, nowMs: 1000 + 29 * day))
        XCTAssertTrue(ProtectBanner.shows(protection: .none, dismissedAtMs: 1000, nowMs: 1000 + 30 * day))
        // A clock that went backwards does not bring it back.
        XCTAssertFalse(ProtectBanner.shows(protection: .none, dismissedAtMs: 1000, nowMs: 500))
    }

    func testAVaultIsSealedOnlyWhileNoPhraseIsSaved() {
        XCTAssertTrue(EnrolSealing.sealsVault(phraseSaved: false))
        XCTAssertFalse(EnrolSealing.sealsVault(phraseSaved: true))
    }

    func testTheStoreKeepsFlagsOnlyAndTheWipeClearsThem() {
        let defaults = defaults()
        let store = AccountProtectionStore(defaults: defaults)
        XCTAssertFalse(store.phraseSaved(accountIdB64: "acct"))
        XCTAssertNil(store.status(accountIdB64: "acct"))
        XCTAssertNil(store.noticeCursor(accountIdB64: "acct"), "a device that never kept a cursor")

        store.setStatus(status(enabled: true, vault: true, phraseSaved: true), accountIdB64: "acct")
        store.setBannerDismissed(atMs: 7, accountIdB64: "acct")
        store.setNoticeCursor(12, accountIdB64: "acct")
        store.markOwnAction(.emailAdded, atMs: 99, accountIdB64: "acct")
        XCTAssertTrue(store.phraseSaved(accountIdB64: "acct"), "learned from the status")
        XCTAssertEqual(store.status(accountIdB64: "acct")?.enabled, true)
        XCTAssertEqual(store.bannerDismissedAtMs(accountIdB64: "acct"), 7)
        XCTAssertEqual(store.noticeCursor(accountIdB64: "acct"), 12)
        XCTAssertEqual(store.ownActions(accountIdB64: "acct"), ["recovery-email-added": 99])
        XCTAssertFalse(store.phraseSaved(accountIdB64: "other"), "per account")

        // Nothing stored could hold an address: every value is a flag, a
        // number, or the four booleans of the status.
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

        // What comes back is shown, not adopted silently.
        let outcome = NoticeSelection.select(
            fetched: [notice(41, .passwordChangedByEmail), notice(40, .emailAdded)], cursor: nil, ownActions: [:])
        XCTAssertEqual(outcome.shown.map(\.id), [40, 41])
        XCTAssertEqual(outcome.cursor, 41, "then a cursor is kept")
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
            notice(9, .accountOpenedByEmail, at: 49_000),
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
        XCTAssertNotNil(AccountNoticeCopy.of(.accountOpenedByEmail).bodyKey)
        XCTAssertNil(AccountNoticeCopy.of(.emailAdded).bodyKey)

        // In order: a new password, remove the email, save a phrase.
        XCTAssertEqual(
            SecureAccountActions.offered(status(enabled: true, vault: true, phraseSaved: false)),
            [.newPassword, .removeEmail, .savePhrase])
        XCTAssertEqual(
            SecureAccountActions.offered(status(enabled: true, vault: false, phraseSaved: true)),
            [.newPassword, .removeEmail], "no way back from a saved phrase, nothing to save")
        XCTAssertEqual(
            SecureAccountActions.offered(status(enabled: false, vault: false, phraseSaved: false)),
            [.newPassword, .savePhrase])
    }

    // MARK: - The centre

    private final class Server {
        var status: Result<RecoveryEmailStatus, Error>
        var pages: [NoticeQuery: AccountNoticePage] = [:]
        var now: Int64 = 1_000_000
        var statusReads = 0
        var noticeReads: [NoticeQuery] = []
        init(_ status: RecoveryEmailStatus) { self.status = .success(status) }
    }

    private func center(
        _ server: Server, store: AccountProtectionStore, session: Bool = true
    ) -> AccountProtectionCenter {
        AccountProtectionCenter(
            ports: .init(
                session: { session ? .init(accountIdB64: "acct", sessionToken: "tok") : nil },
                fetchStatus: { _ in
                    server.statusReads += 1
                    return try server.status.get()
                },
                fetchNotices: { _, query in
                    server.noticeReads.append(query)
                    return server.pages[query] ?? AccountNoticePage(notices: [], nextAfter: 0, more: false)
                },
                nowMs: { server.now }),
            store: store)
    }

    func testTheStatusIsReadAtTheLandingAndKeptForTheNextOne() async {
        let store = AccountProtectionStore(defaults: defaults())
        let server = Server(status(enabled: true, vault: true, phraseSaved: false))
        let center = center(server, store: store)
        XCTAssertNil(center.rows)

        await center.land(accountIdB64: "acct")
        XCTAssertEqual(center.status?.protection, .email)
        XCTAssertEqual(store.status(accountIdB64: "acct")?.vault, true)
        XCTAssertFalse(center.bannerVisible)

        // The next landing shows the stored status at once; a server that
        // does not answer keeps it.
        server.status = .failure(URLError(.notConnectedToInternet))
        let next = self.center(server, store: store)
        await next.land(accountIdB64: "acct")
        XCTAssertEqual(next.status?.protection, .email)
    }

    func testAResetDropsTheVaultAndTheBannerAsksForTheEmailAgain() async {
        let store = AccountProtectionStore(defaults: defaults())
        let server = Server(status(enabled: true, vault: false, phraseSaved: false))
        let center = center(server, store: store)
        await center.land(accountIdB64: "acct")

        XCTAssertTrue(center.bannerVisible)
        XCTAssertEqual(center.rows?.statusKey, "protect.status.none")
        XCTAssertEqual(center.rows?.email, .addAgain)

        // Dismissed: gone for thirty days, then back if nothing changed.
        center.dismissBanner()
        XCTAssertFalse(center.bannerVisible)
        server.now += ProtectBanner.returnsAfterMs
        XCTAssertTrue(center.bannerVisible)

        // Enrolling again seals the vault: protected by the email.
        center.enrolled(EmailEnrolled(enabled: true, vault: true), replaced: true)
        XCTAssertEqual(center.status?.protection, .email)
        XCTAssertFalse(center.bannerVisible)
        XCTAssertNotNil(store.ownActions(accountIdB64: "acct")[AccountNoticeKind.emailReplaced.rawValue])
    }

    func testNoticesAreShownOnceAndAnsweredOnesAreNotFetchedAgain() async {
        let store = AccountProtectionStore(defaults: defaults())
        let server = Server(status(enabled: true, vault: true, phraseSaved: false))
        // No cursor on this device: the last 30 days, then by id.
        let first = NoticeQuery.sinceMs(server.now - NoticeQuery.lookbackMs)
        server.pages[first] = AccountNoticePage(
            notices: [notice(1, .emailAdded), notice(2, .passwordChangedByEmail)], nextAfter: 2, more: true)
        server.pages[.after(2)] = AccountNoticePage(
            notices: [notice(3, .accountOpenedByEmail)], nextAfter: 3, more: false)
        let center = center(server, store: store)
        await center.land(accountIdB64: "acct")

        XCTAssertEqual(server.noticeReads, [first, .after(2)])
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

    func testARecoveryOnAFreshDeviceIsNotToldAboutItself() async {
        let store = AccountProtectionStore(defaults: defaults())
        let server = Server(status(enabled: true, vault: true, phraseSaved: false))
        server.pages[.sinceMs(server.now - NoticeQuery.lookbackMs)] = AccountNoticePage(
            notices: [notice(1, .accountOpenedByEmail, at: server.now + 2_000)], nextAfter: 1, more: false)
        let center = center(server, store: store)

        // The account is not known yet when the vault is opened.
        center.noteOwnAction(.accountOpenedByEmail)
        XCTAssertEqual(store.ownActions(accountIdB64: "acct"), [:])
        await center.land(accountIdB64: "acct")

        XCTAssertEqual(center.notices, [], "this device did it")
        XCTAssertEqual(store.noticeCursor(accountIdB64: "acct"), 1)
        XCTAssertEqual(store.ownActions(accountIdB64: "acct"), [:], "the marker is spent")
    }

    func testAKeptDeviceLearnsThatAPhraseWasSaved() async {
        let store = AccountProtectionStore(defaults: defaults())
        let server = Server(status(enabled: true, vault: true, phraseSaved: false))
        let center = center(server, store: store)
        await center.land(accountIdB64: "acct")
        XCTAssertFalse(store.phraseSaved(accountIdB64: "acct"))
        XCTAssertFalse(center.phraseSaved)

        // At the next landing, from the status...
        server.status = .success(status(enabled: true, vault: false, phraseSaved: true))
        await center.land(accountIdB64: "acct")
        XCTAssertTrue(store.phraseSaved(accountIdB64: "acct"))

        // ...and when the "recovery phrase saved" notice arrives, even if
        // the status could not be read at that moment.
        let other = AccountProtectionStore(defaults: defaults("kept-notice"))
        let quiet = Server(status(enabled: true, vault: true, phraseSaved: false))
        let kept = self.center(quiet, store: other)
        await kept.land(accountIdB64: "acct")
        quiet.status = .failure(URLError(.timedOut))
        quiet.pages[.after(0)] = AccountNoticePage(
            notices: [notice(5, .phraseSaved)], nextAfter: 5, more: false)
        await kept.loadNotices()
        XCTAssertTrue(other.phraseSaved(accountIdB64: "acct"))
        // Opening Settings reads the status again.
        let reads = quiet.statusReads
        await kept.refresh()
        XCTAssertEqual(quiet.statusReads, reads + 1)
    }

    func testWithoutASessionNothingIsAsked() async {
        let server = Server(status(enabled: false, vault: false, phraseSaved: false))
        let center = center(server, store: AccountProtectionStore(defaults: defaults()), session: false)
        await center.refresh()
        await center.loadNotices()
        XCTAssertEqual(server.statusReads, 0)
        XCTAssertEqual(server.noticeReads, [])
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
        // There is no way past it: `open` sends the person back to it.
        let restoring = EmailRecover.saving(EmailRecover.verified(code, totpRequired: false))
        XCTAssertEqual(EmailRecover.ticketRouteRefused(restoring, .totpRequired).step, .totp)
        // A ticket that ran out: a new letter.
        let expired = EmailRecover.ticketRouteRefused(totp, .ticketRefused)
        XCTAssertEqual(expired.step, .start)
        XCTAssertEqual(expired.line, .codeExpired)
    }

    func testOpenIsAskedOnlyAfterTheNewPasswordStep() {
        // start -> code -> authenticator -> NEW PASSWORD -> open.
        var state = EmailRecover.State()
        XCTAssertFalse(EmailRecover.mayOpen(state))
        state = EmailRecover.started(state)
        XCTAssertFalse(EmailRecover.mayOpen(state), "not on the code step")
        state = EmailRecover.verified(state, totpRequired: true)
        XCTAssertFalse(EmailRecover.mayOpen(state), "not on the authenticator step")
        state = EmailRecover.totpAccepted(state)
        XCTAssertEqual(state.step, .newPassword)
        XCTAssertFalse(EmailRecover.mayOpen(state), "not before the password was submitted")
        state = EmailRecover.saving(state)
        XCTAssertEqual(state.step, .restoring)
        XCTAssertTrue(EmailRecover.mayOpen(state))

        // The vault did not open: the route is over, with one sentence.
        let notOpened = EmailRecover.vaultNotOpened(state)
        XCTAssertTrue(notOpened.leaveToList)
        XCTAssertEqual(notOpened.line?.key, "recovery.email.failed")
        XCTAssertEqual(notOpened.step, .start)

        // A failure of the last step brings the form back.
        let failed = EmailRecover.saveFailed(state, line: .offline)
        XCTAssertEqual(failed.step, .newPassword)
        XCTAssertEqual(failed.line, .offline)
        XCTAssertEqual(EmailRecover.ticketRouteRefused(state, .offline).step, .newPassword)
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
        XCTAssertEqual(of(409, "phrase-saved"), .phraseSaved)
        XCTAssertEqual(of(409, "wrong-mode"), .wrongMode)
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
        var openAnswer: Result<EmailOpen, Error> = .success(.password)
        var unsealAnswer: Result<String, Error> = .success("the twelve words")
        var saveError: Error?
        var calls: [String] = []
        var started: [(handle: Data, address: String)] = []
        var unsealed: [(handle: Data, address: String, salt: Data, sealed: Data)] = []
        var restored: [(phrase: String, password: Data, username: String)] = []
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
                open: { _ in
                    world.calls.append("open")
                    return try world.openAnswer.get()
                },
                unseal: { handle, address, salt, sealed in
                    world.calls.append("unseal")
                    world.unsealed.append((handle, address, salt, sealed))
                    return try world.unsealAnswer.get()
                },
                restoreWithPhrase: { phrase, password, username in
                    world.calls.append("restore")
                    if let error = world.saveError { throw error }
                    world.restored.append((phrase, password, username))
                },
                changePassword: { username, ticket, password in
                    world.calls.append("password")
                    if let error = world.saveError { throw error }
                    world.changed.append((username, ticket, password))
                },
                noteOwnAction: { world.own.append($0) }),
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

    func testTheVaultPathAsksOpenOnlyAfterThePasswordThenRunsTheExistingPhraseRestore() async {
        let world = RecoverWorld()
        world.openAnswer = .success(.vault(salt: Data(repeating: 2, count: 16), sealed: Data(repeating: 3, count: 44)))
        let model = recoverModel(world)
        await typeAndStart(model)
        model.code = "123 456"
        await model.verify()

        // The new-password step, and nothing has asked what the code brings.
        XCTAssertEqual(model.state.step, .newPassword)
        XCTAssertEqual(world.calls, ["start", "verify"])
        XCTAssertEqual(world.own, [])

        model.password = "new password"
        model.confirmPassword = "new password"
        await model.save()
        XCTAssertEqual(world.calls, ["start", "verify", "open", "unseal", "restore"])
        // Opened with the typed username's handle and the typed address.
        XCTAssertEqual(world.unsealed.first?.handle, try? loginHandle(username: "alice"))
        XCTAssertEqual(world.unsealed.first?.address, "alice@example.com")
        XCTAssertEqual(world.own, [.accountOpenedByEmail])
        XCTAssertEqual(world.restored.first?.phrase, "the twelve words")
        XCTAssertEqual(world.restored.first?.username, "alice")
        XCTAssertEqual(world.restored.first?.password, Data("new password".utf8))
        XCTAssertTrue(world.changed.isEmpty, "not the password-only route")
        // Nothing typed or received is kept.
        XCTAssertEqual(model.address, "")
        XCTAssertEqual(model.password, "")
        XCTAssertEqual(model.state, EmailRecover.State())
    }

    func testARestoreThatFailsAfterTheVaultOpenedIsRepeatedWithoutASecondOpen() async {
        let world = RecoverWorld()
        world.openAnswer = .success(.vault(salt: Data(count: 16), sealed: Data(count: 44)))
        world.saveError = AuthClientError.network(endpoint: "/password/reset/start", underlying: URLError(.timedOut))
        let model = recoverModel(world)
        await typeAndStart(model)
        model.code = "123456"
        await model.verify()
        model.password = "new password"
        model.confirmPassword = "new password"
        await model.save()
        XCTAssertEqual(model.state.step, .newPassword)
        XCTAssertEqual(model.state.line, .offline)

        world.saveError = nil
        await model.save()
        XCTAssertEqual(world.calls.filter { $0 == "open" }.count, 1, "the ticket is spent: no second open")
        XCTAssertEqual(world.restored.count, 1)
    }

    func testAVaultThatDoesNotOpenLeavesThePersonOnGetBackIn() async {
        let world = RecoverWorld()
        world.openAnswer = .success(.vault(salt: Data(count: 16), sealed: Data(count: 44)))
        world.unsealAnswer = .failure(FfiError.EmailVaultNotOpened(message: "not opened"))
        let model = recoverModel(world)
        await typeAndStart(model)
        model.code = "123456"
        await model.verify()
        model.password = "new password"
        model.confirmPassword = "new password"
        await model.save()

        XCTAssertTrue(model.state.leaveToList)
        XCTAssertEqual(model.state.line?.key, "recovery.email.failed")
        // No restore, no password route; the password typed is not kept.
        XCTAssertFalse(world.calls.contains("restore"))
        XCTAssertFalse(world.calls.contains("password"))
        XCTAssertEqual(model.password, "")
        await model.save()
        XCTAssertFalse(world.calls.contains("restore"))
    }

    func testThePasswordOnlyPathChangesThePasswordUnderTheTicket() async {
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
        XCTAssertFalse(world.calls.contains("open"), "nothing before the password step calls open")

        // The password rule, before anything is sent.
        model.password = "short"
        model.confirmPassword = "short"
        await model.save()
        XCTAssertNotNil(model.passwordError)
        XCTAssertFalse(world.calls.contains("open"))

        model.password = "long enough"
        model.confirmPassword = "long enough"
        await model.save()
        XCTAssertEqual(Array(world.calls.suffix(2)), ["open", "password"])
        XCTAssertFalse(world.calls.contains("unseal"))
        XCTAssertEqual(world.changed.first?.ticket, "tk-7")
        XCTAssertEqual(world.changed.first?.username, "alice")
        XCTAssertEqual(world.own, [.passwordChangedByEmail])
        XCTAssertTrue(world.restored.isEmpty)
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
        var phraseSaved = false
        var startError: Error?
        var finishErrors: [Error] = []
        var removeError: Error?
        var calls: [String] = []
        var started: [(grant: String, handle: Data, address: String)] = []
        var sealed: [(handle: Data, address: String, salt: Data)] = []
        var finished: [(code: String, sealed: Data?)] = []
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
                phraseSaved: { world.phraseSaved },
                normalize: { try normalizeRecoveryEmail(address: $0) },
                start: { _, grant, handle, address in
                    world.calls.append("start")
                    if let error = world.startError { throw error }
                    world.started.append((grant, handle, address))
                    return EmailEnrolChallenge(
                        challenge: EmailChallenge(challengeId: "ch", resendAfterSecs: 45, expiresInSecs: 600),
                        vaultSalt: Data(repeating: 9, count: 16))
                },
                seal: { handle, address, salt in
                    world.calls.append("seal")
                    world.sealed.append((handle, address, salt))
                    return Data(repeating: 0x5E, count: 44)
                },
                finish: { _, _, code, sealed in
                    world.calls.append("finish")
                    if !world.finishErrors.isEmpty { throw world.finishErrors.removeFirst() }
                    world.finished.append((code, sealed))
                    return EmailEnrolled(enabled: true, vault: sealed != nil)
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

    func testAnAccountWithoutASavedPhraseSealsTheVaultAfterStartAndSendsItWithTheCode() async {
        let world = EnrolWorld()
        let model = enrolModel(world)
        model.address = "Alice@Example.com"
        await model.send()

        XCTAssertEqual(model.state.step, .code)
        XCTAssertEqual(model.sentTo, "alice@example.com", "shown back on this surface only")
        XCTAssertEqual(world.started.first?.address, "alice@example.com")
        XCTAssertEqual(world.started.first?.grant, "grant")

        model.code = "123456"
        await model.confirm()
        // Sealed with THIS start's salt, bound to the address being confirmed.
        XCTAssertEqual(world.sealed.first?.salt, Data(repeating: 9, count: 16))
        XCTAssertEqual(world.sealed.first?.address, "alice@example.com")
        XCTAssertEqual(world.sealed.first?.handle, world.handle)
        XCTAssertEqual(world.finished.first?.sealed, Data(repeating: 0x5E, count: 44))
        XCTAssertEqual(model.state.step, .done)
        XCTAssertEqual(world.enrolled.first?.0, EmailEnrolled(enabled: true, vault: true))
        XCTAssertEqual(world.enrolled.first?.1, false)
        // The normalised address is handed on for the devices to keep.
        XCTAssertEqual(world.keptAddresses, ["alice@example.com"])

        // The surface closed: the address is not kept.
        model.closed()
        XCTAssertEqual(model.address, "")
        XCTAssertNil(model.sentTo)
    }

    func testAPhraseProtectedAccountSendsNoVault() async {
        let world = EnrolWorld()
        world.phraseSaved = true
        let model = enrolModel(world, replacing: true)
        model.address = "alice@example.com"
        await model.send()
        model.code = "123456"
        await model.confirm()

        XCTAssertFalse(world.calls.contains("seal"))
        XCTAssertEqual(world.finished.count, 1)
        XCTAssertNil(world.finished[0].sealed)
        XCTAssertEqual(world.enrolled.first?.1, true, "a replace")
    }

    func testAPhraseSavedElsewhereMeanwhileIsFinishedWithoutTheVault() async {
        let world = EnrolWorld()
        world.finishErrors = [http(409, "phrase-saved")]
        let model = enrolModel(world)
        model.address = "alice@example.com"
        await model.send()
        model.code = "123456"
        await model.confirm()

        XCTAssertEqual(world.calls.filter { $0 == "finish" }.count, 2)
        XCTAssertEqual(world.finished.count, 1)
        XCTAssertNil(world.finished[0].sealed)
        XCTAssertEqual(model.state.step, .done)
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

    // MARK: - The vault, off the main thread

    func testTheVaultSealsAndOpensOffTheMainThreadWithTheUsernameAndTheAddress() async throws {
        let mnemonic = RecoveryMnemonic.generate()
        let handle = try loginHandle(username: "alice")
        let salt = Data(repeating: 0x22, count: 16)
        let address = try normalizeRecoveryEmail(address: " Alice@Example.COM ")
        XCTAssertEqual(address, "alice@example.com")

        let sealed = try await EmailVaultWork.seal(
            phrase: mnemonic.phrase(), handle: handle, address: address, vaultSalt: salt)
        XCTAssertEqual(sealed.count, 44)
        let phrase = try await EmailVaultWork.open(handle: handle, address: address, vaultSalt: salt, sealed: sealed)
        XCTAssertEqual(phrase, mnemonic.phrase())

        // Another username or another address does not open it.
        do {
            _ = try await EmailVaultWork.open(
                handle: try loginHandle(username: "bob"), address: address, vaultSalt: salt, sealed: sealed)
            XCTFail("another username opened the vault")
        } catch {}
        do {
            _ = try await EmailVaultWork.open(
                handle: handle, address: "mallory@example.com", vaultSalt: salt, sealed: sealed)
            XCTFail("another address opened the vault")
        } catch {}
        XCTAssertThrowsError(try normalizeRecoveryEmail(address: "no-at-sign"))
    }

    // MARK: - Save a recovery phrase

    func testTheNewWordsAreShownOnlyAfterTheChangeWentThrough() {
        XCTAssertEqual(SavePhraseState.after(.rotated(phrase: "w")), .words("w"))
        XCTAssertEqual(SavePhraseState.after(.rotatedPublishOwed), .publishOwed, "withheld")
        XCTAssertEqual(SavePhraseState.after(.freshAuthRequired), .idle, "the proof block again")
        XCTAssertEqual(SavePhraseState.after(.removedNotRotated(.failed)), .failed)
        XCTAssertEqual(SavePhraseState.after(.removedNotRotated(nil)), .failed)
        XCTAssertEqual(SavePhraseState.after(.notRemoved(.notRevoked(.failed))), .failed)
    }

    func testTheRotationFinishCarriesPhraseSavedOnlyWhenAsked() throws {
        func json(_ phraseSaved: Bool?) throws -> [String: Any] {
            try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(KeyRotateFinishRequest(
                freshGrant: "g", recoveryUpload: "u", newSignaturePubkeyB64: "k",
                supersessionProofB64: "p", revokeDeviceIdsB64: [], phraseSaved: phraseSaved))) as? [String: Any])
        }
        XCTAssertEqual(try json(true)["phrase_saved"] as? Bool, true)
        XCTAssertNil(try json(nil)["phrase_saved"], "a device rotation sends no field")
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
        // A rotation: the save-phrase flow always, a device rotation only
        // on a phrase-protected account.
        XCTAssertTrue(KeyRotationPlan.showsNewWords(savesPhrase: true, phraseProtected: false))
        XCTAssertTrue(KeyRotationPlan.showsNewWords(savesPhrase: false, phraseProtected: true))
        XCTAssertFalse(KeyRotationPlan.showsNewWords(savesPhrase: false, phraseProtected: false))
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

    // MARK: - The protect step: rows, Done marks, Continue vs Not now

    func testTheProtectStepRowsMarksAndFooter() {
        // Nothing done: email, passkey, phrase; `Not now`.
        let fresh = ProtectStep.decide(offersEmail: true, passkeysSupported: true, done: [], leavable: true)
        XCTAssertEqual(fresh.items.map(\.row), [.email, .passkey, .phrase], "the passkey between the two")
        XCTAssertEqual(fresh.items.map(\.done), [false, false, false])
        XCTAssertEqual(fresh.footer, .notNow)

        // The passkey row only where this device can use passkeys; the
        // email row only with mail set up and a username.
        XCTAssertEqual(
            ProtectStep.decide(offersEmail: true, passkeysSupported: false, done: [], leavable: true)
                .items.map(\.row), [.email, .phrase])
        XCTAssertEqual(
            ProtectStep.decide(offersEmail: false, passkeysSupported: true, done: [], leavable: true)
                .items.map(\.row), [.passkey, .phrase])

        // Any row done: it is marked, and `Continue` replaces `Not now`.
        for row in [ProtectStep.Row.email, .passkey, .phrase] {
            let step = ProtectStep.decide(offersEmail: true, passkeysSupported: true, done: [row], leavable: true)
            XCTAssertEqual(step.items.filter(\.done).map(\.row), [row])
            XCTAssertEqual(step.footer, .continueOn)
        }
        // An email that is on stays listed (marked) even where mail is down.
        XCTAssertEqual(
            ProtectStep.decide(offersEmail: false, passkeysSupported: true, done: [.email], leavable: true)
                .items.map(\.row), [.email, .passkey, .phrase])
        // The Settings entry has no footer of its own.
        XCTAssertEqual(
            ProtectStep.decide(offersEmail: true, passkeysSupported: true, done: [.email], leavable: false).footer,
            .none)

        // What the status marks done.
        XCTAssertEqual(ProtectStep.done(status: nil, hasPasskey: false), [])
        XCTAssertEqual(
            ProtectStep.done(status: status(enabled: true, vault: true, phraseSaved: false), hasPasskey: true),
            [.email, .passkey])
        XCTAssertEqual(
            ProtectStep.done(status: status(enabled: true, vault: false, phraseSaved: false), hasPasskey: false),
            [], "an email waiting to be added again is not done")
        XCTAssertEqual(
            ProtectStep.done(status: status(enabled: false, vault: false, phraseSaved: true), hasPasskey: false),
            [.phrase])
        for key in ["protect.passkey.title", "protect.passkey.body", "protect.done", "protect.continue"] {
            XCTAssertNotEqual(Localization.text(key), key, key)
        }
    }

    func testNoBannerForAnAccountWithAPasskey() async {
        XCTAssertTrue(ProtectBanner.shows(protection: .none, hasPasskey: false, dismissedAtMs: nil, nowMs: 0))
        XCTAssertFalse(ProtectBanner.shows(protection: .none, hasPasskey: true, dismissedAtMs: nil, nowMs: 0))
        // Nor its return after thirty days.
        XCTAssertFalse(ProtectBanner.shows(
            protection: .none, hasPasskey: true, dismissedAtMs: 0, nowMs: ProtectBanner.returnsAfterMs))

        // The centre feeds the rule from the passkey list.
        let server = Server(status(enabled: false, vault: false, phraseSaved: false))
        var hasPasskey: Bool? = false
        let center = AccountProtectionCenter(
            ports: .init(
                session: { .init(accountIdB64: "acct", sessionToken: "tok") },
                fetchStatus: { _ in try server.status.get() },
                fetchNotices: { _, _ in AccountNoticePage(notices: [], nextAfter: 0, more: false) },
                hasPasskey: { hasPasskey },
                nowMs: { server.now }),
            store: AccountProtectionStore(defaults: defaults()))
        await center.land(accountIdB64: "acct")
        XCTAssertTrue(center.bannerVisible)
        hasPasskey = true
        await center.refresh()
        XCTAssertFalse(center.bannerVisible)
        // The email row still asks to be added again when its vault is gone.
        server.status = .success(status(enabled: true, vault: false, phraseSaved: false))
        await center.refresh()
        XCTAssertFalse(center.bannerVisible)
        XCTAssertEqual(center.rows?.email, .addAgain)
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

    func testRestoreYourKeysHidesThePhraseRowAndOffersTheEmailCodeByTheStatus() {
        // No saved phrase, the server holds the vault: the email code, no phrase row.
        let mail = RestoreKeysRows.decide(status(enabled: true, vault: true, phraseSaved: false))
        XCTAssertFalse(mail.showsPhrase)
        XCTAssertTrue(mail.showsEmailCode)
        // A saved phrase: the phrase row; the mailed code brings no keys.
        let phrase = RestoreKeysRows.decide(status(enabled: true, vault: false, phraseSaved: true))
        XCTAssertTrue(phrase.showsPhrase)
        XCTAssertFalse(phrase.showsEmailCode)
        // The vault was dropped: neither.
        let dropped = RestoreKeysRows.decide(status(enabled: true, vault: false, phraseSaved: false))
        XCTAssertFalse(dropped.showsPhrase)
        XCTAssertFalse(dropped.showsEmailCode)
        // Not read yet: the phrase row stays, nothing is promised.
        let unknown = RestoreKeysRows.decide(nil)
        XCTAssertTrue(unknown.showsPhrase)
        XCTAssertFalse(unknown.showsEmailCode)
    }

    func testTheStatusCarriesTheAuthenticatorFlag() throws {
        let with = try JSONDecoder().decode(
            RecoveryEmailStatus.self,
            from: Data(#"{"available":true,"enabled":true,"vault":true,"phrase_saved":false,"totp":true}"#.utf8))
        XCTAssertEqual(with.totp, true)
        let without = try JSONDecoder().decode(
            RecoveryEmailStatus.self,
            from: Data(#"{"available":true,"enabled":true,"vault":true,"phrase_saved":false}"#.utf8))
        XCTAssertNil(without.totp)
    }

    // MARK: - Strings

    func testEveryKeyTheseScreensUseIsInTheCatalog() {
        let keys = [
            "protect.title", "protect.body", "protect.email.title", "protect.email.badge",
            "protect.email.body", "protect.phrase.title", "protect.phrase.body", "protect.skip",
            "protect.skip.confirm.title", "protect.skip.confirm.body", "protect.skip.confirm.continue",
            "protect.banner.none", "protect.banner.action", "protect.status.email", "protect.status.phrase",
            "protect.status.none", "protect.phrase.save.body", "protect.phrase.save.effect",
            "protect.phrase.save.show", "protect.phrase.save.failed",
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
            "recovery.email.totp.body", "recovery.email.totp.lost", "recovery.email.restoring",
            "recovery.email.save", "notice.was_me", "notice.was_not_me", "notice.secure.new_keys",
            "proof.recovery_email.help", "proof.save_phrase.help", "notice.secure.title",
            "notice.secure.password_saved", "protect.phrase.save.progress", "protect.phrase.save.finishing",
            "recall.phrase.wrong", "recovery_email.add_again.title", "recovery_email.add_again.body",
            "recovery.email.failed", "recovery_email.totp_warning",
        ]
        for key in keys {
            XCTAssertNotEqual(Localization.text(key), key, "\(key) is missing from the catalog")
        }
        XCTAssertTrue(Localization.text(
            "recovery_email.code.sent_to", texts: ["address": "a@b.example"]).contains("a@b.example"))
        XCTAssertTrue(Localization.text("recovery_email.code.resend_in", texts: ["time": "0:45"]).contains("0:45"))
        for line in [
            RecoveryEmailLine.addressInvalid, .codeWrong, .codeExpired, .codeUsedUp,
            .limited(retryAfterSecs: nil), .sendFailed, .unavailable, .vaultNotOpened, .needsUsername,
            .offline, .failed,
        ] {
            let key = try? XCTUnwrap(line.key)
            XCTAssertNotEqual(key.map { Localization.text($0) }, key, "\(line)")
        }
    }
}
