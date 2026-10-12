// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0065 R.2 - "a reset was accepted", as an in-process signal.
//
// iOS mirror of `apps/web/src/contacts/reset-events.ts`.
//
// The identity gate (`ContactsClient`) accepts a reset wherever a bundle is
// fetched; the work that follows (re-keying the contact row, moving the
// shared conversations onto keys the old devices do not hold, R.5 to R.7)
// belongs to the contacts and chat layers. This is the one seam between
// them. It carries no UI: nothing that listens may show anything (R.0).
//
// Process-wide, because a `ContactsClient` is built wherever an identity is
// fetched and every one of them must report to the same listeners.

/// One accepted reset.
public struct ResetAccepted: Equatable {
    /// Standard base64 of the 16-byte account id.
    public let accountIdB64: String
    /// The key this device held for the account before the re-pin.
    public let previousKeyB64: String
    public let newKeyB64: String
    public let keyGeneration: UInt64
    /// The attestation's `signed_at`, UNIX milliseconds.
    public let signedAtMs: Int64

    public init(
        accountIdB64: String,
        previousKeyB64: String,
        newKeyB64: String,
        keyGeneration: UInt64,
        signedAtMs: Int64
    ) {
        self.accountIdB64 = accountIdB64
        self.previousKeyB64 = previousKeyB64
        self.newKeyB64 = newKeyB64
        self.keyGeneration = keyGeneration
        self.signedAtMs = signedAtMs
    }
}

public enum ResetEvents {

    private static let lock = NSLock()
    private static var listeners: [UUID: (ResetAccepted) -> Void] = [:]

    /// Listen for accepted resets. Returns the unsubscribe.
    @discardableResult
    public static func subscribe(_ listener: @escaping (ResetAccepted) -> Void) -> () -> Void {
        let id = UUID()
        lock.lock()
        listeners[id] = listener
        lock.unlock()
        return {
            lock.lock()
            listeners.removeValue(forKey: id)
            lock.unlock()
        }
    }

    /// Tell every listener. Called by the gate after the re-pin is written.
    public static func notify(_ event: ResetAccepted) {
        lock.lock()
        let current = Array(listeners.values)
        lock.unlock()
        for listener in current { listener(event) }
    }
}
