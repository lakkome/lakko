// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0076 — changing a username, and what keeps a rename from being
// reverted by a device that has not heard of it. Pure rules:
//
//   * `LoginHandleState` / `HandleSync`   the session's handle sync: check
//                                         first, bind only a first handle;
//   * `OwnUsernameRule`                   what a device does with the
//                                         own-username register;
//   * `UsernameRename`                    the rename's steps, its owed
//                                         record, the too-soon rule and what
//                                         the screen warns about.
//
// The flow over the network is `AuthRepository.renameUsername(to:)`.

/// `POST /auth/login-handle/check`: the account's bound handle relative to
/// the one asked about.
public enum LoginHandleState: String, Equatable, Sendable {
    /// It is the bound one.
    case same
    /// The account is bound to ANOTHER handle: it was renamed elsewhere.
    case other
    /// The account has no handle.
    case none
}

/// The session's handle sync (ADR-0076 section 2). It replaces the blind
/// once-per-session `PUT`, which let a device with a stale name revert a
/// rename made on another device.
public enum HandleSync {

    public enum Action: Equatable {
        /// The handle of the local name is the bound one.
        case nothing
        /// The account has no handle: bind this one (a first bind needs no
        /// grant).
        case bind
        /// The account was renamed elsewhere: bind NOTHING and wait for the
        /// name from the account's other devices (the register).
        case waitForRegister
    }

    public static func action(_ state: LoginHandleState) -> Action {
        switch state {
        case .same: return .nothing
        case .none: return .bind
        case .other: return .waitForRegister
        }
    }
}

/// What a device does when it reads the own-username register (ADR-0076
/// section 3).
public enum OwnUsernameRule {

    public enum Action: Equatable {
        case nothing
        /// The register is empty and this device has a name (an account
        /// from before the register): write it once.
        case seed(String)
        /// A name written elsewhere differs from the local marker: adopt it.
        case adopt(String)
        /// This device's own write has not reached the register yet: write
        /// it again and adopt nothing meanwhile (the register still holds
        /// the name this device just replaced).
        case writePending(String)
    }

    /// - Parameters:
    ///   - local: the username in the local account marker ("" for a guest).
    ///   - register: the register's value, `nil` when never written or removed.
    ///   - pending: a name this device wrote locally and still owes the register.
    ///   - handleConfirmed: auth confirmed in THIS session that the account
    ///     is bound to the local name's handle (the check answered `same`,
    ///     or this device made the first bind on `none`). An empty register
    ///     is seeded only then: a stale device that has not yet received a
    ///     rename would otherwise write its old name with a newer stamp and
    ///     undo the rename on every device. On `other`, or while the check
    ///     has not answered, it waits for the register.
    public static func action(
        local: String, register: String?, pending: String?, handleConfirmed: Bool
    ) -> Action {
        if let pending, !pending.isEmpty {
            return register == pending ? .nothing : .writePending(pending)
        }
        guard let register, !register.isEmpty else {
            return local.isEmpty || !handleConfirmed ? .nothing : .seed(local)
        }
        return register == local ? .nothing : .adopt(register)
    }
}

/// The rename (ADR-0076 section 4), pure.
public enum UsernameRename {

    /// One rename per 30 days (D2).
    public static let intervalMs: Int64 = 30 * 24 * 60 * 60 * 1000

    /// When the next change is possible (unix ms), or `nil` when one is
    /// possible now. The device's own stamp only; the server's `429` is the
    /// authority.
    public static func tooSoonUntilMs(lastChangeAtMs: Int64?, nowMs: Int64) -> Int64? {
        guard let lastChangeAtMs else { return nil }
        let until = lastChangeAtMs + intervalMs
        return nowMs < until && nowMs >= lastChangeAtMs ? until : nil
    }

    /// What the screen warns about.
    public struct Warnings: Equatable {
        /// `recovery_email.username_change`: the account has a recovery
        /// email and this device does not hold its address, so the rename
        /// turns the recovery email off.
        public let recoveryEmailTurnedOff: Bool
    }

    public static func warnings(recoveryEmailEnabled: Bool, addressHeld: Bool) -> Warnings {
        Warnings(recoveryEmailTurnedOff: recoveryEmailEnabled && !addressHeld)
    }

    /// What the bind carries for the recovery email: nothing when the
    /// account has none or this device does not hold the address (the email
    /// is then dropped); else the address, with a vault sealed under the
    /// new handle only while no phrase is saved.
    public enum EmailCarry: Equatable {
        case none
        case addressOnly
        case addressAndVault
    }

    public static func emailCarry(recoveryEmailEnabled: Bool, addressHeld: Bool, phraseSaved: Bool) -> EmailCarry {
        guard recoveryEmailEnabled, addressHeld else { return .none }
        return phraseSaved ? .addressOnly : .addressAndVault
    }

    // MARK: The owed record

    /// What a rename still owes, written before the first request so a
    /// crash between steps is finished or rolled back at the next landing.
    public struct Record: Codable, Equatable {
        public enum Phase: String, Codable, Equatable {
            /// The handle bind was sent; whether it landed is not known.
            case binding
            /// The handle is the new one; discovery still has the old name.
            case bound
            /// Discovery has the new name; the local marker, the register
            /// and the re-publish are owed.
            case replaced
        }

        public let oldName: String
        public let newName: String
        public var phase: Phase
        public let startedAtMs: Int64

        public init(oldName: String, newName: String, phase: Phase, startedAtMs: Int64) {
            self.oldName = oldName
            self.newName = newName
            self.phase = phase
            self.startedAtMs = startedAtMs
        }

        enum CodingKeys: String, CodingKey {
            case oldName = "old_name"
            case newName = "new_name"
            case phase
            case startedAtMs = "started_at_ms"
        }
    }

    /// What discovery's replace came to.
    public enum ReplaceOutcome: Equatable {
        case replaced
        /// `409 username-taken`.
        case taken
        /// `429 rename-too-soon`.
        case tooSoon(retryAfterSecs: Int?)
        /// No answer: the replace may or may not have landed.
        case unknown
        case failed

        public static func of(status: Int, body: String) -> ReplaceOutcome {
            let json = (try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any]
            let code = json?["error"] as? String
            switch (status, code) {
            case (200..<300, _): return .replaced
            case (409, _): return .taken
            case (429, "rename-too-soon"):
                return .tooSoon(retryAfterSecs: (json?["retry_after_secs"] as? NSNumber)?.intValue)
            default: return .failed
            }
        }

        public static func of(_ error: Error?) -> ReplaceOutcome {
            guard let error else { return .replaced }
            switch error as? DiscoveryClientError {
            case .http(let status, _, let body): return of(status: status, body: body)
            case .network: return .unknown
            default: return .failed
            }
        }
    }

    /// The next step after discovery's answer.
    public enum AfterReplace: Equatable {
        /// Step d: the marker, the session, the register, the re-publish.
        case finalize
        /// Bind the old handle back (the recovery email re-keyed back), then
        /// show why.
        case rollBack
        /// Unknown whether it landed: keep the record; the next landing
        /// asks again.
        case keepOwed
    }

    public static func afterReplace(_ outcome: ReplaceOutcome) -> AfterReplace {
        switch outcome {
        case .replaced: return .finalize
        case .taken, .tooSoon, .failed: return .rollBack
        case .unknown: return .keepOwed
        }
    }

    /// What a stored record asks of a landing.
    public enum Resume: Equatable {
        case nothing
        /// Ask the server which handle is bound, then go on from there.
        case checkHandle
        /// The handle is the new one: run discovery's replace.
        case replace
        /// Discovery is done: finish locally.
        case finalize
    }

    public static func resume(_ record: Record?) -> Resume {
        guard let record else { return .nothing }
        switch record.phase {
        case .binding: return .checkHandle
        case .bound: return .replace
        case .replaced: return .finalize
        }
    }

    /// For a record left at `binding`: what the handle check means. `new`
    /// is the state of the NEW name's handle.
    public static func afterBindingCheck(newHandle state: LoginHandleState) -> Resume {
        // `same`: the bind landed, discovery is next. Anything else: it did
        // not land, nothing changed, the record goes.
        state == .same ? .replace : .nothing
    }

    /// A rollback at a landing needs the fresh-auth grant (binding the old
    /// handle back CHANGES the handle). Without one the record is kept and
    /// the rename screen finishes the rollback after the next proof.
    public static func canRollBack(holdsGrant: Bool) -> Bool { holdsGrant }

    /// The failure a screen shows.
    public enum Failure: Equatable {
        case taken
        case tooSoon(untilMs: Int64?)
        case bindLimited(retryAfterSecs: Int?)
        case proofNeeded
        case rule
        case sameName
        case offline
        case failed
    }

    public static func failure(_ outcome: ReplaceOutcome, nowMs: Int64) -> Failure {
        switch outcome {
        case .taken: return .taken
        case .tooSoon(let secs): return .tooSoon(untilMs: secs.map { nowMs + Int64($0) * 1000 })
        case .unknown: return .offline
        case .replaced, .failed: return .failed
        }
    }
}

/// A rename did not happen (or was rolled back): why.
public struct UsernameRenameFailed: Error, Equatable {
    public let failure: UsernameRename.Failure
    public init(_ failure: UsernameRename.Failure) { self.failure = failure }
}
