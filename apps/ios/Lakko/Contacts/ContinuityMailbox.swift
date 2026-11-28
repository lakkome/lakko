// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0101 section 5 — for the window after a key reset the account's
// CONTINUITY mailbox is read beside the account mailbox, with the same row
// handler (``ContactsRepository/refreshInbox(waitSecs:source:)`` with
// `.continuity`), and left once it ends. The window is the resetting
// device's own record (``ResetContinuityStore``), else the account's binding
// as the directory serves it (another device of the account), verified under
// this account's key and naming exactly this account's continuity mailbox.
//
// iOS mirror of `continuityWindowOpen` / `pollContinuityMailbox` in
// `apps/web/src/contacts/MailboxContext.tsx`.

@MainActor
final class ContinuityMailboxWindow {
    /// How often another device of a reset account asks the directory
    /// whether a window is open (chosen, not measured).
    static let recheckMs: Int64 = 6 * 60 * 60 * 1000

    private var checked: [String: (expiresAtMs: Int64?, checkedAtMs: Int64)] = [:]

    func cached(accountIdB64: String, nowMs: Int64) -> Int64?? {
        guard let held = checked[accountIdB64], nowMs - held.checkedAtMs < Self.recheckMs else { return nil }
        return .some(held.expiresAtMs)
    }

    func remember(accountIdB64: String, expiresAtMs: Int64?, nowMs: Int64) {
        checked[accountIdB64] = (expiresAtMs, nowMs)
    }

    func previous(accountIdB64: String) -> Int64? {
        checked[accountIdB64]?.expiresAtMs ?? nil
    }

    func reset() { checked = [:] }

    /// The continuity mailbox's own cursor, per account.
    struct CursorStore {
        static let prefix = "lakko.continuity-mailbox-cursor.v1."

        private let defaults: UserDefaults

        init(defaults: UserDefaults = .standard) {
            self.defaults = defaults
        }

        func cursor(accountIdB64: String) -> Int64 {
            max(0, (defaults.object(forKey: Self.prefix + accountIdB64) as? NSNumber)?.int64Value ?? 0)
        }

        func advance(accountIdB64: String, to cursor: Int64) {
            guard cursor > self.cursor(accountIdB64: accountIdB64),
                  let gate = AccountStoreGate.enter(accountIdB64) else { return }
            defer { gate.leave() }
            defaults.set(NSNumber(value: cursor), forKey: Self.prefix + accountIdB64)
        }

        func clear(accountIdB64: String) {
            defaults.removeObject(forKey: Self.prefix + accountIdB64)
        }
    }
}

extension ContactsRepository {
    /// Whether the continuity mailbox is read now. Never throws.
    func continuityWindowOpen(accountIdB64: String, identity: Identity) async -> Bool {
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        if let own = ResetContinuityStore().read(accountIdB64: accountIdB64) {
            // ADR-0101 section 11 — after an unsealed reset of its own this
            // device reads nothing there: peers send nothing to it.
            if ResetContinuityRules.ownResetUnsealed(own) { return false }
            return ResetContinuityRules.mailboxReadable(expiresAtMs: own.expiresAtMs, nowMs: now)
        }
        if let held = continuityWindow.cached(accountIdB64: accountIdB64, nowMs: now) {
            return held.map { now < $0 } ?? false
        }
        guard let accountId = base64Decode(accountIdB64) else { return false }
        var expiresAtMs: Int64?
        do {
            let bundle = try await DiscoveryClient().identityFetch(accountId: accountId)
            let target = ContinuityTargets.peerTarget(
                bindingB64: bundle?.continuityBindingB64, attestedKey: identity.signaturePublicKey(), nowMs: now,
                verify: { try verifyContinuityBinding(binding: $0, identityPk: $1, nowMs: $2) })
            let mine = base64Encode(MailboxKey.continuityFromAccountIdentity(accountIdentity: identity).publicKey())
            if let target, target.mailboxPkB64 == mine { expiresAtMs = target.expiresAtMs }
        } catch {
            expiresAtMs = continuityWindow.previous(accountIdB64: accountIdB64)
        }
        continuityWindow.remember(accountIdB64: accountIdB64, expiresAtMs: expiresAtMs, nowMs: now)
        return expiresAtMs.map { now < $0 } ?? false
    }

    /// Read the continuity mailbox once when its window is open (claim on
    /// open: a refused read claims it, as for the account mailbox). Never
    /// costs the account mailbox its poll.
    func pollContinuityMailboxIfOpen(accountIdB64: String, secrets: IdentitySecrets) async {
        guard let identity = try? Identity.fromSeed(seed: secrets.accountIdentitySeed),
              await continuityWindowOpen(accountIdB64: accountIdB64, identity: identity) else { return }
        await refreshInbox(waitSecs: 0, source: .continuity)
    }

    /// ADR-0101 section 9 — a conversation joined inside this account's own
    /// continuity window (its own record, not the directory check) is noted
    /// for the history requests.
    func noteReturnedConversationIfInsideWindow(groupIdHex: String, accountIdB64: String) {
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        guard let own = ResetContinuityStore().read(accountIdB64: accountIdB64),
              !ResetContinuityRules.ownResetUnsealed(own),
              ResetContinuityRules.mailboxReadable(expiresAtMs: own.expiresAtMs, nowMs: now),
              let groupId = ChatRepository.hexToBytes(groupIdHex) else { return }
        HistoryReturnStore().noteReturned(groupIdB64: base64Encode(groupId), accountIdB64: accountIdB64)
    }
}
