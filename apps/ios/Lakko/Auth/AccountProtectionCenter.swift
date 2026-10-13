// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation

/// ADR-0067 — what the signed-in account's protection is and what its
/// devices are to be told, as this device knows it: the status of
/// `GET /recovery-email`, the notices of `GET /account/notices`, the
/// no-protection banner.
///
/// Read at every session landing (``AuthRepository/onProtectedSessionLanded``)
/// and after every change this device makes. Everything it keeps on the
/// device is a flag or a cursor (``AccountProtectionStore``); never the
/// address, never a code.
///
/// One per process, built by the composition root over the live routes; a
/// test builds its own over scripted ports, so nothing here reaches a
/// server unless the caller wired one.
@MainActor
final class AccountProtectionCenter: ObservableObject {

    struct Session: Equatable {
        let accountIdB64: String
        let sessionToken: String
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
        /// Seal the current phrase under the username and `address` with a
        /// fresh salt (off the main thread) and `PUT /recovery-email/vault`.
        var reseal: @MainActor (_ sessionToken: String, _ address: String) async throws -> Void = { _, _ in
            throw AuthClientError.argument("re-sealing is not wired")
        }
        var nowMs: () -> Int64

        static func live(_ repository: AuthRepository, sync: DeviceGroupSyncRepository) -> Ports {
            Ports(
                session: { [weak repository] in
                    guard let repository, case .session(let account) = repository.state,
                          let token = repository.identitySecrets?.sessionToken else { return nil }
                    return Session(accountIdB64: account.accountIdB64, sessionToken: token)
                },
                fetchStatus: { try await AuthClient().recoveryEmailStatus(sessionToken: $0) },
                fetchNotices: { try await AuthClient().accountNotices(sessionToken: $0, query: $1) },
                hasPasskey: { [weak repository] in await repository?.accountHasPasskey() },
                addressHeld: { [weak sync] in sync?.recoveryEmail },
                storeAddress: { [weak sync] address in await sync?.setRecoveryEmail(address) ?? false },
                reseal: { [weak repository] sessionToken, address in
                    // A guest has no handle; while a key reset's publish is
                    // owed the phrase held is not the published one yet.
                    guard let repository, let handle = DeviceSession.current(repository)?.loginHandle,
                          let phrase = repository.heldPhraseForEmailVault() else {
                        throw SessionNotReady()
                    }
                    // A fresh 16-byte salt from the OS CSPRNG.
                    var rng = SystemRandomNumberGenerator()
                    let salt = Data((0..<16).map { _ in UInt8.random(in: UInt8.min...UInt8.max, using: &rng) })
                    let sealed = try await EmailVaultWork.seal(
                        phrase: phrase, handle: handle, address: address, vaultSalt: salt)
                    try await AuthClient().recoveryEmailVaultPut(
                        sessionToken: sessionToken, handle: handle, address: address,
                        vaultSalt: salt, sealedEntropy: sealed)
                },
                nowMs: { Int64(Date().timeIntervalSince1970 * 1000) }
            )
        }
    }

    /// The last status read for the signed-in account; the stored one until
    /// the server answered, `nil` when nothing is known yet.
    @Published private(set) var status: RecoveryEmailStatus?
    /// The notices still to be shown, oldest first.
    @Published private(set) var notices: [AccountNotice] = []
    @Published private(set) var bannerDismissedAtMs: Int64?
    /// Whether the account has a passkey; `nil` until read.
    @Published private(set) var hasPasskey: Bool?
    /// `It was not me` asked for the screen that secures the account.
    @Published var secureRequested = false

    private let ports: Ports
    private let store: AccountProtectionStore
    private var accountIdB64: String?
    /// The cursor every fetched notice lies at or below.
    private var fetchedThrough: UInt64 = 0
    /// Actions performed before the account was known (a recovery on a
    /// fresh device): written to the store at the landing.
    private var pendingOwn: [(kind: AccountNoticeKind, atMs: Int64)] = []
    private var refreshing = false
    /// The composition root's watch on the address register.
    var addressWatch: AnyCancellable?
    /// The composition root's watch on the own-username register (ADR-0068).
    var usernameWatch: AnyCancellable?
    /// An address to write to the register once the synced state is loaded
    /// (`.some(nil)` = a removal).
    private var pendingAddress: String??
    /// One re-seal in flight per account.
    private var resealing = false
    /// The address a re-seal was already tried for since the last landing:
    /// a failure is retried at the next landing, never in a loop.
    private var resealTriedFor: String?

    /// How many pages of notices one landing reads at most.
    static let maxNoticePages = 5

    init(ports: Ports, store: AccountProtectionStore = AccountProtectionStore()) {
        self.ports = ports
        self.store = store
    }

    // MARK: What the screens read

    /// The recovery address this device holds, or `nil`. Shown in Settings
    /// on a device that holds it; never logged.
    var addressHeld: String? { ports.addressHeld() }

    /// The rows of Settings > Account, or `nil` while nothing is known.
    var rows: ProtectionRows? {
        status.map { ProtectionRows.decide($0, addressHeld: addressHeld != nil) }
    }

    /// What the banner says.
    var bannerCopy: ProtectBanner.Copy? {
        status.map { ProtectBanner.copy($0, addressHeld: addressHeld != nil) }
    }

    /// Whether the "no way back in" banner is on screen.
    var bannerVisible: Bool {
        guard let status else { return false }
        // An account with a passkey has a way back in: no banner.
        return ProtectBanner.shows(
            protection: status.protection, hasPasskey: hasPasskey == true,
            resealing: EmailReseal.resealNeeded(
                enabled: status.enabled, vault: status.vault, phraseSaved: status.phraseSaved,
                addressHeld: addressHeld != nil),
            dismissedAtMs: bannerDismissedAtMs, nowMs: ports.nowMs())
    }

    /// Whether the account's phrase was saved, as far as this device knows.
    var phraseSaved: Bool {
        if let status { return status.phraseSaved }
        return accountIdB64.map { store.phraseSaved(accountIdB64: $0) } ?? false
    }

    // MARK: Landing

    /// A session with keys landed for `accountIdB64`: show what is stored,
    /// then read the status and the notices in the background.
    func landed(accountIdB64: String) {
        Task { [weak self] in await self?.land(accountIdB64: accountIdB64) }
    }

    /// The landing itself: the stored status at once, then the server's
    /// status and the notices.
    func land(accountIdB64: String) async {
        if self.accountIdB64 != accountIdB64 {
            self.accountIdB64 = accountIdB64
            notices = []
            fetchedThrough = 0
        }
        status = store.status(accountIdB64: accountIdB64)
        bannerDismissedAtMs = store.bannerDismissedAtMs(accountIdB64: accountIdB64)
        for own in pendingOwn {
            store.markOwnAction(own.kind, atMs: own.atMs, accountIdB64: accountIdB64)
        }
        pendingOwn = []
        // A failed re-seal is tried again at each landing.
        resealTriedFor = nil
        await flushPendingAddress()
        await refresh()
        await loadNotices()
    }

    // MARK: The address the devices keep

    /// Keep `address` as the account's recovery email (`nil` = removed).
    /// Before the synced state is loaded (a recovery on a fresh device, the
    /// first moments of a landing) the write waits for it.
    func keepAddress(_ address: String?) {
        pendingAddress = .some(address)
        Task { [weak self] in await self?.flushPendingAddress() }
    }

    /// The synced state was loaded: a waiting write runs, and the re-seal
    /// rule is evaluated with what the register holds.
    func syncedStateReady() {
        Task { [weak self] in
            await self?.flushPendingAddress()
            await self?.resealIfNeeded()
        }
    }

    /// The register changed (this device or a sibling wrote it).
    func addressChanged() {
        objectWillChange.send()
        Task { [weak self] in await self?.resealIfNeeded() }
    }

    func flushPendingAddress() async {
        guard let waiting = pendingAddress, ports.session() != nil else { return }
        if await ports.storeAddress(waiting) {
            // A newer write may have arrived while this one ran.
            if pendingAddress == .some(waiting) { pendingAddress = nil }
            objectWillChange.send()
        }
    }

    /// The automatic re-seal (``EmailReseal``): silent, one attempt in
    /// flight, one attempt per address per landing.
    func resealIfNeeded() async {
        guard !resealing, let session = ports.session(), let status,
              self.accountIdB64 == session.accountIdB64, let address = addressHeld,
              EmailReseal.resealNeeded(
                  enabled: status.enabled, vault: status.vault, phraseSaved: status.phraseSaved,
                  addressHeld: true),
              resealTriedFor != address else { return }
        resealing = true
        resealTriedFor = address
        var failure: Error?
        do {
            try await ports.reseal(session.sessionToken, address)
        } catch {
            failure = error
        }
        resealing = false
        switch EmailReseal.outcome(of: failure) {
        case .done:
            await readStatus()
        case .addressStale:
            // The address held here is not the account's: forget it; "add
            // your email again" takes over.
            NSLog("[protect] the recovery address held here is stale; cleared")
            _ = await ports.storeAddress(nil)
            objectWillChange.send()
        case .retryAtNextLanding:
            NSLog("[protect] the email vault was not put back; tried again at the next landing")
        }
    }

    /// No account is signed in.
    func reset() {
        accountIdB64 = nil
        pendingAddress = nil
        resealTriedFor = nil
        status = nil
        notices = []
        bannerDismissedAtMs = nil
        hasPasskey = nil
        fetchedThrough = 0
        secureRequested = false
    }

    /// Read the status from the server, then evaluate the automatic
    /// re-seal. A failure keeps what was known.
    func refresh() async {
        await readStatus()
        // After every read of the status.
        await resealIfNeeded()
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

    /// An enrolment finished on this device for `address` (normalised): the
    /// account's devices keep it from here.
    func enrolled(_ result: EmailEnrolled, replaced: Bool, address: String? = nil) {
        if let address { keepAddress(address) }
        noteOwnAction(replaced ? .emailReplaced : .emailAdded)
        guard let accountIdB64 else { return }
        let known = status ?? RecoveryEmailStatus(
            available: true, enabled: false, vault: false, phraseSaved: phraseSaved)
        apply(
            RecoveryEmailStatus(
                available: known.available, enabled: result.enabled, vault: result.vault,
                phraseSaved: known.phraseSaved, totp: known.totp),
            accountIdB64: accountIdB64)
    }

    /// A passkey was added on this device.
    func passkeyAdded() { hasPasskey = true }

    /// The recovery email was removed on this device.
    func removed() {
        // Removing the recovery email writes the empty value.
        keepAddress(nil)
        noteOwnAction(.emailRemoved)
        guard let accountIdB64, let known = status else { return }
        apply(
            RecoveryEmailStatus(
                available: known.available, enabled: false, vault: false, phraseSaved: known.phraseSaved,
                totp: known.totp),
            accountIdB64: accountIdB64)
    }

    // MARK: The banner

    func dismissBanner() {
        guard let accountIdB64 else { return }
        let now = ports.nowMs()
        store.setBannerDismissed(atMs: now, accountIdB64: accountIdB64)
        bannerDismissedAtMs = now
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
    /// this device's own actions) and a cursor is kept from then on. A
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
        notices = outcome.shown
        storeCursor(accountIdB64: accountIdB64)
        // A phrase was saved on another device: this one learns it now, not
        // at the next landing.
        if fetched.contains(where: { $0.known == .phraseSaved }) {
            store.setPhraseSaved(true, accountIdB64: accountIdB64)
            await refresh()
        }
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
        let cursor = notices.map(\.id).min().map { $0 > 0 ? $0 - 1 : 0 } ?? fetchedThrough
        store.setNoticeCursor(
            max(cursor, store.noticeCursor(accountIdB64: accountIdB64) ?? 0), accountIdB64: accountIdB64)
    }
}
