// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Who may write an account's stores on this device, and when.
///
/// A session's work writes its per-account stores from two places the wipe
/// does not control: off the main actor (the backup, contacts and pairing
/// clients are not main-actor types, so what follows an `await` in them runs
/// on the global executor), and on the main actor after a wait, with the
/// account id captured before it. Forget device, Delete account and a guest's
/// removal run on the main actor and take none of that into account, so a
/// write already past its "which account?" read went on to finish after the
/// wipe, and put a forgotten account's data back on disk.
///
/// A binding check does not close the first kind: the two sides run at the
/// same time and have to exclude each other.
///
///  - A store's mutator holds the gate shared (``enter(_:)`` or
///    ``enterOrEnd(_:)``) for the length of ONE write: disk, Keychain or
///    `UserDefaults`, no network, no suspension.
///  - Ending a session takes it exclusively (``seal(_:wiping:during:)``): it
///    waits for the writes in flight, and from then on the account's writes
///    are refused. The wipe of a forgotten account runs inside it.
///  - The account is written again from its next landing on this device
///    (``open(_:)``).
///
/// That a hold never spans a wait is what lets the main actor seal: it waits
/// for disk writes in flight, never for a request.
///
/// Process-wide and in memory. A new process starts with every account open,
/// which is right: no writer of an ended session survives the process. The
/// notification service extension is a process of its own and writes none of
/// these stores (C-59); the gate is compiled into it only because store
/// sources it reads are.
///
/// Mirror of `apps/android/.../account/AccountStoreGate.kt`.
public enum AccountStoreGate {

    /// One held write. ``leave()`` ends it; call it exactly once (`defer`).
    public struct Hold {
        fileprivate let locked: Bool

        public func leave() {
            let depth = AccountStoreGate.depth
            AccountStoreGate.depth = max(0, depth - 1)
            if locked { pthread_rwlock_unlock(AccountStoreGate.lock) }
        }
    }

    /// Thrown by ``enterOrEnd(_:)`` for a sealed account: the caller must not
    /// go on (to a publish) on the strength of a write that was not made.
    /// A cancellation, not a failure (``Cancellation``).
    public struct Sealed: Error, Equatable {
        public init() {}
    }

    private static let lock: UnsafeMutablePointer<pthread_rwlock_t> = {
        let pointer = UnsafeMutablePointer<pthread_rwlock_t>.allocate(capacity: 1)
        pthread_rwlock_init(pointer, nil)
        return pointer
    }()

    /// Sealed accounts, and whether their stores were wiped while sealed.
    /// Guarded by ``lock``.
    private static var sealed: [String: Bool] = [:]

    /// Accounts whose stores were removed by a FULL wipe (Forget device,
    /// Delete account, a removed device, the end of a recall), as opposed to
    /// the STAGED wipe of ADR-0074 section 1.1, which keeps the recall
    /// record. Guarded by ``lock``.
    private static var fullyWiped: Set<String> = []

    /// How many times each account's session has ended in this process.
    /// Guarded by ``lock``.
    private static var endings: [String: UInt64] = [:]

    /// How many times each account's stores were wiped in this process (full
    /// or staged). Guarded by ``lock``.
    private static var wipes: [String: UInt64] = [:]

    /// The wipes of one account so far, as work that started then holds them
    /// (``wipeMark(_:)``).
    ///
    /// For what a sign-in writes BEFORE its session lands (a key the device is
    /// about to rely on, written off the main actor or after a wait): the
    /// account may still be sealed then by an earlier sign-out on this device,
    /// or not opened yet, so neither "sealed" nor "wiped and not reopened" is
    /// the question. The question is whether the account was wiped AFTER the
    /// sign-in started: such a write would put a forgotten account's key back.
    /// A sign-out, or a wipe that came before the work started, refuses
    /// nothing here.
    public struct WipeMark: Equatable {
        fileprivate let key: String
        fileprivate let wipes: UInt64
    }

    /// The wipes of `accountIdB64` so far.
    public static func wipeMark(_ accountIdB64: String) -> WipeMark {
        pthread_rwlock_rdlock(lock)
        defer { pthread_rwlock_unlock(lock) }
        let key = key(accountIdB64)
        return WipeMark(key: key, wipes: wipes[key] ?? 0)
    }

    /// The wipes of EVERY account so far: a ``WipeMark`` for work that does
    /// not know its account yet when it starts. A sign-in by name learns the
    /// account id from the server's answer, after its first wait; the mark
    /// must still be the one from before that wait, or a wipe during it
    /// would go unnoticed.
    public struct WipeMarks {
        fileprivate let wipes: [String: UInt64]

        /// The mark `accountIdB64` had when these marks were taken. An
        /// account never wiped by then is at zero.
        public func mark(_ accountIdB64: String) -> WipeMark {
            let key = AccountStoreGate.key(accountIdB64)
            return WipeMark(key: key, wipes: wipes[key] ?? 0)
        }
    }

    /// The wipes of every account so far (``WipeMarks``).
    public static func wipeMarks() -> WipeMarks {
        pthread_rwlock_rdlock(lock)
        defer { pthread_rwlock_unlock(lock) }
        return WipeMarks(wipes: wipes)
    }

    /// Begin one write that is refused only when the account was wiped since
    /// `mark` was taken (``WipeMark``).
    public static func enter(unlessWipedSince mark: WipeMark) -> Hold? {
        if sealing || depth > 0 {
            depth += 1
            return Hold(locked: false)
        }
        pthread_rwlock_rdlock(lock)
        if (wipes[mark.key] ?? 0) != mark.wipes {
            pthread_rwlock_unlock(lock)
            return nil
        }
        depth += 1
        return Hold(locked: true)
    }

    /// ``enter(unlessWipedSince:)`` for a write its caller must not go on
    /// without: refused, it throws ``Sealed``.
    public static func enterOrEnd(unlessWipedSince mark: WipeMark) throws -> Hold {
        guard let hold = enter(unlessWipedSince: mark) else { throw Sealed() }
        return hold
    }

    /// One session of one account, as a long-lived handle holds it.
    ///
    /// An account that signs out and back in is open again, but a handle
    /// made in the session before (a DeviceGroup identity and its provider,
    /// which is persisted whole) is not that session's: writing it would put
    /// the earlier session's state over the new one's. A write entered with
    /// a ``Session`` is refused once the session it was taken in has ended,
    /// whether or not the account is open by then.
    public struct Session: Equatable {
        fileprivate let key: String
        fileprivate let ending: UInt64
    }

    /// The session of `accountIdB64` that is current now.
    public static func session(_ accountIdB64: String) -> Session {
        pthread_rwlock_rdlock(lock)
        defer { pthread_rwlock_unlock(lock) }
        let key = key(accountIdB64)
        return Session(key: key, ending: endings[key] ?? 0)
    }

    /// ``enter(_:)`` for a handle's write: also refused when the session the
    /// handle was made in has ended.
    public static func enter(session: Session) -> Hold? {
        if sealing || depth > 0 {
            depth += 1
            return Hold(locked: false)
        }
        pthread_rwlock_rdlock(lock)
        if sealed[session.key] != nil || (endings[session.key] ?? 0) != session.ending {
            pthread_rwlock_unlock(lock)
            return nil
        }
        depth += 1
        return Hold(locked: true)
    }

    /// ``enterOrEnd(_:)`` for a handle's write.
    public static func enterOrEnd(session: Session) throws -> Hold {
        guard let hold = enter(session: session) else { throw Sealed() }
        return hold
    }

    private static let depthKey = "me.lakko.account-store-gate.depth"
    private static let sealingKey = "me.lakko.account-store-gate.sealing"

    /// How many holds this thread has. A nested hold takes no lock: a second
    /// shared acquisition can wait behind a sealer that is itself waiting for
    /// the first.
    fileprivate static var depth: Int {
        get { Thread.current.threadDictionary[depthKey] as? Int ?? 0 }
        set { Thread.current.threadDictionary[depthKey] = newValue }
    }

    /// Whether this thread is inside ``seal(_:wiping:during:)``.
    private static var sealing: Bool {
        get { Thread.current.threadDictionary[sealingKey] as? Bool ?? false }
        set { Thread.current.threadDictionary[sealingKey] = newValue }
    }

    /// One key for the spellings an account id is written in (standard and
    /// URL-safe base64, padded or not).
    static func key(_ accountIdB64: String) -> String {
        var key = accountIdB64
            .replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        while key.hasSuffix("=") { key.removeLast() }
        return key
    }

    /// Begin one best-effort store write for `accountIdB64`. `nil` when the
    /// account is sealed: the write is then simply not made.
    public static func enter(_ accountIdB64: String) -> Hold? {
        // The wipe's own steps (inside the seal, on the sealing thread) and a
        // write nested in another hold run as they are.
        if sealing || depth > 0 {
            depth += 1
            return Hold(locked: false)
        }
        pthread_rwlock_rdlock(lock)
        if sealed[key(accountIdB64)] != nil {
            pthread_rwlock_unlock(lock)
            return nil
        }
        depth += 1
        return Hold(locked: true)
    }

    /// ``enter(_:)`` for a write its caller must not go on without (the
    /// ratchet state persisted before a ciphertext is published, a commit
    /// recorded before it is sent): refused, it throws ``Sealed``.
    public static func enterOrEnd(_ accountIdB64: String) throws -> Hold {
        guard let hold = enter(accountIdB64) else { throw Sealed() }
        return hold
    }

    /// Begin one write that a plain sign-out does not refuse and a wipe does:
    /// for what a fresh identity owes its account after the publish, which
    /// may land after the person signed out (``AuthRepository/mintFreshIdentity()``).
    public static func enterUnlessWiped(_ accountIdB64: String) -> Hold? {
        if sealing || depth > 0 {
            depth += 1
            return Hold(locked: false)
        }
        pthread_rwlock_rdlock(lock)
        if sealed[key(accountIdB64)] == true {
            pthread_rwlock_unlock(lock)
            return nil
        }
        depth += 1
        return Hold(locked: true)
    }

    /// ``enterUnlessWiped(_:)`` for a write its caller must not go on
    /// without (a key the device is about to rely on): refused, it throws
    /// ``Sealed``.
    public static func enterUnlessWipedOrEnd(_ accountIdB64: String) throws -> Hold {
        guard let hold = enterUnlessWiped(accountIdB64) else { throw Sealed() }
        return hold
    }

    /// Begin one write of the recall record (ADR-0074 section 1.1): what a
    /// device whose account keys were reset elsewhere keeps to offer "Was it
    /// you?". It survives the STAGED wipe by design, and its own updates (a
    /// refreshed status, a used attempt) are made while that wipe's seal is
    /// in force, so it is refused only once the account was FULLY wiped.
    public static func enterUnlessFullyWiped(_ accountIdB64: String) -> Hold? {
        if sealing || depth > 0 {
            depth += 1
            return Hold(locked: false)
        }
        pthread_rwlock_rdlock(lock)
        if fullyWiped.contains(key(accountIdB64)) {
            pthread_rwlock_unlock(lock)
            return nil
        }
        depth += 1
        return Hold(locked: true)
    }

    /// The session of `accountIdB64` has ended: wait for its writes in
    /// flight, refuse the later ones, and run `during` while nothing can
    /// write. `wiping` when `during` removes the account's stores; `staged`
    /// when that wipe keeps the recall record.
    @discardableResult
    public static func seal<T>(
        _ accountIdB64: String, wiping: Bool = false, staged: Bool = false, during: () -> T
    ) -> T {
        // A seal inside a seal (the wipe ending the session) adds its mark
        // and runs.
        if sealing {
            mark(accountIdB64, wiping: wiping, staged: staged)
            return during()
        }
        pthread_rwlock_wrlock(lock)
        sealing = true
        defer {
            sealing = false
            pthread_rwlock_unlock(lock)
        }
        mark(accountIdB64, wiping: wiping, staged: staged)
        return during()
    }

    /// ``seal(_:wiping:staged:during:)`` with nothing to run: a sign-out, which
    /// deletes nothing.
    public static func seal(_ accountIdB64: String) {
        seal(accountIdB64) {}
    }

    private static func mark(_ accountIdB64: String, wiping: Bool, staged: Bool) {
        guard !accountIdB64.isEmpty else { return }
        let key = key(accountIdB64)
        if wiping && !staged { fullyWiped.insert(key) }
        if wiping { wipes[key] = (wipes[key] ?? 0) &+ 1 }
        // Every ending is a new one, also for an account sealed already.
        endings[key] = (endings[key] ?? 0) &+ 1
        sealed[key] = (sealed[key] ?? false) || wiping
    }

    /// `accountIdB64` landed on this device again: its stores are written.
    public static func open(_ accountIdB64: String) {
        pthread_rwlock_wrlock(lock)
        sealed[key(accountIdB64)] = nil
        fullyWiped.remove(key(accountIdB64))
        pthread_rwlock_unlock(lock)
    }

    /// Whether writes of `accountIdB64` are refused. Tests and diagnostics.
    public static func isSealed(_ accountIdB64: String) -> Bool {
        pthread_rwlock_rdlock(lock)
        defer { pthread_rwlock_unlock(lock) }
        return sealed[key(accountIdB64)] != nil
    }

    /// Open every account (a test starts from a new process's state).
    static func resetForTest() {
        pthread_rwlock_wrlock(lock)
        sealed.removeAll()
        fullyWiped.removeAll()
        endings.removeAll()
        wipes.removeAll()
        pthread_rwlock_unlock(lock)
    }
}
