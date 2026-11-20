// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The recovery row of a pairing: what an already signed-in device hands to
/// a new one over the pairing rendezvous mailbox ("another device"
/// recovery, ADR-0018 Path B).
///
/// **Payload v3** (ADR-0074 "Joining after a rotation", G-56):
///
/// ```text
///   plaintext = grant(32) ‖ phrase_entropy(16) ‖ count(1)
///               ‖ count × ( seed(64) ‖ adopted_at_ms(8, big-endian) )
///   count     = 0..=8 own predecessors, OLDEST FIRST
///   key       = deviceGroup.exportSecret(identity,
///                 recoveryExporterLabel() /* "lakko/device-recovery/v3" */,
///                 rendezvous_pseudonym, recoveryExporterKeyLen())
///   sealed    = sealWithKey(key, plaintext)
///   row       = wrapRecoveryRow(sealed) /* "lakko/recovery-payload/v3" ‖ sealed */
/// ```
///
/// The codec and the row framing are the core's (`composeRecoveryPayload`,
/// `decodeRecoveryPayload`, `wrapRecoveryRow`, `unwrapRecoveryRow`), so the
/// three clients cannot disagree. v2 is refused (flag day, no dual accept):
/// its row is `unsupported-row`, its 48-byte payload `bad-length`. The seeds
/// travel only inside the AEAD blob; only their count may be logged.
///
/// This file holds the transport (publish, poll) and the phrase rebuild.
public enum RecoveryPayload {

    /// Entropy length of the 12-word recovery phrase (BIP39, 128 bits).
    public static let recoveryPhraseEntropyLength: Int = 16

    /// Server-side long-poll budget per request, in seconds — same
    /// rationale as ``WelcomeCourier``.
    private static let perRequestLongPollSeconds: Int = 15

    /// Default soft timeout for the joiner-side recovery poll. The
    /// scanner publishes the row right after the Welcome + history
    /// bundle (one extra `/device/authorize/start` round-trip),
    /// so a short window suffices.
    public static let recoveryPollTimeoutSeconds: TimeInterval = 15

    /// The phrase and the 64-byte AccountIdentity seed a transferred
    /// entropy stands for. The phrase is the core's canonical form (12
    /// lower-case words, single spaces), which is what the device stores
    /// once the seed passed the published-identity check. Throws when the
    /// entropy is not 16 bytes.
    public static func phraseAndSeed(
        fromEntropy entropy: Data
    ) throws -> (phrase: String, seed: Data) {
        guard entropy.count == recoveryPhraseEntropyLength else {
            throw RecoveryPayloadError.argument(
                "phrase entropy must be \(recoveryPhraseEntropyLength) bytes " +
                "(got \(entropy.count))"
            )
        }
        let mnemonic = try RecoveryMnemonic.fromEntropy(entropy: entropy)
        return (mnemonic.phrase(), mnemonic.toSeed())
    }

    /// `true` when a mailbox row is a recovery row of ANY version (v3, or a
    /// refused older one): the history-bundle poll skips every such row.
    public static func isRecoveryRow(_ row: Data) -> Bool {
        do {
            return try unwrapRecoveryRow(row: row) != nil
        } catch {
            return true
        }
    }

    // MARK: - Publish / poll

    /// Scanner side: PUT the sealed recovery payload to the rendezvous
    /// mailbox AFTER the Welcome (and the optional history bundle), framed
    /// by the core's ``wrapRecoveryRow(sealed:)``.
    public static func publishRecoveryPayload(
        rendezvousPseudonym: Data,
        sealedBlob: Data,
        ds: DsClient = DsClient()
    ) async throws {
        _ = try await ds.publishRendezvous(id: rendezvousPseudonym,
            payload: wrapRecoveryRow(sealed: sealedBlob)
        )
    }

    /// Discriminated outcome of
    /// ``pollForRecoveryPayload(rendezvousPseudonym:afterCursor:timeoutSeconds:ds:)``.
    public enum RecoveryPayloadOutcome {
        /// The sealed blob of a v3 row.
        case received(Data)
        /// A recovery row of a version this client refuses (v2): the other
        /// device runs an older build. The joiner shows "could not hand over
        /// the keys".
        case refused
        case timeout
        case cancelled
    }

    /// Joining side: poll the rendezvous mailbox past the Welcome cursor
    /// for the recovery row. Rows that are not recovery rows (the history
    /// bundle, the state bundle) are skipped by advancing the cursor.
    ///
    /// Absence is an expected outcome (`.timeout`): a scanner that holds no
    /// recovery phrase, or whose recovery half failed (its own predecessor
    /// slot could not be read included), publishes no row.
    public static func pollForRecoveryPayload(
        rendezvousPseudonym: Data,
        afterCursor: Int64,
        timeoutSeconds: TimeInterval = recoveryPollTimeoutSeconds,
        ds: DsClient = DsClient()
    ) async throws -> RecoveryPayloadOutcome {
        let deadline = Date(timeIntervalSinceNow: timeoutSeconds)
        var cursor = afterCursor

        while true {
            if Task.isCancelled { return .cancelled }
            let remaining = deadline.timeIntervalSinceNow
            if remaining <= 0 { return .timeout }

            let waitSecs = min(
                Self.perRequestLongPollSeconds,
                max(1, Int(remaining))
            )
            let result: FetchResult
            do {
                result = try await ds.fetchRendezvous(id: rendezvousPseudonym,
                    after: cursor,
                    limit: 1,
                    waitSecs: waitSecs
                )
            } catch DsClientError.http(let status, _, _) where status == 404 {
                // Same transient-error posture as the Welcome /
                // history polls — 404 means "no rows yet".
                continue
            } catch let err as DsClientError {
                if case .network = err {
                    continue
                }
                throw err
            }
            if let first = result.messages.first {
                switch outcome(of: first.payload) {
                case .some(let found):
                    return found
                case .none:
                    // Not a recovery row (e.g. the history bundle) — skip it.
                    cursor = first.id
                    continue
                }
            }
            cursor = result.messages.reduce(cursor) { max($0, $1.id) }
        }
    }

    /// What one mailbox row is to the recovery poll: `nil` when it is not a
    /// recovery row, else the outcome it ends the poll with.
    static func outcome(of row: Data) -> RecoveryPayloadOutcome? {
        do {
            guard let sealed = try unwrapRecoveryRow(row: row) else { return nil }
            return .received(sealed)
        } catch {
            NSLog("recovery row refused (another version): %@", String(describing: error))
            return .refused
        }
    }
}

/// Error vocabulary for ``RecoveryPayload``.
public enum RecoveryPayloadError: Error, LocalizedError {
    case argument(String)

    public var errorDescription: String? {
        switch self {
        case .argument(let message):
            return message
        }
    }
}
