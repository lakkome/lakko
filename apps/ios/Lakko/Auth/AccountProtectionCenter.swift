// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation

/// ADR-0075 (amended 2026-11-26) — what the signed-in account's protection
/// is and what its devices are to be told, as this device knows it: the
/// status of `GET /recovery-email` (the recovery email and the
/// authenticator), whether it has a passkey, the account's published
/// protection (``AccountSeals``, ADR-0101 section 10) and the notices of
/// `GET /account/notices`.
///
/// Read at every session landing (``AuthRepository/onProtectedSessionLanded``)
/// and after every change this device makes. Everything it keeps on the
/// device is a flag or a cursor (``AccountProtectionStore``,
/// ``AccountSealsStore``); never the address, never a code.
///
/// One per process, built by the composition root over the live routes; a
/// test builds its own over scripted ports, so nothing here reaches a
/// server unless the caller wired one.
@MainActor
final class AccountProtectionCenter: ObservableObject {

    struct Session: Equatable {
        let accountIdB64: String
        let sessionToken: String
        /// The account has a username (a guest has none and sees none of
        /// this).
        var hasUsername: Bool = true
    }

    struct Ports {
        /// The signed-in account, or `nil`.
        var session: @MainActor () -> Session?
        var fetchStatus: (_ sessionToken: String) async throws -> RecoveryEmailStatus
        var fetchNotices: (_ sessionToken: String, _ query: NoticeQuery) async throws -> AccountNoticePage
        /// Whether the account has a passkey; `nil` when it could not be
        /// read.
        var hasPasskey: @MainActor () async -> Bool? = { nil }
        /// The recovery address this account's devices keep (the synced
        /// state's register), or `nil`.
        var addressHeld: @MainActor () -> String? = { nil }
        /// Write the register (`nil` = removed). `false` when the synced
        /// state is not loaded yet: tried again once it is.
        var storeAddress: @MainActor (_ address: String?) async -> Bool = { _ in false }
        /// Look at the account's published protection and publish the next
        /// announcement when `change` owes it; the seals as they stand
        /// afterwards, or `nil` when they could not be read.
        var ensureSeals: @MainActor (_ change: SealChange) async -> AccountSeals? = { _ in nil }
        /// This session followed a key reset made on this device (its own
        /// screens said what was kept; the once-per-device notice is not
        /// shown).
        var afterKeyReset: @MainActor () -> Bool = { false }
        var nowMs: () -> Int64

        static func live(_ repository: AuthRepository, sync: DeviceGroupSyncRepository) -> Ports {
            Ports(
                session: { [weak repository] in
                    guard let repository, case .session(let account) = repository.state,
                          let token = repository.identitySecrets?.sessionToken else { return nil }
                    return Session(
                        accountIdB64: account.accountIdB64, sessionToken: token,
                        hasUsername: !account.username.isEmpty)
                },
                fetchStatus: { try await AuthClient().recoveryEmailStatus(sessionToken: $0) },
                fetchNotices: { try await AuthClient().accountNotices(sessionToken: $0, query: $1) },
                hasPasskey: { [weak repository] in await repository?.accountHasPasskey() },
                addressHeld: { [weak sync] in sync?.recoveryEmail },
                storeAddress: { [weak sync] address in await sync?.setRecoveryEmail(address) ?? false },
                ensureSeals: { [weak repository] change in
                    switch await repository?.ensureSealAnnouncement(change) {
                    case .published(let seals), .unchanged(let seals): return seals
                    default: return nil
                    }
                },
                afterKeyReset: { [weak repository] in repository?.keysResetThisSession ?? false },
                nowMs: { Int64(Date().timeIntervalSince1970 * 1000) }
            )
        }
    }

    /// The last status read for the signed-in account; the stored one until
    /// the server answered, `nil` when nothing is known yet.
    @Published private(set) var status: RecoveryEmailStatus?
    /// The notices still to be shown, oldest first.
    @Published private(set) var notices: [AccountNotice] = []
    /// Whether the account has a passkey; `nil` until read.
    @Published private(set) var hasPasskey: Bool?
    /// The account's published protection; the stored one until it was read
    /// again, `nil` when this device never read it.
    @Published private(set) var seals: AccountSeals?
    /// UX 19.1c — the landing found an account with a username and none of
    /// the three: the step stands before the app until one is added.
    @Published private(set) var factorStepRequired = false
    /// UX 19.1d place 3 — the once-per-device dialog is due.
    @Published private(set) var passwordOnlyDialogDue = false
    /// Alignment item G (UX 19.1c, no flash) — the account whose landing
    /// is settled: the factor read answered (or failed), or the wait ran
    /// out. The app's content is not shown before, so an account that needs
    /// the step never sees the app for a moment first.
    @Published private(set) var landingSettled: String?
    /// The longest the app waits for the factor read (chosen, not measured).
    static let landingWaitNanos: UInt64 = 3_000_000_000
    /// The status read of this landing answered (the step is decided only
    /// from an answer, never from the stored status alone).
    private var statusAnswered = false
    /// `It was not me` asked for the screen that secures the account.
    @Published var secureRequested = false
    /// The sign-up step left "Save a recovery phrase" for the app: Settings
    /// opens on it.
    @Published var savePhraseRequested = false

    private let ports: Ports
    private let store: AccountProtectionStore
    private let sealsStore: AccountSealsStore
    private var accountIdB64: String?
    /// The cursor every fetched notice lies at or below.
    private var fetchedThrough: UInt64 = 0
    /// Actions performed before the account was known (a recovery on a
    /// fresh device): written to the store at the landing.
    private var pendingOwn: [(kind: AccountNoticeKind, atMs: Int64)] = []
    private var refreshing = false
    /// The composition root's watch on the address register.
    var addressWatch: AnyCancellable?
    /// The composition root's watch on the own-username register (ADR-0076).
    var usernameWatch: AnyCancellable?
    /// An address to write to the register once the synced state is loaded
    /// (`.some(nil)` = a removal).
    private var pendingAddress: String??

    /// How many pages of notices one landing reads at most.
    static let maxNoticePages = 5

    init(
        ports: Ports, store: AccountProtectionStore = AccountProtectionStore(),
        sealsStore: AccountSealsStore = AccountSealsStore()
    ) {
        self.ports = ports
        self.store = store
        self.sealsStore = sealsStore
    }

    // MARK: What the screens read

    /// The recovery address this device holds, or `nil`. Shown in Settings
    /// on a device that holds it; never logged.
    var addressHeld: String? { ports.addressHeld() }

    /// The account's three second factors; `nil` while either the status or
    /// the passkey list is unknown.
    var factors: AccountFactors? { AccountFactors.of(status: status, hasPasskey: hasPasskey) }

    /// The rows of Settings > Account, or `nil` while nothing is known.
    var rows: ProtectionRows? {
        status.map { ProtectionRows.decide($0, seals: seals) }
    }

    /// Whether the account's phrase was saved: its published protection
    /// lists a phrase seal.
    var phraseSaved: Bool {
        if let seals { return seals.phraseSaved }
        return accountIdB64.map { store.phraseSaved(accountIdB64: $0) } ?? false
    }

    /// A phrase is saved or a passkey keeps the chats.
    var keepsChats: Bool { seals.map { $0.keptBy != .passwordOnly } ?? false }

    /// UX 19.1d — the account is kept by the password only.
    var passwordOnly: Bool { PasswordOnlyNotice.applies(factors: factors, seals: seals) }

    /// The note at the top of Settings > Security > Recovery while the
    /// authenticator is the only second way in.
    var authenticatorOnlyWarning: ProtectStep.Warning? {
        AuthenticatorOnly.settingsWarning(factors: factors, phraseSaved: phraseSaved)
    }

    // MARK: Landing

    /// A session with keys landed for `accountIdB64`: show what is stored,
    /// then read the status and the notices in the background.
    func landed(accountIdB64: String) {
        Task { [weak self] in await self?.land(accountIdB64: accountIdB64) }
    }

    /// The landing itself: the stored status at once, then the server's
    /// status, the account's published protection and the notices.
    func land(accountIdB64: String) async {
        if self.accountIdB64 != accountIdB64 {
            self.accountIdB64 = accountIdB64
            notices = []
            fetchedThrough = 0
            hasPasskey = nil
            factorStepRequired = false
            passwordOnlyDialogDue = false
            owedPasskeySeals = []
            landingSettled = nil
        }
        statusAnswered = false
        status = store.status(accountIdB64: accountIdB64)
        seals = sealsStore.read(accountIdB64: accountIdB64)
        for own in pendingOwn {
            store.markOwnAction(own.kind, atMs: own.atMs, accountIdB64: accountIdB64)
        }
        pendingOwn = []
        await flushPendingAddress()
        await refresh()
        // Item G: the step is decided from the factor read alone, before the
        // notices and the published protection are read, and the app shows.
        evaluateLanding()
        settleLanding(accountIdB64: accountIdB64)
        await loadNotices()
        await readSeals(.landing)
        evaluateLanding()
    }

    /// The landing of `accountIdB64` is settled (item G): its factor read
    /// answered, or the wait for it ran out.
    func settleLanding(accountIdB64: String) {
        if landingSettled != accountIdB64 { landingSettled = accountIdB64 }
    }

    /// The landing rule (19.1c) and the once-per-device notice (19.1d),
    /// from what was just read. Unknown factors decide nothing.
    private func evaluateLanding() {
        guard let session = ports.session(), session.accountIdB64 == accountIdB64 else { return }
        // Once the step stands, only `Continue` on it takes it away (the
        // person may add another way before leaving it).
        if !factorStepRequired, statusAnswered {
            factorStepRequired = FactorLanding.needsStep(hasUsername: session.hasUsername, factors: factors)
        }
        passwordOnlyDialogDue = !factorStepRequired && PasswordOnlyNotice.showsDialog(
            applies: passwordOnly,
            closedOnThisDevice: sealsStore.passwordOnlyNoticeClosed(accountIdB64: session.accountIdB64),
            afterKeyReset: ports.afterKeyReset())
    }

    /// The once-per-device dialog was answered (`Not now`, or one of its
    /// actions): closed for good on this device.
    func closePasswordOnlyDialog() {
        passwordOnlyDialogDue = false
        if let accountIdB64 { sealsStore.closePasswordOnlyNotice(accountIdB64: accountIdB64) }
    }

    // MARK: The account's published protection

    /// Read (and, when `change` owes it, publish) the account's
    /// announcement. A failure keeps what was known.
    @discardableResult
    func readSeals(_ change: SealChange) async -> Bool {
        guard let session = ports.session(), session.accountIdB64 == accountIdB64 else { return false }
        let previous = seals
        guard let fresh = await ports.ensureSeals(change),
              ports.session()?.accountIdB64 == session.accountIdB64 else { return false }
        seals = fresh
        // Another device saved the phrase: said once here (UX 19.5; the
        // server keeps no notice for it since 2026-11-26).
        if let previous, !previous.phraseSaved, fresh.phraseSaved,
           store.ownActions(accountIdB64: session.accountIdB64)[AccountNoticeKind.phraseSaved.rawValue] == nil {
            let now = ports.nowMs()
            if !notices.contains(where: { $0.kind == AccountNoticeKind.phraseSaved.rawValue }) {
                notices.append(AccountNotice(id: 0, kind: AccountNoticeKind.phraseSaved.rawValue, atMs: now))
            }
        }
        store.clearOwnAction(AccountNoticeKind.phraseSaved.rawValue, accountIdB64: session.accountIdB64)
        // A passkey's secret that could not be published when it was given
        // (the account's first directory row was not there yet, or the
        // directory was out) is published now that a look succeeded.
        if case .passkeyAdded = change {} else { await flushPasskeySeals() }
        return true
    }

    /// The secrets passkeys gave on this device that the account's
    /// protection does not list yet. In memory only, never stored: a
    /// process that ends before they were published leaves those passkeys
    /// signing in without keeping the chats.
    private var owedPasskeySeals: [(credentialId: Data, prfOutput: Data)] = []

    private func flushPasskeySeals() async {
        while let next = owedPasskeySeals.first {
            guard await readSeals(.passkeyAdded(credentialId: next.credentialId, prfOutput: next.prfOutput)) else {
                return
            }
            owedPasskeySeals.removeAll { $0.credentialId == next.credentialId }
        }
    }

    // MARK: The address the devices keep

    /// Keep `address` as the account's recovery email (`nil` = removed).
    /// Before the synced state is loaded (a recovery on a fresh device, the
    /// first moments of a landing) the write waits for it.
    func keepAddress(_ address: String?) {
        pendingAddress = .some(address)
        Task { [weak self] in await self?.flushPendingAddress() }
    }

    /// The synced state was loaded: a waiting write runs.
    func syncedStateReady() {
        Task { [weak self] in await self?.flushPendingAddress() }
    }

    /// The register changed (this device or a sibling wrote it).
    func addressChanged() {
        objectWillChange.send()
    }

    func flushPendingAddress() async {
        guard let waiting = pendingAddress, ports.session() != nil else { return }
        if await ports.storeAddress(waiting) {
            // A newer write may have arrived while this one ran.
            if pendingAddress == .some(waiting) { pendingAddress = nil }
            objectWillChange.send()
        }
    }

    /// No account is signed in.
    func reset() {
        accountIdB64 = nil
        pendingAddress = nil
        status = nil
        seals = nil
        notices = []
        hasPasskey = nil
        factorStepRequired = false
        passwordOnlyDialogDue = false
        landingSettled = nil
        statusAnswered = false
        owedPasskeySeals = []
        fetchedThrough = 0
        secureRequested = false
        savePhraseRequested = false
    }

    /// Read the status from the server. A failure keeps what was known.
    func refresh() async {
        await readStatus()
    }

    private func readStatus() async {
        guard !refreshing, let session = ports.session() else { return }
        refreshing = true
        defer { refreshing = false }
        if let known = await ports.hasPasskey(), ports.session()?.accountIdB64 == session.accountIdB64 {
            hasPasskey = known
        }
        do {
            let fresh = try await ports.fetchStatus(session.sessionToken)
            guard ports.session()?.accountIdB64 == session.accountIdB64 else { return }
            apply(fresh, accountIdB64: session.accountIdB64)
            statusAnswered = true
        } catch {
            NSLog("[protect] the account's protection could not be read; keeping what was known")
        }
    }

    /// Store and publish a status this device just learned.
    func apply(_ status: RecoveryEmailStatus, accountIdB64: String) {
        store.setStatus(status, accountIdB64: accountIdB64)
        if self.accountIdB64 == accountIdB64 || self.accountIdB64 == nil {
            self.accountIdB64 = accountIdB64
            self.status = status
        }
    }

    /// `Continue` on the required step (UX 19.1c): it is enabled only once
    /// one of the three is there, and it is the only way off the step.
    func factorStepDone() {
        guard factors?.any == true else { return }
        factorStepRequired = false
        // A sign-up's landing can run before the account's first directory
        // row exists: the account's protection is looked at again here.
        Task { [weak self] in await self?.readSeals(.landing) }
    }

    /// An enrolment finished on this device for `address` (normalised): the
    /// account's devices keep it from here.
    func enrolled(_ result: EmailEnrolled, replaced: Bool, address: String? = nil) {
        if let address { keepAddress(address) }
        noteOwnAction(replaced ? .emailReplaced : .emailAdded)
        guard let accountIdB64 else { return }
        let known = status ?? RecoveryEmailStatus(available: true, enabled: false)
        apply(
            RecoveryEmailStatus(available: known.available, enabled: result.enabled, totp: known.totp),
            accountIdB64: accountIdB64)
    }

    /// An authenticator app was confirmed on this device.
    func authenticatorAdded() {
        guard let accountIdB64 else { return }
        let known = status ?? RecoveryEmailStatus(available: false, enabled: false)
        apply(
            RecoveryEmailStatus(available: known.available, enabled: known.enabled, totp: true),
            accountIdB64: accountIdB64)
    }

    /// A passkey was added on this device. With the secret its authenticator
    /// gave (a passkey that keeps the chats) the account's protection is
    /// published again with it.
    func passkeyAdded(credentialId: Data? = nil, prfOutput: Data? = nil) {
        hasPasskey = true
        guard let credentialId, let prfOutput else { return }
        owedPasskeySeals.removeAll { $0.credentialId == credentialId }
        owedPasskeySeals.append((credentialId, prfOutput))
        Task { [weak self] in await self?.flushPasskeySeals() }
    }

    /// A passkey was removed on this device; `remaining` is how many the
    /// account has left.
    func passkeyRemoved(credentialId: Data, remaining: Int) {
        hasPasskey = remaining > 0
        owedPasskeySeals.removeAll { $0.credentialId == credentialId }
        Task { [weak self] in await self?.readSeals(.passkeyRemoved(credentialId: credentialId)) }
    }

    /// The three-word check of "Save a recovery phrase" passed: publish the
    /// account's protection with the phrase in it. `true` once the
    /// announcement lists it; only then does anything count as saved
    /// (UX 19.5). The notice about it is not shown on this device.
    func savePhraseSeal() async -> Bool {
        guard let accountIdB64 else { return false }
        store.markOwnAction(.phraseSaved, atMs: ports.nowMs(), accountIdB64: accountIdB64)
        await readSeals(.phraseSaved)
        guard seals?.phraseSaved == true else {
            store.clearOwnAction(AccountNoticeKind.phraseSaved.rawValue, accountIdB64: accountIdB64)
            return false
        }
        store.setPhraseSaved(true, accountIdB64: accountIdB64)
        passwordOnlyDialogDue = false
        return true
    }

    /// The recovery email was removed on this device.
    func removed() {
        // Removing the recovery email writes the empty value.
        keepAddress(nil)
        noteOwnAction(.emailRemoved)
        guard let accountIdB64, let known = status else { return }
        apply(
            RecoveryEmailStatus(available: known.available, enabled: false, totp: known.totp),
            accountIdB64: accountIdB64)
    }

    // MARK: Notices

    /// This device performed `kind` just now: the notice about it is not
    /// shown here. Before the account is known (a recovery on a fresh
    /// device) the marker waits for the landing.
    func noteOwnAction(_ kind: AccountNoticeKind) {
        let now = ports.nowMs()
        if let accountIdB64 {
            store.markOwnAction(kind, atMs: now, accountIdB64: accountIdB64)
        } else {
            pendingOwn.append((kind, now))
        }
    }

    /// Read the notices: after the stored cursor, or, on a device that
    /// never kept one, of the last 30 days. What comes back is shown (minus
    /// this device's own actions), except on a device with no cursor: that
    /// device is new, so it only takes the newest id as its cursor. A
    /// failure shows nothing and moves nothing: the next landing reads again.
    func loadNotices() async {
        guard let session = ports.session() else { return }
        let accountIdB64 = session.accountIdB64
        let cursor = store.noticeCursor(accountIdB64: accountIdB64)
        var fetched: [AccountNotice] = []
        var query = NoticeQuery.first(cursor: cursor, nowMs: ports.nowMs())
        var through = cursor ?? 0
        do {
            for _ in 0..<Self.maxNoticePages {
                let page = try await ports.fetchNotices(session.sessionToken, query)
                fetched.append(contentsOf: page.notices)
                guard page.more, page.nextAfter > through else { break }
                through = page.nextAfter
                query = .after(through)
            }
        } catch {
            NSLog("[protect] the account's notices could not be read")
            return
        }
        guard ports.session()?.accountIdB64 == accountIdB64, self.accountIdB64 == accountIdB64 else { return }
        let outcome = NoticeSelection.select(
            fetched: fetched, cursor: cursor, ownActions: store.ownActions(accountIdB64: accountIdB64))
        for kind in outcome.spentMarkers {
            store.clearOwnAction(kind, accountIdB64: accountIdB64)
        }
        fetchedThrough = outcome.cursor
        // A notice this device made itself (id 0: "a phrase was saved",
        // read from the account's published protection) stays.
        notices = outcome.shown + notices.filter { $0.id == 0 }
        storeCursor(accountIdB64: accountIdB64)
    }

    /// `It was me`, or the notice was otherwise answered: it is not shown
    /// again.
    func acknowledge(_ notice: AccountNotice) {
        notices.removeAll { $0.id == notice.id }
        if let accountIdB64 { storeCursor(accountIdB64: accountIdB64) }
    }

    /// `It was not me`: the notice is answered and the screen that secures
    /// the account opens.
    func wasNotMe(_ notice: AccountNotice) {
        acknowledge(notice)
        secureRequested = true
    }

    /// The seen cursor: everything fetched once nothing is left to show,
    /// else just below the oldest notice still on screen, so a notice the
    /// person never answered is read again at the next landing.
    private func storeCursor(accountIdB64: String) {
        let cursor = notices.map(\.id).filter { $0 > 0 }.min().map { $0 - 1 } ?? fetchedThrough
        store.setNoticeCursor(
            max(cursor, store.noticeCursor(accountIdB64: accountIdB64) ?? 0), accountIdB64: accountIdB64)
    }
}
